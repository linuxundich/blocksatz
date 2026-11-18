//! Mastodon and Bluesky posts for an article, written in Blocksatz and
//! sent by the WP Social Publisher plugin on the blog (`docs/social.md`).
//!
//! The texts live in the article's frontmatter:
//!
//! ```yaml
//! social_networks: ["mastodon", "bluesky"]
//! social_intro: "Neu im Blog:"
//! social_mastodon: "Eigener Text …\n\n{link}"
//! social_bluesky_embed: card
//! ```
//!
//! `social_networks` lists the networks that go out; a network's text
//! stays in the file when it is switched off. An empty or missing text
//! means "as the plugin writes it" (from title, excerpt and tags, with
//! `social_intro` in front). `{link}` stands for the permalink, which
//! often isn't final yet while writing. Texts are JSON strings, which is
//! valid YAML and keeps line breaks on one line.
//!
//! Uploading a post that isn't published yet sends the texts as the REST
//! field `wpsp_social`; the plugin plans them and posts them the moment
//! WordPress publishes the article - in the same request, or later when
//! a scheduled post goes live. A post that is already online is shared
//! with "Jetzt teilen" (`share`).

use serde_json::{json, Value};

use crate::document::{self, Frontmatter, PostStatus, PostType};
use crate::i18n::tr;
use crate::wpclient;

/// The networks the plugin knows, in display order.
pub const NETWORKS: [(&str, &str); 2] = [("mastodon", "Mastodon"), ("bluesky", "Bluesky")];

/// Stand-in for the permalink in a text.
pub const LINK: &str = "{link}";

/// REST namespace of the plugin - present in the blog's index when it's
/// active.
pub const NAMESPACE: &str = "wpsp/v1";

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SocialPosts {
    /// Line in front of generated texts.
    pub intro: String,
    /// Networks that go out, e.g. `["mastodon", "bluesky"]`.
    pub enabled: Vec<String>,
    /// Text and attachment per network, kept while switched off.
    pub entries: Vec<SocialEntry>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SocialEntry {
    pub network: String,
    /// Empty: written by the plugin.
    pub text: String,
    /// `image`, `card` or `none` - `None` takes the plugin's default.
    pub embed: Option<String>,
}

impl SocialPosts {
    pub fn entry(&self, network: &str) -> Option<&SocialEntry> {
        self.entries.iter().find(|e| e.network == network)
    }

    pub fn entry_mut(&mut self, network: &str) -> &mut SocialEntry {
        if let Some(i) = self.entries.iter().position(|e| e.network == network) {
            return &mut self.entries[i];
        }
        self.entries.push(SocialEntry { network: network.to_string(), ..Default::default() });
        self.entries.last_mut().expect("just pushed")
    }

    pub fn is_enabled(&self, network: &str) -> bool {
        self.enabled.iter().any(|n| n == network)
    }

    pub fn set_enabled(&mut self, network: &str, on: bool) {
        self.enabled.retain(|n| n != network);
        if on {
            self.enabled.push(network.to_string());
            // Keep the display order regardless of the order of clicks.
            self.enabled.sort_by_key(|n| NETWORKS.iter().position(|(id, _)| id == n).unwrap_or(usize::MAX));
        }
    }

    /// What the REST field `wpsp_social` gets: the enabled networks only;
    /// none at all withdraws a plan made earlier.
    pub fn payload(&self) -> Value {
        let items: Vec<Value> = self
            .enabled
            .iter()
            .map(|network| {
                let entry = self.entry(network);
                json!({
                    "network": network,
                    "text": entry.map(|e| e.text.trim()).unwrap_or_default(),
                    "embed": entry.and_then(|e| e.embed.clone()).unwrap_or_default(),
                })
            })
            .collect();
        json!({ "intro": self.intro.trim(), "items": items })
    }
}

pub fn is_key(key: &str) -> bool {
    key.starts_with("social_")
}

pub fn parse_key(social: &mut SocialPosts, key: &str, value: &str) {
    let Some(name) = key.strip_prefix("social_") else { return };
    match name {
        "networks" => social.enabled = document::parse_list(value),
        "intro" => social.intro = json_unquote(value),
        _ => match name.strip_suffix("_embed") {
            Some(network) => social.entry_mut(network).embed = Some(document::unquote(value)).filter(|v| !v.is_empty()),
            None => social.entry_mut(name).text = json_unquote(value),
        },
    }
}

pub fn serialize(social: &SocialPosts) -> String {
    let mut out = String::new();
    let list: Vec<String> = social.enabled.iter().map(|n| format!("\"{n}\"")).collect();
    out.push_str(&format!("social_networks: [{}]\n", list.join(", ")));
    if !social.intro.is_empty() {
        out.push_str(&format!("social_intro: {}\n", json_quote(&social.intro)));
    }
    for entry in &social.entries {
        if !entry.text.is_empty() {
            out.push_str(&format!("social_{}: {}\n", entry.network, json_quote(&entry.text)));
        }
        if let Some(embed) = &entry.embed {
            out.push_str(&format!("social_{}_embed: {embed}\n", entry.network));
        }
    }
    out
}

fn json_quote(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string())
}

