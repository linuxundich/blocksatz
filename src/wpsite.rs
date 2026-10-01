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
