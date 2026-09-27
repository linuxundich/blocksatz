//! "WordPress-Mediathek" dialog: browse and manage the site's whole media
//! library without opening wp-admin - a thumbnail grid (images get their
//! real WordPress-generated thumbnail, everything else a type icon), a
//! type filter (Alle Medien/Bilder/Dokumente/Audio/Video), server-side
//! search, and a details pane showing file name, type, dimensions, size,
//! upload date and URL, a live-editable Alt-Text field (images only), with
//! "URL kopieren", "Im Browser öffnen", "In Artikel einfügen" (images only)
//! and "Löschen".
//!
//! Unlike `medialibrary.rs`'s picker (images only, first 60, a plain list),
//! this pages through the library 48 items at a time behind a "Mehr laden"
//! button, since a real site's library easily runs into the thousands.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk4::{gdk, gio, glib};

use crate::i18n::tr;
use crate::wpclient::{MediaFilter, WpMediaEntry};
use crate::{secrets, wpclient, wpsite};

const SEARCH_DEBOUNCE_MS: u64 = 400;
const TILE_SIZE: i32 = 112;

const FILTERS: [MediaFilter; 5] = [MediaFilter::All, MediaFilter::Images, MediaFilter::Documents, MediaFilter::Audio, MediaFilter::Video];

fn filter_label(filter: MediaFilter) -> String {
    match filter {
        MediaFilter::All => tr("Alle Medien"),
        MediaFilter::Images => tr("Bilder"),
        MediaFilter::Documents => tr("Dokumente"),
        MediaFilter::Audio => tr("Audio"),
        MediaFilter::Video => tr("Video"),
    }
}

/// Human-readable file size ("1,2 MB") - German decimal comma, since the
/// rest of the UI's numbers (`stats.rs`) are formatted that way too.
fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit]).replace('.', ",")
}

/// The last path segment of a media URL - WordPress's `title` is often
/// just the file name without its extension, so this is shown separately.
fn file_name_of(url: &str) -> String {
    url.rsplit('/').next().unwrap_or(url).split(['?', '#']).next().unwrap_or_default().to_string()
}

/// A symbolic icon standing in for a thumbnail, by MIME type.
fn icon_for_mime(mime: &str) -> &'static str {
    if mime.starts_with("image/") {
        "image-x-generic-symbolic"
    } else if mime.starts_with("audio/") {
        "audio-x-generic-symbolic"
    } else if mime.starts_with("video/") {
        "video-x-generic-symbolic"
    } else if mime == "application/pdf" || mime.starts_with("text/") || mime.contains("document") {
        "x-office-document-symbolic"
    } else {
        "text-x-generic-symbolic"
    }
}

/// The details pane's widgets, refreshed whenever the selection changes.
struct Details {
    root: gtk4::Box,
    placeholder: adw::StatusPage,
    preview: gtk4::Picture,
    title: gtk4::Label,
    rows: gtk4::Label,
    /// Editable, unlike every other detail here - Quill's own media
    /// library (`site/images/media-view.png`) shows alt text as a live
    /// edit field in the details pane rather than a separate dialog, and
    /// it's the one field actually worth fixing from right here: a
    /// missing/wrong alt text is exactly what browsing the library to
    /// find a specific file would surface. `alt_text_list` (a small
    /// boxed-list holding just this one `Adw.EntryRow`) is the widget
    /// whose visibility toggles with the rest of the pane - images only,
    /// same condition the row's own content already followed.
    alt_text_list: gtk4::ListBox,
    alt_text_row: adw::EntryRow,
    url: gtk4::Label,
    insert_button: gtk4::Button,
}

struct BrowserCtx {
    site: wpsite::SiteConfig,
    filter: Cell<MediaFilter>,
    search: RefCell<String>,
    page: Cell<u32>,
    total_pages: Cell<u32>,
    /// Bumped on every fresh (non-"Mehr laden") load, so a slow response
    /// to an earlier filter/search can't land on top of a newer one.
    generation: Cell<u64>,
    entries: RefCell<Vec<WpMediaEntry>>,
    /// One `Gtk.Picture`/`Gtk.Image` holder per entry, in the same order,
    /// so thumbnails arriving later can be dropped into the right tile.
    tiles: RefCell<Vec<gtk4::Box>>,
    selected: Cell<Option<usize>>,
    flow_box: gtk4::FlowBox,
    status_label: gtk4::Label,
    more_button: gtk4::Button,
    details: Details,
    dialog: glib::WeakRef<adw::Dialog>,
    on_insert: Option<Rc<dyn Fn(WpMediaEntry)>>,
}

