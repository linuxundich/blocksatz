//! Adds a "Bildbeschriftung bearbeiten…" item to the editor's right-click
//! context menu - alt text (and caption) already exist as a full feature
//! via Medienverwaltung/the export dialog's "Medien" tab, but reaching
//! them meant leaving the editor entirely. This lets a right-click on the
//! specific line holding an image reference jump straight to that image's
//! fields, backed by the exact same `MediaItem`/`AltText` model.
//!
//! The menu section (`install`'s returned `gio::Menu` handle) starts empty
//! and is rebuilt on every secondary click from whether the clicked line
//! actually holds a media reference - mutating an already-installed
//! `gio::Menu` is reflected immediately in the popover showing it, the
//! same live-update trick `aimenu.rs` already uses for its custom-prompts
//! section - so the item only ever appears when it would actually do
//! something, instead of always showing and popping an explanation dialog
//! when clicked on a line with nothing to act on. The dialog itself
//! (`open_dialog_for_index`) also offers "generate with AI" suffix buttons
//! for both fields, folded in rather than kept as separate menu
//! items/dialogs (`aialt::generate`/`aicaption::generate`) - each only
//! shown for an actual image (not video/audio), since vision-based
//! generation makes no sense for those.
//! "Where the click landed" comes from the buffer's insertion mark at the
//! moment the menu item is built/activated - but a plain right-click does
//! NOT reposition that mark on its own (confirmed live: it stayed wherever
//! an earlier left-click/edit had left it), unlike e.g. a web browser's
//! text field. So a small `Gtk.GestureClick` explicitly moves the cursor
//! to the click point on every secondary-button press (unless it lands in
//! the selection, which must survive for Cut/Copy), before rebuilding the
//! menu from the now-accurate position.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk4::gio;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use webkit6::prelude::*;

use crate::document::{self, Frontmatter};
use crate::i18n::tr;
use crate::media::{self, AltText};
use crate::{aialt, aicaption, preview};

/// Shows/hides an alt-text-length warning icon and sets its tooltip from
/// `media::alt_text_length_warning` - shared by the initial state when a
/// dialog opens and every subsequent edit.
fn update_alt_length_warning(icon: &gtk4::Image, text: &str) {
    match media::alt_text_length_warning(text) {
        Some(message) => {
            icon.set_tooltip_text(Some(&message));
            icon.set_visible(true);
        }
        None => icon.set_visible(false),
    }
}

/// Rebuilds the context-menu section (see `install`) from whether `line`
/// actually holds a media reference - empty when it doesn't, so the
/// section contributes no items (and no stray separator) to the popover.
fn rebuild_menu_for_line(menu: &gio::Menu, buffer: &sourceview5::Buffer, line: i32) {
    menu.remove_all();
    let body = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
    if image_source_on_line(&body, line).is_none() {
        return;
    }
    menu.append(Some(&tr("Bildbeschriftung bearbeiten…")), Some("imagealt.set"));
}

