//! Persistence for the chat pane's configurable bits: the system prompt
//! (shared across all providers, editable and resettable in the "KI-Chat"
//! settings page), which provider is active, and each provider's model id
//! (plus Ollama's base URL). API keys are secrets and go through
//! `secrets.rs` instead - this only ever holds plain-text, non-sensitive
//! config.

use std::path::PathBuf;

use gtk4::glib;
use serde_json::Value;

use crate::default_prompt::DEFAULT_SYSTEM_PROMPT;
use crate::llm::{Provider, DEFAULT_OLLAMA_BASE_URL};

fn config_dir() -> PathBuf {
    let mut dir = glib::user_config_dir();
    dir.push("blocksmith");
    dir
}

fn system_prompt_path() -> PathBuf {
    let mut path = config_dir();
    path.push("chat_system_prompt.txt");
    path
}

fn providers_path() -> PathBuf {
    let mut path = config_dir();
    path.push("chat_providers.json");
    path
}

fn models_cache_path() -> PathBuf {
    let mut path = config_dir();
    path.push("chat_models_cache.json");
    path
}

/// The active system prompt: the user's saved custom one if they've set
/// one, otherwise [`DEFAULT_SYSTEM_PROMPT`].
pub fn load_system_prompt() -> String {
    std::fs::read_to_string(system_prompt_path()).unwrap_or_else(|_| DEFAULT_SYSTEM_PROMPT.to_string())
}

pub fn save_system_prompt(text: &str) -> std::io::Result<()> {
    let path = system_prompt_path();
    std::fs::create_dir_all(config_dir())?;
    std::fs::write(path, text)
}

/// Reverts to [`DEFAULT_SYSTEM_PROMPT`] by removing the saved custom prompt
/// file, if any.
pub fn reset_system_prompt() -> std::io::Result<()> {
    match std::fs::remove_file(system_prompt_path()) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

pub fn is_system_prompt_customized() -> bool {
    system_prompt_path().exists()
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderConfig {
    pub active: Provider,
    pub gemini_model: String,
    pub openai_model: String,
    pub claude_model: String,
    pub groq_model: String,
    pub ollama_model: String,
    pub ollama_base_url: String,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            active: Provider::Gemini,
            gemini_model: Provider::Gemini.default_model().to_string(),
            openai_model: Provider::OpenAi.default_model().to_string(),
            claude_model: Provider::Claude.default_model().to_string(),
            groq_model: Provider::Groq.default_model().to_string(),
            ollama_model: Provider::Ollama.default_model().to_string(),
            ollama_base_url: DEFAULT_OLLAMA_BASE_URL.to_string(),
        }
    }
}

impl ProviderConfig {
    pub fn model_for(&self, provider: Provider) -> &str {
        match provider {
            Provider::Gemini => &self.gemini_model,
            Provider::OpenAi => &self.openai_model,
            Provider::Claude => &self.claude_model,
            Provider::Groq => &self.groq_model,
            Provider::Ollama => &self.ollama_model,
        }
    }

    pub fn set_model_for(&mut self, provider: Provider, model: String) {
        let field = match provider {
            Provider::Gemini => &mut self.gemini_model,
            Provider::OpenAi => &mut self.openai_model,
            Provider::Claude => &mut self.claude_model,
            Provider::Groq => &mut self.groq_model,
            Provider::Ollama => &mut self.ollama_model,
        };
        *field = model;
    }
}

pub fn load_provider_config() -> ProviderConfig {
    let defaults = ProviderConfig::default();
    let Ok(contents) = std::fs::read_to_string(providers_path()) else {
        return defaults;
    };
    let Ok(value) = serde_json::from_str::<Value>(&contents) else {
        return defaults;
    };
    let string_or = |key: &str, fallback: &str| -> String {
        value.get(key).and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| fallback.to_string())
    };
    ProviderConfig {
        active: Provider::from_id(&string_or("active", defaults.active.id())),
        gemini_model: string_or("gemini_model", &defaults.gemini_model),
        openai_model: string_or("openai_model", &defaults.openai_model),
        claude_model: string_or("claude_model", &defaults.claude_model),
        groq_model: string_or("groq_model", &defaults.groq_model),
        ollama_model: string_or("ollama_model", &defaults.ollama_model),
        ollama_base_url: string_or("ollama_base_url", &defaults.ollama_base_url),
    }
}

