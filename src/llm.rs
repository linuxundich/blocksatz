//! Blocking REST clients for the chat pane's five supported providers -
//! Gemini, ChatGPT (OpenAI), Claude (Anthropic), Groq (OpenAI-compatible
//! API, so it shares the ChatGPT request code with a different base URL),
//! and Ollama (self-hosted, no API key). Blocking for the same reason as `wpclient` - see its
//! module docs: this app already committed to `oo7`'s async-std reactor
//! for keyring access, so a blocking client run on a spawned thread is
//! simpler than reconciling two async runtimes for one occasional call.

use std::time::Duration;

use base64::Engine;
use serde_json::Value;

use crate::i18n::tr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Gemini,
    OpenAi,
    Claude,
    Groq,
    Ollama,
}

impl Provider {
    pub const ALL: [Provider; 5] = [Provider::Gemini, Provider::OpenAi, Provider::Claude, Provider::Groq, Provider::Ollama];

    /// Stable identifier used in config files and keyring attributes - not
    /// shown to the user (see `label` for that).
    pub fn id(&self) -> &'static str {
        match self {
            Provider::Gemini => "gemini",
            Provider::OpenAi => "openai",
            Provider::Claude => "claude",
            Provider::Groq => "groq",
            Provider::Ollama => "ollama",
        }
    }

    pub fn from_id(s: &str) -> Self {
        match s.trim() {
            "openai" => Provider::OpenAi,
            "claude" => Provider::Claude,
            "groq" => Provider::Groq,
            "ollama" => Provider::Ollama,
            _ => Provider::Gemini,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Provider::Gemini => "Gemini",
            Provider::OpenAi => "ChatGPT",
            Provider::Claude => "Claude",
            Provider::Groq => "Groq",
            Provider::Ollama => "Ollama",
        }
    }

    pub fn default_model(&self) -> &'static str {
        match self {
            Provider::Gemini => "gemini-2.5-flash",
            Provider::OpenAi => "gpt-4o-mini",
            Provider::Claude => "claude-sonnet-5",
            Provider::Groq => "llama-3.3-70b-versatile",
            Provider::Ollama => "llama3.2",
        }
    }

    /// Base URL of an OpenAI-compatible provider's API (`/chat/completions`,
    /// `/models` below it) - `None` for the providers with their own API
    /// shape.
    fn openai_compatible_base(&self) -> Option<&'static str> {
        match self {
            Provider::OpenAi => Some("https://api.openai.com/v1"),
            Provider::Groq => Some("https://api.groq.com/openai/v1"),
            Provider::Gemini | Provider::Claude | Provider::Ollama => None,
        }
    }

    /// Ollama runs locally with no account, so it has no API key to enter.
    pub fn needs_api_key(&self) -> bool {
        !matches!(self, Provider::Ollama)
    }

    /// Only Ollama's endpoint is user-configurable (it's self-hosted, often
    /// not on the default port/host); the others have a fixed cloud API.
    pub fn needs_base_url(&self) -> bool {
        matches!(self, Provider::Ollama)
    }
}

pub const DEFAULT_OLLAMA_BASE_URL: &str = "http://localhost:11434";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Model,
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: Role,
    pub text: String,
}

/// What a failed (or successful) call says about whether a model is usable
/// with the current key - the error matrix `classify` maps HTTP statuses
/// and provider-specific error texts onto. Doubles as the per-model status
/// the capability check (`modelcheck.rs`) caches and the task routing
/// (`aitasks.rs`) consults before picking a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelStatus {
    /// Never checked, or the last check has expired.
    Unchecked,
    Available,
    /// Temporarily out of requests (per-minute/per-day limit) - worth
    /// retrying later, not a reason to hide the model.
    RateLimited,
    /// Needs a paid plan, billing or credit: HTTP 402, OpenAI's
    /// `insufficient_quota`, Anthropic's "credit balance is too low", or a
    /// Gemini free-tier quota of `limit: 0` (models the free tier doesn't
    /// include at all).
    PaymentRequired,
    /// 403 - the key/project isn't allowed to use this model.
    NoAccess,
    /// Not offered in the account's country/region.
    RegionBlocked,
    /// 404 - unknown, retired or not usable for generation.
    NotFound,
    /// 401 (or Gemini's 400 "API key not valid") - a key problem, not a
    /// model problem.
    InvalidKey,
    /// The model rejected the kind of request (e.g. an image sent to a
    /// text-only model) - specific to the task, so never cached.
    Unsupported,
    /// 5xx, overloaded, or unreachable - temporary.
    ServerError,
    /// Any other failure (malformed reply, safety filter, bad request) -
    /// says nothing about the model's availability.
    Other,
}

impl ModelStatus {
    pub const ALL: [ModelStatus; 11] = [
        ModelStatus::Unchecked,
        ModelStatus::Available,
        ModelStatus::RateLimited,
        ModelStatus::PaymentRequired,
        ModelStatus::NoAccess,
        ModelStatus::RegionBlocked,
        ModelStatus::NotFound,
        ModelStatus::InvalidKey,
        ModelStatus::Unsupported,
        ModelStatus::ServerError,
        ModelStatus::Other,
    ];