/// A JSON string, else a plain YAML scalar (written by hand).
fn json_unquote(value: &str) -> String {
    serde_json::from_str::<String>(value.trim()).unwrap_or_else(|_| document::unquote(value))
}

/// Whether an upload sends the texts: Blocksatz manages them and the post
/// isn't online yet. An online post is shared with `share` instead - the
/// plugin would refuse a plan for it anyway.
pub fn upload_sends(fm: &Frontmatter) -> bool {
    fm.post_type == PostType::Post && fm.social.is_some() && !matches!(fm.wp_status, Some(PostStatus::Publish | PostStatus::Private))
}

/// A network as the blog has it set up (`wpsp/v1/status`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NetworkInfo {
    pub id: String,
    pub name: String,
    pub configured: bool,
    pub limit: usize,
    /// Mastodon counts every URL as this many characters (0: as written).
    pub url_length: usize,
    pub attachments: Vec<String>,
    pub embed_mode: String,
    pub posted: bool,
    pub url: String,
}

/// What the blog knows about the article's posts.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Status {
    pub networks: Vec<NetworkInfo>,
    /// Networks with a plan waiting for the article to go live.
    pub planned: Vec<String>,
    /// The permalink the plan was counted with.
    pub planned_link: String,
    /// Last automatic send: network, success, message.
    pub results: Vec<(String, bool, String)>,
    /// Something the plugin refused while saving the field.
    pub notice: String,
}

impl Status {
    pub fn network(&self, id: &str) -> Option<&NetworkInfo> {
        self.networks.iter().find(|n| n.id == id)
    }
}