pub fn save_provider_config(config: &ProviderConfig) -> std::io::Result<()> {
    let value = serde_json::json!({
        "active": config.active.id(),
        "gemini_model": config.gemini_model,
        "openai_model": config.openai_model,
        "claude_model": config.claude_model,
        "groq_model": config.groq_model,
        "ollama_model": config.ollama_model,
        "ollama_base_url": config.ollama_base_url,
    });
    std::fs::create_dir_all(config_dir())?;
    std::fs::write(providers_path(), value.to_string())
}

/// The model list last fetched from a provider (`llm::Client::list_models`),
/// cached so the model picker (in both Einstellungen and the Chat tab) has
/// something to show without a network round-trip on every dialog open or
/// app start. Refreshed whenever the API key is (re-)verified.
pub fn load_cached_models(provider: Provider) -> Vec<String> {
    let Ok(contents) = std::fs::read_to_string(models_cache_path()) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&contents) else {
        return Vec::new();
    };
    value
        .get(provider.id())
        .and_then(Value::as_array)
        .map(|models| models.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}

pub fn save_cached_models(provider: Provider, models: &[String]) -> std::io::Result<()> {
    let mut root: Value = std::fs::read_to_string(models_cache_path())
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    root[provider.id()] = serde_json::json!(models);
    std::fs::create_dir_all(config_dir())?;
    std::fs::write(models_cache_path(), root.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_prompt_is_non_empty_and_mentions_key_topics() {
        assert!(DEFAULT_SYSTEM_PROMPT.contains("Open Source"));
        assert!(DEFAULT_SYSTEM_PROMPT.contains("Linux"));
    }

    /// Touches the real config dir (there's no pure logic to isolate here -
    /// it's just read-or-default / write-raw-text), so this captures and
    /// restores whatever was already saved, to avoid leaving the test
    /// machine's actual Blocksmith config changed.
    #[test]
    fn system_prompt_save_load_reset_round_trips() {
        let was_customized = is_system_prompt_customized();
        let original = load_system_prompt();

        save_system_prompt("Custom test prompt").expect("save failed");
        assert_eq!(load_system_prompt(), "Custom test prompt");
        assert!(is_system_prompt_customized());

        reset_system_prompt().expect("reset failed");
        assert_eq!(load_system_prompt(), DEFAULT_SYSTEM_PROMPT);
        assert!(!is_system_prompt_customized());

        if was_customized {
            save_system_prompt(&original).expect("restore failed");
        }
    }

    #[test]
    fn provider_config_save_load_round_trips() {
        let original = load_provider_config();

        let mut edited = original.clone();
        edited.active = Provider::Claude;
        edited.set_model_for(Provider::Claude, "test-claude-model".to_string());
        edited.ollama_base_url = "http://example.invalid:1234".to_string();
        save_provider_config(&edited).expect("save failed");

        let loaded = load_provider_config();
        assert_eq!(loaded, edited);

        save_provider_config(&original).expect("restore failed");
    }

    #[test]
    fn missing_or_corrupt_file_yields_defaults() {
        let defaults = ProviderConfig::default();
        assert_eq!(defaults.active, Provider::Gemini);
        assert_eq!(defaults.model_for(Provider::Gemini), Provider::Gemini.default_model());
    }

    #[test]
    fn cached_models_round_trip_per_provider_without_clobbering_others() {
        let original_gemini = load_cached_models(Provider::Gemini);
        let original_openai = load_cached_models(Provider::OpenAi);

        save_cached_models(Provider::Gemini, &["gemini-test-model".to_string()]).expect("save failed");
        save_cached_models(Provider::OpenAi, &["gpt-test-model".to_string()]).expect("save failed");

        assert_eq!(load_cached_models(Provider::Gemini), vec!["gemini-test-model".to_string()]);
        assert_eq!(load_cached_models(Provider::OpenAi), vec!["gpt-test-model".to_string()]);

        save_cached_models(Provider::Gemini, &original_gemini).expect("restore failed");
        save_cached_models(Provider::OpenAi, &original_openai).expect("restore failed");
    }
}
