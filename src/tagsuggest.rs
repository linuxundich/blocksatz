//! AI-suggested tags: analyzes the article's title and body - plus the
//! site's already-known tags, so a suggestion prefers reusing one of those
//! rather than fragmenting the taxonomy with a near-duplicate - and
//! proposes a handful of relevant ones for review (each a checkable row,
//! all pre-checked) before any of them are actually added. Reachable from
//! a button next to the "Tags" field in Artikel-Eigenschaften
//! (`properties.rs`), which owns merging the checked suggestions into its
//! own comma-separated tags field - this module only ever hands back which
//! ones were checked, the same "review before anything is written"
//! division of responsibility `aialt.rs` already uses for alt text.
//!
//! Deliberately no attempt to hide this behind "is an LLM actually
//! configured", matching `aialt.rs`'s own choice - a missing/invalid API
//! key surfaces as a normal inline error from the generation call itself.

use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk4::glib;

use crate::document;
use crate::i18n::tr;
use crate::{aitasks, llm, termcache};

const SYSTEM_PROMPT: &str = "Du schlägst passende Tags (Schlagwörter) für einen Blogartikel vor. \
     Analysiere Titel und Text und nenne 5 bis 8 treffende Tags. Bereits existierende Tags der \
     Website werden mitgeliefert - verwende einen davon, wenn er inhaltlich passt, statt ein \
     bedeutungsgleiches neues Tag zu erfinden; schlage neue Tags nur vor, wenn wirklich nichts \
     Passendes existiert. Antworte ausschließlich mit den Tags selbst, kommagetrennt, in einer \
     einzigen Zeile - keine Nummerierung, keine Erklärung, keine Anführungszeichen, kein Text davor \
     oder danach.";

/// Opens the review dialog. `existing_tags` is the site's known tag list
/// (for the prompt's "prefer reusing one of these" instruction);
/// `current_tags` is this article's own tags already set, both for the
/// same "prefer existing" instruction and to filter them back out of
/// whatever comes back (suggesting a tag already applied is never useful).
/// `on_apply` is called once with whichever suggestions were still checked
/// when "Übernehmen" was clicked - never with an empty `Vec` (that button
/// is insensitive until at least one is checked).
pub fn open(window: &gtk4::Window, article_title: String, body: String, existing_tags: Vec<String>, current_tags: Vec<String>, on_apply: impl Fn(Vec<String>) + 'static) {
    let generate_button = gtk4::Button::with_label(&tr("Vorschläge generieren"));
    generate_button.add_css_class("suggested-action");

    let status_label = gtk4::Label::builder().wrap(true).xalign(0.0).build();
    status_label.set_visible(false);

    let suggestions_list = gtk4::ListBox::new();
    suggestions_list.set_selection_mode(gtk4::SelectionMode::None);
    suggestions_list.add_css_class("boxed-list");
    let suggestions_scroller = gtk4::ScrolledWindow::builder().child(&suggestions_list).min_content_height(160).vexpand(true).build();

    let placeholder_label = gtk4::Label::builder()
        .label(tr("Noch keine Vorschläge - auf „Vorschläge generieren“ klicken."))
        .wrap(true)
        .xalign(0.0)
        .build();
    placeholder_label.add_css_class("dim-label");
    suggestions_list.set_placeholder(Some(&placeholder_label));

    let content = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(12)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    content.append(&generate_button);
    content.append(&status_label);
    content.append(&suggestions_scroller);

    let apply_button = gtk4::Button::with_label(&tr("Übernehmen"));
    apply_button.add_css_class("suggested-action");
    apply_button.set_sensitive(false);

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&gtk4::Label::new(Some(&tr("KI-Tags vorschlagen")))));
    header.pack_end(&apply_button);

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&content));

    let dialog = adw::Dialog::builder().title(tr("KI-Tags vorschlagen")).content_width(420).content_height(420).child(&toolbar_view).build();

    {
        let generate_button_for_click = generate_button.clone();
        let status_label = status_label.clone();
        let suggestions_list = suggestions_list.clone();
        let apply_button = apply_button.clone();
        let article_title = article_title.clone();
        let body = body.clone();
        let existing_tags = existing_tags.clone();
        let current_tags = current_tags.clone();
        generate_button.connect_clicked(move |_| {
            run_generation(
                &article_title,
                &body,
                &existing_tags,
                &current_tags,
                &generate_button_for_click,
                &status_label,
                &suggestions_list,
                &apply_button,
            );
        });
    }

    {
        let suggestions_list = suggestions_list.clone();
        let dialog = dialog.clone();
        apply_button.connect_clicked(move |_| {
            let checked = checked_suggestions(&suggestions_list);
            if !checked.is_empty() {
                on_apply(checked);
            }
            dialog.close();
        });
    }

    dialog.present(Some(window));
}

/// Every row `populate_suggestions` adds is a `Gtk.CheckButton` directly
/// inside the `ListBox` (auto-wrapped in a plain `Gtk.ListBoxRow` by GTK),
/// its `child` holding a colored `Gtk.Label` (see `populate_suggestions`) -
/// `Gtk.Label::text()` strips that label's Pango markup back down to the
/// plain tag text, so reading it back this way still avoids needing a
/// parallel `Vec<(String, CheckButton)>` just to know which ones ended up
/// checked.
fn checked_suggestions(list: &gtk4::ListBox) -> Vec<String> {
    let mut checked = Vec::new();
    let mut child = list.first_child();
    while let Some(row) = child {
        if let Some(check) = row.first_child().and_then(|w| w.downcast::<gtk4::CheckButton>().ok()) {
            if check.is_active() {
                if let Some(label) = check.child().and_then(|w| w.downcast::<gtk4::Label>().ok()) {
                    checked.push(label.text().to_string());
                }
            }
        }
        child = row.next_sibling();
    }
    checked
}