    pub fn id(&self) -> &'static str {
        match self {
            ModelStatus::Unchecked => "unchecked",
            ModelStatus::Available => "available",
            ModelStatus::RateLimited => "rate_limited",
            ModelStatus::PaymentRequired => "payment_required",
            ModelStatus::NoAccess => "no_access",
            ModelStatus::RegionBlocked => "region_blocked",
            ModelStatus::NotFound => "not_found",
            ModelStatus::InvalidKey => "invalid_key",
            ModelStatus::Unsupported => "unsupported",
            ModelStatus::ServerError => "server_error",
            ModelStatus::Other => "other",
        }
    }

    pub fn from_id(s: &str) -> Self {
        ModelStatus::ALL.into_iter().find(|status| status.id() == s).unwrap_or(ModelStatus::Unchecked)
    }

    /// Short UI label ("Free-Tier nutzbar", "Abo/Guthaben nötig", ...).
    pub fn label(&self) -> String {
        match self {
            ModelStatus::Unchecked => tr("Nicht geprüft"),
            ModelStatus::Available => tr("Nutzbar"),
            ModelStatus::RateLimited => tr("Kontingent vorübergehend erschöpft"),
            ModelStatus::PaymentRequired => tr("Abo/Guthaben erforderlich"),
            ModelStatus::NoAccess => tr("Kein Zugriff mit diesem Key"),
            ModelStatus::RegionBlocked => tr("In deiner Region nicht verfügbar"),
            ModelStatus::NotFound => tr("Nicht (mehr) verfügbar"),
            ModelStatus::InvalidKey => tr("API-Key ungültig"),
            ModelStatus::Unsupported => tr("Für diese Anfrage nicht geeignet"),
            ModelStatus::ServerError => tr("Anbieter gerade nicht erreichbar"),
            ModelStatus::Other => tr("Fehler"),
        }
    }

    /// Won't start working by just waiting - the model picker hides these
    /// and the task routing skips them without even trying.
    pub fn is_permanent_block(&self) -> bool {
        matches!(self, ModelStatus::PaymentRequired | ModelStatus::NoAccess | ModelStatus::RegionBlocked | ModelStatus::NotFound | ModelStatus::InvalidKey)
    }

    /// Whether a call failing this way is a reason to try the task's
    /// fallback model - everything that's about the model or the account,
    /// not about the request's content.
    pub fn warrants_fallback(&self) -> bool {
        !matches!(self, ModelStatus::Available | ModelStatus::Unchecked | ModelStatus::Other)
    }
}

#[derive(Debug)]
pub struct ApiError {
    pub message: String,
    pub status: ModelStatus,
}

impl ApiError {
    fn other(message: String) -> Self {
        Self { message, status: ModelStatus::Other }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ApiError {}

pub type Result<T> = std::result::Result<T, ApiError>;

pub struct Client {
    provider: Provider,
    agent: ureq::Agent,
    api_key: String,
    model: String,
    base_url: String,
}

impl Client {
    pub fn new(provider: Provider, api_key: &str, model: &str, base_url: &str) -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(60)))
            .build();
        Self {
            provider,
            agent: ureq::Agent::new_with_config(config),
            api_key: api_key.to_string(),
            model: model.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// Sends the full conversation history (the last element being the new
    /// user message) plus a system prompt, returning the model's reply text.
    pub fn send(&self, system_prompt: &str, history: &[ChatMessage]) -> Result<String> {
        match self.provider {
            Provider::Gemini => self.send_gemini(system_prompt, history),
            Provider::OpenAi | Provider::Groq => self.send_openai(system_prompt, history),
            Provider::Claude => self.send_claude(system_prompt, history),
            Provider::Ollama => self.send_ollama(system_prompt, history),
        }
    }

    fn send_gemini(&self, system_prompt: &str, history: &[ChatMessage]) -> Result<String> {
        let contents: Vec<Value> = history
            .iter()
            .map(|m| {
                serde_json::json!({
                    "role": match m.role { Role::User => "user", Role::Model => "model" },
                    "parts": [{"text": m.text}],
                })
            })
            .collect();
        let mut body = serde_json::json!({ "contents": contents });
        if !system_prompt.trim().is_empty() {
            body["system_instruction"] = serde_json::json!({ "parts": [{"text": system_prompt}] });
        }

        let url = format!("https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent", self.model);
        let (status, body_text) = self.post_json(&url, &[("x-goog-api-key", &self.api_key)], &body)?;
        if !(200..300).contains(&status) {
            return Err(error_from_body(status, &body_text, &["error", "message"]));
        }
        let value: Value = parse_json(&body_text)?;
        value
            .pointer("/candidates/0/content/parts/0/text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| ApiError::other(tr("Keine Antwort erhalten (möglicherweise durch einen Sicherheitsfilter blockiert).")))
    }

    fn send_openai(&self, system_prompt: &str, history: &[ChatMessage]) -> Result<String> {
        let mut messages = Vec::new();
        if !system_prompt.trim().is_empty() {
            messages.push(serde_json::json!({ "role": "system", "content": system_prompt }));
        }
        for m in history {
            messages.push(serde_json::json!({
                "role": match m.role { Role::User => "user", Role::Model => "assistant" },
                "content": m.text,
            }));
        }
        let body = serde_json::json!({ "model": self.model, "messages": messages });

        let auth = format!("Bearer {}", self.api_key);
        let (status, body_text) = self.post_json(&self.openai_url("chat/completions"), &[("Authorization", &auth)], &body)?;
        if !(200..300).contains(&status) {
            return Err(error_from_body(status, &body_text, &["error", "message"]));
        }
        let value: Value = parse_json(&body_text)?;
        value
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| ApiError::other(tr("Keine Antwort erhalten.")))
    }

    fn send_claude(&self, system_prompt: &str, history: &[ChatMessage]) -> Result<String> {
        let messages: Vec<Value> = history
            .iter()
            .map(|m| {
                serde_json::json!({
                    "role": match m.role { Role::User => "user", Role::Model => "assistant" },
                    "content": m.text,
                })
            })
            .collect();
        let mut body = serde_json::json!({ "model": self.model, "max_tokens": 4096, "messages": messages });
        if !system_prompt.trim().is_empty() {
            body["system"] = serde_json::Value::String(system_prompt.to_string());
        }

        let (status, body_text) = self.post_json(
            "https://api.anthropic.com/v1/messages",
            &[("x-api-key", &self.api_key), ("anthropic-version", "2023-06-01")],
            &body,
        )?;
        if !(200..300).contains(&status) {
            return Err(error_from_body(status, &body_text, &["error", "message"]));
        }
        let value: Value = parse_json(&body_text)?;
        value
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| ApiError::other(tr("Keine Antwort erhalten.")))
    }

