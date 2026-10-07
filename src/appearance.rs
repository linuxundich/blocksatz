//! "Erscheinungsbild" (Appearance) page in Einstellungen, adopted directly
//! from GNOME Builder's own implementation (`gnome-builder.git`,
//! `src/plugins/platformui/gbp-platformui-tweaks-addin.c` and
//! `src/plugins/editorui/gbp-editorui-scheme-selector.c`):
//!
//! - The interface style picker (follow-system/light/dark) uses Builder's
//!   own bundled preview illustrations (`data/icons/appearance-preview/`,
//!   see `ATTRIBUTION.md` there) inside a `GtkPicture`, exactly like
//!   `IdeStyleVariantPreview` does - not a hand-drawn approximation.
//! - The color-scheme grid uses GtkSourceView's own `StyleSchemePreview`
//!   widget (the same one Builder's `GbpEditoruiSchemeSelector` uses) laid
//!   out in a `GtkFlowBox`, filtered to the schemes matching the current
//!   light/dark mode (Builder's `update_style_schemes`/`is_dark` logic,
//!   ported to Rust below) rather than showing every scheme at once.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk4::{glib, pango};
use sourceview5::prelude::*;
use webkit6::prelude::*;

use crate::document::Frontmatter;
use crate::fontutil;
use crate::i18n::tr;
use crate::preview::{self, PreviewStyle};

// GNOME's own default GtkSourceView scheme - real (not bundled by this
// app), always present, and light, matching this app's own default
// interface theme reasoning.
const DEFAULT_SOURCE_SCHEME_ID: &str = "Adwaita";
// Fontconfig's generic aliases ("Sans"/"Monospace") always resolve to
// *some* installed font, unlike a specific family name (e.g. "Cantarell")
// which may not be installed on every system - these are display-only
// seeds for the font picker before the user customizes anything, so
// resolving to a real font name (not "Keine"/"None") matters here.
const DEFAULT_FONT_DISPLAY: &str = "Sans 11";
const DEFAULT_MONOSPACE_FONT_DISPLAY: &str = "Monospace 11";

const PREVIEW_LIGHT_SVG: &[u8] = include_bytes!("../data/icons/appearance-preview/preview-light.svg");
const PREVIEW_DARK_SVG: &[u8] = include_bytes!("../data/icons/appearance-preview/preview-dark.svg");
const PREVIEW_SYSTEM_SVG: &[u8] = include_bytes!("../data/icons/appearance-preview/preview-system.svg");

fn config_dir() -> PathBuf {
    let mut dir = glib::user_config_dir();
    dir.push(crate::APP_DIR);
    dir
}

fn color_scheme_path() -> PathBuf {
    let mut path = config_dir();
    path.push("color_scheme.txt");
    path
}

fn source_scheme_path() -> PathBuf {
    let mut path = config_dir();
    path.push("source_scheme.txt");
    path
}

pub fn load_color_scheme() -> adw::ColorScheme {
    match std::fs::read_to_string(color_scheme_path()).ok().as_deref().map(str::trim) {
        Some("light") => adw::ColorScheme::ForceLight,
        Some("dark") => adw::ColorScheme::ForceDark,
        _ => adw::ColorScheme::Default,
    }
}

fn save_color_scheme(scheme: adw::ColorScheme) {
    let value = match scheme {
        adw::ColorScheme::ForceLight => "light",
        adw::ColorScheme::ForceDark => "dark",
        _ => "system",
    };
    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::write(color_scheme_path(), value);
}

fn load_source_scheme_id() -> String {
    std::fs::read_to_string(source_scheme_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_SOURCE_SCHEME_ID.to_string())
}

/// The scheme everything code-like is drawn in - editor, Gutenberg code,
/// comparison, terminal, the preview's code blocks: the saved pick in the
/// variant matching the interface, looked up the way GNOME Builder does
/// (`scheme_variant`), so "Adwaita" picked in light mode becomes
/// "Adwaita-dark" in a dark window. A scheme without a counterpart stays
/// as it is, as in Builder.
pub fn current_scheme() -> Option<sourceview5::StyleScheme> {
    let manager = sourceview5::StyleSchemeManager::default();
    install_bundled_schemes();
    let dark = adw::StyleManager::default().is_dark();
    let saved = manager.scheme(&load_source_scheme_id()).or_else(|| manager.scheme(DEFAULT_SOURCE_SCHEME_ID))?;
    Some(scheme_variant(&saved, if dark { "dark" } else { "light" }))
}

