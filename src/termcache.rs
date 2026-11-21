//! On-disk + in-memory cache of WordPress categories/tags, so the
//! properties dialog's autocomplete (`autocomplete.rs`) has something to
//! show immediately - at app startup, before any network round trip
//! completes - rather than starting empty every time the dialog opens.
//! Refreshable automatically at startup and on demand (a button in the
//! properties dialog calls `spawn_refresh` again).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
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

/// A view that wants to hear about new term lists (`connect_changed`).
type ChangedListener = Weak<dyn Fn()>;

/// The shared, in-memory term lists of one blog, created at startup
/// (`window.rs`) and pointed at the open document's blog by `use_site` -
/// bundled to keep functions that pass them around (they're always
/// read/refreshed together) under
/// clippy::too_many_arguments, the same fix already used for `DocContext`/
/// `RecentFilesWidgets` in `window.rs`. Cloning the whole bundle is as
/// cheap as cloning any one field, since every field already is one.
#[derive(Clone)]
pub struct TermCacheHandles {
    pub categories: Rc<RefCell<Vec<String>>>,
    pub tags: Rc<RefCell<Vec<String>>>,
    pub category_slugs: Rc<RefCell<HashMap<String, String>>>,
    /// `site_id` of the blog the lists above belong to - the blog of the
    /// open document (`use_site`), not necessarily the active one: an
    /// `artikel.en.md` is checked against the English blog's tags.
    pub site: Rc<RefCell<String>>,
    /// Called after the lists changed (another blog, or a refresh from
    /// WordPress arrived), so views showing "exists / new" can redraw.
    pub changed: Rc<RefCell<Vec<ChangedListener>>>,
}

