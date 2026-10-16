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
    /// The working copy whose blog preview "Im Blog" shows - when another
    /// one is opened (DE/EN switch) while the view is on screen, the
    /// preview follows.
    previewed: std::cell::RefCell<Option<std::path::PathBuf>>,
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
            previewed: std::cell::RefCell::new(None),
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
        self.follow_previewed_document();
        let (doc, state, _) = self.state();
        let fm = &doc.frontmatter;
        let empty = doc.body.trim().is_empty() && fm.title.trim().is_empty();

        // (primary action, label, suggested, menu entries)
        // A status or date chosen in the properties since the last sync is
        // what the next upload sends - the button says so instead of
        // offering a plain update that would leave it unsent.
        let pending = fm.pending_status_change().filter(|_| fm.wp_post_id.is_some() && state.sync != SyncState::RemoteGone);
        let (action, label, suggested, menu): (&str, String, bool, Vec<(String, &str)>) = if let Some(change) = pending {
            if Some(change) == fm.wp_status {
                // Same status, new publish date.
                ("main.update", tr("Datum hochladen"), true, vec![(tr("Datum im Blog lassen"), "main.keep-status")])
            } else {
                pending_change_action(change)
            }
        } else {
            match (state.status, state.sync) {
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
                if state.status == Some(PostStatus::Publish) {
                    vec![(tr("Neu veröffentlichen (Datum: jetzt) …"), "main.republish"), (tr("Auf Entwurf zurücksetzen …"), "main.revert-draft")]
                } else {
                    vec![(tr("Auf Entwurf zurücksetzen …"), "main.revert-draft")]
                },
            ),
            }
        };
        let menu_model = gio::Menu::new();
        for (entry, entry_action) in menu {
            menu_model.append(Some(&entry), Some(entry_action));
        }
        // Language versions (`translatedialog.rs`): copying the original
        // for DeepL or a chat and pasting the result come first; the AI
        // translation is one more way, if offered at all.
        let translation_menu = gio::Menu::new();
        let ai_menu = gio::Menu::new();
        let ai = crate::translationsettings::ai_enabled();
        if fm.translation.is_some() {
            translation_menu.append(Some(&tr("Original kopieren")), Some("main.copy-original"));
            translation_menu.append(Some(&tr("Abschnitt des Originals kopieren")), Some("main.copy-original-section"));
            translation_menu.append(Some(&tr("Übersetzung einfügen")), Some("main.paste-translation"));
            translation_menu.append(Some(&tr("Prüfen …")), Some("main.review"));
            translation_menu.append(Some(&tr("Bilder dieser Fassung …")), Some("main.language-images"));
            translation_menu.append(Some(&tr("Als aktuell markieren")), Some("main.mark-current"));
            if ai {
                ai_menu.append(Some(&tr("Per KI aktualisieren …")), Some("main.translate"));
            }
        } else if self.ctx.current_path.borrow().as_deref().is_some_and(|p| library::contains(&library::root(), p)) {
            translation_menu.append(Some(&tr("Original kopieren")), Some("main.copy-original"));
            if ai && fm.wp_post_id.is_some() {
                ai_menu.append(Some(&tr("Per KI übersetzen …")), Some("main.translate"));
            }
        }
        if translation_menu.n_items() > 0 {
            menu_model.append_section(None, &translation_menu);
        }
        if ai_menu.n_items() > 0 {
            menu_model.append_section(None, &ai_menu);
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
            // The button already says what goes up ("Termin hochladen" ...).
            _ if pending.is_some() => (
                BannerKind::PublishedChanges,
                tr("Status oder Termin geändert - geht erst mit „{action}“ an den Blog.").replace("{action}", &label),
                String::new(),
            ),
            (Some(PostStatus::Publish | PostStatus::Private), SyncState::LocalChanges) => (
                BannerKind::PublishedChanges,
                tr("Veröffentlichter Beitrag: Deine Änderungen gehen erst mit „Änderungen veröffentlichen“ online."),
                String::new(),
            ),
            _ if original_changed == Some(true) => {
                let open = crate::translatedialog::changed_in_original(&self.ctx).len();
                let message = match open {
                    0 => tr("Titel, Auszug oder andere Angaben des Originals wurden geändert."),
                    1 => tr("Original geändert: 1 Abschnitt offen."),
                    n => tr("Original geändert: {n} Abschnitte offen.").replace("{n}", &n.to_string()),
                };
                (BannerKind::TranslationChanged, message, if open == 0 { tr("Als aktuell markieren") } else { tr("Zeigen") })
            }
            _ if fm.translation.as_ref().is_some_and(|t| !t.reviewed) => (
                BannerKind::TranslationUnreviewed,
                tr("Diese KI-Übersetzung ist noch nicht gegengelesen. Erst danach lässt sie sich veröffentlichen und wird im Blog verknüpft."),
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
        add("mark-current", |this| {
            if crate::translatedialog::mark_current(&this.ctx) {
                window::show_toast(&this.ctx.toast_overlay, &tr("Als aktuell markiert."));
                this.refresh();
            }
        });
        add("review", MainAction::review);
        add("copy-original", |this| crate::translatedialog::copy_original(&this.ctx, false));
        add("copy-original-section", |this| crate::translatedialog::copy_original(&this.ctx, true));
        add("paste-translation", |this| {
            if let Some(window) = this.window.upgrade() {
                crate::translatedialog::paste_translation(&window, &this.ctx);
            }
        });
        add("language-images", |this| {
            if let Some(window) = this.window.upgrade() {
                crate::langmedia::open(&window, &this.ctx);
            }
        });
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
        // A deliberate new date for a post that is already online: it moves
        // to the top of the blog, the old date is only kept in the dialog.
        add("republish", |this| {
            let fm = this.ctx.frontmatter.borrow().clone();
            let remote_date = match this.ctx.remote_for(&fm) {
                Remote::Present { date, .. } if !date.is_empty() => Some(date),
                _ => None,
            };
            let before = fm.wp_published_at.clone().or(remote_date).as_deref().map(crate::postpane::format_site_date);
            let mut body = match &before {
                Some(date) => tr("Der Beitrag bekommt das aktuelle Datum und steht im Blog wieder oben. Bisher veröffentlicht am {date}.").replace("{date}", date),
                None => tr("Der Beitrag bekommt das aktuelle Datum und steht im Blog wieder oben."),
            };
            if this.state().1.sync == SyncState::LocalChanges {
                body.push(' ');
                body.push_str(&tr("Deine lokalen Änderungen gehen mit online."));
            }
            this.confirm(&tr("Neu veröffentlichen?"), &body, &tr("Neu veröffentlichen"), false, |this| {
                // "Now" is the point: a date chosen in the properties yields.
                this.ctx.frontmatter.borrow_mut().publish_date = None;
                this.upload(TargetStatus::Set(PostStatus::Publish), false)
            })
        });
        add("keep-status", |this| {
            {
                let mut fm = this.ctx.frontmatter.borrow_mut();
                fm.publish_date = None;
                if let Some(status) = fm.wp_status {
                    fm.status = status;
                    if status == PostStatus::Future {
                        fm.scheduled_at = fm.wp_scheduled_at.clone();
                    }
                }
            }
            // The properties show the restored status again.
            this.ctx.bump_generation();
            worksave::flush(&this.ctx, true);
            this.refresh();
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

    /// The banner's "Zeigen": the original next to the editor, the cursor
    /// on the first section whose original changed. Each changed section
    /// there says what changed and has "Erledigt". With only title or
    /// excerpt changed, the translation is marked current right away.
    fn show_original_changes(&self) {
        let changed = crate::translatedialog::changed_section_indices(&self.ctx);
        let Some(&first) = changed.first() else {
            if crate::translatedialog::mark_current(&self.ctx) {
                window::show_toast(&self.ctx.toast_overlay, &tr("Als aktuell markiert."));
                self.refresh();
            }
            return;
        };
        if let Some(window) = self.window.upgrade() {
            if window.lookup_action("toggle-preview").and_then(|a| a.state()).and_then(|s| s.get::<bool>()) == Some(false) {
                let _ = WidgetExt::activate_action(&window, "win.toggle-preview", None);
            }
        }
        self.links.view_stack.set_visible_child_name(crate::counterpart::PAGE);
        let buffer = &self.ctx.buffer;
        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
        let line = crate::translate::section_ranges(&text).get(first).map_or(0, |(start, _)| start - 1);
        if let Some(iter) = buffer.iter_at_line(line as i32) {
            buffer.place_cursor(&iter);
        }
        window::show_toast(&self.ctx.toast_overlay, &tr("Geänderte Abschnitte sind im Original orange markiert. „Erledigt“ hakt sie ab."));
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
        crate::export::set_upload_busy(busy);
        self.button.set_sensitive(!busy);
        if busy {
            self.button.set_label(&tr("Wird hochgeladen …"));
        } else {
            self.refresh();
        }
    }

    /// Uploads the open article with `target`'s status - after asking, if
    /// its text reads like another language than the target blog's (a
    /// German text on its way to the English blog, or the other way).
    fn upload(&self, target: TargetStatus, preview_after: bool) {
        // Remembered for the toast: the date is cleared once it went up.
        SENDING_DATE.with(|cell| cell.set(self.ctx.frontmatter.borrow().publish_date.is_some()));
        // A translation is linked to its original's post: without one yet,
        // the original goes first.
        crate::translatedialog::fill_source_id(&self.ctx);
        if self.ctx.frontmatter.borrow().translation.as_ref().is_some_and(|t| t.source_id == 0) {
            window::show_toast(&self.ctx.toast_overlay, &tr("Erst das Original hochladen - die Übersetzung wird mit ihm verknüpft."));
            return;
        }
        self.confirm_unpublish(target, preview_after);
    }

    /// A scheduled date in the future on a post that is online takes it
    /// offline until then - WordPress turns it into "future" and its
    /// address answers 404. That only happens after saying so.
    fn confirm_unpublish(&self, target: TargetStatus, preview_after: bool) {
        let Some(window) = self.window.upgrade() else { return };
        let fm = self.ctx.frontmatter.borrow().clone();
        let now = glib::DateTime::now_local().and_then(|now| now.format("%Y-%m-%dT%H:%M:%S")).map(|s| s.to_string()).unwrap_or_default();
        if !takes_offline(&fm, target.resolve(&fm), &now) {
            self.check_existing_translation(target, preview_after);
            return;
        }
        let when = fm.scheduled_at.as_deref().map(document::format_scheduled_at_for_display).unwrap_or_default();
        let alert = adw::AlertDialog::builder()
            .heading(tr("Veröffentlichten Beitrag offline nehmen?"))
            .body(tr("Der Beitrag ist online. Mit dem Termin {when} nimmt WordPress ihn bis dahin offline - seine Adresse ist bis dahin nicht erreichbar.\n\nNur das Datum ändern, ohne ihn offline zu nehmen, geht mit einem Termin in der Vergangenheit.").replace("{when}", &when))
            .build();
        alert.add_response("cancel", &tr("Abbrechen"));
        alert.add_response("schedule", &tr("Offline nehmen und planen"));
        alert.set_response_appearance("schedule", adw::ResponseAppearance::Destructive);
        alert.set_default_response(Some("cancel"));
        alert.set_close_response("cancel");
        let weak = self.weak.clone();
        alert.connect_response(None, move |_, response| {
            if response == "schedule" {
                if let Some(this) = weak.upgrade() {
                    this.check_existing_translation(target, preview_after);
                }
            }
        });
        alert.present(Some(&window));
    }

    /// Before a language version goes to its blog for the first time:
    /// does the original already have one there (lui-translations' list of
    /// pairs)? Then that one is offered instead of creating a second post
    /// for the same original. Without an answer (offline, no plugin) the
    /// upload goes ahead as before.
    fn check_existing_translation(&self, target: TargetStatus, preview_after: bool) {
        let fm = self.ctx.frontmatter.borrow().clone();
        let Some(source_id) = fm.translation.as_ref().map(|t| t.source_id).filter(|id| *id != 0 && fm.wp_post_id.is_none()) else {
            self.check_language(target, preview_after);
            return;
        };
        let weak = self.weak.clone();
        // Already part of the upload: the button says so, and closing asks.
        self.set_busy(true);
        importer::run_with_password(
            &self.site(),
            move |site, password| wpclient::Client::new(&site.url, &site.username, password).translation_pairs().map_err(|err| err.to_string()),
            move |outcome| {
                let Some(this) = weak.upgrade() else { return };
                this.set_busy(false);
                let existing: Vec<wpclient::TranslationPair> = outcome.unwrap_or_default().into_iter().filter(|pair| pair.source_id == source_id).collect();
                match existing.first() {
                    None => this.check_language(target, preview_after),
                    Some(pair) => this.offer_existing_translation(pair.clone(), existing.len(), target, preview_after),
                }
            },
        );
    }

    fn offer_existing_translation(&self, pair: wpclient::TranslationPair, count: usize, target: TargetStatus, preview_after: bool) {
        let Some(window) = self.window.upgrade() else { return };
        let status = document::PostStatus::from_str(&pair.status).label();
        let mut body = tr("Zu diesem Original gibt es in {site} schon eine Fassung: „{title}“ ({status}, ID {id}). Hochladen würde einen zweiten Beitrag anlegen.")
            .replace("{site}", &self.site().site_id())
            .replace("{title}", &pair.title)
            .replace("{status}", &status)
            .replace("{id}", &pair.id.to_string());
        if count > 1 {
            body.push(' ');
            body.push_str(&tr("Es gibt sogar {n} davon.").replace("{n}", &count.to_string()));
        }
        body.push_str("\n\n");
        body.push_str(&tr("„Vorhandene übernehmen“ öffnet sie als diese Fassung; dein Text bleibt als Kopie neben der Datei."));
        let alert = adw::AlertDialog::builder().heading(tr("Fassung gibt es schon")).body(body).build();
        alert.add_response("cancel", &tr("Abbrechen"));
        alert.add_response("new", &tr("Trotzdem neu anlegen"));
        alert.add_response("adopt", &tr("Vorhandene übernehmen"));
        alert.set_response_appearance("new", adw::ResponseAppearance::Destructive);
        alert.set_response_appearance("adopt", adw::ResponseAppearance::Suggested);
        alert.set_default_response(Some("adopt"));
        alert.set_close_response("cancel");
        let weak = self.weak.clone();
        alert.connect_response(None, move |_, response| {
            let Some(this) = weak.upgrade() else { return };
            match response {
                "adopt" => this.adopt_existing_translation(pair.id, pair.source_hash.clone()),
                "new" => this.check_language(target, preview_after),
                _ => {}
            }
        });
        alert.present(Some(&window));
    }

    /// Replaces the open language version with the existing post `id` of
    /// its blog - kept as this language version of the original (language,
    /// link), with the local text saved beside it first.
    fn adopt_existing_translation(&self, id: u64, source_hash: String) {
        let Some(path) = self.ctx.current_path.borrow().clone() else { return };
        worksave::flush(&self.ctx, true);
        let local = self.ctx.current_document();
        let site = self.site();
        let ctx = self.ctx.clone();
        importer::run_with_password(
            &site,
            move |site, password| importer::fetch_and_convert(site, password, document::PostType::Post, id),
            move |outcome| match outcome {
                Ok(imported) => {
                    let stamp = glib::DateTime::now_local().and_then(|now| now.format("%Y%m%d-%H%M%S")).map(|s| s.to_string()).unwrap_or_default();
                    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let copy = path.with_file_name(format!("{name}.lokal-{stamp}"));
                    if let Err(err) = std::fs::write(&copy, document::serialize(&local)) {
                        window::show_toast(&ctx.toast_overlay, &tr("Kopie deiner Fassung fehlgeschlagen: {err}").replace("{err}", &err.to_string()));
                        return;
                    }
                    let adopted = adopted_translation(&local, imported, &source_hash);
                    if let Err(err) = document::write(&path, &adopted) {
                        window::show_toast(&ctx.toast_overlay, &tr("Fehler beim Speichern: {err}").replace("{err}", &err.to_string()));
                        return;
                    }
                    window::open_document_at_path(path.clone(), &ctx);
                    let shown = copy.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    window::show_toast(&ctx.toast_overlay, &tr("Vorhandene Fassung übernommen. Dein Text liegt als „{name}“ neben der Datei.").replace("{name}", &shown));
                }
                Err(err) => window::show_toast(&ctx.toast_overlay, &tr("Fassung nicht geladen: {err}").replace("{err}", &err)),
            },
        );
    }

    fn check_language(&self, target: TargetStatus, preview_after: bool) {
        let Some(window) = self.window.upgrade() else { return };
        let site = self.site();
        let expected = wpsite::site_lang(&site).unwrap_or_else(|| "de".to_string());
        let doc = self.ctx.current_document();
        let path = self.ctx.current_path.borrow().clone();
        // A language version that still holds the original's text is in the
        // original's language, however short it is.
        let untranslated = doc.frontmatter.translation.is_some()
            && crate::translatedialog::find_original(&doc, path.as_deref()).is_some_and(|(_, original)| crate::translate::still_original_text(&doc, &original));
        let found = if untranslated { Some("de") } else { crate::translate::detect_language(&doc.body) };
        let Some(found) = found.filter(|found| *found != expected) else {
            self.confirm_new_categories(target, preview_after);
            return;
        };
        let name = |lang: &str| match lang {
            "de" => tr("Deutsch"),
            "en" => tr("Englisch"),
            other => other.to_uppercase(),
        };
        let alert = adw::AlertDialog::builder()
            .heading(tr("Falsche Sprache für diesen Blog?"))
            .body(
                tr("Der Text sieht nach {found} aus, geht aber an {site}, einen Blog auf {expected}.")
                    .replace("{found}", &name(found))
                    .replace("{site}", &site.site_id())
                    .replace("{expected}", &name(&expected)),
            )
            .build();
        alert.add_response("cancel", &tr("Abbrechen"));
        alert.add_response("upload", &tr("Trotzdem hochladen"));
        alert.set_response_appearance("upload", adw::ResponseAppearance::Destructive);
        alert.set_default_response(Some("cancel"));
        alert.set_close_response("cancel");
        let weak = self.weak.clone();
        alert.connect_response(None, move |_, response| {
            if response == "upload" {
                if let Some(this) = weak.upgrade() {
                    this.confirm_new_categories(target, preview_after);
                }
            }
        });
        alert.present(Some(&window));
    }

    /// Asks before an upload creates categories the target blog doesn't
    /// have - typically a German category name in an English language
    /// version, which WordPress would otherwise silently add to the English
    /// blog. Names the mapping setting that avoids it. Without a connection
    /// (or any error) the upload goes ahead as before; the upload itself
    /// reports real problems.
    fn confirm_new_categories(&self, target: TargetStatus, preview_after: bool) {
        let fm = self.ctx.frontmatter.borrow().clone();
        if fm.post_type != document::PostType::Post || fm.categories.is_empty() {
            self.upload_now(target, preview_after);
            return;
        }
        let site = self.site();
        let names = fm.categories.clone();
        let (tx, rx) = std::sync::mpsc::channel::<Vec<String>>();
        self.set_busy(true);
        std::thread::spawn(move || {
            let missing = futures_lite::future::block_on(crate::secrets::load_app_password(&site.url, &site.username))
                .ok()
                .flatten()
                .map(|password| {
                    let client = wpclient::Client::new(&site.url, &site.username, &password);
                    names.into_iter().filter(|name| matches!(client.find_term("categories", name), Ok(None))).collect()
                })
                .unwrap_or_default();
            let _ = tx.send(missing);
        });
        let weak = self.weak.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            let missing = match rx.try_recv() {
                Ok(missing) => missing,
                Err(std::sync::mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Vec::new(),
            };
            let Some(this) = weak.upgrade() else { return glib::ControlFlow::Break };
            this.set_busy(false);
            if missing.is_empty() {
                this.upload_now(target, preview_after);
                return glib::ControlFlow::Break;
            }
            let Some(window) = this.window.upgrade() else { return glib::ControlFlow::Break };
            let mut body = tr("Diese Kategorien gibt es in {site} noch nicht und würden neu angelegt: {names}.")
                .replace("{site}", &this.site().site_id())
                .replace("{names}", &missing.join(", "));
            if this.ctx.frontmatter.borrow().translation.is_some() {
                body.push_str("\n\n");
                body.push_str(&tr("Für Übersetzungen lassen sich Kategorien unter Einstellungen › Übersetzung › „Kategorien zuordnen“ übertragen."));
            }
            let alert = adw::AlertDialog::builder().heading(tr("Neue Kategorien anlegen?")).body(body).build();
            alert.add_response("cancel", &tr("Abbrechen"));
            alert.add_response("create", &tr("Anlegen und hochladen"));
            alert.set_default_response(Some("cancel"));
            alert.set_close_response("cancel");
            let weak = this.weak.clone();
            alert.connect_response(None, move |_, response| {
                if response == "create" {
                    if let Some(this) = weak.upgrade() {
                        this.upload_now(target, preview_after);
                    }
                }
            });
            alert.present(Some(&window));
            glib::ControlFlow::Break
        });
    }

    fn upload_now(&self, target: TargetStatus, preview_after: bool) {
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
                on_success: Rc::new(move |post: &wpclient::PostResult, status: PostStatus, requested: Option<PostStatus>| {
                    let Some(this) = weak.upgrade() else { return };
                    blogsync::record_upload(&this.ctx, post, status);
                    let scheduled = this.ctx.frontmatter.borrow().scheduled_at.clone();
                    let mut message = if SENDING_DATE.with(|cell| cell.replace(false)) && matches!(status, PostStatus::Publish | PostStatus::Private) {
                        tr("Datum geändert: veröffentlicht am {date}.").replace("{date}", &post.date.as_deref().map(crate::postpane::format_site_date).unwrap_or_default())
                    } else {
                        upload_message(status, requested, scheduled.as_deref(), post.date.as_deref())
                    };
                    for note in &post.notes {
                        message.push(' ');
                        message.push_str(note);
                    }
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
        *self.previewed.borrow_mut() = self.ctx.current_path.borrow().clone();
        if let Some(link) = link {
            if published {
                (self.open_url)(link);
            } else {
                self.open_draft_preview(export::preview_url_for(&link));
            }
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
                    Ok(detail) => this.open_draft_preview(export::preview_url_for(&detail.link)),
                    Err(err) => window::show_toast(&this.ctx.toast_overlay, &tr("Vorschau nicht verfügbar: {err}").replace("{err}", &err)),
                }
            },
        );
    }

    /// A draft's preview. WordPress shows it only to a logged-in user, and
    /// the app's browser view has its own session: if the blog answers
    /// with its 404 page, the view goes to the login with the preview as
    /// the way back, and a toast says why - instead of a bare 404.
    fn open_draft_preview(&self, url: String) {
        let login = format!("{}/wp-login.php?redirect_to={}", self.site().url.trim_end_matches('/'), percent_encode_component(&url));
        let target = serde_json::to_string(&login).unwrap_or_default();
        self.blog_view.run_after_next_load(format!("if (document.body && document.body.classList.contains('error404')) {{ location.href = {target}; }}"));
        (self.open_url)(url);
        window::show_toast(&self.ctx.toast_overlay, &tr("Entwurfsvorschau: Falls die Anmeldung erscheint, einmal bei WordPress anmelden - danach zeigt der Browser-Tab die Vorschau."));
    }

    /// Re-loads "Im Blog" for the article now open when it still shows
    /// another one's preview and is on screen (DE/EN switch).
    fn follow_previewed_document(&self) {
        let current = self.ctx.current_path.borrow().clone();
        let shown = self.previewed.borrow().clone();
        if shown.is_some() && shown != current && self.blog_view.widget.is_mapped() {
            // Recorded first: an article not in the blog yet only gets its
            // toast once, not on every refresh.
            *self.previewed.borrow_mut() = current;
            self.blog_preview();
        }
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
        *self.previewed.borrow_mut() = self.ctx.current_path.borrow().clone();
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
            BannerKind::TranslationChanged => self.show_original_changes(),
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
            let site = self.ctx.frontmatter.borrow().wp_site.clone().unwrap_or_else(|| wpsite::load().site_id());
            let key = (site, post_id);
            let mut remote = self.ctx.remote.borrow_mut();
            let link = match remote.get(&key) {
                Some(Remote::Present { link, .. }) => link.clone(),
                _ => String::new(),
            };
            remote.insert(key, Remote::Present { modified_gmt: fetched, status: imported.frontmatter.status, link, date: imported.frontmatter.wp_published_at.clone().unwrap_or_default() });
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
            if let Some(key) = window::remote_key(&fm) {
                self.ctx.remote.borrow_mut().remove(&key);
            }
            fm.wp_post_id = None;
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

/// Whether uploading `fm` with `resolved` takes a post that is online
/// offline: it becomes "future" with a date after `now` (local time,
/// `YYYY-MM-DDTHH:MM:SS`, as `scheduled_at` is written).
fn takes_offline(fm: &document::Frontmatter, resolved: Option<PostStatus>, now: &str) -> bool {
    resolved == Some(PostStatus::Future)
        && matches!(fm.wp_status, Some(PostStatus::Publish | PostStatus::Private))
        && fm.scheduled_at.as_deref().is_some_and(|at| at > now)
}

/// An existing post adopted as the open language version: the blog's
/// content and metadata, with the version's language and link to the
/// original kept. Its sections count as the baseline of hand edits. Which
/// state of the original it follows is what the blog recorded
/// (`source_hash` from lui-translations) - when that isn't the original's
/// current state, the version shows "Original geändert" until checked.
fn adopted_translation(local: &Document, imported: importer::ImportedPost, source_hash: &str) -> Document {
    let mut frontmatter = imported.frontmatter;
    frontmatter.lang = local.frontmatter.lang.clone();
    frontmatter.translation = local.frontmatter.translation.clone().map(|mut link| {
        link.source_sections = Vec::new();
        link.source_hash = source_hash.to_string();
        link.reviewed = true;
        link.target_sections = crate::translate::split_sections(&imported.body).iter().map(|s| crate::translate::section_hash(s)).collect();
        link
    });
    let (site, modified) = (frontmatter.wp_site.clone().unwrap_or_default(), frontmatter.wp_modified_gmt.clone().unwrap_or_default());
    let mut doc = Document { frontmatter, body: imported.body };
    // In sync with the blog as adopted - language and link are this app's
    // bookkeeping, not a change to upload.
    syncstate::mark_synced(&mut doc, &site, &modified, &syncstate::now_rfc3339());
    doc
}

/// `s` for use inside a query parameter.
fn percent_encode_component(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The main action for a status changed in the properties: scheduling or
/// a new date goes up as an update (which now sends it), publishing goes
/// through the release check, back to draft through its confirmation.
fn pending_change_action(change: PostStatus) -> (&'static str, String, bool, Vec<(String, &'static str)>) {
    let back = vec![(tr("Status im Blog lassen"), "main.keep-status")];
    match change {
        PostStatus::Future => ("main.update", tr("Termin hochladen"), true, back),
        PostStatus::Publish => ("main.publish", tr("Veröffentlichen …"), true, back),
        PostStatus::Draft => ("main.revert-draft", tr("Auf Entwurf zurücksetzen …"), true, back),
        PostStatus::Pending => ("main.update", tr("Zur Prüfung einreichen"), true, back),
        PostStatus::Private => ("main.update", tr("Privat veröffentlichen"), true, back),
    }
}

thread_local! {
    /// Whether the running upload sends a new publish date.
    static SENDING_DATE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The toast after an upload, from what WordPress actually stored. When
/// that isn't what was asked for, it says so - a "publish" that came back
/// scheduled must not read as "Veröffentlicht". A date in the past for
/// "Geplant" is WordPress's way of backdating: it says that instead.
fn upload_message(stored: PostStatus, requested: Option<PostStatus>, scheduled_at: Option<&str>, stored_date: Option<&str>) -> String {
    let when = || scheduled_at.map(crate::document::format_scheduled_at_for_display).unwrap_or_default();
    match requested {
        Some(PostStatus::Future) if matches!(stored, PostStatus::Publish | PostStatus::Private) => match stored_date.filter(|d| !d.is_empty()) {
            Some(date) => tr("Rückdatiert: veröffentlicht am {date}.").replace("{date}", &crate::postpane::format_site_date(date)),
            None => tr("Rückdatiert."),
        },
        Some(requested) if requested != stored => {
            let base = tr("Hochgeladen, aber WordPress hat den Beitrag als „{stored}“ gespeichert, nicht als „{requested}“.")
                .replace("{stored}", &stored.label())
                .replace("{requested}", &requested.label());
            if stored == PostStatus::Future {
                format!("{base} {}", tr("Termin: {when}").replace("{when}", &when()))
            } else {
                base
            }
        }
        Some(PostStatus::Draft) => tr("Als Entwurf hochgeladen."),
        Some(PostStatus::Pending) => tr("Zur Prüfung eingereicht."),
        Some(PostStatus::Publish) => tr("Veröffentlicht."),
        Some(PostStatus::Future) => tr("Geplant für {when}.").replace("{when}", &when()),
        Some(PostStatus::Private) => tr("Privat veröffentlicht."),
        None => tr("Aktualisiert."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_publish_that_came_back_scheduled_does_not_read_as_published() {
        let message = upload_message(PostStatus::Future, Some(PostStatus::Publish), Some("2026-12-24T10:00:00"), None);
        assert!(message.contains("nicht als"), "{message}");
        assert!(message.contains("2026-12-24 10:00"), "{message}");
        assert_eq!(upload_message(PostStatus::Publish, Some(PostStatus::Publish), None, None), tr("Veröffentlicht."));
        assert_eq!(upload_message(PostStatus::Publish, Some(PostStatus::Future), Some("2025-01-15T10:00:00"), Some("2025-01-15T10:00:00")), tr("Rückdatiert: veröffentlicht am {date}.").replace("{date}", "15.01.2025, 10:00"));
        assert_eq!(upload_message(PostStatus::Draft, None, None, None), tr("Aktualisiert."));
    }

    #[test]
    fn the_preview_address_survives_as_login_redirect() {
        assert_eq!(percent_encode_component("http://b.example/?p=5&preview=true"), "http%3A%2F%2Fb.example%2F%3Fp%3D5%26preview%3Dtrue");
    }

    #[test]
    fn only_a_future_date_on_an_online_post_takes_it_offline() {
        let mut fm = document::Frontmatter { wp_status: Some(PostStatus::Publish), scheduled_at: Some("2026-12-20T10:00:00".into()), ..Default::default() };
        let now = "2026-10-15T22:00:00";
        assert!(takes_offline(&fm, Some(PostStatus::Future), now));
        fm.scheduled_at = Some("2025-01-15T10:00:00".into());
        assert!(!takes_offline(&fm, Some(PostStatus::Future), now), "a past date only backdates");
        fm.scheduled_at = Some("2026-12-20T10:00:00".into());
        fm.wp_status = Some(PostStatus::Draft);
        assert!(!takes_offline(&fm, Some(PostStatus::Future), now), "a draft isn't online");
        fm.wp_status = Some(PostStatus::Publish);
        assert!(!takes_offline(&fm, None, now));
    }

    #[test]
    fn an_adopted_post_keeps_language_and_link() {
        let link = document::TranslationLink { lang: "en".into(), source_id: 48295, source_sections: vec!["aa".into()], reviewed: false, ..Default::default() };
        let local = Document { frontmatter: document::Frontmatter { lang: Some("en".into()), translation: Some(link), ..Default::default() }, body: "Local".into() };
        let imported = importer::ImportedPost { frontmatter: document::Frontmatter { title: "QA Pair EN".into(), wp_post_id: Some(59), ..Default::default() }, body: "Existing text.\n".into() };
        let doc = adopted_translation(&local, imported, "old");
        assert_eq!(doc.frontmatter.wp_post_id, Some(59));
        assert_eq!(doc.frontmatter.lang.as_deref(), Some("en"));
        let link = doc.frontmatter.translation.clone().unwrap();
        assert_eq!(link.source_id, 48295);
        assert!(link.source_sections.is_empty() && link.reviewed);
        assert_eq!(link.source_hash, "old", "the blog's record of the original, not the local one");
        assert_eq!(link.target_sections.len(), 1);
        assert_eq!(doc.body, "Existing text.\n");
        assert!(!syncstate::has_local_changes(&doc), "adopted as it is on the blog");
    }
}
