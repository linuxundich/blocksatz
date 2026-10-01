//! The capability check: which of the models a provider *lists* for a key
//! can that key actually *use*? A models list only says what exists -
//! Gemini's free tier lists Pro models it grants no quota for, OpenAI lists
//! models an account without credit can't call. So each model gets a
//! minimal dry-run request (`llm::Client::probe`), and the outcome, as
//! classified by `llm::classify`'s error matrix, is cached here per model.
//!
//! The cache is what keeps this cheap: the settings page and the task
//! routing (`aitasks.rs`) read it instead of probing again, and the routing
//! writes back whatever a real call just revealed. Entries expire by kind
//! (`ttl`) - a rate limit after an hour, "needs payment" after a week - and
//! are tied to a fingerprint of the key (or Ollama's base URL) they were
//! checked with, so entering a different key starts from scratch instead
//! of inheriting another account's verdicts.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gtk4::glib;
use serde_json::Value;
use sha2::Digest;

use crate::llm::{self, ModelStatus, Provider};

/// Serializes the load-modify-save cycle - the settings page's check and a
/// task's routing can both write from their own worker threads.
static FILE_LOCK: Mutex<()> = Mutex::new(());

/// Pause between two probes of one check run, so checking a long model
/// list doesn't itself trip a provider's requests-per-minute limit.
const PROBE_SPACING: Duration = Duration::from_millis(400);

