//! "Bild bearbeiten…" - right-click a rendered image in the Vorschau pane
//! (see `preview.rs::install_image_edit_menu`) to convert it to a different
//! format (PNG/JPEG/WebP) and/or resize it by width or height. Unlike
//! `imagecompress.rs` (a best-effort, invisible re-encode applied only to
//! the bytes *uploaded* to WordPress, never touching the local file), this
//! is a direct, visible edit the user asked for: it writes a new local file
//! and updates the article's own `![alt](src)` reference to point at it,
//! leaving the original file untouched on disk in case it's still wanted.
//!
//! Built on `gdk-pixbuf` for decode/resize/PNG/JPEG (already part of the
//! GTK stack, same as `imagecompress.rs`) plus the `webp` crate for WebP
//! encoding specifically - gdk-pixbuf's own WebP loader (where installed at
//! all) is read-only, it cannot *write* WebP.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk4::glib;

use crate::document::Frontmatter;
use crate::export;
use crate::i18n::tr;

const JPEG_QUALITY: &str = "90";
const WEBP_QUALITY: f32 = 85.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    WebP,
}

impl ImageFormat {
    pub const ALL: [ImageFormat; 3] = [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::WebP];

    fn extension(&self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::WebP => "webp",
        }
    }

    // Format names, not UI prose - left untranslated like provider names
    // elsewhere in this app (see po/README.md).
    fn label(&self) -> &'static str {
        match self {
            ImageFormat::Png => "PNG",
            ImageFormat::Jpeg => "JPEG",
            ImageFormat::WebP => "WebP",
        }
    }

    /// Guesses the format from a filename's current extension, for
    /// preselecting the dialog's format picker - falls back to JPEG (the
    /// most common photo format) for anything unrecognized rather than
    /// refusing to open at all.
    fn from_extension(filename: &str) -> ImageFormat {
        match filename.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
            "png" => ImageFormat::Png,
            "webp" => ImageFormat::WebP,
            _ => ImageFormat::Jpeg,
        }
    }
}

/// Computes the actual output size from what the dialog's two entry rows
/// hold - pure and gdk-pixbuf-free so the resize math is unit-testable
/// without a real image. `None` for both means "don't resize at all".
/// With `keep_aspect`, a width takes priority over a height if both are
/// somehow set (the dialog only really expects one to be filled in at a
/// time); without it, either axis missing simply keeps that axis's
/// original size rather than distorting it implicitly.
fn compute_target_dimensions(original_width: u32, original_height: u32, target_width: Option<u32>, target_height: Option<u32>, keep_aspect: bool) -> (u32, u32) {
    if !keep_aspect {
        return (target_width.unwrap_or(original_width).max(1), target_height.unwrap_or(original_height).max(1));
    }
    match (target_width, target_height) {
        (Some(w), _) => {
            let w = w.max(1);
            let h = ((f64::from(w)) * f64::from(original_height) / f64::from(original_width)).round().max(1.0) as u32;
            (w, h)
        }
        (None, Some(h)) => {
            let h = h.max(1);
            let w = ((f64::from(h)) * f64::from(original_width) / f64::from(original_height)).round().max(1.0) as u32;
            (w, h)
        }
        (None, None) => (original_width, original_height),
    }
}

/// The new file's reference, in the same style as `old_source` (preserving
/// any directory prefix it already had, e.g. `images/photo.png` ->
/// `images/photo-bearbeitet.webp`) - a sibling of the original rather than
/// overwriting it in place, so the edit is always undoable by hand (delete
/// the new file, the old reference still works) even outside Blocksatz's
/// own Ctrl+Z.
fn sibling_reference(old_source: &str, format: ImageFormat) -> String {
    let (dir_prefix, filename) = match old_source.rsplit_once('/') {
        Some((dir, name)) => (format!("{dir}/"), name),
        None => (String::new(), old_source),
    };
    let stem = filename.rsplit_once('.').map_or(filename, |(stem, _)| stem);
    format!("{dir_prefix}{stem}-bearbeitet.{}", format.extension())
}

