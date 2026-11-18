//! Mastodon and Bluesky postings in the live preview: a lone posting URL
//! shows as the same card the blog's `lui-social-embed` plugin renders,
//! instead of the generic embed placeholder.
//!
//! The posting comes from the networks' public interfaces, without
//! signing in - the same two the plugin uses:
//!
//! - Mastodon: `GET https://<instance>/api/v1/statuses/<id>`
//! - Bluesky: `com.atproto.identity.resolveHandle`, then
//!   `app.bsky.feed.getPostThread` on `public.api.bsky.app`
//!
//! Rendering never waits for the network. `card` only looks into the
//! cache; a URL it doesn't know yet is queued, fetched on a worker thread,
//! and every preview that registered with `on_loaded` renders again once
//! it is in. Tests register nothing, so they never fetch. Fetched postings
//! are kept as JSON under the user cache dir and refreshed after a day or
//! after the preview's reload button (`refresh_all`).
//!
//! The card's texts follow the article's language (`set_english`), not
//! the UI's - like the plugin, which follows the blog's.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gtk4::glib;
use serde_json::{json, Value};

/// How long a fetched posting is shown without asking again.
const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const BSKY_API: &str = "https://public.api.bsky.app/xrpc/";

/// A recognized posting URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Posting {
    Mastodon { host: String, id: String },
    Bluesky { actor: String, rkey: String },
}

impl Posting {
    fn key(&self) -> String {
        match self {
            Self::Mastodon { host, id } => format!("mastodon:{host}:{id}"),
            Self::Bluesky { actor, rkey } => format!("bluesky:{actor}:{rkey}"),
        }
    }

    fn is_bluesky(&self) -> bool {
        matches!(self, Self::Bluesky { .. })
    }
}

/// The posting a URL points to: `https://<instance>/@user/<id>`,
/// `https://<instance>/@user@other.host/<id>`,
/// `https://<instance>/users/<user>/statuses/<id>` or
/// `https://bsky.app/profile/<handle or did>/post/<rkey>` - the same
/// addresses the plugin recognizes.
pub fn parse_url(url: &str) -> Option<Posting> {
    let rest = url.trim();
    let rest = rest.strip_prefix("https://").or_else(|| rest.strip_prefix("http://"))?;
    let rest = rest.split(['?', '#']).next()?;
    let mut parts = rest.trim_end_matches('/').split('/');
    let host = parts.next()?.to_ascii_lowercase();
    let segments: Vec<&str> = parts.collect();
    let word = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-'));

    if host == "bsky.app" || host == "www.bsky.app" {
        if let ["profile", actor, "post", rkey] = segments.as_slice() {
            let actor_ok = !actor.is_empty() && actor.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '_' | '-'));
            let rkey_ok = rkey.len() >= 8 && rkey.chars().all(|c| c.is_ascii_alphanumeric());
            if actor_ok && rkey_ok {
                return Some(Posting::Bluesky { actor: actor.to_ascii_lowercase(), rkey: (*rkey).to_string() });
            }
        }
        return None;
    }

    let host_ok = host.contains('.') && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-')) && !host.starts_with(['.', '-']);
    if !host_ok {
        return None;
    }
    let id_ok = |id: &str| id.len() >= 6 && id.chars().all(|c| c.is_ascii_digit());
    let id = match segments.as_slice() {
        [user, id] if user.starts_with('@') && user[1..].split('@').all(word) && user[1..].split('@').count() <= 2 => id,
        ["users", user, "statuses", id] if word(user) => id,
        _ => return None,
    };
    id_ok(id).then(|| Posting::Mastodon { host, id: (*id).to_string() })
}

/// What the cache knows about one posting.
#[derive(Clone)]
enum Entry {
    /// The normalized posting (see `normalize_mastodon`/`normalize_bluesky`).
    Post(Value),
    /// Deleted, not public or not there at all.
    Gone,
    /// The last fetch failed (network, locked instance) - shown as a
    /// placeholder with the reason, tried again on the next reload.
    Failed(String),
}

struct Cached {
    entry: Entry,
    fetched: SystemTime,
}

thread_local! {
    static CACHE: RefCell<HashMap<String, Cached>> = RefCell::new(HashMap::new());
    /// URLs a render asked for that aren't (freshly) cached.
    static WANTED: RefCell<Vec<Posting>> = const { RefCell::new(Vec::new()) };
    static IN_FLIGHT: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static FETCH_SCHEDULED: Cell<bool> = const { Cell::new(false) };
    /// Cached postings fetched before this are refetched (reload button).
    static STALE_BEFORE: Cell<SystemTime> = const { Cell::new(UNIX_EPOCH) };
    static LISTENERS: RefCell<Vec<Box<dyn Fn() -> bool>>> = RefCell::new(Vec::new());
    static ENGLISH: Cell<bool> = const { Cell::new(false) };
}

/// Whether the card texts are English - set per render from the article's
/// `lang:`.
pub fn set_english(english: bool) {
    ENGLISH.with(|e| e.set(english));
}

/// Registers a preview to render again once fetched postings arrive. The
/// callback returns `false` once its preview is gone, and is dropped then.
/// Nothing is ever fetched while no callback is registered.
pub fn on_loaded(callback: impl Fn() -> bool + 'static) {
    LISTENERS.with(|l| l.borrow_mut().push(Box::new(callback)));
}

/// The preview's reload button: every posting shown is fetched again (the
/// cached one stays on screen until the new one is in), failed ones too.
pub fn refresh_all() {
    STALE_BEFORE.with(|s| s.set(SystemTime::now()));
    CACHE.with(|c| c.borrow_mut().retain(|_, cached| !matches!(cached.entry, Entry::Failed(_))));
}

/// The card for a posting URL - `None` for any other URL. While the
/// posting is still loading (or failed) it's a placeholder card saying so.
pub fn card(url: &str) -> Option<String> {
    let posting = parse_url(url)?;
    let key = posting.key();
    let cached = CACHE.with(|c| c.borrow().get(&key).map(|cached| (cached.entry.clone(), cached.fetched))).or_else(|| {
        let (entry, fetched) = load_from_disk(&key)?;
        CACHE.with(|c| c.borrow_mut().insert(key.clone(), Cached { entry: entry.clone(), fetched }));
        Some((entry, fetched))
    });
    let fresh = cached.as_ref().is_some_and(|(_, fetched)| {
        let stale_before = STALE_BEFORE.with(Cell::get);
        *fetched >= stale_before && fetched.elapsed().map_or(true, |age| age < MAX_AGE)
    });
    if !fresh && !matches!(cached, Some((Entry::Failed(_), _))) {
        want(posting.clone());
    }
    let english = ENGLISH.with(Cell::get);
    Some(match cached {
        Some((Entry::Post(post), _)) => card_html(&post, english),
        Some((Entry::Gone, _)) => gone_html(&posting, url, english),
        Some((Entry::Failed(reason), _)) => placeholder_html(&posting, url, Some(&reason), english),
        None => placeholder_html(&posting, url, None, english),
    })
}

