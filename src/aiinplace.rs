//! "Direkt im Text korrigieren": a second set of context-menu KI-Aktionen
//! (Stil/Rechtschreibung/Zeichensetzung/Länge) that, unlike `aimenu.rs`'s
//! existing "KI-Aktionen" (which sends the same customizable prompts to
//! the Chat tab for review), replace the selection in place - modeled on
//! the Quill macOS app's `AIResultPanel`, a floating Accept/Discard bar
//! shown over an AI edit. Blocksatz has no floating in-editor overlay, so
//! this reuses `searchbar.rs`'s own shape instead - a `Gtk.Revealer`
//! sliding up from the bottom of the editor pane - with Übernehmen/
//! Verwerfen buttons in place of search/replace fields.
//!
//! Deliberately its own dedicated, non-customizable instructions rather
//! than reusing `aiprompts.rs`'s "check-style"/"check-spelling"/...
//! templates - those are written to also return a findings list for a
//! human to read in chat (see their own default templates), which isn't
//! something that can be silently dropped into the buffer. `aiprompts.rs`'s
//! "Prompt anpassen" settings only ever affect the existing chat-based
//! actions, not these.
//!
//! "Verwerfen" is a single `buffer.undo()`, not a hand-tracked text/offset
//! snapshot - the replacement is written as one `begin_user_action()`/
//! `end_user_action()` group, so GtkSourceView's own undo stack already
//! knows how to reverse exactly that edit. To keep that safe regardless of
//! what the user does afterwards, any further buffer change (more typing,
//! another in-place run, ...) hides the bar the moment it happens - once
//! that's fired, "Verwerfen" is guaranteed either not clickable at all, or
//! still pointing at exactly the AI's own edit and nothing the user has
//! since layered on top of it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk4::glib;

use crate::aitasks;
use crate::i18n::tr;
use crate::llm::{ChatMessage, Role};

const SYSTEM_PROMPT: &str = "Du überarbeitest Textabschnitte für einen deutschsprachigen Blog. Gib IMMER nur den überarbeiteten Text aus - keine Erklärung, keine Einleitung, keine Zusammenfassung, keinen Code-Block.";

/// The instruction sent alongside the selected text for each in-place
/// action - `None` for an unknown id, or for `"length"` without an
/// instruction yet (its dialog hasn't been answered).
pub(crate) fn instruction_for(action: &str, length_instruction: Option<&str>) -> Option<String> {
    match action {
        "style" => Some(
            "Verbessere Stil und Formatierung des folgenden Textes - Tonfall, Satzlänge, Absatzstruktur - orientiert an typischem Blog-Schreibstil. Inhalt und Kernaussagen bleiben unverändert."
                .to_string(),
        ),
        "spelling" => Some(
            "Korrigiere ausschließlich Rechtschreibfehler im folgenden Text. Inhalt, Stil und Zeichensetzung bleiben unverändert, außer wo direkt ein Rechtschreibfehler betroffen ist.".to_string(),
        ),
        "punctuation" => Some(
            "Korrigiere ausschließlich Fehler in der Zeichensetzung (Kommasetzung, Anführungszeichen, Bindestriche/Gedankenstriche) im folgenden Text. Inhalt, Stil und Rechtschreibung bleiben unverändert."
                .to_string(),
        ),
        "length" => length_instruction.map(|instr| format!("Passe die Länge des folgenden Textes {instr} an. Behalte Kernaussagen und Schreibstil bei.")),
        _ => None,
    }
}

/// Drops a wrapping ```` ``` ```` fence some models add despite being told
/// not to - same logic as `aiwriter::parse_generated`'s own fence-stripping,
/// minus the title-line parsing that doesn't apply here.
fn strip_wrapping_fence(text: &str) -> &str {
    let mut text = text.trim();
    if text.starts_with("```") && text.ends_with("```") && text.len() > 6 {
        text = text[3..text.len() - 3].trim();
        if let Some((first, rest)) = text.split_once('\n') {
            if !first.contains(' ') {
                text = rest.trim();
            }
        }
    }
    text
}

