//! The "KI-Modelle" page of the Einstellungen dialog: the capability check
//! (`modelcheck.rs`) per provider - which listed models this key can
//! actually use - and the per-task model assignment (`aitasks.rs`), whose
//! pickers only offer models that aren't known to be blocked.
//!
//! The API keys and the KI-Chat's own provider/model stay on the
//! "KI-Chat" page (`chatsettings.rs`); this page only reads them.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk4::glib;

use crate::aitasks::{self, AiTask, ModelRef, TaskAssignment};
use crate::i18n::tr;
use crate::llm::{self, ModelStatus, Provider};
use crate::{chatconfig, modelcheck, secrets};

/// One provider's expander in the availability group, plus the model rows
/// currently inside it (an `Adw.ExpanderRow` can't enumerate its own rows).
struct ProviderSection {
    provider: Provider,
    expander: adw::ExpanderRow,
    check_button: gtk4::Button,
    rows: RefCell<Vec<adw::ActionRow>>,
}

/// One task's two pickers and the `ModelRef` behind each of their entries
/// (`None` = the "follow KI-Chat"/"no fallback" entry).
struct TaskRows {
    task: AiTask,
    primary: adw::ComboRow,
    fallback: adw::ComboRow,
    primary_options: RefCell<Vec<Option<ModelRef>>>,
    fallback_options: RefCell<Vec<Option<ModelRef>>>,
}

fn status_icon(status: ModelStatus) -> gtk4::Image {
    let (icon, class) = match status {
        ModelStatus::Available => ("emblem-ok-symbolic", Some("success")),
        ModelStatus::Unchecked => ("content-loading-symbolic", None),
        ModelStatus::RateLimited | ModelStatus::ServerError => ("dialog-warning-symbolic", Some("warning")),
        _ => ("action-unavailable-symbolic", Some("error")),
    };
    let image = gtk4::Image::from_icon_name(icon);
    if let Some(class) = class {
        image.add_css_class(class);
    }
    image
}

/// "12 Modelle · 7 nutzbar · 3 Abo/Guthaben erforderlich · 2 nicht geprüft"
fn summary(provider: Provider, models: &[String], cache: &modelcheck::Cache, now: u64) -> String {
    if models.is_empty() {
        return if provider.needs_api_key() {
            tr("Noch keine Modelle geladen - API-Key unter „KI-Chat“ eintragen, dann hier prüfen.")
        } else {
            tr("Noch keine Modelle geladen - Server unter „KI-Chat“ eintragen, dann hier prüfen.")
        };
    }
    let mut parts = vec![tr("{n} Modelle").replace("{n}", &models.len().to_string())];
    for status in ModelStatus::ALL {
        let count = models.iter().filter(|m| cache.latest_status(provider, m, now) == status).count();
        if count > 0 {
            parts.push(format!("{count} × {}", status.label()));
        }
    }
    parts.join(" · ")
}

fn populate_section(section: &ProviderSection) {
    for row in section.rows.borrow_mut().drain(..) {
        section.expander.remove(&row);
    }
    let models = chatconfig::load_cached_models(section.provider);
    let cache = modelcheck::load();
    let now = modelcheck::now();
    section.expander.set_subtitle(&summary(section.provider, &models, &cache, now));
    for model in &models {
        let status = cache.latest_status(section.provider, model, now);
        let row = adw::ActionRow::builder().title(model.as_str()).subtitle(status.label()).use_markup(false).build();
        row.add_prefix(&status_icon(status));
        if let Some(entry) = cache.latest_entry(section.provider, model).filter(|e| !e.detail.is_empty()) {
            row.set_tooltip_text(Some(&entry.detail));
        }
        section.expander.add_row(&row);
        section.rows.borrow_mut().push(row);
    }
}

/// Every model a task picker may offer: all cached models of all
/// providers, minus the ones the capability check found blocked - except
/// `current`, which stays listed (marked with its status) so opening the
/// page never silently changes a saved assignment.
fn model_options(current: Option<&ModelRef>) -> Vec<(ModelRef, String)> {
    let cache = modelcheck::load();
    let now = modelcheck::now();
    let mut out = Vec::new();
    for provider in Provider::ALL {
        for model in chatconfig::load_cached_models(provider) {
            let model_ref = ModelRef { provider, model };
            let status = cache.latest_status(provider, &model_ref.model, now);
            if status.is_permanent_block() && current != Some(&model_ref) {
                continue;
            }
            let label = format!("{} - {}", model_ref.label(), status.label());
            out.push((model_ref, label));
        }
    }
    if let Some(current) = current {
        if !out.iter().any(|(m, _)| m == current) {
            let status = cache.latest_status(current.provider, &current.model, now);
            out.insert(0, (current.clone(), format!("{} - {}", current.label(), status.label())));
        }
    }
    out
}

