//! The state-dependent main action (`docs/gui-redesign.md`, 5.4): one
//! `AdwSplitButton` at the end of the editor's header bar whose label
//! says what it does for the open article right now - "Als Entwurf
//! hochladen", "Entwurf aktualisieren", "Veröffentlichen …",
//! "Änderungen veröffentlichen …" - with the alternatives in its menu.
//! It's the only suggested-style button in the view; for a published,
//! unchanged article it turns into a plain "Im Blog ansehen".
//!
//! Also owns what goes with that state: the window title (article title,
//! state as subtitle) and the banner below the header bar for a published
//! article with pending changes, a post changed or deleted on the server,
//! and conflicts.

use std::cell::Cell;
use std::path::Path;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk4::{gio, glib};

use crate::document::{self, Document, PostStatus};
use crate::export::{self, BusySetter, PublishFeedback, TargetStatus};
use crate::i18n::tr;
use crate::syncstate::{self, PostState, Remote, SyncState};
use crate::window::{self, DocContext};
use crate::releasecheck::{self, Decision, Mode};
use crate::{blogsync, importer, library, worksave, wpclient, wpsite};

/// What the banner currently offers, so its single button knows what to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BannerKind {
    None,
    PublishedChanges,
    RemoteChanged,
    Conflict,
    Gone,
    /// Opened from the blog with more than plain Markdown in it.
    MarkdownHint,
    /// A translation whose original changed since (`translatedialog.rs`).
    TranslationChanged,
    /// A translation nobody has reviewed yet.
    TranslationUnreviewed,
}

pub struct MainAction {
    pub button: adw::SplitButton,
    pub banner: adw::Banner,
    ctx: DocContext,
    window: glib::WeakRef<adw::ApplicationWindow>,
    open_url: Rc<dyn Fn(String)>,
    /// The "Im Blog" view `open_url` shows - the autosave preview runs its
    /// request in there.
    blog_view: Rc<crate::browser::BrowserView>,
    links: releasecheck::LinkTarget,
    banner_kind: Cell<BannerKind>,
    busy: Cell<bool>,
    /// Open the blog preview once the running upload is done.
    preview_after_upload: Cell<bool>,
    /// Whether the open translation's original changed since - looked up
    /// in the library once per opened document (`doc_generation`), not on
    /// every keystroke.
    original_changed: Cell<(u64, Option<bool>)>,
    weak: Weak<MainAction>,
}

