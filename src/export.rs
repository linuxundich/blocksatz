//! "Artikel exportieren" dialog: shows the Gutenberg block HTML that's about
//! to be sent, then publishes/updates the post via `wpclient`.
//!
//! `wpclient` is blocking (see its module docs for why), so the actual HTTP
//! work runs on a spawned `std::thread`, not the GTK thread. GTK widgets are
//! `!Send`, so the result comes back over a plain `std::sync::mpsc` channel
//! (`Send`-safe because it only ever carries owned strings/structs) that the
//! main thread polls with `glib::timeout_add_local` - the widget-touching
//! code all runs there, never inside the background thread.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk4::glib;
use webkit6::prelude::*;

use crate::document::{self, Document, Frontmatter, PostStatus, PostType};
use crate::i18n::tr;
use crate::{browser, linkcheck, media, mediapanel, notify, preview, secrets, syncstate, wpclient, wpsite};

/// Builds the "Artikel exportieren" wizard's shared bottom navigation bar -
/// an `Adw.CarouselIndicatorDots` centered between "Zurück"/"Weiter"
/// buttons, the same shape GNOME's own welcome/tour dialogs (GNOME Tour,
/// first-run screens, ...) use for a linear step flow, rather than the
/// header-bar-back-button convention `Adw.NavigationView` gives a settings-
/// style drill-down dialog. `pages` is every step's top-level widget, in
/// order - needed to resolve "the widget at position N" for `scroll_to`,
/// since `Adw.Carousel` (unlike `Adw.ViewStack`) has no such lookup of its
/// own. "Zurück" hides on the first page; "Weiter" hides on the last (the
/// export actions live in that page's own content instead of behind a
/// "continue" button).
fn wizard_nav_bar(carousel: &adw::Carousel, pages: Vec<gtk4::Widget>) -> gtk4::Widget {
    let indicator = adw::CarouselIndicatorDots::builder().carousel(carousel).build();
    let indicator_box = gtk4::Box::builder().hexpand(true).halign(gtk4::Align::Center).build();
    indicator_box.append(&indicator);

    let back_button = gtk4::Button::with_label(&tr("Zurück"));
    back_button.set_visible(false);
    let next_button = gtk4::Button::with_label(&tr("Weiter"));
    next_button.add_css_class("suggested-action");

    {
        let carousel = carousel.clone();
        let pages = pages.clone();
        back_button.connect_clicked(move |_| {
            let pos = carousel.position().round() as usize;
            if pos > 0 {
                // Not animated: each step's content is a real, dense
                // functional page (a link list, Medienverwaltung, the
                // Gutenberg HTML preview), not a decorative onboarding
                // slide - a sliding transition would show both pages
                // overlapping mid-swipe, whereas exactly one step should
                // ever be visible at a time.
                carousel.scroll_to(&pages[pos - 1], false);
            }
        });
    }
    {
        let carousel = carousel.clone();
        let pages = pages.clone();
        next_button.connect_clicked(move |_| {
            let pos = carousel.position().round() as usize;
            if pos + 1 < pages.len() {
                carousel.scroll_to(&pages[pos + 1], false);
            }
        });
    }
    {
        let back_button = back_button.clone();
        let next_button = next_button.clone();
        let last_index = pages.len().saturating_sub(1) as u32;
        carousel.connect_page_changed(move |_carousel, index| {
            back_button.set_visible(index > 0);
            next_button.set_visible(index < last_index);
        });
    }

    let bar = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(12)
        .margin_top(6)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    bar.append(&back_button);
    bar.append(&indicator_box);
    bar.append(&next_button);
    bar.upcast()
}