pub fn parse_status(value: &Value) -> Status {
    let networks = value
        .get("networks")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(id, n)| NetworkInfo {
                    id: id.clone(),
                    name: n.get("name").and_then(Value::as_str).unwrap_or(id).to_string(),
                    configured: n.get("configured").and_then(Value::as_bool).unwrap_or(false),
                    limit: n.get("limit").and_then(Value::as_u64).unwrap_or(0) as usize,
                    url_length: n.get("urlLength").and_then(Value::as_u64).unwrap_or(0) as usize,
                    attachments: n.get("attachments").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default(),
                    embed_mode: n.get("embedMode").and_then(Value::as_str).unwrap_or_default().to_string(),
                    posted: n.get("posted").and_then(Value::as_bool).unwrap_or(false),
                    url: n.get("url").and_then(Value::as_str).unwrap_or_default().to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    let plan = value.get("plan").filter(|p| p.is_object());
    let planned = plan.and_then(|p| p.get("networks")).and_then(Value::as_object).map(|m| m.keys().cloned().collect()).unwrap_or_default();
    let planned_link = plan.and_then(|p| p.get("link")).and_then(Value::as_str).unwrap_or_default().to_string();
    let results = value.get("result").and_then(|r| r.get("results")).map(parse_results).unwrap_or_default();
    let notice = value.get("notice").and_then(Value::as_str).unwrap_or_default().to_string();
    Status { networks, planned, planned_link, results, notice }
}

fn parse_results(value: &Value) -> Vec<(String, bool, String)> {
    value
        .as_object()
        .map(|map| {
            map.iter()
                .map(|(id, r)| (id.clone(), r.get("success").and_then(Value::as_bool).unwrap_or(false), r.get("message").and_then(Value::as_str).unwrap_or_default().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn network_name(id: &str) -> String {
    NETWORKS.iter().find(|(n, _)| *n == id).map(|(_, name)| name.to_string()).unwrap_or_else(|| id.to_string())
}

/// Lines for the upload's success message: what was planned or sent.
pub fn upload_notes(answer: Option<&Value>, sent: bool) -> Vec<String> {
    if !sent {
        return Vec::new();
    }
    let Some(answer) = answer else {
        return vec![tr("Social Media: Das Blog hat die Texte nicht angenommen – ist WP Social Publisher ab 1.10 aktiv?")];
    };
    let status = parse_status(answer);
    let mut notes = Vec::new();
    if !status.notice.is_empty() {
        notes.push(tr("Social Media: {msg}").replace("{msg}", &status.notice));
    }
    if !status.planned.is_empty() {
        let names: Vec<String> = status.planned.iter().map(|n| network_name(n)).collect();
        notes.push(tr("Social Media vorgemerkt für {networks} – geht beim Veröffentlichen hinaus.").replace("{networks}", &names.join(", ")));
    }
    for (id, ok, message) in &status.results {
        notes.push(if *ok {
            tr("{network}: geteilt.").replace("{network}", &network_name(id))
        } else {
            tr("{network}: nicht geteilt – {msg}").replace("{network}", &network_name(id)).replace("{msg}", message)
        });
    }
    notes
}

/// Characters a text takes on a network, as the plugin counts them
/// (codepoints - close to graphemes for ordinary text; the blog checks
/// again before anything goes out). `{link}` counts as `link_len`.
pub fn count(text: &str, info: &NetworkInfo, link_len: usize) -> usize {
    let mut total = 0;
    for word in split_keep_ws(text) {
        if word == LINK || word.contains(LINK) {
            let rest = word.replace(LINK, "");
            total += if info.url_length > 0 { info.url_length } else { link_len } + rest.chars().count();
        } else if info.url_length > 0 && (word.starts_with("http://") || word.starts_with("https://")) {
            total += info.url_length;
        } else {
            total += word.chars().count();
        }
    }
    total
}

/// Splits into runs of whitespace and non-whitespace, keeping both.
fn split_keep_ws(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut in_ws: Option<bool> = None;
    for (i, c) in text.char_indices() {
        let ws = c.is_whitespace();
        if in_ws.is_some_and(|prev| prev != ws) {
            parts.push(&text[start..i]);
            start = i;
        }
        in_ws = Some(ws);
    }
    if start < text.len() {
        parts.push(&text[start..]);
    }
    parts
}

/// The permalink length to count `{link}` with before the plugin knows it:
/// the blog address plus the slug.
pub fn estimated_link_len(site_url: &str, slug: &str) -> usize {
    format!("{}/{}/", site_url.trim_end_matches('/'), if slug.is_empty() { "artikel" } else { slug }).chars().count()
}

// --- Talking to the plugin (blocking; call from a worker thread) -------

/// The plugin's view of the blog and, with `post_id`, of the article.
/// `Ok(None)`: the plugin isn't active there.
pub fn fetch_status(client: &wpclient::Client, post_id: Option<u64>) -> Result<Option<Status>, String> {
    let route = match post_id {
        Some(id) => format!("{NAMESPACE}/status?post={id}"),
        None => format!("{NAMESPACE}/status"),
    };
    match client.rest_get(&route) {
        Ok(value) => Ok(Some(parse_status(&value))),
        Err(err) if err.status == 404 => Ok(None),
        Err(err) => Err(err.to_string()),
    }
}

/// Texts as the plugin writes them, per network (`{link}` for the link).
pub fn generate(client: &wpclient::Client, post_id: u64, intro: &str) -> Result<Vec<(String, String)>, String> {
    let value = client.rest_post(&format!("{NAMESPACE}/posts/{post_id}/generate"), &json!({ "intro": intro })).map_err(|err| err.to_string())?;
    Ok(value
        .get("networks")
        .and_then(Value::as_object)
        .map(|map| map.iter().map(|(id, n)| (id.clone(), n.get("text").and_then(Value::as_str).unwrap_or_default().to_string())).collect())
        .unwrap_or_default())
}

/// Per network: id, success, message.
pub type Results = Vec<(String, bool, String)>;

/// Shares an article that is online now. Returns network, success,
/// message for each network, plus the new status.
pub fn share(client: &wpclient::Client, post_id: u64, social: &SocialPosts) -> Result<(Results, Option<Status>), String> {
    let value = client.rest_post(&format!("{NAMESPACE}/posts/{post_id}/share"), &social.payload()).map_err(|err| err.to_string())?;
    let results = value.get("results").map(parse_results).unwrap_or_default();
    Ok((results, value.get("social").map(parse_status)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SocialPosts {
        let mut s = SocialPosts { intro: "Neu im Blog:".into(), ..Default::default() };
        s.set_enabled("bluesky", true);
        s.set_enabled("mastodon", true);
        s.entry_mut("mastodon").text = "Zeile eins\n\n\"Zitat\" und {link}".into();
        s.entry_mut("bluesky").embed = Some("card".into());
        s
    }

    #[test]
    fn round_trips_through_the_frontmatter() {
        let mut doc = document::Document::default();
        doc.frontmatter.title = "T".into();
        doc.frontmatter.social = Some(sample());
        doc.body = "Text\n".into();
        let text = document::serialize(&doc);
        assert!(text.contains("social_networks: [\"mastodon\", \"bluesky\"]\n"), "{text}");
        assert!(text.contains("social_mastodon: \"Zeile eins\\n\\n\\\"Zitat\\\" und {link}\"\n"), "{text}");
        assert!(text.contains("social_bluesky_embed: card\n"), "{text}");
        let back = document::parse(&text);
        assert_eq!(back.frontmatter.social, doc.frontmatter.social);
        assert_eq!(back.body, "Text\n");
    }

    #[test]
    fn no_social_keys_without_social() {
        let mut doc = document::Document::default();
        doc.frontmatter.title = "T".into();
        assert!(!document::serialize(&doc).contains("social_"));
        assert_eq!(document::parse(&document::serialize(&doc)).frontmatter.social, None);
    }

    #[test]
    fn hand_written_plain_values_are_read() {
        let doc = document::parse("---\ntitle: \"T\"\nsocial_networks: [mastodon]\nsocial_mastodon: Kurz und gut\n---\n\nx");
        let social = doc.frontmatter.social.unwrap();
        assert!(social.is_enabled("mastodon"));
        assert_eq!(social.entry("mastodon").unwrap().text, "Kurz und gut");
    }

    #[test]
    fn payload_sends_only_enabled_networks() {
        let mut s = sample();
        s.set_enabled("mastodon", false);
        let p = s.payload();
        let items = p["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["network"], "bluesky");
        assert_eq!(items[0]["text"], "");
        assert_eq!(items[0]["embed"], "card");
        assert_eq!(p["intro"], "Neu im Blog:");
    }

    #[test]
    fn upload_sends_only_for_posts_not_yet_online() {
        let mut fm = Frontmatter::default();
        assert!(!upload_sends(&fm), "not managed");
        fm.social = Some(SocialPosts::default());
        assert!(upload_sends(&fm), "new post");
        fm.wp_status = Some(PostStatus::Future);
        assert!(upload_sends(&fm), "scheduled");
        fm.wp_status = Some(PostStatus::Publish);
        assert!(!upload_sends(&fm), "online");
    }

    #[test]
    fn counting_follows_the_networks() {
        let mastodon = NetworkInfo { url_length: 23, ..Default::default() };
        let bluesky = NetworkInfo::default();
        assert_eq!(count("Hallo {link}", &mastodon, 50), 6 + 23);
        assert_eq!(count("Hallo {link}", &bluesky, 50), 6 + 50);
        assert_eq!(count("Siehe https://example.org/ein/sehr/langer/pfad/", &mastodon, 0), 6 + 23);
        assert_eq!(count("Grüße 👋", &bluesky, 0), 7);
    }

    #[test]
    fn status_and_notes_from_the_plugin_answer() {
        let answer = json!({
            "networks": { "mastodon": { "name": "Mastodon", "configured": true, "limit": 500, "urlLength": 23, "attachments": ["image", "none"], "embedMode": "image", "posted": false, "url": "" } },
            "plan": { "intro": "", "link": "https://example.org/x/", "networks": { "mastodon": { "text": "t" } } },
            "result": null,
            "notice": ""
        });
        let status = parse_status(&answer);
        assert_eq!(status.network("mastodon").unwrap().limit, 500);
        assert_eq!(status.planned, vec!["mastodon".to_string()]);
        let notes = upload_notes(Some(&answer), true);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("Mastodon"));
        assert!(upload_notes(None, false).is_empty());
        assert_eq!(upload_notes(None, true).len(), 1, "plugin missing is said");
    }
}
