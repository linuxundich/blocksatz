//! The "Chat" tab: an LLM-backed chat pane (Gemini/ChatGPT/Claude/Groq/Ollama,
//! whichever is active in the KI-Chat settings) with message bubbles. Sends
//! run on a background thread (see `llm.rs`'s module docs for why), polled
//! back via the same thread+mpsc-channel+`glib::timeout_add_local` pattern
//! used throughout this app's WordPress-facing dialogs. The model's replies
//! are rendered as Markdown (`mdpango`) so headings/lists/code/links show up
//! formatted instead of as raw Markdown source.
//!
//! A free-form message typed here gets the editor's current selection (or,
//! if nothing's selected, the whole article) appended after it before it's
//! sent - the same "what should this apply to" rule the context menu's AI
//! actions already use (see `editor::selected_or_full_text`), so the model
//! always has the article as context without the user having to paste it
//! in by hand. Only the typed message itself is shown in the chat bubble -
//! the appended context would make the transcript unreadable otherwise.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk4::glib;

use crate::editor;
use crate::i18n::tr;
use crate::llm;
use crate::mdpango;
use crate::{chatconfig, secrets};

pub struct ChatView {
    pub widget: gtk4::Widget,
    provider_label: gtk4::Label,
    model_dropdown: gtk4::DropDown,
    updating_model_dropdown: Rc<Cell<bool>>,
    send_fn: Rc<dyn Fn(String, String)>,
}

impl ChatView {
    pub fn new(buffer: &sourceview5::Buffer) -> Self {
        let buffer = buffer.clone();
        let messages_box = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(8)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        let scroller = gtk4::ScrolledWindow::builder().child(&messages_box).vexpand(true).build();

        let provider_label = gtk4::Label::new(None);
        provider_label.add_css_class("dim-label");

        let model_dropdown = gtk4::DropDown::new(None::<gtk4::StringList>, None::<gtk4::Expression>);
        model_dropdown.set_hexpand(true);

        let model_row = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).spacing(8).margin_top(6).margin_bottom(6).margin_start(6).margin_end(6).build();
        model_row.append(&provider_label);
        model_row.append(&model_dropdown);

        let entry = gtk4::Entry::builder().placeholder_text(tr("Nachricht an die KI …")).hexpand(true).build();
        let send_button = gtk4::Button::from_icon_name("mail-send-symbolic");
        send_button.add_css_class("suggested-action");
        send_button.set_tooltip_text(Some(&tr("Senden (Enter)")));

        let status_label = gtk4::Label::new(None);
        status_label.add_css_class("dim-label");
        status_label.set_xalign(0.0);
        status_label.set_margin_start(6);
        status_label.set_visible(false);

        let input_row = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .spacing(6)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(6)
            .margin_end(6)
            .build();
        input_row.append(&entry);
        input_row.append(&send_button);