/// Port of Builder's `ide_source_style_scheme_get_variant()`: the
/// scheme's own "light-variant"/"dark-variant" metadata if that scheme
/// exists, else the id with its "-light"/"-dark" suffix swapped ("foo-dark",
/// then "foo"), else `scheme` itself.
fn scheme_variant(scheme: &sourceview5::StyleScheme, variant: &str) -> sourceview5::StyleScheme {
    let manager = sourceview5::StyleSchemeManager::default();
    if let Some(mapped) = scheme.metadata(&format!("{variant}-variant")).and_then(|id| manager.scheme(&id)) {
        return mapped;
    }
    let id = scheme.id();
    let base = id.strip_suffix("-light").or_else(|| id.strip_suffix("-dark")).unwrap_or(&id);
    manager.scheme(&format!("{base}-{variant}")).or_else(|| manager.scheme(base)).unwrap_or_else(|| scheme.clone())
}

type SchemeListener = Box<dyn Fn(&sourceview5::StyleScheme) -> bool>;

thread_local! {
    static SCHEME_LISTENERS: std::cell::RefCell<Vec<SchemeListener>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Calls `f` now and whenever the effective scheme changes - a pick in
/// Einstellungen or a light/dark switch. `f` returns `false` once its
/// widget is gone, which drops it.
pub fn connect_scheme_changed(f: impl Fn(&sourceview5::StyleScheme) -> bool + 'static) {
    static DARK_HOOK: std::sync::Once = std::sync::Once::new();
    DARK_HOOK.call_once(|| {
        adw::StyleManager::default().connect_dark_notify(|_| notify_scheme_changed());
    });
    if let Some(scheme) = current_scheme() {
        if !f(&scheme) {
            return;
        }
    }
    SCHEME_LISTENERS.with(|listeners| listeners.borrow_mut().push(Box::new(f)));
}

fn notify_scheme_changed() {
    let Some(scheme) = current_scheme() else { return };
    // Taken out while calling, so a listener may register another one.
    let listeners = SCHEME_LISTENERS.with(|listeners| std::mem::take(&mut *listeners.borrow_mut()));
    let kept: Vec<SchemeListener> = listeners.into_iter().filter(|f| f(&scheme)).collect();
    SCHEME_LISTENERS.with(|listeners| {
        let mut listeners = listeners.borrow_mut();
        let added = std::mem::take(&mut *listeners);
        *listeners = kept;
        listeners.extend(added);
    });
}

/// Keeps `buffer` in the current scheme for as long as it exists.
pub fn follow_scheme(buffer: &sourceview5::Buffer) {
    let buffer = buffer.downgrade();
    connect_scheme_changed(move |scheme| {
        let Some(buffer) = buffer.upgrade() else { return false };
        buffer.set_style_scheme(Some(scheme));
        true
    });
}

/// A style's colors from `scheme`, as CSS color strings.
pub fn scheme_style_colors(scheme: &sourceview5::StyleScheme, style_id: &str) -> (Option<String>, Option<String>) {
    let Some(style) = scheme.style(style_id) else { return (None, None) };
    let background = style.is_background_set().then(|| style.background()).flatten().map(|c| c.to_string());
    let foreground = style.is_foreground_set().then(|| style.foreground()).flatten().map(|c| c.to_string());
    (background, foreground)
}

/// The current scheme's "text" colors (background, foreground) for the
/// preview's code blocks. `None` if it doesn't set both - callers keep
/// their own defaults then.
pub fn current_scheme_colors() -> Option<(String, String)> {
    let scheme = current_scheme()?;
    let style = scheme.style("text")?;
    let background = style.is_background_set().then(|| style.background()).flatten()?;
    let foreground = style.is_foreground_set().then(|| style.foreground()).flatten()?;
    Some((background.to_string(), foreground.to_string()))
}

fn save_source_scheme_id(id: &str) {
    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::write(source_scheme_path(), id);
}

/// Applies the saved color-scheme preference - call once at startup so the
/// app's chrome starts in the right scheme immediately rather than
/// flashing the default first - and makes GNOME Builder's own color
/// schemes available (`install_bundled_schemes`).
pub fn apply_saved_color_scheme() {
    adw::StyleManager::default().set_color_scheme(load_color_scheme());
    install_bundled_schemes();
}

/// GNOME Builder's color schemes (`data/style-schemes/`, see
/// `ATTRIBUTION.md` there), so the grid offers exactly what Builder's
/// "Appearance" page does. GtkSourceView only reads schemes from
/// directories, so they're written to the cache once (and again whenever
/// they change) and that directory goes first on the search path.
const BUNDLED_SCHEMES: &[(&str, &str)] = &[
    ("arctic-dark.xml", include_str!("../data/style-schemes/arctic-dark.xml")),
    ("builder-dark.xml", include_str!("../data/style-schemes/builder-dark.xml")),
    ("builder.xml", include_str!("../data/style-schemes/builder.xml")),
    ("catppuccin-latte.xml", include_str!("../data/style-schemes/catppuccin-latte.xml")),
    ("catppuccin-mocha.xml", include_str!("../data/style-schemes/catppuccin-mocha.xml")),
    ("fishtank.xml", include_str!("../data/style-schemes/fishtank.xml")),
    ("horizon-dark.xml", include_str!("../data/style-schemes/horizon-dark.xml")),
    ("horizon-light.xml", include_str!("../data/style-schemes/horizon-light.xml")),
    ("monokai-soda.xml", include_str!("../data/style-schemes/monokai-soda.xml")),
    ("peninsula-dark.xml", include_str!("../data/style-schemes/peninsula-dark.xml")),
    ("peninsula.xml", include_str!("../data/style-schemes/peninsula.xml")),
    ("pixiefloss.xml", include_str!("../data/style-schemes/pixiefloss.xml")),
    ("spacedust.xml", include_str!("../data/style-schemes/spacedust.xml")),
    ("tokyo-night-light.xml", include_str!("../data/style-schemes/tokyo-night-light.xml")),
    ("tokyo-night.xml", include_str!("../data/style-schemes/tokyo-night.xml")),
    ("ubuntu.xml", include_str!("../data/style-schemes/ubuntu.xml")),
    ("vscode-dark.xml", include_str!("../data/style-schemes/vscode-dark.xml")),
    ("vscode-light.xml", include_str!("../data/style-schemes/vscode-light.xml")),
    ("xterm-dark.xml", include_str!("../data/style-schemes/xterm-dark.xml")),
    ("xterm-light.xml", include_str!("../data/style-schemes/xterm-light.xml")),
];

fn install_bundled_schemes() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let mut dir = glib::user_cache_dir();
        dir.push(crate::APP_DIR);
        dir.push("style-schemes");
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        for (name, contents) in BUNDLED_SCHEMES {
            let path = dir.join(name);
            if std::fs::read_to_string(&path).ok().as_deref() != Some(*contents) {
                let _ = std::fs::write(&path, contents);
            }
        }
        sourceview5::StyleSchemeManager::default().prepend_search_path(&dir.to_string_lossy());
    });
}