#[allow(clippy::too_many_arguments)]
pub fn open(
    parent: &adw::ApplicationWindow,
    body: String,
    frontmatter: Rc<RefCell<Frontmatter>>,
    doc_dir: Option<PathBuf>,
    preview_pane: Rc<preview::PreviewPane>,
    app_view_stack: &adw::ViewStack,
    browser_view: &Rc<browser::BrowserView>,
    save_document: DocumentSaver,
) {
    let site = wpsite::load();
    let current_fm = frontmatter.borrow().clone();

    let preview_label = gtk4::Label::builder().label(tr("Zu sendendes Gutenberg-HTML:")).xalign(0.0).build();

    // Reconciled fresh against `body` (not just trusting `current_fm.media`,
    // which reflects however recently `wire_live_preview`'s own debounce
    // last ran) - the same reasoning `mediapanel::build_content` below
    // already follows for the same "Medien" tab.
    let reconciled_media = media::reconcile(&current_fm.media, &body);
    let preview_buffer = gtk4::TextBuffer::new(None::<&gtk4::TextTagTable>);
    preview_buffer.set_text(&gutenberg_preview_html(&body, &reconciled_media));
    let preview_view = gtk4::TextView::builder()
        .buffer(&preview_buffer)
        .monospace(true)
        .editable(false)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(8)
        .right_margin(8)
        .build();
    let preview_scroller = gtk4::ScrolledWindow::builder()
        .child(&preview_view)
        .vexpand(true)
        .min_content_height(240)
        .build();

    // Same spacing/margins as `mediapanel::build_content`'s own outer box,
    // so switching between "Vorschau" and "Medien" doesn't visibly shift
    // the content's inset within the dialog.
    let preview_page = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(12)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .vexpand(true)
        .hexpand(true)
        .build();
    preview_page.append(&preview_label);
    preview_page.append(&preview_scroller);

    // Embedding Medienverwaltung here (not just linking to the separate
    // Ctrl+Shift+M dialog) lets alt text/captions/uploads be checked and
    // fixed right before publishing, in the same dialog - `build_content`
    // does its own `media::reconcile`, so this tab is always in sync with
    // the current body even if Medienverwaltung was never opened before.
    let media_page = mediapanel::build_content(frontmatter.clone(), &body, doc_dir.clone(), preview_pane.clone());
    media_page.set_hexpand(true);

    // Same one-shot-scan-at-open-time approach as the media tab above: a
    // pre-publish sanity pass, not a live watcher.
    let links_page = linkcheck::build_content(&body, app_view_stack, browser_view);
    links_page.set_hexpand(true);

    let status_label = gtk4::Label::new(None);
    status_label.set_wrap(true);
    status_label.set_xalign(0.0);

    // Shows the published post's real permalink as a clickable link once a
    // publish/draft/schedule succeeds - kept as its own widget rather than
    // embedding an `<a href>` in `status_label` via Pango markup, since
    // that label's other messages are plain, unescaped, server-provided
    // error text that could otherwise break markup parsing.
    let link_button = gtk4::LinkButton::builder().visible(false).halign(gtk4::Align::Start).build();

    let publish_button = gtk4::Button::with_label(&publish_button_label(&current_fm));
    publish_button.add_css_class("suggested-action");
    publish_button.set_halign(gtk4::Align::End);

    let draft_button = gtk4::Button::with_label(&tr("Als Entwurf hochladen"));
    draft_button.set_halign(gtk4::Align::End);

    // Only shown when "Geplant" is actually selected in den Artikel-
    // Eigenschaften - the third status the export dialog's two buttons
    // above can't express, since each forces its own fixed status
    // (`wire_publish_button`'s `target_status`) - visible so scheduling is
    // reachable at all, but only when it means something.
    let schedule_button = gtk4::Button::with_label(&tr("Terminieren"));
    schedule_button.set_halign(gtk4::Align::End);
    schedule_button.set_visible(current_fm.status == PostStatus::Future);

    // Same reasoning as `schedule_button` above, for the "Privat" status.
    let private_button = gtk4::Button::with_label(&tr("Privat veröffentlichen"));
    private_button.set_halign(gtk4::Align::End);
    private_button.set_visible(current_fm.status == PostStatus::Private);

    let delete_button = gtk4::Button::with_label(&tr("Von WordPress löschen"));
    delete_button.add_css_class("destructive-action");
    delete_button.set_halign(gtk4::Align::End);
    delete_button.set_visible(current_fm.wp_post_id.is_some());

    // Only useful for a post that already exists on WordPress but isn't
    // publicly published yet - once it's `Publish`, the real permalink
    // shown via `link_button` after a successful send already covers this.
    // Needs a login, once (see the tooltip): WordPress's `?preview=true`
    // convention only shows the current draft content to a session that's
    // logged into wp-admin as a user allowed to edit this post, and shows
    // an anonymous visitor the site's 404 page instead, because a draft
    // simply isn't public content. The Browser tab this opens it in and
    // the Live-Vorschau below both run on `websession::shared`, so one
    // login in either place covers both and outlives the app's restart.
    let preview_button = gtk4::Button::with_label(&tr("Vorschau öffnen"));
    preview_button.set_halign(gtk4::Align::End);
    preview_button.set_tooltip_text(Some(&tr(
        "Öffnet die WordPress-Vorschau im Browser-Tab. Entwürfe zeigt WordPress nur einer bei wp-admin angemeldeten Sitzung - andernfalls erscheint dort ein Login oder eine 404-Seite. Die Anmeldung im Browser-Tab bleibt gespeichert.",
    )));
    preview_button.set_visible(current_fm.wp_post_id.is_some() && current_fm.status != PostStatus::Publish);

    let button_row = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).spacing(6).halign(gtk4::Align::End).build();
    button_row.append(&delete_button);
    button_row.append(&preview_button);
    button_row.append(&draft_button);
    button_row.append(&schedule_button);
    button_row.append(&private_button);
    button_row.append(&publish_button);

    if site.url.is_empty() {
        status_label.set_label(&tr("Keine WordPress-Verbindung eingerichtet - bitte zuerst über den Verbindungs-Dialog konfigurieren."));
        publish_button.set_sensitive(false);
        draft_button.set_sensitive(false);
        schedule_button.set_sensitive(false);
        private_button.set_sensitive(false);
        delete_button.set_sensitive(false);
        preview_button.set_sensitive(false);
    } else if current_fm.title.is_empty() {
        status_label.set_label(&tr("Bitte zuerst einen Titel in den Artikel-Eigenschaften setzen."));
        publish_button.set_sensitive(false);
        draft_button.set_sensitive(false);
        schedule_button.set_sensitive(false);
        private_button.set_sensitive(false);
    }

    // Shows the article as it actually looks on the live site right after
    // a draft/update/publish succeeds (`wire_publish_button`/`start_export`
    // load it in here on success) - without this, the last wizard step used
    // to be just the status line and button row sitting above a lot of dead
    // space, especially before anything's been sent yet. A plain embedded
    // `WebKit.WebView`, not the fuller `browser::BrowserView` (no address
    // bar/adblock/back-forward needed for a one-shot "did this actually
    // work" check), but on the app's shared network session, so a draft
    // shows its content here rather than a 404 once the Browser tab has
    // been logged into wp-admin. `Adw.StatusPage` as a placeholder until
    // there's something to show.
    let export_preview_web_view = webkit6::WebView::builder().vexpand(true).hexpand(true).network_session(&crate::websession::shared()).build();
    let export_preview_placeholder = adw::StatusPage::builder()
        .icon_name("web-browser-symbolic")
        .title(tr("Noch keine Vorschau"))
        .description(tr(
            "Sobald der Artikel als Entwurf hochgeladen, aktualisiert oder veröffentlicht wurde, erscheint hier eine Vorschau der Live-Seite.",
        ))
        .vexpand(true)
        .build();
    let export_preview_stack = gtk4::Stack::new();
    export_preview_stack.add_named(&export_preview_placeholder, Some("placeholder"));
    export_preview_stack.add_named(&export_preview_web_view, Some("browser"));
    export_preview_stack.set_vexpand(true);
    export_preview_stack.set_hexpand(true);

    // A step-by-step wizard, not the free-roaming tabs this dialog used to
    // have - Links, then Medien, then Vorschau, then the actual export
    // actions. `Adw.Carousel` + `Adw.CarouselIndicatorDots`, the same
    // welcome/tour-dialog shape GNOME apps use for a linear flow like this
    // one, rather than `Adw.NavigationView`'s settings-style drill-down
    // (small header back-button) - see `wizard_nav_bar`.
    let export_actions = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(12)
        .margin_top(18)
        .margin_bottom(12)
        .margin_start(18)
        .margin_end(18)
        .build();
    export_actions.append(&status_label);
    export_actions.append(&link_button);
    export_actions.append(&button_row);

    let export_content = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).hexpand(true).vexpand(true).build();
    export_content.append(&export_actions);
    export_content.append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));
    export_content.append(&export_preview_stack);

    let pages: Vec<gtk4::Widget> = vec![links_page.clone(), media_page.clone(), preview_page.clone().upcast(), export_content.clone().upcast()];

    // `interactive(false)`: navigation is deliberately button-only (the
    // "Zurück"/"Weiter" pair in `wizard_nav_bar`), not swipe/scroll-wheel -
    // a manual drag would otherwise reveal a sliver of the neighboring
    // page mid-swipe, same reasoning as the non-animated `scroll_to` calls
    // there for why only one step should ever be on screen at once.
    // Each page above also needs its own `hexpand(true)` (not just the
    // carousel's) - confirmed live: `Adw.Carousel` sizes every page to its
    // own natural width unless the page itself expands to fill, so a
    // narrower page (e.g. Medien with no images yet) left empty space in
    // the carousel's viewport that the *neighboring* pages' content bled
    // into on both sides, even with `interactive(false)` and a
    // non-animated `scroll_to`.
    let carousel = adw::Carousel::builder().vexpand(true).hexpand(true).interactive(false).build();
    for page in &pages {
        carousel.append(page);
    }

    let nav_bar = wizard_nav_bar(&carousel, pages);

    let content_box = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    content_box.append(&carousel);
    content_box.append(&nav_bar);

    let header = adw::HeaderBar::new();
    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&content_box));

    // Taller than the other three steps' 640px need - the embedded preview
    // browser on the last step only earns its keep with real vertical room.
    let dialog = adw::Dialog::builder()
        .title(tr("Artikel exportieren"))
        .content_width(680)
        .content_height(760)
        .child(&toolbar_view)
        .build();

    {
        let frontmatter = frontmatter.clone();
        let status_label = status_label.clone();
        let link_button = link_button.clone();
        let preview_button_for_click = preview_button.clone();
        let export_preview_stack = export_preview_stack.clone();
        let export_preview_web_view = export_preview_web_view.clone();
        preview_button.connect_clicked(move |_| {
            let Some(post_id) = frontmatter.borrow().wp_post_id else { return };
            let rest_base = frontmatter.borrow().post_type.rest_base();
            preview_button_for_click.set_sensitive(false);
            status_label.set_label(&tr("Vorschau wird geladen …"));
            link_button.set_visible(false);

            let site = wpsite::load();
            let (tx, rx) = mpsc::channel::<Result<String, String>>();
            std::thread::spawn(move || {
                let outcome = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
                    .map_err(|err| err.to_string())
                    .and_then(|maybe_password| {
                        maybe_password.ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden."))
                    })
                    .and_then(|password| {
                        wpclient::Client::new(&site.url, &site.username, &password)
                            .get_item(rest_base, post_id)
                            .map(|detail| detail.link)
                            .map_err(|err| err.to_string())
                    })
                    .map(|link| preview_url_for(&link));
                let _ = tx.send(outcome);
            });

            let status_label = status_label.clone();
            let preview_button = preview_button_for_click.clone();
            let export_preview_stack = export_preview_stack.clone();
            let export_preview_web_view = export_preview_web_view.clone();
            glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
                Ok(Ok(preview_url)) => {
                    export_preview_web_view.load_uri(&preview_url);
                    export_preview_stack.set_visible_child_name("browser");
                    status_label.set_label(&tr(
                        "Vorschau unten geladen - dort ist ggf. eine Anmeldung bei wp-admin nötig, falls noch keine angemeldete Sitzung besteht.",
                    ));
                    preview_button.set_sensitive(true);
                    glib::ControlFlow::Break
                }
                Ok(Err(err)) => {
                    status_label.set_label(&tr("Fehler beim Laden der Vorschau: {err}").replace("{err}", &err));
                    preview_button.set_sensitive(true);
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    status_label.set_label(&tr("Interner Fehler: Vorschau-Thread hat kein Ergebnis geliefert."));
                    preview_button.set_sensitive(true);
                    glib::ControlFlow::Break
                }
            });
        });
    }

    let feedback = wizard_publish_feedback(&status_label, &link_button, &export_preview_stack, &export_preview_web_view);
    // The main button reads "Aktualisieren" (status left untouched - see
    // `TargetStatus::PublishOrKeep`) once the post exists and
    // "Veröffentlichen" (forces `publish`) before that. Both the label and
    // the status it sends follow the live `wp_post_id`, so a first
    // "Als Entwurf hochladen" in this same wizard turns the button into
    // "Aktualisieren" instead of leaving a "Veröffentlichen" button behind
    // that would only update the draft. The other three buttons are always
    // an explicit choice of status.
    let feedback = {
        let publish_button = publish_button.clone();
        let frontmatter = frontmatter.clone();
        let inner = feedback.on_success.clone();
        PublishFeedback {
            on_success: Rc::new(move |post, final_status| {
                inner(post, final_status);
                publish_button.set_label(&publish_button_label(&frontmatter.borrow()));
            }),
            ..feedback
        }
    };
    let dialog_widget: gtk4::Widget = dialog.clone().upcast();
    // Fixed closures, not read live from anywhere - the wizard is rebuilt
    // fresh from `body`/`doc_dir` every time it opens (see `open`'s own
    // parameters), so a plain snapshot is exactly right here.
    let get_body: BodyProvider = { let body = body.clone(); Rc::new(move || body.clone()) };
    let get_doc_dir: DocDirProvider = { let doc_dir = doc_dir.clone(); Rc::new(move || doc_dir.clone()) };
    wire_publish_button(&publish_button, &[&draft_button, &schedule_button, &private_button], TargetStatus::PublishOrKeep, &frontmatter, &get_body, &get_doc_dir, &feedback, &dialog_widget, &save_document);
    wire_publish_button(&draft_button, &[&publish_button, &schedule_button, &private_button], TargetStatus::Set(PostStatus::Draft), &frontmatter, &get_body, &get_doc_dir, &feedback, &dialog_widget, &save_document);
    wire_publish_button(&schedule_button, &[&publish_button, &draft_button, &private_button], TargetStatus::Set(PostStatus::Future), &frontmatter, &get_body, &get_doc_dir, &feedback, &dialog_widget, &save_document);
    wire_publish_button(&private_button, &[&publish_button, &draft_button, &schedule_button], TargetStatus::Set(PostStatus::Private), &frontmatter, &get_body, &get_doc_dir, &feedback, &dialog_widget, &save_document);
    wire_delete_button(&delete_button, &frontmatter, &dialog_widget, &feedback, {
        let status_label = status_label.clone();
        let publish_button = publish_button.clone();
        let delete_button = delete_button.clone();
        move || {
            status_label.set_label(&tr("Artikel wurde von WordPress gelöscht."));
            publish_button.set_label(&tr("Veröffentlichen"));
            delete_button.set_visible(false);
        }
    });

    dialog.present(Some(parent));
}

/// UI feedback hooks for the publish/delete flow (`wire_publish_button`,
/// `start_export`, `wire_delete_button`) - lets that shared logic report
/// progress/success/error without hard-coding which widgets show it, so the
/// exact same network/conflict-check/threading code can run from the
/// wizard's own status-label/link/embedded-preview-browser widgets (see
/// `wizard_publish_feedback`) or from a much thinner surface elsewhere (e.g.
/// a sidebar's toast + a `refresh()` call). The formatted message text
/// itself is always built by the shared logic, not by a callback - a
/// callback only ever renders whatever string it's given, so wording stays
/// centralized in one place regardless of how many UIs call into it.
type OnPublishSuccess = Rc<dyn Fn(&wpclient::PostResult, PostStatus)>;

