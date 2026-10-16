//! "Neue Version verfügbar": for installs that don't update themselves - the
//! Flatpak bundle from GitHub, the Arch package - Blocksatz asks GitHub for
//! the latest release at most once a day and points to it with a toast.
//! Not in the Flathub build (`BLOCKSATZ_FLATHUB` at build time), where
//! Flathub delivers updates; switchable in Einstellungen → Erscheinungsbild.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gtk4::glib;
use gtk4::prelude::*;

use crate::i18n::tr;

const LATEST_URL: &str = "https://api.github.com/repos/linuxundich/blocksatz/releases/latest";
const ONCE_A_DAY: u64 = 24 * 60 * 60;

/// Whether this build offers the check at all.
pub fn available() -> bool {
    option_env!("BLOCKSATZ_FLATHUB").is_none()
}

fn config_path(name: &str) -> PathBuf {
    let mut path = glib::user_config_dir();
    path.push(crate::APP_DIR);
    path.push(name);
    path
}

pub fn enabled() -> bool {
    available() && std::fs::read_to_string(config_path("update_check.conf")).ok().map(|s| s.trim() != "0").unwrap_or(true)
}

pub fn set_enabled(enabled: bool) {
    let path = config_path("update_check.conf");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, if enabled { "1" } else { "0" });
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Whether a day has passed since the last check (and notes this one).
fn due() -> bool {
    let path = config_path("update_check_last");
    let last = std::fs::read_to_string(&path).ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
    if now().saturating_sub(last) < ONCE_A_DAY {
        return false;
    }
    let _ = std::fs::write(path, now().to_string());
    true
}

/// `(major, minor, patch)` of `v0.73.0` or `0.73.0`.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next().flatten().unwrap_or(0)))
}

/// Whether `latest` is newer than `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    matches!((parse_version(latest), parse_version(current)), (Some(l), Some(c)) if l > c)
}

/// Looks for a newer release in the background and, if there is one,
/// shows a toast with a link to it on `overlay`.
pub fn run(overlay: &adw::ToastOverlay) {
    if !enabled() || !due() {
        return;
    }
    let (tx, rx) = mpsc::channel::<Option<(String, String)>>();
    std::thread::spawn(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).build().into();
        let latest = agent
            .get(LATEST_URL)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", concat!("Blocksatz/", env!("CARGO_PKG_VERSION")))
            .call()
            .ok()
            .and_then(|mut response| response.body_mut().read_json::<serde_json::Value>().ok())
            .and_then(|value| Some((value.get("tag_name")?.as_str()?.to_string(), value.get("html_url")?.as_str()?.to_string())))
            .filter(|(tag, _)| is_newer(tag, env!("CARGO_PKG_VERSION")));
        let _ = tx.send(latest);
    });
    let overlay = overlay.clone();
    glib::timeout_add_local(Duration::from_millis(500), move || match rx.try_recv() {
        Ok(Some((tag, url))) => {
            let toast = adw::Toast::builder().title(tr("Blocksatz {version} ist erschienen.").replace("{version}", tag.trim_start_matches('v'))).button_label(tr("Ansehen")).timeout(0).build();
            let window = overlay.root().and_downcast::<gtk4::Window>();
            toast.connect_button_clicked(move |_| {
                gtk4::UriLauncher::new(&url).launch(window.as_ref(), gtk4::gio::Cancellable::NONE, |_| {});
            });
            overlay.add_toast(toast);
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
    fn versions_compare_by_number() {
        assert!(is_newer("v0.74.0", "0.73.0"));
        assert!(is_newer("v0.73.1", "0.73.0"));
        assert!(is_newer("v1.0.0", "0.99.9"));
        assert!(!is_newer("v0.73.0", "0.73.0"));
        assert!(!is_newer("v0.9.0", "0.73.0"));
        assert!(!is_newer("nightly", "0.73.0"));
    }
}