pub struct InPlaceBar {
    pub widget: gtk4::Revealer,
    view: sourceview5::View,
    buffer: sourceview5::Buffer,
    spinner: gtk4::Spinner,
    status_label: gtk4::Label,
    accept_button: gtk4::Button,
    discard_button: gtk4::Button,
    close_button: gtk4::Button,
    /// Bumped on every `run()` and on every dismissal - a background
    /// reply is only applied if it still matches, so a superseded (a new
    /// run started before the old one answered) or cancelled (dismissed
    /// while still working) reply is silently dropped instead of
    /// clobbering whatever the bar is doing now.
    run_token: Rc<Cell<u64>>,
    changed_handler: RefCell<Option<glib::SignalHandlerId>>,
}

impl InPlaceBar {
    pub fn new(view: &sourceview5::View, buffer: &sourceview5::Buffer) -> Rc<Self> {
        let spinner = gtk4::Spinner::new();
        let status_label = gtk4::Label::builder().hexpand(true).xalign(0.0).ellipsize(gtk4::pango::EllipsizeMode::End).build();
        let accept_button = gtk4::Button::with_label(&tr("Übernehmen"));
        accept_button.add_css_class("suggested-action");
        let discard_button = gtk4::Button::with_label(&tr("Verwerfen"));
        let close_button = gtk4::Button::from_icon_name("window-close-symbolic");
        close_button.add_css_class("flat");
        close_button.set_tooltip_text(Some(&tr("Schließen")));

        let row = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .spacing(6)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(6)
            .margin_end(6)
            .build();
        row.append(&spinner);
        row.append(&status_label);
        row.append(&discard_button);
        row.append(&accept_button);
        row.append(&close_button);

        let widget = gtk4::Revealer::builder().transition_type(gtk4::RevealerTransitionType::SlideUp).child(&row).build();

        let bar = Rc::new(InPlaceBar {
            widget,
            view: view.clone(),
            buffer: buffer.clone(),
            spinner,
            status_label,
            accept_button,
            discard_button,
            close_button,
            run_token: Rc::new(Cell::new(0)),
            changed_handler: RefCell::new(None),
        });

        {
            let this = bar.clone();
            bar.accept_button.connect_clicked(move |_| this.dismiss());
        }
        {
            let this = bar.clone();
            bar.discard_button.connect_clicked(move |_| {
                this.buffer.undo();
                this.dismiss();
            });
        }
        {
            let this = bar.clone();
            bar.close_button.connect_clicked(move |_| this.dismiss());
        }

        bar
    }

    /// Sends `instruction` plus the current selection (or, if nothing is
    /// selected, the whole article - same convention as `aimenu.rs`'s
    /// existing chat-based actions) to the active KI-Chat provider, then
    /// replaces the selection with the reply once it arrives.
    pub fn run(self: &Rc<Self>, action_label: &str, instruction: String) {
        if let Some(id) = self.changed_handler.borrow_mut().take() {
            self.buffer.disconnect(id);
        }
        let (start, end) = self.buffer.selection_bounds().unwrap_or_else(|| (self.buffer.start_iter(), self.buffer.end_iter()));
        let start_offset = start.offset();
        let end_offset = end.offset();
        let original_text = self.buffer.text(&start, &end, false).to_string();
        if original_text.trim().is_empty() {
            return;
        }

        let token = self.run_token.get() + 1;
        self.run_token.set(token);

        self.view.set_editable(false);
        self.spinner.set_visible(true);
        self.spinner.start();
        self.status_label.set_label(&format!("{action_label} …"));
        self.accept_button.set_visible(false);
        self.discard_button.set_visible(false);
        self.close_button.set_visible(true);
        self.widget.set_reveal_child(true);

        let user_prompt = format!("{instruction}\n\n---\n\n{original_text}");
        let (tx, rx) = mpsc::channel::<Result<aitasks::Routed<String>, String>>();
        std::thread::spawn(move || {
            let message = [ChatMessage { role: Role::User, text: user_prompt }];
            let _ = tx.send(aitasks::run(aitasks::AiTask::TextEditing, |client| client.send(SYSTEM_PROMPT, &message)));
        });

        let this = self.clone();
        let action_label = action_label.to_string();
        glib::timeout_add_local(Duration::from_millis(150), move || {
            if this.run_token.get() != token {
                return glib::ControlFlow::Break;
            }
            match rx.try_recv().map(aitasks::deliver) {
                Ok(outcome) => {
                    this.spinner.stop();
                    this.spinner.set_visible(false);
                    this.view.set_editable(true);
                    match outcome {
                        Ok(reply) => this.apply_replacement(start_offset, end_offset, strip_wrapping_fence(&reply), &action_label),
                        Err(err) => {
                            this.status_label.set_label(&tr("Fehler: {err}").replace("{err}", &err));
                            this.close_button.set_visible(true);
                        }
                    }
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    this.spinner.stop();
                    this.spinner.set_visible(false);
                    this.view.set_editable(true);
                    this.status_label.set_label(&tr("Interner Fehler: keine Antwort erhalten."));
                    glib::ControlFlow::Break
                }
            }
        });
    }

