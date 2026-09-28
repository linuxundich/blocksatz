//! "Bewertung" tab: an AI content-quality critique of the whole article -
//! grammar, clarity, readability, wordiness and tone - modeled on the
//! Quill macOS WordPress editor's own "Content Evaluation" panel
//! (`EvaluationPanel.swift`/`AIPromptBuilder.swift` in its source,
//! github.com/cpoteet/Quill), adapted to work directly on this app's plain
//! Markdown buffer instead of Quill's HTML DOM - no HTML-stripping needed
//! first, the buffer already holds nothing but prose.
//!
//! Each finding is clickable: `searchbar::jump_to_text` looks for its
//! "anchor" (3-4 verbatim words the model is asked to copy character-for-
//! character) in the editor buffer, selects it and scrolls it into view -
//! the same "click a finding to jump to it" Quill's own panel offers.
//! Findings whose anchor doesn't actually appear in the text (a model that
//! paraphrased instead of copying) just don't jump - `jump_to_text`
//! reports that back rather than silently selecting the wrong text.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk4::glib;

use crate::document::Frontmatter;
use crate::i18n::tr;
use crate::{aitasks, llm, searchbar};

const SYSTEM_PROMPT: &str = "Du bist ein erfahrener Lektor und bewertest Blogartikel-Entwürfe auf Deutsch - sachlich, konkret und wohlwollend, aber ehrlich.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finding {
    pub quote: String,
    pub anchor: Option<String>,
    pub issue: String,
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvaluationResult {
    pub summary: String,
    pub findings: Vec<Finding>,
}

pub(crate) fn build_prompt(title: &str, body: &str) -> String {
    format!(
        "Analysiere den folgenden Blogartikel-Entwurf auf Grammatik, Klarheit, Lesbarkeit, \
         Weitschweifigkeit und Tonalität/Konsistenz.\n\n\
         Titel: {title}\n\n\
         Text:\n{body}\n\n\
         Antworte exakt in diesem Format:\n\n\
         SUMMARY:\n\
         <2-4 Sätze Fließtext-Kritik zur Gesamtqualität>\n\n\
         FINDINGS:\n\
         QUOTE: \"anzuzeigende Textstelle (max. 15 Wörter, kann ungefähr sein)\" | ANCHOR: \"3-4 Wörter wörtlich\" | ISSUE: kurzes Label | SUGGESTION: Verbesserungsvorschlag (optional)\n\n\
         Regeln:\n\
         - QUOTE wird dem Nutzer angezeigt - kann ungefähr oder paraphrasiert sein, max. 15 Wörter.\n\
         - ANCHOR sind 3-4 zusammenhängende Wörter, wörtlich und zeichengenau aus dem Text oben \
         kopiert - keine Änderungen an Satzzeichen, keine hinzugefügten oder entfernten Zeichen. \
         Wird verwendet, um die Stelle im Editor zu finden, muss exakt passen.\n\
         - ISSUE-Label sollte eines von diesen sein: Grammatik, Klarheit, Lesbarkeit, \
         Weitschweifigkeit, Passiv, Tonalität.\n\
         - SUGGESTION ist optional - Feld ganz weglassen, wenn kein konkreter Vorschlag vorhanden ist.\n\
         - Nur Probleme markieren, die die Qualität wirklich verbessern würden - kleine \
         stilistische Vorlieben überspringen.\n\
         - Bei Weitschweifigkeit: nur wirklich überflüssige Formulierungen markieren, die gekürzt \
         werden könnten, ohne Bedeutung zu verlieren.\n\
         - Die 5-12 wichtigsten Befunde priorisieren.\n\
         - Wenn nichts Nennenswertes zu bemängeln ist, FINDINGS leer lassen."
    )
}

fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    match s.len() {
        len if len > 1 && s.starts_with('"') && s.ends_with('"') => s[1..len - 1].to_string(),
        _ => s.to_string(),
    }
}