fn decode(bytes: &[u8]) -> Option<gdk_pixbuf::Pixbuf> {
    let loader = gdk_pixbuf::PixbufLoader::new();
    loader.write(bytes).ok()?;
    loader.close().ok()?;
    loader.pixbuf()
}

/// Flattens an RGBA pixbuf onto an opaque white background - JPEG has no
/// alpha channel, so converting a transparent image to JPEG without this
/// would either fail or (depending on the encoder) silently discard
/// whatever color happened to be in the fully-transparent pixels, which
/// can look very wrong. Returns the input unchanged if it has no alpha to
/// begin with.
fn flatten_alpha_onto_white(pixbuf: &gdk_pixbuf::Pixbuf) -> gdk_pixbuf::Pixbuf {
    if !pixbuf.has_alpha() {
        return pixbuf.clone();
    }
    let (width, height) = (pixbuf.width(), pixbuf.height());
    let flattened = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, width, height).expect("valid, already-decoded image dimensions");
    flattened.fill(0xffff_ffff);
    pixbuf.composite(&flattened, 0, 0, width, height, 0.0, 0.0, 1.0, 1.0, gdk_pixbuf::InterpType::Nearest, 255);
    flattened
}

/// Copies a pixbuf's pixel data into a tightly-packed RGB(A) buffer with no
/// per-row padding - `Pixbuf::pixels()` is laid out with `rowstride()`
/// bytes per row, which is only ever equal to `width * channels` by
/// coincidence, and the `webp` crate's encoder needs pixels packed with no
/// gaps at all.
fn packed_pixels(pixbuf: &gdk_pixbuf::Pixbuf) -> (Vec<u8>, bool) {
    let width = pixbuf.width() as usize;
    let height = pixbuf.height() as usize;
    let n_channels = pixbuf.n_channels() as usize;
    let rowstride = pixbuf.rowstride() as usize;
    let has_alpha = pixbuf.has_alpha();
    let pixels = unsafe { pixbuf.pixels() };
    let mut packed = Vec::with_capacity(width * height * n_channels);
    for row in 0..height {
        let start = row * rowstride;
        packed.extend_from_slice(&pixels[start..start + width * n_channels]);
    }
    (packed, has_alpha)
}

fn encode(pixbuf: &gdk_pixbuf::Pixbuf, format: ImageFormat) -> Result<Vec<u8>, String> {
    match format {
        ImageFormat::Png => pixbuf.save_to_bufferv("png", &[("compression", "9")]).map_err(|err| err.to_string()),
        ImageFormat::Jpeg => {
            let flattened = flatten_alpha_onto_white(pixbuf);
            flattened.save_to_bufferv("jpeg", &[("quality", JPEG_QUALITY)]).map_err(|err| err.to_string())
        }
        ImageFormat::WebP => Ok(encode_webp(pixbuf)),
    }
}

/// `pixbuf` as WebP, keeping its alpha channel - also what every PNG/JPEG
/// upload is converted to (`imagecompress.rs`).
pub(crate) fn encode_webp(pixbuf: &gdk_pixbuf::Pixbuf) -> Vec<u8> {
    let (packed, has_alpha) = packed_pixels(pixbuf);
    let width = pixbuf.width() as u32;
    let height = pixbuf.height() as u32;
    let encoder = if has_alpha { webp::Encoder::from_rgba(&packed, width, height) } else { webp::Encoder::from_rgb(&packed, width, height) };
    encoder.encode(WEBP_QUALITY).to_vec()
}

