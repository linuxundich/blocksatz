//! The active blog theme's design presets - color palette, gradients, font
//! sizes and registered block styles - fetched over the REST API and cached
//! per blog like `termcache.rs`. Two uses:
//!
//! - the live preview turns them into the same `.has-accent-color` /
//!   `.has-accent-fade-gradient-background` / ... rules WordPress itself
//!   generates, so attribute lines (`{bg=accent}`) show up as they will in
//!   the blog;
//! - the block inspector offers exactly these presets, nothing the theme
//!   doesn't define.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use gtk4::glib;
use serde_json::Value;

use crate::{secrets, wpclient, wpsite};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Preset {
    pub slug: String,
    pub name: String,
    /// The CSS value: a color, a gradient, a font size.
    pub value: String,
    /// From the theme rather than WordPress's own defaults.
    pub from_theme: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BlockStyle {
    pub name: String,
    pub label: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThemeStyle {
    pub colors: Vec<Preset>,
    pub gradients: Vec<Preset>,
    pub font_sizes: Vec<Preset>,
    /// Block name (`core/table`) -> its registered styles.
    pub block_styles: Vec<(String, Vec<BlockStyle>)>,
    /// Whether the theme offers WordPress's default palette in the editor
    /// (`settings.color.defaultPalette`) - the presets are kept either way,
    /// since older posts may still use them.
    pub default_palette: bool,
    pub default_gradients: bool,
    /// CSS for buttons from the theme's global styles
    /// (`styles.elements.button`, the outline variation).
    pub button_css: String,
    /// `styles.spacing.blockGap` - the gap in flex/grid layouts.
    pub block_gap: Option<String>,
}

impl ThemeStyle {
    /// Builds the presets from the REST responses of
    /// `wpclient::get_theme_global_styles` and `get_core_block_styles`.
    pub fn from_rest(global_styles: &Value, block_types: &Value) -> ThemeStyle {
        let settings = &global_styles["settings"];
        ThemeStyle {
            colors: presets(&settings["color"]["palette"], "color"),
            gradients: presets(&settings["color"]["gradients"], "gradient"),
            font_sizes: presets(&settings["typography"]["fontSizes"], "size"),
            block_styles: block_types
                .as_array()
                .map(|types| {
                    types
                        .iter()
                        .filter_map(|block_type| {
                            let name = block_type["name"].as_str()?.to_string();
                            let styles: Vec<BlockStyle> = block_type["styles"]
                                .as_array()?
                                .iter()
                                .filter_map(|style| Some(BlockStyle { name: style["name"].as_str()?.to_string(), label: style["label"].as_str().unwrap_or_default().to_string() }))
                                .collect();
                            (!styles.is_empty()).then_some((name, styles))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            default_palette: settings["color"]["defaultPalette"].as_bool().unwrap_or(true),
            default_gradients: settings["color"]["defaultGradients"].as_bool().unwrap_or(true),
            button_css: button_css(&global_styles["styles"]),
            block_gap: global_styles["styles"]["spacing"]["blockGap"].as_str().map(css_value),
        }
    }

    /// The colors to offer: the theme's own, plus WordPress's defaults only
    /// where the theme leaves them enabled.
    pub fn offered_colors(&self) -> impl Iterator<Item = &Preset> {
        self.colors.iter().filter(|p| p.from_theme || self.default_palette)
    }

    pub fn offered_gradients(&self) -> impl Iterator<Item = &Preset> {
        self.gradients.iter().filter(|p| p.from_theme || self.default_gradients)
    }

    /// The registered styles of one block (`core/table`), WordPress's
    /// implicit `default`/`regular` style excluded.
    pub fn styles_for(&self, block: &str) -> Vec<BlockStyle> {
        self.block_styles.iter().find(|(name, _)| name == block).map(|(_, styles)| styles.iter().filter(|s| !matches!(s.name.as_str(), "default" | "regular" | "fill")).cloned().collect()).unwrap_or_default()
    }

    /// The preset classes WordPress generates, as CSS for the preview.
    /// Defaults first, so a theme preset with the same slug wins.
    pub fn preview_css(&self) -> String {
        let mut css = String::from(":root {\n");
        for (kind, list) in [("color", &self.colors), ("gradient", &self.gradients), ("font-size", &self.font_sizes)] {
            let mut list = list.clone();
            list.sort_by_key(|p| p.from_theme);
            for p in list {
                css.push_str(&format!("  --wp--preset--{kind}--{}: {};\n", css_ident(&p.slug), css_value(&p.value)));
            }
        }
        if let Some(gap) = &self.block_gap {
            css.push_str(&format!("  --wp--style--block-gap: {gap};\n"));
        }
        css.push_str("}\n");
        css.push_str(&self.button_css);
        let ordered = |list: &[Preset]| -> Vec<Preset> {
            let mut list = list.to_vec();
            list.sort_by_key(|p| p.from_theme);
            list
        };
        for p in ordered(&self.colors) {
            let (slug, value) = (css_ident(&p.slug), css_value(&p.value));
            css.push_str(&format!(".has-{slug}-color {{ color: {value} !important; }}\n.has-{slug}-background-color {{ background-color: {value} !important; }}\n.has-{slug}-border-color {{ border-color: {value} !important; }}\n"));
        }
        for p in ordered(&self.gradients) {
            css.push_str(&format!(".has-{}-gradient-background {{ background: {} !important; }}\n", css_ident(&p.slug), css_value(&p.value)));
        }
        for p in ordered(&self.font_sizes) {
            css.push_str(&format!(".has-{}-font-size {{ font-size: {} !important; }}\n", css_ident(&p.slug), css_value(&p.value)));
        }
        css
    }
}

impl ThemeStyle {
    fn to_json(&self) -> Value {
        let presets = |list: &[Preset]| -> Value { list.iter().map(|p| serde_json::json!({"slug": p.slug, "name": p.name, "value": p.value, "from_theme": p.from_theme})).collect() };
        serde_json::json!({
            "colors": presets(&self.colors),
            "gradients": presets(&self.gradients),
            "font_sizes": presets(&self.font_sizes),
            "block_styles": self.block_styles.iter().map(|(block, styles)| serde_json::json!({"block": block, "styles": styles.iter().map(|s| serde_json::json!({"name": s.name, "label": s.label})).collect::<Vec<_>>()})).collect::<Vec<_>>(),
            "default_palette": self.default_palette,
            "default_gradients": self.default_gradients,
            "button_css": self.button_css,
            "block_gap": self.block_gap,
        })
    }

    fn from_json(value: &Value) -> ThemeStyle {
        let presets = |key: &str| -> Vec<Preset> {
            value[key]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| Some(Preset { slug: p["slug"].as_str()?.to_string(), name: p["name"].as_str().unwrap_or_default().to_string(), value: p["value"].as_str()?.to_string(), from_theme: p["from_theme"].as_bool().unwrap_or(false) }))
                .collect()
        };
        ThemeStyle {
            colors: presets("colors"),
            gradients: presets("gradients"),
            font_sizes: presets("font_sizes"),
            block_styles: value["block_styles"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|entry| {
                    let styles = entry["styles"].as_array()?.iter().filter_map(|s| Some(BlockStyle { name: s["name"].as_str()?.to_string(), label: s["label"].as_str().unwrap_or_default().to_string() })).collect();
                    Some((entry["block"].as_str()?.to_string(), styles))
                })
                .collect(),
            default_palette: value["default_palette"].as_bool().unwrap_or(true),
            default_gradients: value["default_gradients"].as_bool().unwrap_or(true),
            button_css: value["button_css"].as_str().unwrap_or_default().to_string(),
            block_gap: value["block_gap"].as_str().map(str::to_string),
        }
    }
}

/// `var:preset|color|accent` -> `var(--wp--preset--color--accent)`.
fn style_value(value: &Value) -> Option<String> {
    let value = value.as_str()?;
    let value = match value.strip_prefix("var:") {
        Some(path) => format!("var(--wp--{})", path.replace('|', "--")),
        None => value.to_string(),
    };
    Some(css_value(&value))
}

/// A global-styles style object (color, spacing, border, typography) as
/// CSS declarations.
fn style_declarations(style: &Value) -> String {
    let mut out = String::new();
    let mut push = |property: &str, value: &Value| {
        if let Some(value) = style_value(value) {
            out.push_str(&format!("{property}: {value}; "));
        }
    };
    push("color", &style["color"]["text"]);
    push("background-color", &style["color"]["background"]);
    push("background", &style["color"]["gradient"]);
    for side in ["top", "right", "bottom", "left"] {
        push(&format!("padding-{side}"), &style["spacing"]["padding"][side]);
    }
    push("border-width", &style["border"]["width"]);
    push("border-style", &style["border"]["style"]);
    push("border-color", &style["border"]["color"]);
    push("border-radius", &style["border"]["radius"]);
    for (key, property) in [("fontFamily", "font-family"), ("fontSize", "font-size"), ("fontStyle", "font-style"), ("fontWeight", "font-weight"), ("letterSpacing", "letter-spacing"), ("lineHeight", "line-height"), ("textTransform", "text-transform"), ("textDecoration", "text-decoration")] {
        push(property, &style["typography"][key]);
    }
    out
}

fn button_css(styles: &Value) -> String {
    let mut css = String::new();
    let button = style_declarations(&styles["elements"]["button"]);
    if !button.is_empty() {
        css.push_str(&format!(".wp-element-button, .wp-block-button__link {{ {button}}}\n"));
    }
    let hover = style_declarations(&styles["elements"]["button"][":hover"]);
    if !hover.is_empty() {
        css.push_str(&format!(".wp-element-button:hover, .wp-block-button__link:hover {{ {hover}}}\n"));
    }
    let outline = style_declarations(&styles["blocks"]["core/button"]["variations"]["outline"]);
    if !outline.is_empty() {
        css.push_str(&format!(".wp-block-button.is-style-outline > .wp-block-button__link {{ background-color: transparent; {outline}}}\n"));
    }
    css
}

/// `{theme: [...], default: [...], custom: [...]}` -> one list.
fn presets(groups: &Value, value_key: &str) -> Vec<Preset> {
    let mut out = Vec::new();
    for (origin, from_theme) in [("default", false), ("theme", true), ("custom", true)] {
        for item in groups[origin].as_array().into_iter().flatten() {
            let (Some(slug), Some(value)) = (item["slug"].as_str(), item[value_key].as_str()) else { continue };
            out.push(Preset { slug: slug.to_string(), name: item["name"].as_str().unwrap_or(slug).to_string(), value: value.to_string(), from_theme });
        }
    }
    out
}

/// Keeps a slug from breaking out of its selector.
fn css_ident(slug: &str) -> String {
    slug.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}

/// Keeps a value from breaking out of its declaration.
fn css_value(value: &str) -> String {
    value.chars().filter(|c| !matches!(c, ';' | '{' | '}' | '<' | '>')).collect()
}

thread_local! {
    static CURRENT: RefCell<ThemeStyle> = RefCell::new(load());
    static LISTENERS: RefCell<Vec<Rc<dyn Fn()>>> = const { RefCell::new(Vec::new()) };
}

/// The active blog's presets (from the cache until a refresh lands).
pub fn current() -> ThemeStyle {
    CURRENT.with(|current| current.borrow().clone())
}

/// Runs `listener` whenever fresh presets arrived.
pub fn connect_changed(listener: impl Fn() + 'static) {
    LISTENERS.with(|listeners| listeners.borrow_mut().push(Rc::new(listener)));
}

fn set_current(style: ThemeStyle) {
    let changed = CURRENT.with(|current| {
        let mut current = current.borrow_mut();
        let changed = *current != style;
        *current = style;
        changed
    });
    if changed {
        let listeners: Vec<Rc<dyn Fn()>> = LISTENERS.with(|listeners| listeners.borrow().clone());
        for listener in listeners {
            listener();
        }
    }
}

/// One cache per blog: `theme-<site id>.json`.
fn cache_path() -> PathBuf {
    let site_id: String = wpsite::load().site_id().chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    let mut dir = glib::user_cache_dir();
    dir.push(crate::APP_DIR);
    dir.push(format!("theme-{site_id}.json"));
    dir
}

fn load() -> ThemeStyle {
    std::fs::read_to_string(cache_path()).ok().and_then(|text| serde_json::from_str::<Value>(&text).ok()).map(|value| ThemeStyle::from_json(&value)).unwrap_or_default()
}

fn save(style: &ThemeStyle) -> std::io::Result<()> {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, style.to_json().to_string())
}

/// Switches to the (newly) active blog's cache and refreshes it.
pub fn reload() {
    set_current(load());
    spawn_refresh();
}

/// Fetches the presets of the active blog on a background thread (three
/// small requests), then updates the cache and the listeners. Leaves the
/// cache alone on failure.
pub fn spawn_refresh() {
    let site = wpsite::load();
    if site.url.is_empty() {
        return;
    }
    let (tx, rx) = mpsc::channel::<Option<ThemeStyle>>();
    std::thread::spawn(move || {
        let result = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .ok()
            .flatten()
            .map(|password| wpclient::Client::new(&site.url, &site.username, &password))
            .and_then(|client| {
                let global_styles = client.get_theme_global_styles().ok()?;
                // Block styles are a nice-to-have; the palette alone is
                // still worth keeping if this request fails.
                let block_types = client.get_core_block_styles().unwrap_or(Value::Null);
                Some(ThemeStyle::from_rest(&global_styles, &block_types))
            });
        let _ = tx.send(result);
    });
    glib::timeout_add_local(Duration::from_millis(200), move || match rx.try_recv() {
        Ok(Some(style)) => {
            let _ = save(&style);
            set_current(style);
            glib::ControlFlow::Break
        }
        Ok(None) | Err(mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ThemeStyle {
        let global_styles = serde_json::json!({
            "settings": {
                "color": {
                    "palette": {
                        "default": [{"slug": "white", "name": "Weiß", "color": "#ffffff"}],
                        "theme": [{"slug": "accent", "name": "Signal-Orange", "color": "#ff6600"}, {"slug": "white", "name": "Weiß", "color": "#fefefe"}]
                    },
                    "gradients": {"theme": [{"slug": "accent-fade", "name": "Orange-Verlauf", "gradient": "linear-gradient(135deg, #ff6600 0%, #cc5200 100%)"}]},
                    "defaultPalette": false
                },
                "typography": {"fontSizes": {"theme": [{"slug": "large", "name": "Groß", "size": "1.0625rem"}]}}
            }
        });
        let block_types = serde_json::json!([
            {"name": "core/table", "styles": [{"name": "regular", "label": "Standard"}, {"name": "stripes", "label": "Streifen"}]},
            {"name": "core/paragraph", "styles": []}
        ]);
        ThemeStyle::from_rest(&global_styles, &block_types)
    }

    #[test]
    fn reads_presets_and_block_styles() {
        let style = sample();
        assert_eq!(style.colors.len(), 3);
        assert!(!style.default_palette);
        assert_eq!(style.offered_colors().map(|p| p.slug.as_str()).collect::<Vec<_>>(), vec!["accent", "white"]);
        assert_eq!(style.styles_for("core/table"), vec![BlockStyle { name: "stripes".into(), label: "Streifen".into() }]);
        assert!(style.styles_for("core/paragraph").is_empty());
    }

    #[test]
    fn preview_css_lets_theme_presets_win() {
        let css = sample().preview_css();
        assert!(css.contains(".has-accent-background-color { background-color: #ff6600 !important; }"));
        assert!(css.contains(".has-accent-fade-gradient-background { background: linear-gradient(135deg, #ff6600 0%, #cc5200 100%) !important; }"));
        assert!(css.contains(".has-large-font-size { font-size: 1.0625rem !important; }"));
        assert!(css.find("#ffffff").unwrap() < css.find("#fefefe").unwrap());
    }

    #[test]
    fn hostile_values_cannot_escape_the_rule() {
        let style = ThemeStyle { colors: vec![Preset { slug: "x}body{".into(), name: String::new(), value: "red;}</style>".into(), from_theme: true }], ..Default::default() };
        let css = style.preview_css();
        assert!(!css.contains("</style>") && !css.contains("x}body"));
    }

    #[test]
    fn serializes_round_trip() {
        let style = sample();
        assert_eq!(ThemeStyle::from_json(&style.to_json()), style);
    }
}
