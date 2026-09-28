//! AI-generated alt text: right-click an image - its `![alt](src)` line in
//! the editor (see `imagealt.rs`) or the rendered image itself in the
//! Vorschau pane (see `preview.rs`) - to have the active KI-Chat provider
//! look at the real image bytes and propose an accessible alt text, at one
//! of three levels of detail. Also reachable for the featured image from
//! Artikel-Eigenschaften (`properties.rs`), which isn't a `MediaItem` at
//! all. The result is shown for review/correction before anything is
//! written; applying it just calls the caller's own `on_apply` (see
//! `open`'s doc comment) - so it's the caller's job to decide where that
//! text actually goes (`MediaItem.alt` for a body image,
//! `Frontmatter.featured_image_alt` for the featured one), not this
//! module's.
//!
//! Deliberately no attempt to hide this behind "is an LLM actually
//! configured" - matching `imagealt.rs`'s own choice to always show its
//! menu item and explain rather than silently do nothing: a missing/invalid
//! API key surfaces as a normal inline error from the same generation call
//! chat.rs already makes, not as a pre-check.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk4::glib;

use crate::i18n::tr;
use crate::{aitasks, export};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailLevel {
    Standard,
    Detailed,
    Precise,
}

impl DetailLevel {
    const ALL: [DetailLevel; 3] = [DetailLevel::Standard, DetailLevel::Detailed, DetailLevel::Precise];

    fn id(&self) -> &'static str {
        match self {
            DetailLevel::Standard => "standard",
            DetailLevel::Detailed => "detailed",
            DetailLevel::Precise => "precise",
        }
    }

    fn from_id(s: &str) -> Self {
        match s.trim() {
            "detailed" => DetailLevel::Detailed,
            "precise" => DetailLevel::Precise,
            _ => DetailLevel::Standard,
        }
    }

    fn label(&self) -> String {
        match self {
            DetailLevel::Standard => tr("Standard (kurz & bündig)"),
            DetailLevel::Detailed => tr("Ausführlich"),
            DetailLevel::Precise => tr("Hohe Genauigkeit"),
        }
    }

    fn prompt(&self) -> &'static str {
        match self {
            DetailLevel::Standard => {
                "Du erstellst einen barrierefreien Alternativtext (alt-Text) für ein Bild in einem \
                 Blogartikel. Beschreibe in maximal einem kurzen Satz (ca. 8-12 Wörter), was auf dem \
                 Bild zu sehen ist, sodass eine Person mit Bildschirmleser den Inhalt versteht. Keine \
                 Einleitung wie \"Bild von\" oder \"Ein Foto zeigt\", keine Anführungszeichen, keine \
                 Interpretation - nur der reine Alternativtext."
            }
            DetailLevel::Detailed => {
                "Du erstellst einen barrierefreien Alternativtext (alt-Text) für ein Bild in einem \
                 Blogartikel. Beschreibe in 2-3 vollständigen Sätzen, was auf dem Bild zu sehen ist - \
                 Hauptmotiv, relevanter Kontext und Umgebung, soweit erkennbar - sodass sich eine \
                 Person mit Bildschirmleser ein klares Bild machen kann. Keine Einleitung wie \"Bild \
                 von\" oder \"Ein Foto zeigt\", keine Anführungszeichen, keine Spekulation über nicht \
                 erkennbare Dinge - nur der reine Alternativtext."
            }
            DetailLevel::Precise => {
                "Du erstellst einen barrierefreien Alternativtext (alt-Text) für ein Bild in einem \
                 Blogartikel. Beschreibe möglichst genau und sachlich alle relevanten visuellen \
                 Details: Objekte, ihre Anordnung, Farben, sichtbaren Text (wortwörtlich, falls \
                 vorhanden), sowie Zahlen/Werte, falls es sich um eine Grafik oder ein Diagramm \
                 handelt. Beschreibe Personen nur anhand äußerlich erkennbarer Merkmale, ohne \
                 Vermutungen zu Identität anzustellen. Keine Einleitung wie \"Bild von\" oder \"Ein \
                 Foto zeigt\", keine Anführungszeichen - nur der reine Alternativtext."
            }
        }
    }
}