    fn apply_replacement(self: &Rc<Self>, start_offset: i32, end_offset: i32, replacement: &str, action_label: &str) {
        if replacement.trim().is_empty() {
            self.status_label.set_label(&tr("Die KI hat keinen Text zurückgegeben."));
            self.close_button.set_visible(true);
            return;
        }
        let mut start = self.buffer.iter_at_offset(start_offset);
        let mut end = self.buffer.iter_at_offset(end_offset);
        self.buffer.begin_user_action();
        self.buffer.delete(&mut start, &mut end);
        self.buffer.insert(&mut start, replacement);
        self.buffer.end_user_action();

        self.status_label.set_label(&format!("{action_label} - {}", tr("übernehmen oder verwerfen?")));
        self.accept_button.set_visible(true);
        self.discard_button.set_visible(true);
        self.close_button.set_visible(false);

        let weak = Rc::downgrade(self);
        let handler_id = self.buffer.connect_changed(move |_| {
            if let Some(this) = weak.upgrade() {
                this.dismiss();
            }
        });
        *self.changed_handler.borrow_mut() = Some(handler_id);
    }

    fn dismiss(self: &Rc<Self>) {
        self.run_token.set(self.run_token.get() + 1);
        self.view.set_editable(true);
        self.spinner.stop();
        if let Some(id) = self.changed_handler.borrow_mut().take() {
            self.buffer.disconnect(id);
        }
        self.widget.set_reveal_child(false);
    }
}

/// The visible label for each in-place action id - shown in the bar while
/// working and, appended with the übernehmen/verwerfen prompt, once a
/// reply has landed.
pub(crate) fn action_label(action: &str) -> String {
    match action {
        "style" => tr("Stil korrigieren"),
        "spelling" => tr("Rechtschreibung korrigieren"),
        "punctuation" => tr("Zeichensetzung korrigieren"),
        "length" => tr("Länge anpassen"),
        _ => tr("KI-Aktion"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruction_for_style_is_stable() {
        assert!(instruction_for("style", None).unwrap().contains("Stil"));
    }

    #[test]
    fn instruction_for_spelling_only_targets_spelling() {
        let text = instruction_for("spelling", None).unwrap();
        assert!(text.contains("Rechtschreibfehler"));
    }

    #[test]
    fn instruction_for_length_needs_an_instruction() {
        assert_eq!(instruction_for("length", None), None);
        assert!(instruction_for("length", Some("auf etwa 800 Wörter")).unwrap().contains("auf etwa 800 Wörter"));
    }

    #[test]
    fn instruction_for_unknown_action_is_none() {
        assert_eq!(instruction_for("unknown", None), None);
    }

    #[test]
    fn strip_wrapping_fence_drops_a_bare_fence() {
        assert_eq!(strip_wrapping_fence("```\nHello world.\n```"), "Hello world.");
    }

    #[test]
    fn strip_wrapping_fence_drops_a_language_tagged_fence() {
        assert_eq!(strip_wrapping_fence("```markdown\nHello world.\n```"), "Hello world.");
    }

    #[test]
    fn strip_wrapping_fence_leaves_plain_text_alone() {
        assert_eq!(strip_wrapping_fence("Hello world."), "Hello world.");
    }

    #[test]
    fn action_label_covers_every_known_action() {
        for action in ["style", "spelling", "punctuation", "length"] {
            assert_ne!(action_label(action), tr("KI-Aktion"));
        }
    }
}
