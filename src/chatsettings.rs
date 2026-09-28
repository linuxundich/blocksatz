//! The "KI-Chat" page of the Einstellungen dialog: provider selection
//! (Gemini/ChatGPT/Claude/Groq/Ollama), that provider's API key (a secret,
//! stored via `secrets.rs`, verified live against the provider's API as
//! soon as it's entered) and model (picked from the models the provider
//! actually offers, fetched via `llm::Client::list_models`), Ollama's base
//! URL, and the editable/resettable system prompt (persisted via
//! `chatconfig.rs`).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk4::glib;

use crate::i18n::tr;
use crate::llm::{self, Provider};
use crate::{chatconfig, secrets};

pub fn build_page() -> adw::PreferencesPage {
    let state: Rc<RefCell<chatconfig::ProviderConfig>> = Rc::new(RefCell::new(chatconfig::load_provider_config()));

    let provider_labels: Vec<&str> = Provider::ALL.iter().map(|p| p.label()).collect();
    let provider_row = adw::ComboRow::builder()
        .title(tr("Anbieter"))
        .model(&gtk4::StringList::new(&provider_labels))
        .build();
    let active_index = Provider::ALL.iter().position(|p| *p == state.borrow().active).unwrap_or(0);
    provider_row.set_selected(active_index as u32);

    let api_key_row = adw::PasswordEntryRow::builder().title("API-Key").build();
    let base_url_row = adw::EntryRow::builder().title(tr("Basis-URL")).build();
    let model_row = adw::ComboRow::builder().title(tr("Modell")).build();
    model_row.set_enable_search(true);

    let connection_group = adw::PreferencesGroup::builder().title(tr("KI-Verbindung")).build();
    connection_group.set_description(Some(&tr(
        "Der API-Key wird im Schlüsselbund gespeichert, nicht als Klartext. Er wird nach der Eingabe direkt gegen die API des Anbieters geprüft und bei Erfolg automatisch gespeichert.",
    )));
    connection_group.add(&provider_row);
    connection_group.add(&api_key_row);
    connection_group.add(&base_url_row);
    connection_group.add(&model_row);

    let connection_status = gtk4::Label::builder().label("").xalign(0.0).build();
    connection_status.add_css_class("dim-label");
    connection_status.set_wrap(true);
    connection_status.set_visible(false);
    let connection_status_group = adw::PreferencesGroup::new();
    connection_status_group.add(&connection_status);

    fn combo_row_selected_string(row: &adw::ComboRow) -> String {
        row.model()
            .and_then(|m| m.downcast::<gtk4::StringList>().ok())
            .and_then(|list| list.string(row.selected()))
            .map(|s| s.to_string())
            .unwrap_or_default()
    }

    fn populate_model_row(model_row: &adw::ComboRow, models: &[String], current: &str) {
        let mut items: Vec<String> = models.to_vec();
        if !current.is_empty() && !items.iter().any(|m| m == current) {
            items.insert(0, current.to_string());
        }
        if items.is_empty() {
            items.push(current.to_string());
        }
        let refs: Vec<&str> = items.iter().map(String::as_str).collect();
        model_row.set_model(Some(&gtk4::StringList::new(&refs)));
        let selected = items.iter().position(|m| m == current).unwrap_or(0);
        model_row.set_selected(selected as u32);
    }

    /// Calls the provider's models endpoint on a background thread and
    /// reports the outcome - this is both the "is this API key valid?"
    /// check and the source of the model picker's contents, since a
    /// successful models list *is* the proof the key works. That success is
    /// also the save trigger: the key is only ever written to the keyring
    /// once it's been confirmed to actually work, never while it's still
    /// being typed or known to be wrong.
    fn spawn_verify(
        provider: Provider,
        api_key: String,
        base_url: String,
        status_label: gtk4::Label,
        model_row: adw::ComboRow,
        current_model: String,
    ) {
        if provider.needs_api_key() && api_key.trim().is_empty() {
            status_label.set_visible(false);
            return;
        }
        status_label.remove_css_class("error");
        status_label.set_label(&if provider.needs_api_key() { tr("API-Key wird geprüft …") } else { tr("Verbindung wird geprüft …") });
        status_label.set_visible(true);

        let key_to_store = api_key.clone();
        let (tx, rx) = mpsc::channel::<Result<Vec<String>, String>>();
        std::thread::spawn(move || {
            let client = llm::Client::new(provider, &api_key, "unused", &base_url);
            let _ = tx.send(client.list_models().map_err(|err| err.to_string()));
        });

        glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
            Ok(Ok(models)) => {
                let _ = chatconfig::save_cached_models(provider, &models);
                populate_model_row(&model_row, &models, &current_model);
                let verified_message = tr("✓ Verbindung erfolgreich - {n} Modelle gefunden.").replace("{n}", &models.len().to_string());
                if provider.needs_api_key() {
                    let key = key_to_store.clone();
                    let status_label = status_label.clone();
                    glib::MainContext::default().spawn_local(async move {
                        match secrets::store_llm_api_key(provider.id(), &key).await {
                            Ok(()) => status_label.set_label(&format!("{verified_message} {}", tr("Gespeichert."))),
                            Err(err) => status_label.set_label(&format!("{verified_message} {}", tr("Fehler beim Speichern: {err}").replace("{err}", &err.to_string()))),
                        }
                        status_label.set_visible(true);
                    });
                } else {
                    status_label.set_label(&verified_message);
                    status_label.set_visible(true);
                }
                glib::ControlFlow::Break
            }
            Ok(Err(err)) => {
                status_label.add_css_class("error");
                status_label.set_label(&format!("✗ {err}"));
                status_label.set_visible(true);
                glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
        });
    }

    fn load_api_key_into_row(provider: Provider, api_key_row: &adw::PasswordEntryRow) {
        api_key_row.set_text("");
        let api_key_row = api_key_row.clone();
        glib::MainContext::default().spawn_local(async move {
            if let Ok(Some(key)) = secrets::load_llm_api_key(provider.id()).await {
                api_key_row.set_text(&key);
            }
        });
    }

    fn apply_provider_to_fields(
        provider: Provider,
        config: &chatconfig::ProviderConfig,
        api_key_row: &adw::PasswordEntryRow,
        base_url_row: &adw::EntryRow,
        model_row: &adw::ComboRow,
    ) {
        api_key_row.set_visible(provider.needs_api_key());
        base_url_row.set_visible(provider.needs_base_url());
        base_url_row.set_text(&config.ollama_base_url);
        populate_model_row(model_row, &chatconfig::load_cached_models(provider), config.model_for(provider));
        load_api_key_into_row(provider, api_key_row);
    }

    apply_provider_to_fields(state.borrow().active, &state.borrow(), &api_key_row, &base_url_row, &model_row);

    {
        let state = state.clone();
        let api_key_row = api_key_row.clone();
        let base_url_row = base_url_row.clone();
        let model_row = model_row.clone();
        let connection_status = connection_status.clone();
        provider_row.connect_selected_notify(move |row| {
            // Persist whatever was picked/typed for the *previous* provider
            // before switching the visible fields to the new one, so quick
            // back-and-forth between providers doesn't lose in-progress edits.
            {
                let mut config = state.borrow_mut();
                let previous = config.active;
                config.set_model_for(previous, combo_row_selected_string(&model_row));
                config.ollama_base_url = base_url_row.text().to_string();
            }
            let provider = Provider::ALL[row.selected() as usize];
            state.borrow_mut().active = provider;
            if let Err(err) = chatconfig::save_provider_config(&state.borrow()) {
                connection_status.set_label(&tr("Fehler beim Speichern: {err}").replace("{err}", &err.to_string()));
                connection_status.set_visible(true);
            }
            apply_provider_to_fields(provider, &state.borrow(), &api_key_row, &base_url_row, &model_row);
        });
    }

    // The model picker's own choice needs saving independently of the
    // provider switch above (which only preserves the *previous*
    // provider's pick when navigating away from it) - this is what
    // actually persists picking a different model for the *current* one.
    {
        let state = state.clone();
        let connection_status = connection_status.clone();
        model_row.connect_selected_notify(move |row| {
            let provider = state.borrow().active;
            state.borrow_mut().set_model_for(provider, combo_row_selected_string(row));
            if let Err(err) = chatconfig::save_provider_config(&state.borrow()) {
                connection_status.set_label(&tr("Fehler beim Speichern: {err}").replace("{err}", &err.to_string()));
                connection_status.set_visible(true);
            }
        });
    }

    {
        let state = state.clone();
        let base_url_row = base_url_row.clone();
        let model_row = model_row.clone();
        let connection_status = connection_status.clone();
        let debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        api_key_row.connect_changed(move |row| {
            if let Some(id) = debounce.borrow_mut().take() {
                id.remove();
            }
            let key = row.text().to_string();
            let provider = state.borrow().active;
            let base_url = base_url_row.text().to_string();
            let current_model = state.borrow().model_for(provider).to_string();
            let status_label = connection_status.clone();
            let model_row = model_row.clone();
            let debounce_inner = debounce.clone();
            let id = glib::timeout_add_local(Duration::from_millis(700), move || {
                spawn_verify(provider, key.clone(), base_url.clone(), status_label.clone(), model_row.clone(), current_model.clone());
                *debounce_inner.borrow_mut() = None;
                glib::ControlFlow::Break
            });
            *debounce.borrow_mut() = Some(id);
        });
    }

    {
        let state = state.clone();
        let model_row = model_row.clone();
        let connection_status = connection_status.clone();
        let verify_debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        let save_debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        base_url_row.connect_changed(move |row| {
            let base_url = row.text().to_string();

            // Persists independently of the verify check below, so a base
            // URL is remembered even before (or without) a successful
            // connection - unlike the API key, there's nothing here that
            // could be "wrong" in a way worth withholding the save for.
            if let Some(id) = save_debounce.borrow_mut().take() {
                id.remove();
            }
            {
                let state = state.clone();
                let connection_status = connection_status.clone();
                let base_url = base_url.clone();
                let save_debounce_inner = save_debounce.clone();
                let id = glib::timeout_add_local(Duration::from_millis(500), move || {
                    state.borrow_mut().ollama_base_url = base_url.clone();
                    if let Err(err) = chatconfig::save_provider_config(&state.borrow()) {
                        connection_status.set_label(&tr("Fehler beim Speichern: {err}").replace("{err}", &err.to_string()));
                        connection_status.set_visible(true);
                    }
                    *save_debounce_inner.borrow_mut() = None;
                    glib::ControlFlow::Break
                });
                *save_debounce.borrow_mut() = Some(id);
            }

            if let Some(id) = verify_debounce.borrow_mut().take() {
                id.remove();
            }
            let provider = state.borrow().active;
            if provider.needs_api_key() {
                // Base-URL changes only trigger a live check for providers
                // that don't need a key (Ollama) - for the others this row
                // is hidden anyway.
                return;
            }
            let current_model = state.borrow().model_for(provider).to_string();
            let status_label = connection_status.clone();
            let model_row = model_row.clone();
            let verify_debounce_inner = verify_debounce.clone();
            let id = glib::timeout_add_local(Duration::from_millis(700), move || {
                spawn_verify(provider, String::new(), base_url.clone(), status_label.clone(), model_row.clone(), current_model.clone());
                *verify_debounce_inner.borrow_mut() = None;
                glib::ControlFlow::Break
            });
            *verify_debounce.borrow_mut() = Some(id);
        });
    }

    let prompt_buffer = gtk4::TextBuffer::new(None::<&gtk4::TextTagTable>);
    prompt_buffer.set_text(&chatconfig::load_system_prompt());
    let prompt_view = gtk4::TextView::builder()
        .buffer(&prompt_buffer)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(8)
        .right_margin(8)
        .build();
    let prompt_scroller = gtk4::ScrolledWindow::builder().child(&prompt_view).vexpand(true).min_content_height(320).build();
    prompt_scroller.add_css_class("card");

    let reset_button = gtk4::Button::with_label(&tr("Auf Standard zurücksetzen"));
    reset_button.set_sensitive(chatconfig::is_system_prompt_customized());

    let prompt_status = gtk4::Label::new(None);
    prompt_status.add_css_class("dim-label");
    prompt_status.set_xalign(0.0);
    prompt_status.set_visible(false);

    let debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    // Suppresses the auto-save handler for exactly one `changed` signal -
    // set right before a programmatic `set_text` (the reset button below),
    // so resetting doesn't immediately re-save the default text as a new
    // "customized" prompt.
    let suppress_autosave: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
    {
        let debounce = debounce.clone();
        let prompt_status = prompt_status.clone();
        let reset_button = reset_button.clone();
        let suppress_autosave = suppress_autosave.clone();
        prompt_buffer.connect_changed(move |buf| {
            if suppress_autosave.replace(false) {
                return;
            }
            if let Some(id) = debounce.borrow_mut().take() {
                id.remove();
            }
            let text = buf.text(&buf.start_iter(), &buf.end_iter(), false).to_string();
            let debounce_inner = debounce.clone();
            let prompt_status = prompt_status.clone();
            let reset_button = reset_button.clone();
            let id = glib::timeout_add_local(Duration::from_millis(500), move || {
                match chatconfig::save_system_prompt(&text) {
                    Ok(()) => prompt_status.set_label(&tr("Gespeichert.")),
                    Err(err) => prompt_status.set_label(&tr("Fehler beim Speichern: {err}").replace("{err}", &err.to_string())),
                }
                prompt_status.set_visible(true);
                reset_button.set_sensitive(chatconfig::is_system_prompt_customized());
                *debounce_inner.borrow_mut() = None;
                glib::ControlFlow::Break
            });
            *debounce.borrow_mut() = Some(id);
        });
    }

    {
        let prompt_buffer = prompt_buffer.clone();
        let prompt_status = prompt_status.clone();
        let reset_button_for_click = reset_button.clone();
        reset_button.connect_clicked(move |_| {
            if let Err(err) = chatconfig::reset_system_prompt() {
                prompt_status.set_label(&tr("Fehler beim Zurücksetzen: {err}").replace("{err}", &err.to_string()));
                prompt_status.set_visible(true);
                return;
            }
            *suppress_autosave.borrow_mut() = true;
            prompt_buffer.set_text(&chatconfig::load_system_prompt());
            prompt_status.set_label(&tr("Auf Standard zurückgesetzt."));
            prompt_status.set_visible(true);
            reset_button_for_click.set_sensitive(false);
        });
    }

    let prompt_label = gtk4::Label::builder().label(tr("Systemprompt")).xalign(0.0).hexpand(true).build();
    prompt_label.add_css_class("heading");

    let prompt_header_row = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).spacing(6).build();
    prompt_header_row.append(&prompt_label);
    prompt_header_row.append(&reset_button);

    let prompt_box = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(8).margin_top(12).build();
    prompt_box.append(&prompt_header_row);
    prompt_box.append(&prompt_scroller);
    prompt_box.append(&prompt_status);

    let prompt_group = adw::PreferencesGroup::new();
    prompt_group.add(&prompt_box);

    let page = adw::PreferencesPage::builder().title(tr("KI-Chat")).icon_name("chat-message-new-symbolic").build();
    page.add(&connection_group);
    page.add(&connection_status_group);
    page.add(&prompt_group);
    page
}
