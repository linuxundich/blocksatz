//! The collapsible left-hand document-management sidebar: browsing local
//! and WordPress articles ("Durchsuchen") and the currently-open
//! document's publish state ("Dokument"). Wraps `Adw.OverlaySplitView`
//! around `window.rs`'s existing `layout_view` (untouched) - this module
//! owns only the sidebar's own content, and reuses `window.rs`'s own
//! `DocContext` bundle rather than a second, field-for-field-identical one.
//!
//! Replaces two things that used to live directly in the headerbar: the
//! "Zuletzt geöffnet" popover (now this sidebar's "Durchsuchen" → "Lokal"
//! list, still backed by `recentfiles.rs`) and the "Von WordPress öffnen"
//! modal dialog (now "Durchsuchen" → "WordPress", `importer::build_content`
//! embedded instead of dialog-wrapped) - and consolidates the Properties
//! dialog's status/type dropdowns plus the export wizard's quick-publish
//! buttons into one always-visible "Dokument" page, via the same
//! `statuscontrols`/`export` functions those surfaces themselves use, so
//! all of these stay perfectly in sync with each other.
//!
//! Not `"sidebar"`: that string is already `window.rs`'s own name for the
//! `Adw.LayoutSlot` holding the *preview* pane inside its narrow layout -
//! unrelated to this module, and nothing here reuses or renames it.

use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;

use crate::document::{Document, PostStatus, PostType};
use crate::i18n::tr;
use crate::window::{self, DocContext};
use crate::{autosave, browser, export, importer, recentfiles, statuscontrols};

pub struct DocSidebar {
    pub split_view: adw::OverlaySplitView,
    /// Switches to "Durchsuchen" → "WordPress" (the list of the site's
    /// posts) - backs `win.open-from-wordpress` (Ctrl+Shift+O). Showing the
    /// sidebar itself is left to the caller, which owns the toggle action.
    pub show_wordpress_posts: Rc<dyn Fn()>,
}

/// Handles the sidebar needs beyond `DocContext` - opening the export
/// wizard (`export::open`) for its "Vor Veröffentlichung prüfen…" button
/// needs the same `view_stack`/`browser_view` handles `wire_publish_action`
/// already threads through for the identical call in `window.rs`.
#[derive(Clone)]
pub struct DocSidebarExtras {
    pub view_stack: adw::ViewStack,
    pub browser_view: Rc<browser::BrowserView>,
}

pub fn build(window: &adw::ApplicationWindow, ctx: &DocContext, extras: &DocSidebarExtras, content: &impl IsA<gtk4::Widget>) -> DocSidebar {
    let view_stack = adw::ViewStack::new();

    let (document_page, refresh_document_page) = build_document_page(window, ctx, extras);
    view_stack.add_titled_with_icon(&document_page, Some("document"), &tr("Dokument"), "document-properties-symbolic");

    let on_document_loaded: Rc<dyn Fn()> = {
        let view_stack = view_stack.clone();
        let refresh_document_page = refresh_document_page.clone();
        Rc::new(move || {
            refresh_document_page();
            view_stack.set_visible_child_name("document");
        })
    };
    let (browse_page, wordpress_toggle) = build_browse_page(window, ctx, on_document_loaded);
    view_stack.add_titled_with_icon(&browse_page, Some("browse"), &tr("Durchsuchen"), "folder-symbolic");

    view_stack.set_visible_child_name("document");

    let switcher = adw::InlineViewSwitcher::builder().stack(&view_stack).build();
    let switcher_bar = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).halign(gtk4::Align::Center).margin_top(6).margin_bottom(6).build();
    switcher_bar.append(&switcher);

    let sidebar_box = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    sidebar_box.append(&switcher_bar);
    sidebar_box.append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));
    sidebar_box.append(&view_stack);

    let split_view = adw::OverlaySplitView::builder()
        .sidebar(&sidebar_box)
        .content(content)
        .sidebar_position(gtk4::PackType::Start)
        .show_sidebar(false)
        .min_sidebar_width(260.0)
        .max_sidebar_width(420.0)
        .sidebar_width_unit(adw::LengthUnit::Sp)
        .build();

    let show_wordpress_posts: Rc<dyn Fn()> = {
        let view_stack = view_stack.clone();
        Rc::new(move || {
            view_stack.set_visible_child_name("browse");
            wordpress_toggle.set_active(true);
        })
    };

    DocSidebar { split_view, show_wordpress_posts }
}