/// Runs `job` with the stored Application Password on a background thread,
/// handing the outcome to `on_done` on the GTK thread.
fn run_with_password<T: Send + 'static>(
    site: &wpsite::SiteConfig,
    job: impl FnOnce(&wpsite::SiteConfig, &str) -> Result<T, String> + Send + 'static,
    on_done: impl Fn(Result<T, String>) + 'static,
) {
    let site = site.clone();
    let (tx, rx) = mpsc::channel::<Result<T, String>>();
    std::thread::spawn(move || {
        let outcome = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .map_err(|err| err.to_string())
            .and_then(|maybe_password| maybe_password.ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden.")))
            .and_then(|password| job(&site, &password));
        let _ = tx.send(outcome);
    });
    glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
        Ok(outcome) => {
            on_done(outcome);
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            on_done(Err(tr("Interner Fehler: Lade-Thread hat kein Ergebnis geliefert.")));
            glib::ControlFlow::Break
        }
    });
}

/// Opens the media browser. `on_insert`, when given, adds an "In Artikel
/// einfügen" button for images - the caller decides what inserting means
/// (`window.rs` reuses "Aus Mediathek wählen…"'s own insertion logic).
pub fn open(parent: &adw::ApplicationWindow, on_insert: Option<Rc<dyn Fn(WpMediaEntry)>>) {
    let site = wpsite::load();

    let filter_labels: Vec<String> = FILTERS.iter().map(|f| filter_label(*f)).collect();
    let filter_label_refs: Vec<&str> = filter_labels.iter().map(String::as_str).collect();
    let filter_dropdown = gtk4::DropDown::from_strings(&filter_label_refs);
    filter_dropdown.set_tooltip_text(Some(&tr("Nach Medientyp filtern")));

    let refresh_button = gtk4::Button::from_icon_name("view-refresh-symbolic");
    refresh_button.set_tooltip_text(Some(&tr("Aktualisieren")));

    let header = adw::HeaderBar::new();
    header.pack_start(&filter_dropdown);
    header.pack_end(&refresh_button);

    let search_entry = gtk4::SearchEntry::new();
    search_entry.set_placeholder_text(Some(&tr("Mediathek durchsuchen…")));
    search_entry.set_hexpand(true);

    let status_label = gtk4::Label::new(None);
    status_label.set_xalign(0.0);
    status_label.set_wrap(true);
    status_label.add_css_class("dim-label");

    let flow_box = gtk4::FlowBox::builder()
        .selection_mode(gtk4::SelectionMode::Single)
        .homogeneous(false)
        .min_children_per_line(2)
        .max_children_per_line(12)
        .column_spacing(6)
        .row_spacing(6)
        .valign(gtk4::Align::Start)
        .build();

    let more_button = gtk4::Button::with_label(&tr("Mehr laden"));
    more_button.set_halign(gtk4::Align::Center);
    more_button.set_visible(false);

    let grid_box = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(12).build();
    grid_box.append(&flow_box);
    grid_box.append(&more_button);
    let grid_scroller = gtk4::ScrolledWindow::builder().child(&grid_box).hexpand(true).vexpand(true).hscrollbar_policy(gtk4::PolicyType::Never).build();

    let left = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    left.append(&search_entry);
    left.append(&status_label);
    left.append(&grid_scroller);

    let details = build_details(on_insert.is_some());
    let details_scroller = gtk4::ScrolledWindow::builder().child(&details.root).width_request(280).hscrollbar_policy(gtk4::PolicyType::Never).build();

    let content = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    content.append(&left);
    content.append(&gtk4::Separator::new(gtk4::Orientation::Vertical));
    content.append(&details_scroller);

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&content));

    let dialog = adw::Dialog::builder()
        .title(tr("WordPress-Mediathek"))
        .content_width(900)
        .content_height(620)
        .child(&toolbar_view)
        .build();

    if site.url.is_empty() {
        status_label.set_label(&tr("Keine WordPress-Verbindung eingerichtet - bitte zuerst in den Einstellungen konfigurieren."));
        filter_dropdown.set_sensitive(false);
        search_entry.set_sensitive(false);
        refresh_button.set_sensitive(false);
        dialog.present(Some(parent));
        return;
    }

    let ctx = Rc::new(BrowserCtx {
        site,
        filter: Cell::new(MediaFilter::All),
        search: RefCell::new(String::new()),
        page: Cell::new(1),
        total_pages: Cell::new(1),
        generation: Cell::new(0),
        entries: RefCell::new(Vec::new()),
        tiles: RefCell::new(Vec::new()),
        selected: Cell::new(None),
        flow_box,
        status_label,
        more_button,
        details,
        dialog: dialog.downgrade(),
        on_insert,
    });

    wire_details_buttons(&ctx);
    {
        let ctx_weak = Rc::downgrade(&ctx);
        ctx.flow_box.connect_selected_children_changed(move |flow_box| {
            let Some(ctx) = ctx_weak.upgrade() else { return };
            let index = flow_box.selected_children().first().map(|child| child.index() as usize);
            ctx.selected.set(index);
            show_details(&ctx);
        });
    }
    {
        let ctx_weak = Rc::downgrade(&ctx);
        filter_dropdown.connect_selected_notify(move |dropdown| {
            let Some(ctx) = ctx_weak.upgrade() else { return };
            ctx.filter.set(FILTERS.get(dropdown.selected() as usize).copied().unwrap_or(MediaFilter::All));
            reload(&ctx);
        });
    }
    {
        let ctx_weak = Rc::downgrade(&ctx);
        refresh_button.connect_clicked(move |_| {
            if let Some(ctx) = ctx_weak.upgrade() {
                reload(&ctx);
            }
        });
    }
    {
        let ctx_weak = Rc::downgrade(&ctx);
        ctx.more_button.connect_clicked(move |_| {
            if let Some(ctx) = ctx_weak.upgrade() {
                ctx.page.set(ctx.page.get() + 1);
                fetch_page(&ctx, false);
            }
        });
    }
    {
        let debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        let ctx_weak = Rc::downgrade(&ctx);
        search_entry.connect_search_changed(move |entry| {
            if let Some(id) = debounce.borrow_mut().take() {
                id.remove();
            }
            let query = entry.text().to_string();
            let ctx_weak = ctx_weak.clone();
            let debounce_inner = debounce.clone();
            let id = glib::timeout_add_local(Duration::from_millis(SEARCH_DEBOUNCE_MS), move || {
                *debounce_inner.borrow_mut() = None;
                if let Some(ctx) = ctx_weak.upgrade() {
                    *ctx.search.borrow_mut() = query.clone();
                    reload(&ctx);
                }
                glib::ControlFlow::Break
            });
            *debounce.borrow_mut() = Some(id);
        });
    }

    reload(&ctx);

    // Every closure above holds only a `Weak` - this is the one strong
    // reference, so the context lives exactly as long as the dialog.
    dialog.connect_closed(move |_| {
        let _keep_alive = &ctx;
    });

    dialog.present(Some(parent));
}