impl TermCacheHandles {
    /// Handles holding `site`'s on-disk cache.
    pub fn for_site(site: &wpsite::SiteConfig) -> Self {
        let id = site.site_id();
        let cache = load_site(&id);
        TermCacheHandles {
            categories: Rc::new(RefCell::new(cache.categories)),
            tags: Rc::new(RefCell::new(cache.tags)),
            category_slugs: Rc::new(RefCell::new(cache.category_slugs)),
            site: Rc::new(RefCell::new(id)),
            changed: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// Registers `listener` for `changed`; it lives as long as the caller
    /// keeps the returned `Rc`.
    pub fn connect_changed(&self, listener: impl Fn() + 'static) -> Rc<dyn Fn()> {
        let listener: Rc<dyn Fn()> = Rc::new(listener);
        let mut listeners = self.changed.borrow_mut();
        listeners.retain(|l| l.strong_count() > 0);
        listeners.push(Rc::downgrade(&listener));
        listener
    }

    fn notify(&self) {
        let listeners: Vec<Rc<dyn Fn()>> = self.changed.borrow().iter().filter_map(Weak::upgrade).collect();
        for listener in listeners {
            listener();
        }
    }

    fn apply(&self, cache: TermCache) {
        *self.categories.borrow_mut() = cache.categories;
        *self.tags.borrow_mut() = cache.tags;
        *self.category_slugs.borrow_mut() = cache.category_slugs;
        self.notify();
    }
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

/// One cache per blog: `terms-<site id>.json`.
fn cache_path(site_id: &str) -> PathBuf {
    let site_id: String = site_id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    let mut dir = glib::user_cache_dir();
    dir.push(crate::APP_DIR);
    dir.push(format!("terms-{site_id}.json"));
    dir
}

/// Points the shared lists at `site` - the blog of the document just
/// opened (`wpsite::for_document`). Loads that blog's on-disk cache at
/// once and refreshes it from WordPress. Nothing happens if the lists
/// already belong to `site`.
pub fn use_site(handles: &TermCacheHandles, site: &wpsite::SiteConfig) {
    let id = site.site_id();
    if *handles.site.borrow() == id {
        return;
    }
    *handles.site.borrow_mut() = id.clone();
    handles.apply(load_site(&id));
    spawn_refresh(handles);
}

/// The blog `handles` belongs to (the active one if none is set yet).
pub fn site_of(handles: &TermCacheHandles) -> wpsite::SiteConfig {
    let id = handles.site.borrow().clone();
    wpsite::for_site_id(if id.is_empty() { None } else { Some(id.as_str()) })
}

/// The same lists, but fixed to the blog they belong to right now: a
/// dialog that writes terms keeps talking to the blog it showed, even if
/// another document is opened meanwhile.
pub fn pinned(handles: &TermCacheHandles) -> TermCacheHandles {
    TermCacheHandles { site: Rc::new(RefCell::new(handles.site.borrow().clone())), ..handles.clone() }
}

/// `site_id`'s cache as last saved, empty if there is none.
pub fn load_site(site_id: &str) -> TermCache {
    match std::fs::read_to_string(cache_path(site_id)) {
        Ok(contents) => parse(&contents),
        Err(_) => TermCache::default(),
    }
}

fn save(site_id: &str, cache: &TermCache) -> std::io::Result<()> {
    let path = cache_path(site_id);
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

/// Refreshes the lists from the blog they belong to (`handles.site`) on a
/// background thread (see `wpclient`'s module docs for why it's
/// blocking), updating the shared lists and that blog's on-disk cache once
/// done. A no-op if no blog is configured; leaves the cache untouched on
/// failure, and drops the answer if the lists moved to another blog in
/// the meantime.
pub fn spawn_refresh(handles: &TermCacheHandles) {
    let site = site_of(handles);
    if site.url.is_empty() {
        return;
    }
    let site_id = site.site_id();
    let handles = handles.clone();

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
            let _ = save(&site_id, &cache);
            if *handles.site.borrow() == site_id {
                handles.apply(cache);
            }
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
    fn each_blog_has_its_own_cache_file() {
        let de = cache_path("linuxundich.de");
        let en = cache_path("linuxundich.de/en");
        assert!(de.ends_with("terms-linuxundich.de.json"));
        assert!(en.ends_with("terms-linuxundich.de_en.json"));
        assert_ne!(de, en);
    }

    fn handles(site: &str, tags: &[&str]) -> TermCacheHandles {
        TermCacheHandles {
            categories: Rc::new(RefCell::new(Vec::new())),
            tags: Rc::new(RefCell::new(tags.iter().map(|t| t.to_string()).collect())),
            category_slugs: Rc::new(RefCell::new(HashMap::new())),
            site: Rc::new(RefCell::new(site.to_string())),
            changed: Rc::new(RefCell::new(Vec::new())),
        }
    }

    #[test]
    fn switching_lists_tells_listeners_and_drops_dead_ones() {
        // German tags first, then the English blog's: "Projects" only
        // exists on the English one, "Projekte" only on the German one.
        let h = handles("linuxundich.de", &["Projekte", "Google Play"]);
        assert!(!term_exists("Projects", &h.tags.borrow()));
        let calls = Rc::new(std::cell::Cell::new(0));
        let listener = {
            let calls = calls.clone();
            h.connect_changed(move || calls.set(calls.get() + 1))
        };
        h.apply(TermCache { tags: vec!["Projects".to_string()], ..TermCache::default() });
        assert_eq!(calls.get(), 1);
        assert!(term_exists("Projects", &h.tags.borrow()));
        assert!(!term_exists("Google Play", &h.tags.borrow()));
        drop(listener);
        h.apply(TermCache::default());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn use_site_keeps_the_lists_of_the_same_blog() {
        let h = handles("linuxundich.de/en", &["Projects"]);
        let site = wpsite::SiteConfig { url: "https://linuxundich.de/en".to_string(), ..wpsite::SiteConfig::default() };
        use_site(&h, &site);
        assert_eq!(*h.tags.borrow(), vec!["Projects".to_string()]);
    }

    #[test]
    fn a_pinned_copy_keeps_its_blog_but_shares_the_lists() {
        let main = handles("linuxundich.de/en", &["Projects"]);
        let dialog = pinned(&main);
        *main.site.borrow_mut() = "linuxundich.de".to_string();
        assert_eq!(*dialog.site.borrow(), "linuxundich.de/en");
        main.tags.borrow_mut().push("Projekte".to_string());
        assert!(term_exists("Projekte", &dialog.tags.borrow()));
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