fn want(posting: Posting) {
    let key = posting.key();
    if IN_FLIGHT.with(|f| f.borrow().contains(&key)) || LISTENERS.with(|l| l.borrow().is_empty()) {
        return;
    }
    WANTED.with(|w| {
        let mut wanted = w.borrow_mut();
        if !wanted.iter().any(|p| p.key() == key) {
            wanted.push(posting);
        }
    });
    if !FETCH_SCHEDULED.with(|s| s.replace(true)) {
        // After the current render: `card` runs in the middle of one.
        glib::idle_add_local_once(start_fetch);
    }
}

fn start_fetch() {
    FETCH_SCHEDULED.with(|s| s.set(false));
    let postings: Vec<Posting> = WANTED.with(|w| std::mem::take(&mut *w.borrow_mut()));
    if postings.is_empty() {
        return;
    }
    IN_FLIGHT.with(|f| f.borrow_mut().extend(postings.iter().map(Posting::key)));
    glib::spawn_future_local(async move {
        let fetched = gtk4::gio::spawn_blocking(move || {
            let agent = agent();
            postings.into_iter().map(|p| (p.key(), fetch(&agent, &p))).collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        let now = SystemTime::now();
        for (key, result) in fetched {
            IN_FLIGHT.with(|f| f.borrow_mut().remove(&key));
            let entry = match result {
                Fetched::Post(post) => Entry::Post(post),
                Fetched::Gone => Entry::Gone,
                Fetched::Failed(reason) => {
                    // A posting fetched before stays as it was.
                    if CACHE.with(|c| c.borrow().get(&key).is_some_and(|cached| !matches!(cached.entry, Entry::Failed(_)))) {
                        continue;
                    }
                    Entry::Failed(reason)
                }
            };
            save_to_disk(&key, &entry, now);
            CACHE.with(|c| c.borrow_mut().insert(key, Cached { entry, fetched: now }));
        }
        // A callback may render right away and so call `card` again - no
        // borrow may be held across it.
        let listeners: Vec<Box<dyn Fn() -> bool>> = LISTENERS.with(|l| std::mem::take(&mut *l.borrow_mut()));
        let alive: Vec<Box<dyn Fn() -> bool>> = listeners.into_iter().filter(|callback| callback()).collect();
        LISTENERS.with(|l| {
            let mut l = l.borrow_mut();
            let added = std::mem::take(&mut *l);
            *l = alive;
            l.extend(added);
        });
    });
}

// ---------------------------------------------------------------- disk cache

fn cache_path(key: &str) -> PathBuf {
    let name: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-') { c } else { '_' }).collect();
    let mut path = glib::user_cache_dir();
    path.push(crate::APP_DIR);
    path.push("social");
    path.push(format!("{name}.json"));
    path
}

fn load_from_disk(key: &str) -> Option<(Entry, SystemTime)> {
    let value: Value = serde_json::from_str(&std::fs::read_to_string(cache_path(key)).ok()?).ok()?;
    let fetched = UNIX_EPOCH + Duration::from_secs(value["fetched"].as_u64()?);
    let entry = match &value["post"] {
        Value::Null => Entry::Gone,
        post => Entry::Post(post.clone()),
    };
    Some((entry, fetched))
}

fn save_to_disk(key: &str, entry: &Entry, fetched: SystemTime) {
    let post = match entry {
        Entry::Post(post) => post.clone(),
        Entry::Gone => Value::Null,
        Entry::Failed(_) => return,
    };
    let path = cache_path(key);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let secs = fetched.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let _ = std::fs::write(path, json!({ "fetched": secs, "post": post }).to_string());
}

// ------------------------------------------------------------------ fetching

enum Fetched {
    Post(Value),
    Gone,
    Failed(String),
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .user_agent(concat!("Blocksatz/", env!("CARGO_PKG_VERSION"), " (+https://github.com/linuxundich/blocksatz)"))
        .build()
        .into()
}

/// Status code and JSON body (`Null` if the body isn't JSON).
fn get_json(agent: &ureq::Agent, url: &str) -> Result<(u16, Value), String> {
    let mut response = agent.get(url).header("Accept", "application/json").call().map_err(|err| err.to_string())?;
    let status = response.status().as_u16();
    let body = response.body_mut().read_json::<Value>().unwrap_or(Value::Null);
    Ok((status, body))
}

fn fetch(agent: &ureq::Agent, posting: &Posting) -> Fetched {
    // The reason's text is made at render time, in the article's language.
    let net_error = |detail: String| Fetched::Failed(format!("net:{detail}"));
    match posting {
        Posting::Mastodon { host, id } => {
            let (status, data) = match get_json(agent, &format!("https://{host}/api/v1/statuses/{id}")) {
                Ok(result) => result,
                Err(err) => return net_error(err),
            };
            match status {
                404 | 410 => return Fetched::Gone,
                401 | 403 => return Fetched::Failed("locked".to_string()),
                200 if data["id"].is_string() => {}
                _ => return net_error(format!("HTTP {status}")),
            }
            let status = if data["reblog"].is_object() { &data["reblog"] } else { &data };
            if !matches!(status["visibility"].as_str().unwrap_or("public"), "public" | "unlisted") {
                return Fetched::Gone;
            }
            Fetched::Post(normalize_mastodon(status, host))
        }
        Posting::Bluesky { actor, rkey } => {
            let did = if actor.starts_with("did:") {
                actor.clone()
            } else {
                match get_json(agent, &format!("{BSKY_API}com.atproto.identity.resolveHandle?handle={}", url_encode(actor))) {
                    Err(err) => return net_error(err),
                    Ok((400, _)) => return Fetched::Gone,
                    Ok((200, data)) if data["did"].as_str().is_some_and(|d| d.starts_with("did:")) => data["did"].as_str().unwrap_or_default().to_string(),
                    Ok((status, _)) => return net_error(format!("HTTP {status}")),
                }
            };
            let uri = format!("at://{did}/app.bsky.feed.post/{rkey}");
            let (status, data) = match get_json(agent, &format!("{BSKY_API}app.bsky.feed.getPostThread?uri={}&depth=0&parentHeight=1", url_encode(&uri))) {
                Ok(result) => result,
                Err(err) => return net_error(err),
            };
            if data["error"].as_str().is_some_and(|e| e.contains("NotFound")) {
                return Fetched::Gone;
            }
            if status != 200 {
                return net_error(format!("HTTP {status}"));
            }
            let thread = &data["thread"];
            let kind = thread["$type"].as_str().unwrap_or_default();
            if kind.contains("notFoundPost") || kind.contains("blockedPost") || !thread["post"].is_object() {
                return Fetched::Gone;
            }
            Fetched::Post(normalize_bluesky(thread))
        }
    }
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

// ------------------------------------------------------------- normalizing

/// Only http(s), otherwise empty - addresses out of someone else's answer.
fn clean_url(value: &Value) -> String {
    let url = value.as_str().unwrap_or_default().trim();
    let lower = url.to_ascii_lowercase();
    if (lower.starts_with("https://") || lower.starts_with("http://")) && !url.contains(['"', '<', '>', ' ']) {
        url.to_string()
    } else {
        String::new()
    }
}

fn domain(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or_default();
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default().to_ascii_lowercase();
    host.strip_prefix("www.").map(str::to_string).unwrap_or(host)
}

/// Plain text: tags stripped, whitespace collapsed, cut to `max` chars.
fn plain(value: &Value, max: usize) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    for c in value.as_str().unwrap_or_default().chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => text.push(if c.is_control() { ' ' } else { c }),
            _ => {}
        }
    }
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > max {
        format!("{}…", text.chars().take(max - 1).collect::<String>().trim_end())
    } else {
        text
    }
}