fn build_details(can_insert: bool) -> Details {
    let placeholder = adw::StatusPage::builder()
        .icon_name("image-x-generic-symbolic")
        .title(tr("Keine Datei ausgewählt"))
        .description(tr("Eine Datei im Raster auswählen, um ihre Details zu sehen."))
        .vexpand(true)
        .build();
    placeholder.add_css_class("compact");

    let preview = gtk4::Picture::builder().content_fit(gtk4::ContentFit::Contain).height_request(180).can_shrink(true).build();
    preview.add_css_class("card");

    let title = gtk4::Label::builder().xalign(0.0).wrap(true).selectable(true).build();
    title.add_css_class("title-4");

    let rows = gtk4::Label::builder().xalign(0.0).wrap(true).selectable(true).use_markup(true).build();

    let alt_text_row = adw::EntryRow::builder().title(tr("Alt-Text")).build();
    let alt_text_list = gtk4::ListBox::new();
    alt_text_list.set_selection_mode(gtk4::SelectionMode::None);
    alt_text_list.add_css_class("boxed-list");
    alt_text_list.append(&alt_text_row);

    let url = gtk4::Label::builder().xalign(0.0).wrap(true).wrap_mode(gtk4::pango::WrapMode::Char).selectable(true).build();
    url.add_css_class("caption");
    url.add_css_class("dim-label");

    let insert_button = gtk4::Button::with_label(&tr("In Artikel einfügen"));
    insert_button.add_css_class("suggested-action");
    insert_button.set_visible(can_insert);

    let root = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    root.append(&placeholder);
    root.append(&preview);
    root.append(&title);
    root.append(&rows);
    root.append(&alt_text_list);
    root.append(&url);
    root.append(&insert_button);

    Details { root, placeholder, preview, title, rows, alt_text_list, alt_text_row, url, insert_button }
}