    fn send_ollama(&self, system_prompt: &str, history: &[ChatMessage]) -> Result<String> {
        let mut messages = Vec::new();
        if !system_prompt.trim().is_empty() {
            messages.push(serde_json::json!({ "role": "system", "content": system_prompt }));
        }
        for m in history {
            messages.push(serde_json::json!({
                "role": match m.role { Role::User => "user", Role::Model => "assistant" },
                "content": m.text,
            }));
        }
        let body = serde_json::json!({ "model": self.model, "messages": messages, "stream": false });

        let url = format!("{}/api/chat", self.base_url);
        let (status, body_text) = self.post_json(&url, &[], &body)?;
        if !(200..300).contains(&status) {
            return Err(error_from_body(status, &body_text, &["error"]));
        }
        let value: Value = parse_json(&body_text)?;
        if let Some(text) = value.pointer("/message/content").and_then(Value::as_str) {
            return Ok(text.to_string());
        }
        Err(error_from_body(status, &body_text, &["error"]))
    }

    /// Sends a single image plus an instruction prompt and returns the
    /// model's text reply - a one-shot "describe this image" call (used for
    /// AI-generated alt text, see `aialt.rs`), unlike `send`'s multi-turn
    /// conversation history. `image_bytes` goes over the wire as base64,
    /// inlined directly in the request body - all four providers support
    /// this for a single image without needing a separate upload step.
    pub fn describe_image(&self, prompt: &str, image_bytes: &[u8], mime_type: &str) -> Result<String> {
        let data = base64::engine::general_purpose::STANDARD.encode(image_bytes);
        match self.provider {
            Provider::Gemini => self.describe_image_gemini(prompt, mime_type, &data),
            Provider::OpenAi | Provider::Groq => self.describe_image_openai(prompt, mime_type, &data),
            Provider::Claude => self.describe_image_claude(prompt, mime_type, &data),
            Provider::Ollama => self.describe_image_ollama(prompt, &data),
        }
    }

    /// The capability check's dry run: the smallest real generation request
    /// this provider accepts ("ping", capped at a handful of output tokens),
    /// so a model that's listed but not actually usable with this key -
    /// no free tier, no access, retired - shows up as such without spending
    /// more than a few tokens. Only the HTTP outcome matters; the reply
    /// itself (often empty or cut off by the token cap) is ignored.
    pub fn probe(&self) -> std::result::Result<(), ApiError> {
        let auth = format!("Bearer {}", self.api_key);
        let (status, body_text) = match self.provider {
            Provider::Gemini => {
                let url = format!("https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent", self.model);
                self.post_json(&url, &[("x-goog-api-key", &self.api_key)], &probe_body(self.provider, &self.model))?
            }
            Provider::OpenAi | Provider::Groq => self.post_json(&self.openai_url("chat/completions"), &[("Authorization", &auth)], &probe_body(self.provider, &self.model))?,
            Provider::Claude => self.post_json(
                "https://api.anthropic.com/v1/messages",
                &[("x-api-key", &self.api_key), ("anthropic-version", "2023-06-01")],
                &probe_body(self.provider, &self.model),
            )?,
            Provider::Ollama => {
                let url = format!("{}/api/chat", self.base_url);
                self.post_json(&url, &[], &probe_body(self.provider, &self.model))?
            }
        };
        if (200..300).contains(&status) {
            return Ok(());
        }
        let path: &[&str] = if self.provider == Provider::Ollama { &["error"] } else { &["error", "message"] };
        Err(error_from_body(status, &body_text, path))
    }