/// Moves the cursor to wherever a secondary click lands (see the module
/// docs for why this can't just rely on default `Gtk.TextView` behavior),
/// rebuilds the returned menu section from that now-accurate position, and
/// wires the `imagealt.set` action the section's item invokes.
pub fn install(
    view: &sourceview5::View,
    buffer: &sourceview5::Buffer,
    frontmatter: Rc<RefCell<Frontmatter>>,
    current_path: Rc<RefCell<Option<PathBuf>>>,
    preview_pane: Rc<preview::PreviewPane>,
) -> gio::Menu {
    let menu = gio::Menu::new();

    let gesture = gtk4::GestureClick::new();
    gesture.set_button(3); // secondary/right button
    {
        let buffer = buffer.clone();
        let menu = menu.clone();
        let view_weak = view.downgrade();
        gesture.connect_pressed(move |_gesture, _n_press, x, y| {
            let Some(view) = view_weak.upgrade() else { return };
            let (buffer_x, buffer_y) = view.window_to_buffer_coords(gtk4::TextWindowType::Widget, x as i32, y as i32);
            if let Some((iter, _trailing)) = view.iter_at_position(buffer_x, buffer_y) {
                // A right-click into the selection keeps it, otherwise
                // Cut/Copy and the KI-Aktionen would have nothing to act on.
                let inside_selection = buffer.selection_bounds().is_some_and(|(start, end)| iter.in_range(&start, &end) || iter == end);
                if !inside_selection {
                    buffer.place_cursor(&iter);
                }
            }
            let line = buffer.iter_at_mark(&buffer.get_insert()).line();
            rebuild_menu_for_line(&menu, &buffer, line);
        });
    }
    view.add_controller(gesture);

    let actions = gio::SimpleActionGroup::new();

    let set_action = gio::SimpleAction::new("set", None);
    {
        let buffer = buffer.clone();
        let frontmatter = frontmatter.clone();
        let current_path = current_path.clone();
        let preview_pane = preview_pane.clone();
        let view_weak = view.downgrade();
        set_action.connect_activate(move |_, _| {
            let Some(view) = view_weak.upgrade() else { return };
            let Some(window) = view.root().and_then(|root| root.downcast::<gtk4::Window>().ok()) else {
                return;
            };
            let line = buffer.iter_at_mark(&buffer.get_insert()).line();
            let doc_dir = current_path.borrow().as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf()));
            open_for_line(&window, &buffer, &frontmatter, line, doc_dir, &preview_pane);
        });
    }
    actions.add_action(&set_action);

    view.insert_action_group("imagealt", Some(&actions));

    menu
}

/// Plain-text context for the AI caption prompt (`aicaption.rs`): the
/// article text immediately surrounding the image whose reference is
/// `source` - the top-level block before it and the one after it, in
/// document order, joined by a blank line - not the whole article, so the
/// prompt stays focused on what the photo is actually illustrating rather
/// than drowning in unrelated sections. Formatting markup is dropped (only
/// text/inline-code content is kept) since this feeds a text prompt, not
/// another render. Empty if the reference can't be found, or there's no
/// block on either side (e.g. an image right at the very start or end of
/// the article) - `aicaption.rs` treats that as "no extra context", not an
/// error.
pub fn surrounding_context(markdown: &str, source: &str) -> String {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let events: Vec<(Event, std::ops::Range<usize>)> = Parser::new_ext(markdown, options).into_offset_iter().collect();

    let mut blocks: Vec<(usize, usize, bool)> = Vec::new();
    let mut i = 0;
    while i < events.len() {
        match &events[i].0 {
            Event::Start(tag) => {
                let end_marker = tag.to_end();
                let end = find_matching_end(&events, i, &end_marker);
                let has_image = events[i..=end]
                    .iter()
                    .any(|(event, _)| matches!(event, Event::Start(Tag::Image { dest_url, .. }) if dest_url.as_ref() == source));
                blocks.push((i, end, has_image));
                i = end + 1;
            }
            Event::Rule => {
                blocks.push((i, i, false));
                i += 1;
            }
            _ => i += 1,
        }
    }

    let Some(target) = blocks.iter().position(|(_, _, has_image)| *has_image) else {
        return String::new();
    };

    let mut parts = Vec::new();
    if target > 0 {
        let (start, end, _) = blocks[target - 1];
        let text = block_plain_text(&events[start..=end]);
        if !text.is_empty() {
            parts.push(text);
        }
    }
    if target + 1 < blocks.len() {
        let (start, end, _) = blocks[target + 1];
        let text = block_plain_text(&events[start..=end]);
        if !text.is_empty() {
            parts.push(text);
        }
    }
    parts.join("\n\n")
}