/// Reads the article body/doc-dir to send, at the moment a publish button
/// is actually clicked rather than when it was wired up - the wizard's own
/// buttons are rebuilt fresh every time it opens, so a plain snapshot value
/// is equivalent there, but the document-management sidebar's buttons stay
/// wired for as long as the app runs, across many different documents and
/// edits - without this, they'd keep sending whatever content happened to
/// be current the moment the sidebar was first built. `wire_publish_button`
/// only ever calls these right before actually sending something.
pub(crate) type BodyProvider = Rc<dyn Fn() -> String>;
pub(crate) type DocDirProvider = Rc<dyn Fn() -> Option<PathBuf>>;
/// Writes the document back to disk after a successful export.
///
/// A successful send fills in `wp_post_id`, the per-image
/// `WordPressMediaRef`s and `wp_content_hash` - all of it in the shared
/// `Rc<RefCell<Frontmatter>>` and none of it in the editor's text buffer.
/// Without this, nothing ever persisted that: the "unsaved changes" check
/// compares the buffer against `saved_text`, so a frontmatter-only change
/// leaves the document looking untouched, and both autosave and the close
/// handler follow that same signal. Reopening the file therefore lost the
/// post id (the draft could no longer be updated, only published a second
/// time) and the upload refs (images looked local again and `sync_uploads`
/// re-uploaded them, creating duplicate attachments).
///
/// Deliberately takes no arguments: the caller owns the same
/// `Rc<RefCell<Frontmatter>>` this module just mutated, plus the buffer and
/// the path, so it reads the current state itself rather than being handed
/// a snapshot that could be stale by the time the upload finishes.
pub(crate) type DocumentSaver = Rc<dyn Fn()>;

#[derive(Clone)]
pub(crate) struct PublishFeedback {
    pub on_progress: Rc<dyn Fn(&str)>,
    pub on_success: OnPublishSuccess,
    pub on_error: Rc<dyn Fn(&str)>,
}

/// The wizard's own `PublishFeedback`: progress/error messages go to
/// `status_label` (hiding `link_button` while in progress, exactly as the
/// inline code used to), and a success loads the real post's permalink into
/// `link_button` plus the embedded preview browser - unpublished statuses
/// use WordPress's `?preview=true` convention (`preview_url_for`) since a
/// plain permalink would just 404/login for those.
pub(crate) fn wizard_publish_feedback(status_label: &gtk4::Label, link_button: &gtk4::LinkButton, preview_stack: &gtk4::Stack, preview_web_view: &webkit6::WebView) -> PublishFeedback {
    let on_progress = {
        let status_label = status_label.clone();
        let link_button = link_button.clone();
        move |message: &str| {
            status_label.set_label(message);
            link_button.set_visible(false);
        }
    };
    let on_success = {
        let status_label = status_label.clone();
        let link_button = link_button.clone();
        let preview_stack = preview_stack.clone();
        let preview_web_view = preview_web_view.clone();
        move |post: &wpclient::PostResult, final_status: PostStatus| {
            status_label.set_label(&tr("Erfolgreich gesendet:"));
            link_button.set_uri(&post.link);
            link_button.set_label(&post.link);
            link_button.set_visible(true);
            let preview_url = if final_status == PostStatus::Publish { post.link.clone() } else { preview_url_for(&post.link) };
            preview_web_view.load_uri(&preview_url);
            preview_stack.set_visible_child_name("browser");
        }
    };
    let on_error = {
        let status_label = status_label.clone();
        move |message: &str| status_label.set_label(message)
    };
    PublishFeedback { on_progress: Rc::new(on_progress), on_success: Rc::new(on_success), on_error: Rc::new(on_error) }
}

/// Appends WordPress's `preview=true` query parameter to a post's
/// permalink (`PostDetail::link`) - its documented convention for showing
/// an unpublished post's current content to a logged-in, authorized
/// session, used by the "Vorschau öffnen" button's click handler.
fn preview_url_for(link: &str) -> String {
    if link.contains('?') {
        format!("{link}&preview=true")
    } else {
        format!("{link}?preview=true")
    }
}

/// True when a re-fetched server content hash no longer matches the
/// locally-known baseline - i.e. the post changed on the server (most
/// likely edited directly in wp-admin) since it was last fetched or sent
/// from here. `local_hash` is `None` for a document never yet synced with
/// a server copy (see `Frontmatter::wp_content_hash`'s doc comment), in
/// which case there's no baseline to compare against, so this is never a
/// conflict.
fn has_conflicting_server_change(local_hash: Option<&str>, server_hash: &str) -> bool {
    local_hash.is_some_and(|local| local != server_hash)
}

/// Which `status` a publish-flow button sends, resolved from the live
/// `Frontmatter` at the moment the button is clicked - not when it was
/// wired up. That distinction matters for `PublishOrKeep`: the sidebar's
/// buttons stay wired for the whole app session, so a value computed once
/// at build time (before any document was even loaded, hence always
/// "no `wp_post_id` yet") made its "Aktualisieren" button force `publish`
/// and silently publish existing drafts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TargetStatus {
    /// Always send this status ("Als Entwurf hochladen", "Terminieren", ...).
    Set(PostStatus),
    /// "Veröffentlichen" for a post that doesn't exist on WordPress yet,
    /// "Aktualisieren" (status left untouched) once it does.
    PublishOrKeep,
}

/// Label for the button wired with `TargetStatus::PublishOrKeep`, kept in
/// one place so it can never disagree with what that button sends.
pub(crate) fn publish_button_label(frontmatter: &Frontmatter) -> String {
    if frontmatter.wp_post_id.is_some() { tr("Aktualisieren") } else { tr("Veröffentlichen") }
}

impl TargetStatus {
    pub(crate) fn resolve(self, frontmatter: &Frontmatter) -> Option<PostStatus> {
        match self {
            TargetStatus::Set(status) => Some(status),
            TargetStatus::PublishOrKeep if frontmatter.wp_post_id.is_some() => None,
            TargetStatus::PublishOrKeep => Some(PostStatus::Publish),
        }
    }
}

/// Wires one of the publish-flow buttons ("Veröffentlichen"/"Aktualisieren" /
/// "Als Entwurf hochladen" / "Terminieren" / "Privat veröffentlichen") -
/// `target_status` is sent regardless of whatever `Frontmatter.status`
/// happens to currently hold (e.g. from the separate "Artikel-Eigenschaften"
/// dialog), so clicking any one button is an unambiguous, deterministic
/// choice rather than depending on a status set somewhere else first. The
/// one exception is `None` (only ever passed for the "Aktualisieren" case -
/// see `open`'s own comment above its `wire_publish_button` calls): the
/// status field is omitted from the request entirely, so WordPress leaves
/// whatever status the post already has untouched. `other_buttons` are disabled
/// alongside `button` while a request is in flight, so none of them can
/// race the same post/media at once; on success `Frontmatter.status` is
/// updated to match, so "Artikel-Eigenschaften" reflects what was actually
/// just sent.
///
/// For a post that already exists on WordPress (`wp_post_id` set) and has
/// a known content baseline (`wp_content_hash` set - see its doc comment),
/// a click first re-fetches the post's current server content and compares
/// its hash against that baseline (`has_conflicting_server_change`) before
/// ever sending anything - if it doesn't match, the post was edited
/// somewhere else (wp-admin, most likely) since this article was last
/// synced, and `dialog_parent` hosts a confirmation dialog asking whether
/// to overwrite that change anyway, rather than silently clobbering it.
/// This check has no way to run for a brand new post (nothing to compare
/// against yet) or a document opened from a `.md` file written before this
/// field existed, so those publish immediately, same as before.
#[allow(clippy::too_many_arguments)]
pub(crate) fn wire_publish_button(
    button: &gtk4::Button,
    other_buttons: &[&gtk4::Button],
    target: TargetStatus,
    frontmatter: &Rc<RefCell<Frontmatter>>,
    get_body: &BodyProvider,
    get_doc_dir: &DocDirProvider,
    feedback: &PublishFeedback,
    dialog_parent: &gtk4::Widget,
    save_document: &DocumentSaver,
) {
    let other_buttons: Vec<gtk4::Button> = other_buttons.iter().map(|b| (*b).clone()).collect();
    let frontmatter = frontmatter.clone();
    let get_body = get_body.clone();
    let get_doc_dir = get_doc_dir.clone();
    let save_document = save_document.clone();
    let feedback = feedback.clone();
    let dialog_parent = dialog_parent.clone();

    let button_for_click = button.clone();
    button.connect_clicked(move |_| {
        let (post_id, local_hash, rest_base, target_status) = {
            let fm = frontmatter.borrow();
            (fm.wp_post_id, fm.wp_content_hash.clone(), fm.post_type.rest_base(), target.resolve(&fm))
        };
        let Some(post_id) = post_id.filter(|_| local_hash.is_some()) else {
            start_export(target_status, &frontmatter, &get_body(), &get_doc_dir(), &feedback, &button_for_click, &other_buttons, &save_document);
            return;
        };

        button_for_click.set_sensitive(false);
        for b in &other_buttons {
            b.set_sensitive(false);
        }
        (feedback.on_progress)(&tr("Prüfe auf Änderungen auf WordPress …"));

        let site = wpsite::load();
        let (tx, rx) = mpsc::channel::<Result<String, String>>();
        std::thread::spawn(move || {
            let outcome = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
                .map_err(|err| err.to_string())
                .and_then(|maybe_password| {
                    maybe_password.ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden."))
                })
                .and_then(|password| {
                    wpclient::Client::new(&site.url, &site.username, &password)
                        .get_item(rest_base, post_id)
                        .map(|detail| document::content_hash(&detail.content))
                        .map_err(|err| err.to_string())
                });
            let _ = tx.send(outcome);
        });

        let frontmatter = frontmatter.clone();
        let get_body = get_body.clone();
        let get_doc_dir = get_doc_dir.clone();
        let feedback = feedback.clone();
        let button = button_for_click.clone();
        let other_buttons = other_buttons.clone();
        let dialog_parent = dialog_parent.clone();
        let save_document = save_document.clone();
        glib::timeout_add_local(Duration::from_millis(150), move || {
            let proceed_directly = |feedback: &PublishFeedback| {
                start_export(target_status, &frontmatter, &get_body(), &get_doc_dir(), feedback, &button, &other_buttons, &save_document);
            };
            match rx.try_recv() {
                Ok(Ok(server_hash)) => {
                    if has_conflicting_server_change(local_hash.as_deref(), &server_hash) {
                        let confirm = adw::AlertDialog::new(
                            Some(&tr("Artikel wurde extern geändert")),
                            Some(&tr(
                                "Der Artikel wurde seit dem letzten Abruf/Senden direkt auf WordPress geändert - z. B. in wp-admin. Trotzdem mit der lokalen Version überschreiben?",
                            )),
                        );
                        confirm.add_response("cancel", &tr("Abbrechen"));
                        confirm.add_response("overwrite", &tr("Überschreiben"));
                        confirm.set_response_appearance("overwrite", adw::ResponseAppearance::Destructive);
                        confirm.set_default_response(Some("cancel"));
                        confirm.set_close_response("cancel");

                        let target_status = target_status;
                        let frontmatter = frontmatter.clone();
                        let get_body = get_body.clone();
                        let get_doc_dir = get_doc_dir.clone();
                        let feedback = feedback.clone();
                        let button = button.clone();
                        let other_buttons = other_buttons.clone();
                        let save_document = save_document.clone();
                        confirm.connect_response(None, move |_, response| {
                            if response == "overwrite" {
                                start_export(target_status, &frontmatter, &get_body(), &get_doc_dir(), &feedback, &button, &other_buttons, &save_document);
                            } else {
                                (feedback.on_progress)(&tr("Abgebrochen - lokale Änderungen wurden nicht gesendet."));
                                button.set_sensitive(true);
                                for b in &other_buttons {
                                    b.set_sensitive(true);
                                }
                            }
                        });
                        confirm.present(Some(&dialog_parent));
                    } else {
                        proceed_directly(&feedback);
                    }
                    glib::ControlFlow::Break
                }
                Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                    // Fail-open: the conflict check itself is a secondary
                    // safety net, not the actual publish attempt - if *it*
                    // can't complete (a transient network/auth hiccup), the
                    // real publish attempt right after will surface its own,
                    // more specific error if something is genuinely wrong.
                    // Refusing to publish at all just because this extra
                    // check failed would trade a rare conflict risk for a
                    // much more common "can't publish edits at all" one.
                    proceed_directly(&feedback);
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            }
        });
    });
}

