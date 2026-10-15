//! What a blog offers beyond plain WordPress, found out once per blog and
//! app run - so the UI offers only fields that the blog actually keeps.
//! For now: whether Rank Math is active. Its SEO fields are otherwise sent
//! and silently dropped by WordPress.

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
            .and_then(|password| wpclient::Client::new(&site.url, &site.username, &password).namespaces().ok())
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
}
