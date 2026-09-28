//! Per-task model routing: each kind of AI work in the app can use its own
//! model - a cheap vision model for image descriptions, a stronger one for
//! editing - with a fallback for when that one isn't usable.
//!
//! A task's assignment is optional: without a primary model it follows the
//! KI-Chat settings' active provider/model, exactly as before this existed.
//! `run` tries the primary, then the fallback, skipping a candidate the
//! capability cache (`modelcheck.rs`) already knows is blocked (no free
//! tier, no access, retired) and moving on when a real call fails for a
//! reason about the model or account rather than the request
//! (`ModelStatus::warrants_fallback`). Whatever a call reveals is written
//! back to that cache. A switch is reported as a notice ("X nicht nutzbar
//! (Grund) - stattdessen Y verwendet"), shown as a toast by `deliver`.

use std::cell::RefCell;
use std::path::PathBuf;

use gtk4::glib;
use serde_json::Value;

use crate::chatconfig::{self, ProviderConfig};
use crate::i18n::tr;
use crate::llm::{self, ModelStatus, Provider};
use crate::{modelcheck, secrets};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiTask {
    /// AI alt text and captions - needs a model that accepts images.
    ImageCaptioning,
    /// Rewriting/correcting in the editor, the article evaluation, tag
    /// suggestions - working on text that already exists.
    TextEditing,
    /// The AI article draft - writing new text from a brief.
    TextGeneration,
}

impl AiTask {
    pub const ALL: [AiTask; 3] = [AiTask::ImageCaptioning, AiTask::TextEditing, AiTask::TextGeneration];

    pub fn id(&self) -> &'static str {
        match self {
            AiTask::ImageCaptioning => "image_captioning",
            AiTask::TextEditing => "text_editing",
            AiTask::TextGeneration => "text_generation",
        }
    }

    pub fn label(&self) -> String {
        match self {
            AiTask::ImageCaptioning => tr("Bildbeschreibungen"),
            AiTask::TextEditing => tr("Lektorat und Überarbeitung"),
            AiTask::TextGeneration => tr("Texterstellung"),
        }
    }

    pub fn description(&self) -> String {
        match self {
            AiTask::ImageCaptioning => tr("KI-Alternativtext und KI-Bildunterschrift - braucht ein Modell, das Bilder versteht."),
            AiTask::TextEditing => tr("Umformulieren und Korrigieren im Editor, Artikelbewertung, Tag-Vorschläge."),
            AiTask::TextGeneration => tr("KI-Artikelentwurf aus Thema und Anweisungen."),
        }
    }
}

/// One concrete model at one provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRef {
    pub provider: Provider,
    pub model: String,
}

impl ModelRef {
    /// `provider:model` - how an assignment is stored. Model ids can
    /// contain `:` themselves (Ollama's `llama3.2:latest`), so only the
    /// first one separates.
    pub fn id(&self) -> String {
        format!("{}:{}", self.provider.id(), self.model)
    }

    pub fn parse(s: &str) -> Option<Self> {
        let (provider, model) = s.split_once(':')?;
        if model.trim().is_empty() || !Provider::ALL.iter().any(|p| p.id() == provider) {
            return None;
        }
        Some(Self { provider: Provider::from_id(provider), model: model.to_string() })
    }

    pub fn label(&self) -> String {
        format!("{} · {}", self.provider.label(), self.model)
    }