/// `part` with a leading `marker` (case-insensitively) removed, `None` if
/// it doesn't start with it - `marker` is always plain ASCII ("QUOTE:" etc.),
/// so slicing by its byte length after an ASCII-case-insensitive prefix
/// match is safe regardless of the rest of `part`'s content.
fn strip_marker<'a>(part: &'a str, marker: &str) -> Option<&'a str> {
    let trimmed = part.trim();
    (trimmed.len() >= marker.len() && trimmed.as_bytes()[..marker.len()].eq_ignore_ascii_case(marker.as_bytes())).then(|| trimmed[marker.len()..].trim())
}

/// Parses the model's reply into an `EvaluationResult` - a direct port of
/// Quill's own `AIPromptBuilder.parseEvaluationResponse`. `None` if the
/// `SUMMARY:`/`FINDINGS:` markers are missing or the summary is empty -
/// the model didn't follow the format at all, not just one malformed
/// finding line (those are simply skipped, see the loop below).
pub(crate) fn parse_response(text: &str) -> Option<EvaluationResult> {
    let summary_start = text.find("SUMMARY:")? + "SUMMARY:".len();
    let after_summary = &text[summary_start..];
    let findings_marker_pos = after_summary.find("FINDINGS:")?;
    let summary = after_summary[..findings_marker_pos].trim().to_string();
    if summary.is_empty() {
        return None;
    }
    let findings_text = after_summary[findings_marker_pos + "FINDINGS:".len()..].trim();

    let mut findings = Vec::new();
    for line in findings_text.lines() {
        let parts: Vec<&str> = line.trim().split(" | ").collect();
        let Some(quote_raw) = parts.first().and_then(|p| strip_marker(p, "QUOTE:")) else { continue };
        let quote = strip_quotes(quote_raw);
        if quote.is_empty() {
            continue;
        }
        let Some(issue) = parts.iter().find_map(|p| strip_marker(p, "ISSUE:")).map(str::to_string).filter(|s| !s.is_empty()) else { continue };
        let anchor = parts.iter().find_map(|p| strip_marker(p, "ANCHOR:")).map(strip_quotes).filter(|s| !s.is_empty());
        let suggestion = parts.iter().find_map(|p| strip_marker(p, "SUGGESTION:")).map(str::to_string).filter(|s| !s.is_empty());
        findings.push(Finding { quote, anchor, issue, suggestion });
    }
    Some(EvaluationResult { summary, findings })
}

pub struct EvaluateView {
    pub widget: gtk4::Widget,
}