fn fill_combo(combo: &adw::ComboRow, options: &RefCell<Vec<Option<ModelRef>>>, none_label: &str, current: Option<&ModelRef>) {
    let mut values: Vec<Option<ModelRef>> = vec![None];
    let mut labels: Vec<String> = vec![none_label.to_string()];
    for (model_ref, label) in model_options(current) {
        values.push(Some(model_ref));
        labels.push(label);
    }
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    combo.set_model(Some(&gtk4::StringList::new(&refs)));
    combo.set_selected(values.iter().position(|v| v.as_ref() == current).unwrap_or(0) as u32);
    *options.borrow_mut() = values;
}

fn populate_task_rows(rows: &[Rc<TaskRows>], updating: &Cell<bool>) {
    updating.set(true);
    let chat_default = ModelRef::chat_default(&chatconfig::load_provider_config());
    let follow_chat = tr("Wie KI-Chat ({model})").replace("{model}", &chat_default.label());
    for task_rows in rows {
        let assignment = aitasks::load_assignment(task_rows.task);
        fill_combo(&task_rows.primary, &task_rows.primary_options, &follow_chat, assignment.primary.as_ref());
        fill_combo(&task_rows.fallback, &task_rows.fallback_options, &tr("Kein Ersatzmodell"), assignment.fallback.as_ref());
    }
    updating.set(false);
}

/// Loads the key, refreshes the provider's model list, then probes every
/// model - all on a worker thread, with each verdict updating the page as
/// it arrives.
fn start_check(section: Rc<ProviderSection>, status_label: gtk4::Label, on_finished: Rc<dyn Fn()>) {
    let provider = section.provider;
    section.check_button.set_sensitive(false);
    section.expander.set_expanded(true);
    status_label.remove_css_class("error");
    status_label.set_visible(true);
    status_label.set_label(&tr("{provider}: Modellliste wird geladen …").replace("{provider}", provider.label()));

    glib::MainContext::default().spawn_local(async move {
        let api_key = if provider.needs_api_key() {
            match secrets::load_llm_api_key(provider.id()).await {
                Ok(Some(key)) => key,
                _ => {
                    status_label.add_css_class("error");
                    status_label.set_label(&tr("Kein {provider}-API-Key in den Einstellungen hinterlegt.").replace("{provider}", provider.label()));
                    section.check_button.set_sensitive(true);
                    return;
                }
            }
        } else {
            String::new()
        };
        let base_url = chatconfig::load_provider_config().ollama_base_url;

        enum Event {
            Listed(Vec<String>),
            Probed(modelcheck::Probed),
            Failed(String),
            Done,
        }
        let (tx, rx) = mpsc::channel::<Event>();
        std::thread::spawn(move || {
            let models = match llm::Client::new(provider, &api_key, "unused", &base_url).list_models() {
                Ok(models) => models,
                Err(err) => {
                    let _ = tx.send(Event::Failed(err.to_string()));
                    return;
                }
            };
            let _ = chatconfig::save_cached_models(provider, &models);
            let _ = tx.send(Event::Listed(models.clone()));
            modelcheck::check_models(provider, &api_key, &base_url, &models, |probed| {
                let _ = tx.send(Event::Probed(probed));
            });
            let _ = tx.send(Event::Done);
        });

        let total = Rc::new(Cell::new(0usize));
        let done = Rc::new(Cell::new(0usize));
        glib::timeout_add_local(Duration::from_millis(150), move || loop {
            match rx.try_recv() {
                Ok(Event::Listed(models)) => {
                    total.set(models.len());
                    populate_section(&section);
                }
                Ok(Event::Probed(probed)) => {
                    done.set(done.get() + 1);
                    status_label.set_label(
                        &tr("{provider}: {done} von {total} Modellen geprüft …")
                            .replace("{provider}", provider.label())
                            .replace("{done}", &done.get().to_string())
                            .replace("{total}", &total.get().to_string()),
                    );
                    if let Some(row) = section.rows.borrow().iter().find(|r| r.title() == probed.model) {
                        row.set_subtitle(&probed.status.label());
                        row.set_tooltip_text((!probed.detail.is_empty()).then_some(probed.detail.as_str()));
                    }
                }
                Ok(Event::Failed(err)) => {
                    status_label.add_css_class("error");
                    status_label.set_label(&format!("✗ {err}"));
                    section.check_button.set_sensitive(true);
                    return glib::ControlFlow::Break;
                }
                Ok(Event::Done) | Err(mpsc::TryRecvError::Disconnected) => {
                    populate_section(&section);
                    section.check_button.set_sensitive(true);
                    status_label.set_label(&tr("{provider}: Prüfung abgeschlossen.").replace("{provider}", provider.label()));
                    on_finished();
                    return glib::ControlFlow::Break;
                }
                Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
            }
        });
    });
}