impl MainAction {
    /// `open_url` shows a page in the app's own browser view (it shares
    /// the wp-admin login, which draft previews need).
    pub fn new(window: &adw::ApplicationWindow, ctx: &DocContext, open_url: Rc<dyn Fn(String)>, blog_view: Rc<crate::browser::BrowserView>, links: releasecheck::LinkTarget) -> Rc<Self> {
        let button = adw::SplitButton::builder().label(tr("Als Entwurf hochladen")).dropdown_tooltip(tr("Weitere Aktionen")).build();
        let banner = adw::Banner::new("");

        let this = Rc::new_cyclic(|weak| MainAction {
            button: button.clone(),
            banner: banner.clone(),
            ctx: ctx.clone(),
            window: window.downgrade(),
            open_url,
            blog_view,
            links,
            banner_kind: Cell::new(BannerKind::None),
            busy: Cell::new(false),
            preview_after_upload: Cell::new(false),
            original_changed: Cell::new((u64::MAX, None)),
            weak: weak.clone(),
        });
        this.install_actions(window);
        {
            let weak = this.weak.clone();
            banner.connect_button_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.on_banner_button();
                }
            });
        }
        {
            let weak = this.weak.clone();
            ctx.add_library_listener(Rc::new(move |_| {
                let weak = weak.clone();
                glib::idle_add_local_once(move || {
                    if let Some(this) = weak.upgrade() {
                        this.refresh();
                    }
                });
            }));
        }
        this.refresh();
        this
    }

    /// The blog the open working copy belongs to (the active one for a
    /// local-only article).
    fn site(&self) -> wpsite::SiteConfig {
        let path = self.ctx.current_path.borrow().clone();
        wpsite::for_document(path.as_deref(), self.ctx.frontmatter.borrow().wp_site.as_deref())
    }

    fn state(&self) -> (Document, PostState, Remote) {
        let doc = self.ctx.current_document();
        let remote = self.ctx.remote_for(&doc.frontmatter);
        let state = syncstate::state(&doc, &remote);
        (doc, state, remote)
    }

    /// Updates label, menu, title and banner from the open article.
    pub fn refresh(&self) {
        if self.busy.get() {
            return;
        }
        let (doc, state, _) = self.state();
        let fm = &doc.frontmatter;
        let empty = doc.body.trim().is_empty() && fm.title.trim().is_empty();

        // (primary action, label, suggested, menu entries)
        let (action, label, suggested, menu): (&str, String, bool, Vec<(String, &str)>) = match (state.status, state.sync) {
            (None, _) => (
                "main.upload-draft",
                tr("Als Entwurf hochladen"),
                true,
                vec![(tr("Zur Prüfung einreichen"), "main.submit-review"), (tr("Veröffentlichen …"), "main.publish")],
            ),
            (Some(PostStatus::Draft | PostStatus::Pending), SyncState::InSync | SyncState::RemoteChanged) => (
                "main.publish",
                tr("Veröffentlichen …"),
                true,
                vec![(tr("Blog-Vorschau öffnen"), "main.open-preview"), (tr("Planen …"), "main.schedule")],
            ),
            (Some(PostStatus::Draft | PostStatus::Pending), _) => (
                "main.update",
                tr("Entwurf aktualisieren"),
                true,
                vec![(tr("Aktualisieren und Vorschau öffnen"), "main.update-preview"), (tr("Mit Blog-Fassung vergleichen"), "main.compare"), (tr("Veröffentlichen …"), "main.publish")],
            ),
            (Some(PostStatus::Future), SyncState::LocalChanges | SyncState::Conflict) => (
                "main.update",
                tr("Änderungen hochladen"),
                true,
                vec![(tr("Termin ändern …"), "win.properties"), (tr("Jetzt veröffentlichen"), "main.publish-now"), (tr("Auf Entwurf zurücksetzen …"), "main.revert-draft")],
            ),
            (Some(PostStatus::Future), _) => (
                "main.open-preview",
                tr("Blog-Vorschau öffnen"),
                false,
                vec![(tr("Termin ändern …"), "win.properties"), (tr("Jetzt veröffentlichen"), "main.publish-now"), (tr("Auf Entwurf zurücksetzen …"), "main.revert-draft")],
            ),
            (Some(PostStatus::Publish | PostStatus::Private), SyncState::LocalChanges | SyncState::Conflict) => (
                "main.publish-changes",
                tr("Änderungen veröffentlichen …"),
                true,
                vec![(tr("Vorschau im Blog"), "main.autosave-preview"), (tr("Mit Blog-Fassung vergleichen"), "main.compare"), (tr("Änderungen verwerfen …"), "main.discard"), (tr("Auf Entwurf zurücksetzen …"), "main.revert-draft")],
            ),
            (Some(PostStatus::Publish | PostStatus::Private), _) => (
                "main.open-preview",
                tr("Im Blog ansehen"),
                false,
                vec![(tr("Auf Entwurf zurücksetzen …"), "main.revert-draft")],
            ),
        };
        let menu_model = gio::Menu::new();
        for (entry, entry_action) in menu {
            menu_model.append(Some(&entry), Some(entry_action));
        }
        // Translations (`translatedialog.rs`): for a translation its review
        // and original, for anything already on the blog the way to one.
        let translation_menu = gio::Menu::new();
        if fm.translation.is_some() {
            translation_menu.append(Some(&tr("Gegenlesen …")), Some("main.review"));
            translation_menu.append(Some(&tr("Original öffnen")), Some("main.open-original"));
            translation_menu.append(Some(&tr("Übersetzung aktualisieren …")), Some("main.translate"));
        } else if fm.wp_post_id.is_some() {
            translation_menu.append(Some(&tr("Übersetzen …")), Some("main.translate"));
            translation_menu.append(Some(&tr("Übersetzung öffnen")), Some("main.open-translation"));
        }
        if translation_menu.n_items() > 0 {
            menu_model.append_section(None, &translation_menu);
        }
        self.button.set_label(&label);
        self.button.set_action_name(Some(action));
        self.button.set_menu_model(Some(&menu_model));
        if suggested {
            self.button.add_css_class("suggested-action");
        } else {
            self.button.remove_css_class("suggested-action");
        }
        self.button.set_sensitive(!empty);

        // Gone/deleted posts can't be updated; only the banner's way out.
        if state.sync == SyncState::RemoteGone {
            self.button.set_sensitive(false);
        }

        let title = library::title_hint(&doc).unwrap_or_else(|| tr("Unbenannt"));
        self.ctx.title.set_title(&title);
        // With more than one blog, where an upload goes is part of the state.
        let subtitle = if wpsite::load_all().sites.len() > 1 { format!("{} · {}", self.site().site_id(), state_text(&doc, state)) } else { state_text(&doc, state) };
        self.ctx.title.set_subtitle(&subtitle);

        let original_changed = if fm.translation.is_some() {
            let generation = self.ctx.doc_generation.get();
            match self.original_changed.get() {
                (cached, value) if cached == generation => value,
                _ => {
                    let path = self.ctx.current_path.borrow().clone();
                    let value = crate::translatedialog::original_changed(&doc, path.as_deref());
                    self.original_changed.set((generation, value));
                    value
                }
            }
        } else {
            None
        };

        let (kind, message, button) = match (state.status, state.sync) {
            (_, SyncState::RemoteGone) => (BannerKind::Gone, tr("Dieser Beitrag wurde im Blog gelöscht oder in den Papierkorb verschoben."), tr("Verknüpfung lösen")),
            (_, SyncState::Conflict) => (BannerKind::Conflict, tr("Dieser Beitrag wurde im Blog geändert, während du hier weitergeschrieben hast."), tr("Auflösen …")),
            (_, SyncState::RemoteChanged) => (BannerKind::RemoteChanged, tr("Dieser Beitrag wurde im Blog geändert."), tr("Blog-Fassung laden")),
            (Some(PostStatus::Publish | PostStatus::Private), SyncState::LocalChanges) => (
                BannerKind::PublishedChanges,
                tr("Veröffentlichter Beitrag: Deine Änderungen gehen erst mit „Änderungen veröffentlichen“ online."),
                String::new(),
            ),
            _ if original_changed == Some(true) => (BannerKind::TranslationChanged, tr("Das Original wurde seit der Übersetzung geändert."), tr("Übersetzung aktualisieren …")),
            _ if fm.translation.as_ref().is_some_and(|t| !t.reviewed) => (
                BannerKind::TranslationUnreviewed,
                tr("Diese Übersetzung ist noch nicht gegengelesen. Erst danach lässt sie sich veröffentlichen und wird im Blog verknüpft."),
                tr("Gegenlesen …"),
            ),
            _ if fm.markdown_hint && crate::markdowncheck::assess(&doc.body).closeness != gutenberg::Closeness::Plain => (
                BannerKind::MarkdownHint,
                tr("Teile dieses Beitrags sind WordPress-Markup und nur als Text bearbeitbar."),
                tr("Details"),
            ),
            _ => (BannerKind::None, String::new(), String::new()),
        };
        self.banner_kind.set(kind);
        self.banner.set_title(&message);
        self.banner.set_button_label((!button.is_empty()).then_some(button.as_str()));
        self.banner.set_revealed(kind != BannerKind::None);
    }

    fn install_actions(&self, window: &adw::ApplicationWindow) {
        let group = gio::SimpleActionGroup::new();
        let add = |name: &str, f: fn(&MainAction)| {
            let action = gio::SimpleAction::new(name, None);
            let weak = self.weak.clone();
            action.connect_activate(move |_, _| {
                if let Some(this) = weak.upgrade() {
                    f(&this);
                }
            });
            group.add_action(&action);
        };
        add("upload-draft", |this| this.upload(TargetStatus::Set(PostStatus::Draft), false));
        add("submit-review", |this| this.upload(TargetStatus::Set(PostStatus::Pending), false));
        add("update", |this| this.upload(TargetStatus::PublishOrKeep, false));
        add("update-preview", |this| this.upload(TargetStatus::PublishOrKeep, true));
        add("open-preview", MainAction::open_preview);
        add("autosave-preview", MainAction::autosave_preview);
        add("blog-preview", MainAction::blog_preview);
        add("compare", MainAction::compare);
        add("translate", MainAction::translate);
        add("review", MainAction::review);
        add("open-original", |this| {
            let doc = this.ctx.current_document();
            let path = this.ctx.current_path.borrow().clone();
            match crate::translatedialog::find_original(&doc, path.as_deref()) {
                Some((path, _)) => window::open_document_at_path(path, &this.ctx),
                None => window::show_toast(&this.ctx.toast_overlay, &tr("Das Original dieser Übersetzung liegt nicht in der Bibliothek. Öffne es dort zuerst aus dem Blog.")),
            }
        });
        add("open-translation", |this| {
            let doc = this.ctx.current_document();
            let path = this.ctx.current_path.borrow().clone();
            match crate::translatedialog::find_translation(&doc, path.as_deref()) {
                Some((path, _)) => window::open_document_at_path(path, &this.ctx),
                None => window::show_toast(&this.ctx.toast_overlay, &tr("Zu diesem Beitrag gibt es noch keine Übersetzung.")),
            }
        });
        add("publish", |this| this.release_check(Mode::Publish { scheduled: false }));
        add("schedule", |this| this.release_check(Mode::Publish { scheduled: true }));
        add("publish-changes", |this| this.release_check(Mode::PublishChanges));
        add("publish-now", |this| {
            this.confirm(&tr("Jetzt veröffentlichen?"), &tr("Der geplante Termin entfällt, der Beitrag geht sofort online."), &tr("Veröffentlichen"), false, |this| {
                this.upload(TargetStatus::Set(PostStatus::Publish), false)
            })
        });
        add("revert-draft", |this| {
            this.confirm(&tr("Auf Entwurf zurücksetzen?"), &tr("Der Beitrag ist danach nicht mehr öffentlich sichtbar."), &tr("Auf Entwurf zurücksetzen"), true, |this| {
                this.upload(TargetStatus::Set(PostStatus::Draft), false)
            })
        });
        add("discard", |this| {
            this.confirm(&tr("Änderungen verwerfen?"), &tr("Die Arbeitskopie wird durch die Fassung aus dem Blog ersetzt. Deine Änderungen gehen verloren."), &tr("Verwerfen"), true, MainAction::load_from_blog)
        });
        window.insert_action_group("main", Some(&group));

        // Ctrl+Shift+P: the release check fitting the open article.
        let check = gio::SimpleAction::new("publish", None);
        let weak = self.weak.clone();
        check.connect_activate(move |_, _| {
            let Some(this) = weak.upgrade() else { return };
            let (_, state, _) = this.state();
            if matches!(state.status, Some(PostStatus::Publish | PostStatus::Private)) {
                this.release_check(Mode::PublishChanges);
            } else {
                this.release_check(Mode::Publish { scheduled: false });
            }
        });
        window.add_action(&check);
    }

    fn translate(&self) {
        if let Some(window) = self.window.upgrade() {
            crate::translatedialog::open(&window, &self.ctx);
        }
    }

    fn review(&self) {
        if let Some(window) = self.window.upgrade() {
            crate::translatedialog::open_review(&window, &self.ctx);
        }
    }

    /// Opens the release check and uploads with the decision taken there.
    fn release_check(&self, mode: Mode) {
        let Some(window) = self.window.upgrade() else { return };
        // The checks look at the current media list.
        worksave::flush(&self.ctx, false);
        let weak = self.weak.clone();
        releasecheck::open(&window, &self.ctx, mode, &self.links, move |decision| {
            let Some(this) = weak.upgrade() else { return };
            match (mode, decision) {
                (Mode::PublishChanges, _) => this.upload(TargetStatus::PublishOrKeep, false),
                (_, Decision::Now) => this.upload(TargetStatus::Set(PostStatus::Publish), false),
                (_, Decision::Scheduled(at)) => {
                    this.ctx.frontmatter.borrow_mut().scheduled_at = Some(at);
                    this.upload(TargetStatus::Set(PostStatus::Future), false);
                }
            }
        });
    }

    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        self.button.set_sensitive(!busy);
        if busy {
            self.button.set_label(&tr("Wird hochgeladen …"));
        } else {
            self.refresh();
        }
    }

    /// Uploads the open article with `target`'s status.
    fn upload(&self, target: TargetStatus, preview_after: bool) {
        let Some(window) = self.window.upgrade() else { return };
        // The working copy needs a file before the upload can record its
        // post id in it.
        worksave::flush(&self.ctx, true);
        // A new library article goes to the blog of its language, not to
        // whichever blog is active (`wpsite::for_document`).
        if self.ctx.frontmatter.borrow().wp_site.is_none() && self.ctx.current_path.borrow().as_deref().is_some_and(|p| library::contains(&library::root(), p)) {
            let site = self.site().site_id();
            self.ctx.frontmatter.borrow_mut().wp_site = Some(site);
        }
        self.preview_after_upload.set(preview_after);

        let ctx = self.ctx.clone();
        let get_body: export::BodyProvider = {
            let ctx = ctx.clone();
            Rc::new(move || ctx.current_document().body)
        };
        let get_doc_dir: export::DocDirProvider = {
            let ctx = ctx.clone();
            Rc::new(move || ctx.current_path.borrow().as_deref().and_then(Path::parent).map(Path::to_path_buf))
        };
        let weak = self.weak.clone();
        let set_busy: BusySetter = Rc::new(move |busy| {
            if let Some(this) = weak.upgrade() {
                this.set_busy(busy);
            }
        });
        let feedback = {
            let weak = self.weak.clone();
            let toast_overlay = ctx.toast_overlay.clone();
            PublishFeedback {
                on_progress: Rc::new(|_| {}),
                on_success: Rc::new(move |post: &wpclient::PostResult, status: PostStatus| {
                    let Some(this) = weak.upgrade() else { return };
                    blogsync::record_upload(&this.ctx, post, status);
                    let message = match target {
                        TargetStatus::Set(PostStatus::Draft) => tr("Als Entwurf hochgeladen."),
                        TargetStatus::Set(PostStatus::Pending) => tr("Zur Prüfung eingereicht."),
                        TargetStatus::Set(PostStatus::Publish) => tr("Veröffentlicht."),
                        TargetStatus::Set(PostStatus::Future) => tr("Geplant."),
                        _ => tr("Aktualisiert."),
                    };
                    window::show_toast(&this.ctx.toast_overlay, &message);
                    // Status and media ids changed under the property fields.
                    this.ctx.bump_generation();
                    this.ctx.notify_library(false);
                    this.ctx.notify_blog();
                    if this.preview_after_upload.replace(false) {
                        this.open_preview();
                    }
                }),
                on_error: Rc::new(move |message: &str| window::show_toast(&toast_overlay, message)),
            }
        };
        let dialog_parent: gtk4::Widget = window.upcast();
        export::publish(target, &ctx.frontmatter, &get_body, &get_doc_dir, &feedback, &dialog_parent, &window::document_saver(&ctx), &set_busy);
    }

    /// The draft preview (or, once published, the live post) in the app's
    /// browser view.
    fn open_preview(&self) {
        let (doc, state, remote) = self.state();
        let link = match remote {
            Remote::Present { link, .. } if !link.is_empty() => Some(link),
            _ => None,
        };
        let published = matches!(state.status, Some(PostStatus::Publish | PostStatus::Private));
        if let Some(link) = link {
            (self.open_url)(if published { link } else { export::preview_url_for(&link) });
            return;
        }
        // Not checked yet: ask the blog for the link first.
        let Some(post_id) = doc.frontmatter.wp_post_id else { return };
        let rest_base = doc.frontmatter.post_type.rest_base();
        let weak = self.weak.clone();
        importer::run_with_password(
            &self.site(),
            move |site, password| wpclient::Client::new(&site.url, &site.username, password).get_item(rest_base, post_id).map_err(|err| err.to_string()),
            move |outcome| {
                let Some(this) = weak.upgrade() else { return };
                match outcome {
                    Ok(detail) if detail.status == "publish" || detail.status == "private" => (this.open_url)(detail.link),
                    Ok(detail) => (this.open_url)(export::preview_url_for(&detail.link)),
                    Err(err) => window::show_toast(&this.ctx.toast_overlay, &tr("Vorschau nicht verfügbar: {err}").replace("{err}", &err)),
                }
            },
        );
    }

    /// "Vorschau → Im Blog": the open article as the blog shows it - the
    /// live post, the draft preview, or for a published post with local
    /// changes, those changes (`autosave_preview`).
    fn blog_preview(&self) {
        let (doc, state, _) = self.state();
        if doc.frontmatter.wp_post_id.is_none() || state.sync == SyncState::RemoteGone {
            window::show_toast(&self.ctx.toast_overlay, &tr("Noch nicht im Blog – erst als Entwurf hochladen."));
            return;
        }
        let published = matches!(state.status, Some(PostStatus::Publish | PostStatus::Private));
        if published && matches!(state.sync, SyncState::LocalChanges | SyncState::Conflict) {
            self.autosave_preview();
        } else {
            self.open_preview();
        }
    }

    /// "Vorschau im Blog" for a published post: saves the local changes
    /// as a WordPress autosave - a separate revision, the live post stays
    /// as it is - and shows its preview. Done by the app's browser view
    /// itself: the preview link's nonce only works for the session that
    /// created it, which is that view's wp-admin login, not the REST
    /// client's application password.
    fn autosave_preview(&self) {
        let doc = self.ctx.current_document();
        let fm = &doc.frontmatter;
        let Some(post_id) = fm.wp_post_id else { return };
        let site = self.site();
        let (title, body) = match document::split_title_heading(&doc.body) {
            Some((title, rest)) if fm.title.trim().is_empty() => (title, rest.to_string()),
            _ => (fm.title.clone(), doc.body.clone()),
        };
        let mut payload = serde_json::json!({
            "title": title,
            "content": export::gutenberg_preview_html(&body, &fm.media),
            "excerpt": fm.excerpt.clone().unwrap_or_default(),
        });
        if let Some(footnotes) = export::footnotes_meta(&export::with_footnotes(&body).1, fm.wp_footnotes.as_deref()) {
            payload["meta"] = serde_json::json!({ "footnotes": footnotes });
        }
        let base = site.url.trim_end_matches('/');
        let failed = serde_json::to_string(&tr("Vorschau fehlgeschlagen. Bist du im Browser-Tab bei WordPress angemeldet?")).unwrap_or_default();
        let script = format!(
            r#"(async () => {{
                const nonce = document.body.innerText.trim();
                const response = await fetch({endpoint}, {{
                    method: 'POST',
                    credentials: 'same-origin',
                    headers: {{ 'Content-Type': 'application/json', 'X-WP-Nonce': nonce }},
                    body: JSON.stringify({payload}),
                }});
                const result = await response.json().catch(() => ({{}}));
                if (result.preview_link) {{
                    location.href = result.preview_link;
                }} else {{
                    document.body.innerText = {failed} + ' (' + (result.message || response.status) + ')';
                }}
            }})()"#,
            endpoint = serde_json::to_string(&format!("{base}/wp-json/wp/v2/{}/{post_id}/autosaves?_fields=preview_link", fm.post_type.rest_base())).unwrap_or_default(),
        );
        self.blog_view.run_after_next_load(script);
        (self.open_url)(format!("{base}/wp-admin/admin-ajax.php?action=rest-nonce"));
    }

    fn on_banner_button(&self) {
        match self.banner_kind.get() {
            BannerKind::RemoteChanged => self.load_from_blog(),
            BannerKind::Conflict => self.resolve_conflict(),
            BannerKind::Gone => self.unlink(),
            BannerKind::MarkdownHint => self.show_markdown_details(),
            BannerKind::TranslationChanged => self.translate(),
            BannerKind::TranslationUnreviewed => self.review(),
            BannerKind::PublishedChanges | BannerKind::None => {}
        }
    }

    /// What makes the open article more than plain Markdown; "Hinweis
    /// ausblenden" stops the banner for it.
    fn show_markdown_details(&self) {
        let body = self.ctx.buffer.text(&self.ctx.buffer.start_iter(), &self.ctx.buffer.end_iter(), false).to_string();
        let assessment = crate::markdowncheck::assess(&body);
        let weak = self.weak.clone();
        let on_hide: Rc<dyn Fn()> = Rc::new(move || {
            let Some(this) = weak.upgrade() else { return };
            this.ctx.frontmatter.borrow_mut().markdown_hint = false;
            worksave::flush(&this.ctx, false);
            this.refresh();
        });
        let parent = self.window.upgrade().map(|window| window.upcast::<gtk4::Widget>());
        crate::markdowncheck::show_details(parent.as_ref(), &assessment, Some(on_hide));
    }

    /// Replaces the working copy with the post as it is on the blog.
    fn load_from_blog(&self) {
        let fm = self.ctx.frontmatter.borrow().clone();
        let Some(post_id) = fm.wp_post_id else { return };
        let post_type = fm.post_type;
        let weak = self.weak.clone();
        importer::run_with_password(
            &self.site(),
            move |site, password| importer::fetch_and_convert(site, password, post_type, post_id),
            move |outcome| {
                let Some(this) = weak.upgrade() else { return };
                match outcome {
                    Ok(imported) => this.apply_blog_version(post_id, imported),
                    Err(err) => window::show_toast(&this.ctx.toast_overlay, &tr("Laden fehlgeschlagen: {err}").replace("{err}", &err)),
                }
            },
        );
    }

    /// Replaces the working copy with `imported`, the blog's version of
    /// post `post_id`.
    fn apply_blog_version(&self, post_id: u64, imported: importer::ImportedPost) {
        // What we just fetched is the server state now.
        if let Some(fetched) = imported.frontmatter.wp_modified_gmt.clone() {
            let mut remote = self.ctx.remote.borrow_mut();
            let link = match remote.get(&post_id) {
                Some(Remote::Present { link, .. }) => link.clone(),
                _ => String::new(),
            };
            remote.insert(post_id, Remote::Present { modified_gmt: fetched, status: imported.frontmatter.status, link });
        }
        self.ctx.buffer.set_text(&imported.body);
        // The blog doesn't know a translation's link to its original
        // (section hashes, review state) - keep it from the working copy.
        let translation = self.ctx.frontmatter.borrow().translation.clone();
        let mut frontmatter = imported.frontmatter;
        if frontmatter.translation.is_none() {
            frontmatter.translation = translation;
        }
        *self.ctx.frontmatter.borrow_mut() = frontmatter;
        self.ctx.preview_pane.set_article_header(&self.ctx.frontmatter.borrow());
        worksave::flush(&self.ctx, true);
        self.ctx.bump_generation();
        self.ctx.notify_library(false);
        window::show_toast(&self.ctx.toast_overlay, &tr("Blog-Fassung geladen."));
    }

    /// Fetches the blog's version and shows it next to the local one.
    fn compare(&self) {
        let fm = self.ctx.frontmatter.borrow().clone();
        let Some(post_id) = fm.wp_post_id else { return };
        let post_type = fm.post_type;
        let weak = self.weak.clone();
        importer::run_with_password(
            &self.site(),
            move |site, password| importer::fetch_and_convert(site, password, post_type, post_id),
            move |outcome| {
                let Some(this) = weak.upgrade() else { return };
                let Some(window) = this.window.upgrade() else { return };
                let imported = match outcome {
                    Ok(imported) => imported,
                    Err(err) => return window::show_toast(&this.ctx.toast_overlay, &tr("Laden fehlgeschlagen: {err}").replace("{err}", &err)),
                };
                let blog = titled_text(&imported.frontmatter.title, &imported.body);
                let local = this.ctx.current_document();
                let local_text = match document::split_title_heading(&local.body) {
                    Some((title, rest)) if local.frontmatter.title.trim().is_empty() => titled_text(&title, rest),
                    _ => titled_text(&local.frontmatter.title, &local.body),
                };
                let conflict = this.state().1.sync == SyncState::Conflict;
                let weak = this.weak.clone();
                let imported = std::cell::RefCell::new(Some(imported));
                crate::compare::open(&window, &blog, &local_text, conflict, move |choice| {
                    let Some(this) = weak.upgrade() else { return };
                    match choice {
                        crate::compare::Choice::TakeBlog => {
                            if let Some(imported) = imported.borrow_mut().take() {
                                this.apply_blog_version(post_id, imported);
                            }
                        }
                        crate::compare::Choice::KeepMine => this.keep_mine(),
                    }
                });
            },
        );
    }

    fn resolve_conflict(&self) {
        let Some(window) = self.window.upgrade() else { return };
        let dialog = adw::AlertDialog::new(
            Some(&tr("Welche Fassung soll gelten?")),
            Some(&tr("Der Beitrag wurde im Blog geändert, nachdem du ihn zuletzt abgeglichen hast, und du hast hier weitergeschrieben.")),
        );
        dialog.add_response("cancel", &tr("Abbrechen"));
        dialog.add_response("compare", &tr("Vergleichen …"));
        dialog.add_response("blog", &tr("Blog-Fassung übernehmen"));
        dialog.add_response("mine", &tr("Meine Fassung behalten"));
        dialog.set_response_appearance("blog", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = self.weak.clone();
        dialog.connect_response(None, move |_, response| {
            let Some(this) = weak.upgrade() else { return };
            match response {
                "blog" => this.load_from_blog(),
                "mine" => this.keep_mine(),
                "compare" => this.compare(),
                _ => {}
            }
        });
        dialog.present(Some(&window));
    }

    /// Accepts the server's newer state as seen, keeping the local text:
    /// the next upload overwrites the blog without asking again.
    fn keep_mine(&self) {
        let remote = self.ctx.remote_for(&self.ctx.frontmatter.borrow());
        if let Remote::Present { modified_gmt, .. } = remote {
            let mut fm = self.ctx.frontmatter.borrow_mut();
            fm.wp_modified_gmt = Some(modified_gmt);
            fm.wp_content_hash = None;
        }
        worksave::flush(&self.ctx, true);
        self.ctx.notify_library(false);
    }

    /// Detaches a working copy whose post is gone, so it can be uploaded
    /// as a new draft. Uploaded images keep their media references.
    fn unlink(&self) {
        {
            let mut fm = self.ctx.frontmatter.borrow_mut();
            if let Some(id) = fm.wp_post_id.take() {
                self.ctx.remote.borrow_mut().remove(&id);
            }
            fm.wp_content_hash = None;
            fm.wp_site = None;
            fm.wp_modified_gmt = None;
            fm.wp_synced_hash = None;
            fm.wp_synced_at = None;
            fm.featured_media_id = None;
        }
        worksave::flush(&self.ctx, true);
        self.ctx.notify_library(false);
        window::show_toast(&self.ctx.toast_overlay, &tr("Verknüpfung gelöst. Der Artikel ist jetzt nur lokal."));
    }

    fn confirm(&self, heading: &str, body: &str, accept: &str, destructive: bool, on_accept: fn(&MainAction)) {
        let Some(window) = self.window.upgrade() else { return };
        let dialog = adw::AlertDialog::new(Some(heading), Some(body));
        dialog.add_response("cancel", &tr("Abbrechen"));
        dialog.add_response("accept", accept);
        dialog.set_response_appearance("accept", if destructive { adw::ResponseAppearance::Destructive } else { adw::ResponseAppearance::Suggested });
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = self.weak.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "accept" {
                if let Some(this) = weak.upgrade() {
                    on_accept(&this);
                }
            }
        });
        dialog.present(Some(&window));
    }
}

