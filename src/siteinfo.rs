//! What a blog offers beyond plain WordPress, found out once per blog and
//! app run - so the UI offers only fields that the blog actually keeps.
//! Whether Rank Math is active (its SEO fields are otherwise sent and
//! silently dropped by WordPress), and whether post addresses carry the
//! category (for the URL length check).

use std::collections::HashMap;
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::Duration;

use gtk4::glib;

use crate::{secrets, wpclient, wpsite};

fn cache() -> &'static Mutex<HashMap<String, bool>> {
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Whether Rank Math is active on the blog at `site_url` - `None` until
/// `probe` found out (or when it couldn't).
pub fn rank_math_active(site_url: &str) -> Option<bool> {
    cache().lock().ok()?.get(site_url).copied()
}

fn category_cache() -> &'static Mutex<HashMap<String, bool>> {
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Whether the blog at `site_url` puts the category in front of the slug
/// (`/%category%/%postname%/`) - `None` until `probe` found out.
pub fn category_in_permalink(site_url: &str) -> Option<bool> {
    category_cache().lock().ok()?.get(site_url).copied()
}

/// From a real post address: `Some(false)` for `<site>/<slug>/`,
/// `Some(true)` for `<site>/<something>/<slug>/`, `None` for anything else
/// (dates, `?p=`).
pub fn permalink_has_category(site_url: &str, link: &str, slug: &str) -> Option<bool> {
    let bare = |u: &str| u.split_once("://").map_or(u, |(_, r)| r).trim_end_matches('/').to_string();
    let site = bare(site_url);
    let path = bare(link).strip_prefix(&site)?.trim_matches('/').to_string();
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        [only] if *only == slug => Some(false),
        [first, last] if *last == slug && !first.chars().all(|c| c.is_ascii_digit()) => Some(true),
        _ => None,
    }
}

/// Whether the REST index lists Rank Math's own namespace.
pub fn has_rank_math(namespaces: &[String]) -> bool {
    namespaces.iter().any(|ns| ns.starts_with("rankmath/"))
}

/// Finds out what `site` offers (once per run) and calls `on_known` with
/// the answer on the main loop; right away when it is already known.
pub fn probe(site: &wpsite::SiteConfig, on_known: impl Fn(bool) + 'static) {
    if let Some(active) = rank_math_active(&site.url) {
        on_known(active);
        return;
    }
    if site.url.is_empty() {
        return;
    }
    let site = site.clone();
    let (tx, rx) = mpsc::channel::<Option<bool>>();
    std::thread::spawn(move || {
        let answer = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .ok()
            .flatten()
            .map(|password| wpclient::Client::new(&site.url, &site.username, &password))
            .and_then(|client| {
                if let Some(Some(with_category)) = client.newest_post_link().ok().flatten().map(|(link, slug)| permalink_has_category(&site.url, &link, &slug)) {
                    if let Ok(mut cache) = category_cache().lock() {
                        cache.insert(site.url.clone(), with_category);
                    }
                }
                client.namespaces().ok()
            })
            .map(|namespaces| has_rank_math(&namespaces));
        if let (Some(active), Ok(mut cache)) = (answer, cache().lock()) {
            cache.insert(site.url.clone(), active);
        }
        let _ = tx.send(answer);
    });
    glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
        Ok(Some(active)) => {
            on_known(active);
            glib::ControlFlow::Break
        }
        Ok(None) | Err(mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_math_is_recognized_by_its_namespace() {
        assert!(has_rank_math(&["wp/v2".into(), "rankmath/v1".into()]));
        assert!(!has_rank_math(&["wp/v2".into(), "lui-translations/v1".into()]));
    }

    #[test]
    fn the_permalink_shape_is_read_from_a_post_address() {
        assert_eq!(permalink_has_category("https://blog.example", "https://blog.example/ein-artikel/", "ein-artikel"), Some(false));
        assert_eq!(permalink_has_category("https://blog.example/en", "https://blog.example/en/gnu-linux/an-article/", "an-article"), Some(true));
        assert_eq!(permalink_has_category("https://blog.example", "https://blog.example/2026/10/ein-artikel/", "ein-artikel"), None);
        assert_eq!(permalink_has_category("https://blog.example", "https://blog.example/?p=5", "ein-artikel"), None);
    }
}