/// Actually sends the article to WordPress - the second half of
/// `wire_publish_button`'s click handler, split out so it can be invoked
/// either immediately (no conflict to check, or nothing to check against)
/// or from the confirmation dialog's "Überschreiben" response.
#[allow(clippy::too_many_arguments)]
pub(crate) fn start_export(
    target_status: Option<PostStatus>,
    frontmatter: &Rc<RefCell<Frontmatter>>,
    body: &str,
    doc_dir: &Option<PathBuf>,
    feedback: &PublishFeedback,
    button: &gtk4::Button,
    other_buttons: &[gtk4::Button],
    save_document: &DocumentSaver,
) {
    button.set_sensitive(false);
    for b in other_buttons {
        b.set_sensitive(false);
    }
    (feedback.on_progress)(&tr("Wird gesendet …"));

    let site = wpsite::load();
    let mut current_fm = frontmatter.borrow().clone();
    if let Some(target_status) = target_status {
        current_fm.status = target_status;
    }
    let body = body.to_string();
    let sent_body = body.clone();
    let site_id = site.site_id();
    let doc_dir = doc_dir.clone();

    let (tx, rx) = mpsc::channel::<Result<(wpclient::PostResult, Vec<media::MediaItem>, Option<String>), String>>();
    std::thread::spawn(move || {
        let outcome = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .map_err(|err| err.to_string())
            .and_then(|maybe_password| {
                maybe_password.ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden."))
            })
            .and_then(|password| run_export(&site, &password, &mut current_fm, target_status, &body, doc_dir.as_deref()))
            .map(|post| (post, current_fm.media, current_fm.wp_content_hash));
        let _ = tx.send(outcome);
    });

    let frontmatter = frontmatter.clone();
    let feedback = feedback.clone();
    let button = button.clone();
    let other_buttons: Vec<gtk4::Button> = other_buttons.to_vec();
    let save_document = save_document.clone();
    glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
        Ok(Ok((post, media, content_hash))) => {
            let (title, final_status) = {
                let mut fm = frontmatter.borrow_mut();
                fm.wp_post_id = Some(post.id);
                fm.media = media;
                if let Some(target_status) = target_status {
                    fm.status = target_status;
                }
                fm.wp_content_hash = content_hash;
                // Fingerprinted against the body as it was *sent*: anything
                // typed while the upload ran is a local change still to go.
                let mut synced = Document { frontmatter: fm.clone(), body: sent_body.clone() };
                syncstate::mark_synced(&mut synced, &site_id, &post.modified_gmt, &syncstate::now_rfc3339());
                *fm = synced.frontmatter;
                (fm.title.clone(), fm.status)
            };
            // After the `borrow_mut` above has ended: `save_document`
            // borrows the same `RefCell` again, and doing this inside the
            // block would panic.
            save_document();
            (feedback.on_success)(&post, final_status);
            // Reflects what actually happened rather than always claiming
            // "Veröffentlicht" - `target_status` being `None` means the
            // status was deliberately left untouched (see `wire_publish_button`'s
            // doc comment), so the toast says "Aktualisiert" instead of
            // (incorrectly) implying a status change that didn't happen.
            let action_label = target_status.map(|s| s.label()).unwrap_or_else(|| tr("Aktualisiert"));
            notify::send("export", &action_label, &tr("„{title}“ wurde erfolgreich gesendet.").replace("{title}", &title));
            button.set_sensitive(true);
            for b in &other_buttons {
                b.set_sensitive(true);
            }
            glib::ControlFlow::Break
        }
        Ok(Err(err)) => {
            (feedback.on_error)(&tr("Fehler: {err}").replace("{err}", &err));
            notify::send("export", &tr("Veröffentlichen fehlgeschlagen"), &err);
            button.set_sensitive(true);
            for b in &other_buttons {
                b.set_sensitive(true);
            }
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            (feedback.on_error)(&tr("Interner Fehler: Export-Thread hat kein Ergebnis geliefert."));
            button.set_sensitive(true);
            for b in &other_buttons {
                b.set_sensitive(true);
            }
            glib::ControlFlow::Break
        }
    });
}

/// Wires "Von WordPress löschen": confirms via an `Adw.AlertDialog`
/// (parented to `dialog_parent`), then deletes the post via `wpclient` on a
/// background thread. Shared by the export wizard and the sidebar's own
/// delete action, same generalization as `wire_publish_button` above -
/// `feedback.on_success` is unused here (there's no `PostResult` for a
/// delete), only `on_progress`/`on_error`; `on_deleted` is the caller's own
/// "now update your widgets/state" hook for the one outcome specific to
/// this button.
pub(crate) fn wire_delete_button(button: &gtk4::Button, frontmatter: &Rc<RefCell<Frontmatter>>, dialog_parent: &gtk4::Widget, feedback: &PublishFeedback, on_deleted: impl Fn() + 'static) {
    let frontmatter = frontmatter.clone();
    let feedback = feedback.clone();
    let dialog_parent = dialog_parent.clone();
    let on_deleted = Rc::new(on_deleted);
    let button_for_click = button.clone();
    button.connect_clicked(move |_| {
        let Some(post_id) = frontmatter.borrow().wp_post_id else { return };
        let rest_base = frontmatter.borrow().post_type.rest_base();
        let confirm = adw::AlertDialog::new(
            Some(&tr("Artikel wirklich löschen?")),
            Some(&tr("Der Artikel wird unwiderruflich von der WordPress-Seite gelöscht.")),
        );
        confirm.add_response("cancel", &tr("Abbrechen"));
        confirm.add_response("delete", &tr("Löschen"));
        confirm.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        confirm.set_default_response(Some("cancel"));
        confirm.set_close_response("cancel");

        let frontmatter = frontmatter.clone();
        let feedback = feedback.clone();
        let button = button_for_click.clone();
        let on_deleted = on_deleted.clone();
        confirm.connect_response(None, move |_, response| {
            if response != "delete" {
                return;
            }
            (feedback.on_progress)(&tr("Wird gelöscht …"));
            button.set_sensitive(false);

            let site = wpsite::load();
            let (tx, rx) = mpsc::channel::<Result<(), String>>();
            std::thread::spawn(move || {
                let outcome = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
                    .map_err(|err| err.to_string())
                    .and_then(|maybe_password| {
                        maybe_password.ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden."))
                    })
                    .and_then(|password| {
                        wpclient::Client::new(&site.url, &site.username, &password)
                            .delete_item(rest_base, post_id)
                            .map_err(|err| err.to_string())
                    });
                let _ = tx.send(outcome);
            });

            let frontmatter = frontmatter.clone();
            let feedback = feedback.clone();
            let button = button.clone();
            let on_deleted = on_deleted.clone();
            glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
                Ok(Ok(())) => {
                    frontmatter.borrow_mut().wp_post_id = None;
                    on_deleted();
                    glib::ControlFlow::Break
                }
                Ok(Err(err)) => {
                    (feedback.on_error)(&tr("Fehler beim Löschen: {err}").replace("{err}", &err));
                    button.set_sensitive(true);
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    (feedback.on_error)(&tr("Interner Fehler: Lösch-Thread hat kein Ergebnis geliefert."));
                    button.set_sensitive(true);
                    glib::ControlFlow::Break
                }
            });
        });
        confirm.present(Some(&dialog_parent));
    });
}