/// Decodes, optionally resizes, and re-encodes `bytes` as `format` -
/// the whole non-UI, non-blocking-thread-aware core of this feature, run
/// on a background thread by `open`'s apply handler.
fn convert_and_resize(bytes: &[u8], format: ImageFormat, target_width: Option<u32>, target_height: Option<u32>, keep_aspect: bool, no_upscale: bool) -> Result<Vec<u8>, String> {
    let pixbuf = decode(bytes).ok_or_else(|| tr("Bild konnte nicht gelesen werden - unbekanntes oder beschädigtes Format."))?;
    let (width, height) = (pixbuf.width(), pixbuf.height());
    if width <= 0 || height <= 0 {
        return Err(tr("Bild konnte nicht gelesen werden - unbekanntes oder beschädigtes Format."));
    }
    let (mut target_w, mut target_h) = compute_target_dimensions(width as u32, height as u32, target_width, target_height, keep_aspect);
    // A batch edit ("all images to 1280px wide") must not blow up the
    // smaller ones: keep their size instead.
    if no_upscale && (target_w > width as u32 || target_h > height as u32) {
        (target_w, target_h) = (width as u32, height as u32);
    }
    let resized = if (target_w, target_h) == (width as u32, height as u32) {
        pixbuf
    } else {
        pixbuf
            .scale_simple(target_w as i32, target_h as i32, gdk_pixbuf::InterpType::Bilinear)
            .ok_or_else(|| tr("Bild konnte nicht skaliert werden."))?
    };
    encode(&resized, format)
}

fn parse_dimension(text: &str) -> Option<u32> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    text.parse::<u32>().ok().filter(|n| *n > 0)
}

/// Opens the "Bild bearbeiten" dialog for `frontmatter.media[index]` - the
/// caller is responsible for having already reconciled the media list
/// against the current document body (see `preview.rs::install_image_edit_
/// menu`), so `index` is guaranteed valid at the moment this is called.
pub fn open(window: &gtk4::Window, frontmatter: Rc<RefCell<Frontmatter>>, index: usize, doc_dir: Option<PathBuf>, buffer: sourceview5::Buffer) {
    let Some(item) = frontmatter.borrow().media.get(index).cloned() else { return };

    let current_format = ImageFormat::from_extension(&item.filename);
    let format_labels: Vec<&str> = ImageFormat::ALL.iter().map(ImageFormat::label).collect();
    let selected_index = ImageFormat::ALL.iter().position(|f| *f == current_format).unwrap_or(0);
    let format_row = adw::ComboRow::builder().title(tr("Format")).model(&gtk4::StringList::new(&format_labels)).selected(selected_index as u32).build();

    let width_row = adw::EntryRow::builder().title(tr("Breite (px)")).build();
    let height_row = adw::EntryRow::builder().title(tr("Höhe (px)")).build();
    let keep_aspect_row = adw::SwitchRow::builder().title(tr("Seitenverhältnis beibehalten")).active(true).build();

    let group = adw::PreferencesGroup::builder().title(item.filename.clone()).build();
    group.add(&format_row);
    group.add(&width_row);
    group.add(&height_row);
    group.add(&keep_aspect_row);

    let status_label = gtk4::Label::builder().wrap(true).xalign(0.0).build();
    status_label.set_visible(false);

    let content = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(12).margin_top(18).margin_bottom(18).margin_start(18).margin_end(18).build();
    content.append(&group);
    content.append(&status_label);

    let apply_button = gtk4::Button::with_label(&tr("Anwenden"));
    apply_button.add_css_class("suggested-action");

    let header = adw::HeaderBar::new();
    header.pack_end(&apply_button);

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&content));

    let dialog = adw::Dialog::builder().title(tr("Bild bearbeiten")).content_width(420).content_height(360).child(&toolbar_view).build();

    for row in [&width_row, &height_row] {
        let row = row.clone();
        row.connect_changed(move |row| {
            let text = row.text().to_string();
            row.remove_css_class("error");
            if !text.trim().is_empty() && parse_dimension(&text).is_none() {
                row.add_css_class("error");
            }
        });
    }

    {
        let source = item.source.clone();
        let doc_dir = doc_dir.clone();
        let format_row = format_row.clone();
        let width_row = width_row.clone();
        let height_row = height_row.clone();
        let keep_aspect_row = keep_aspect_row.clone();
        let apply_button_for_click = apply_button.clone();
        let status_label = status_label.clone();
        let buffer = buffer.clone();
        let dialog = dialog.clone();
        apply_button.connect_clicked(move |_| {
            let format = ImageFormat::ALL[format_row.selected() as usize];
            let target_width = parse_dimension(&width_row.text());
            let target_height = parse_dimension(&height_row.text());
            let keep_aspect = keep_aspect_row.is_active();
            let new_source = sibling_reference(&source, format);
            let output_path = export::resolve_local_path(&new_source, doc_dir.as_deref());

            apply_button_for_click.set_sensitive(false);
            status_label.set_label(&tr("Wird verarbeitet …"));
            status_label.set_visible(true);

            let source_for_thread = source.clone();
            let doc_dir_for_thread = doc_dir.clone();
            let (tx, rx) = mpsc::channel::<Result<(), String>>();
            std::thread::spawn(move || {
                let outcome = export::read_image_bytes(&source_for_thread, doc_dir_for_thread.as_deref())
                    .and_then(|bytes| convert_and_resize(&bytes, format, target_width, target_height, keep_aspect, false))
                    .and_then(|encoded| std::fs::write(&output_path, encoded).map_err(|err| err.to_string()));
                let _ = tx.send(outcome);
            });

            let apply_button = apply_button_for_click.clone();
            let status_label = status_label.clone();
            let buffer = buffer.clone();
            let dialog = dialog.clone();
            let source = source.clone();
            let new_source = new_source.clone();
            glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
                Ok(Ok(())) => {
                    let settings = sourceview5::SearchSettings::builder().search_text(source.as_str()).case_sensitive(true).build();
                    let context = sourceview5::SearchContext::builder().buffer(&buffer).settings(&settings).build();
                    let _ = context.replace_all(&new_source);
                    dialog.close();
                    glib::ControlFlow::Break
                }
                Ok(Err(err)) => {
                    status_label.set_label(&tr("Fehler: {err}").replace("{err}", &err));
                    apply_button.set_sensitive(true);
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    status_label.set_label(&tr("Interner Fehler: Bildbearbeitungs-Thread hat kein Ergebnis geliefert."));
                    apply_button.set_sensitive(true);
                    glib::ControlFlow::Break
                }
            });
        });
    }

    dialog.present(Some(window));
}