fn lang_code(value: &Value) -> String {
    value.as_str().unwrap_or_default().chars().filter(|c| c.is_ascii_alphabetic() || *c == '-').collect()
}

fn link_card(url: String, title: &Value, image: &Value) -> Value {
    let domain = domain(&url);
    let title = Some(plain(title, 200)).filter(|t| !t.is_empty()).unwrap_or_else(|| domain.clone());
    json!({ "url": url, "title": title, "domain": domain, "image": clean_url(image) })
}

fn normalize_mastodon(s: &Value, host: &str) -> Value {
    let acc = &s["account"];
    let profile = clean_url(&acc["url"]);
    let mut acct = acc["acct"].as_str().or_else(|| acc["username"].as_str()).unwrap_or_default().to_string();
    if !acct.is_empty() && !acct.contains('@') {
        let profile_host = domain(&profile);
        acct = format!("{acct}@{}", if profile_host.is_empty() { host } else { &profile_host });
    }
    let name = Some(plain(&acc["display_name"], 80)).filter(|n| !n.is_empty()).unwrap_or_else(|| plain(&acc["username"], 80));

    let mut html = mastodon_html(s["content"].as_str().unwrap_or_default());
    let mut reply_to = Vec::new();
    if s["in_reply_to_id"].is_string() {
        (html, reply_to) = strip_leading_mentions(&html);
    }

    let media: Vec<Value> = s["media_attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let kind = m["type"].as_str().unwrap_or_default();
            let video = matches!(kind, "video" | "gifv");
            if kind != "image" && !video {
                return None;
            }
            let src = Some(clean_url(if video { &m["preview_url"] } else { &m["url"] })).filter(|u| !u.is_empty()).unwrap_or_else(|| clean_url(&m["preview_url"]));
            let size = &m["meta"]["original"];
            (!src.is_empty()).then(|| json!({ "src": src, "alt": plain(&m["description"], 1500), "video": video, "w": size["width"], "h": size["height"] }))
        })
        .take(4)
        .collect();

    let card = &s["card"];
    let link = if media.is_empty() && !clean_url(&card["url"]).is_empty() { link_card(clean_url(&card["url"]), &card["title"], &card["image"]) } else { Value::Null };

    let url = Some(clean_url(&s["url"])).filter(|u| !u.is_empty()).unwrap_or_else(|| format!("https://{host}/"));
    json!({
        "net": "mastodon",
        "url": url,
        "created": s["created_at"],
        "lang": lang_code(&s["language"]),
        "author": {
            "name": name,
            "handle": format!("@{}", acct.trim_start_matches('@')),
            "profile": profile,
            "avatar": Some(clean_url(&acc["avatar_static"])).filter(|u| !u.is_empty()).unwrap_or_else(|| clean_url(&acc["avatar"])),
        },
        "reply_to": reply_to,
        "cw": plain(&s["spoiler_text"], 300),
        "text": html,
        "media": media,
        "link": link,
        "quote": clean_url(&s["quote"]["quoted_status"]["url"]),
        "poll": s["poll"].is_object(),
    })
}

fn normalize_bluesky(thread: &Value) -> Value {
    let post = &thread["post"];
    let rec = &post["record"];
    let author = &post["author"];
    let handle = author["handle"].as_str().unwrap_or_default().to_ascii_lowercase();
    let actor = if !handle.is_empty() && handle != "handle.invalid" { handle } else { author["did"].as_str().unwrap_or_default().to_string() };
    let rkey = post["uri"].as_str().unwrap_or_default().rsplit('/').next().unwrap_or_default().to_string();
    let name = Some(plain(&author["displayName"], 80)).filter(|n| !n.is_empty()).unwrap_or_else(|| actor.clone());

    let mut reply_to = Vec::new();
    if rec["reply"].is_object() {
        if let Some(parent) = thread["parent"]["post"]["author"]["handle"].as_str().filter(|h| !h.is_empty()) {
            reply_to.push(format!("@{}", parent.to_ascii_lowercase()));
        }
    }

    let mut media = Vec::new();
    let mut link = Value::Null;
    let mut quote = String::new();
    let embed = &post["embed"];
    let parts: Vec<&Value> = if embed["$type"].as_str().unwrap_or_default().contains("recordWithMedia") { vec![&embed["media"], &embed["record"]] } else { vec![embed] };
    for e in parts {
        let kind = e["$type"].as_str().unwrap_or_default();
        if kind.contains("embed.images") {
            for img in e["images"].as_array().into_iter().flatten().take(4) {
                let src = Some(clean_url(&img["fullsize"])).filter(|u| !u.is_empty()).unwrap_or_else(|| clean_url(&img["thumb"]));
                if !src.is_empty() {
                    let ratio = &img["aspectRatio"];
                    media.push(json!({ "src": src, "alt": plain(&img["alt"], 1500), "video": false, "w": ratio["width"], "h": ratio["height"] }));
                }
            }
        } else if kind.contains("embed.video") {
            let src = clean_url(&e["thumbnail"]);
            if !src.is_empty() {
                let ratio = &e["aspectRatio"];
                media.push(json!({ "src": src, "alt": plain(&e["alt"], 1500), "video": true, "w": ratio["width"], "h": ratio["height"] }));
            }
        } else if kind.contains("embed.external") {
            let ext = &e["external"];
            let url = clean_url(&ext["uri"]);
            if !url.is_empty() {
                link = link_card(url, &ext["title"], &ext["thumb"]);
            }
        } else if kind.contains("embed.record") {
            // recordWithMedia nests one level deeper.
            let r = if e["record"]["record"].is_object() { &e["record"]["record"] } else { &e["record"] };
            let uri = r["uri"].as_str().unwrap_or_default();
            let quoted_handle = r["author"]["handle"].as_str().unwrap_or_default();
            let quoted_key = uri.rsplit('/').next().unwrap_or_default();
            if !quoted_handle.is_empty() && !quoted_key.is_empty() && uri.contains("app.bsky.feed.post") {
                quote = format!("https://bsky.app/profile/{}/post/{}", url_encode(quoted_handle), url_encode(quoted_key));
            }
        }
    }

    // Moderation labels count as a content warning, the post's own
    // "!no-unauthenticated" doesn't.
    let flagged = post["labels"].as_array().into_iter().flatten().any(|l| matches!(l["val"].as_str(), Some("porn" | "sexual" | "nudity" | "graphic-media" | "gore")));

    json!({
        "net": "bluesky",
        "url": format!("https://bsky.app/profile/{}/post/{}", url_encode(&actor), url_encode(&rkey)),
        "created": rec["createdAt"].as_str().or_else(|| post["indexedAt"].as_str()).unwrap_or_default(),
        "lang": lang_code(&rec["langs"][0]),
        "author": {
            "name": name,
            "handle": format!("@{actor}"),
            "profile": format!("https://bsky.app/profile/{}", url_encode(&actor)),
            "avatar": clean_url(&author["avatar"]),
        },
        "reply_to": reply_to,
        "cw": if flagged { "!cw" } else { "" },
        "text": bluesky_html(rec["text"].as_str().unwrap_or_default(), rec["facets"].as_array().map(Vec::as_slice).unwrap_or_default()),
        "media": media,
        "link": link,
        "quote": quote,
        "poll": false,
    })
}