fn set_details_visible(details: &Details, visible: bool) {
    details.placeholder.set_visible(!visible);
    for widget in [details.preview.upcast_ref::<gtk4::Widget>(), details.title.upcast_ref(), details.rows.upcast_ref(), details.url.upcast_ref()] {
        widget.set_visible(visible);
    }
    // `alt_text_list` additionally needs `entry.media_type == "image"` -
    // `show_details` sets that narrower visibility itself right after
    // calling this, since only it has the selected entry to check.
    if !visible {
        details.alt_text_list.set_visible(false);
    }
    // The action buttons live in a box appended by `wire_details_buttons`.
    if let Some(actions) = details.root.last_child() {
        actions.set_visible(visible);
    }
}

fn selected_entry(ctx: &BrowserCtx) -> Option<WpMediaEntry> {
    ctx.selected.get().and_then(|i| ctx.entries.borrow().get(i).cloned())
}

fn show_details(ctx: &BrowserCtx) {
    let details = &ctx.details;
    let Some(entry) = selected_entry(ctx) else {
        set_details_visible(details, false);
        details.insert_button.set_visible(false);
        return;
    };
    set_details_visible(details, true);

    let file_name = file_name_of(&entry.source_url);
    details.title.set_label(if entry.title.trim().is_empty() { &file_name } else { &entry.title });

    let mut lines = vec![format!("<b>{}</b> {}", tr("Datei:"), glib::markup_escape_text(&file_name))];
    if !entry.mime_type.is_empty() {
        lines.push(format!("<b>{}</b> {}", tr("Typ:"), glib::markup_escape_text(&entry.mime_type)));
    }
    if entry.width > 0 && entry.height > 0 {
        lines.push(format!("<b>{}</b> {} × {} px", tr("Abmessungen:"), entry.width, entry.height));
    }
    if entry.filesize > 0 {
        lines.push(format!("<b>{}</b> {}", tr("Größe:"), format_size(entry.filesize)));
    }
    if !entry.date.is_empty() {
        lines.push(format!("<b>{}</b> {}", tr("Hochgeladen:"), glib::markup_escape_text(&entry.date.replace('T', " "))));
    }
    details.rows.set_markup(&lines.join("\n"));
    details.alt_text_list.set_visible(entry.media_type == "image");
    details.alt_text_row.set_text(&entry.alt_text);
    details.url.set_label(&entry.source_url);
    details.insert_button.set_visible(ctx.on_insert.is_some() && entry.media_type == "image");

    // Re-use the tile's already-downloaded thumbnail if there is one - the
    // full-size original can be many megabytes, not worth fetching just
    // for a 280px-wide preview.
    let texture = ctx.selected.get().and_then(|i| ctx.tiles.borrow().get(i).cloned()).and_then(|tile| tile.first_child()).and_then(|child| child.downcast::<gtk4::Image>().ok()).and_then(|image| image.paintable());
    match texture {
        Some(paintable) => {
            details.preview.set_content_fit(gtk4::ContentFit::Contain);
            details.preview.set_paintable(Some(&paintable));
        }
        None => {
            // Shown at its own 96px size - `Contain` would blow a symbolic
            // icon up to fill the whole preview card.
            details.preview.set_content_fit(gtk4::ContentFit::ScaleDown);
            let icon = gtk4::IconTheme::for_display(&details.preview.display()).lookup_icon(icon_for_mime(&entry.mime_type), &[], 96, 1, gtk4::TextDirection::None, gtk4::IconLookupFlags::empty());
            details.preview.set_paintable(Some(&icon));
        }
    }
}