/// One image of "Alle Bilder bearbeiten…": its source and the new
/// reference, or what went wrong.
type BatchResult = (String, Result<String, String>);

/// The article's local images, each once, in order of appearance - what
/// "Alle Bilder bearbeiten…" works on. Videos, audio and remote images
/// are left out.
fn local_images(frontmatter: &Frontmatter) -> Vec<String> {
    let mut sources: Vec<String> = Vec::new();
    for item in &frontmatter.media {
        if is_local(&item.source) && crate::document::media_reference_kind(&item.source) == crate::document::MediaReferenceKind::Image && !sources.contains(&item.source) {
            sources.push(item.source.clone());
        }
    }
    sources
}

/// "Alle Bilder bearbeiten…": the same edit as `open`, applied to every
/// local image of the article in one go - format (or each keeps its own),
/// size and aspect, and by default without enlarging images smaller than
/// the target. Each result is a sibling file like a single edit, and the
/// article's references are switched over at the end.
pub fn open_all(window: &gtk4::Window, frontmatter: Rc<RefCell<Frontmatter>>, doc_dir: Option<PathBuf>, buffer: sourceview5::Buffer) {
    let body = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
    {
        let mut fm = frontmatter.borrow_mut();
        fm.media = crate::media::reconcile(&fm.media, &body);
    }
    let sources = local_images(&frontmatter.borrow());
    if sources.is_empty() {
        let alert = adw::AlertDialog::new(Some(&tr("Keine Bilder")), Some(&tr("Der Artikel enthält keine lokalen Bilder.")));
        alert.add_response("ok", &tr("OK"));
        alert.present(Some(window));
        return;
    }

    let mut format_labels: Vec<String> = vec![tr("Unverändert")];
    format_labels.extend(ImageFormat::ALL.iter().map(|f| f.label().to_string()));
    let format_label_refs: Vec<&str> = format_labels.iter().map(String::as_str).collect();
    let format_row = adw::ComboRow::builder().title(tr("Format")).model(&gtk4::StringList::new(&format_label_refs)).selected(0).build();
    let width_row = adw::EntryRow::builder().title(tr("Breite (px)")).build();
    let height_row = adw::EntryRow::builder().title(tr("Höhe (px)")).build();
    let keep_aspect_row = adw::SwitchRow::builder().title(tr("Seitenverhältnis beibehalten")).active(true).build();
    let no_upscale_row = adw::SwitchRow::builder().title(tr("Kleinere Bilder nicht vergrößern")).active(true).build();

    let count_title = if sources.len() == 1 { tr("1 Bild im Artikel") } else { tr("{n} Bilder im Artikel").replace("{n}", &sources.len().to_string()) };
    let group = adw::PreferencesGroup::builder()
        .title(count_title)
        .description(tr("Jedes Bild wird als neue Datei „…-bearbeitet“ neben dem Original gespeichert, der Artikel verweist danach darauf."))
        .build();
    group.add(&format_row);
    group.add(&width_row);
    group.add(&height_row);
    group.add(&keep_aspect_row);
    group.add(&no_upscale_row);

    let status_label = gtk4::Label::builder().wrap(true).xalign(0.0).build();
    status_label.set_visible(false);
    let progress = gtk4::ProgressBar::new();
    progress.set_visible(false);

    let content = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(12).margin_top(18).margin_bottom(18).margin_start(18).margin_end(18).build();
    content.append(&group);
    content.append(&progress);
    content.append(&status_label);

    let apply_button = gtk4::Button::with_label(&tr("Alle bearbeiten"));
    apply_button.add_css_class("suggested-action");
    apply_button.set_sensitive(false);
    let header = adw::HeaderBar::new();
    header.pack_end(&apply_button);
    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&gtk4::ScrolledWindow::builder().child(&content).hscrollbar_policy(gtk4::PolicyType::Never).build()));
    let dialog = adw::Dialog::builder().title(tr("Alle Bilder bearbeiten")).content_width(440).content_height(520).child(&toolbar_view).build();

    // Nothing to do while the format stays and no size is given.
    let update_sensitivity: Rc<dyn Fn()> = {
        let format_row = format_row.clone();
        let width_row = width_row.clone();
        let height_row = height_row.clone();
        let apply_button = apply_button.clone();
        Rc::new(move || {
            let invalid = [&width_row, &height_row].iter().any(|row| !row.text().trim().is_empty() && parse_dimension(&row.text()).is_none());
            let sized = parse_dimension(&width_row.text()).is_some() || parse_dimension(&height_row.text()).is_some();
            apply_button.set_sensitive(!invalid && (format_row.selected() > 0 || sized));
        })
    };
    for row in [&width_row, &height_row] {
        let update_sensitivity = update_sensitivity.clone();
        row.connect_changed(move |row| {
            let text = row.text().to_string();
            row.remove_css_class("error");
            if !text.trim().is_empty() && parse_dimension(&text).is_none() {
                row.add_css_class("error");
            }
            update_sensitivity();
        });
    }
    {
        let update_sensitivity = update_sensitivity.clone();
        format_row.connect_selected_notify(move |_| update_sensitivity());
    }

    {
        let dialog = dialog.clone();
        let apply_button_for_click = apply_button.clone();
        apply_button.connect_clicked(move |_| {
            let format_choice = match format_row.selected() {
                0 => None,
                n => ImageFormat::ALL.get(n as usize - 1).copied(),
            };
            let target_width = parse_dimension(&width_row.text());
            let target_height = parse_dimension(&height_row.text());
            let keep_aspect = keep_aspect_row.is_active();
            let no_upscale = no_upscale_row.is_active();
            for row in [format_row.upcast_ref::<gtk4::Widget>(), width_row.upcast_ref(), height_row.upcast_ref(), keep_aspect_row.upcast_ref(), no_upscale_row.upcast_ref()] {
                row.set_sensitive(false);
            }
            apply_button_for_click.set_sensitive(false);
            progress.set_visible(true);
            status_label.set_visible(true);

            // (index, source, new reference or error)
            let (tx, rx) = mpsc::channel::<(usize, String, Result<String, String>)>();
            let jobs = sources.clone();
            let doc_dir_for_thread = doc_dir.clone();
            std::thread::spawn(move || {
                for (index, source) in jobs.into_iter().enumerate() {
                    let format = format_choice.unwrap_or_else(|| ImageFormat::from_extension(&source));
                    let new_source = sibling_reference(&source, format);
                    let output_path = export::resolve_local_path(&new_source, doc_dir_for_thread.as_deref());
                    let outcome = export::read_image_bytes(&source, doc_dir_for_thread.as_deref())
                        .and_then(|bytes| convert_and_resize(&bytes, format, target_width, target_height, keep_aspect, no_upscale))
                        .and_then(|encoded| std::fs::write(&output_path, encoded).map_err(|err| err.to_string()))
                        .map(|()| new_source);
                    if tx.send((index, source, outcome)).is_err() {
                        return;
                    }
                }
            });

            let total = sources.len();
            let done: Rc<RefCell<Vec<BatchResult>>> = Rc::new(RefCell::new(Vec::new()));
            let progress = progress.clone();
            let status_label = status_label.clone();
            let buffer = buffer.clone();
            let dialog = dialog.clone();
            glib::timeout_add_local(Duration::from_millis(100), move || {
                loop {
                    match rx.try_recv() {
                        Ok((_, source, outcome)) => done.borrow_mut().push((source, outcome)),
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => break,
                    }
                }
                let finished = done.borrow().len();
                progress.set_fraction(finished as f64 / total as f64);
                status_label.set_label(&tr("Bild {i} von {n} …").replace("{i}", &finished.min(total).to_string()).replace("{n}", &total.to_string()));
                if finished < total {
                    return glib::ControlFlow::Continue;
                }
                // All through: switch the article's references over in one
                // user action, so a single Ctrl+Z undoes the lot.
                buffer.begin_user_action();
                let mut failed: Vec<String> = Vec::new();
                for (source, outcome) in done.borrow().iter() {
                    match outcome {
                        Ok(new_source) if new_source != source => {
                            let settings = sourceview5::SearchSettings::builder().search_text(source.as_str()).case_sensitive(true).build();
                            let context = sourceview5::SearchContext::builder().buffer(&buffer).settings(&settings).build();
                            let _ = context.replace_all(new_source);
                        }
                        Ok(_) => {}
                        Err(err) => failed.push(format!("{source}: {err}")),
                    }
                }
                buffer.end_user_action();
                if failed.is_empty() {
                    dialog.close();
                } else {
                    status_label.set_label(&tr("{k} von {n} Bildern bearbeitet. Fehler:\n{errors}").replace("{k}", &(total - failed.len()).to_string()).replace("{n}", &total.to_string()).replace("{errors}", &failed.join("\n")));
                }
                glib::ControlFlow::Break
            });
        });
    }

    dialog.present(Some(window));
}