    fn describe_image_gemini(&self, prompt: &str, mime_type: &str, data: &str) -> Result<String> {
        let body = gemini_image_body(prompt, mime_type, data);
        let url = format!("https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent", self.model);
        let (status, body_text) = self.post_json(&url, &[("x-goog-api-key", &self.api_key)], &body)?;
        if !(200..300).contains(&status) {
            return Err(error_from_body(status, &body_text, &["error", "message"]));
        }
        let value: Value = parse_json(&body_text)?;
        value
            .pointer("/candidates/0/content/parts/0/text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| ApiError::other(tr("Keine Antwort erhalten (möglicherweise durch einen Sicherheitsfilter blockiert).")))
    }

    fn describe_image_openai(&self, prompt: &str, mime_type: &str, data: &str) -> Result<String> {
        let body = openai_image_body(&self.model, prompt, mime_type, data);
        let auth = format!("Bearer {}", self.api_key);
        let (status, body_text) = self.post_json(&self.openai_url("chat/completions"), &[("Authorization", &auth)], &body)?;
        if !(200..300).contains(&status) {
            return Err(error_from_body(status, &body_text, &["error", "message"]));
        }
        let value: Value = parse_json(&body_text)?;
        value
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| ApiError::other(tr("Keine Antwort erhalten.")))
    }

    fn describe_image_claude(&self, prompt: &str, mime_type: &str, data: &str) -> Result<String> {
        let body = claude_image_body(&self.model, prompt, mime_type, data);
        let (status, body_text) = self.post_json(
            "https://api.anthropic.com/v1/messages",
            &[("x-api-key", &self.api_key), ("anthropic-version", "2023-06-01")],
            &body,
        )?;
        if !(200..300).contains(&status) {
            return Err(error_from_body(status, &body_text, &["error", "message"]));
        }
        let value: Value = parse_json(&body_text)?;
        value
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| ApiError::other(tr("Keine Antwort erhalten.")))
    }

    fn describe_image_ollama(&self, prompt: &str, data: &str) -> Result<String> {
        let body = ollama_image_body(&self.model, prompt, data);
        let url = format!("{}/api/chat", self.base_url);
        let (status, body_text) = self.post_json(&url, &[], &body)?;
        if !(200..300).contains(&status) {
            return Err(error_from_body(status, &body_text, &["error"]));
        }
        let value: Value = parse_json(&body_text)?;
        if let Some(text) = value.pointer("/message/content").and_then(Value::as_str) {
            return Ok(text.to_string());
        }
        Err(error_from_body(status, &body_text, &["error"]))
    }

    /// `path` below the OpenAI-compatible base URL of this client's provider.
    fn openai_url(&self, path: &str) -> String {
        format!("{}/{path}", self.provider.openai_compatible_base().unwrap_or("https://api.openai.com/v1"))
    }

    fn post_json(&self, url: &str, headers: &[(&str, &str)], body: &Value) -> Result<(u16, String)> {
        let mut request = self.agent.post(url).header("Content-Type", "application/json");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut response = request.send_json(body).map_err(|err| ApiError { message: err.to_string(), status: ModelStatus::ServerError })?;
        let status = response.status().as_u16();
        let body_text = response.body_mut().read_to_string().unwrap_or_default();
        Ok((status, body_text))
    }

    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<(u16, String)> {
        let mut request = self.agent.get(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut response = request.call().map_err(|err| ApiError { message: err.to_string(), status: ModelStatus::ServerError })?;
        let status = response.status().as_u16();
        let body_text = response.body_mut().read_to_string().unwrap_or_default();
        Ok((status, body_text))
    }

    /// Lists the models available to this account/instance. Doubles as an
    /// API-key check: a successful call (200, non-empty list) means the key
    /// is valid, which is why the settings UI uses this same call both to
    /// populate the model picker and to report "key OK" to the user.
    pub fn list_models(&self) -> Result<Vec<String>> {
        match self.provider {
            Provider::Gemini => self.list_models_gemini(),
            Provider::OpenAi | Provider::Groq => self.list_models_openai(),
            Provider::Claude => self.list_models_claude(),
            Provider::Ollama => self.list_models_ollama(),
        }
    }

    fn list_models_gemini(&self) -> Result<Vec<String>> {
        let url = "https://generativelanguage.googleapis.com/v1beta/models";
        let (status, body_text) = self.get(url, &[("x-goog-api-key", &self.api_key)])?;
        if !(200..300).contains(&status) {
            return Err(error_from_body(status, &body_text, &["error", "message"]));
        }
        Ok(extract_gemini_models(&parse_json(&body_text)?))
    }

    fn list_models_openai(&self) -> Result<Vec<String>> {
        let auth = format!("Bearer {}", self.api_key);
        let (status, body_text) = self.get(&self.openai_url("models"), &[("Authorization", &auth)])?;
        if !(200..300).contains(&status) {
            return Err(error_from_body(status, &body_text, &["error", "message"]));
        }
        Ok(extract_openai_models(&parse_json(&body_text)?))
    }

    fn list_models_claude(&self) -> Result<Vec<String>> {
        let (status, body_text) = self.get(
            "https://api.anthropic.com/v1/models",
            &[("x-api-key", &self.api_key), ("anthropic-version", "2023-06-01")],
        )?;
        if !(200..300).contains(&status) {
            return Err(error_from_body(status, &body_text, &["error", "message"]));
        }
        Ok(extract_claude_models(&parse_json(&body_text)?))
    }

    fn list_models_ollama(&self) -> Result<Vec<String>> {
        let url = format!("{}/api/tags", self.base_url);
        let (status, body_text) = self.get(&url, &[])?;
        if !(200..300).contains(&status) {
            return Err(error_from_body(status, &body_text, &["error"]));
        }
        Ok(extract_ollama_models(&parse_json(&body_text)?))
    }
}