fn wire_details_buttons(ctx: &Rc<BrowserCtx>) {
    let copy_button = gtk4::Button::from_icon_name("edit-copy-symbolic");
    copy_button.set_tooltip_text(Some(&tr("URL kopieren")));
    let open_button = gtk4::Button::from_icon_name("web-browser-symbolic");
    open_button.set_tooltip_text(Some(&tr("Im Browser öffnen")));
    let delete_button = gtk4::Button::from_icon_name("user-trash-symbolic");
    delete_button.set_tooltip_text(Some(&tr("Endgültig löschen")));
    delete_button.add_css_class("destructive-action");

    let actions = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).spacing(6).halign(gtk4::Align::Center).build();
    actions.append(&copy_button);
    actions.append(&open_button);
    actions.append(&delete_button);
    ctx.details.root.append(&actions);
    set_details_visible(&ctx.details, false);
    ctx.details.insert_button.set_visible(false);

    {
        let ctx_weak = Rc::downgrade(ctx);
        // `connect_apply`, not `connect_changed` - fires once on Enter (or
        // the row losing focus with an edit pending), the same "commit,
        // don't save every keystroke" signal `Adw.EntryRow` is designed
        // for, matching Quill's own alt-text field editing right in the
        // details pane (`site/images/media-view.png`) instead of only
        // through `mediapanel.rs`'s per-body-image editor.
        ctx.details.alt_text_row.connect_apply(move |row| {
            let Some(ctx) = ctx_weak.upgrade() else { return };
            let Some(entry) = selected_entry(&ctx) else { return };
            let media_id = entry.id;
            let alt_text = row.text().to_string();
            if alt_text == entry.alt_text {
                return;
            }
            ctx.status_label.set_label(&tr("Alt-Text wird gespeichert …"));
            let ctx_weak = ctx_weak.clone();
            let alt_text_for_save = alt_text.clone();
            run_with_password(
                &ctx.site,
                move |site, password| {
                    wpclient::Client::new(&site.url, &site.username, password).update_media_metadata(media_id, Some(&alt_text_for_save), None).map_err(|err| err.to_string())
                },
                move |outcome| {
                    let Some(ctx) = ctx_weak.upgrade() else { return };
                    match outcome {
                        Ok(()) => {
                            if let Some(stored) = ctx.entries.borrow_mut().iter_mut().find(|e| e.id == media_id) {
                                stored.alt_text = alt_text.clone();
                            }
                            ctx.status_label.set_label(&tr("Alt-Text gespeichert."));
                        }
                        Err(err) => ctx.status_label.set_label(&tr("Fehler: {err}").replace("{err}", &err)),
                    }
                },
            );
        });
    }
    {
        let ctx_weak = Rc::downgrade(ctx);
        copy_button.connect_clicked(move |button| {
            let Some(ctx) = ctx_weak.upgrade() else { return };
            if let Some(entry) = selected_entry(&ctx) {
                button.clipboard().set_text(&entry.source_url);
                ctx.status_label.set_label(&tr("URL in die Zwischenablage kopiert."));
            }
        });
    }
    {
        let ctx_weak = Rc::downgrade(ctx);
        open_button.connect_clicked(move |button| {
            let Some(ctx) = ctx_weak.upgrade() else { return };
            if let Some(entry) = selected_entry(&ctx) {
                let window = button.root().and_downcast::<gtk4::Window>();
                gtk4::UriLauncher::new(&entry.source_url).launch(window.as_ref(), None::<&gio::Cancellable>, |_| {});
            }
        });
    }
    {
        let ctx_weak = Rc::downgrade(ctx);
        ctx.details.insert_button.connect_clicked(move |_| {
            let Some(ctx) = ctx_weak.upgrade() else { return };
            let (Some(entry), Some(on_insert)) = (selected_entry(&ctx), ctx.on_insert.clone()) else { return };
            on_insert(entry);
            if let Some(dialog) = ctx.dialog.upgrade() {
                dialog.close();
            }
        });
    }
    {
        let ctx_weak = Rc::downgrade(ctx);
        delete_button.connect_clicked(move |button| {
            if let Some(ctx) = ctx_weak.upgrade() {
                confirm_delete(button, &ctx);
            }
        });
    }
}

