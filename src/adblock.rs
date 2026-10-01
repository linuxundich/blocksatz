//! Basic ad/tracker blocking for the Browser tab (see `browser.rs`),
//! built on the exact same WebKit "content blocker" mechanism GNOME Web
//! (Epiphany) itself uses for its own ad blocker -
//! `WebKitUserContentFilterStore`/`WebKitUserContentManager`, the same
//! rule format Safari's content blockers use, not a bespoke Blocksatz
//! mechanism.
//!
//! Deliberately *not* a hand-coded Rust list of domains: the actual block
//! list lives in `data/adblock/easylist-basic.txt`, written in the real
//! EasyList/Adblock Plus filter syntax
//! (<https://easylist.to/pages/about.html>) - the standard format actual
//! ad-block lists (including the ones GNOME Web itself can subscribe to)
//! are published in. `parse_easylist` below is a real, reusable converter
//! from that syntax to WebKit's own JSON rule format, so updating what's
//! blocked - pasting in more lines from an actual EasyList release, or
//! swapping the whole file for a different list - never needs a code
//! change; only `include_str!`-ing a different/bigger file would.
//! Deliberately "basic" per the request: this bundles a small, fixed
//! subset rather than the full EasyList, which would need its own
//! network fetch/update mechanism - real scope creep beyond what was
//! asked for.
//!
//! WebKit compiles the JSON rule list into its own efficient filter
//! bytecode once (cached on disk under `glib::user_cache_dir()`, keyed by
//! `FILTER_IDENTIFIER`) and re-loads the compiled form on every later
//! launch rather than recompiling from source each time.

use std::path::PathBuf;

use gtk4::{gio, glib};

const FILTER_IDENTIFIER: &str = "blocksatz-basic-adblock";

/// A small, hand-picked subset of real EasyList rules covering well-known
/// ad/tracker network infrastructure - see the module doc comment for why
/// this lives as data in this exact syntax rather than as Rust code.
const EASYLIST_BASIC: &str = include_str!("../data/adblock/easylist-basic.txt");

/// Parses the common `||domain^` "domain anchor" rule from the EasyList/
/// Adblock Plus filter syntax - the form the large majority of network-
/// level (as opposed to element-hiding) ad/tracker-blocking rules in any
/// real EasyList-derived file actually use. Comments (`!`), exception
/// rules (`@@...`), element-hiding rules (containing `#`), and anything
/// with filter options or wildcards this simple converter doesn't
/// attempt to interpret are skipped rather than guessed at - safe (just
/// slightly less gets blocked) rather than risking a wrong conversion of
/// syntax this "basic" converter was never meant to cover.
fn parse_easylist(text: &str) -> Vec<String> {
    let mut domains = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('!') || line.starts_with("@@") || line.contains('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix("||") else { continue };
        let Some(domain) = rest.strip_suffix('^') else { continue };
        if !domain.is_empty() && domain.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
            domains.push(domain.to_string());
        }
    }
    domains
}

/// Builds the WebKit content-blocker JSON (one `{"trigger":...,
/// "action":{"type":"block"}}` rule per domain) - pure and
/// `serde_json`-built rather than hand-formatted, so the JSON/regex
/// escaping can't come out subtly wrong and is unit-testable without a
/// real `WebView`.
fn build_rules_json(domains: &[String]) -> String {
    let rules: Vec<serde_json::Value> = domains
        .iter()
        .map(|domain| {
            let escaped = domain.replace('.', "\\.");
            serde_json::json!({
                "trigger": { "url-filter": format!("^https?://([a-z0-9-]+\\.)*{escaped}") },
                "action": { "type": "block" }
            })
        })
        .collect();
    serde_json::to_string(&rules).expect("a Vec<Value> of plain strings always serializes")
}

fn config_dir() -> PathBuf {
    let mut dir = glib::user_config_dir();
    dir.push(crate::APP_DIR);
    dir
}

fn enabled_flag_path() -> PathBuf {
    let mut path = config_dir();
    path.push("adblock_enabled.txt");
    path
}

/// On by default - the user asked for ad-blocking to actually happen,
/// with an *option* to turn it back off, not the other way around.
pub fn is_enabled() -> bool {
    std::fs::read_to_string(enabled_flag_path()).ok().map(|s| s.trim() != "0").unwrap_or(true)
}