/// Request-body builders for `describe_image_*` - kept as pure functions
/// (not inlined) so their JSON shape can be unit-tested the same way
/// `extract_*_models` is, without needing a live network call.
fn gemini_image_body(prompt: &str, mime_type: &str, data: &str) -> Value {
    serde_json::json!({
        "contents": [{
            "role": "user",
            "parts": [
                {"text": prompt},
                {"inline_data": {"mime_type": mime_type, "data": data}}
            ]
        }]
    })
}

fn openai_image_body(model: &str, prompt: &str, mime_type: &str, data: &str) -> Value {
    serde_json::json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": prompt},
                {"type": "image_url", "image_url": {"url": format!("data:{mime_type};base64,{data}")}}
            ]
        }]
    })
}

fn claude_image_body(model: &str, prompt: &str, mime_type: &str, data: &str) -> Value {
    serde_json::json!({
        "model": model,
        "max_tokens": 1024,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "image", "source": {"type": "base64", "media_type": mime_type, "data": data}},
                {"type": "text", "text": prompt}
            ]
        }]
    })
}

/// Request body for `Client::probe` - one "ping" user message with the
/// output capped low. OpenAI's reasoning models reject a cap too small to
/// finish any reply, and Gemini's thinking models spend tokens before
/// answering, hence 16 rather than 1 - still a negligible cost.
fn probe_body(provider: Provider, model: &str) -> Value {
    match provider {
        Provider::Gemini => serde_json::json!({
            "contents": [{"role": "user", "parts": [{"text": "ping"}]}],
            "generationConfig": {"maxOutputTokens": 16}
        }),
        Provider::OpenAi | Provider::Groq => serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": "ping"}],
            "max_completion_tokens": 16
        }),
        Provider::Claude => serde_json::json!({
            "model": model,
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "ping"}]
        }),
        Provider::Ollama => serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": "ping"}],
            "stream": false,
            "options": {"num_predict": 1}
        }),
    }
}

/// Ollama's `/api/chat` takes images as a plain array of base64 strings
/// alongside the message - no data-URL prefix and no per-image mime type,
/// unlike the other three providers.
fn ollama_image_body(model: &str, prompt: &str, data: &str) -> Value {
    serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt, "images": [data]}],
        "stream": false
    })
}