/// The current document's own directory - `None` for a document never yet
/// saved locally (a brand-new document, or one opened from WordPress and
/// not yet given a local copy - see the "Lokal speichern unter…" row
/// below), same derivation `window.rs` uses wherever it needs this.
fn doc_dir(ctx: &DocContext) -> Option<PathBuf> {
    ctx.current_path.borrow().as_ref().and_then(|p| p.parent().map(PathBuf::from))
}

/// Builds the "Dokument" page: a header showing what's open, the shared
/// `Typ`/`Status` controls (`statuscontrols`, the same ones the Properties
/// dialog uses), and the publish/delete/save-local/pre-flight-check
/// actions. Returns the page alongside a `refresh()` closure that re-reads
/// `Frontmatter`/`current_path` and updates every dynamic bit - called once
/// here at construction, and again by `build_browse_page` whenever a
/// different document gets loaded, and after every publish/delete/save
/// that changes what should be shown.
fn build_document_page(window: &adw::ApplicationWindow, ctx: &DocContext, extras: &DocSidebarExtras) -> (gtk4::Widget, Rc<dyn Fn()>) {
    let header_title = gtk4::Label::builder().xalign(0.0).wrap(true).build();
    header_title.add_css_class("title-3");
    let header_subtitle = gtk4::Label::builder().xalign(0.0).wrap(true).build();
    header_subtitle.add_css_class("dim-label");
    let header = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(2).build();
    header.append(&header_title);
    header.append(&header_subtitle);

    let type_row = statuscontrols::build_type_row(&ctx.frontmatter);
    let (status_row, scheduled_row) = statuscontrols::build_status_row(&ctx.frontmatter);
    let controls_group = adw::PreferencesGroup::new();
    controls_group.add(&type_row);
    controls_group.add(&status_row);
    controls_group.add(&scheduled_row);

    let publish_button = gtk4::Button::new();
    publish_button.add_css_class("suggested-action");
    let draft_button = gtk4::Button::with_label(&tr("Als Entwurf hochladen"));
    let private_button = gtk4::Button::with_label(&tr("Privat veröffentlichen"));
    let schedule_button = gtk4::Button::with_label(&tr("Terminieren"));
    let delete_button = gtk4::Button::with_label(&tr("Von WordPress löschen"));
    delete_button.add_css_class("destructive-action");
    let save_as_button = gtk4::Button::with_label(&tr("Lokal speichern unter…"));
    let check_button = gtk4::Button::with_label(&tr("Vor Veröffentlichung prüfen…"));
    check_button.add_css_class("flat");

    let buttons_box = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(6).build();
    for button in [&publish_button, &draft_button, &private_button, &schedule_button, &delete_button, &save_as_button, &check_button] {
        buttons_box.append(button);
    }

    let page = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(18)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(12)
        .margin_end(12)
        .build();
    page.append(&header);
    page.append(&controls_group);
    page.append(&buttons_box);

    let refresh: Rc<dyn Fn()> = {
        let ctx = ctx.clone();
        let header_title = header_title.clone();
        let header_subtitle = header_subtitle.clone();
        let type_row = type_row.clone();
        let status_row = status_row.clone();
        let publish_button = publish_button.clone();
        let private_button = private_button.clone();
        let schedule_button = schedule_button.clone();
        let delete_button = delete_button.clone();
        let save_as_button = save_as_button.clone();
        Rc::new(move || {
            let fm = ctx.frontmatter.borrow().clone();
            let path = ctx.current_path.borrow().clone();
            header_title.set_label(&window::subtitle_for(path.as_deref(), &fm));
            header_subtitle.set_label(&match (&path, fm.wp_post_id) {
                (Some(path), _) => path.display().to_string(),
                (None, Some(id)) => tr("Nur auf WordPress (#{id})").replace("{id}", &id.to_string()),
                (None, None) => tr("Neues, noch nicht gespeichertes Dokument"),
            });
            type_row.set_selected(PostType::ALL.iter().position(|t| *t == fm.post_type).unwrap_or(0) as u32);
            type_row.set_sensitive(fm.wp_post_id.is_none());
            status_row.set_selected(PostStatus::ALL.iter().position(|s| *s == fm.status).unwrap_or(0) as u32);
            publish_button.set_label(&export::publish_button_label(&fm));
            private_button.set_visible(fm.status == PostStatus::Private);
            schedule_button.set_visible(fm.status == PostStatus::Future);
            delete_button.set_visible(fm.wp_post_id.is_some());
            save_as_button.set_visible(path.is_none());
        })
    };
    refresh();

    // `get_body`/`get_doc_dir` read live from `ctx` at *click* time (not
    // captured once here) - unlike the export wizard, which is rebuilt
    // fresh every time it opens, these buttons stay wired for the whole
    // app session across however many different documents get opened - see
    // `export::BodyProvider`'s own doc comment.
    let get_body: export::BodyProvider = {
        let buffer = ctx.buffer.clone();
        Rc::new(move || buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string())
    };
    let get_doc_dir: export::DocDirProvider = {
        let ctx = ctx.clone();
        Rc::new(move || doc_dir(&ctx))
    };
    let dialog_parent: gtk4::Widget = window.clone().upcast();
    let save_document = crate::window::document_saver(ctx);
    let feedback = export::sidebar_publish_feedback(&ctx.toast_overlay, refresh.clone());

    export::wire_publish_button(&publish_button, &[&draft_button, &schedule_button, &private_button], export::TargetStatus::PublishOrKeep, &ctx.frontmatter, &get_body, &get_doc_dir, &feedback, &dialog_parent, &save_document);
    export::wire_publish_button(&draft_button, &[&publish_button, &schedule_button, &private_button], export::TargetStatus::Set(PostStatus::Draft), &ctx.frontmatter, &get_body, &get_doc_dir, &feedback, &dialog_parent, &save_document);
    export::wire_publish_button(&schedule_button, &[&publish_button, &draft_button, &private_button], export::TargetStatus::Set(PostStatus::Future), &ctx.frontmatter, &get_body, &get_doc_dir, &feedback, &dialog_parent, &save_document);
    export::wire_publish_button(&private_button, &[&publish_button, &draft_button, &schedule_button], export::TargetStatus::Set(PostStatus::Private), &ctx.frontmatter, &get_body, &get_doc_dir, &feedback, &dialog_parent, &save_document);
    export::wire_delete_button(&delete_button, &ctx.frontmatter, &dialog_parent, &feedback, {
        let refresh = refresh.clone();
        move || refresh()
    });

    {
        let window = window.clone();
        let ctx = ctx.clone();
        let refresh = refresh.clone();
        save_as_button.connect_clicked(move |_| {
            let body = ctx.buffer.text(&ctx.buffer.start_iter(), &ctx.buffer.end_iter(), false).to_string();
            let doc = Document { frontmatter: ctx.frontmatter.borrow().clone(), body };
            let refresh = refresh.clone();
            window::save_as(&window, &ctx, doc, move || refresh());
        });
    }
    {
        let window = window.clone();
        let ctx = ctx.clone();
        let extras = extras.clone();
        check_button.connect_clicked(move |_| {
            let body = ctx.buffer.text(&ctx.buffer.start_iter(), &ctx.buffer.end_iter(), false).to_string();
            export::open(&window, body, ctx.frontmatter.clone(), doc_dir(&ctx), ctx.preview_pane.clone(), &extras.view_stack, &extras.browser_view, crate::window::document_saver(&ctx));
        });
    }

    (page.upcast(), refresh)
}

