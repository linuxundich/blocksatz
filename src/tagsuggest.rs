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

use std::cell::RefCell;
use std::rc::Rc;
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
/// is insensitive while nothing is checked).
///
/// Nothing is generated on open - every run costs tokens, so it only
/// happens on an explicit click.
pub fn open(window: &gtk4::Window, article_title: String, body: String, existing_tags: Vec<String>, current_tags: Vec<String>, on_apply: impl Fn(Vec<String>) + 'static) {
    let stack = gtk4::Stack::builder().transition_type(gtk4::StackTransitionType::Crossfade).vexpand(true).build();

    // Start page: what this does, and the one button that does it.
    let generate_button = pill_button(&tr("Vorschläge generieren"));
    let start_page = adw::StatusPage::builder()
        .icon_name("user-bookmarks-symbolic")
        .title(tr("Tags vorschlagen lassen"))
        .description(tr("Die KI liest Titel und Text und schlägt passende Tags vor. Vorhandene Tags der Website haben Vorrang."))
        .child(&generate_button)
        .build();
    start_page.add_css_class("compact");
    stack.add_named(&start_page, Some("start"));

    let spinner = adw::SpinnerPaintable::new(None::<&gtk4::Widget>);
    let loading_page = adw::StatusPage::builder().paintable(&spinner).title(tr("Wird generiert …")).build();
    // Without a widget the paintable never animates.
    spinner.set_widget(Some(&loading_page));
    loading_page.add_css_class("compact");
    stack.add_named(&loading_page, Some("loading"));

    // Error and "nothing new" share one page; only icon, title and
    // description change.
    let retry_button = pill_button(&tr("Erneut versuchen"));
    let message_page = adw::StatusPage::builder().child(&retry_button).build();
    message_page.add_css_class("compact");
    stack.add_named(&message_page, Some("message"));

    let regenerate_button = gtk4::Button::builder().icon_name("view-refresh-symbolic").tooltip_text(tr("Neu generieren")).valign(gtk4::Align::Center).build();
    regenerate_button.add_css_class("flat");
    let suggestions_group = adw::PreferencesGroup::builder()
        .title(tr("Vorschläge"))
        .description(tr("Grün: Tag gibt es schon. Rot: wird neu angelegt."))
        .header_suffix(&regenerate_button)
        .build();
    let results_page = adw::PreferencesPage::new();
    results_page.add(&suggestions_group);
    stack.add_named(&results_page, Some("results"));

    let cancel_button = gtk4::Button::with_label(&tr("Abbrechen"));
    let apply_button = gtk4::Button::with_label(&tr("Übernehmen"));
    apply_button.add_css_class("suggested-action");
    apply_button.set_sensitive(false);

    let header = adw::HeaderBar::builder().show_start_title_buttons(false).show_end_title_buttons(false).build();
    header.pack_start(&cancel_button);
    header.pack_end(&apply_button);

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&stack));

    let dialog = adw::Dialog::builder().title(tr("KI-Tags vorschlagen")).content_width(480).content_height(520).child(&toolbar_view).build();

    let ui = Rc::new(Ui { stack, message_page, suggestions_group, apply_button: apply_button.clone(), rows: RefCell::new(Vec::new()) });
    let request = Rc::new(Request { article_title, body, existing_tags, current_tags });
    // Handlers hold `ui` weakly (its widgets own them); the dialog keeps
    // the one strong reference until it closes.
    for button in [&generate_button, &retry_button, &regenerate_button] {
        let ui = Rc::downgrade(&ui);
        let request = request.clone();
        button.connect_clicked(move |_| {
            if let Some(ui) = ui.upgrade() {
                run_generation(&request, &ui);
            }
        });
    }
    {
        let keep_alive = RefCell::new(Some(ui.clone()));
        dialog.connect_closed(move |_| {
            keep_alive.take();
        });
    }

    {
        let dialog = dialog.clone();
        cancel_button.connect_clicked(move |_| {
            dialog.close();
        });
    }
    {
        let ui = Rc::downgrade(&ui);
        let dialog = dialog.clone();
        apply_button.connect_clicked(move |_| {
            let checked = ui.upgrade().map(|ui| ui.checked_suggestions()).unwrap_or_default();
            if !checked.is_empty() {
                on_apply(checked);
            }
            dialog.close();
        });
    }

    dialog.set_default_widget(Some(&generate_button));
    dialog.present(Some(window));
    generate_button.grab_focus();
}