impl EvaluateView {
    pub fn new(view: &sourceview5::View, buffer: &sourceview5::Buffer, frontmatter: Rc<RefCell<Frontmatter>>) -> Self {
        let placeholder = adw::StatusPage::builder()
            .icon_name("edit-find-symbolic")
            .title(tr("Noch keine Bewertung"))
            .description(tr("Lässt den Artikel von der KI auf Grammatik, Klarheit, Lesbarkeit, Weitschweifigkeit und Tonalität prüfen."))
            .vexpand(true)
            .build();
        placeholder.add_css_class("compact");

        let evaluate_button = gtk4::Button::with_label(&tr("Bewerten"));
        evaluate_button.add_css_class("suggested-action");
        evaluate_button.add_css_class("pill");
        evaluate_button.set_halign(gtk4::Align::Center);

        let status_label = gtk4::Label::builder().wrap(true).xalign(0.0).build();
        status_label.add_css_class("dim-label");
        status_label.set_visible(false);

        let summary_label = gtk4::Label::builder().wrap(true).xalign(0.0).build();
        summary_label.set_visible(false);

        let findings_list = gtk4::ListBox::new();
        findings_list.set_selection_mode(gtk4::SelectionMode::None);
        findings_list.add_css_class("boxed-list");
        findings_list.set_visible(false);

        let findings_count_label = gtk4::Label::builder().wrap(true).xalign(0.0).build();
        findings_count_label.add_css_class("dim-label");
        findings_count_label.add_css_class("caption");
        findings_count_label.set_visible(false);

        let content = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(12).margin_top(18).margin_bottom(18).margin_start(18).margin_end(18).build();
        content.append(&evaluate_button);
        content.append(&status_label);
        content.append(&placeholder);
        content.append(&summary_label);
        content.append(&findings_count_label);
        content.append(&findings_list);
        let widget = gtk4::ScrolledWindow::builder().child(&content).hscrollbar_policy(gtk4::PolicyType::Never).vexpand(true).build();

        {
            let view = view.clone();
            let buffer = buffer.clone();
            let evaluate_button_for_click = evaluate_button.clone();
            let status_label = status_label.clone();
            let placeholder = placeholder.clone();
            let summary_label = summary_label.clone();
            let findings_count_label = findings_count_label.clone();
            let findings_list = findings_list.clone();
            evaluate_button.connect_clicked(move |_| {
                let title = frontmatter.borrow().title.clone();
                let body = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
                if body.split_whitespace().count() < 40 {
                    status_label.set_label(&tr("Der Artikel ist noch zu kurz für eine sinnvolle Bewertung."));
                    status_label.set_visible(true);
                    return;
                }

                evaluate_button_for_click.set_sensitive(false);
                status_label.set_label(&tr("Wird bewertet …"));
                status_label.set_visible(true);
                placeholder.set_visible(false);
                summary_label.set_visible(false);
                findings_count_label.set_visible(false);
                findings_list.set_visible(false);

                let (tx, rx) = mpsc::channel::<Result<aitasks::Routed<String>, String>>();
                std::thread::spawn(move || {
                    let message = [llm::ChatMessage { role: llm::Role::User, text: build_prompt(&title, &body) }];
                    let _ = tx.send(aitasks::run(aitasks::AiTask::TextEditing, |client| client.send(SYSTEM_PROMPT, &message)));
                });

                let view = view.clone();
                let buffer = buffer.clone();
                let evaluate_button = evaluate_button_for_click.clone();
                let status_label = status_label.clone();
                let placeholder = placeholder.clone();
                let summary_label = summary_label.clone();
                let findings_count_label = findings_count_label.clone();
                let findings_list = findings_list.clone();
                glib::timeout_add_local(Duration::from_millis(150), move || {
                    let outcome = match rx.try_recv() {
                        Ok(outcome) => aitasks::deliver(outcome),
                        Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                        Err(mpsc::TryRecvError::Disconnected) => Err(tr("Interner Fehler: Bewertung hat kein Ergebnis geliefert.")),
                    };
                    evaluate_button.set_sensitive(true);
                    evaluate_button.set_label(&tr("Neu bewerten"));
                    match outcome.as_deref().map(parse_response) {
                        Ok(Some(result)) => {
                            status_label.set_visible(false);
                            show_result(&view, &buffer, &summary_label, &findings_count_label, &findings_list, &result);
                        }
                        Ok(None) => {
                            status_label.set_label(&tr("Die KI hat kein auswertbares Ergebnis geliefert. Bitte erneut versuchen."));
                            status_label.set_visible(true);
                            placeholder.set_visible(true);
                        }
                        Err(err) => {
                            status_label.set_label(&tr("Fehler: {err}").replace("{err}", err));
                            status_label.set_visible(true);
                            placeholder.set_visible(true);
                        }
                    }
                    glib::ControlFlow::Break
                });
            });
        }

        Self { widget: widget.upcast() }
    }
}