/// Concatenates the visible text of a slice of events, dropping all markup -
/// `Event::Text`/`Event::Code` content only, with soft/hard breaks folded to
/// a single space, so multi-line source wraps into one flowing sentence.
fn block_plain_text(events: &[(Event, std::ops::Range<usize>)]) -> String {
    let mut out = String::new();
    for (event, _) in events {
        match event {
            Event::Text(text) | Event::Code(text) => out.push_str(text),
            Event::SoftBreak | Event::HardBreak => out.push(' '),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// Same `Tag::to_end()` depth-counting trick `preview.rs`/`crates/gutenberg`
/// both already use - duplicated rather than shared, matching how those two
/// already keep their own private copies (see `preview.rs::find_matching_end`'s
/// doc comment).
fn find_matching_end(events: &[(Event, std::ops::Range<usize>)], start: usize, end_marker: &TagEnd) -> usize {
    let mut depth = 0usize;
    let mut j = start;
    while j < events.len() {
        match &events[j].0 {
            Event::Start(t) if &t.to_end() == end_marker => depth += 1,
            Event::End(e) if e == end_marker => {
                depth -= 1;
                if depth == 0 {
                    return j;
                }
            }
            _ => {}
        }
        j += 1;
    }
    events.len().saturating_sub(1)
}

/// Finds the `![alt](source)` reference starting on `line` (0-indexed,
/// matching `Gtk.TextIter::line()`), reconciles the tracked media list
/// against the current body so the reference is guaranteed to have a
/// `MediaItem`, then delegates to `open_dialog_for_index` for that image's
/// own alt text and caption.
fn open_for_line(window: &gtk4::Window, buffer: &sourceview5::Buffer, frontmatter: &Rc<RefCell<Frontmatter>>, line: i32, doc_dir: Option<PathBuf>, preview_pane: &Rc<preview::PreviewPane>) {
    let body = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();

    let Some(source) = image_source_on_line(&body, line) else {
        let alert = adw::AlertDialog::builder()
            .heading(tr("Keine Bildreferenz gefunden"))
            .body(tr("Für die Bildbeschriftung bitte mit der rechten Maustaste auf eine Zeile mit einem Bild (![Beschreibung](bild.png)) klicken."))
            .build();
        alert.add_response("ok", &tr("OK"));
        alert.present(Some(window));
        return;
    };

    {
        let mut fm = frontmatter.borrow_mut();
        fm.media = media::reconcile(&fm.media, &body);
    }
    let Some(index) = frontmatter.borrow().media.iter().position(|item| item.source == source) else {
        return;
    };
    open_dialog_for_index(window, frontmatter, index, buffer, doc_dir, preview_pane);
}

/// Opens the manual alt-text/caption dialog for one already-known
/// `MediaItem` index - shared by `open_for_line` above (the editor
/// context-menu path, which first has to look up the index from a clicked
/// line) and the preview pane's own "Bildbeschriftung bearbeiten…" context
/// menu item (`preview.rs`, which already knows the index from the
/// clicked image). Same fields and wiring as `mediapanel.rs`'s row, minus
/// the upload button, which isn't part of what a quick shortcut needs.
///
/// Both fields are kept in sync with the Markdown, by explicit request,
/// using this app's own convention for where each one lives in the image
/// syntax - `![Bildunterschrift](bild.png "Alternativtext")`, the
/// *opposite* of CommonMark's usual bracket=alt/title=caption pairing (see
/// `media::markdown_image_text_for`'s own doc comment for why). Opening
/// the dialog seeds both fields from the live Markdown rather than a
/// possibly-stale cached `MediaItem`; closing it writes the final text of
/// both back into the Markdown (`apply_image_text_to_buffer`) - deferred
/// to close, not applied on every keystroke, so typing in either field
/// doesn't churn the editor's undo stack or re-trigger its live-preview
/// debounce on every character.
///
/// `doc_dir` is only used to show a thumbnail - for a local image that
/// still resolves to a real file; a remote source (already uploaded, or
/// opened from WordPress) has nothing to load without a network fetch
/// this dialog deliberately never makes.
///
/// Both fields also get a "generate with AI" suffix button
/// (`aialt::generate`/`aicaption::generate`) - folded in here rather than
/// kept as the separate "KI-Alternativtext generieren…"/"KI-Bildunterschrift
/// generieren…" menu items/dialogs this replaced, by explicit request, so
/// the whole per-image workflow lives in one place. Only shown for an
/// actual image (not video/audio), since vision-based generation makes no
/// sense for those - `![]()` syntax is reused for all local media (see
/// `crates/gutenberg`'s `as_lone_media`).
pub fn open_dialog_for_index(window: &gtk4::Window, frontmatter: &Rc<RefCell<Frontmatter>>, index: usize, buffer: &sourceview5::Buffer, doc_dir: Option<PathBuf>, preview_pane: &Rc<preview::PreviewPane>) {
    let Some(item) = frontmatter.borrow().media.get(index).cloned() else {
        return;
    };
    let markdown = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
    let (markdown_caption, markdown_alt) = media::markdown_image_text_for(&markdown, &item.source);
    let is_image = document::media_reference_kind(&item.source) == document::MediaReferenceKind::Image;

    let content = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(18).margin_top(18).margin_bottom(18).margin_start(18).margin_end(18).build();

    // Rendered through WebKit rather than `Gtk.Picture` - a plain
    // `gdk_pixbuf`-backed widget depends on the system having a WebP
    // gdk-pixbuf loader installed (a separate, easy-to-miss package this
    // app doesn't otherwise need and doesn't declare as a dependency), and
    // silently shows nothing at all without one - confirmed live: a blank
    // white box for a `.webp` source, this app's own default export
    // format for a compressed image. WebKit already decodes WebP natively
    // for the Vorschau pane and Browser tab regardless of that package, so
    // reusing it here needs no extra dependency and matches what already
    // reliably works elsewhere in the app.
    if crate::imageedit::is_local(&item.source) {
        let path = crate::export::resolve_local_path(&item.source, doc_dir.as_deref());
        if path.exists() {
            let thumbnail = webkit6::WebView::new();
            thumbnail.set_height_request(160);
            thumbnail.set_hexpand(true);
            thumbnail.add_css_class("card");
            content.append(&thumbnail);

            // A page loaded via `load_html` with no base URI has a null
            // origin, and WebKit's cross-origin rules then silently refuse
            // to load a `file://` image from it - passing the image's own
            // directory as the base URI (`preview::base_uri`, the same
            // helper the main Vorschau pane already relies on for its own
            // local images) gives the page a matching `file://` origin, so
            // a plain relative `src` resolves and loads normally. Loading
            // is deferred to `connect_map` (fires once this specific
            // WebView is actually realized/shown) rather than called right
            // after construction - confirmed live: calling it immediately,
            // before this widget had a real size or was part of the
            // window's widget tree yet, left it permanently blank even
            // with the base URI fix in place.
            let filename = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            let html = format!(
                "<!doctype html><html><head><meta charset=\"utf-8\"><style>\
                 html, body {{ margin: 0; height: 100%; display: flex; align-items: center; justify-content: center; }}\
                 img {{ max-width: 100%; max-height: 100%; object-fit: contain; }}\
                 </style></head><body><img src=\"{filename}\"></body></html>"
            );
            let base_uri = preview::base_uri(path.parent());
            thumbnail.connect_map(move |view| {
                view.load_html(&html, base_uri.as_deref());
            });
        }
    }
    let filename_label = gtk4::Label::builder().label(&item.filename).xalign(0.0).wrap(true).build();
    filename_label.add_css_class("dim-label");
    filename_label.add_css_class("caption");
    content.append(&filename_label);

    // Prefers the live Markdown title slot when one exists, not the cached
    // `MediaItem.alt` - see this function's own doc comment for why both
    // fields are meant to track the Markdown, not drift from it. Falls
    // back to the cached three-way `AltText` state (Text/Empty/Undefined)
    // when there's no title yet - a plain "absent" title can't distinguish
    // "deliberately blank" from "never set", so that distinction only
    // survives via the cache until the Markdown actually gets one.
    let (initial_alt_active, initial_alt_text) = match &markdown_alt {
        Some(text) => (true, text.clone()),
        None => match &item.alt {
            AltText::Text(text) => (true, text.clone()),
            AltText::Empty => (true, String::new()),
            AltText::Undefined => (false, String::new()),
        },
    };
    let alt_switch_row = adw::SwitchRow::builder()
        .title(tr("Alternativtext definieren"))
        .subtitle(tr("Aus lassen für rein dekorative Bilder - das ist kein Fehler"))
        .active(initial_alt_active)
        .build();

    let alt_entry_row = adw::EntryRow::builder().title(tr("Alternativtext")).text(initial_alt_text.as_str()).build();
    alt_entry_row.set_visible(alt_switch_row.is_active());

    // Shared feedback for either "generate with AI" button below - only
    // one can run at a time in practice (both disable themselves while
    // busy), so one label is enough.
    let ai_status_label = gtk4::Label::builder().wrap(true).xalign(0.0).build();
    ai_status_label.add_css_class("dim-label");
    ai_status_label.set_visible(false);

    if is_image {
        let ai_alt_button = gtk4::Button::from_icon_name("chat-message-new-symbolic");
        ai_alt_button.set_tooltip_text(Some(&tr("KI-Alternativtext generieren…")));
        ai_alt_button.set_valign(gtk4::Align::Center);
        ai_alt_button.add_css_class("flat");
        alt_entry_row.add_suffix(&ai_alt_button);

        let source = item.source.clone();
        let doc_dir = doc_dir.clone();
        let ai_alt_button_for_click = ai_alt_button.clone();
        let alt_switch_row = alt_switch_row.clone();
        let alt_entry_row = alt_entry_row.clone();
        let ai_status_label = ai_status_label.clone();
        ai_alt_button.connect_clicked(move |_| {
            ai_alt_button_for_click.set_sensitive(false);
            ai_status_label.set_label(&tr("Wird generiert …"));
            ai_status_label.set_visible(true);
            let ai_alt_button_for_click = ai_alt_button_for_click.clone();
            let alt_switch_row = alt_switch_row.clone();
            let alt_entry_row = alt_entry_row.clone();
            let ai_status_label = ai_status_label.clone();
            aialt::generate(&source, doc_dir.clone(), move |result| {
                match result {
                    Ok(text) => {
                        alt_switch_row.set_active(true);
                        alt_entry_row.set_text(&text);
                        ai_status_label.set_visible(false);
                    }
                    Err(err) => ai_status_label.set_label(&tr("Fehler: {err}").replace("{err}", &err)),
                }
                ai_alt_button_for_click.set_sensitive(true);
            });
        });
    }

    // Non-blocking hint for an unusually long alt text (see
    // `media::alt_text_length_warning`) - only ever a suffix icon with a
    // tooltip, never something that stops the dialog from being closed, since
    // some images genuinely need a longer description.
    let alt_length_warning_icon = gtk4::Image::from_icon_name("dialog-warning-symbolic");
    alt_length_warning_icon.add_css_class("warning");
    alt_length_warning_icon.set_visible(false);
    alt_entry_row.add_suffix(&alt_length_warning_icon);

    {
        let frontmatter = frontmatter.clone();
        let alt_entry_row = alt_entry_row.clone();
        let alt_length_warning_icon = alt_length_warning_icon.clone();
        let preview_pane = preview_pane.clone();
        alt_switch_row.connect_active_notify(move |row| {
            let active = row.is_active();
            alt_entry_row.set_visible(active);
            if let Some(item) = frontmatter.borrow_mut().media.get_mut(index) {
                item.alt = if active {
                    let text = alt_entry_row.text().to_string();
                    if text.is_empty() { AltText::Empty } else { AltText::Text(text) }
                } else {
                    AltText::Undefined
                };
            }
            update_alt_length_warning(&alt_length_warning_icon, &alt_entry_row.text());
            preview_pane.refresh_media(&frontmatter.borrow().media);
        });
    }
    {
        let frontmatter = frontmatter.clone();
        let alt_switch_row = alt_switch_row.clone();
        let alt_length_warning_icon = alt_length_warning_icon.clone();
        let preview_pane = preview_pane.clone();
        alt_entry_row.connect_changed(move |row| {
            if !alt_switch_row.is_active() {
                return;
            }
            let text = row.text().to_string();
            update_alt_length_warning(&alt_length_warning_icon, &text);
            if let Some(item) = frontmatter.borrow_mut().media.get_mut(index) {
                item.alt = if text.is_empty() { AltText::Empty } else { AltText::Text(text) };
            }
            preview_pane.refresh_media(&frontmatter.borrow().media);
        });
    }
    update_alt_length_warning(&alt_length_warning_icon, &alt_entry_row.text());

    let alt_group = adw::PreferencesGroup::builder()
        .title(tr("Alternativtext"))
        .description(tr("Für Screenreader - wird im Artikel selbst nicht sichtbar angezeigt."))
        .build();
    alt_group.add(&alt_switch_row);
    alt_group.add(&alt_entry_row);

    let initial_caption = markdown_caption.unwrap_or_else(|| item.caption.clone().unwrap_or_default());
    let caption_row = adw::EntryRow::builder().title(tr("Bildunterschrift")).text(initial_caption.as_str()).build();
    {
        let frontmatter = frontmatter.clone();
        let preview_pane = preview_pane.clone();
        caption_row.connect_changed(move |row| {
            let text = row.text().to_string();
            if let Some(item) = frontmatter.borrow_mut().media.get_mut(index) {
                item.caption = (!text.is_empty()).then_some(text);
            }
            preview_pane.refresh_media(&frontmatter.borrow().media);
        });
    }

    if is_image {
        let ai_caption_button = gtk4::Button::from_icon_name("chat-message-new-symbolic");
        ai_caption_button.set_tooltip_text(Some(&tr("KI-Bildunterschrift generieren…")));
        ai_caption_button.set_valign(gtk4::Align::Center);
        ai_caption_button.add_css_class("flat");
        caption_row.add_suffix(&ai_caption_button);

        let source = item.source.clone();
        let context = surrounding_context(&markdown, &item.source);
        let doc_dir = doc_dir.clone();
        let ai_caption_button_for_click = ai_caption_button.clone();
        let caption_row_for_result = caption_row.clone();
        let ai_status_label = ai_status_label.clone();
        ai_caption_button.connect_clicked(move |_| {
            ai_caption_button_for_click.set_sensitive(false);
            ai_status_label.set_label(&tr("Wird generiert …"));
            ai_status_label.set_visible(true);
            let ai_caption_button_for_click = ai_caption_button_for_click.clone();
            let caption_row_for_result = caption_row_for_result.clone();
            let ai_status_label = ai_status_label.clone();
            aicaption::generate(&source, &context, doc_dir.clone(), move |result| {
                match result {
                    Ok(text) => {
                        caption_row_for_result.set_text(&text);
                        ai_status_label.set_visible(false);
                    }
                    Err(err) => ai_status_label.set_label(&tr("Fehler: {err}").replace("{err}", &err)),
                }
                ai_caption_button_for_click.set_sensitive(true);
            });
        });
    }

    let caption_group = adw::PreferencesGroup::builder()
        .title(tr("Bildunterschrift"))
        .description(tr("Sichtbarer Text, der im Artikel unter dem Bild angezeigt wird."))
        .build();
    caption_group.add(&caption_row);

    content.append(&alt_group);
    content.append(&caption_group);
    content.append(&ai_status_label);

    let clamp = adw::Clamp::builder().maximum_size(420).child(&content).build();
    // `propagate_natural_height` lets the scroller size to fit everything
    // when it actually does fit (the normal case) rather than always
    // reserving a scrollbar's worth of cropped content - `content_height`
    // below is still a hard cap for the rare case that doesn't fit (a very
    // long alt text wrapping to several lines), not the everyday size.
    let scroller = gtk4::ScrolledWindow::builder().child(&clamp).vexpand(true).propagate_natural_height(true).build();

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&adw::HeaderBar::new());
    toolbar_view.set_content(Some(&scroller));

    let dialog = adw::Dialog::builder().title(tr("Bildbeschriftung")).content_width(440).content_height(720).child(&toolbar_view).build();

    {
        let buffer = buffer.clone();
        let source = item.source.clone();
        let alt_switch_row = alt_switch_row.clone();
        let alt_entry_row = alt_entry_row.clone();
        let caption_row = caption_row.clone();
        dialog.connect_closed(move |_| {
            let caption_text = caption_row.text().to_string();
            let caption = (!caption_text.trim().is_empty()).then_some(caption_text);
            let alt = alt_switch_row.is_active().then(|| alt_entry_row.text().to_string()).filter(|text| !text.trim().is_empty());
            apply_image_text_to_buffer(&buffer, &source, caption.as_deref(), alt.as_deref());
        });
    }

    dialog.present(Some(window));
}

/// Writes `caption`/`alt` into the Markdown bracket/title slots for every
/// reference to `source` in `buffer` (see `media::image_text_edits_for`) -
/// edits are applied back-to-front by position so an earlier edit's byte
/// range is never shifted by a later one still waiting to be applied.
fn apply_image_text_to_buffer(buffer: &sourceview5::Buffer, source: &str, caption: Option<&str>, alt: Option<&str>) {
    let markdown = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
    let mut edits = media::image_text_edits_for(&markdown, source, caption, alt);
    edits.sort_by_key(|edit| std::cmp::Reverse(edit.0.start));
    for (byte_range, replacement) in edits {
        let start_chars = markdown[..byte_range.start].chars().count() as i32;
        let end_chars = markdown[..byte_range.end].chars().count() as i32;
        let mut start_iter = buffer.iter_at_offset(start_chars);
        let mut end_iter = buffer.iter_at_offset(end_chars);
        buffer.delete(&mut start_iter, &mut end_iter);
        buffer.insert(&mut start_iter, &replacement);
    }
}

/// Scans `markdown` for an `![alt](source)` reference whose opening `![`
/// starts on `line` (0-indexed) - deliberately line-based rather than
/// matching by click column, since the whole point is "the image this
/// line is about", not needing the click to land exactly on the syntax.
fn image_source_on_line(markdown: &str, line: i32) -> Option<String> {
    for (event, range) in pulldown_cmark::Parser::new(markdown).into_offset_iter() {
        if let pulldown_cmark::Event::Start(pulldown_cmark::Tag::Image { dest_url, .. }) = event {
            let start_line = markdown[..range.start].matches('\n').count() as i32;
            if start_line == line {
                return Some(dest_url.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_image_source_on_the_given_line() {
        let markdown = "Intro.\n\n![a cat](cat.png)\n\nOutro.\n";
        assert_eq!(image_source_on_line(markdown, 2), Some("cat.png".to_string()));
    }

    #[test]
    fn returns_none_for_a_line_without_an_image() {
        let markdown = "Intro.\n\n![a cat](cat.png)\n\nOutro.\n";
        assert_eq!(image_source_on_line(markdown, 0), None);
        assert_eq!(image_source_on_line(markdown, 4), None);
    }

    #[test]
    fn surrounding_context_returns_the_paragraphs_before_and_after() {
        let markdown = "Intro text before the photo.\n\n![a cat](cat.png)\n\nOutro text after the photo.\n";
        assert_eq!(surrounding_context(markdown, "cat.png"), "Intro text before the photo.\n\nOutro text after the photo.");
    }

    #[test]
    fn surrounding_context_handles_an_image_with_no_block_before_it() {
        let markdown = "![a cat](cat.png)\n\nOutro text after the photo.\n";
        assert_eq!(surrounding_context(markdown, "cat.png"), "Outro text after the photo.");
    }

    #[test]
    fn surrounding_context_handles_an_image_with_no_block_after_it() {
        let markdown = "Intro text before the photo.\n\n![a cat](cat.png)\n";
        assert_eq!(surrounding_context(markdown, "cat.png"), "Intro text before the photo.");
    }

    #[test]
    fn surrounding_context_is_empty_when_the_source_is_not_found() {
        let markdown = "Intro.\n\n![a cat](cat.png)\n\nOutro.\n";
        assert_eq!(surrounding_context(markdown, "dog.png"), "");
    }

    #[test]
    fn surrounding_context_uses_a_heading_neighbor_as_context_too() {
        let markdown = "## Der Ausflug\n\n![a cat](cat.png)\n\nOutro text.\n";
        assert_eq!(surrounding_context(markdown, "cat.png"), "Der Ausflug\n\nOutro text.");
    }
}