fn config_dir() -> PathBuf {
    let mut dir = glib::user_config_dir();
    dir.push("blocksmith");
    dir
}

fn detail_level_path() -> PathBuf {
    let mut path = config_dir();
    path.push("ai_alt_text_detail_level.txt");
    path
}

/// The detail level picked last time this dialog was used, for any image -
/// deliberately global rather than per-image, matching how a user tends to
/// settle on one preferred level for the whole article/site rather than
/// re-deciding per picture.
fn load_detail_level() -> DetailLevel {
    std::fs::read_to_string(detail_level_path()).map(|s| DetailLevel::from_id(&s)).unwrap_or(DetailLevel::Standard)
}

fn save_detail_level(level: DetailLevel) {
    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::write(detail_level_path(), level.id());
}

/// Opens the review dialog for one image, identified only by `title`
/// (shown as the dialog's group heading - a filename for a body image, or
/// a fixed label for the featured image, which has no filename of its
/// own) and `source` (resolved against `doc_dir` the same way any other
/// local image reference is). Applying the result just calls `on_apply`
/// with the trimmed text - deliberately not tied to `MediaItem`/
/// `Frontmatter.media` at all, so the same dialog serves both a body
/// image (`imagealt.rs`/`preview.rs`, writing into `frontmatter.media
/// [index].alt`) and the featured image (`properties.rs`, writing into
/// `Frontmatter.featured_image_alt`), which isn't a `MediaItem` and has
/// no index into that list.
pub fn open(window: &gtk4::Window, title: String, source: String, doc_dir: Option<PathBuf>, on_apply: impl Fn(String) + 'static) {
    let level_labels: Vec<String> = DetailLevel::ALL.iter().map(DetailLevel::label).collect();
    let level_label_refs: Vec<&str> = level_labels.iter().map(String::as_str).collect();
    let selected_level = DetailLevel::ALL.iter().position(|l| *l == load_detail_level()).unwrap_or(0) as u32;
    let level_row = adw::ComboRow::builder()
        .title(tr("Detailgrad"))
        .model(&gtk4::StringList::new(&level_label_refs))
        .selected(selected_level)
        .build();
    level_row.connect_selected_notify(|row| {
        save_detail_level(DetailLevel::ALL[row.selected() as usize]);
    });

    let generate_button = gtk4::Button::with_label(&tr("Text generieren"));
    generate_button.add_css_class("suggested-action");

    let status_label = gtk4::Label::builder().wrap(true).xalign(0.0).build();
    status_label.set_visible(false);

    let text_view = gtk4::TextView::builder()
        .wrap_mode(gtk4::WrapMode::WordChar)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(8)
        .right_margin(8)
        .build();
    let text_buffer = text_view.buffer();
    text_buffer.set_text(&tr("Noch kein Text generiert - auf „Text generieren“ klicken."));

    let frame = gtk4::Frame::new(None);
    frame.set_child(Some(&text_view));
    let text_scroller = gtk4::ScrolledWindow::builder().child(&frame).min_content_height(120).vexpand(true).build();

    let generate_row_box = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).spacing(8).build();
    generate_row_box.append(&generate_button);

    let group = adw::PreferencesGroup::builder().title(&title).build();
    group.add(&level_row);

    let content = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(12)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    content.append(&group);
    content.append(&generate_row_box);
    content.append(&status_label);
    content.append(&text_scroller);

    let apply_button = gtk4::Button::with_label(&tr("Übernehmen"));
    apply_button.add_css_class("suggested-action");

    let header = adw::HeaderBar::new();
    header.pack_end(&apply_button);

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&content));

    let dialog = adw::Dialog::builder().title(tr("KI-Alternativtext")).content_width(460).content_height(420).child(&toolbar_view).build();

    {
        let source = source.clone();
        let doc_dir = doc_dir.clone();
        let generate_button_for_click = generate_button.clone();
        let status_label = status_label.clone();
        let text_buffer = text_buffer.clone();
        generate_button.connect_clicked(move |_| {
            generate_button_for_click.set_sensitive(false);
            status_label.set_label(&tr("Wird generiert …"));
            status_label.set_visible(true);
            let generate_button_for_click = generate_button_for_click.clone();
            let status_label = status_label.clone();
            let text_buffer = text_buffer.clone();
            generate(&source, doc_dir.clone(), move |result| {
                match result {
                    Ok(text) => {
                        text_buffer.set_text(&text);
                        status_label.set_visible(false);
                    }
                    Err(err) => status_label.set_label(&tr("Fehler: {err}").replace("{err}", &err)),
                }
                generate_button_for_click.set_sensitive(true);
            });
        });
    }

    {
        let text_buffer = text_buffer.clone();
        let dialog = dialog.clone();
        apply_button.connect_clicked(move |_| {
            let text = text_buffer.text(&text_buffer.start_iter(), &text_buffer.end_iter(), false).to_string();
            on_apply(text.trim().to_string());
            dialog.close();
        });
    }

    dialog.present(Some(window));
}

