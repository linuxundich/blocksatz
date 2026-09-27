//! "Galerie einfügen" dialog: pick several images from the WordPress media
//! library, reorder them, edit each one's alt text/caption, and choose the
//! gallery's own settings (Spalten/Zuschnitt/Verlinkung/Bildgröße) - modeled
//! on the Quill macOS app's `GallerySheet`. Builds the resulting fenced
//! ` ```gallery ``` ` block text (see `gutenberg::GallerySettings`) and
//! hands it to `on_insert`, which inserts it at the cursor.
//!
//! Reorder is a pair of Auf/Ab buttons per selected row, not drag-and-drop -
//! GTK4 drag-and-drop needs its own `Gtk.DragSource`/`Gtk.DropTarget` wiring
//! per row for what two buttons already do with far less code and no
//! separate hit-testing to get right.
//!
//! Media browsing (fetch/page/thumbnail-load) deliberately isn't shared
//! with `mediabrowser.rs` - that dialog's `BrowserCtx` is single-select and
//! tightly coupled to its own details pane, and the fetch/thumbnail-load
//! shape it uses (spawn a thread, poll via `mpsc` + `glib::timeout_add_local`)
//! is already duplicated across this codebase (`aiwriter.rs`, `mediapanel.rs`,
//! `mediabrowser.rs` itself) rather than factored out - one more instance
//! here follows that same established convention instead of inventing a
//! shared abstraction none of the others use either.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk4::{gdk, glib};

use crate::i18n::tr;
use crate::wpclient::{MediaFilter, WpMediaEntry};
use crate::{secrets, wpclient, wpsite};

const SEARCH_DEBOUNCE_MS: u64 = 400;
const TILE_SIZE: i32 = 72;

/// The fenced ` ```gallery ``` ` block text, plus each inserted image's
/// `(media_id, source_url, width, height)` - the caller uses these to mark
/// them as already-uploaded (see `window.rs`'s `wire_insert_gallery_action`).
pub type OnInsertGallery = Rc<dyn Fn(String, Vec<(u64, String, u64, u64)>)>;

/// One image the user has added to the gallery - alt/caption start as the
/// media library's own values but are edited independently here, the same
/// "editing here never writes back to the library" reasoning as Quill's own
/// `GallerySelection`.
struct SelectedImage {
    entry: WpMediaEntry,
    alt: String,
    caption: String,
}

struct GalleryDialogCtx {
    site: wpsite::SiteConfig,
    search: RefCell<String>,
    page: Cell<u32>,
    total_pages: Cell<u32>,
    generation: Cell<u64>,
    entries: RefCell<Vec<WpMediaEntry>>,
    /// One holder per entry, same order - see `mediabrowser.rs`'s own
    /// `BrowserCtx::tiles` for why this needs to exist separately from the
    /// `Gtk.FlowBox` itself (thumbnails arrive later, asynchronously).
    tiles: RefCell<Vec<gtk4::Box>>,
    media_flow: gtk4::FlowBox,
    status_label: gtk4::Label,
    more_button: gtk4::Button,
    selected: RefCell<Vec<SelectedImage>>,
    selected_list: gtk4::ListBox,
    selected_placeholder: gtk4::Label,
    selected_count_label: gtk4::Label,
    insert_button: gtk4::Button,
    columns_row: adw::SpinRow,
    crop_row: adw::SwitchRow,
    link_row: adw::ComboRow,
    size_row: adw::ComboRow,
    dialog: glib::WeakRef<adw::Dialog>,
    on_insert: OnInsertGallery,
}

