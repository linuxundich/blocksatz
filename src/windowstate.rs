//! Persists the main window's size (and maximized state) and the layout
//! of its panes across restarts - the editor/pane split, which sidebars
//! are shown and the right-hand pane's view - so relaunching the app
//! doesn't reset any of it. Same plain `key = value` file convention as
//! `wpsite.rs`. (The window's position can't be restored: on Wayland the
//! compositor places windows, not the app.)

use std::path::PathBuf;

use gtk4::glib;

#[derive(Debug, Clone, PartialEq)]
pub struct WindowState {
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
    /// Editor share of the editor/pane split, 0.15..0.85.
    pub split_ratio: f64,
    pub sidebar_visible: bool,
    pub pane_visible: bool,
    /// The right-hand pane's visible page ("preview", "post", "chat" ...).
    pub pane_page: String,
    /// Height of the fold-out terminal (`terminal.rs`) in pixels.
    pub terminal_height: i32,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 800,
            maximized: false,
            split_ratio: 0.5,
            sidebar_visible: true,
            pane_visible: true,
            pane_page: "preview".to_string(),
            terminal_height: 240,
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
                "split_ratio" => {
                    if let Ok(ratio) = value.parse::<f64>() {
                        state.split_ratio = ratio.clamp(0.15, 0.85);
                    }
                }
                "sidebar_visible" => state.sidebar_visible = value != "false",
                "pane_visible" => state.pane_visible = value != "false",
                "pane_page" if !value.is_empty() => state.pane_page = value.to_string(),
                "terminal_height" => {
                    if let Ok(height) = value.parse::<i32>() {
                        if height >= 80 {
                            state.terminal_height = height;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    state
}

fn serialize(state: &WindowState) -> String {
    format!(
        "width = {}\nheight = {}\nmaximized = {}\nsplit_ratio = {:.3}\nsidebar_visible = {}\npane_visible = {}\npane_page = {}\nterminal_height = {}\n",
        state.width, state.height, state.maximized, state.split_ratio, state.sidebar_visible, state.pane_visible, state.pane_page, state.terminal_height
    )
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
            split_ratio: 0.625,
            sidebar_visible: false,
            pane_visible: true,
            pane_page: "post".to_string(),
            terminal_height: 320,
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