fn confirm_delete(anchor: &gtk4::Button, ctx: &Rc<BrowserCtx>) {
    let Some(entry) = selected_entry(ctx) else { return };
    let name = file_name_of(&entry.source_url);
    let confirm = adw::AlertDialog::new(
        Some(&tr("Datei endgültig löschen?")),
        Some(&tr("„{name}“ wird unwiderruflich aus der WordPress-Mediathek gelöscht. Artikel, die die Datei verwenden, zeigen sie danach nicht mehr an.").replace("{name}", &name)),
    );
    confirm.add_response("cancel", &tr("Abbrechen"));
    confirm.add_response("delete", &tr("Löschen"));
    confirm.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    confirm.set_default_response(Some("cancel"));
    confirm.set_close_response("cancel");

    let ctx_weak: Weak<BrowserCtx> = Rc::downgrade(ctx);
    confirm.connect_response(None, move |_, response| {
        if response != "delete" {
            return;
        }
        let Some(ctx) = ctx_weak.upgrade() else { return };
        ctx.status_label.set_label(&tr("Wird gelöscht …"));
        let media_id = entry.id;
        let ctx_weak = ctx_weak.clone();
        let name = name.clone();
        run_with_password(
            &ctx.site,
            move |site, password| wpclient::Client::new(&site.url, &site.username, password).delete_media(media_id).map_err(|err| err.to_string()),
            move |outcome| {
                let Some(ctx) = ctx_weak.upgrade() else { return };
                match outcome {
                    Ok(()) => {
                        // Removed locally instead of reloading, so the user
                        // keeps their scroll position in a long library.
                        let Some(index) = ctx.entries.borrow().iter().position(|e| e.id == media_id) else { return };
                        ctx.entries.borrow_mut().remove(index);
                        ctx.tiles.borrow_mut().remove(index);
                        if let Some(child) = ctx.flow_box.child_at_index(index as i32) {
                            ctx.flow_box.remove(&child);
                        }
                        ctx.selected.set(None);
                        show_details(&ctx);
                        ctx.status_label.set_label(&tr("„{name}“ gelöscht.").replace("{name}", &name));
                    }
                    Err(err) => ctx.status_label.set_label(&tr("Fehler: {err}").replace("{err}", &err)),
                }
            },
        );
    });
    confirm.present(Some(anchor));
}

/// Starts over at page 1 for the current filter/search.
fn reload(ctx: &Rc<BrowserCtx>) {
    ctx.generation.set(ctx.generation.get() + 1);
    ctx.page.set(1);
    ctx.entries.borrow_mut().clear();
    ctx.tiles.borrow_mut().clear();
    ctx.flow_box.remove_all();
    ctx.selected.set(None);
    show_details(ctx);
    fetch_page(ctx, true);
}

fn fetch_page(ctx: &Rc<BrowserCtx>, fresh: bool) {
    let generation = ctx.generation.get();
    let filter = ctx.filter.get();
    let search = ctx.search.borrow().clone();
    let page = ctx.page.get();
    ctx.more_button.set_sensitive(false);
    ctx.status_label.set_label(&tr("Lade Mediathek …"));

    let ctx_weak = Rc::downgrade(ctx);
    run_with_password(
        &ctx.site,
        move |site, password| {
            wpclient::Client::new(&site.url, &site.username, password)
                .list_media_library(filter, (!search.trim().is_empty()).then_some(search.as_str()), page)
                .map_err(|err| err.to_string())
        },
        move |outcome| {
            let Some(ctx) = ctx_weak.upgrade() else { return };
            if ctx.generation.get() != generation {
                return;
            }
            ctx.more_button.set_sensitive(true);
            match outcome {
                Ok((fetched, total_pages)) => {
                    ctx.total_pages.set(total_pages);
                    let first_new = ctx.entries.borrow().len();
                    for entry in &fetched {
                        let tile = build_tile(entry);
                        ctx.flow_box.append(&tile);
                        ctx.tiles.borrow_mut().push(tile.first_child().and_downcast::<gtk4::Box>().expect("tile holder"));
                    }
                    ctx.entries.borrow_mut().extend(fetched.iter().cloned());
                    ctx.more_button.set_visible(ctx.page.get() < total_pages);
                    let count = ctx.entries.borrow().len();
                    ctx.status_label.set_label(&if count == 0 {
                        tr("Keine Medien gefunden.")
                    } else if count == 1 {
                        tr("1 Datei angezeigt.")
                    } else {
                        tr("{n} Dateien angezeigt.").replace("{n}", &count.to_string())
                    });
                    load_thumbnails(&ctx, generation, first_new, &fetched);
                }
                Err(err) => {
                    if fresh {
                        ctx.more_button.set_visible(false);
                    } else {
                        ctx.page.set(ctx.page.get().saturating_sub(1).max(1));
                    }
                    ctx.status_label.set_label(&tr("Fehler beim Laden: {err}").replace("{err}", &err));
                }
            }
        },
    );
}