pub fn open(parent: &adw::ApplicationWindow, on_insert: OnInsertGallery) {
    let site = wpsite::load();

    let search_entry = gtk4::SearchEntry::new();
    search_entry.set_placeholder_text(Some(&tr("Mediathek durchsuchen…")));
    search_entry.set_hexpand(true);

    let status_label = gtk4::Label::new(None);
    status_label.set_xalign(0.0);
    status_label.set_wrap(true);
    status_label.add_css_class("dim-label");

    let media_flow = gtk4::FlowBox::builder()
        .selection_mode(gtk4::SelectionMode::None)
        .homogeneous(false)
        .min_children_per_line(2)
        .max_children_per_line(12)
        .column_spacing(6)
        .row_spacing(6)
        .valign(gtk4::Align::Start)
        .activate_on_single_click(true)
        .build();

    let more_button = gtk4::Button::with_label(&tr("Mehr laden"));
    more_button.set_halign(gtk4::Align::Center);
    more_button.set_visible(false);

    let grid_box = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(12).build();
    grid_box.append(&media_flow);
    grid_box.append(&more_button);
    let grid_scroller = gtk4::ScrolledWindow::builder().child(&grid_box).hexpand(true).vexpand(true).hscrollbar_policy(gtk4::PolicyType::Never).build();

    let left = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .width_request(360)
        .build();
    left.append(&search_entry);
    left.append(&status_label);
    left.append(&grid_scroller);

    let selected_count_label = gtk4::Label::builder().xalign(0.0).build();
    selected_count_label.add_css_class("heading");

    let selected_list = gtk4::ListBox::new();
    selected_list.set_selection_mode(gtk4::SelectionMode::None);
    selected_list.add_css_class("boxed-list");

    let selected_placeholder = gtk4::Label::builder().label(tr("Bilder im Raster anklicken, um sie zur Galerie hinzuzufügen.")).wrap(true).xalign(0.0).build();
    selected_placeholder.add_css_class("dim-label");

    let selected_scroller = gtk4::ScrolledWindow::builder().hscrollbar_policy(gtk4::PolicyType::Never).vexpand(true).build();
    let selected_box = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(6).build();
    selected_box.append(&selected_placeholder);
    selected_box.append(&selected_list);
    selected_scroller.set_child(Some(&selected_box));

    let columns_row = adw::SpinRow::builder().title(tr("Spalten")).subtitle(tr("0 = automatisch")).adjustment(&gtk4::Adjustment::new(0.0, 0.0, 8.0, 1.0, 1.0, 0.0)).build();
    let crop_row = adw::SwitchRow::builder().title(tr("Quadratisch zuschneiden")).active(true).build();
    let link_row = adw::ComboRow::builder().title(tr("Verlinken zu")).model(&gtk4::StringList::new(&[&tr("Nichts"), &tr("Volles Bild")])).build();
    let size_row = adw::ComboRow::builder()
        .title(tr("Bildgröße"))
        .model(&gtk4::StringList::new(&[&tr("Thumbnail"), &tr("Mittel"), &tr("Groß"), &tr("Volle Größe")]))
        .selected(2)
        .build();
    let settings_group = adw::PreferencesGroup::builder().title(tr("Einstellungen")).build();
    settings_group.add(&columns_row);
    settings_group.add(&crop_row);
    settings_group.add(&link_row);
    settings_group.add(&size_row);

    let cancel_button = gtk4::Button::with_label(&tr("Abbrechen"));
    let insert_button = gtk4::Button::with_label(&tr("Galerie einfügen"));
    insert_button.add_css_class("suggested-action");
    insert_button.set_sensitive(false);
    let action_row = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).spacing(6).halign(gtk4::Align::End).build();
    action_row.append(&cancel_button);
    action_row.append(&insert_button);

    let right = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .width_request(320)
        .build();
    right.append(&selected_count_label);
    right.append(&selected_scroller);
    right.append(&settings_group);
    right.append(&action_row);

    let content = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    content.append(&left);
    content.append(&gtk4::Separator::new(gtk4::Orientation::Vertical));
    content.append(&right);

    let header = adw::HeaderBar::new();
    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&content));

    let dialog = adw::Dialog::builder().title(tr("Galerie einfügen")).content_width(980).content_height(640).child(&toolbar_view).build();

    if site.url.is_empty() {
        status_label.set_label(&tr("Keine WordPress-Verbindung eingerichtet - bitte zuerst in den Einstellungen konfigurieren."));
        search_entry.set_sensitive(false);
        dialog.present(Some(parent));
        return;
    }

    let ctx = Rc::new(GalleryDialogCtx {
        site,
        search: RefCell::new(String::new()),
        page: Cell::new(1),
        total_pages: Cell::new(1),
        generation: Cell::new(0),
        entries: RefCell::new(Vec::new()),
        tiles: RefCell::new(Vec::new()),
        media_flow,
        status_label,
        more_button,
        selected: RefCell::new(Vec::new()),
        selected_list,
        selected_placeholder: selected_placeholder.clone(),
        selected_count_label,
        insert_button,
        columns_row,
        crop_row,
        link_row,
        size_row,
        dialog: dialog.downgrade(),
        on_insert,
    });

    rebuild_selected_list(&ctx);

    {
        let ctx_weak = Rc::downgrade(&ctx);
        ctx.media_flow.connect_child_activated(move |_, child| {
            let Some(ctx) = ctx_weak.upgrade() else { return };
            toggle_selection(&ctx, child.index() as usize);
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
    {
        let dialog_weak = dialog.downgrade();
        cancel_button.connect_clicked(move |_| {
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog.close();
            }
        });
    }
    {
        let this = ctx.clone();
        ctx.insert_button.connect_clicked(move |_| {
            let ctx = &this;
            let selected = ctx.selected.borrow();
            if selected.is_empty() {
                return;
            }
            let settings = gutenberg::GallerySettings {
                columns: (ctx.columns_row.value().round() as u8 != 0).then(|| ctx.columns_row.value().round() as u8),
                cropped: ctx.crop_row.is_active(),
                link_to: if ctx.link_row.selected() == 1 { "media".to_string() } else { "none".to_string() },
                size_slug: match ctx.size_row.selected() {
                    0 => "thumbnail",
                    1 => "medium",
                    3 => "full",
                    _ => "large",
                }
                .to_string(),
            };
            let images: Vec<gutenberg::GalleryImage> = selected
                .iter()
                .map(|sel| gutenberg::GalleryImage { url: sel.entry.source_url.clone(), alt: sel.alt.clone(), caption: (!sel.caption.trim().is_empty()).then(|| sel.caption.clone()) })
                .collect();
            let media_refs: Vec<(u64, String, u64, u64)> = selected.iter().map(|sel| (sel.entry.id, sel.entry.source_url.clone(), sel.entry.width, sel.entry.height)).collect();
            let fenced = gutenberg::render_gallery_fence(&images, &settings);
            (ctx.on_insert)(fenced, media_refs);
            if let Some(dialog) = ctx.dialog.upgrade() {
                dialog.close();
            }
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

/// Adds `index` (into `ctx.entries`) to the selection, or removes it if
/// it's already there - the FlowBox's own `child-activated` (plain single
/// click, not a modifier-based multi-select) is the only way images are
/// picked here, so this is the one place selection state actually changes.
fn toggle_selection(ctx: &Rc<GalleryDialogCtx>, index: usize) {
    let Some(entry) = ctx.entries.borrow().get(index).cloned() else { return };
    let already_selected = {
        let mut selected = ctx.selected.borrow_mut();
        match selected.iter().position(|s| s.entry.id == entry.id) {
            Some(pos) => {
                selected.remove(pos);
                true
            }
            None => {
                selected.push(SelectedImage { alt: entry.alt_text.clone(), caption: entry.caption.clone(), entry });
                false
            }
        }
    };
    if let Some(tile) = ctx.tiles.borrow().get(index) {
        if already_selected {
            tile.remove_css_class("gallery-selected");
        } else {
            tile.add_css_class("gallery-selected");
        }
    }
    rebuild_selected_list(ctx);
}

fn rebuild_selected_list(ctx: &Rc<GalleryDialogCtx>) {
    while let Some(child) = ctx.selected_list.first_child() {
        ctx.selected_list.remove(&child);
    }
    let count = ctx.selected.borrow().len();
    for index in 0..count {
        ctx.selected_list.append(&build_selected_row(ctx, index));
    }
    ctx.selected_count_label.set_label(&match count {
        0 => tr("Noch keine Bilder ausgewählt"),
        1 => tr("1 Bild ausgewählt"),
        n => tr("{n} Bilder ausgewählt").replace("{n}", &n.to_string()),
    });
    ctx.selected_placeholder.set_visible(count == 0);
    ctx.selected_list.set_visible(count > 0);
    ctx.insert_button.set_sensitive(count > 0);
}

/// One `Adw.ExpanderRow` per selected image - collapsed by default (just
/// the file name and Auf/Ab/Entfernen buttons), expanding to reveal its
/// alt-text/caption fields, the same "click to reveal editable fields"
/// affordance Quill's own chevron gives its selection rows, for free from
/// `Adw.ExpanderRow` itself rather than hand-rolled show/hide state.
fn build_selected_row(ctx: &Rc<GalleryDialogCtx>, index: usize) -> adw::ExpanderRow {
    let selected = ctx.selected.borrow();
    let sel = &selected[index];
    let name = if sel.entry.title.trim().is_empty() { sel.entry.source_url.rsplit('/').next().unwrap_or_default().to_string() } else { sel.entry.title.clone() };

    let row = adw::ExpanderRow::builder().title(glib::markup_escape_text(&name).to_string()).build();

    let up_button = gtk4::Button::from_icon_name("go-up-symbolic");
    up_button.add_css_class("flat");
    up_button.set_valign(gtk4::Align::Center);
    up_button.set_sensitive(index > 0);
    up_button.set_tooltip_text(Some(&tr("Nach oben verschieben")));
    let down_button = gtk4::Button::from_icon_name("go-down-symbolic");
    down_button.add_css_class("flat");
    down_button.set_valign(gtk4::Align::Center);
    down_button.set_sensitive(index + 1 < selected.len());
    down_button.set_tooltip_text(Some(&tr("Nach unten verschieben")));
    let remove_button = gtk4::Button::from_icon_name("edit-delete-symbolic");
    remove_button.add_css_class("flat");
    remove_button.set_valign(gtk4::Align::Center);
    remove_button.set_tooltip_text(Some(&tr("Aus der Galerie entfernen")));
    drop(selected);

    row.add_suffix(&up_button);
    row.add_suffix(&down_button);
    row.add_suffix(&remove_button);

    let alt_row = adw::EntryRow::builder().title(tr("Alt-Text")).text(ctx.selected.borrow()[index].alt.as_str()).build();
    let caption_row = adw::EntryRow::builder().title(tr("Bildunterschrift")).text(ctx.selected.borrow()[index].caption.as_str()).build();
    row.add_row(&alt_row);
    row.add_row(&caption_row);

    {
        let ctx = ctx.clone();
        alt_row.connect_apply(move |entry| {
            if let Some(sel) = ctx.selected.borrow_mut().get_mut(index) {
                sel.alt = entry.text().to_string();
            }
        });
    }
    {
        let ctx = ctx.clone();
        caption_row.connect_apply(move |entry| {
            if let Some(sel) = ctx.selected.borrow_mut().get_mut(index) {
                sel.caption = entry.text().to_string();
            }
        });
    }
    {
        let ctx = ctx.clone();
        up_button.connect_clicked(move |_| {
            ctx.selected.borrow_mut().swap(index, index - 1);
            rebuild_selected_list(&ctx);
        });
    }
    {
        let ctx = ctx.clone();
        down_button.connect_clicked(move |_| {
            ctx.selected.borrow_mut().swap(index, index + 1);
            rebuild_selected_list(&ctx);
        });
    }
    {
        let ctx = ctx.clone();
        remove_button.connect_clicked(move |_| {
            let id = ctx.selected.borrow()[index].entry.id;
            ctx.selected.borrow_mut().remove(index);
            if let Some(tile_index) = ctx.entries.borrow().iter().position(|e| e.id == id) {
                if let Some(tile) = ctx.tiles.borrow().get(tile_index) {
                    tile.remove_css_class("gallery-selected");
                }
            }
            rebuild_selected_list(&ctx);
        });
    }

    row
}

fn reload(ctx: &Rc<GalleryDialogCtx>) {
    ctx.generation.set(ctx.generation.get() + 1);
    ctx.page.set(1);
    ctx.entries.borrow_mut().clear();
    ctx.tiles.borrow_mut().clear();
    ctx.media_flow.remove_all();
    fetch_page(ctx, true);
}

fn fetch_page(ctx: &Rc<GalleryDialogCtx>, fresh: bool) {
    let generation = ctx.generation.get();
    let search = ctx.search.borrow().clone();
    let page = ctx.page.get();
    ctx.more_button.set_sensitive(false);
    ctx.status_label.set_label(&tr("Lade Mediathek …"));

    let ctx_weak = Rc::downgrade(ctx);
    run_with_password(
        &ctx.site,
        move |site, password| {
            wpclient::Client::new(&site.url, &site.username, password)
                .list_media_library(MediaFilter::Images, (!search.trim().is_empty()).then_some(search.as_str()), page)
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
                    let selected_ids: Vec<u64> = ctx.selected.borrow().iter().map(|s| s.entry.id).collect();
                    for entry in &fetched {
                        let tile = build_tile(entry);
                        if selected_ids.contains(&entry.id) {
                            tile.first_child().and_downcast::<gtk4::Box>().expect("tile holder").add_css_class("gallery-selected");
                        }
                        ctx.media_flow.append(&tile);
                        ctx.tiles.borrow_mut().push(tile.first_child().and_downcast::<gtk4::Box>().expect("tile holder"));
                    }
                    ctx.entries.borrow_mut().extend(fetched.iter().cloned());
                    ctx.more_button.set_visible(ctx.page.get() < total_pages);
                    let count = ctx.entries.borrow().len();
                    ctx.status_label.set_label(&if count == 0 {
                        tr("Keine Bilder gefunden.")
                    } else if count == 1 {
                        tr("1 Bild angezeigt.")
                    } else {
                        tr("{n} Bilder angezeigt.").replace("{n}", &count.to_string())
                    });
                    load_thumbnails(&ctx, generation, first_new, &fetched);
                }
                Err(err) => {
                    if fresh {
                        ctx.more_button.set_visible(false);
                    }
                    ctx.status_label.set_label(&tr("Fehler: {err}").replace("{err}", &err));
                }
            }
        },
    );
}

/// One grid tile - a fixed-size holder (icon now, thumbnail once it has
/// downloaded) plus the file name underneath, same shape as
/// `mediabrowser.rs`'s own `build_tile`. `gallery-selected` (added/removed
/// by `toggle_selection`) draws a highlighted border via this dialog's own
/// tiny inline CSS, loaded once in `open`.
fn build_tile(entry: &WpMediaEntry) -> gtk4::Box {
    let holder = gtk4::Box::builder().width_request(TILE_SIZE).height_request(TILE_SIZE).halign(gtk4::Align::Center).build();
    holder.add_css_class("card");
    let icon = gtk4::Image::from_icon_name("image-x-generic-symbolic");
    icon.set_pixel_size(32);
    icon.set_hexpand(true);
    icon.set_halign(gtk4::Align::Center);
    icon.add_css_class("dim-label");
    holder.append(&icon);

    let name = if entry.title.trim().is_empty() { entry.source_url.rsplit('/').next().unwrap_or_default().to_string() } else { entry.title.clone() };
    let label = gtk4::Label::builder().label(name.as_str()).ellipsize(gtk4::pango::EllipsizeMode::Middle).max_width_chars(10).build();
    label.add_css_class("caption");

    let tile = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(4).margin_top(4).margin_bottom(4).build();
    tile.set_tooltip_text(Some(&name));
    tile.append(&holder);
    tile.append(&label);
    tile
}

/// Downloads the new page's thumbnails on one background thread,
/// sequentially, dropping each into its tile as it arrives - same approach
/// as `mediabrowser.rs`'s own `load_thumbnails` (see this module's doc
/// comment for why it isn't shared directly).
fn load_thumbnails(ctx: &Rc<GalleryDialogCtx>, generation: u64, first_index: usize, entries: &[WpMediaEntry]) {
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
                    let image = gtk4::Image::from_paintable(Some(&texture));
                    image.set_pixel_size(TILE_SIZE);
                    image.set_hexpand(true);
                    holder.append(&image);
                }
                Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => return glib::ControlFlow::Break,
            }
        }
    });
}

/// Runs `job` with the stored Application Password on a background thread,
/// handing the outcome to `on_done` on the GTK thread - same shape as
/// `mediabrowser.rs`'s own `run_with_password` (see this module's doc
/// comment for why it isn't shared directly).
fn run_with_password<T: Send + 'static>(site: &wpsite::SiteConfig, job: impl FnOnce(&wpsite::SiteConfig, &str) -> Result<T, String> + Send + 'static, on_done: impl Fn(Result<T, String>) + 'static) {
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