/// Only offered for a local image - editing a remote (already-published,
/// e.g. from a WordPress-imported article) source would have nothing to
/// write back to.
pub fn is_local(source: &str) -> bool {
    !(source.starts_with("http://") || source.starts_with("https://"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_target_dimensions_leaves_size_unchanged() {
        assert_eq!(compute_target_dimensions(800, 600, None, None, true), (800, 600));
    }

    #[test]
    fn target_width_with_keep_aspect_scales_height_proportionally() {
        assert_eq!(compute_target_dimensions(800, 600, Some(400), None, true), (400, 300));
    }

    #[test]
    fn target_height_with_keep_aspect_scales_width_proportionally() {
        assert_eq!(compute_target_dimensions(800, 600, None, Some(300), true), (400, 300));
    }

    #[test]
    fn width_takes_priority_over_height_when_both_are_set_and_aspect_is_kept() {
        assert_eq!(compute_target_dimensions(800, 600, Some(400), Some(1000), true), (400, 300));
    }

    #[test]
    fn without_keep_aspect_both_axes_are_used_independently() {
        assert_eq!(compute_target_dimensions(800, 600, Some(400), Some(500), false), (400, 500));
    }

    #[test]
    fn without_keep_aspect_a_missing_axis_keeps_its_original_size() {
        assert_eq!(compute_target_dimensions(800, 600, Some(400), None, false), (400, 600));
        assert_eq!(compute_target_dimensions(800, 600, None, Some(500), false), (800, 500));
    }

    #[test]
    fn sibling_reference_preserves_a_directory_prefix() {
        assert_eq!(sibling_reference("images/photo.png", ImageFormat::WebP), "images/photo-bearbeitet.webp");
    }

    #[test]
    fn sibling_reference_handles_a_bare_filename() {
        assert_eq!(sibling_reference("photo.png", ImageFormat::Jpeg), "photo-bearbeitet.jpg");
    }

    #[test]
    fn sibling_reference_handles_a_filename_with_no_extension() {
        assert_eq!(sibling_reference("photo", ImageFormat::Png), "photo-bearbeitet.png");
    }

    #[test]
    fn from_extension_recognizes_known_formats() {
        assert_eq!(ImageFormat::from_extension("a.png"), ImageFormat::Png);
        assert_eq!(ImageFormat::from_extension("a.PNG"), ImageFormat::Png);
        assert_eq!(ImageFormat::from_extension("a.webp"), ImageFormat::WebP);
        assert_eq!(ImageFormat::from_extension("a.jpg"), ImageFormat::Jpeg);
        assert_eq!(ImageFormat::from_extension("a.jpeg"), ImageFormat::Jpeg);
        assert_eq!(ImageFormat::from_extension("a.gif"), ImageFormat::Jpeg);
    }

    #[test]
    fn parse_dimension_rejects_empty_zero_and_non_numeric_input() {
        assert_eq!(parse_dimension(""), None);
        assert_eq!(parse_dimension("   "), None);
        assert_eq!(parse_dimension("0"), None);
        assert_eq!(parse_dimension("abc"), None);
        assert_eq!(parse_dimension("400"), Some(400));
    }

    #[test]
    fn no_upscale_keeps_a_smaller_image_at_its_size() {
        let pixbuf = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, 400, 300).unwrap();
        let png = pixbuf.save_to_bufferv("png", &[]).unwrap();
        let kept = decode(&convert_and_resize(&png, ImageFormat::Png, Some(1280), None, true, true).unwrap()).unwrap();
        assert_eq!((kept.width(), kept.height()), (400, 300));
        let enlarged = decode(&convert_and_resize(&png, ImageFormat::Png, Some(800), None, true, false).unwrap()).unwrap();
        assert_eq!((enlarged.width(), enlarged.height()), (800, 600));
        let shrunk = decode(&convert_and_resize(&png, ImageFormat::Png, Some(200), None, true, true).unwrap()).unwrap();
        assert_eq!((shrunk.width(), shrunk.height()), (200, 150));
    }

    #[test]
    fn local_images_lists_each_local_image_once_without_videos_or_remote_ones() {
        let body = "![a](eins.png)\n\n![b](https://example.org/x.png)\n\n![c](clip.mp4)\n\n![d](eins.png)\n\n![e](zwei.jpg)\n";
        let frontmatter = Frontmatter { media: crate::media::reconcile(&[], body), ..Frontmatter::default() };
        assert_eq!(local_images(&frontmatter), vec!["eins.png".to_string(), "zwei.jpg".to_string()]);
    }

    #[test]
    fn is_local_rejects_http_and_https_sources() {
        assert!(is_local("photo.png"));
        assert!(is_local("images/photo.png"));
        assert!(!is_local("https://example.com/photo.png"));
        assert!(!is_local("http://example.com/photo.png"));
    }
}