/// Builds the "Durchsuchen" page: a `Lokal`/`WordPress` toggle over the
/// recent-local-files list (`recentfiles.rs`) and the embedded WordPress
/// article browser (`importer::build_content`). `on_document_loaded` fires
/// once either side has actually finished loading something into the
/// editor - switches back to the "Dokument" page and refreshes it.
fn build_browse_page(window: &adw::ApplicationWindow, ctx: &DocContext, on_document_loaded: Rc<dyn Fn()>) -> (gtk4::Widget, gtk4::ToggleButton) {
    let local_toggle = gtk4::ToggleButton::builder().label(tr("Lokal")).active(true).hexpand(true).build();
    let wordpress_toggle = gtk4::ToggleButton::builder().label(tr("WordPress")).group(&local_toggle).hexpand(true).build();
    let source_switcher = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).build();
    source_switcher.add_css_class("linked");
    source_switcher.append(&local_toggle);
    source_switcher.append(&wordpress_toggle);

    let local_page = build_local_page(ctx, on_document_loaded.clone());
    let wordpress_content = importer::build_content({
        let window = window.clone();
        let ctx = ctx.clone();
        let on_document_loaded = on_document_loaded.clone();
        move |imported| {
            apply_imported_post(&ctx, imported);
            on_document_loaded();
            let _ = &window; // kept for symmetry with `local_page`'s own closures; no dialog to anchor here anymore.
        }
    });

    let source_stack = gtk4::Stack::new();
    source_stack.add_named(&local_page, Some("local"));
    source_stack.add_named(&wordpress_content, Some("wordpress"));
    source_stack.set_visible_child_name("local");
    source_stack.set_vexpand(true);

    {
        let source_stack = source_stack.clone();
        wordpress_toggle.connect_toggled(move |toggle| {
            source_stack.set_visible_child_name(if toggle.is_active() { "wordpress" } else { "local" });
        });
    }

    let page = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(12)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(12)
        .margin_end(12)
        .build();
    page.append(&source_switcher);
    page.append(&source_stack);
    (page.upcast(), wordpress_toggle)
}

