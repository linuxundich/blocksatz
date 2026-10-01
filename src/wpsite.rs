//! Non-secret half of the WordPress site connections (site URL +
//! username) - one or more blogs, one of them active. The Application
//! Passwords themselves never touch disk in plain text - see `secrets.rs`,
//! which stores one per URL and username in the Secret Service via `oo7`.
//!
//! `load()` is the active blog: everything that browses or creates (the
//! archive, the counters, new uploads) works against it. Anything acting
//! on an existing working copy uses `for_site_id` with the copy's own
//! `Frontmatter::wp_site` instead, so its `wp_post_id` always addresses
//! the blog it came from.
//!
//! Stored in `sites.conf`; a `wordpress.conf` from before several blogs
//! existed is taken over as the only blog on first read.

use std::path::PathBuf;

use gtk4::glib;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SiteConfig {
    pub url: String,
    pub username: String,
}

impl SiteConfig {
    /// A stable short id for the site - its URL without scheme, `www.` or
    /// trailing slash (`https://www.linuxundich.de/` → `linuxundich.de`).
    /// Stored in each working copy (`Frontmatter::wp_site`) so its
    /// `wp_post_id` keeps meaning the right post with several sites.
    pub fn site_id(&self) -> String {
        let url = self.url.trim();
        let url = url.split_once("://").map_or(url, |(_, rest)| rest);
        let url = url.strip_prefix("www.").unwrap_or(url);
        url.trim_end_matches('/').to_lowercase()
    }
}

/// All configured blogs and which one is active.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sites {
    pub sites: Vec<SiteConfig>,
    /// `site_id` of the active blog.
    pub active: String,
}

impl Sites {
    /// The active blog - the first one if `active` names none, an empty
    /// config if there are none.
    pub fn active_site(&self) -> SiteConfig {
        self.sites.iter().find(|s| s.site_id() == self.active).or_else(|| self.sites.first()).cloned().unwrap_or_default()
    }

    /// Adds `site`, or replaces the blog with the same id; makes it active.
    pub fn upsert(&mut self, site: SiteConfig) {
        let id = site.site_id();
        match self.sites.iter_mut().find(|s| s.site_id() == id) {
            Some(existing) => *existing = site,
            None => self.sites.push(site),
        }
        self.active = id;
    }

    /// Removes a blog; if it was the active one, the first remaining one
    /// becomes active.
    pub fn remove(&mut self, site_id: &str) {
        self.sites.retain(|s| s.site_id() != site_id);
        if self.active == site_id {
            self.active = self.sites.first().map(SiteConfig::site_id).unwrap_or_default();
        }
    }
}

fn config_dir() -> PathBuf {
    let mut dir = glib::user_config_dir();
    dir.push(crate::APP_DIR);
    dir
}

/// Every configured blog.
pub fn load_all() -> Sites {
    let dir = config_dir();
    if let Ok(contents) = std::fs::read_to_string(dir.join("sites.conf")) {
        return parse_sites(&contents);
    }
    // Before several blogs: one `url`/`username` pair in wordpress.conf.
    match std::fs::read_to_string(dir.join("wordpress.conf")) {
        Ok(contents) => {
            let site = parse_site(&contents);
            if site.url.is_empty() {
                Sites::default()
            } else {
                Sites { active: site.site_id(), sites: vec![site] }
            }
        }
        Err(_) => Sites::default(),
    }
}

pub fn save_all(sites: &Sites) -> std::io::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("sites.conf"), serialize_sites(sites))
}

/// The active blog.
pub fn load() -> SiteConfig {
    load_all().active_site()
}

/// The blog with this id (a working copy's `wp_site`), else the active one.
pub fn for_site_id(site_id: Option<&str>) -> SiteConfig {
    let sites = load_all();
    site_id.and_then(|id| sites.sites.iter().find(|s| s.site_id() == id).cloned()).unwrap_or_else(|| sites.active_site())
}

/// Saves `config` (adding it or updating the blog with the same id) and
/// makes it the active blog.
pub fn save(config: &SiteConfig) -> std::io::Result<()> {
    let mut sites = load_all();
    sites.upsert(config.clone());
    save_all(&sites)
}

pub fn set_active(site_id: &str) -> std::io::Result<()> {
    let mut sites = load_all();
    sites.active = site_id.to_string();
    save_all(&sites)
}

fn parse_site(input: &str) -> SiteConfig {
    let mut config = SiteConfig::default();
    for line in input.lines() {
        if let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                "url" => config.url = value.trim().to_string(),
                "username" => config.username = value.trim().to_string(),
                _ => {}
            }
        }
    }
    config
}

/// `active = <id>`, then one `[site]` block with `url`/`username` each.
fn parse_sites(input: &str) -> Sites {
    let mut sites = Sites::default();
    let mut current: Option<SiteConfig> = None;
    for line in input.lines() {
        let line = line.trim();
        if line == "[site]" {
            sites.sites.extend(current.take().filter(|s| !s.url.is_empty()));
            current = Some(SiteConfig::default());
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim().to_string();
        match (key.trim(), current.as_mut()) {
            ("active", None) => sites.active = value,
            ("url", Some(site)) => site.url = value,
            ("username", Some(site)) => site.username = value,
            _ => {}
        }
    }
    sites.sites.extend(current.filter(|s| !s.url.is_empty()));
    sites
}

fn serialize_sites(sites: &Sites) -> String {
    let mut out = format!("active = {}\n", sites.active);
    for site in &sites.sites {
        out.push_str(&format!("\n[site]\nurl = {}\nusername = {}\n", site.url, site.username));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(url: &str, user: &str) -> SiteConfig {
        SiteConfig { url: url.into(), username: user.into() }
    }

    #[test]
    fn site_id_strips_scheme_www_and_trailing_slash() {
        assert_eq!(site("https://www.linuxundich.de/", "").site_id(), "linuxundich.de");
        assert_eq!(site("http://Example.org/blog/", "").site_id(), "example.org/blog");
        assert_eq!(site("example.org", "").site_id(), "example.org");
    }

    #[test]
    fn several_sites_round_trip() {
        let sites = Sites { sites: vec![site("https://linuxundich.de", "christoph"), site("https://tuxsucht.de", "admin")], active: "tuxsucht.de".into() };
        assert_eq!(parse_sites(&serialize_sites(&sites)), sites);
        assert_eq!(parse_sites(&serialize_sites(&sites)).active_site().username, "admin");
    }

    #[test]
    fn the_old_single_site_file_still_parses() {
        assert_eq!(parse_site("url = https://example.com\nusername = admin\n"), site("https://example.com", "admin"));
        assert_eq!(parse_site(""), SiteConfig::default());
    }

    #[test]
    fn upsert_replaces_by_id_and_activates() {
        let mut sites = Sites::default();
        sites.upsert(site("https://linuxundich.de", "a"));
        sites.upsert(site("https://tuxsucht.de", "b"));
        sites.upsert(site("https://www.linuxundich.de/", "c"));
        assert_eq!(sites.sites.len(), 2);
        assert_eq!(sites.active, "linuxundich.de");
        assert_eq!(sites.active_site().username, "c");
    }

    #[test]
    fn removing_the_active_site_activates_the_next() {
        let mut sites = Sites { sites: vec![site("https://a.de", ""), site("https://b.de", "")], active: "a.de".into() };
        sites.remove("a.de");
        assert_eq!(sites.active, "b.de");
        sites.remove("b.de");
        assert_eq!(sites.active_site(), SiteConfig::default());
    }
}
