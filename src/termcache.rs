//! On-disk + in-memory cache of WordPress categories/tags, so the
//! properties dialog's autocomplete (`autocomplete.rs`) has something to
//! show immediately - at app startup, before any network round trip
//! completes - rather than starting empty every time the dialog opens.
//! Refreshable automatically at startup and on demand (a button in the
//! properties dialog calls `spawn_refresh` again).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use gtk4::glib;
use serde_json::Value;

use crate::{secrets, wpclient, wpsite};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TermCache {
    pub categories: Vec<String>,
    pub tags: Vec<String>,
    /// Category name -> its real WordPress slug, which can diverge from
    /// what `document::slugify` would derive from the name (a category's
    /// slug can be edited independently of its display name) - used by the
    /// properties dialog's URL-length check (`properties.rs`), which needs
    /// the real permalink, not a guess.
    pub category_slugs: HashMap<String, String>,
}

/// The three shared, in-memory handles `load()`'s result is unpacked into
/// at startup (`window.rs`) - bundled purely to keep functions that pass
/// all three around (they're always read/refreshed together) under
/// clippy::too_many_arguments, the same fix already used for `DocContext`/
/// `RecentFilesWidgets` in `window.rs`. Cloning the whole bundle is as
/// cheap as cloning any one field, since every field already is one.
#[derive(Clone)]
pub struct TermCacheHandles {
    pub categories: Rc<RefCell<Vec<String>>>,
    pub tags: Rc<RefCell<Vec<String>>>,
    pub category_slugs: Rc<RefCell<HashMap<String, String>>>,
}

/// Adwaita's standard green_5 - used wherever a category/tag is shown as
/// already existing on WordPress (`term_exists`), the same positive
/// semantics `properties.rs`'s URL-length icon already gives its "success"
/// CSS class, just as an inline Pango color instead (a `gtk4::Label`
/// mixing colors per-substring has no CSS-class equivalent).
pub const EXISTING_TERM_COLOR: &str = "#26a269";
/// Adwaita's standard red_4 ("destructive-action" semantics) - used
/// wherever a category/tag would create a brand new WordPress term on
/// publish.
pub const NEW_TERM_COLOR: &str = "#e01b24";

/// True when `term` already exists in `known_terms` (case-insensitive,
/// matching how WordPress itself treats category/tag names, and
/// `export.rs`'s `resolve_or_create_term`, which is what would actually
/// create a new one on publish).
pub fn term_exists(term: &str, known_terms: &[String]) -> bool {
    known_terms.iter().any(|known| known.eq_ignore_ascii_case(term))
}

/// Pango markup for one term, colored `EXISTING_TERM_COLOR` if it already
/// exists (`term_exists`) or `NEW_TERM_COLOR` if publishing it would
/// create a brand new one. Escapes `term` first (`glib::markup_escape_text`)
/// so a name containing `&`/`<`/`>` can't break the markup or be
/// misparsed as a tag.
pub fn term_markup(term: &str, known_terms: &[String]) -> String {
    let color = if term_exists(term, known_terms) { EXISTING_TERM_COLOR } else { NEW_TERM_COLOR };
    format!(r#"<span foreground="{color}">{}</span>"#, glib::markup_escape_text(term))
}

fn cache_path() -> PathBuf {
    let mut dir = glib::user_cache_dir();
    dir.push(crate::APP_DIR);
    dir.push("terms.json");
    dir
}

pub fn load() -> TermCache {
    match std::fs::read_to_string(cache_path()) {
        Ok(contents) => parse(&contents),
        Err(_) => TermCache::default(),
    }
}

fn save(cache: &TermCache) -> std::io::Result<()> {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serialize(cache))
}

fn parse(s: &str) -> TermCache {
    let Ok(value) = serde_json::from_str::<Value>(s) else {
        return TermCache::default();
    };
    let string_list = |key: &str| -> Vec<String> {
        value
            .get(key)
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    };
    let category_slugs = value
        .get("category_slugs")
        .and_then(Value::as_object)
        .map(|map| map.iter().filter_map(|(name, slug)| slug.as_str().map(|slug| (name.clone(), slug.to_string()))).collect())
        .unwrap_or_default();
    TermCache {
        categories: string_list("categories"),
        tags: string_list("tags"),
        category_slugs,
    }
}

fn serialize(cache: &TermCache) -> String {
    serde_json::json!({ "categories": cache.categories, "tags": cache.tags, "category_slugs": cache.category_slugs }).to_string()
}

/// Refreshes the cache from the configured WordPress site on a background
/// thread (see `wpclient`'s module docs for why it's blocking), updating
/// the shared in-memory lists and the on-disk cache once done. A no-op if
/// no site is configured; leaves the existing cache untouched on failure.
pub fn spawn_refresh(handles: &TermCacheHandles) {
    let site = wpsite::load();
    if site.url.is_empty() {
        return;
    }
    let TermCacheHandles { categories, tags, category_slugs } = handles.clone();

    let (tx, rx) = mpsc::channel::<Option<TermCache>>();
    std::thread::spawn(move || {
        let result = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .ok()
            .flatten()
            .map(|password| wpclient::Client::new(&site.url, &site.username, &password))
            .and_then(|client| {
                // `list_terms`, not `list_term_names`, for categories - it
                // also carries each one's real slug (see `TermCache::category_slugs`'s doc comment).
                let category_terms = client.list_terms("categories").ok()?;
                let tags = client.list_term_names("tags").ok()?;
                Some(TermCache {
                    categories: category_terms.iter().map(|t| t.name.clone()).collect(),
                    category_slugs: category_terms.into_iter().map(|t| (t.name, t.slug)).collect(),
                    tags,
                })
            });
        let _ = tx.send(result);
    });

    glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
        Ok(Some(cache)) => {
            *categories.borrow_mut() = cache.categories.clone();
            *tags.borrow_mut() = cache.tags.clone();
            *category_slugs.borrow_mut() = cache.category_slugs.clone();
            let _ = save(&cache);
            glib::ControlFlow::Break
        }
        Ok(None) => glib::ControlFlow::Break,
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_parse_and_serialize() {
        let cache = TermCache {
            categories: vec!["GNU/Linux".to_string(), "Android".to_string()],
            tags: vec!["arch".to_string()],
            category_slugs: HashMap::from([("GNU/Linux".to_string(), "gnu-linux".to_string())]),
        };
        assert_eq!(parse(&serialize(&cache)), cache);
    }

    #[test]
    fn missing_or_corrupt_file_yields_default() {
        assert_eq!(parse(""), TermCache::default());
        assert_eq!(parse("not json"), TermCache::default());
    }

    #[test]
    fn term_exists_matches_case_insensitively() {
        let known = vec!["KI".to_string(), "Hardware".to_string()];
        assert!(term_exists("ki", &known));
        assert!(!term_exists("Terminal", &known));
    }

    #[test]
    fn term_markup_colors_an_existing_term_green_and_a_new_one_red() {
        let known = vec!["KI".to_string()];
        assert_eq!(term_markup("KI", &known), format!(r#"<span foreground="{EXISTING_TERM_COLOR}">KI</span>"#));
        assert_eq!(term_markup("Terminal", &known), format!(r#"<span foreground="{NEW_TERM_COLOR}">Terminal</span>"#));
    }

    #[test]
    fn term_markup_escapes_special_characters() {
        assert_eq!(term_markup("R&D", &[]), format!(r#"<span foreground="{NEW_TERM_COLOR}">R&amp;D</span>"#));
    }
}