fn escape(text: &str) -> String {
    glib::markup_escape_text(text).to_string()
}

/// An `<a>` start tag of the card text: only an http(s) `href`, a class for
/// hashtags and mentions, and `rel`.
fn anchor(href: &str, class: &str) -> String {
    let kind = if class.contains("hashtag") || class.contains("is-tag") {
        " class=\"is-tag\""
    } else if class.contains("mention") {
        " class=\"is-mention\""
    } else {
        ""
    };
    let href = clean_url(&Value::String(href.replace("&amp;", "&")));
    if href.is_empty() {
        format!("<a{kind}>")
    } else {
        format!("<a href=\"{}\"{kind} rel=\"nofollow noopener\">", escape(&href))
    }
}

/// One attribute's value out of a start tag's attribute text.
fn attribute(attrs: &str, name: &str) -> String {
    let lower = attrs.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower[from..].find(name) {
        let at = from + found;
        let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        let after = lower[at + name.len()..].trim_start();
        if before_ok && after.starts_with('=') {
            let value = attrs[attrs.len() - after.len() + 1..].trim_start();
            return match value.chars().next() {
                Some(q @ ('"' | '\'')) => value[1..].split(q).next().unwrap_or_default().to_string(),
                _ => value.split_whitespace().next().unwrap_or_default().to_string(),
            };
        }
        from = at + name.len();
    }
    String::new()
}

/// Mastodon's HTML reduced to `p`, `br` and `a`. A shortened link comes as
/// three spans (invisible, ellipsis, invisible) that Mastodon's own CSS
/// hides - without it the visible part plus "…" stays, as in the plugin.
fn mastodon_html(html: &str) -> String {
    let mut out = String::new();
    // For each open span: whether it is hidden / ends with an ellipsis.
    let mut spans: Vec<(bool, bool)> = Vec::new();
    let hidden = |spans: &[(bool, bool)]| spans.iter().any(|(hidden, _)| *hidden);
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        if !hidden(&spans) {
            out.push_str(&rest[..start]);
        }
        let Some(end) = rest[start..].find('>') else {
            rest = "";
            break;
        };
        let tag = &rest[start + 1..start + end];
        rest = &rest[start + end + 1..];
        let closing = tag.starts_with('/');
        let tag = tag.trim_start_matches('/').trim_end_matches('/');
        let name_len = tag.find(|c: char| c.is_whitespace()).unwrap_or(tag.len());
        let name = tag[..name_len].to_ascii_lowercase();
        let attrs = &tag[name_len..];
        if name == "span" {
            if closing {
                if let Some((was_hidden, ellipsis)) = spans.pop() {
                    if ellipsis && !was_hidden && !hidden(&spans) {
                        out.push('…');
                    }
                }
            } else {
                let class = attribute(attrs, "class");
                let words: Vec<&str> = class.split_whitespace().collect();
                spans.push((words.contains(&"invisible"), words.contains(&"ellipsis")));
            }
            continue;
        }
        if hidden(&spans) {
            continue;
        }
        match (name.as_str(), closing) {
            ("p", false) => out.push_str("<p>"),
            ("p", true) => out.push_str("</p>"),
            ("br", _) => out.push_str("<br>"),
            ("a", false) => out.push_str(&anchor(&attribute(attrs, "href"), &attribute(attrs, "class"))),
            ("a", true) => out.push_str("</a>"),
            _ => {}
        }
    }
    if !hidden(&spans) {
        out.push_str(&rest.replace('>', "&gt;"));
    }
    out.trim().to_string()
}

/// A Mastodon reply starts with a chain of mention links to everyone in
/// the thread - the card names them in "Reply to …" instead.
fn strip_leading_mentions(html: &str) -> (String, Vec<String>) {
    let Some(mut rest) = html.strip_prefix("<p>") else {
        return (html.to_string(), Vec::new());
    };
    let mut names = Vec::new();
    loop {
        let trimmed = rest.trim_start();
        let Some(tag_end) = trimmed.find('>') else { break };
        if !trimmed.starts_with("<a ") || !trimmed[..tag_end].contains("is-mention") {
            break;
        }
        let Some(close) = trimmed.find("</a>") else { break };
        let name = trimmed[tag_end + 1..close].trim();
        // The visible "@user" - a remote account's host stays out.
        let name = format!("@{}", name.trim_start_matches('@').split('@').next().unwrap_or_default());
        if !names.contains(&name) {
            names.push(name);
        }
        rest = &trimmed[close + 4..];
    }
    if names.is_empty() {
        return (html.to_string(), names);
    }
    let mut rest = rest.trim_start();
    while let Some(after) = rest.strip_prefix("<br>") {
        rest = after.trim_start();
    }
    let html = format!("<p>{rest}");
    let html = html.strip_prefix("<p></p>").map(str::to_string).unwrap_or(html);
    (html.trim().to_string(), names)
}

/// Bluesky's plain text plus facets (links, mentions, hashtags) as HTML.
/// Facets count UTF-8 bytes; one that starts or ends inside a character,
/// or overlaps the previous one, is left out.
fn bluesky_html(text: &str, facets: &[Value]) -> String {
    let mut spans: Vec<(usize, usize, &Value)> = facets
        .iter()
        .filter_map(|f| {
            let start = usize::try_from(f["index"]["byteStart"].as_u64()?).ok()?;
            let end = usize::try_from(f["index"]["byteEnd"].as_u64()?).ok()?;
            (start < end && end <= text.len() && text.is_char_boundary(start) && text.is_char_boundary(end)).then_some((start, end, &f["features"][0]))
        })
        .collect();
    spans.sort_by_key(|(start, _, _)| *start);

    let mut out = String::new();
    let mut pos = 0;
    for (start, end, feature) in spans {
        if start < pos {
            continue;
        }
        let kind = feature["$type"].as_str().unwrap_or_default();
        let (href, class) = if kind.ends_with("#link") {
            (clean_url(&feature["uri"]), "")
        } else if kind.ends_with("#mention") {
            (feature["did"].as_str().map(|did| format!("https://bsky.app/profile/{}", url_encode(did))).unwrap_or_default(), "mention")
        } else if kind.ends_with("#tag") {
            (feature["tag"].as_str().map(|tag| format!("https://bsky.app/hashtag/{}", url_encode(tag))).unwrap_or_default(), "hashtag")
        } else {
            (String::new(), "")
        };
        out.push_str(&escape(&text[pos..start]));
        let segment = escape(&text[start..end]);
        if href.is_empty() {
            out.push_str(&segment);
        } else {
            out.push_str(&anchor(&href, class));
            out.push_str(&segment);
            out.push_str("</a>");
        }
        pos = end;
    }
    out.push_str(&escape(&text[pos..]));

    let mut html = String::new();
    let mut para = Vec::new();
    for line in out.trim().split('\n').chain(std::iter::once("")) {
        if line.trim().is_empty() {
            if !para.is_empty() {
                html.push_str(&format!("<p>{}</p>", para.join("<br>")));
                para.clear();
            }
        } else {
            para.push(line.trim_end());
        }
    }
    html
}