/// `app.style-variant` ("default"/"light"/"dark"), the action behind both
/// the theme selector in the primary menu (`theme_selector`) and the
/// three cards on the Erscheinungsbild page - as in GNOME Builder.
pub fn install_style_variant_action(app: &adw::Application) {
    let current = match load_color_scheme() {
        adw::ColorScheme::ForceLight => "light",
        adw::ColorScheme::ForceDark => "dark",
        _ => "default",
    };
    let action = gtk4::gio::SimpleAction::new_stateful("style-variant", Some(glib::VariantTy::STRING), &current.to_variant());
    action.connect_change_state(|action, value| {
        let Some(variant) = value.and_then(|v| v.get::<String>()) else { return };
        let scheme = match variant.as_str() {
            "light" => adw::ColorScheme::ForceLight,
            "dark" => adw::ColorScheme::ForceDark,
            _ => adw::ColorScheme::Default,
        };
        adw::StyleManager::default().set_color_scheme(scheme);
        save_color_scheme(scheme);
        action.set_state(&variant.to_variant());
    });
    action.connect_activate(|action, value| action.change_state(value.expect("style-variant takes a string")));
    app.add_action(&action);
}

/// The three round buttons (follow system, light, dark) at the top of
/// the primary menu - a port of libpanel's `PanelThemeSelector`, which is
/// what GNOME Builder puts there, including its stylesheet.
pub fn theme_selector() -> gtk4::Widget {
    install_theme_card_css();
    let row = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).spacing(12).hexpand(true).build();
    row.add_css_class("themeselector");
    let mut group: Option<gtk4::CheckButton> = None;
    for (variant, class, tooltip) in [("default", "follow", tr("Dem System folgen")), ("light", "light", tr("Hell")), ("dark", "dark", tr("Dunkel"))] {
        let button = gtk4::CheckButton::builder()
            .hexpand(true)
            .halign(gtk4::Align::Center)
            .focus_on_click(false)
            .tooltip_text(tooltip.as_str())
            .action_name("app.style-variant")
            .action_target(&variant.to_variant())
            .build();
        button.add_css_class("theme-selector");
        button.add_css_class(class);
        button.update_property(&[gtk4::accessible::Property::Label(&tooltip)]);
        if let Some(group) = &group {
            button.set_group(Some(group));
        } else {
            group = Some(button.clone());
        }
        row.append(&button);
    }
    row.upcast()
}