pub fn build_page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder().title(tr("KI-Modelle")).icon_name("applications-science-symbolic").build();

    let availability_group = adw::PreferencesGroup::builder()
        .title(tr("Verfügbarkeit"))
        .description(tr(
            "„Prüfen“ lädt die Modellliste des Anbieters und schickt jedem Modell eine Mini-Anfrage mit wenigen Tokens. So zeigt sich, welche Modelle mit deinem Key wirklich nutzbar sind - und welche ein Abo oder Guthaben brauchen, ihr Kontingent erschöpft haben oder in deiner Region gesperrt sind. Die Ergebnisse werden zwischengespeichert.",
        ))
        .build();
    let status_label = gtk4::Label::builder().xalign(0.0).wrap(true).visible(false).build();
    status_label.add_css_class("dim-label");
    let status_group = adw::PreferencesGroup::new();
    status_group.add(&status_label);

    let updating = Rc::new(Cell::new(false));
    let task_rows: Rc<Vec<Rc<TaskRows>>> = Rc::new(
        AiTask::ALL
            .iter()
            .map(|task| {
                Rc::new(TaskRows {
                    task: *task,
                    primary: adw::ComboRow::builder().title(tr("Hauptmodell")).enable_search(true).build(),
                    fallback: adw::ComboRow::builder().title(tr("Ersatzmodell")).subtitle(tr("Wird genutzt, wenn das Hauptmodell nicht verfügbar ist")).enable_search(true).build(),
                    primary_options: RefCell::new(Vec::new()),
                    fallback_options: RefCell::new(Vec::new()),
                })
            })
            .collect(),
    );

    let on_finished: Rc<dyn Fn()> = {
        let task_rows = task_rows.clone();
        let updating = updating.clone();
        Rc::new(move || populate_task_rows(&task_rows, &updating))
    };

    for provider in Provider::ALL {
        let check_button = gtk4::Button::builder().label(tr("Prüfen")).valign(gtk4::Align::Center).build();
        let expander = adw::ExpanderRow::builder().title(provider.label()).build();
        expander.add_suffix(&check_button);
        let section = Rc::new(ProviderSection { provider, expander, check_button, rows: RefCell::new(Vec::new()) });
        populate_section(&section);
        {
            let section_for_click = section.clone();
            let status_label = status_label.clone();
            let on_finished = on_finished.clone();
            section.check_button.connect_clicked(move |_| start_check(section_for_click.clone(), status_label.clone(), on_finished.clone()));
        }
        availability_group.add(&section.expander);
    }

    page.add(&availability_group);
    page.add(&status_group);

    page.add(
        &adw::PreferencesGroup::builder()
            .title(tr("Modelle je Aufgabe"))
            .description(tr("Jede Aufgabe kann ein eigenes Modell nutzen. Ist das Hauptmodell nicht nutzbar (Abo nötig, Kontingent erschöpft, kein Zugriff), springt automatisch das Ersatzmodell ein - mit einem Hinweis. Zur Auswahl stehen alle geladenen Modelle, außer denen, die die Prüfung als gesperrt erkannt hat."))
            .build(),
    );
    for task_rows_item in task_rows.iter() {
        let group = adw::PreferencesGroup::builder().title(task_rows_item.task.label()).description(task_rows_item.task.description()).build();
        group.add(&task_rows_item.primary);
        group.add(&task_rows_item.fallback);
        for combo in [&task_rows_item.primary, &task_rows_item.fallback] {
            let this = task_rows_item.clone();
            let updating = updating.clone();
            combo.connect_selected_notify(move |_| {
                if updating.get() {
                    return;
                }
                let pick = |combo: &adw::ComboRow, options: &RefCell<Vec<Option<ModelRef>>>| options.borrow().get(combo.selected() as usize).cloned().flatten();
                let assignment = TaskAssignment { primary: pick(&this.primary, &this.primary_options), fallback: pick(&this.fallback, &this.fallback_options) };
                let _ = aitasks::save_assignment(this.task, &assignment);
            });
        }
        page.add(&group);
    }
    populate_task_rows(&task_rows, &updating);

    page
}