// ----------------------------------------------------------------- the card

fn text(key: &str, english: bool) -> &'static str {
    match (key, english) {
        ("view_on", false) => "Auf %s ansehen",
        ("view_on", true) => "View on %s",
        ("reply_to", false) => "Antwort an %s",
        ("reply_to", true) => "Reply to %s",
        ("and", false) => "und",
        ("and", true) => "and",
        ("gone", false) => "Das Posting wurde gelöscht oder ist nicht mehr öffentlich.",
        ("gone", true) => "This post has been deleted or is no longer public.",
        ("orig", false) => "Link zum Original",
        ("orig", true) => "Link to the original",
        ("video", false) => "Video, abspielbar im Original",
        ("video", true) => "Video, plays in the original",
        ("cw", false) => "Inhaltswarnung",
        ("cw", true) => "Content warning",
        ("cw_show", false) => "anzeigen",
        ("cw_show", true) => "show",
        ("quote", false) => "Zitiert ein weiteres Posting:",
        ("quote", true) => "Quotes another post:",
        ("poll", false) => "Enthält eine Umfrage.",
        ("poll", true) => "Includes a poll.",
        ("missing", false) => "Posting auf %s",
        ("missing", true) => "Post on %s",
        ("loading", false) => "wird geladen …",
        ("loading", true) => "loading …",
        ("locked", false) => "Die Instanz gibt das Posting nicht ohne Anmeldung heraus.",
        ("locked", true) => "The instance does not hand out this post without signing in.",
        ("net_error", false) => "Abruf fehlgeschlagen (%s).",
        ("net_error", true) => "Fetching failed (%s).",
        _ => "",
    }
}

/// "@a", "@a und @b", "@a, @b und @c".
fn join_names(names: &[String], english: bool) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} {} {last}", init.join(", "), text("and", english)),
    }
}

const MONTHS_DE: [&str; 12] = ["Januar", "Februar", "März", "April", "Mai", "Juni", "Juli", "August", "September", "Oktober", "November", "Dezember"];
const MONTHS_EN: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// "18. November 2026, 14:05" / "November 18, 2026, 2:05 pm", local time.
fn date_html(iso: &str, with_time: bool, english: bool) -> String {
    let Some(date) = glib::DateTime::from_iso8601(iso, Some(&glib::TimeZone::utc())).ok().and_then(|d| d.to_local().ok()) else {
        return String::new();
    };
    let month = usize::try_from(date.month() - 1).unwrap_or(0).min(11);
    let (day, year, hour, minute) = (date.day_of_month(), date.year(), date.hour(), date.minute());
    let shown = if english {
        let time = format!(", {}:{minute:02} {}", if hour % 12 == 0 { 12 } else { hour % 12 }, if hour < 12 { "am" } else { "pm" });
        format!("{} {day}, {year}{}", MONTHS_EN[month], if with_time { time.as_str() } else { "" })
    } else {
        let time = format!(", {hour:02}:{minute:02}");
        format!("{day}. {} {year}{}", MONTHS_DE[month], if with_time { time.as_str() } else { "" })
    };
    format!("<time datetime=\"{}\">{}</time>", escape(iso), escape(&shown))
}

const ICON_MASTODON: &str = "M23.193 7.879c0-5.206-3.411-6.732-3.411-6.732C18.062.357 15.108.025 12.041 0h-.076c-3.068.025-6.02.357-7.74 1.147 0 0-3.411 1.526-3.411 6.732 0 1.192-.023 2.618.015 4.129.124 5.092.934 10.109 5.641 11.355 2.17.574 4.034.695 5.535.612 2.722-.15 4.25-.972 4.25-.972l-.09-1.975s-1.945.613-4.129.539c-2.165-.074-4.449-.233-4.799-2.891a5.499 5.499 0 0 1-.048-.745s2.125.52 4.817.643c1.646.075 3.19-.097 4.758-.283 3.007-.359 5.625-2.212 5.954-3.905.517-2.665.475-6.507.475-6.507zm-4.024 6.709h-2.497V8.469c0-1.29-.543-1.944-1.628-1.944-1.2 0-1.802.776-1.802 2.312v3.349h-2.483v-3.35c0-1.536-.602-2.312-1.802-2.312-1.085 0-1.628.655-1.628 1.944v6.119H4.832V8.284c0-1.289.328-2.313.987-3.07.68-.758 1.569-1.146 2.674-1.146 1.278 0 2.246.491 2.886 1.474L12 6.585l.622-1.043c.64-.983 1.608-1.474 2.886-1.474 1.104 0 1.994.388 2.674 1.146.658.757.986 1.781.986 3.07v6.304z";
const ICON_BLUESKY: &str = "M12 10.8c-1.087-2.114-4.046-6.053-6.798-7.995C2.566.944 1.561 1.266.902 1.565.139 1.908 0 3.08 0 3.768c0 .69.378 5.65.624 6.479.815 2.736 3.713 3.66 6.383 3.364.136-.02.275-.039.415-.056-.138.022-.276.04-.415.056-3.912.58-7.387 2.005-2.83 7.078 5.013 5.19 6.87-1.113 7.823-4.308.953 3.195 2.05 9.271 7.733 4.308 4.267-4.308 1.172-6.498-2.74-7.078a8.741 8.741 0 0 1-.415-.056c.14.017.279.036.415.056 2.67.297 5.568-.628 6.383-3.364.246-.828.624-5.79.624-6.478 0-.69-.139-1.861-.902-2.206-.659-.298-1.664-.62-4.3 1.24C16.046 4.748 13.087 8.687 12 10.8Z";

fn net_tag(bluesky: bool) -> String {
    let (icon, label) = if bluesky { (ICON_BLUESKY, "Bluesky") } else { (ICON_MASTODON, "Mastodon") };
    format!("<span class=\"lui-social-net\"><svg viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"{icon}\"/></svg><span>{label}</span></span>")
}

const REL: &str = " rel=\"nofollow noopener\"";

fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}