/// One grid tile: a fixed-size holder (icon now, thumbnail once it has
/// downloaded) plus the file name underneath.
fn build_tile(entry: &WpMediaEntry) -> gtk4::Box {
    let holder = gtk4::Box::builder().width_request(TILE_SIZE).height_request(TILE_SIZE).halign(gtk4::Align::Center).build();
    holder.add_css_class("card");
    let icon = gtk4::Image::from_icon_name(icon_for_mime(&entry.mime_type));
    icon.set_pixel_size(48);
    icon.set_hexpand(true);
    icon.set_halign(gtk4::Align::Center);
    icon.add_css_class("dim-label");
    holder.append(&icon);

    let name = if entry.title.trim().is_empty() { file_name_of(&entry.source_url) } else { entry.title.clone() };
    let label = gtk4::Label::builder().label(name.as_str()).ellipsize(gtk4::pango::EllipsizeMode::Middle).max_width_chars(14).build();
    label.add_css_class("caption");

    let tile = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(4).margin_top(4).margin_bottom(4).build();
    tile.set_tooltip_text(Some(&name));
    tile.append(&holder);
    tile.append(&label);
    tile
}

/// Downloads the new page's thumbnails on one background thread,
/// sequentially (a few KB each - not worth a thread per image), dropping
/// each into its tile as it arrives.
fn load_thumbnails(ctx: &Rc<BrowserCtx>, generation: u64, first_index: usize, entries: &[WpMediaEntry]) {
    let jobs: Vec<(usize, String)> = entries.iter().enumerate().filter_map(|(i, e)| e.thumbnail_url.clone().map(|url| (first_index + i, url))).collect();
    if jobs.is_empty() {
        return;
    }
    let (tx, rx) = mpsc::channel::<(usize, Vec<u8>)>();
    std::thread::spawn(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(15))).build().into();
        for (index, url) in jobs {
            let Ok(mut response) = agent.get(&url).call() else { continue };
            let Ok(bytes) = response.body_mut().with_config().limit(5 * 1024 * 1024).read_to_vec() else { continue };
            if tx.send((index, bytes)).is_err() {
                return;
            }
        }
    });

    let ctx_weak = Rc::downgrade(ctx);
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let Some(ctx) = ctx_weak.upgrade() else { return glib::ControlFlow::Break };
        if ctx.generation.get() != generation {
            return glib::ControlFlow::Break;
        }
        loop {
            match rx.try_recv() {
                Ok((index, bytes)) => {
                    let Ok(texture) = gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)) else { continue };
                    let Some(holder) = ctx.tiles.borrow().get(index).cloned() else { continue };
                    while let Some(child) = holder.first_child() {
                        holder.remove(&child);
                    }
                    // A fixed-size `Gtk.Image`, not a `Gtk.Picture`: a
                    // picture reports the texture's full pixel width as its
                    // natural width, and `Gtk.FlowBox` lays out columns by
                    // natural width - one 300px thumbnail was enough to
                    // collapse the whole grid to two columns.
                    let image = gtk4::Image::from_paintable(Some(&texture));
                    image.set_pixel_size(TILE_SIZE);
                    image.set_hexpand(true);
                    holder.append(&image);
                    if ctx.selected.get() == Some(index) {
                        show_details(&ctx);
                    }
                }
                Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => return glib::ControlFlow::Break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_size_uses_binary_units_and_a_decimal_comma() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1536), "1,5 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5,0 MB");
    }

    #[test]
    fn file_name_of_strips_path_and_query() {
        assert_eq!(file_name_of("https://example.com/wp-content/uploads/2026/09/shot.png?ver=2"), "shot.png");
        assert_eq!(file_name_of("plain.pdf"), "plain.pdf");
    }

    #[test]
    fn icon_for_mime_buckets_common_types() {
        assert_eq!(icon_for_mime("image/png"), "image-x-generic-symbolic");
        assert_eq!(icon_for_mime("application/pdf"), "x-office-document-symbolic");
        assert_eq!(icon_for_mime("audio/mpeg"), "audio-x-generic-symbolic");
        assert_eq!(icon_for_mime("application/zip"), "text-x-generic-symbolic");
    }
}