    /// The KI-Chat settings' active provider and model - what a task
    /// without its own primary model uses.
    pub fn chat_default(config: &ProviderConfig) -> Self {
        Self { provider: config.active, model: config.model_for(config.active).to_string() }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskAssignment {
    /// `None` = follow the KI-Chat settings.
    pub primary: Option<ModelRef>,
    pub fallback: Option<ModelRef>,
}

fn tasks_path() -> PathBuf {
    let mut path = glib::user_config_dir();
    path.push("blocksmith");
    path.push("ai_tasks.json");
    path
}

fn load_root() -> Value {
    std::fs::read_to_string(tasks_path()).ok().and_then(|c| serde_json::from_str(&c).ok()).unwrap_or_else(|| serde_json::json!({}))
}

pub fn load_assignment(task: AiTask) -> TaskAssignment {
    let root = load_root();
    let slot = |name: &str| root.pointer(&format!("/{}/{name}", task.id())).and_then(Value::as_str).and_then(ModelRef::parse);
    TaskAssignment { primary: slot("primary"), fallback: slot("fallback") }
}

pub fn save_assignment(task: AiTask, assignment: &TaskAssignment) -> std::io::Result<()> {
    let mut root = load_root();
    if !root.is_object() {
        root = serde_json::json!({});
    }
    root[task.id()] = serde_json::json!({
        "primary": assignment.primary.as_ref().map(ModelRef::id),
        "fallback": assignment.fallback.as_ref().map(ModelRef::id),
    });
    let path = tasks_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, root.to_string())
}

/// The models to try, in order: the primary (or the chat default), then
/// the fallback unless it's the same model.
pub fn candidates(assignment: &TaskAssignment, chat: &ProviderConfig) -> Vec<ModelRef> {
    let mut out = vec![assignment.primary.clone().unwrap_or_else(|| ModelRef::chat_default(chat))];
    if let Some(fallback) = &assignment.fallback {
        if !out.contains(fallback) {
            out.push(fallback.clone());
        }
    }
    out
}

/// A task's result plus, if the primary model had to be skipped, the notice
/// saying so.
#[derive(Debug)]
pub struct Routed<T> {
    pub value: T,
    pub notice: Option<String>,
}

fn switch_notice(skipped: &[(ModelRef, String)], used: &ModelRef) -> Option<String> {
    let (first, reason) = skipped.first()?;
    Some(
        tr("„{model}“ nicht nutzbar ({reason}) - stattdessen „{fallback}“ verwendet.")
            .replace("{model}", &first.label())
            .replace("{reason}", reason)
            .replace("{fallback}", &used.label()),
    )
}

/// Runs `call` against the task's models with automatic fallback - see the
/// module docs. Blocking (loads keys, makes the request): call it from the
/// worker thread, then hand the result to `deliver` on the main loop.
pub fn run<T>(task: AiTask, call: impl Fn(&llm::Client) -> llm::Result<T>) -> Result<Routed<T>, String> {
    let chat = chatconfig::load_provider_config();
    let cache = modelcheck::load();
    let now = modelcheck::now();

    let mut skipped: Vec<(ModelRef, String)> = Vec::new();
    // Candidates skipped purely on the cache's word - retried as a last
    // resort if nothing else was even attempted, so a stale "blocked"
    // verdict can never lock a task out entirely.
    let mut cache_blocked: Vec<(ModelRef, String, String)> = Vec::new();
    let mut last_error: Option<String> = None;
    let mut attempted = false;

    let attempt = |candidate: &ModelRef, key: &str, fingerprint: &str, skipped: &mut Vec<(ModelRef, String)>, last_error: &mut Option<String>| -> Option<Result<T, String>> {
        let client = llm::Client::new(candidate.provider, key, &candidate.model, &chat.ollama_base_url);
        match call(&client) {
            Ok(value) => {
                modelcheck::remember(candidate.provider, fingerprint, &candidate.model, ModelStatus::Available, "");
                Some(Ok(value))
            }
            Err(err) if err.status.warrants_fallback() => {
                modelcheck::remember(candidate.provider, fingerprint, &candidate.model, err.status, &err.message);
                skipped.push((candidate.clone(), err.status.label()));
                *last_error = Some(err.message);
                None
            }
            Err(err) => Some(Err(err.message)),
        }
    };

    for candidate in candidates(&load_assignment(task), &chat) {
        let key = if candidate.provider.needs_api_key() {
            match futures_lite::future::block_on(secrets::load_llm_api_key(candidate.provider.id())) {
                Ok(Some(key)) => key,
                Ok(None) => {
                    skipped.push((candidate.clone(), tr("kein API-Key hinterlegt")));
                    continue;
                }
                Err(err) => {
                    skipped.push((candidate.clone(), err.to_string()));
                    continue;
                }
            }
        } else {
            String::new()
        };
        let fingerprint = modelcheck::fingerprint(candidate.provider, &key, &chat.ollama_base_url);
        let cached = cache.status(candidate.provider, &fingerprint, &candidate.model, now);
        if cached.is_permanent_block() {
            skipped.push((candidate.clone(), cached.label()));
            cache_blocked.push((candidate, key, fingerprint));
            continue;
        }
        attempted = true;
        if let Some(result) = attempt(&candidate, &key, &fingerprint, &mut skipped, &mut last_error) {
            return result.map(|value| Routed { value, notice: switch_notice(&skipped, &candidate) });
        }
    }

    if !attempted {
        if let Some((candidate, key, fingerprint)) = cache_blocked.into_iter().next() {
            skipped.retain(|(model, _)| *model != candidate);
            if let Some(result) = attempt(&candidate, &key, &fingerprint, &mut skipped, &mut last_error) {
                return result.map(|value| Routed { value, notice: switch_notice(&skipped, &candidate) });
            }
        }
    }

    let tried: Vec<String> = skipped.iter().map(|(model, reason)| format!("„{}“ ({reason})", model.label())).collect();
    let mut message = tr("Kein nutzbares Modell für „{task}“: {models}.").replace("{task}", &task.label()).replace("{models}", &tried.join(", "));
    if let Some(detail) = last_error {
        message.push_str(&format!("\n{detail}"));
    }
    message.push_str(&format!("\n{}", tr("Modelle je Aufgabe lassen sich unter Einstellungen → KI-Modelle festlegen.")));
    Err(message)
}

thread_local! {
    static TOAST_OVERLAY: RefCell<Option<adw::ToastOverlay>> = const { RefCell::new(None) };
}

/// Registers the main window's toast overlay, where `deliver` shows a
/// model-switch notice.
pub fn set_toast_overlay(overlay: &adw::ToastOverlay) {
    TOAST_OVERLAY.with(|slot| *slot.borrow_mut() = Some(overlay.clone()));
}

/// Main-loop side of `run`: shows the switch notice (if any) as a toast and
/// unwraps the value.
pub fn deliver<T>(outcome: Result<Routed<T>, String>) -> Result<T, String> {
    outcome.map(|routed| {
        if let Some(notice) = &routed.notice {
            TOAST_OVERLAY.with(|slot| {
                if let Some(overlay) = slot.borrow().as_ref() {
                    overlay.add_toast(adw::Toast::builder().title(notice.as_str()).timeout(8).build());
                }
            });
        }
        routed.value
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(provider: Provider, id: &str) -> ModelRef {
        ModelRef { provider, model: id.to_string() }
    }

    #[test]
    fn model_ref_round_trips_and_keeps_colons_in_the_model_id() {
        let ollama = model(Provider::Ollama, "llama3.2:latest");
        assert_eq!(ModelRef::parse(&ollama.id()), Some(ollama));
        assert_eq!(ModelRef::parse("gemini:gemini-2.5-flash"), Some(model(Provider::Gemini, "gemini-2.5-flash")));
    }

    #[test]
    fn model_ref_rejects_garbage() {
        assert_eq!(ModelRef::parse("nonsense"), None);
        assert_eq!(ModelRef::parse("unknown:model"), None);
        assert_eq!(ModelRef::parse("gemini:"), None);
    }

    #[test]
    fn an_unassigned_task_follows_the_chat_settings() {
        let chat = ProviderConfig::default();
        assert_eq!(candidates(&TaskAssignment::default(), &chat), vec![ModelRef::chat_default(&chat)]);
    }

    #[test]
    fn primary_comes_before_fallback_and_a_duplicate_fallback_is_dropped() {
        let chat = ProviderConfig::default();
        let primary = model(Provider::Gemini, "gemini-2.5-pro");
        let fallback = model(Provider::Gemini, "gemini-2.5-flash");
        let assignment = TaskAssignment { primary: Some(primary.clone()), fallback: Some(fallback.clone()) };
        assert_eq!(candidates(&assignment, &chat), vec![primary.clone(), fallback]);

        let same = TaskAssignment { primary: Some(primary.clone()), fallback: Some(primary.clone()) };
        assert_eq!(candidates(&same, &chat), vec![primary]);
    }

    #[test]
    fn switch_notice_names_the_skipped_model_its_reason_and_the_replacement() {
        let skipped = vec![(model(Provider::Gemini, "gemini-2.5-pro"), "Abo/Guthaben erforderlich".to_string())];
        let notice = switch_notice(&skipped, &model(Provider::Gemini, "gemini-2.5-flash")).unwrap();
        assert!(notice.contains("gemini-2.5-pro") && notice.contains("Abo/Guthaben erforderlich") && notice.contains("gemini-2.5-flash"), "{notice}");
        assert_eq!(switch_notice(&[], &model(Provider::Gemini, "x")), None);
    }

    #[test]
    fn assignment_save_load_round_trips() {
        let original = load_assignment(AiTask::TextGeneration);
        let edited = TaskAssignment { primary: Some(model(Provider::Claude, "claude-sonnet-5")), fallback: Some(model(Provider::Ollama, "llama3.2:latest")) };
        save_assignment(AiTask::TextGeneration, &edited).expect("save failed");
        assert_eq!(load_assignment(AiTask::TextGeneration), edited);
        save_assignment(AiTask::TextGeneration, &original).expect("restore failed");
    }
}