/// The plugin's card (`lui_social_card_html`), with the networks' own
/// image addresses where the blog has its local copies.
fn card_html(s: &Value, english: bool) -> String {
    let bluesky = str_of(s, "net") == "bluesky";
    let net = if bluesky { "bluesky" } else { "mastodon" };
    let url = str_of(s, "url");
    let a = &s["author"];

    let avatar = match str_of(a, "avatar") {
        "" => "<span class=\"lui-social-avatar\" aria-hidden=\"true\"></span>".to_string(),
        src => format!("<img class=\"lui-social-avatar\" src=\"{}\" alt=\"\" width=\"44\" height=\"44\">", escape(src)),
    };
    let name = match str_of(a, "profile") {
        "" => format!("<span class=\"lui-social-name\">{}</span>", escape(str_of(a, "name"))),
        profile => format!("<a class=\"lui-social-name\" href=\"{}\"{REL}>{}</a>", escape(profile), escape(str_of(a, "name"))),
    };
    let head = format!(
        "<header class=\"lui-social-head\">{avatar}<span class=\"lui-social-who\">{name}<span class=\"lui-social-handle\">{}</span></span>{}</header>",
        escape(str_of(a, "handle")),
        net_tag(bluesky)
    );

    let reply_names: Vec<String> = s["reply_to"].as_array().into_iter().flatten().filter_map(|n| n.as_str().map(str::to_string)).collect();
    let reply = if reply_names.is_empty() {
        String::new()
    } else {
        format!("<p class=\"lui-social-reply\">{}</p>", escape(&text("reply_to", english).replace("%s", &join_names(&reply_names, english))))
    };

    let lang = match str_of(s, "lang") {
        "" => String::new(),
        lang => format!(" lang=\"{}\"", escape(lang)),
    };
    let body_text = str_of(s, "text");
    let mut content = if body_text.trim().is_empty() { String::new() } else { format!("<blockquote class=\"lui-social-text\"{lang}>{body_text}</blockquote>") };
    if s["poll"].as_bool() == Some(true) {
        content.push_str(&format!("<p class=\"lui-social-extra\">{}</p>", escape(text("poll", english))));
    }

    let items = s["media"].as_array().cloned().unwrap_or_default();
    if !items.is_empty() {
        content.push_str(&format!("<div class=\"lui-social-media{}\">", if items.len() > 1 { " is-grid" } else { "" }));
        for m in &items {
            let size = match (m["w"].as_u64(), m["h"].as_u64()) {
                (Some(w), Some(h)) if w > 0 && h > 0 => format!(" width=\"{w}\" height=\"{h}\""),
                _ => String::new(),
            };
            let img = format!("<img src=\"{}\"{size} alt=\"{}\">", escape(str_of(m, "src")), escape(str_of(m, "alt")));
            if m["video"].as_bool() == Some(true) {
                content.push_str(&format!("<a class=\"lui-social-video\" href=\"{}\"{REL} aria-label=\"{}\">{img}</a>", escape(url), escape(text("video", english))));
            } else {
                content.push_str(&img);
            }
        }
        content.push_str("</div>");
    }

    let link = &s["link"];
    if !str_of(link, "url").is_empty() {
        let image = match str_of(link, "image") {
            "" => String::new(),
            src => format!("<img src=\"{}\" alt=\"\">", escape(src)),
        };
        content.push_str(&format!(
            "<a class=\"lui-social-link\" href=\"{}\"{REL}>{image}<span class=\"t\">{}</span><span class=\"d\">{}</span></a>",
            escape(str_of(link, "url")),
            escape(str_of(link, "title")),
            escape(str_of(link, "domain"))
        ));
    }

    let quote = str_of(s, "quote");
    if !quote.is_empty() {
        content.push_str(&format!("<p class=\"lui-social-extra\">{} <a href=\"{}\"{REL}>{}</a></p>", escape(text("quote", english)), escape(quote), escape(&domain(quote))));
    }

    let cw = match str_of(s, "cw") {
        // Bluesky's moderation labels: the text depends on the language.
        "!cw" => text("cw", english).to_string(),
        cw => cw.to_string(),
    };
    if !cw.is_empty() {
        content = format!(
            "<details class=\"lui-social-cw\"><summary><span>{}</span> <span class=\"lui-social-cw-show\">{}</span></summary>{content}</details>",
            escape(&cw),
            escape(text("cw_show", english))
        );
    }

    let where_ = if bluesky { "Bluesky".to_string() } else { domain(url) };
    let foot = format!(
        "<footer class=\"lui-social-foot\">{}<a href=\"{}\"{REL}>{}</a></footer>",
        date_html(str_of(s, "created"), true, english),
        escape(url),
        escape(&text("view_on", english).replace("%s", &where_))
    );

    format!("<figure class=\"lui-social is-{net}\">{head}{reply}{content}{foot}</figure>")
}

/// A deleted or no longer public posting - the plugin's dashed card.
fn gone_html(posting: &Posting, url: &str, english: bool) -> String {
    let net = if posting.is_bluesky() { "bluesky" } else { "mastodon" };
    format!(
        "<figure class=\"lui-social is-{net} is-gone\"><header class=\"lui-social-head\"><span class=\"lui-social-who\"></span>{}</header><div class=\"lui-social-text\"><p>{}</p></div><footer class=\"lui-social-foot\"><span></span><a href=\"{}\"{REL}>{}</a></footer></figure>",
        net_tag(posting.is_bluesky()),
        escape(text("gone", english)),
        escape(url),
        escape(text("orig", english))
    )
}

/// While loading, or after a failed fetch: the card's frame with the
/// network, the URL and what's going on.
fn placeholder_html(posting: &Posting, url: &str, failure: Option<&str>, english: bool) -> String {
    let net = if posting.is_bluesky() { "bluesky" } else { "mastodon" };
    let label = match posting {
        Posting::Bluesky { .. } => "Bluesky".to_string(),
        Posting::Mastodon { host, .. } => host.clone(),
    };
    let status = match failure {
        None => text("loading", english).to_string(),
        Some("locked") => text("locked", english).to_string(),
        Some(reason) => text("net_error", english).replace("%s", reason.strip_prefix("net:").unwrap_or(reason)),
    };
    format!(
        "<figure class=\"lui-social is-{net} is-gone is-loading\"><header class=\"lui-social-head\"><span class=\"lui-social-who\"><span class=\"lui-social-name\">{}</span></span>{}</header><div class=\"lui-social-text\"><p>{}</p></div><footer class=\"lui-social-foot\"><span></span><a href=\"{}\"{REL}>{}</a></footer></figure>",
        escape(&text("missing", english).replace("%s", &label)),
        net_tag(posting.is_bluesky()),
        escape(&status),
        escape(url),
        escape(&text("view_on", english).replace("%s", &if posting.is_bluesky() { "Bluesky".to_string() } else { label.clone() }))
    )
}