pub fn set_enabled(enabled: bool) {
    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::write(enabled_flag_path(), if enabled { "1" } else { "0" });
}

fn content_filter_store() -> webkit6::UserContentFilterStore {
    let mut path = glib::user_cache_dir();
    path.push(crate::APP_DIR);
    path.push("content-filters");
    let _ = std::fs::create_dir_all(&path);
    webkit6::UserContentFilterStore::new(&path.to_string_lossy())
}

/// Compiles the basic ad-block rule set (a cheap cache hit after the
/// first run - see the module doc comment) and attaches it to `manager`
/// if ad-blocking is currently enabled. Call once per `WebView`'s own
/// `UserContentManager`, right after creating it.
pub fn install(manager: &webkit6::UserContentManager) {
    let store = content_filter_store();
    let manager = manager.clone();
    let rules = build_rules_json(&parse_easylist(EASYLIST_BASIC));
    store.save(FILTER_IDENTIFIER, &glib::Bytes::from(rules.as_bytes()), gio::Cancellable::NONE, move |result| {
        if let Ok(filter) = result {
            if is_enabled() {
                manager.add_filter(&filter);
            }
        }
    });
}

/// Turns ad-blocking on/off for an already-open `UserContentManager`
/// live, without needing the Browser tab reopened - the settings toggle
/// in `browsersettings.rs` calls this directly. Disabling is immediate
/// (`remove_filter_by_id` needs no async round trip); re-enabling
/// reloads the already-compiled filter from the store (see `install`),
/// which is fast since it was compiled once already.
pub fn set_manager_enabled(manager: &webkit6::UserContentManager, enabled: bool) {
    if !enabled {
        manager.remove_filter_by_id(FILTER_IDENTIFIER);
        return;
    }
    let store = content_filter_store();
    let manager = manager.clone();
    store.load(FILTER_IDENTIFIER, gio::Cancellable::NONE, move |result| {
        if let Ok(filter) = result {
            manager.add_filter(&filter);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn build_rules_json_produces_one_block_rule_per_domain() {
        let json = build_rules_json(&strings(&["example.com", "ads.example.net"]));
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        let rules = parsed.as_array().expect("a JSON array");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0]["action"]["type"], "block");
        assert!(rules[0]["trigger"]["url-filter"].as_str().unwrap().contains("example"));
        assert!(rules[1]["trigger"]["url-filter"].as_str().unwrap().contains("ads"));
    }

    #[test]
    fn build_rules_json_escapes_dots_in_the_domain_regex() {
        let json = build_rules_json(&strings(&["example.com"]));
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        let filter = parsed[0]["trigger"]["url-filter"].as_str().unwrap();
        assert!(filter.contains(r"example\.com"), "{filter}");
        assert!(!filter.contains("example.com"), "the unescaped dot would match any character: {filter}");
    }

    #[test]
    fn parse_easylist_extracts_plain_domain_anchor_rules() {
        let list = "||doubleclick.net^\n||ads.example.com^\n";
        assert_eq!(parse_easylist(list), vec!["doubleclick.net", "ads.example.com"]);
    }

    #[test]
    fn parse_easylist_skips_comments_and_blank_lines() {
        let list = "! this is a comment\n\n||doubleclick.net^\n";
        assert_eq!(parse_easylist(list), vec!["doubleclick.net"]);
    }

    #[test]
    fn parse_easylist_skips_exception_rules() {
        let list = "@@||doubleclick.net^\n||really-an-ad.net^\n";
        assert_eq!(parse_easylist(list), vec!["really-an-ad.net"]);
    }

    #[test]
    fn parse_easylist_skips_element_hiding_rules() {
        let list = "example.com##.ad-banner\n||doubleclick.net^\n";
        assert_eq!(parse_easylist(list), vec!["doubleclick.net"]);
    }

    #[test]
    fn parse_easylist_skips_rules_with_filter_options_or_paths() {
        let list = "||doubleclick.net^$third-party\n||example.com/path*\n||doubleclick.net^\n";
        assert_eq!(parse_easylist(list), vec!["doubleclick.net"]);
    }

    #[test]
    fn the_bundled_easylist_file_yields_a_non_empty_domain_list() {
        assert!(!parse_easylist(EASYLIST_BASIC).is_empty());
    }
}
