//! AI-generated image captions ("Bildunterschrift"): reachable inline from
//! the consolidated Bildbeschriftung dialog (`imagealt.rs`'s
//! `open_dialog_for_index`) via a "generate" suffix button on the caption
//! field - a deliberately separate prompt/module from `aialt.rs`'s alt-text
//! generation, since a caption needs a different prompt (an editorial style
//! with a fixed target length, and - unlike alt text - real article
//! context, not just the image itself).
//!
//! `context` (the text immediately surrounding the image in the article -
//! see `imagealt::surrounding_context`) is folded directly into the vision
//! prompt sent alongside the image, so the caption can describe what the
//! photo is illustrating *in the article*, not just what's visible in
//! isolation.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use gtk4::glib;

use crate::i18n::tr;
use crate::{aitasks, export};

/// The user's own captions typically run 20-25 words - stated explicitly in
/// the prompt rather than left to the model's own judgment of "a caption's
/// usual length".
fn build_prompt(context: &str) -> String {
    let mut prompt = String::from(
        "Du schreibst eine Bildunterschrift (Caption) für ein Bild in einem Blogartikel, im \
         redaktionellen Stil des Artikels - nicht nur eine reine Bildbeschreibung, sondern ein \
         Satz, der das Bild inhaltlich in den Artikel einordnet. Ziel-Länge: 20 bis 25 Wörter. \
         Antworte ausschließlich mit der Bildunterschrift selbst, ohne Anführungszeichen, ohne \
         Erklärung, ohne Zeilenumbrüche.",
    );
    if !context.trim().is_empty() {
        prompt.push_str("\n\nKontext aus dem Artikeltext direkt vor und nach dem Bild:\n");
        prompt.push_str(context.trim());
    }
    prompt
}

/// Runs the vision-based caption generation in a background thread. Calls
/// `on_result` exactly once, back on the GLib main loop, with the trimmed
/// text or an error message. `context` is the surrounding article text
/// (`imagealt::surrounding_context`); passing an empty string is fine,
/// `build_prompt` just omits that part of the request.
pub fn generate(source: &str, context: &str, doc_dir: Option<PathBuf>, on_result: impl Fn(Result<String, String>) + 'static) {
    let source = source.to_string();
    let mime_type = export::mime_from_extension(&source);
    let prompt = build_prompt(context);
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
    fn build_prompt_states_the_target_word_count() {
        let prompt = build_prompt("");
        assert!(prompt.contains("20 bis 25 Wörter"), "{prompt}");
    }

    #[test]
    fn build_prompt_includes_the_surrounding_context_when_given() {
        let prompt = build_prompt("Der Autor testet ein neues Gerät.");
        assert!(prompt.contains("Der Autor testet ein neues Gerät."), "{prompt}");
    }

    #[test]
    fn build_prompt_omits_the_context_section_when_empty() {
        let prompt = build_prompt("");
        assert!(!prompt.contains("Kontext aus dem Artikeltext"), "{prompt}");
    }

    #[test]
    fn build_prompt_omits_the_context_section_when_only_whitespace() {
        let prompt = build_prompt("   \n  ");
        assert!(!prompt.contains("Kontext aus dem Artikeltext"), "{prompt}");
    }
}