/// Runs the vision-based alt-text generation in a background thread, using
/// whichever `DetailLevel` was last selected in this module's own dialog
/// (see `load_detail_level`) - so a caller that wants a specific level
/// should go through `open` instead, which lets the user pick one. Calls
/// `on_result` exactly once, back on the GLib main loop, with the trimmed
/// text or an error message. This is the reusable core both `open` (the
/// standalone review dialog, still used for the featured image by
/// `properties.rs`) and the consolidated Bildbeschriftung dialog
/// (`imagealt.rs`, for a body image) build their own UI feedback around,
/// instead of each re-implementing the same background-thread-plus-polling
/// plumbing.
pub fn generate(source: &str, doc_dir: Option<PathBuf>, on_result: impl Fn(Result<String, String>) + 'static) {
    let level = load_detail_level();
    let source = source.to_string();
    let mime_type = export::mime_from_extension(&source);
    let prompt = level.prompt().to_string();
    let (tx, rx) = mpsc::channel::<Result<aitasks::Routed<String>, String>>();
    std::thread::spawn(move || {
        let outcome = export::read_image_bytes(&source, doc_dir.as_deref())
            .and_then(|bytes| aitasks::run(aitasks::AiTask::ImageCaptioning, |client| client.describe_image(&prompt, &bytes, mime_type)));
        let _ = tx.send(outcome);
    });

    glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv().map(aitasks::deliver) {
        Ok(Ok(text)) => {
            on_result(Ok(text.trim().to_string()));
            glib::ControlFlow::Break
        }
        Ok(Err(err)) => {
            on_result(Err(err));
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            on_result(Err(tr("Interner Fehler: Generierungs-Thread hat kein Ergebnis geliefert.")));
            glib::ControlFlow::Break
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_detail_level_has_a_distinct_prompt() {
        let prompts: Vec<&str> = DetailLevel::ALL.iter().map(DetailLevel::prompt).collect();
        assert_ne!(prompts[0], prompts[1]);
        assert_ne!(prompts[1], prompts[2]);
        assert_ne!(prompts[0], prompts[2]);
    }

    #[test]
    fn every_detail_level_has_a_non_empty_label() {
        for level in DetailLevel::ALL {
            assert!(!level.label().is_empty());
        }
    }

    #[test]
    fn every_detail_level_round_trips_through_its_id() {
        for level in DetailLevel::ALL {
            assert_eq!(DetailLevel::from_id(level.id()), level);
        }
    }

    #[test]
    fn from_id_falls_back_to_standard_for_garbage() {
        assert_eq!(DetailLevel::from_id("not-a-level"), DetailLevel::Standard);
        assert_eq!(DetailLevel::from_id(""), DetailLevel::Standard);
    }
}