fn cache_path() -> PathBuf {
    let mut path = glib::user_config_dir();
    path.push(crate::APP_DIR);
    path.push("model_status.json");
    path
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Identifies which key (or, for Ollama, which server) a cached verdict
/// belongs to without storing the key itself - a truncated SHA-256 is
/// plenty to tell two keys apart and useless for recovering either.
pub fn fingerprint(provider: Provider, api_key: &str, base_url: &str) -> String {
    let material = if provider.needs_api_key() { api_key } else { base_url };
    sha2::Sha256::digest(material.trim().as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// How long a verdict stays trustworthy - `None` for outcomes that are
/// never cached because they say nothing lasting about the model.
pub fn ttl(status: ModelStatus) -> Option<u64> {
    const HOUR: u64 = 60 * 60;
    match status {
        ModelStatus::Available => Some(3 * 24 * HOUR),
        ModelStatus::PaymentRequired | ModelStatus::NoAccess | ModelStatus::RegionBlocked | ModelStatus::NotFound | ModelStatus::InvalidKey => Some(7 * 24 * HOUR),
        ModelStatus::RateLimited => Some(HOUR),
        ModelStatus::ServerError => Some(15 * 60),
        ModelStatus::Unchecked | ModelStatus::Unsupported | ModelStatus::Other => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub status: ModelStatus,
    pub checked_at: u64,
    /// The provider's own error text, shown as a tooltip.
    pub detail: String,
}

impl Entry {
    /// `Unchecked` once the entry has outlived its `ttl`.
    pub fn effective_status(&self, now: u64) -> ModelStatus {
        match ttl(self.status) {
            Some(ttl) if now.saturating_sub(self.checked_at) < ttl => self.status,
            _ => ModelStatus::Unchecked,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
struct ProviderEntries {
    fingerprint: String,
    models: HashMap<String, Entry>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Cache {
    providers: HashMap<String, ProviderEntries>,
}

impl Cache {
    /// The cached entry for `model`, if it was checked with the same key.
    pub fn entry(&self, provider: Provider, fingerprint: &str, model: &str) -> Option<&Entry> {
        self.providers.get(provider.id()).filter(|p| p.fingerprint == fingerprint).and_then(|p| p.models.get(model))
    }

    pub fn status(&self, provider: Provider, fingerprint: &str, model: &str, now: u64) -> ModelStatus {
        self.entry(provider, fingerprint, model).map(|e| e.effective_status(now)).unwrap_or(ModelStatus::Unchecked)
    }

    /// The status for whichever key this provider was last checked with -
    /// for display where the key isn't at hand (the settings page's model
    /// pickers), since a key change clears the provider's entries anyway.
    pub fn latest_status(&self, provider: Provider, model: &str, now: u64) -> ModelStatus {
        self.providers.get(provider.id()).and_then(|p| p.models.get(model)).map(|e| e.effective_status(now)).unwrap_or(ModelStatus::Unchecked)
    }

    pub fn latest_entry(&self, provider: Provider, model: &str) -> Option<&Entry> {
        self.providers.get(provider.id()).and_then(|p| p.models.get(model))
    }

    /// Stores a verdict; a different `fingerprint` than the stored one
    /// first drops every entry checked with the old key. Uncacheable
    /// outcomes (`ttl` of `None`) leave the cache untouched.
    pub fn record(&mut self, provider: Provider, fingerprint: &str, model: &str, status: ModelStatus, detail: &str, now: u64) {
        if ttl(status).is_none() {
            return;
        }
        let entries = self.providers.entry(provider.id().to_string()).or_default();
        if entries.fingerprint != fingerprint {
            *entries = ProviderEntries { fingerprint: fingerprint.to_string(), models: HashMap::new() };
        }
        entries.models.insert(model.to_string(), Entry { status, checked_at: now, detail: detail.to_string() });
    }

    fn to_json(&self) -> Value {
        let mut root = serde_json::Map::new();
        for (provider, entries) in &self.providers {
            let models: serde_json::Map<String, Value> = entries
                .models
                .iter()
                .map(|(model, e)| (model.clone(), serde_json::json!({ "status": e.status.id(), "checked_at": e.checked_at, "detail": e.detail })))
                .collect();
            root.insert(provider.clone(), serde_json::json!({ "fingerprint": entries.fingerprint, "models": models }));
        }
        Value::Object(root)
    }

    fn from_json(value: &Value) -> Self {
        let mut cache = Cache::default();
        let Some(root) = value.as_object() else { return cache };
        for (provider, entries) in root {
            let fingerprint = entries.get("fingerprint").and_then(Value::as_str).unwrap_or_default().to_string();
            let models = entries
                .get("models")
                .and_then(Value::as_object)
                .map(|models| {
                    models
                        .iter()
                        .map(|(model, e)| {
                            (
                                model.clone(),
                                Entry {
                                    status: ModelStatus::from_id(e.get("status").and_then(Value::as_str).unwrap_or_default()),
                                    checked_at: e.get("checked_at").and_then(Value::as_u64).unwrap_or(0),
                                    detail: e.get("detail").and_then(Value::as_str).unwrap_or_default().to_string(),
                                },
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            cache.providers.insert(provider.clone(), ProviderEntries { fingerprint, models });
        }
        cache
    }
}

pub fn load() -> Cache {
    std::fs::read_to_string(cache_path())
        .ok()
        .and_then(|contents| serde_json::from_str::<Value>(&contents).ok())
        .map(|value| Cache::from_json(&value))
        .unwrap_or_default()
}

/// Records one verdict straight into the cache file.
pub fn remember(provider: Provider, fingerprint: &str, model: &str, status: ModelStatus, detail: &str) {
    if ttl(status).is_none() {
        return;
    }
    let _guard = FILE_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut cache = load();
    cache.record(provider, fingerprint, model, status, detail, now());
    let path = cache_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, cache.to_json().to_string());
}

/// One model's result from a `check_models` run.
#[derive(Debug, Clone)]
pub struct Probed {
    pub model: String,
    pub status: ModelStatus,
    pub detail: String,
}

/// Dry-runs every model in `models` one after another (blocking - run it
/// on a worker thread), caching and reporting each verdict through
/// `on_result` as it arrives. Ollama models are local and free, so they're
/// taken as usable without a probe, which would load each one into memory.
/// Stops early on an invalid key, since every further probe would only
/// fail the same way.
pub fn check_models(provider: Provider, api_key: &str, base_url: &str, models: &[String], on_result: impl Fn(Probed)) {
    let fp = fingerprint(provider, api_key, base_url);
    for (index, model) in models.iter().enumerate() {
        let (status, detail) = if provider == Provider::Ollama {
            (ModelStatus::Available, String::new())
        } else {
            if index > 0 {
                std::thread::sleep(PROBE_SPACING);
            }
            match llm::Client::new(provider, api_key, model, base_url).probe() {
                Ok(()) => (ModelStatus::Available, String::new()),
                Err(err) => (err.status, err.message),
            }
        };
        remember(provider, &fp, model, status, &detail);
        let stop = status == ModelStatus::InvalidKey;
        on_result(Probed { model: model.clone(), status, detail });
        if stop {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verdict_expires_after_its_ttl() {
        let entry = Entry { status: ModelStatus::RateLimited, checked_at: 1_000, detail: String::new() };
        assert_eq!(entry.effective_status(1_000 + 60), ModelStatus::RateLimited);
        assert_eq!(entry.effective_status(1_000 + 2 * 60 * 60), ModelStatus::Unchecked);
    }

    #[test]
    fn a_payment_verdict_outlives_a_rate_limit() {
        assert!(ttl(ModelStatus::PaymentRequired) > ttl(ModelStatus::RateLimited));
    }

    #[test]
    fn a_different_key_starts_from_scratch() {
        let mut cache = Cache::default();
        cache.record(Provider::Gemini, "key-a", "gemini-2.5-pro", ModelStatus::PaymentRequired, "", 100);
        assert_eq!(cache.status(Provider::Gemini, "key-a", "gemini-2.5-pro", 200), ModelStatus::PaymentRequired);
        assert_eq!(cache.status(Provider::Gemini, "key-b", "gemini-2.5-pro", 200), ModelStatus::Unchecked);

        cache.record(Provider::Gemini, "key-b", "gemini-2.5-flash", ModelStatus::Available, "", 300);
        assert_eq!(cache.status(Provider::Gemini, "key-a", "gemini-2.5-pro", 400), ModelStatus::Unchecked, "the old key's verdicts are dropped");
        assert_eq!(cache.status(Provider::Gemini, "key-b", "gemini-2.5-flash", 400), ModelStatus::Available);
    }

    #[test]
    fn uncacheable_outcomes_are_not_recorded() {
        let mut cache = Cache::default();
        cache.record(Provider::OpenAi, "k", "gpt-4o", ModelStatus::Other, "", 1);
        cache.record(Provider::OpenAi, "k", "gpt-4o", ModelStatus::Unsupported, "", 1);
        assert_eq!(cache, Cache::default());
    }

    #[test]
    fn cache_round_trips_through_json() {
        let mut cache = Cache::default();
        cache.record(Provider::Claude, "fp", "claude-sonnet-5", ModelStatus::Available, "", 42);
        cache.record(Provider::Gemini, "fp2", "gemini-2.5-pro", ModelStatus::PaymentRequired, "HTTP 429: limit: 0", 43);
        assert_eq!(Cache::from_json(&cache.to_json()), cache);
    }

    #[test]
    fn fingerprint_tells_keys_apart_without_containing_them() {
        let a = fingerprint(Provider::Gemini, "secret-key-a", "");
        let b = fingerprint(Provider::Gemini, "secret-key-b", "");
        assert_ne!(a, b);
        assert!(!a.contains("secret"));
        assert_ne!(fingerprint(Provider::Ollama, "", "http://a:11434"), fingerprint(Provider::Ollama, "", "http://b:11434"));
    }
}