fn extract_gemini_models(value: &Value) -> Vec<String> {
    value
        .get("models")
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter(|m| {
                    m.get("supportedGenerationMethods")
                        .and_then(Value::as_array)
                        .is_some_and(|methods| methods.iter().any(|m| m.as_str() == Some("generateContent")))
                })
                .filter_map(|m| m.get("name").and_then(Value::as_str))
                .map(|name| name.strip_prefix("models/").unwrap_or(name).to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Excludes obviously non-chat model families (embeddings, audio, image
/// generation, moderation, Groq's guard classifiers and TTS voices) to keep
/// the picker focused - an OpenAI-compatible `/models` endpoint lists
/// everything the account can use, chat or not. Groq also marks retired
/// models `"active": false` instead of dropping them.
fn extract_openai_models(value: &Value) -> Vec<String> {
    const EXCLUDED_SUBSTRINGS: &[&str] = &["embedding", "whisper", "tts", "dall-e", "moderation", "guard", "orpheus"];
    let mut models: Vec<String> = value
        .get("data")
        .and_then(Value::as_array)
        .map(|data| {
            data.iter()
                .filter(|m| m.get("active").and_then(Value::as_bool) != Some(false))
                .filter_map(|m| m.get("id").and_then(Value::as_str))
                .filter(|id| !EXCLUDED_SUBSTRINGS.iter().any(|excluded| id.contains(excluded)))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    models.sort();
    models
}

fn extract_claude_models(value: &Value) -> Vec<String> {
    value
        .get("data")
        .and_then(Value::as_array)
        .map(|data| data.iter().filter_map(|m| m.get("id").and_then(Value::as_str)).map(str::to_string).collect())
        .unwrap_or_default()
}

fn extract_ollama_models(value: &Value) -> Vec<String> {
    value
        .get("models")
        .and_then(Value::as_array)
        .map(|models| models.iter().filter_map(|m| m.get("name").and_then(Value::as_str)).map(str::to_string).collect())
        .unwrap_or_default()
}

fn parse_json(body_text: &str) -> Result<Value> {
    serde_json::from_str(body_text).map_err(|err| ApiError::other(tr("Antwort nicht lesbar: {err}").replace("{err}", &err.to_string())))
}

/// Digs an error message out of a provider's error response body, walking
/// `path` (e.g. `["error", "message"]`) into the parsed JSON; falls back to
/// the raw body text if that shape doesn't match (Ollama in particular
/// sometimes returns a bare string message instead of nested JSON).
fn error_from_body(status: u16, body_text: &str, path: &[&str]) -> ApiError {
    let message = serde_json::from_str::<Value>(body_text)
        .ok()
        .and_then(|v| {
            let mut current = &v;
            for key in path {
                current = current.get(key)?;
            }
            current.as_str().map(str::to_string)
        })
        .unwrap_or_else(|| body_text.to_string());
    ApiError {
        message: format!("HTTP {status}: {message}"),
        status: classify(status, body_text),
    }
}

/// The error matrix: maps a failed call's HTTP status plus the provider's
/// own error text onto a `ModelStatus`. The status code alone isn't
/// enough - Gemini reports an invalid key as 400 and a model the free tier
/// doesn't include as a 429 with `limit: 0`, OpenAI an unpaid account as a
/// 429 `insufficient_quota`, and Anthropic an empty balance as a 400.
pub fn classify(status: u16, body_text: &str) -> ModelStatus {
    let body = body_text.to_lowercase();
    let has = |needle: &str| body.contains(needle);
    if (200..300).contains(&status) {
        return ModelStatus::Available;
    }
    if has("location is not supported") || has("unsupported_country") || has("not available in your country") || has("region is not supported") {
        return ModelStatus::RegionBlocked;
    }
    // Not a bare "billing" match: Gemini's ordinary per-minute 429 also
    // says "check your plan and billing details".
    if status == 402 || has("insufficient_quota") || has("credit balance is too low") {
        return ModelStatus::PaymentRequired;
    }
    if has("api key not valid") || has("api_key_invalid") || has("invalid_api_key") || has("invalid x-api-key") {
        return ModelStatus::InvalidKey;
    }
    match status {
        401 => ModelStatus::InvalidKey,
        403 => ModelStatus::NoAccess,
        404 => ModelStatus::NotFound,
        429 => {
            // Gemini's free tier lists models it grants no quota for at
            // all ("... free_tier_requests, limit: 0") - waiting won't help.
            if has("limit: 0") || has("\"quota_value\":\"0\"") {
                ModelStatus::PaymentRequired
            } else {
                ModelStatus::RateLimited
            }
        }
        400 if has("image") && (has("support") || has("vision")) => ModelStatus::Unsupported,
        400 if has("does not exist") || has("not found") || has("deprecated") || has("decommissioned") => ModelStatus::NotFound,
        408 | 500..=599 => ModelStatus::ServerError,
        _ => ModelStatus::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_id_round_trips() {
        for provider in Provider::ALL {
            assert_eq!(Provider::from_id(provider.id()), provider);
        }
    }

    #[test]
    fn only_ollama_skips_the_api_key() {
        assert!(!Provider::Ollama.needs_api_key());
        assert!(Provider::Gemini.needs_api_key());
        assert!(Provider::OpenAi.needs_api_key());
        assert!(Provider::Claude.needs_api_key());
    }

    #[test]
    fn only_ollama_has_a_configurable_base_url() {
        assert!(Provider::Ollama.needs_base_url());
        assert!(!Provider::Gemini.needs_base_url());
    }

    #[test]
    fn gemini_success_body_is_parsed() {
        let body = r#"{"candidates":[{"content":{"parts":[{"text":"Hallo!"}]}}]}"#;
        let value: Value = parse_json(body).unwrap();
        assert_eq!(value.pointer("/candidates/0/content/parts/0/text").and_then(Value::as_str), Some("Hallo!"));
    }

    #[test]
    fn openai_success_body_is_parsed() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"Hi!"}}]}"#;
        let value: Value = parse_json(body).unwrap();
        assert_eq!(value.pointer("/choices/0/message/content").and_then(Value::as_str), Some("Hi!"));
    }

    #[test]
    fn claude_success_body_is_parsed() {
        let body = r#"{"content":[{"type":"text","text":"Servus!"}],"role":"assistant"}"#;
        let value: Value = parse_json(body).unwrap();
        assert_eq!(value.pointer("/content/0/text").and_then(Value::as_str), Some("Servus!"));
    }

    #[test]
    fn ollama_success_body_is_parsed() {
        let body = r#"{"message":{"role":"assistant","content":"Moin!"},"done":true}"#;
        let value: Value = parse_json(body).unwrap();
        assert_eq!(value.pointer("/message/content").and_then(Value::as_str), Some("Moin!"));
    }

    #[test]
    fn error_from_body_extracts_nested_message() {
        let err = error_from_body(401, r#"{"error":{"message":"invalid key"}}"#, &["error", "message"]);
        assert_eq!(err.message, "HTTP 401: invalid key");
    }

    #[test]
    fn error_from_body_falls_back_to_raw_text_on_mismatch() {
        let err = error_from_body(500, "plain text error", &["error", "message"]);
        assert_eq!(err.message, "HTTP 500: plain text error");
    }

    #[test]
    fn classify_maps_provider_errors_onto_the_error_matrix() {
        let cases: &[(u16, &str, ModelStatus)] = &[
            (200, "{}", ModelStatus::Available),
            // Gemini
            (400, r#"{"error":{"code":400,"message":"API key not valid. Please pass a valid API key.","status":"INVALID_ARGUMENT"}}"#, ModelStatus::InvalidKey),
            (400, r#"{"error":{"message":"User location is not supported for the API use.","status":"FAILED_PRECONDITION"}}"#, ModelStatus::RegionBlocked),
            (
                429,
                r#"{"error":{"message":"You exceeded your current quota, please check your plan and billing details. Quota exceeded for metric: generativelanguage.googleapis.com/generate_content_free_tier_requests, limit: 0, model: gemini-2.5-pro","status":"RESOURCE_EXHAUSTED"}}"#,
                ModelStatus::PaymentRequired,
            ),
            (
                429,
                r#"{"error":{"message":"You exceeded your current quota, please check your plan and billing details. Quota exceeded for metric: generate_content_free_tier_requests, limit: 10, model: gemini-2.5-flash","status":"RESOURCE_EXHAUSTED"}}"#,
                ModelStatus::RateLimited,
            ),
            (404, r#"{"error":{"message":"models/gemini-1.0-pro is not found for API version v1beta","status":"NOT_FOUND"}}"#, ModelStatus::NotFound),
            (403, r#"{"error":{"message":"Permission denied","status":"PERMISSION_DENIED"}}"#, ModelStatus::NoAccess),
            (503, r#"{"error":{"message":"The model is overloaded.","status":"UNAVAILABLE"}}"#, ModelStatus::ServerError),
            // OpenAI
            (401, r#"{"error":{"message":"Incorrect API key provided","code":"invalid_api_key"}}"#, ModelStatus::InvalidKey),
            (429, r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","code":"insufficient_quota"}}"#, ModelStatus::PaymentRequired),
            (429, r#"{"error":{"message":"Rate limit reached for gpt-4o","code":"rate_limit_exceeded"}}"#, ModelStatus::RateLimited),
            (403, r#"{"error":{"message":"Country, region, or territory not supported","code":"unsupported_country_region_territory"}}"#, ModelStatus::RegionBlocked),
            (404, r#"{"error":{"message":"The model `gpt-5-pro` does not exist or you do not have access to it.","code":"model_not_found"}}"#, ModelStatus::NotFound),
            (400, r#"{"error":{"message":"Invalid content type. image_url is only supported by certain models."}}"#, ModelStatus::Unsupported),
            // Anthropic
            (400, r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API."}}"#, ModelStatus::PaymentRequired),
            (429, r#"{"type":"error","error":{"type":"rate_limit_error","message":"Number of request tokens has exceeded your per-minute rate limit"}}"#, ModelStatus::RateLimited),
            (529, r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#, ModelStatus::ServerError),
            (402, "", ModelStatus::PaymentRequired),
            // A plain bad request says nothing about availability.
            (400, r#"{"error":{"message":"Invalid value for temperature"}}"#, ModelStatus::Other),
        ];
        for (status, body, expected) in cases {
            assert_eq!(classify(*status, body), *expected, "HTTP {status}: {body}");
        }
    }

    #[test]
    fn only_account_and_model_problems_trigger_a_fallback() {
        assert!(ModelStatus::PaymentRequired.warrants_fallback());
        assert!(ModelStatus::RateLimited.warrants_fallback());
        assert!(ModelStatus::Unsupported.warrants_fallback());
        assert!(!ModelStatus::Other.warrants_fallback());
        assert!(!ModelStatus::RateLimited.is_permanent_block(), "a rate limit is temporary");
        assert!(ModelStatus::RegionBlocked.is_permanent_block());
    }

    #[test]
    fn model_status_round_trips_through_its_id() {
        for status in ModelStatus::ALL {
            assert_eq!(ModelStatus::from_id(status.id()), status);
        }
    }

    #[test]
    fn probe_bodies_cap_the_output() {
        assert_eq!(probe_body(Provider::Gemini, "m").pointer("/generationConfig/maxOutputTokens").and_then(Value::as_u64), Some(16));
        assert_eq!(probe_body(Provider::OpenAi, "m").get("max_completion_tokens").and_then(Value::as_u64), Some(16));
        assert_eq!(probe_body(Provider::Claude, "m").get("max_tokens").and_then(Value::as_u64), Some(1));
        assert_eq!(probe_body(Provider::Ollama, "m").pointer("/options/num_predict").and_then(Value::as_u64), Some(1));
    }

    #[test]
    fn gemini_models_are_filtered_to_generate_content_and_stripped_of_prefix() {
        let body = r#"{"models":[
            {"name":"models/gemini-2.5-flash","supportedGenerationMethods":["generateContent"]},
            {"name":"models/embedding-001","supportedGenerationMethods":["embedContent"]}
        ]}"#;
        let value: Value = parse_json(body).unwrap();
        assert_eq!(extract_gemini_models(&value), vec!["gemini-2.5-flash".to_string()]);
    }

    #[test]
    fn openai_models_exclude_non_chat_families_and_are_sorted() {
        let body = r#"{"data":[
            {"id":"gpt-4o-mini"},
            {"id":"text-embedding-3-small"},
            {"id":"whisper-1"},
            {"id":"gpt-4o"}
        ]}"#;
        let value: Value = parse_json(body).unwrap();
        assert_eq!(extract_openai_models(&value), vec!["gpt-4o".to_string(), "gpt-4o-mini".to_string()]);
    }

    #[test]
    fn groq_models_skip_audio_guard_and_inactive_entries() {
        let body = r#"{"object":"list","data":[
            {"id":"llama-3.3-70b-versatile","active":true},
            {"id":"whisper-large-v3","active":true},
            {"id":"meta-llama/llama-guard-4-12b","active":true},
            {"id":"playai-tts","active":true},
            {"id":"gemma2-9b-it","active":false},
            {"id":"llama-3.1-8b-instant","active":true}
        ]}"#;
        let value: Value = parse_json(body).unwrap();
        assert_eq!(extract_openai_models(&value), vec!["llama-3.1-8b-instant".to_string(), "llama-3.3-70b-versatile".to_string()]);
    }

    #[test]
    fn groq_shares_the_openai_request_shape_under_its_own_base_url() {
        assert_eq!(Provider::Groq.openai_compatible_base(), Some("https://api.groq.com/openai/v1"));
        assert_eq!(Client::new(Provider::Groq, "k", "m", "").openai_url("models"), "https://api.groq.com/openai/v1/models");
        assert_eq!(Client::new(Provider::OpenAi, "k", "m", "").openai_url("chat/completions"), "https://api.openai.com/v1/chat/completions");
        assert!(Provider::Groq.needs_api_key());
        assert!(!Provider::Groq.needs_base_url());
    }

    #[test]
    fn classify_handles_groq_errors() {
        assert_eq!(classify(429, r#"{"error":{"message":"Rate limit reached for model `llama-3.3-70b-versatile` on tokens per minute (TPM)","type":"tokens","code":"rate_limit_exceeded"}}"#), ModelStatus::RateLimited);
        assert_eq!(classify(400, r#"{"error":{"message":"The model `mixtral-8x7b-32768` has been decommissioned and is no longer supported.","code":"model_decommissioned"}}"#), ModelStatus::NotFound);
        assert_eq!(classify(401, r#"{"error":{"message":"Invalid API Key","code":"invalid_api_key"}}"#), ModelStatus::InvalidKey);
    }

    #[test]
    fn claude_models_are_extracted_in_api_order() {
        let body = r#"{"data":[{"id":"claude-sonnet-5"},{"id":"claude-haiku-4-5"}]}"#;
        let value: Value = parse_json(body).unwrap();
        assert_eq!(extract_claude_models(&value), vec!["claude-sonnet-5".to_string(), "claude-haiku-4-5".to_string()]);
    }

    #[test]
    fn ollama_models_are_extracted_from_tags() {
        let body = r#"{"models":[{"name":"llama3.2:latest"},{"name":"mistral:latest"}]}"#;
        let value: Value = parse_json(body).unwrap();
        assert_eq!(extract_ollama_models(&value), vec!["llama3.2:latest".to_string(), "mistral:latest".to_string()]);
    }

    #[test]
    fn gemini_image_body_inlines_the_image_alongside_the_prompt() {
        let body = gemini_image_body("Beschreibe dieses Bild.", "image/png", "QUJD");
        assert_eq!(body.pointer("/contents/0/parts/0/text").and_then(Value::as_str), Some("Beschreibe dieses Bild."));
        assert_eq!(body.pointer("/contents/0/parts/1/inline_data/mime_type").and_then(Value::as_str), Some("image/png"));
        assert_eq!(body.pointer("/contents/0/parts/1/inline_data/data").and_then(Value::as_str), Some("QUJD"));
    }

    #[test]
    fn openai_image_body_uses_a_data_url() {
        let body = openai_image_body("gpt-4o-mini", "Beschreibe dieses Bild.", "image/png", "QUJD");
        assert_eq!(body.pointer("/messages/0/content/0/text").and_then(Value::as_str), Some("Beschreibe dieses Bild."));
        assert_eq!(
            body.pointer("/messages/0/content/1/image_url/url").and_then(Value::as_str),
            Some("data:image/png;base64,QUJD")
        );
    }

    #[test]
    fn claude_image_body_uses_base64_source() {
        let body = claude_image_body("claude-sonnet-5", "Beschreibe dieses Bild.", "image/png", "QUJD");
        assert_eq!(body.pointer("/messages/0/content/0/source/media_type").and_then(Value::as_str), Some("image/png"));
        assert_eq!(body.pointer("/messages/0/content/0/source/data").and_then(Value::as_str), Some("QUJD"));
        assert_eq!(body.pointer("/messages/0/content/1/text").and_then(Value::as_str), Some("Beschreibe dieses Bild."));
    }

    #[test]
    fn ollama_image_body_carries_images_as_plain_base64_array() {
        let body = ollama_image_body("llava", "Beschreibe dieses Bild.", "QUJD");
        assert_eq!(body.pointer("/messages/0/content").and_then(Value::as_str), Some("Beschreibe dieses Bild."));
        assert_eq!(body.pointer("/messages/0/images/0").and_then(Value::as_str), Some("QUJD"));
    }
}