/// The plugin's `karte.css`, with the blog's theme colors replaced by
/// neutrals that work in every preview style, light and dark, and sizes
/// relative to the preview's text.
pub const CSS: &str = ".lui-social { --ls-surface: rgba(127,127,127,.08); --ls-surface-2: rgba(127,127,127,.2); --ls-bg: rgba(127,127,127,.05); --ls-line: rgba(127,127,127,.3); --ls-muted: color-mix(in srgb, currentColor 62%, transparent); box-sizing: border-box; max-width: 100%; margin: 1.75rem 0; padding: 1.35rem; background: var(--ls-surface); border: 1px solid var(--ls-line); border-radius: 12px; font-size: 1em; line-height: 1.6; text-align: left; text-indent: 0; }
.lui-social *, .lui-social *::before, .lui-social *::after { box-sizing: border-box; }
.lui-social-head { display: flex; align-items: center; gap: 12px; margin: 0 0 1rem; }
.lui-social-avatar { display: block; flex: none; width: 44px; height: 44px; margin: 0; border-radius: 50%; background: var(--ls-surface-2); }
.lui-social-who { flex: 1; min-width: 0; line-height: 1.3; }
.lui-social .lui-social-name { display: block; overflow: hidden; color: inherit; font-weight: 600; text-decoration: none; text-overflow: ellipsis; white-space: nowrap; }
.lui-social a.lui-social-name:hover { text-decoration: underline; }
.lui-social-handle { display: block; overflow: hidden; color: var(--ls-muted); font-size: .85em; text-overflow: ellipsis; white-space: nowrap; }
.lui-social-net { display: flex; flex: none; align-items: center; gap: 6px; color: var(--ls-muted); font-size: .75em; font-weight: 700; letter-spacing: .04em; }
.lui-social-net svg { width: 18px; height: 18px; fill: currentColor; }
.lui-social-reply, .lui-social-extra { margin: 0 0 .5rem; color: var(--ls-muted); font-size: .85em; text-indent: 0; }
.lui-social-extra { margin: .75rem 0 0; }
.lui-social blockquote.lui-social-text { margin: 0; padding: 0; border: 0; background: none; color: inherit; font: inherit; font-style: normal; quotes: none; }
.lui-social blockquote.lui-social-text::before, .lui-social blockquote.lui-social-text::after { content: none; display: none; }
.lui-social-text p { margin: 0 0 .75em; font-size: inherit; text-indent: 0; overflow-wrap: anywhere; }
.lui-social-text p:last-child { margin-bottom: 0; }
.lui-social-text a { text-decoration-thickness: 1px; text-underline-offset: 2px; }
.lui-social-text a.is-tag, .lui-social-text a.is-mention { text-decoration: none; }
.lui-social-text a.is-tag:hover, .lui-social-text a.is-mention:hover { text-decoration: underline; }
.lui-social-media { margin: 1rem 0 0; }
.lui-social-media.is-grid { display: grid; grid-template-columns: 1fr 1fr; gap: .5rem; }
.lui-social-media img { display: block; width: 100%; max-width: 100%; height: auto; margin: 0; border-radius: 4px; }
.lui-social-media.is-grid img { height: 100%; aspect-ratio: 4 / 3; object-fit: cover; }
.lui-social-video { position: relative; display: block; }
.lui-social-video::after { content: \"\"; position: absolute; top: 50%; left: 50%; width: 52px; height: 52px; transform: translate(-50%, -50%); border-radius: 50%; background: rgba(0,0,0,.55) url(\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='%23ffffff'%3E%3Cpath d='M8 5v14l11-7z'/%3E%3C/svg%3E\") no-repeat center / 22px; pointer-events: none; }
.lui-social .lui-social-link { display: block; overflow: hidden; margin: 1rem 0 0; background: var(--ls-bg); border: 1px solid var(--ls-line); border-radius: 4px; color: inherit; text-decoration: none; }
.lui-social .lui-social-link:hover { border-color: var(--ls-muted); }
.lui-social-link img { display: block; width: 100%; height: auto; margin: 0; aspect-ratio: 1.91 / 1; object-fit: cover; }
.lui-social-link span { display: block; padding: .75rem 1rem; }
.lui-social-link .t { font-weight: 600; line-height: 1.35; }
.lui-social-link .d { padding-top: 0; color: var(--ls-muted); font-size: .85em; }
.lui-social-cw summary { cursor: pointer; font-weight: 600; }
.lui-social-cw summary .lui-social-cw-show { font-size: .85em; font-weight: 400; }
.lui-social-cw[open] summary { margin-bottom: .75rem; }
.lui-social-foot { display: flex; flex-wrap: wrap; justify-content: space-between; gap: 4px 1rem; margin: 1rem 0 0; padding: .75rem 0 0; border-top: 1px solid var(--ls-line); color: var(--ls-muted); font-size: .85em; line-height: 1.4; }
.lui-social-foot a { font-weight: 600; text-decoration: none; }
.lui-social-foot a:hover { text-decoration: underline; }
.lui-social.is-gone { background: transparent; border-style: dashed; }
.lui-social.is-gone .lui-social-text { color: var(--ls-muted); font-size: .85em; }
.lui-social.is-gone .lui-social-text p { margin: 0; }
.lui-social.is-loading .lui-social-name { font-weight: 600; }";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_url_recognizes_the_plugins_addresses() {
        assert_eq!(
            parse_url("https://muenchen.social/@tetrapyloctomist/117559248725982793"),
            Some(Posting::Mastodon { host: "muenchen.social".into(), id: "117559248725982793".into() })
        );
        assert_eq!(parse_url("https://chaos.social/@s_bergmann@example.org/117621863670008466/").map(|p| p.key()), Some("mastodon:chaos.social:117621863670008466".into()));
        assert_eq!(parse_url("https://mastodon.social/users/Gargron/statuses/123456789?x=1").map(|p| p.key()), Some("mastodon:mastodon.social:123456789".into()));
        assert_eq!(parse_url("https://bsky.app/profile/Linuxundich.de/post/3lbtkeoq2pc2k").map(|p| p.key()), Some("bluesky:linuxundich.de:3lbtkeoq2pc2k".into()));
        assert_eq!(parse_url("https://bsky.app/profile/did:plc:abc123/post/3lbtkeoq2pc2k").map(|p| p.key()), Some("bluesky:did:plc:abc123:3lbtkeoq2pc2k".into()));
    }

    #[test]
    fn parse_url_ignores_other_addresses() {
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://muenchen.social/@tetrapyloctomist",
            "https://muenchen.social/@tetrapyloctomist/12345",
            "https://example.org/blog/2025/01/03/eintrag",
            "https://bsky.app/profile/linuxundich.de",
            "ftp://muenchen.social/@a/1234567",
        ] {
            assert_eq!(parse_url(url), None, "{url}");
        }
    }

    #[test]
    fn mastodon_html_keeps_only_paragraphs_breaks_and_links() {
        let html = r#"<p>Hallo <strong>Welt</strong><br/><a href="https://example.org/a?b=1&amp;c=2" class="foo" onclick="x()" rel="me"><span class="invisible">https://</span><span class="ellipsis">example.org/a</span><span class="invisible">?b=1&amp;c=2</span></a> <a href="https://x.social/tags/linux" class="mention hashtag" rel="tag">#<span>linux</span></a></p><script>alert(1)</script>"#;
        assert_eq!(
            mastodon_html(html),
            "<p>Hallo Welt<br><a href=\"https://example.org/a?b=1&amp;c=2\" rel=\"nofollow noopener\">example.org/a…</a> <a href=\"https://x.social/tags/linux\" class=\"is-tag\" rel=\"nofollow noopener\">#linux</a></p>alert(1)"
        );
    }

    #[test]
    fn mastodon_html_drops_unsafe_links() {
        assert_eq!(mastodon_html(r#"<p><a href="javascript:alert(1)">x</a></p>"#), "<p><a>x</a></p>");
    }

    #[test]
    fn strip_leading_mentions_moves_them_to_the_reply_line() {
        let html = mastodon_html(r#"<p><span class="h-card"><a href="https://a.social/@anna" class="u-url mention">@<span>anna</span></a></span> <span class="h-card"><a href="https://b.social/@bert" class="u-url mention">@<span>bert</span></a></span> Genau so.</p><p>Zweiter Absatz</p>"#);
        let (html, names) = strip_leading_mentions(&html);
        assert_eq!(names, vec!["@anna".to_string(), "@bert".to_string()]);
        assert_eq!(html, "<p>Genau so.</p><p>Zweiter Absatz</p>");
    }

    #[test]
    fn bluesky_html_links_facets_and_builds_paragraphs() {
        let text = "Grüße an @anna.bsky.social\nZeile zwei\n\nSiehe example.org #linux";
        let mention_start = text.find('@').unwrap();
        let link_start = text.find("example.org").unwrap();
        let tag_start = text.find('#').unwrap();
        let facets = vec![
            json!({ "index": { "byteStart": mention_start, "byteEnd": mention_start + 17 }, "features": [{ "$type": "app.bsky.richtext.facet#mention", "did": "did:plc:anna" }] }),
            json!({ "index": { "byteStart": link_start, "byteEnd": link_start + 11 }, "features": [{ "$type": "app.bsky.richtext.facet#link", "uri": "https://example.org/" }] }),
            json!({ "index": { "byteStart": tag_start, "byteEnd": tag_start + 6 }, "features": [{ "$type": "app.bsky.richtext.facet#tag", "tag": "linux" }] }),
            // Inside the "ü" - left out.
            json!({ "index": { "byteStart": 3, "byteEnd": 5 }, "features": [{ "$type": "app.bsky.richtext.facet#link", "uri": "https://evil.example/" }] }),
        ];
        assert_eq!(
            bluesky_html(text, &facets),
            "<p>Grüße an <a href=\"https://bsky.app/profile/did%3Aplc%3Aanna\" class=\"is-mention\" rel=\"nofollow noopener\">@anna.bsky.social</a><br>Zeile zwei</p><p>Siehe <a href=\"https://example.org/\" rel=\"nofollow noopener\">example.org</a> <a href=\"https://bsky.app/hashtag/linux\" class=\"is-tag\" rel=\"nofollow noopener\">#linux</a></p>"
        );
    }

    fn mastodon_status() -> Value {
        json!({
            "id": "117559248725982793",
            "url": "https://muenchen.social/@tetrapyloctomist/117559248725982793",
            "created_at": "2026-11-12T09:30:00.000Z",
            "language": "de",
            "visibility": "public",
            "spoiler_text": "",
            "content": "<p>Text mit <b>Fett</b> &amp; Umlauten: äöü</p>",
            "account": { "acct": "tetrapyloctomist", "username": "tetrapyloctomist", "display_name": "Axel & <b>Co</b>", "url": "https://muenchen.social/@tetrapyloctomist", "avatar_static": "https://files.muenchen.social/a.png" },
            "media_attachments": [{ "type": "image", "url": "https://files.muenchen.social/b.jpg", "description": "Ein \"Bild\"", "meta": { "original": { "width": 800, "height": 600 } } }],
            "card": null,
            "poll": null
        })
    }

    #[test]
    fn mastodon_card_shows_author_text_media_and_link_to_the_original() {
        let post = normalize_mastodon(&mastodon_status(), "muenchen.social");
        assert_eq!(post["author"]["handle"], "@tetrapyloctomist@muenchen.social");
        let card = card_html(&post, false);
        assert!(card.starts_with("<figure class=\"lui-social is-mastodon\">"), "{card}");
        assert!(card.contains(">Axel &amp; Co</a>"), "{card}");
        assert!(card.contains("<blockquote class=\"lui-social-text\" lang=\"de\"><p>Text mit Fett &amp; Umlauten: äöü</p></blockquote>"), "{card}");
        assert!(card.contains("<img src=\"https://files.muenchen.social/b.jpg\" width=\"800\" height=\"600\" alt=\"Ein &quot;Bild&quot;\">"), "{card}");
        assert!(card.contains(">Auf muenchen.social ansehen</a>"), "{card}");
        assert!(card.contains("2026, "), "{card}");
        assert!(card_html(&post, true).contains(">View on muenchen.social</a>"));
    }

    #[test]
    fn bluesky_card_has_external_link_preview() {
        let thread = json!({
            "post": {
                "uri": "at://did:plc:x/app.bsky.feed.post/3lbtkeoq2pc2k",
                "author": { "did": "did:plc:x", "handle": "Linuxundich.de", "displayName": "", "avatar": "https://cdn.bsky.app/a.jpg" },
                "record": { "text": "Neuer Artikel", "createdAt": "2026-11-18T10:00:00Z", "langs": ["de"] },
                "embed": { "$type": "app.bsky.embed.external#view", "external": { "uri": "https://linuxundich.de/x/", "title": "", "thumb": "https://cdn.bsky.app/t.jpg" } },
                "labels": []
            }
        });
        let post = normalize_bluesky(&thread);
        assert_eq!(post["url"], "https://bsky.app/profile/linuxundich.de/post/3lbtkeoq2pc2k");
        assert_eq!(post["author"]["name"], "linuxundich.de");
        let card = card_html(&post, false);
        assert!(card.contains("<span class=\"t\">linuxundich.de</span><span class=\"d\">linuxundich.de</span>"), "{card}");
        assert!(card.contains(">Auf Bluesky ansehen</a>"), "{card}");
    }

    #[test]
    fn card_without_cache_is_a_loading_placeholder_and_fetches_nothing() {
        let url = "https://example-instance.test/@nobody/999999999999";
        let html = card(url).expect("a posting URL");
        assert!(html.contains("is-loading") && html.contains("wird geladen"), "{html}");
        // No preview registered in tests: nothing queued.
        assert!(WANTED.with(|w| w.borrow().is_empty()));
        assert_eq!(card("https://www.youtube.com/watch?v=dQw4w9WgXcQ"), None);
    }

    #[test]
    fn join_names_reads_naturally() {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(join_names(&names(&["@a"]), false), "@a");
        assert_eq!(join_names(&names(&["@a", "@b", "@c"]), false), "@a, @b und @c");
        assert_eq!(join_names(&names(&["@a", "@b"]), true), "@a and @b");
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Fetches real postings and writes a page with their cards - by hand:
    /// `SOCIAL_LIVE_OUT=/tmp/x.html cargo test --bin blocksatz live_cards -- --ignored`
    #[test]
    #[ignore = "network"]
    fn live_cards() {
        let agent = agent();
        let mut cards = String::new();
        for url in std::env::var("SOCIAL_LIVE_URLS").unwrap_or_default().split_whitespace() {
            let posting = parse_url(url).expect("posting URL");
            cards.push_str(&match fetch(&agent, &posting) {
                Fetched::Post(post) => card_html(&post, false),
                Fetched::Gone => gone_html(&posting, url, false),
                Fetched::Failed(reason) => placeholder_html(&posting, url, Some(&reason), false),
            });
        }
        let page = format!("<!doctype html><meta charset=utf-8><style>body {{ font-family: Cantarell, sans-serif; max-width: 46rem; margin: 2rem auto; color: #e3e3e3; background: #1e1e1e; }} a {{ color: #78aeed; }} {CSS}</style>{cards}");
        std::fs::write(std::env::var("SOCIAL_LIVE_OUT").unwrap(), page).unwrap();
    }
}
