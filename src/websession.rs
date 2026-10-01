//! The one `WebKit` network session shared by the Browser tab and the
//! export dialog's Live-Vorschau - the only two views in the app that
//! visit the WordPress site rather than rendering local content.
//!
//! WebKit keeps cookies in memory unless a cookie manager has been handed
//! persistent storage explicitly, and a `WebView` built without a session
//! of its own gets WebKit's default one, which nobody ever configures.
//! That was visible in the publish flow: an unpublished post is only shown
//! to a session logged into wp-admin as someone allowed to edit it, so
//! `preview=true` on a draft's permalink returned the site's own 404 page
//! to anyone else - and even after logging in through the Browser tab, the
//! cookie was gone again with the next start of the app.
//!
//! Sharing one session means a single login in the Browser tab also counts
//! for the preview, and the SQLite cookie jar under `blocksatz/webkit`
//! means it keeps counting across restarts.

use std::cell::OnceCell;
use std::path::PathBuf;

use gtk4::glib;

fn data_dir() -> PathBuf {
    let mut dir = glib::user_data_dir();
    dir.push(crate::APP_DIR);
    dir.push("webkit");
    dir
}

fn cache_dir() -> PathBuf {
    let mut dir = glib::user_cache_dir();
    dir.push(crate::APP_DIR);
    dir.push("webkit");
    dir
}

thread_local! {
    static SESSION: OnceCell<webkit6::NetworkSession> = const { OnceCell::new() };
}

/// The session every `WebView` that talks to the live site is built with -
/// created on first use, then kept for the rest of the process. Cheap to
/// call repeatedly; the returned handle is a reference to the same session.
pub fn shared() -> webkit6::NetworkSession {
    SESSION.with(|cell| {
        cell.get_or_init(|| {
            let data = data_dir();
            let cache = cache_dir();
            // Without these, WebKit silently falls back to an in-memory
            // session and the cookie jar below never reaches the disk.
            let _ = std::fs::create_dir_all(&data);
            let _ = std::fs::create_dir_all(&cache);

            let session = webkit6::NetworkSession::new(data.to_str(), cache.to_str());
            if let Some(cookies) = session.cookie_manager() {
                let mut jar = data;
                jar.push("cookies.sqlite");
                if let Some(jar) = jar.to_str() {
                    cookies.set_persistent_storage(jar, webkit6::CookiePersistentStorage::Sqlite);
                }
            }
            session
        })
        .clone()
    })
}