        let root = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
        root.append(&model_row);
        root.append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));
        root.append(&scroller);
        root.append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));
        root.append(&status_label);
        root.append(&input_row);

        let history: Rc<RefCell<Vec<llm::ChatMessage>>> = Rc::new(RefCell::new(Vec::new()));
        let updating_model_dropdown: Rc<Cell<bool>> = Rc::new(Cell::new(false));

        {
            let updating_model_dropdown = updating_model_dropdown.clone();
            model_dropdown.connect_selected_notify(move |dropdown| {
                if updating_model_dropdown.get() {
                    return;
                }
                let Some(model) = dropdown.selected_item().and_downcast::<gtk4::StringObject>() else {
                    return;
                };
                let mut config = chatconfig::load_provider_config();
                let provider = config.active;
                config.set_model_for(provider, model.string().to_string());
                let _ = chatconfig::save_provider_config(&config);
            });
        }

        let send_fn: Rc<dyn Fn(String, String)> = {
            let entry = entry.clone();
            let send_button = send_button.clone();
            let messages_box = messages_box.clone();
            let scroller = scroller.clone();
            let status_label = status_label.clone();
            let history = history.clone();
            Rc::new(move |display_text: String, history_text: String| {
                append_bubble(&messages_box, &display_text, true);
                scroll_to_bottom(&scroller);
                history.borrow_mut().push(llm::ChatMessage {
                    role: llm::Role::User,
                    text: history_text,
                });

                entry.set_sensitive(false);
                send_button.set_sensitive(false);

                let config = chatconfig::load_provider_config();
                let provider = config.active;
                let model = config.model_for(provider).to_string();
                let base_url = config.ollama_base_url.clone();
                let system_prompt = chatconfig::load_system_prompt();
                let history_snapshot = history.borrow().clone();

                status_label.set_label(&tr("{provider} denkt nach …").replace("{provider}", provider.label()));
                status_label.set_visible(true);

                let (tx, rx) = mpsc::channel::<Result<String, String>>();
                std::thread::spawn(move || {
                    let outcome = if provider.needs_api_key() {
                        futures_lite::future::block_on(secrets::load_llm_api_key(provider.id()))
                            .map_err(|err| err.to_string())
                            .and_then(|maybe_key| {
                                maybe_key.ok_or_else(|| tr("Kein {provider}-API-Key in den Einstellungen hinterlegt.").replace("{provider}", provider.label()))
                            })
                            .and_then(|key| {
                                llm::Client::new(provider, &key, &model, &base_url)
                                    .send(&system_prompt, &history_snapshot)
                                    .map_err(|err| err.to_string())
                            })
                    } else {
                        llm::Client::new(provider, "", &model, &base_url)
                            .send(&system_prompt, &history_snapshot)
                            .map_err(|err| err.to_string())
                    };
                    let _ = tx.send(outcome);
                });

                let entry = entry.clone();
                let send_button = send_button.clone();
                let messages_box = messages_box.clone();
                let scroller = scroller.clone();
                let status_label = status_label.clone();
                let history = history.clone();
                glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
                    Ok(Ok(reply)) => {
                        append_bubble(&messages_box, &reply, false);
                        scroll_to_bottom(&scroller);
                        history.borrow_mut().push(llm::ChatMessage {
                            role: llm::Role::Model,
                            text: reply,
                        });
                        status_label.set_visible(false);
                        entry.set_sensitive(true);
                        send_button.set_sensitive(true);
                        entry.grab_focus();
                        glib::ControlFlow::Break
                    }
                    Ok(Err(err)) => {
                        status_label.set_label(&tr("Fehler: {err}").replace("{err}", &err));
                        entry.set_sensitive(true);
                        send_button.set_sensitive(true);
                        glib::ControlFlow::Break
                    }
                    Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        status_label.set_label(&tr("Interner Fehler: Chat-Thread hat kein Ergebnis geliefert."));
                        entry.set_sensitive(true);
                        send_button.set_sensitive(true);
                        glib::ControlFlow::Break
                    }
                });
            })
        };

        {
            let send_fn = send_fn.clone();
            let entry_for_activate = entry.clone();
            let buffer = buffer.clone();
            entry.connect_activate(move |_| {
                let text = entry_for_activate.text().trim().to_string();
                if text.is_empty() {
                    return;
                }
                entry_for_activate.set_text("");
                let context = editor::selected_or_full_text(&buffer);
                let full_prompt = format!("{text}\n\n---\n\n{context}");
                send_fn(text, full_prompt);
            });
        }
        {
            let send_fn = send_fn.clone();
            let entry = entry.clone();
            let buffer = buffer.clone();
            send_button.connect_clicked(move |_| {
                let text = entry.text().trim().to_string();
                if text.is_empty() {
                    return;
                }
                entry.set_text("");
                let context = editor::selected_or_full_text(&buffer);
                let full_prompt = format!("{text}\n\n---\n\n{context}");
                send_fn(text, full_prompt);
            });
        }

        let view = Self {
            widget: root.upcast(),
            provider_label,
            model_dropdown,
            updating_model_dropdown,
            send_fn,
        };
        view.refresh();
        view
    }

    /// Re-reads the active provider/model from `chatconfig` and repopulates
    /// the provider label and model picker accordingly. Call this whenever
    /// the Chat tab becomes visible, since the active provider/model may
    /// have been changed in Einstellungen since the tab was built.
    pub fn refresh(&self) {
        let config = chatconfig::load_provider_config();
        let provider = config.active;
        self.provider_label.set_label(provider.label());

        let current_model = config.model_for(provider).to_string();
        let mut models = chatconfig::load_cached_models(provider);
        if !current_model.is_empty() && !models.iter().any(|m| m == &current_model) {
            models.insert(0, current_model.clone());
        }
        if models.is_empty() {
            models.push(current_model.clone());
        }

        self.updating_model_dropdown.set(true);
        let refs: Vec<&str> = models.iter().map(String::as_str).collect();
        self.model_dropdown.set_model(Some(&gtk4::StringList::new(&refs)));
        let selected = models.iter().position(|m| m == &current_model).unwrap_or(0);
        self.model_dropdown.set_selected(selected as u32);
        self.updating_model_dropdown.set(false);
    }

    /// Runs a predefined AI action (from the editor's context menu): shows
    /// `display_label` as the user's chat bubble (so the transcript stays
    /// readable, e.g. "Stil & Formatierung prüfen" instead of the whole
    /// article), while `full_prompt` - the actual instruction plus the
    /// selected/full article text - is what's sent to and remembered by the
    /// model.
    pub fn run_action(&self, display_label: &str, full_prompt: String) {
        (self.send_fn)(display_label.to_string(), full_prompt);
    }
}

fn append_bubble(messages_box: &gtk4::Box, text: &str, is_user: bool) {
    let label = gtk4::Label::builder().wrap(true).xalign(0.0).selectable(true).max_width_chars(48).build();
    label.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
    if is_user {
        label.set_text(text);
    } else {
        label.set_use_markup(true);
        label.set_markup(&mdpango::markdown_to_pango(text));
    }

    let bubble = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    bubble.append(&label);
    bubble.add_css_class("chat-bubble");
    bubble.add_css_class(if is_user { "chat-bubble-user" } else { "chat-bubble-model" });

    let row = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).build();
    row.set_halign(if is_user { gtk4::Align::End } else { gtk4::Align::Start });
    row.append(&bubble);

    messages_box.append(&row);
}

fn scroll_to_bottom(scroller: &gtk4::ScrolledWindow) {
    let adjustment = scroller.vadjustment();
    glib::idle_add_local_once(move || {
        adjustment.set_value(adjustment.upper());
    });
}