fn run_export(
    site: &wpsite::SiteConfig,
    password: &str,
    frontmatter: &mut Frontmatter,
    target_status: Option<PostStatus>,
    body: &str,
    doc_dir: Option<&Path>,
) -> Result<wpclient::PostResult, String> {
    // WordPress only actually schedules a post if its `date` is genuinely
    // in the future - given a missing or past date, it silently publishes
    // immediately instead of scheduling (see `PostStatus::Future`'s doc
    // comment), so this is checked up front rather than letting that
    // surprise happen after an otherwise-successful export.
    if target_status == Some(PostStatus::Future) && frontmatter.scheduled_at.is_none() {
        return Err(tr("Für den Status „Geplant“ muss ein gültiger Veröffentlichungstermin gesetzt sein."));
    }

    let client = wpclient::Client::new(&site.url, &site.username, password);

    frontmatter.media = media::reconcile(&frontmatter.media, body);
    let uploaded_urls = media::sync_uploads(&client, &mut frontmatter.media, doc_dir)?;

    let mut blocks = gutenberg::parse_markdown(body);
    apply_media_metadata(&mut blocks, &frontmatter.media);
    rewrite_image_urls(&mut blocks, &uploaded_urls);
    let content = gutenberg::render_blocks(&blocks);

    let mut payload = serde_json::json!({
        "title": frontmatter.title,
        "content": content,
    });
    // Pages don't have categories/tags at all (WordPress registers neither
    // taxonomy for the `page` type) - skipped entirely rather than
    // resolving/creating terms that would then just be silently dropped.
    if frontmatter.post_type == PostType::Post {
        let mut category_ids = Vec::new();
        for name in &frontmatter.categories {
            category_ids.push(client.resolve_or_create_term("categories", name).map_err(|err| err.to_string())?);
        }
        let mut tag_ids = Vec::new();
        for name in &frontmatter.tags {
            tag_ids.push(client.resolve_or_create_term("tags", name).map_err(|err| err.to_string())?);
        }
        payload["categories"] = serde_json::json!(category_ids);
        payload["tags"] = serde_json::json!(tag_ids);
    }
    // Pages only, mirroring the categories/tags guard above - WordPress's
    // hierarchical-pages `parent` field doesn't exist on posts at all. Sent
    // unconditionally (unlike `author_id`'s "only when set"): `parent_id`
    // being `None` is itself a meaningful choice (top-level) that needs to
    // reach the server as `parent: 0`, not be left as a silent no-op that
    // could leave a stale parent from before in place.
    if frontmatter.post_type == PostType::Page {
        payload["parent"] = serde_json::json!(frontmatter.parent_id.unwrap_or(0));
    }
    // Omitted entirely (not just "left at whatever `frontmatter.status`
    // says") when `target_status` is `None` - WordPress's REST API leaves
    // an existing post's status untouched when the field is absent from
    // the request, which is exactly the "Aktualisieren" button's contract
    // (see `wire_publish_button`'s doc comment). Sending it unconditionally
    // here, even with the locally-known status, would risk re-asserting a
    // stale value if `Frontmatter.status` ever drifted from the post's real
    // status on the server.
    if let Some(target_status) = target_status {
        payload["status"] = serde_json::Value::String(target_status.as_str().to_string());
    }
    if !frontmatter.slug.is_empty() {
        payload["slug"] = serde_json::Value::String(frontmatter.slug.clone());
    }
    if let Some(excerpt) = &frontmatter.excerpt {
        payload["excerpt"] = serde_json::Value::String(excerpt.clone());
    }
    // RankMath registers these meta keys with `show_in_rest`, so they're
    // writable the same way as any other post meta; sent only when set, so
    // an unset field never overwrites a value already set directly in
    // RankMath's own editor. Harmless against a site without RankMath -
    // WordPress's REST API silently drops an unrecognized meta key rather
    // than erroring.
    let mut meta = serde_json::Map::new();
    if let Some(title) = &frontmatter.rank_math_title {
        meta.insert("rank_math_title".to_string(), serde_json::Value::String(title.clone()));
    }
    if let Some(description) = &frontmatter.rank_math_description {
        meta.insert("rank_math_description".to_string(), serde_json::Value::String(description.clone()));
    }
    if let Some(keyword) = &frontmatter.rank_math_focus_keyword {
        meta.insert("rank_math_focus_keyword".to_string(), serde_json::Value::String(keyword.clone()));
    }
    if !meta.is_empty() {
        payload["meta"] = serde_json::Value::Object(meta);
    }
    if target_status == Some(PostStatus::Future) {
        if let Some(scheduled_at) = &frontmatter.scheduled_at {
            payload["date"] = serde_json::Value::String(scheduled_at.clone());
        }
    }
    if let Some(path) = &frontmatter.featured_image {
        // Normally already uploaded via Medienverwaltung's own "Aufmacherbild"
        // row (`mediapanel::build_featured_image_row`) before publishing gets
        // this far, which clears `featured_image` in favor of
        // `featured_media_id` below. This is the fallback for a
        // `featured_image` set but never manually uploaded: upload it now and
        // use the resulting media id. Unlike body images, this always
        // re-uploads rather than going through `media::sync_uploads`'s hash
        // check - the featured image isn't a `MediaItem` at all (it's a
        // single Frontmatter field, never scanned from the Markdown body),
        // so it has no tracked content hash to compare against.
        let media = upload_image_file(&client, path, doc_dir).map_err(|err| err.to_string())?;
        payload["featured_media"] = serde_json::json!(media.id);
    } else if let Some(id) = frontmatter.featured_media_id {
        // Nothing new was set, but the document carries an existing
        // featured image from importing this post (see `importer.rs`) -
        // keep it rather than silently clearing it on re-export.
        payload["featured_media"] = serde_json::json!(id);
    }
    // Only sent when explicitly set (same "only when set" reasoning as the
    // RankMath meta fields above) - an unset `author_id` leaves the post's
    // existing author untouched rather than resetting it to whichever user
    // the Application Password belongs to. Setting it to a *different*
    // user requires that Application Password's own user to have
    // WordPress's `edit_others_posts` capability; a lower-privileged user
    // gets a normal, readable `rest_cannot_edit_others`-style error back
    // through the existing error path below, not a silent no-op.
    if let Some(author_id) = frontmatter.author_id {
        payload["author"] = serde_json::json!(author_id);
    }
    // Unlike `author`/`status` above, sent unconditionally rather than only
    // when explicitly touched - `false` (not ignored/tracked as normal) is
    // a safe default that matches how a post behaves before this is ever
    // touched at all, so there's no "unset" state worth distinguishing.
    // Silently dropped by WordPress if the "Worthy" plugin (or any plugin
    // registering this field) isn't installed on the target site, the same
    // way an unrecognized `meta` key is - see `Frontmatter::vgwort_ignored`.
    payload["wp-worthy-pixel"] = serde_json::json!({ "ignored": frontmatter.vgwort_ignored });
    // Only sent when explicitly set (same "only when set" reasoning as
    // `author_id` above) - an unset comment status leaves the post's
    // existing one untouched rather than resetting it to WordPress's own
    // site-wide default on every export.
    if let Some(open) = frontmatter.comment_status {
        payload["comment_status"] = serde_json::Value::String(if open { "open" } else { "closed" }.to_string());
    }

    let rest_base = frontmatter.post_type.rest_base();
    let result = match frontmatter.wp_post_id {
        Some(id) => client.update_item(rest_base, id, &payload),
        None => client.create_item(rest_base, &payload),
    }
    .map_err(|err| err.to_string())?;
    // `content` is exactly what the server now stores (WordPress's REST API
    // persists the `content` field verbatim, it doesn't re-serialize it on
    // save) - recording its hash here, not just on import, is what lets the
    // *next* update's conflict check (`wire_publish_button`) compare a
    // fresh fetch against a baseline that's actually still in sync, instead
    // of against a stale one from whenever this document was last opened.
    frontmatter.wp_content_hash = Some(document::content_hash(&content));
    Ok(result)
}

/// Renders `markdown` as Gutenberg block-comment HTML with Medienverwaltung's
/// alt-text/caption edits (`media`) already overlaid on top - the same
/// `apply_media_metadata` step `run_export` applies right before actually
/// publishing, exposed here so every *preview* of "the HTML that would be
/// sent" (the export dialog's own "Vorschau" tab, and the standalone
/// "Gutenberg-Code" tab in `codeview.rs`) shows the same thing that would
/// really be published, instead of silently reverting to whatever alt/
/// title text happens to be written literally in the Markdown source (a
/// plain `gutenberg::markdown_to_gutenberg` call has no way to know about
/// `Frontmatter.media` at all). Doesn't also apply `rewrite_image_urls` -
/// that substitutes a local path for its *uploaded* WordPress URL, which
/// only exists once an upload has actually happened, so showing the local
/// path here is the correct preview before that point.
pub(crate) fn gutenberg_preview_html(markdown: &str, media: &[media::MediaItem]) -> String {
    let mut blocks = gutenberg::parse_markdown(markdown);
    apply_media_metadata(&mut blocks, media);
    gutenberg::render_blocks(&blocks)
}

