//! Non-secret half of the WordPress site connection (site URL + username).
//! The Application Password itself never touches disk in plain text - see
//! `secrets.rs`, which stores it in the Secret Service via `oo7`.

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
    /// `wp_post_id` keeps meaning the right post once several sites exist.
    pub fn site_id(&self) -> String {
        let url = self.url.trim();
        let url = url.split_once("://").map_or(url, |(_, rest)| rest);
        let url = url.strip_prefix("www.").unwrap_or(url);
        url.trim_end_matches('/').to_lowercase()
    }
}

fn config_path() -> PathBuf {
    let mut dir = glib::user_config_dir();
    dir.push(crate::APP_DIR);
    dir.push("wordpress.conf");
    dir
}

pub fn load() -> SiteConfig {
    match std::fs::read_to_string(config_path()) {
        Ok(contents) => parse(&contents),
        Err(_) => SiteConfig::default(),
    }
}

pub fn save(config: &SiteConfig) -> std::io::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serialize(config))
}

fn parse(input: &str) -> SiteConfig {
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

fn serialize(config: &SiteConfig) -> String {
    format!("url = {}\nusername = {}\n", config.url, config.username)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_id_strips_scheme_www_and_trailing_slash() {
        let site = |url: &str| SiteConfig { url: url.into(), username: String::new() }.site_id();
        assert_eq!(site("https://www.linuxundich.de/"), "linuxundich.de");
        assert_eq!(site("http://Example.org/blog/"), "example.org/blog");
        assert_eq!(site("example.org"), "example.org");
    }

    #[test]
    fn round_trips_through_parse_and_serialize() {
        let config = SiteConfig {
            url: "https://example.com".into(),
            username: "admin".into(),
        };
        assert_eq!(parse(&serialize(&config)), config);
    }

    #[test]
    fn missing_file_yields_default() {
        assert_eq!(parse(""), SiteConfig::default());
    }
}
