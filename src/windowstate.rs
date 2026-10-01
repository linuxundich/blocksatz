//! Persists the main window's size (and maximized state) across restarts,
//! so relaunching the app doesn't reset it back to a fixed default every
//! time - same plain `key = value` file convention as `wpsite.rs`.

use std::path::PathBuf;

use gtk4::glib;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowState {
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 800,
            maximized: false,
        }
    }
}

fn config_path() -> PathBuf {
    let mut dir = glib::user_config_dir();
    dir.push(crate::APP_DIR);
    dir.push("window_state.conf");
    dir
}

pub fn load() -> WindowState {
    match std::fs::read_to_string(config_path()) {
        Ok(contents) => parse(&contents),
        Err(_) => WindowState::default(),
    }
}

pub fn save(state: &WindowState) -> std::io::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serialize(state))
}

/// Falls back to the default for a missing/corrupt/non-positive dimension -
/// a width or height of zero (or negative) would otherwise produce an
/// unusable window on the next launch.
fn parse(input: &str) -> WindowState {
    let mut state = WindowState::default();
    for line in input.lines() {
        if let Some((key, value)) = line.split_once('=') {
            let value = value.trim();
            match key.trim() {
                "width" => {
                    if let Ok(width) = value.parse::<i32>() {
                        if width > 0 {
                            state.width = width;
                        }
                    }
                }
                "height" => {
                    if let Ok(height) = value.parse::<i32>() {
                        if height > 0 {
                            state.height = height;
                        }
                    }
                }
                "maximized" => state.maximized = value == "true",
                _ => {}
            }
        }
    }
    state
}

fn serialize(state: &WindowState) -> String {
    format!("width = {}\nheight = {}\nmaximized = {}\n", state.width, state.height, state.maximized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_parse_and_serialize() {
        let state = WindowState {
            width: 1600,
            height: 900,
            maximized: true,
        };
        assert_eq!(parse(&serialize(&state)), state);
    }

    #[test]
    fn missing_file_yields_default() {
        assert_eq!(parse(""), WindowState::default());
    }

    #[test]
    fn non_positive_dimensions_fall_back_to_default() {
        let state = parse("width = 0\nheight = -5\nmaximized = false\n");
        assert_eq!(state.width, WindowState::default().width);
        assert_eq!(state.height, WindowState::default().height);
    }
}