/// Overlays each image block's alt text/caption with the corresponding
/// `MediaItem`'s (matched by `source`, i.e. the block's still-original,
/// pre-`rewrite_image_urls` url - so this must run before that) - so an
/// edit made in Medienverwaltung actually reaches the published post's
/// HTML. Without this, `blocks` only ever carries whatever alt/title text
/// happens to be written literally in the Markdown source, since
/// Medienverwaltung's alt-text/caption editors (`mediapanel.rs`) only ever
/// update `Frontmatter.media`, never the body itself; the only place that
/// data otherwise reaches WordPress is `media::sync_uploads`'s
/// `update_media_metadata` call, which sets the *attachment's* alt
/// text/caption in the media library, not the `<img>`/`<figcaption>` baked
/// into this post's own content - and WordPress never re-reads an
/// attachment's current metadata into an already-published block.
/// `AltText::Undefined` (nothing decided yet) deliberately leaves the
/// parsed alt alone rather than blanking it.
fn apply_media_metadata(blocks: &mut [gutenberg::Block], media: &[media::MediaItem]) {
    for block in blocks.iter_mut() {
        match block {
            gutenberg::Block::Image { url, alt, title, media_id, width, height } => {
                if let Some(item) = media.iter().find(|item| &item.source == url) {
                    if let Some(text) = item.alt.as_wordpress_value() {
                        *alt = text.to_string();
                    }
                    *title = item.caption.clone();
                    if let Some(wp) = &item.wordpress {
                        *media_id = Some(wp.media_id);
                        *width = wp.width;
                        *height = wp.height;
                    }
                }
            }
            gutenberg::Block::BlockQuote { blocks } => apply_media_metadata(blocks, media),
            gutenberg::Block::List { items, .. } => {
                for item in items.iter_mut() {
                    apply_media_metadata(item, media);
                }
            }
            gutenberg::Block::Columns { columns } => {
                for column in columns.iter_mut() {
                    apply_media_metadata(column, media);
                }
            }
            gutenberg::Block::Details { blocks, .. } => apply_media_metadata(blocks, media),
            _ => {}
        }
    }
}

/// Recursively substitutes `wp:image`/`wp:video`/`wp:audio` blocks' source
/// with the WordPress URL `media::sync_uploads` resolved for it, wherever
/// the block's current url is a key in `urls` - an already-remote url (not
/// tracked by `sync_uploads` at all, e.g. an embed) simply has no matching
/// key and is left as-is.
fn rewrite_image_urls(blocks: &mut [gutenberg::Block], urls: &std::collections::HashMap<String, String>) {
    for block in blocks.iter_mut() {
        match block {
            gutenberg::Block::Image { url, .. } | gutenberg::Block::Video { url } | gutenberg::Block::Audio { url } => {
                if let Some(new_url) = urls.get(url) {
                    *url = new_url.clone();
                }
            }
            gutenberg::Block::BlockQuote { blocks } => rewrite_image_urls(blocks, urls),
            gutenberg::Block::List { items, .. } => {
                for item in items.iter_mut() {
                    rewrite_image_urls(item, urls);
                }
            }
            gutenberg::Block::Columns { columns } => {
                for column in columns.iter_mut() {
                    rewrite_image_urls(column, urls);
                }
            }
            gutenberg::Block::Gallery { images, .. } => {
                for image in images.iter_mut() {
                    if let Some(new_url) = urls.get(&image.url) {
                        image.url = new_url.clone();
                    }
                }
            }
            gutenberg::Block::Details { blocks, .. } => rewrite_image_urls(blocks, urls),
            _ => {}
        }
    }
}

/// Resolves a `MediaItem.source`/Markdown image path against the
/// document's own directory - shared by every place that needs the actual
/// file behind a local reference (`upload_image_file` here, and
/// `media::sync_uploads`/`hash_local_file`).
pub(crate) fn resolve_local_path(source: &str, base_dir: Option<&Path>) -> PathBuf {
    let path = Path::new(source);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.map(|dir| dir.join(path)).unwrap_or_else(|| path.to_path_buf())
    }
}

/// Reads the actual bytes behind a `MediaItem.source`/Markdown image
/// destination - a local path resolved against the document's own
/// directory (see `resolve_local_path`), or fetched over plain HTTP for an
/// already-remote `http(s)://` source (e.g. an image in an article opened
/// via "Von WordPress öffnen", which was never local to begin with). Used
/// by `aialt.rs`, which needs the real image bytes to send to a vision-
/// capable LLM - unlike `sync_uploads`, which only ever needs to *upload*
/// a changed local file and can skip a remote source entirely.
pub(crate) fn read_image_bytes(source: &str, base_dir: Option<&Path>) -> Result<Vec<u8>, String> {
    if source.starts_with("http://") || source.starts_with("https://") {
        let config = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).build();
        let agent = ureq::Agent::new_with_config(config);
        let mut response = agent.get(source).call().map_err(|err| tr("Bild nicht abrufbar: {err}").replace("{err}", &err.to_string()))?;
        response
            .body_mut()
            .read_to_vec()
            .map_err(|err| tr("Bild nicht lesbar: {err}").replace("{err}", &err.to_string()))
    } else {
        let resolved = resolve_local_path(source, base_dir);
        std::fs::read(&resolved).map_err(|err| tr("Bild {path} nicht lesbar: {err}").replace("{path}", &resolved.display().to_string()).replace("{err}", &err.to_string()))
    }
}

/// Uploads (or, for the mediapanel "Erneut hochladen" case, re-uploads) the
/// image behind `path_str` - a local path/reference, resolved against
/// `base_dir` the usual way, *or* an already-remote `http(s)://` source
/// (e.g. one picked from the WordPress media library via
/// `medialibrary.rs`, or an image opened from an already-imported
/// article) - fetched over plain HTTP instead of read from disk, via
/// `read_image_bytes`, the same helper `aialt.rs` already relies on for
/// this exact local-vs-remote distinction. Re-uploading an already-remote
/// image is an unusual but valid thing to click "Erneut hochladen" for
/// (forces a fresh attachment), so this fixes it rather than needing the
/// mediapanel UI to hide/disable the button for a remote source instead.
pub(crate) fn upload_image_file(client: &wpclient::Client, path_str: &str, base_dir: Option<&Path>) -> wpclient::Result<wpclient::MediaResult> {
    let bytes = read_image_bytes(path_str, base_dir).map_err(|message| wpclient::ApiError { status: 0, message })?;
    let filename = image_filename(path_str);
    let compressed = crate::imagecompress::maybe_compress(&bytes, &filename);
    client.upload_media(&compressed.bytes, &compressed.filename, compressed.mime_type)
}

/// The display filename for a `MediaItem.source`/Markdown image
/// destination - the last path segment for a local path, or a remote
/// URL's last path segment with any `?query` stripped first (a cache-
/// busting `?ver=`-style parameter shouldn't end up baked into the
/// re-uploaded attachment's filename).
fn image_filename(source: &str) -> String {
    let without_query = source.split('?').next().unwrap_or(source);
    without_query.rsplit(['/', '\\']).next().filter(|s| !s.is_empty()).unwrap_or("image").to_string()
}