fn editor_font_path() -> PathBuf {
    let mut path = config_dir();
    path.push("editor_font.txt");
    path
}

/// A saved Pango font description (e.g. `"Fira Code 11"`) if the user has
/// picked a custom editor font - `None` means the editor keeps using the
/// system monospace font, as before this setting existed.
pub fn load_editor_font_override() -> Option<String> {
    std::fs::read_to_string(editor_font_path()).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn save_editor_font_override(desc: &str) {
    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::write(editor_font_path(), desc);
}

fn reset_editor_font_override() {
    let _ = std::fs::remove_file(editor_font_path());
}

pub const EDITOR_FONT_CSS_CLASS: &str = "blocksatz-editor-font";
// GTK objects aren't `Sync` (GLib's single-threaded-by-convention model),
// so this can't be a plain `static` - `thread_local!` is fine since GTK
// only ever runs on the main thread anyway.
thread_local! {
    static EDITOR_FONT_PROVIDER: std::cell::RefCell<Option<gtk4::CssProvider>> = const { std::cell::RefCell::new(None) };
}

fn refresh_editor_font_css(provider: &gtk4::CssProvider) {
    let css = match load_editor_font_override() {
        Some(desc) => format!(".{EDITOR_FONT_CSS_CLASS} {{ {} }}", fontutil::css_declarations(&pango::FontDescription::from_string(&desc))),
        None => String::new(),
    };
    provider.load_from_string(&css);
}

/// Installs the editor's custom-font CSS provider (once, application-wide)
/// and applies whatever's currently saved - call once when the real editor
/// view is built. Any widget wanting to reflect the same font (e.g. the
/// settings page's own live sample) just needs the same CSS class.
pub fn install_editor_font_css(view: &sourceview5::View) {
    view.add_css_class(EDITOR_FONT_CSS_CLASS);
    EDITOR_FONT_PROVIDER.with(|cell| {
        let mut provider_ref = cell.borrow_mut();
        let provider = provider_ref.get_or_insert_with(|| {
            let provider = gtk4::CssProvider::new();
            if let Some(display) = gtk4::gdk::Display::default() {
                gtk4::style_context_add_provider_for_display(&display, &provider, gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION);
            }
            provider
        });
        refresh_editor_font_css(provider);
    });
}

thread_local! {
    static FONT_LISTENERS: std::cell::RefCell<Vec<Box<dyn Fn() -> bool>>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Calls `f` whenever the editor font changes in Einstellungen, for
/// widgets the editor-font CSS class can't reach (the terminal draws its
/// own text). `f` returns `false` once its widget is gone.
pub fn connect_editor_font_changed(f: impl Fn() -> bool + 'static) {
    FONT_LISTENERS.with(|listeners| listeners.borrow_mut().push(Box::new(f)));
}

fn notify_editor_font_changed() {
    let listeners = FONT_LISTENERS.with(|listeners| std::mem::take(&mut *listeners.borrow_mut()));
    let kept: Vec<Box<dyn Fn() -> bool>> = listeners.into_iter().filter(|f| f()).collect();
    FONT_LISTENERS.with(|listeners| {
        let mut listeners = listeners.borrow_mut();
        let added = std::mem::take(&mut *listeners);
        *listeners = kept;
        listeners.extend(added);
    });
}

fn apply_editor_font_override_live(desc: &str) {
    save_editor_font_override(desc);
    EDITOR_FONT_PROVIDER.with(|cell| {
        if let Some(provider) = cell.borrow().as_ref() {
            refresh_editor_font_css(provider);
        }
    });
    notify_editor_font_changed();
}

fn reset_editor_font_override_live() {
    reset_editor_font_override();
    EDITOR_FONT_PROVIDER.with(|cell| {
        if let Some(provider) = cell.borrow().as_ref() {
            refresh_editor_font_css(provider);
        }
    });
    notify_editor_font_changed();
}

/// Port of Builder's `ide_source_style_scheme_is_dark()`
/// (`src/libide/sourceview/ide-source-style-scheme.c`): prefer the
/// scheme's own "variant" metadata or an "-dark" id suffix, and fall back
/// to the perceived brightness (HSP) of its "text" style's background.
fn scheme_is_dark(scheme: &sourceview5::StyleScheme) -> bool {
    match scheme.metadata("variant").as_deref() {
        Some("light") => return false,
        Some("dark") => return true,
        _ => {}
    }
    if scheme.id().contains("-dark") {
        return true;
    }
    if let Some(style) = scheme.style("text") {
        if style.is_background_set() {
            if let Some(bg) = style.background() {
                if let Ok(rgba) = gtk4::gdk::RGBA::parse(&bg) {
                    let (r, g, b) = (f64::from(rgba.red()) * 255.0, f64::from(rgba.green()) * 255.0, f64::from(rgba.blue()) * 255.0);
                    let hsp = (0.299 * r * r + 0.587 * g * g + 0.114 * b * b).sqrt();
                    return hsp <= 127.5;
                }
            }
        }
    }
    false
}

static CSS_INSTALLED: std::sync::Once = std::sync::Once::new();

fn install_theme_card_css() {
    CSS_INSTALLED.call_once(|| {
        let Some(display) = gtk4::gdk::Display::default() else {
            return;
        };
        let provider = gtk4::CssProvider::new();
        provider.load_from_string(
            "
            .theme-card, .theme-card:checked, .theme-card:hover { padding: 6px; border-radius: 12px; background: none; box-shadow: none; }
            .theme-card label { font-weight: normal; }
            .theme-card-preview {
                border-radius: 8px;
                outline: 2px solid transparent;
                outline-offset: 2px;
                box-shadow: 0 0 0 1px alpha(currentColor, 0.15);
            }
            .theme-card:checked .theme-card-preview { outline-color: var(--accent-bg-color); }
            .theme-card:hover .theme-card-preview { box-shadow: 0 0 0 1px alpha(currentColor, 0.3); }

            /* GNOME Builder's scheme grid (plugins/editorui/style.css) */
            flowbox.style-schemes flowboxchild {
                outline-offset: 2px;
                border-radius: 12px;
                outline-width: 2px;
                padding: 0;
            }
            flowbox.style-schemes flowboxchild GtkSourceStyleSchemePreview { margin: 0; }
            flowbox.style-schemes flowboxchild GtkSourceStyleSchemePreview:not(.selected) {
                box-shadow: 0 0 0 1px rgb(0 0 0 / 3%),
                            0 1px 3px 1px rgb(0 0 0 / 7%),
                            0 2px 6px 2px rgb(0 0 0 / 3%);
            }

            /* libpanel's PanelThemeSelector (stylesheet.css) */
            .themeselector { margin: 9px; }
            .themeselector checkbutton {
                padding: 1px;
                min-height: 44px;
                min-width: 44px;
                background-clip: content-box;
                border-radius: 9999px;
                box-shadow: inset 0 0 0 1px var(--border-color);
                --light-bg: #fff;
                --dark-bg: #202020;
            }
            .themeselector checkbutton.follow:checked,
            .themeselector checkbutton.light:checked,
            .themeselector checkbutton.dark:checked {
                box-shadow: inset 0 0 0 2px var(--accent-bg-color);
            }
            .themeselector checkbutton.follow {
                background-image: linear-gradient(to bottom right, var(--light-bg) 49.99%, var(--dark-bg) 50.01%);
            }
            .themeselector checkbutton.light { background-color: var(--light-bg); }
            .themeselector checkbutton.dark { background-color: var(--dark-bg); }
            .themeselector checkbutton radio {
                -gtk-icon-source: none;
                border: none;
                background: none;
                box-shadow: none;
                min-width: 12px;
                min-height: 12px;
                transform: translate(27px, 14px);
                padding: 2px;
            }
            .themeselector checkbutton radio:checked {
                -gtk-icon-source: -gtk-icontheme(\"object-select-symbolic\");
                background-color: var(--accent-bg-color);
                color: var(--accent-fg-color);
            }
            ",
        );
        gtk4::style_context_add_provider_for_display(&display, &provider, gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}

fn picture_from_svg_bytes(bytes: &'static [u8]) -> gtk4::Picture {
    let texture = gtk4::gdk::Texture::from_bytes(&glib::Bytes::from_static(bytes)).expect("bundled preview SVG should always parse");
    let picture = gtk4::Picture::for_paintable(&texture);
    picture.set_content_fit(gtk4::ContentFit::Fill);
    picture.set_can_shrink(true);
    picture.set_size_request(148, 81); // matches the SVGs' native 164:90 aspect ratio
    picture
}

fn build_theme_card(label_text: &str, svg_bytes: &'static [u8], variant: &str) -> gtk4::ToggleButton {
    let picture = picture_from_svg_bytes(svg_bytes);
    let preview = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    preview.add_css_class("theme-card-preview");
    preview.set_overflow(gtk4::Overflow::Hidden);
    preview.append(&picture);

    let label = gtk4::Label::new(Some(label_text));

    let content = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(6).halign(gtk4::Align::Center).build();
    content.append(&preview);
    content.append(&label);

    let button = gtk4::ToggleButton::builder().child(&content).action_name("app.style-variant").action_target(&variant.to_variant()).build();
    button.add_css_class("theme-card");
    button.add_css_class("flat");
    button
}

/// Port of Builder's `update_style_schemes()`: every installed scheme
/// except "printing", light ones first, those with a light/dark
/// counterpart before those without, then by name (a dark scheme sorts
/// by its light counterpart's id, so pairs line up in both grids). Only
/// the current mode's schemes are shown, plus the current pick if it has
/// no counterpart. A pick is saved and announced through
/// `notify_scheme_changed`, which reaches every buffer, the terminal and
/// the preview at once (and repopulates this grid, see `build_page`).
fn populate_scheme_flow_box(flow_box: &gtk4::FlowBox) {
    while let Some(child) = flow_box.first_child() {
        flow_box.remove(&child);
    }

    let manager = sourceview5::StyleSchemeManager::default();
    let is_dark = adw::StyleManager::default().is_dark();
    let saved_id = load_source_scheme_id();
    let current_id = current_scheme().map(|s| s.id().to_string()).unwrap_or_default();

    struct Info {
        scheme: sourceview5::StyleScheme,
        sort_key: String,
        has_alt: bool,
        is_dark: bool,
    }
    let mut schemes: Vec<Info> = manager
        .scheme_ids()
        .iter()
        .filter(|id| id.as_str() != "printing")
        .filter_map(|id| manager.scheme(id))
        .map(|scheme| {
            let dark = scheme_is_dark(&scheme);
            let alt = scheme_variant(&scheme, if dark { "light" } else { "dark" });
            let has_alt = alt.id() != scheme.id();
            let sort_key = if dark && has_alt { alt.id().to_string() } else { scheme.name().to_string() };
            Info { scheme, sort_key, has_alt, is_dark: dark }
        })
        .collect();
    schemes.sort_by(|a, b| a.is_dark.cmp(&b.is_dark).then(b.has_alt.cmp(&a.has_alt)).then_with(|| glib::GString::from(a.sort_key.as_str()).as_gstr().collate(b.sort_key.as_str())));

    for info in schemes {
        if is_dark != info.is_dark && (info.scheme.id() != saved_id || info.has_alt) {
            continue;
        }
        let preview = sourceview5::StyleSchemePreview::new(&info.scheme);
        preview.set_selected(info.scheme.id() == current_id);
        preview.connect_activate(|activated| {
            save_source_scheme_id(&activated.scheme().id());
            notify_scheme_changed();
        });
        flow_box.insert(&preview, -1);
    }
}

/// Builder's GbpEditoruiPreview: four lines of C with line numbers in the
/// current scheme and the editor font, as a card above the scheme grid.
fn scheme_preview() -> gtk4::Widget {
    let buffer = sourceview5::Buffer::new(None::<&gtk4::TextTagTable>);
    if let Some(lang) = sourceview5::LanguageManager::default().language("c") {
        buffer.set_language(Some(&lang));
    }
    buffer.set_text("#include <glib.h>\ntypedef struct _type_t type_t;\ntype_t *type_new (int id);\nvoid type_free (type_t *t);");
    follow_scheme(&buffer);
    let view = sourceview5::View::builder()
        .buffer(&buffer)
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .show_line_numbers(true)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(12)
        .right_margin(12)
        .right_margin_position(30)
        .build();
    view.add_css_class("card");
    view.add_css_class(EDITOR_FONT_CSS_CLASS);
    view.set_overflow(gtk4::Overflow::Hidden);
    view.upcast()
}

pub fn build_page(preview_pane: Rc<preview::PreviewPane>) -> adw::PreferencesPage {
    install_theme_card_css();

    let interface_group = adw::PreferencesGroup::builder().title(tr("Schnittstelle")).build();

    // Builder's IdeStyleVariantPreview cards, wired to the same
    // `app.style-variant` action as the menu's theme selector.
    let scheme_row = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).spacing(12).halign(gtk4::Align::Center).homogeneous(true).build();
    scheme_row.append(&build_theme_card(&tr("Dem System folgen"), PREVIEW_SYSTEM_SVG, "default"));
    scheme_row.append(&build_theme_card(&tr("Hell"), PREVIEW_LIGHT_SVG, "light"));
    scheme_row.append(&build_theme_card(&tr("Dunkel"), PREVIEW_DARK_SVG, "dark"));
    let scheme_card = gtk4::Box::builder().margin_top(0).build();
    scheme_card.add_css_class("card");
    scheme_row.set_margin_top(12);
    scheme_row.set_margin_bottom(12);
    scheme_row.set_margin_start(12);
    scheme_row.set_margin_end(12);
    scheme_row.set_hexpand(true);
    scheme_card.append(&scheme_row);
    let scheme_row = scheme_card;

    interface_group.add(&scheme_row);

    let color_group = adw::PreferencesGroup::builder()
        .title(tr("Farbschema"))
        .description(tr("Gilt für Editor, Gutenberg-Code, Vergleich, Terminal und die Code-Blöcke der Vorschau. Hell und Dunkel wechseln mit der Oberfläche."))
        .build();

    color_group.add(&scheme_preview());
    // Builder's GbpEditoruiSchemeSelector: 4 per line, 18px below the preview.
    let flow_box = gtk4::FlowBox::builder()
        .column_spacing(12)
        .row_spacing(12)
        .max_children_per_line(4)
        .selection_mode(gtk4::SelectionMode::None)
        .hexpand(true)
        .margin_top(18)
        .build();
    flow_box.add_css_class("style-schemes");
    color_group.add(&flow_box);
    {
        // Repopulated on every change: a light/dark switch swaps the whole
        // set, a pick moves the checkmark.
        let flow_box = flow_box.downgrade();
        connect_scheme_changed(move |_| {
            let Some(flow_box) = flow_box.upgrade() else { return false };
            populate_scheme_flow_box(&flow_box);
            true
        });
    }

    let (preview_group, refresh_preview_sample) = build_preview_group(preview_pane.clone());
    {
        let refresh_preview_sample = Rc::downgrade(&refresh_preview_sample);
        connect_scheme_changed(move |_| {
            let Some(refresh) = refresh_preview_sample.upgrade() else { return false };
            refresh();
            true
        });
    }
    let editor_font_group = build_editor_font_group();

    let page = adw::PreferencesPage::builder().title(tr("Erscheinungsbild")).icon_name("preferences-desktop-appearance-symbolic").build();
    page.add(&interface_group);
    page.add(&color_group);
    page.add(&editor_font_group);
    page.add(&preview_group);
    page
}

/// "Editor-Schriftart": a `Gtk.FontDialogButton`/Reset pair for the
/// custom-font override; the scheme preview above shows its effect.
fn build_editor_font_group() -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(tr("Editor-Schriftart")).build();

    let font_row = adw::ActionRow::builder().title(tr("Schriftart")).build();
    let font_dialog = gtk4::FontDialog::builder().title(tr("Editor-Schriftart wählen")).build();
    let font_button = gtk4::FontDialogButton::builder().dialog(&font_dialog).level(gtk4::FontLevel::Font).use_size(true).valign(gtk4::Align::Center).build();
    let initial_desc = load_editor_font_override().unwrap_or_else(|| DEFAULT_MONOSPACE_FONT_DISPLAY.to_string());
    font_button.set_font_desc(&pango::FontDescription::from_string(&initial_desc));

    let reset_button = gtk4::Button::from_icon_name("edit-undo-symbolic");
    reset_button.set_tooltip_text(Some(&tr("Auf Systemschrift zurücksetzen")));
    reset_button.add_css_class("flat");
    reset_button.set_valign(gtk4::Align::Center);
    reset_button.set_sensitive(load_editor_font_override().is_some());

    let suppress_font_notify = Rc::new(Cell::new(false));
    {
        let reset_button = reset_button.clone();
        let suppress_font_notify = suppress_font_notify.clone();
        font_button.connect_font_desc_notify(move |button| {
            if suppress_font_notify.replace(false) {
                return;
            }
            let Some(desc) = button.font_desc() else { return };
            apply_editor_font_override_live(&desc.to_str());
            reset_button.set_sensitive(true);
        });
    }
    {
        let font_button = font_button.clone();
        let suppress_font_notify = suppress_font_notify.clone();
        reset_button.connect_clicked(move |button| {
            reset_editor_font_override_live();
            suppress_font_notify.set(true);
            font_button.set_font_desc(&pango::FontDescription::from_string(DEFAULT_MONOSPACE_FONT_DISPLAY));
            button.set_sensitive(false);
        });
    }

    font_row.add_suffix(&font_button);
    font_row.add_suffix(&reset_button);
    group.add(&font_row);

    group
}

/// The "Vorschau" group: style picker, custom-font override, and a live
/// rendered-Markdown sample so a font or style choice's effect is visible
/// immediately, the same way the editor's own font sample works. Returns
/// the group alongside its own `refresh_sample` closure, so `build_page`
/// can fold it into the scheme grid's own activation callback (the
/// swatch's code-block colors need to follow a newly picked scheme too,
/// not just the interface light/dark toggle already wired below).
fn build_preview_group(preview_pane: Rc<preview::PreviewPane>) -> (adw::PreferencesGroup, Rc<dyn Fn()>) {
    let group = adw::PreferencesGroup::builder().title(tr("Vorschau")).build();

    let sample_view = webkit6::WebView::new();
    sample_view.set_size_request(-1, 160);
    let sample_scroller = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    sample_scroller.add_css_class("card");
    sample_scroller.append(&sample_view);
    group.add(&sample_scroller);

    let refresh_sample: Rc<dyn Fn()> = {
        let preview_pane = preview_pane.clone();
        let sample_view = sample_view.clone();
        Rc::new(move || {
            let dark = adw::StyleManager::default().is_dark();
            let sample_markdown = tr("# Beispielartikel\n\nDies ist ein **Beispieltext**, der zeigt, wie der gewählte *Stil* und die Schrift wirken.\n\n> Ein Zitat zur Veranschaulichung.\n");
            sample_view.load_html(&preview::render_html(&sample_markdown, preview_pane.style(), dark, &[], preview::ScrollRestore::Top, &Frontmatter::default(), false, current_scheme_colors()), None);
        })
    };
    refresh_sample();

    let style_row = adw::ComboRow::builder().title(tr("Stil")).build();
    let style_labels: Vec<String> = PreviewStyle::ALL.iter().map(|s| s.label()).collect();
    let style_label_refs: Vec<&str> = style_labels.iter().map(String::as_str).collect();
    style_row.set_model(Some(&gtk4::StringList::new(&style_label_refs)));
    let current_index = PreviewStyle::ALL.iter().position(|s| *s == preview_pane.style()).unwrap_or(0);
    style_row.set_selected(current_index as u32);
    {
        let preview_pane = preview_pane.clone();
        let refresh_sample = refresh_sample.clone();
        style_row.connect_selected_notify(move |row| {
            if let Some(style) = PreviewStyle::ALL.get(row.selected() as usize) {
                preview_pane.set_style(*style);
                refresh_sample();
            }
        });
    }
    group.add(&style_row);

    let font_row = adw::ActionRow::builder().title(tr("Schriftart")).build();
    let font_dialog = gtk4::FontDialog::builder().title(tr("Vorschau-Schriftart wählen")).build();
    let font_button = gtk4::FontDialogButton::builder().dialog(&font_dialog).level(gtk4::FontLevel::Font).use_size(true).valign(gtk4::Align::Center).build();
    let initial_desc = preview_pane.font_override().unwrap_or_else(|| DEFAULT_FONT_DISPLAY.to_string());
    font_button.set_font_desc(&pango::FontDescription::from_string(&initial_desc));

    let reset_button = gtk4::Button::from_icon_name("edit-undo-symbolic");
    reset_button.set_tooltip_text(Some(&tr("Auf Stil-Standardschrift zurücksetzen")));
    reset_button.add_css_class("flat");
    reset_button.set_valign(gtk4::Align::Center);
    reset_button.set_sensitive(preview_pane.is_font_customized());

    let suppress_font_notify = Rc::new(Cell::new(false));
    {
        let preview_pane = preview_pane.clone();
        let reset_button = reset_button.clone();
        let refresh_sample = refresh_sample.clone();
        let suppress_font_notify = suppress_font_notify.clone();
        font_button.connect_font_desc_notify(move |button| {
            if suppress_font_notify.replace(false) {
                return;
            }
            let Some(desc) = button.font_desc() else { return };
            preview_pane.set_font_override(&desc.to_str());
            reset_button.set_sensitive(true);
            refresh_sample();
        });
    }
    {
        let preview_pane = preview_pane.clone();
        let font_button = font_button.clone();
        let refresh_sample = refresh_sample.clone();
        let suppress_font_notify = suppress_font_notify.clone();
        reset_button.connect_clicked(move |button| {
            preview_pane.reset_font_override();
            suppress_font_notify.set(true);
            font_button.set_font_desc(&pango::FontDescription::from_string(DEFAULT_FONT_DISPLAY));
            button.set_sensitive(false);
            refresh_sample();
        });
    }

    font_row.add_suffix(&font_button);
    font_row.add_suffix(&reset_button);
    group.add(&font_row);

    (group, refresh_sample)
}
