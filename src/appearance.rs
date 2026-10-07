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
/// comparison, terminal, the preview's code blocks: the saved pick, or its
/// light/dark counterpart when that matches the interface instead. Every
/// bundled scheme names its counterpart in its metadata ("Adwaita" ↔
/// "Adwaita-dark", "cobalt-light" ↔ "cobalt" ...), so a scheme picked in
/// light mode doesn't leave a white editor in a dark window. Falls back
/// to Adwaita in the matching variant.
pub fn current_scheme() -> Option<sourceview5::StyleScheme> {
    let manager = sourceview5::StyleSchemeManager::default();
    let dark = adw::StyleManager::default().is_dark();
    let fallback = || manager.scheme(if dark { "Adwaita-dark" } else { DEFAULT_SOURCE_SCHEME_ID });
    let Some(saved) = manager.scheme(&load_source_scheme_id()) else { return fallback() };
    if scheme_is_dark(&saved) == dark {
        return Some(saved);
    }
    let counterpart_key = if dark { "dark-variant" } else { "light-variant" };
    saved.metadata(counterpart_key).and_then(|id| manager.scheme(&id)).filter(|s| scheme_is_dark(s) == dark).or_else(fallback)
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
/// flashing the default first.
pub fn apply_saved_color_scheme() {
    adw::StyleManager::default().set_color_scheme(load_color_scheme());
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
            .theme-card { padding: 6px; border-radius: 12px; }
            .theme-card-preview {
                border-radius: 8px;
                border: 1px solid alpha(currentColor, 0.15);
            }
            .theme-card:checked .theme-card-preview {
                border: 2px solid @accent_bg_color;
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

fn build_theme_card(label_text: &str, svg_bytes: &'static [u8], group: Option<&gtk4::ToggleButton>) -> gtk4::ToggleButton {
    let picture = picture_from_svg_bytes(svg_bytes);
    let preview = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    preview.add_css_class("theme-card-preview");
    preview.set_overflow(gtk4::Overflow::Hidden);
    preview.append(&picture);

    let label = gtk4::Label::new(Some(label_text));

    let content = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(6).halign(gtk4::Align::Center).build();
    content.append(&preview);
    content.append(&label);

    let mut builder = gtk4::ToggleButton::builder().child(&content);
    if let Some(group) = group {
        builder = builder.group(group);
    }
    let button = builder.build();
    button.add_css_class("theme-card");
    button.add_css_class("flat");
    button
}

/// Rebuilds the scheme `flow_box`'s children from every installed
/// `GtkSourceStyleScheme` matching the current light/dark mode - Builder's
/// `update_style_schemes()`. GtkSourceView's own bundled set is what GNOME
/// Builder and Text Editor offer too, so there's nothing to ship here.
/// A pick is saved and announced through `notify_scheme_changed`, which
/// reaches every buffer, the terminal and the preview at once (and
/// repopulates this grid, see `build_page`).
fn populate_scheme_flow_box(flow_box: &gtk4::FlowBox) {
    while let Some(child) = flow_box.first_child() {
        flow_box.remove(&child);
    }

    let manager = sourceview5::StyleSchemeManager::default();
    let is_dark = adw::StyleManager::default().is_dark();
    let current_id = current_scheme().map(|s| s.id().to_string()).unwrap_or_default();

    let mut schemes: Vec<sourceview5::StyleScheme> =
        manager.scheme_ids().iter().filter(|id| id.as_str() != "printing").filter_map(|id| manager.scheme(id)).collect();
    schemes.sort_by_key(|s| s.name().to_string());

    for scheme in schemes {
        if scheme_is_dark(&scheme) != is_dark {
            continue;
        }
        let preview = sourceview5::StyleSchemePreview::new(&scheme);
        preview.set_selected(scheme.id() == current_id);
        preview.set_tooltip_text(Some(&scheme.name()));
        preview.connect_activate(|activated| {
            save_source_scheme_id(&activated.scheme().id());
            notify_scheme_changed();
        });
        flow_box.insert(&preview, -1);
    }
}

pub fn build_page(preview_pane: Rc<preview::PreviewPane>) -> adw::PreferencesPage {
    install_theme_card_css();

    let interface_group = adw::PreferencesGroup::builder().title(tr("Schnittstelle")).build();

    let follow_button = build_theme_card(&tr("Dem System folgen"), PREVIEW_SYSTEM_SVG, None);
    let light_button = build_theme_card(&tr("Hell"), PREVIEW_LIGHT_SVG, Some(&follow_button));
    let dark_button = build_theme_card(&tr("Dunkel"), PREVIEW_DARK_SVG, Some(&follow_button));

    match load_color_scheme() {
        adw::ColorScheme::ForceLight => light_button.set_active(true),
        adw::ColorScheme::ForceDark => dark_button.set_active(true),
        _ => follow_button.set_active(true),
    }

    let scheme_row = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).spacing(12).halign(gtk4::Align::Center).margin_top(12).margin_bottom(12).build();
    scheme_row.append(&follow_button);
    scheme_row.append(&light_button);
    scheme_row.append(&dark_button);

    follow_button.connect_toggled(|button| {
        if button.is_active() {
            adw::StyleManager::default().set_color_scheme(adw::ColorScheme::Default);
            save_color_scheme(adw::ColorScheme::Default);
        }
    });
    light_button.connect_toggled(|button| {
        if button.is_active() {
            adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceLight);
            save_color_scheme(adw::ColorScheme::ForceLight);
        }
    });
    dark_button.connect_toggled(|button| {
        if button.is_active() {
            adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
            save_color_scheme(adw::ColorScheme::ForceDark);
        }
    });

    interface_group.add(&scheme_row);

    let color_group = adw::PreferencesGroup::builder()
        .title(tr("Farbschema"))
        .description(tr("Gilt für Editor, Gutenberg-Code, Vergleich, Terminal und die Code-Blöcke der Vorschau. Hell und Dunkel wechseln mit der Oberfläche."))
        .build();

    // 3 columns - GtkSourceView's own bundled set is six schemes per mode
    // (see `populate_scheme_flow_box`'s doc comment), so this renders as a
    // clean 3x2 rectangle.
    let flow_box = gtk4::FlowBox::builder().column_spacing(12).row_spacing(12).max_children_per_line(3).selection_mode(gtk4::SelectionMode::None).homogeneous(true).build();
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

/// "Editor-Schriftart": a live Markdown sample (Builder's
/// `GbpEditoruiPreview` pattern - a small read-only source view reflecting
/// the current scheme *and* font, not just a static swatch) plus a
/// `Gtk.FontDialogButton`/Reset pair for the custom-font override.
fn build_editor_font_group() -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(tr("Editor-Schriftart")).build();

    let sample_buffer = sourceview5::Buffer::new(None::<&gtk4::TextTagTable>);
    sample_buffer.set_highlight_syntax(true);
    if let Some(lang) = sourceview5::LanguageManager::default().language("markdown") {
        sample_buffer.set_language(Some(&lang));
    }
    sample_buffer.set_text(&tr("# Überschrift\n\nEin **fetter** und *kursiver* Text mit `Inline-Code`.\n\n- Erster Listenpunkt\n- Zweiter Listenpunkt\n"));
    follow_scheme(&sample_buffer);
    let sample_view = sourceview5::View::with_buffer(&sample_buffer);
    sample_view.set_editable(false);
    sample_view.set_cursor_visible(false);
    sample_view.set_monospace(true);
    sample_view.set_top_margin(8);
    sample_view.set_bottom_margin(8);
    sample_view.set_left_margin(10);
    sample_view.set_right_margin(10);
    sample_view.add_css_class(EDITOR_FONT_CSS_CLASS);
    let sample_scroller = gtk4::ScrolledWindow::builder()
        .child(&sample_view)
        .height_request(130)
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Never)
        .build();
    sample_scroller.add_css_class("card");
    group.add(&sample_scroller);

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
        let sample_view = sample_view.clone();
        let suppress_font_notify = suppress_font_notify.clone();
        font_button.connect_font_desc_notify(move |button| {
            if suppress_font_notify.replace(false) {
                return;
            }
            let Some(desc) = button.font_desc() else { return };
            apply_editor_font_override_live(&desc.to_str());
            sample_view.add_css_class(EDITOR_FONT_CSS_CLASS); // re-touch to force a redraw
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