pub(crate) fn mime_from_extension(filename: &str) -> &'static str {
    match filename.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "m4a" => "audio/mp4",
        "flac" => "audio/flac",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_or_keep_publishes_a_post_that_does_not_exist_yet() {
        let fm = Frontmatter::default();
        assert_eq!(TargetStatus::PublishOrKeep.resolve(&fm), Some(PostStatus::Publish));
        assert_eq!(publish_button_label(&fm), tr("Veröffentlichen"));
    }

    /// The bug this guards against: the sidebar resolved this once at
    /// build time (no document loaded yet), so updating an existing draft
    /// sent `status=publish`. Resolving against the live frontmatter must
    /// leave an existing post's status untouched - even a draft.
    #[test]
    fn publish_or_keep_leaves_an_existing_draft_alone() {
        let fm = Frontmatter { wp_post_id: Some(42), status: PostStatus::Draft, ..Frontmatter::default() };
        assert_eq!(TargetStatus::PublishOrKeep.resolve(&fm), None);
        assert_eq!(publish_button_label(&fm), tr("Aktualisieren"));
    }

    #[test]
    fn explicit_target_status_ignores_the_post_state() {
        let fm = Frontmatter { wp_post_id: Some(42), status: PostStatus::Publish, ..Frontmatter::default() };
        assert_eq!(TargetStatus::Set(PostStatus::Draft).resolve(&fm), Some(PostStatus::Draft));
        assert_eq!(TargetStatus::Set(PostStatus::Future).resolve(&Frontmatter::default()), Some(PostStatus::Future));
    }

    #[test]
    fn no_conflict_when_there_is_no_known_baseline_to_compare_against() {
        assert!(!has_conflicting_server_change(None, "any-server-hash"));
    }

    #[test]
    fn no_conflict_when_the_server_hash_still_matches_the_baseline() {
        assert!(!has_conflicting_server_change(Some("abc123"), "abc123"));
    }

    #[test]
    fn conflict_when_the_server_hash_no_longer_matches_the_baseline() {
        assert!(has_conflicting_server_change(Some("abc123"), "def456"));
    }

    #[test]
    fn preview_url_appends_preview_true_to_a_plain_permalink() {
        assert_eq!(preview_url_for("https://example.com/?p=123"), "https://example.com/?p=123&preview=true");
    }

    #[test]
    fn preview_url_appends_preview_true_with_a_question_mark_when_the_link_has_no_query_string() {
        assert_eq!(preview_url_for("https://example.com/my-slug/"), "https://example.com/my-slug/?preview=true");
    }

    #[test]
    fn rewrite_image_urls_recurses_into_columns_and_gallery_blocks() {
        let urls: std::collections::HashMap<String, String> =
            [("local-a.png".to_string(), "https://example.com/a.png".to_string()), ("local-b.png".to_string(), "https://example.com/b.png".to_string())]
                .into_iter()
                .collect();
        let mut blocks = vec![
            gutenberg::Block::Columns {
                columns: vec![vec![gutenberg::Block::Image { url: "local-a.png".to_string(), alt: String::new(), title: None, media_id: None, width: 0, height: 0 }]],
            },
            gutenberg::Block::Gallery {
                images: vec![gutenberg::GalleryImage { url: "local-b.png".to_string(), alt: String::new(), caption: None }],
                settings: gutenberg::GallerySettings::default(),
            },
        ];
        rewrite_image_urls(&mut blocks, &urls);
        let gutenberg::Block::Columns { columns } = &blocks[0] else { panic!("expected Columns") };
        let gutenberg::Block::Image { url, .. } = &columns[0][0] else { panic!("expected Image") };
        assert_eq!(url, "https://example.com/a.png");
        let gutenberg::Block::Gallery { images, .. } = &blocks[1] else { panic!("expected Gallery") };
        assert_eq!(images[0].url, "https://example.com/b.png");
    }

    #[test]
    fn rewrite_image_urls_recurses_into_details_blocks() {
        let urls: std::collections::HashMap<String, String> = [("local-c.png".to_string(), "https://example.com/c.png".to_string())].into_iter().collect();
        let mut blocks = vec![gutenberg::Block::Details {
            summary: "Mehr anzeigen".to_string(),
            blocks: vec![gutenberg::Block::Image { url: "local-c.png".to_string(), alt: String::new(), title: None, media_id: None, width: 0, height: 0 }],
        }];
        rewrite_image_urls(&mut blocks, &urls);
        let gutenberg::Block::Details { blocks: inner, .. } = &blocks[0] else { panic!("expected Details") };
        let gutenberg::Block::Image { url, .. } = &inner[0] else { panic!("expected Image") };
        assert_eq!(url, "https://example.com/c.png");
    }

    #[test]
    fn gutenberg_preview_html_reflects_medienverwaltung_edits_not_just_the_markdown_source() {
        // Regression test: the "Gutenberg-Code" tab and the export dialog's
        // own "Vorschau" tab both used to call `gutenberg::markdown_to_gutenberg`
        // directly, which has no way to know about `Frontmatter.media` at
        // all - so a caption set in Medienverwaltung (with nothing written
        // as a Markdown image title) silently never showed up in either
        // preview, even though the real `run_export` publish path already
        // applied it correctly.
        let media = vec![media::MediaItem {
            id: "media-001".to_string(),
            filename: "cat.png".to_string(),
            source: "cat.png".to_string(),
            alt: media::AltText::Text("a red cat".to_string()),
            caption: Some("Our cat, sleeping".to_string()),
            wordpress: None,
            last_markdown_caption: None,
        }];
        let html = gutenberg_preview_html("![a red cat](cat.png)", &media);
        assert!(html.contains("<figcaption class=\"wp-element-caption\">Our cat, sleeping</figcaption>"), "{html}");
    }

    #[test]
    fn apply_media_metadata_overlays_alt_text_and_caption_from_the_matching_media_item() {
        let media = vec![media::MediaItem {
            id: "media-001".to_string(),
            filename: "cat.png".to_string(),
            source: "cat.png".to_string(),
            alt: media::AltText::Text("a red cat".to_string()),
            caption: Some("Our cat, sleeping".to_string()),
            wordpress: None,
            last_markdown_caption: None,
        }];
        let mut blocks = vec![gutenberg::Block::Image { url: "cat.png".to_string(), alt: String::new(), title: None, media_id: None, width: 0, height: 0 }];
        apply_media_metadata(&mut blocks, &media);
        let gutenberg::Block::Image { alt, title, .. } = &blocks[0] else { panic!("expected Image") };
        assert_eq!(alt, "a red cat");
        assert_eq!(title.as_deref(), Some("Our cat, sleeping"));
    }

    #[test]
    fn apply_media_metadata_leaves_alt_untouched_while_undefined() {
        let media = vec![media::MediaItem {
            id: "media-001".to_string(),
            filename: "cat.png".to_string(),
            source: "cat.png".to_string(),
            alt: media::AltText::Undefined,
            caption: None,
            wordpress: None,
            last_markdown_caption: None,
        }];
        let mut blocks = vec![gutenberg::Block::Image { url: "cat.png".to_string(), alt: "from the markdown source".to_string(), title: None, media_id: None, width: 0, height: 0 }];
        apply_media_metadata(&mut blocks, &media);
        let gutenberg::Block::Image { alt, .. } = &blocks[0] else { panic!("expected Image") };
        assert_eq!(alt, "from the markdown source");
    }

    #[test]
    fn apply_media_metadata_fills_in_the_uploaded_attachment_id_and_dimensions() {
        let media = vec![media::MediaItem {
            id: "media-001".to_string(),
            filename: "cat.png".to_string(),
            source: "cat.png".to_string(),
            alt: media::AltText::Undefined,
            caption: None,
            wordpress: Some(media::WordPressMediaRef { media_id: 123, url: "https://example.com/cat.png".to_string(), content_hash: "abc".to_string(), width: 640, height: 480 }),
            last_markdown_caption: None,
        }];
        let mut blocks = vec![gutenberg::Block::Image { url: "cat.png".to_string(), alt: String::new(), title: None, media_id: None, width: 0, height: 0 }];
        apply_media_metadata(&mut blocks, &media);
        let gutenberg::Block::Image { media_id, width, height, .. } = &blocks[0] else { panic!("expected Image") };
        assert_eq!(*media_id, Some(123));
        assert_eq!((*width, *height), (640, 480));
    }

    /// Exercises the "Als Entwurf hochladen" vs "Veröffentlichen" choice
    /// directly: `run_export` must send whatever `target_status` it's given
    /// (the export-dialog buttons each force this to a specific value
    /// before calling it - see `wire_publish_button`), and a later call
    /// with a different status on the same `wp_post_id` must update it in
    /// place, not create a second post.
    #[test]
    #[ignore]
    fn run_export_respects_the_requested_post_status() {
        let site = wpsite::load();
        assert!(!site.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");
        let client = wpclient::Client::new(&site.url, &site.username, &password);

        let body = "Ein Testartikel für Entwurf/Veröffentlichen.\n";
        let mut frontmatter = Frontmatter {
            title: "Blocksatz draft/publish status test".to_string(),
            post_type: PostType::Post,
            slug: String::new(),
            status: crate::document::PostStatus::Draft,
            scheduled_at: None,
            categories: Vec::new(),
            tags: Vec::new(),
            excerpt: None,
            rank_math_title: None,
            rank_math_description: None,
            rank_math_focus_keyword: None,
            featured_image: None,
            featured_image_alt: None,
            wp_post_id: None,
            wp_content_hash: None,
            wp_site: None,
            wp_modified_gmt: None,
            wp_synced_hash: None,
            wp_synced_at: None,
            featured_media_id: None,
            author_id: None,
            author_name: None,
            parent_id: None,
            parent_name: None,
            vgwort_ignored: false,
            comment_status: None,
            media: Vec::new(),
        };

        let created = run_export(&site, &password, &mut frontmatter, Some(crate::document::PostStatus::Draft), body, None).expect("draft export failed");
        assert_eq!(client.get_post(created.id).expect("get_post failed").status, "draft");

        frontmatter.wp_post_id = Some(created.id);
        let updated =
            run_export(&site, &password, &mut frontmatter, Some(crate::document::PostStatus::Publish), body, None).expect("publish export failed");
        assert_eq!(updated.id, created.id, "updating status must reuse the same post, not create a new one");
        assert_eq!(client.get_post(updated.id).expect("get_post failed").status, "publish");

        client.delete_post(created.id).expect("cleanup delete_post failed");
    }

    /// Regression test for the "Aktualisieren" button silently republishing
    /// a draft: `target_status: None` must send a request with no `status`
    /// field at all, so WordPress leaves the post's existing status
    /// untouched - a draft updated this way must still be a draft
    /// afterwards, not flip to published.
    #[test]
    #[ignore]
    fn run_export_with_no_target_status_leaves_the_existing_status_untouched() {
        let site = wpsite::load();
        assert!(!site.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");
        let client = wpclient::Client::new(&site.url, &site.username, &password);

        let body = "Ein Testartikel für das Aktualisieren-ohne-Statuswechsel-Verhalten.\n";
        let mut frontmatter = Frontmatter {
            title: "Blocksatz update-without-status-change test".to_string(),
            post_type: PostType::Post,
            slug: String::new(),
            status: crate::document::PostStatus::Draft,
            scheduled_at: None,
            categories: Vec::new(),
            tags: Vec::new(),
            excerpt: None,
            rank_math_title: None,
            rank_math_description: None,
            rank_math_focus_keyword: None,
            featured_image: None,
            featured_image_alt: None,
            wp_post_id: None,
            wp_content_hash: None,
            wp_site: None,
            wp_modified_gmt: None,
            wp_synced_hash: None,
            wp_synced_at: None,
            featured_media_id: None,
            author_id: None,
            author_name: None,
            parent_id: None,
            parent_name: None,
            vgwort_ignored: false,
            comment_status: None,
            media: Vec::new(),
        };

        let created = run_export(&site, &password, &mut frontmatter, Some(crate::document::PostStatus::Draft), body, None).expect("draft export failed");
        assert_eq!(client.get_post(created.id).expect("get_post failed").status, "draft");

        frontmatter.wp_post_id = Some(created.id);
        let updated = run_export(&site, &password, &mut frontmatter, None, body, None).expect("status-less update failed");
        assert_eq!(updated.id, created.id, "updating content must reuse the same post, not create a new one");
        assert_eq!(client.get_post(updated.id).expect("get_post failed").status, "draft", "a status-less update must not change the post's status");

        client.delete_post(created.id).expect("cleanup delete_post failed");
    }

    /// Confirms `vgwort_ignored` actually reaches the real site and reads
    /// back correctly - both directions (setting it true, then flipping it
    /// back to false on the same post) - against the "Worthy" WordPress
    /// plugin's real `wp-worthy-pixel.ignored` REST field, not just that
    /// the local payload construction looks right.
    #[test]
    #[ignore]
    fn run_export_round_trips_vgwort_ignored() {
        let site = wpsite::load();
        assert!(!site.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");
        let client = wpclient::Client::new(&site.url, &site.username, &password);

        let body = "Ein Testartikel für den VG-Wort-Toggle.\n";
        let mut frontmatter = Frontmatter {
            title: "Blocksatz vgwort_ignored round-trip test".to_string(),
            post_type: PostType::Post,
            slug: String::new(),
            status: crate::document::PostStatus::Draft,
            scheduled_at: None,
            categories: Vec::new(),
            tags: Vec::new(),
            excerpt: None,
            rank_math_title: None,
            rank_math_description: None,
            rank_math_focus_keyword: None,
            featured_image: None,
            featured_image_alt: None,
            wp_post_id: None,
            wp_content_hash: None,
            wp_site: None,
            wp_modified_gmt: None,
            wp_synced_hash: None,
            wp_synced_at: None,
            featured_media_id: None,
            author_id: None,
            author_name: None,
            parent_id: None,
            parent_name: None,
            vgwort_ignored: true,
            comment_status: None,
            media: Vec::new(),
        };

        let created = run_export(&site, &password, &mut frontmatter, Some(crate::document::PostStatus::Draft), body, None).expect("draft export failed");
        assert!(client.get_post(created.id).expect("get_post failed").vgwort_ignored, "expected the post to come back marked as VG-Wort-ignored");

        frontmatter.wp_post_id = Some(created.id);
        frontmatter.vgwort_ignored = false;
        run_export(&site, &password, &mut frontmatter, None, body, None).expect("update failed");
        assert!(!client.get_post(created.id).expect("get_post failed").vgwort_ignored, "expected the post to come back no longer VG-Wort-ignored");

        client.delete_post(created.id).expect("cleanup delete_post failed");
    }

    /// Confirms `comment_status` actually reaches the real site and reads
    /// back correctly - both directions (closing comments, then reopening
    /// them on the same post) - against WordPress's own `comment_status`
    /// field, not just that the local payload construction looks right.
    #[test]
    #[ignore]
    fn run_export_round_trips_comment_status() {
        let site = wpsite::load();
        assert!(!site.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");
        let client = wpclient::Client::new(&site.url, &site.username, &password);

        let body = "Ein Testartikel für den Kommentar-Status-Toggle.\n";
        let mut frontmatter = Frontmatter {
            title: "Blocksatz comment_status round-trip test".to_string(),
            post_type: PostType::Post,
            slug: String::new(),
            status: crate::document::PostStatus::Draft,
            scheduled_at: None,
            categories: Vec::new(),
            tags: Vec::new(),
            excerpt: None,
            rank_math_title: None,
            rank_math_description: None,
            rank_math_focus_keyword: None,
            featured_image: None,
            featured_image_alt: None,
            wp_post_id: None,
            wp_content_hash: None,
            wp_site: None,
            wp_modified_gmt: None,
            wp_synced_hash: None,
            wp_synced_at: None,
            featured_media_id: None,
            author_id: None,
            author_name: None,
            parent_id: None,
            parent_name: None,
            vgwort_ignored: false,
            comment_status: Some(false),
            media: Vec::new(),
        };

        let created = run_export(&site, &password, &mut frontmatter, Some(crate::document::PostStatus::Draft), body, None).expect("draft export failed");
        assert_eq!(client.get_post(created.id).expect("get_post failed").comment_status, "closed");

        frontmatter.wp_post_id = Some(created.id);
        frontmatter.comment_status = Some(true);
        run_export(&site, &password, &mut frontmatter, None, body, None).expect("update failed");
        assert_eq!(client.get_post(created.id).expect("get_post failed").comment_status, "open");

        client.delete_post(created.id).expect("cleanup delete_post failed");
    }

    /// Exercises `run_export`'s own composition (local image path
    /// resolution + upload, category/tag name resolution, frontmatter ->
    /// REST payload mapping) against the real, already-configured
    /// WordPress site - not just `wpclient`'s lower-level calls, which
    /// `wpclient::tests` already covers. Ignored by default; run explicitly
    /// with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn run_export_with_local_image_and_terms_against_real_site() {
        let site = wpsite::load();
        assert!(!site.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");

        let doc_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let body = "# Blocksatz export test\n\nSome **text** with a local image below.\n\n![a red pixel](pixel.png)\n";

        let mut frontmatter = Frontmatter {
            title: "Blocksatz export test post".to_string(),
            post_type: PostType::Post,
            slug: String::new(),
            status: crate::document::PostStatus::Draft,
            scheduled_at: None,
            categories: vec!["Blocksatz Export Test".to_string()],
            tags: vec!["blocksatz-test".to_string()],
            excerpt: None,
            rank_math_title: None,
            rank_math_description: None,
            rank_math_focus_keyword: None,
            featured_image: None,
            featured_image_alt: None,
            wp_post_id: None,
            wp_content_hash: None,
            wp_site: None,
            wp_modified_gmt: None,
            wp_synced_hash: None,
            wp_synced_at: None,
            featured_media_id: None,
            author_id: None,
            author_name: None,
            parent_id: None,
            parent_name: None,
            vgwort_ignored: false,
            comment_status: None,
            media: Vec::new(),
        };

        let created =
            run_export(&site, &password, &mut frontmatter, Some(crate::document::PostStatus::Draft), body, Some(&doc_dir)).expect("run_export failed");
        assert!(created.id > 0);

        // `run_export` reconciles + uploads media as a side effect - confirm
        // it actually tracked and uploaded the one local image, not just
        // that the post itself was created.
        assert_eq!(frontmatter.media.len(), 1);
        let uploaded = frontmatter.media[0].wordpress.clone().expect("expected the local image to have been uploaded");
        assert!(uploaded.media_id > 0);
        assert!(!uploaded.content_hash.is_empty());

        // Cleanup: the post, the uploaded media item, and the category/tag
        // terms `run_export` created.
        let client = wpclient::Client::new(&site.url, &site.username, &password);
        client.delete_post(created.id).expect("cleanup delete_post failed");
        client.delete_media(uploaded.media_id).expect("cleanup delete_media failed");
    }

    /// Exercises the duplicate-upload fix directly: exporting the same
    /// unchanged local image twice must reuse the same WordPress media id
    /// both times, not create a second attachment.
    #[test]
    #[ignore]
    fn run_export_does_not_reupload_an_unchanged_local_image() {
        let site = wpsite::load();
        assert!(!site.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");

        let doc_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let body = "# Blocksatz re-export test\n\n![a red pixel](pixel.png)\n";

        let mut frontmatter = Frontmatter {
            title: "Blocksatz re-export test post".to_string(),
            post_type: PostType::Post,
            slug: String::new(),
            status: crate::document::PostStatus::Draft,
            scheduled_at: None,
            categories: Vec::new(),
            tags: Vec::new(),
            excerpt: None,
            rank_math_title: None,
            rank_math_description: None,
            rank_math_focus_keyword: None,
            featured_image: None,
            featured_image_alt: None,
            wp_post_id: None,
            wp_content_hash: None,
            wp_site: None,
            wp_modified_gmt: None,
            wp_synced_hash: None,
            wp_synced_at: None,
            featured_media_id: None,
            author_id: None,
            author_name: None,
            parent_id: None,
            parent_name: None,
            vgwort_ignored: false,
            comment_status: None,
            media: Vec::new(),
        };

        let first =
            run_export(&site, &password, &mut frontmatter, Some(crate::document::PostStatus::Draft), body, Some(&doc_dir)).expect("first run_export failed");
        let first_media_id = frontmatter.media[0].wordpress.clone().expect("expected an upload on the first export").media_id;

        // Re-export the identical body/frontmatter (as an update, since
        // `wp_post_id` now carries over) - the image content hasn't
        // changed, so this must NOT create a second media attachment.
        frontmatter.wp_post_id = Some(first.id);
        let _second = run_export(&site, &password, &mut frontmatter, Some(crate::document::PostStatus::Draft), body, Some(&doc_dir))
            .expect("second run_export failed");
        let second_media_id = frontmatter.media[0].wordpress.clone().expect("expected the ref to survive re-export").media_id;

        assert_eq!(first_media_id, second_media_id, "re-exporting an unchanged local image must reuse the same WordPress media id");

        let client = wpclient::Client::new(&site.url, &site.username, &password);
        client.delete_post(first.id).expect("cleanup delete_post failed");
        client.delete_media(first_media_id).expect("cleanup delete_media failed");
    }

    #[test]
    fn read_image_bytes_reads_a_local_file_relative_to_the_doc_dir() {
        let dir = std::env::temp_dir().join(format!("blocksatz-read-image-bytes-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("photo.png"), b"not a real png, just test bytes").unwrap();

        let bytes = read_image_bytes("photo.png", Some(&dir)).expect("expected the local file to be readable");
        assert_eq!(bytes, b"not a real png, just test bytes");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_image_bytes_reports_a_readable_error_for_a_missing_local_file() {
        let dir = std::env::temp_dir().join(format!("blocksatz-read-image-bytes-missing-test-{}", std::process::id()));
        let err = read_image_bytes("nope.png", Some(&dir)).expect_err("expected a missing file to be an error");
        assert!(err.contains("nope.png"), "{err}");
    }

    #[test]
    fn image_filename_takes_the_last_path_segment_of_a_local_path() {
        assert_eq!(image_filename("cat.png"), "cat.png");
        assert_eq!(image_filename("photos/2026/cat.png"), "cat.png");
    }

    #[test]
    fn image_filename_takes_the_last_path_segment_of_a_remote_url_without_its_query() {
        assert_eq!(image_filename("https://example.com/wp-content/uploads/2026/01/cat.png?ver=2"), "cat.png");
        assert_eq!(image_filename("https://example.com/wp-content/uploads/2026/01/cat.png"), "cat.png");
    }
}