fn pill_button(label: &str) -> gtk4::Button {
    let button = gtk4::Button::builder().label(label).halign(gtk4::Align::Center).build();
    button.add_css_class("suggested-action");
    button.add_css_class("pill");
    button
}

struct Request {
    article_title: String,
    body: String,
    existing_tags: Vec<String>,
    current_tags: Vec<String>,
}

struct Ui {
    stack: gtk4::Stack,
    message_page: adw::StatusPage,
    suggestions_group: adw::PreferencesGroup,
    apply_button: gtk4::Button,
    /// One entry per suggestion row currently in `suggestions_group`.
    rows: RefCell<Vec<(String, adw::ActionRow, gtk4::CheckButton)>>,
}

impl Ui {
    fn checked_suggestions(&self) -> Vec<String> {
        self.rows.borrow().iter().filter(|(_, _, check)| check.is_active()).map(|(tag, _, _)| tag.clone()).collect()
    }

    fn update_apply_sensitivity(&self) {
        self.apply_button.set_sensitive(self.rows.borrow().iter().any(|(_, _, check)| check.is_active()));
    }

    fn show_message(&self, icon: &str, title: &str, description: &str) {
        self.message_page.set_icon_name(Some(icon));
        self.message_page.set_title(title);
        self.message_page.set_description(Some(description));
        self.stack.set_visible_child_name("message");
    }

    /// `existing_tags` colors each row's title green if the model suggested
    /// a tag the site already has (`termcache::term_markup`) or red if
    /// accepting it would create a brand new WordPress tag - the same
    /// distinction `properties.rs`'s own tags-field status line shows, so
    /// it's visible right here too, before a suggestion is even applied.
    fn populate(self: &Rc<Self>, tags: &[String], existing_tags: &[String]) {
        for (_, row, _) in self.rows.borrow_mut().drain(..) {
            self.suggestions_group.remove(&row);
        }
        for tag in tags {
            let check = gtk4::CheckButton::builder().active(true).valign(gtk4::Align::Center).build();
            let row = adw::ActionRow::builder().title(termcache::term_markup(tag, existing_tags)).activatable_widget(&check).build();
            row.add_prefix(&check);
            let ui = Rc::downgrade(self);
            check.connect_toggled(move |_| {
                if let Some(ui) = ui.upgrade() {
                    ui.update_apply_sensitivity();
                }
            });
            self.suggestions_group.add(&row);
            self.rows.borrow_mut().push((tag.clone(), row, check));
        }
        self.update_apply_sensitivity();
    }
}

fn run_generation(request: &Request, ui: &Rc<Ui>) {
    ui.apply_button.set_sensitive(false);
    ui.stack.set_visible_child_name("loading");

    let prompt = build_prompt(&request.article_title, &request.body, &request.existing_tags);
    let current_tags = request.current_tags.clone();
    let existing_tags = request.existing_tags.clone();

    let (tx, rx) = mpsc::channel::<Result<aitasks::Routed<String>, String>>();
    std::thread::spawn(move || {
        let message = [llm::ChatMessage { role: llm::Role::User, text: prompt }];
        let _ = tx.send(aitasks::run(aitasks::AiTask::TextEditing, |client| client.send(SYSTEM_PROMPT, &message)));
    });

    let ui = ui.clone();
    glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv().map(aitasks::deliver) {
        Ok(Ok(text)) => {
            let suggestions = parse_suggestions(&text, &current_tags);
            ui.populate(&suggestions, &existing_tags);
            if suggestions.is_empty() {
                ui.show_message("user-bookmarks-symbolic", &tr("Keine neuen Tag-Vorschläge"), &tr("Alles Passende ist schon gesetzt."));
            } else {
                ui.stack.set_visible_child_name("results");
            }
            glib::ControlFlow::Break
        }
        Ok(Err(err)) => {
            ui.show_message("dialog-error-symbolic", &tr("Generierung fehlgeschlagen"), &err);
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            ui.show_message("dialog-error-symbolic", &tr("Generierung fehlgeschlagen"), &tr("Interner Fehler: Generierungs-Thread hat kein Ergebnis geliefert."));
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