/// Rebuilds `findings_list`'s rows from `result` - each activatable, its
/// title the issue label, its subtitle the quoted text (italic) and, if
/// the model gave one, the suggested rewrite on its own line below.
fn show_result(view: &sourceview5::View, buffer: &sourceview5::Buffer, summary_label: &gtk4::Label, findings_count_label: &gtk4::Label, findings_list: &gtk4::ListBox, result: &EvaluationResult) {
    summary_label.set_label(&result.summary);
    summary_label.set_visible(true);

    findings_count_label.set_label(&match result.findings.len() {
        0 => tr("Keine konkreten Befunde."),
        1 => tr("1 Befund - zum Aufrufen anklicken."),
        n => tr("{n} Befunde - zum Aufrufen anklicken.").replace("{n}", &n.to_string()),
    });
    findings_count_label.set_visible(true);

    while let Some(child) = findings_list.first_child() {
        findings_list.remove(&child);
    }
    for finding in &result.findings {
        let mut subtitle = format!("<i>„{}“</i>", glib::markup_escape_text(&finding.quote));
        if let Some(suggestion) = &finding.suggestion {
            subtitle.push_str(&format!("\n→ {}", glib::markup_escape_text(suggestion)));
        }
        let row = adw::ActionRow::builder().title(glib::markup_escape_text(&finding.issue).to_string()).subtitle(subtitle).use_markup(true).activatable(true).build();
        row.set_subtitle_lines(3);
        {
            let view = view.clone();
            let buffer = buffer.clone();
            let needle = finding.anchor.clone().unwrap_or_else(|| finding.quote.clone());
            row.connect_activated(move |_| {
                searchbar::jump_to_text(&view, &buffer, &needle);
            });
        }
        findings_list.append(&row);
    }
    findings_list.set_visible(!result.findings.is_empty());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_prompt_includes_title_and_body() {
        let prompt = build_prompt("Mein Titel", "Der Artikeltext.");
        assert!(prompt.contains("Mein Titel"));
        assert!(prompt.contains("Der Artikeltext."));
        assert!(prompt.contains("SUMMARY:"));
        assert!(prompt.contains("FINDINGS:"));
    }

    #[test]
    fn parse_response_is_none_without_the_markers() {
        assert_eq!(parse_response("Irgendein Text ohne Format."), None);
    }

    #[test]
    fn parse_response_is_none_with_an_empty_summary() {
        assert_eq!(parse_response("SUMMARY:\n\nFINDINGS:\nQUOTE: \"x\" | ISSUE: Klarheit"), None);
    }

    #[test]
    fn parse_response_reads_the_summary_and_a_full_finding() {
        let text = "SUMMARY:\nDer Artikel ist insgesamt gut, aber stellenweise weitschweifig.\n\n\
                     FINDINGS:\n\
                     QUOTE: \"in Anbetracht der Tatsache, dass\" | ANCHOR: \"in Anbetracht der\" | ISSUE: Weitschweifigkeit | SUGGESTION: \"weil\"";
        let result = parse_response(text).unwrap();
        assert_eq!(result.summary, "Der Artikel ist insgesamt gut, aber stellenweise weitschweifig.");
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].quote, "in Anbetracht der Tatsache, dass");
        assert_eq!(result.findings[0].anchor.as_deref(), Some("in Anbetracht der"));
        assert_eq!(result.findings[0].issue, "Weitschweifigkeit");
        assert_eq!(result.findings[0].suggestion.as_deref(), Some("\"weil\""));
    }

    #[test]
    fn parse_response_omits_the_optional_suggestion() {
        let text = "SUMMARY:\nKurze Kritik.\n\nFINDINGS:\nQUOTE: \"Text\" | ISSUE: Klarheit";
        let result = parse_response(text).unwrap();
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].suggestion, None);
        assert_eq!(result.findings[0].anchor, None);
    }

    #[test]
    fn parse_response_skips_a_finding_line_missing_the_required_fields() {
        let text = "SUMMARY:\nKritik.\n\nFINDINGS:\nQUOTE: \"Text ohne Issue-Feld\"\nQUOTE: \"Text\" | ISSUE: Klarheit";
        let result = parse_response(text).unwrap();
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].quote, "Text");
    }

    #[test]
    fn parse_response_matches_markers_case_insensitively_per_field() {
        let text = "SUMMARY:\nKritik.\n\nFINDINGS:\nquote: \"Text\" | issue: Klarheit | anchor: \"a b c\"";
        let result = parse_response(text).unwrap();
        assert_eq!(result.findings[0].quote, "Text");
        assert_eq!(result.findings[0].issue, "Klarheit");
        assert_eq!(result.findings[0].anchor.as_deref(), Some("a b c"));
    }

    #[test]
    fn parse_response_ignores_findings_with_no_findings_at_all() {
        let text = "SUMMARY:\nAlles gut, keine Einwände.\n\nFINDINGS:\n";
        let result = parse_response(text).unwrap();
        assert_eq!(result.findings.len(), 0);
    }
}
