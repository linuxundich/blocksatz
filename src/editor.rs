//! The left-hand Markdown editing pane: a `GtkSourceView` with markdown
//! syntax highlighting inside a scrolled window, plus spell-checking via
//! `libspelling` (the GTK4-native successor to gspell, which never got a
//! GTK4 port).

use gtk4::prelude::*;
use libspelling as spelling;
use sourceview5::prelude::*;

use crate::appearance;

pub fn build() -> (gtk4::ScrolledWindow, sourceview5::View, sourceview5::Buffer, gtk4::gio::MenuModel) {
    let buffer = sourceview5::Buffer::new(None::<&gtk4::TextTagTable>);

    if let Some(lang) = sourceview5::LanguageManager::default().language("markdown") {
        buffer.set_language(Some(&lang));
    }
    buffer.set_highlight_syntax(true);

    if let Some(scheme) = sourceview5::StyleSchemeManager::default().scheme(&appearance::load_source_scheme_id()) {
        buffer.set_style_scheme(Some(&scheme));
    }

    let view = sourceview5::View::with_buffer(&buffer);
    view.set_show_line_numbers(true);
    view.set_wrap_mode(gtk4::WrapMode::WordChar);
    view.set_monospace(true);
    view.set_top_margin(8);
    view.set_bottom_margin(8);
    view.set_left_margin(12);
    view.set_right_margin(12);
    view.set_hexpand(true);
    view.set_vexpand(true);
    appearance::install_editor_font_css(&view);

    let checker = spelling::Checker::default();
    let adapter = spelling::TextBufferAdapter::new(&buffer, &checker);
    adapter.set_enabled(true);
    // Kept alive by the view: inserted action groups and the buffer's own
    // signal connections hold their own strong references. The menu model
    // itself is returned rather than installed here, so the caller
    // (`window.rs`/`aimenu.rs`) can combine it with the editor's AI-actions
    // menu into a single `extra-menu` - `set_extra_menu` replaces whatever
    // was set before, so only one caller may ever call it for this view.
    view.insert_action_group("spelling", Some(&adapter));
    let spelling_menu = adapter.menu_model();

    let scroller = gtk4::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&view)
        .build();

    (scroller, view, buffer, spelling_menu)
}

/// Points the spell checker at the language of the open file: the first
/// installed dictionary for `lang` (`"en"` → en_US, en_GB …), or the
/// system's own for an original (`None`). The editor uses the default
/// checker, so this is all it takes.
pub fn follow_language(lang: Option<&str>) {
    let checker = spelling::Checker::default();
    let provider = checker.provider();
    let code = match lang {
        Some(lang) => {
            let codes: Vec<String> = provider
                .list_languages()
                .iter::<spelling::Language>()
                .filter_map(Result::ok)
                .filter_map(|l| l.code().map(|c| c.to_string()))
                .collect();
            let preferred = format!("{lang}_{}", lang.to_uppercase());
            let us = (lang == "en").then(|| "en_US".to_string());
            codes
                .iter()
                .find(|c| Some(*c) == us.as_ref())
                .or_else(|| codes.iter().find(|c| **c == preferred))
                .or_else(|| codes.iter().find(|c| c.split(['_', '-']).next() == Some(lang)))
                .cloned()
        }
        None => provider.default_code().map(|c| c.to_string()),
    };
    if let Some(code) = code {
        if checker.language().as_deref() != Some(code.as_str()) {
            checker.set_language(&code);
        }
    }
}

/// The current selection if there is one, otherwise the whole document -
/// the shared "what should this AI action apply to" rule used both by the
/// editor context menu's prompts (`aimenu.rs`) and by a free-form message
/// typed into the Chat tab (`chat.rs`). Not unit-tested here - exercising
/// it needs a real `Gtk.TextBuffer`, which panics unless GTK has already
/// been initialized (needs a live display), unlike every other function
/// this project unit-tests.
pub fn selected_or_full_text(buffer: &sourceview5::Buffer) -> String {
    if let Some((start, end)) = buffer.selection_bounds() {
        buffer.text(&start, &end, false).to_string()
    } else {
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string()
    }
}