/// The `Lokal` sub-page: recent local files (`recentfiles::load()`, same
/// source the old "Zuletzt geöffnet" popover read), plus an "Öffnen…" row
/// that just delegates to `win.open` - keeping the file-picker itself a
/// single source of truth rather than a second copy of that dialog code.
fn build_local_page(ctx: &DocContext, on_document_loaded: Rc<dyn Fn()>) -> gtk4::Widget {
    let list = gtk4::ListBox::new();
    list.add_css_class("boxed-list");

    let scroller = gtk4::ScrolledWindow::builder().child(&list).vexpand(true).build();
    let page = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(12).build();
    page.append(&scroller);

    let refresh_list = {
        let list = list.clone();
        let ctx = ctx.clone();
        let on_document_loaded = on_document_loaded.clone();
        move || {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            // A permanent first row, rebuilt along with the rest rather
            // than sitting outside the `Gtk.ListBox` - an `Adw.ActionRow`'s
            // `activatable`/`activated` click handling only actually
            // engages as a child of a real `Gtk.ListBox`; one built as a
            // plain `Gtk.Box`'s direct child renders identically but never
            // fires (confirmed live: AT-SPI reported zero actions on it).
            let open_row = adw::ActionRow::builder().title(tr("Datei öffnen…")).activatable(true).build();
            open_row.add_prefix(&gtk4::Image::from_icon_name("document-open-symbolic"));
            open_row.connect_activated(|row| {
                let Some(root) = row.root() else { return };
                let Ok(window) = root.downcast::<gtk4::Window>() else { return };
                window.activate_action("win.open", None).ok();
            });
            list.append(&open_row);

            let entries = recentfiles::load();
            if entries.is_empty() {
                list.append(&adw::ActionRow::builder().title(tr("Keine zuletzt geöffneten Artikel")).activatable(false).build());
                return;
            }
            for path in entries {
                let filename = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.display().to_string());
                let parent = path.parent().map(|p| p.display().to_string()).unwrap_or_default();
                let row = adw::ActionRow::builder().title(filename).subtitle(parent).activatable(true).use_markup(false).build();
                let ctx = ctx.clone();
                let on_document_loaded = on_document_loaded.clone();
                row.connect_activated(move |_| {
                    window::open_document_at_path(path.clone(), &ctx);
                    on_document_loaded();
                });
                list.append(&row);
            }
        }
    };
    // Rebuilt every time this page becomes visible - a file opened,
    // renamed, or deleted elsewhere since the sidebar last showed this list
    // (or the sidebar simply never having refreshed it since launch) would
    // otherwise leave a stale list showing.
    {
        let refresh_list = refresh_list.clone();
        page.connect_map(move |_| refresh_list());
    }
    refresh_list();

    page.upcast()
}

/// What `wire_open_from_wordpress_action` used to do inline - fills the
/// editor from an `ImportedPost` and, since there's no local file for it
/// yet, clears `current_path` (see `importer.rs`'s own module doc comment
/// for why that stays a deliberate, separate step rather than something
/// this function also does).
fn apply_imported_post(ctx: &DocContext, imported: importer::ImportedPost) {
    ctx.buffer.set_text(&imported.body);
    ctx.title.set_subtitle(&window::subtitle_for(None, &imported.frontmatter));
    *ctx.saved_text.borrow_mut() = imported.body.clone();
    *ctx.frontmatter.borrow_mut() = imported.frontmatter;
    *ctx.current_path.borrow_mut() = None;
    ctx.preview_pane.set_doc_dir(None);
    ctx.preview_pane.set_article_header(&ctx.frontmatter.borrow());
    autosave::clear();
}