/// `# Title` plus body, the way both sides are compared.
fn titled_text(title: &str, body: &str) -> String {
    format!("# {}\n\n{}", title.trim(), body.trim_start())
}

/// The window subtitle: WordPress status plus what's pending.
pub(crate) fn state_text(doc: &Document, state: PostState) -> String {
    let status = match state.status {
        None => return tr("Nur lokal"),
        Some(PostStatus::Future) => match &doc.frontmatter.scheduled_at {
            Some(at) => format!("{} · {}", PostStatus::Future.label(), document::format_scheduled_at_for_display(at)),
            None => PostStatus::Future.label(),
        },
        Some(status) => status.label(),
    };
    let detail = match (state.status, state.sync) {
        (_, SyncState::RemoteGone) => Some(tr("im Blog gelöscht")),
        (_, SyncState::Conflict) => Some(tr("Konflikt")),
        (_, SyncState::RemoteChanged) => Some(tr("im Blog geändert")),
        (Some(PostStatus::Publish | PostStatus::Private), SyncState::LocalChanges) => Some(tr("Änderungen nicht online")),
        (_, SyncState::LocalChanges) => Some(tr("nicht hochgeladen")),
        _ => None,
    };
    match detail {
        Some(detail) => format!("{status} · {detail}"),
        None => status,
    }
}