/// `existing_tags` colors each row green if the model suggested a tag the
/// site already has (`termcache::term_markup`) or red if accepting it
/// would create a brand new WordPress tag - the same distinction
/// `properties.rs`'s own tags-field status line shows, so it's visible
/// right here too, before a suggestion is even applied.
fn populate_suggestions(list: &gtk4::ListBox, tags: &[String], existing_tags: &[String]) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    for tag in tags {
        let label = gtk4::Label::builder().use_markup(true).label(termcache::term_markup(tag, existing_tags)).xalign(0.0).build();
        let check = gtk4::CheckButton::builder().active(true).child(&label).build();
        list.append(&check);
    }
}

#[allow(clippy::too_many_arguments)]
fn run_generation(
    article_title: &str,
    body: &str,
    existing_tags: &[String],
    current_tags: &[String],
    generate_button: &gtk4::Button,
    status_label: &gtk4::Label,
    suggestions_list: &gtk4::ListBox,
    apply_button: &gtk4::Button,
) {
    generate_button.set_sensitive(false);
    apply_button.set_sensitive(false);
    status_label.set_label(&tr("Wird generiert …"));
    status_label.set_visible(true);

    let prompt = build_prompt(article_title, body, existing_tags);
    let current_tags = current_tags.to_vec();
    let existing_tags = existing_tags.to_vec();

    let (tx, rx) = mpsc::channel::<Result<aitasks::Routed<String>, String>>();
    std::thread::spawn(move || {
        let message = [llm::ChatMessage { role: llm::Role::User, text: prompt }];
        let _ = tx.send(aitasks::run(aitasks::AiTask::TextEditing, |client| client.send(SYSTEM_PROMPT, &message)));
    });

    let generate_button = generate_button.clone();
    let status_label = status_label.clone();
    let suggestions_list = suggestions_list.clone();
    let apply_button = apply_button.clone();
    glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv().map(aitasks::deliver) {
        Ok(Ok(text)) => {
            let suggestions = parse_suggestions(&text, &current_tags);
            if suggestions.is_empty() {
                status_label.set_label(&tr("Keine neuen Tag-Vorschläge gefunden."));
            } else {
                status_label.set_visible(false);
            }
            populate_suggestions(&suggestions_list, &suggestions, &existing_tags);
            apply_button.set_sensitive(!suggestions.is_empty());
            generate_button.set_sensitive(true);
            glib::ControlFlow::Break
        }
        Ok(Err(err)) => {
            status_label.set_label(&tr("Fehler: {err}").replace("{err}", &err));
            generate_button.set_sensitive(true);
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            status_label.set_label(&tr("Interner Fehler: Generierungs-Thread hat kein Ergebnis geliefert."));
            generate_button.set_sensitive(true);
            glib::ControlFlow::Break
        }
    });
}

/// The article body is truncated to a generous character budget - long
/// enough that a typical blog post fits whole, short enough to stay well
/// within any provider's context window without needing per-provider
/// token accounting (the same pragmatic tradeoff `aialt.rs` doesn't even
/// need, since an image is bounded by its own byte size already).
const MAX_BODY_CHARS: usize = 6000;

fn build_prompt(article_title: &str, body: &str, existing_tags: &[String]) -> String {
    let truncated_body: String = body.chars().take(MAX_BODY_CHARS).collect();
    let mut prompt = String::new();
    if !article_title.is_empty() {
        prompt.push_str(&format!("Titel: {article_title}\n\n"));
    }
    if !existing_tags.is_empty() {
        prompt.push_str(&format!("Bereits existierende Tags dieser Website: {}\n\n", existing_tags.join(", ")));
    }
    prompt.push_str("Artikeltext:\n");
    prompt.push_str(&truncated_body);
    prompt
}

/// Parses the model's comma-separated response (`document::parse_list`
/// already handles trimming/unquoting/empty-filtering) and drops anything
/// that case-insensitively matches a tag already on the article -
/// suggesting one already applied is never useful.
fn parse_suggestions(response: &str, current_tags: &[String]) -> Vec<String> {
    let current_lower: Vec<String> = current_tags.iter().map(|t| t.to_lowercase()).collect();
    document::parse_list(response).into_iter().filter(|tag| !current_lower.contains(&tag.to_lowercase())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_suggestions_splits_on_commas() {
        let suggestions = parse_suggestions("GNU/Linux, Terminal, Tipps", &[]);
        assert_eq!(suggestions, vec!["GNU/Linux", "Terminal", "Tipps"]);
    }

    #[test]
    fn parse_suggestions_drops_tags_already_on_the_article_case_insensitively() {
        let suggestions = parse_suggestions("GNU/Linux, Terminal, tipps", &["Tipps".to_string()]);
        assert_eq!(suggestions, vec!["GNU/Linux", "Terminal"]);
    }

    #[test]
    fn parse_suggestions_ignores_empty_entries() {
        let suggestions = parse_suggestions("GNU/Linux, , Terminal", &[]);
        assert_eq!(suggestions, vec!["GNU/Linux", "Terminal"]);
    }

    #[test]
    fn build_prompt_includes_title_existing_tags_and_body() {
        let prompt = build_prompt("Mein Titel", "Der Artikeltext.", &["Terminal".to_string()]);
        assert!(prompt.contains("Mein Titel"));
        assert!(prompt.contains("Terminal"));
        assert!(prompt.contains("Der Artikeltext."));
    }

    #[test]
    fn build_prompt_truncates_a_very_long_body() {
        let long_body = "a".repeat(MAX_BODY_CHARS * 2);
        let prompt = build_prompt("", &long_body, &[]);
        assert!(prompt.len() < long_body.len());
    }
}
