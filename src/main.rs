mod about;
mod adblock;
mod aialt;
mod aicaption;
mod aievaluate;
mod aiinplace;
mod aimenu;
mod aiprompts;
mod aitasks;
mod aiwriter;
mod appearance;
mod autocomplete;
mod blogposts;
mod blogsync;
mod browser;
mod browsersettings;
mod changelog;
mod chat;
mod chatconfig;
mod chatsettings;
mod codeview;
mod connection;
mod default_prompt;
mod document;
mod editor;
mod export;
mod firstrun;
mod fontutil;
mod formatting;
mod gallerydialog;
mod i18n;
mod imagealt;
mod imagecompress;
mod imageedit;
mod importer;
mod library;
mod librarysidebar;
mod linkcheck;
mod linkpicker;
mod llm;
mod mainaction;
mod mdpango;
mod media;
mod mediabrowser;
mod medialibrary;
mod mediapanel;
mod modelcheck;
mod modelsettings;
mod notify;
mod preview;
mod promptsettings;
mod properties;
mod recentfiles;
mod richtext;
mod searchbar;
mod secrets;
mod settings;
mod shortcuts;
mod stats;
mod statusbar;
mod statuscontrols;
mod syncstate;
mod tagsuggest;
mod taxonomy;
mod termcache;
mod websession;
mod window;
mod windowstate;
mod worksave;
mod wpclient;
mod wpsite;

use adw::prelude::*;
use gtk4::{gio, glib};

const APP_ID: &str = "de.linuxundich.Blocksatz";

/// Name of this app's own subfolder under `glib::user_config_dir()`,
/// `user_cache_dir()` and `user_data_dir()` - one place instead of a string
/// literal repeated in every module that persists something.
pub const APP_DIR: &str = "blocksatz";

fn main() -> glib::ExitCode {
    // Must run before anything else - `setlocale` (which this calls) isn't
    // thread-safe against locale-dependent calls running concurrently on
    // other threads, and nothing here has spawned any yet.
    i18n::init();

    // HANDLES_OPEN: the `.desktop` file declares `MimeType=text/markdown;`,
    // so double-clicking a `.md` file (or "Open With" → Blocksatz) in
    // Nautilus launches with a file argument - without this flag GTK
    // refuses that outright ("This application can not open files").
    let app = adw::Application::builder().application_id(APP_ID).flags(gio::ApplicationFlags::HANDLES_OPEN).build();

    app.set_accels_for_action("win.new", &["<Ctrl>n"]);
    app.set_accels_for_action("win.new-page", &["<Ctrl><Alt>n"]);
    app.set_accels_for_action("win.open", &["<Ctrl>o"]);
    app.set_accels_for_action("win.open-from-wordpress", &["<Ctrl><Shift>o"]);
    app.set_accels_for_action("win.save", &["<Ctrl>s"]);
    app.set_accels_for_action("win.settings", &["<Ctrl>comma"]);
    app.set_accels_for_action("win.publish", &["<Ctrl><Shift>p"]);
    app.set_accels_for_action("win.media-manager", &["<Ctrl><Shift>m"]);
    app.set_accels_for_action("win.media-library", &["<Ctrl><Shift>l"]);
    app.set_accels_for_action("win.ai-write", &["<Ctrl><Shift>g"]);
    app.set_accels_for_action("win.find", &["<Ctrl>f"]);
    app.set_accels_for_action("win.toggle-focus-mode", &["<Ctrl><Shift>f"]);
    app.set_accels_for_action("win.show-help-overlay", &["<Ctrl>question"]);

    app.connect_activate(|app| {
        appearance::apply_saved_color_scheme();
        load_chat_bubble_css();
        // A plain launch (no file argument - see `connect_open` below for
        // that case) reopens the most recently opened/saved article
        // instead of always starting at a blank "Unbenannt" document -
        // `recentfiles::load()` already tracks exactly this, filtered to
        // paths that still exist, so there's nothing new to persist here.
        // `Ctrl+N` still gets to a blank document in one step, same as
        // always.
        let initial_path = recentfiles::load().into_iter().next();
        let win = window::build(app, initial_path);
        win.present();
        if firstrun::should_show() {
            firstrun::open(&win);
        }
    });

    // Single-window app (see ROADMAP.md's "Deliberately not recommended" -
    // multi-window is a poor fit here), so a file opened while an instance
    // is already running loads into that same window via the `open-path`
    // action (`window.rs::wire_open_path_action`) rather than spawning a
    // second one; only the very first launch-with-a-file builds the window
    // itself with the path preloaded.
    app.connect_open(|app, files, _hint| {
        let Some(path) = files.first().and_then(gtk4::gio::File::path) else {
            return;
        };
        if let Some(win) = app.windows().into_iter().next() {
            win.activate_action("win.open-path", Some(&path.display().to_string().to_variant())).ok();
            win.present();
            return;
        }
        appearance::apply_saved_color_scheme();
        load_chat_bubble_css();
        let win = window::build(app, Some(path));
        win.present();
        if firstrun::should_show() {
            firstrun::open(&win);
        }
    });

    app.run()
}

/// Chat bubble, tag-pill and gallery-tile-selection colors use libadwaita's
/// named theme colors so they adapt to light/dark mode automatically,
/// rather than hardcoding colors that would only look right in one theme.
/// The `.tag-pill-*` classes are `properties.rs`'s "which tags already
/// exist on WordPress" hint (a row of small colored badges, not just
/// tinted text - tinted text alone read as too small/subtle to notice at a
/// glance) and `success`/`error` are libadwaita's own semantic names for
/// exactly that positive/negative distinction, the same one this app's
/// "success"/"error" CSS classes already give an icon elsewhere (e.g. the
/// URL-length check in `properties.rs`). `.gallery-selected` is
/// `gallerydialog.rs`'s "this tile is in the gallery" highlight.
fn load_chat_bubble_css() {
    let Some(display) = gtk4::gdk::Display::default() else {
        return;
    };
    let provider = gtk4::CssProvider::new();
    provider.load_from_string(
        "
        .chat-bubble { padding: 8px 12px; border-radius: 12px; }
        .chat-bubble-user { background-color: @accent_bg_color; color: @accent_fg_color; }
        .chat-bubble-model { background-color: alpha(currentColor, 0.08); }
        .tag-pill { padding: 2px 10px; border-radius: 999px; }
        .tag-pill-existing { background-color: @success_bg_color; color: @success_fg_color; }
        .tag-pill-new { background-color: @error_bg_color; color: @error_fg_color; }
        .gallery-selected { border: 2px solid @accent_bg_color; border-radius: 6px; }
        ",
    );
    gtk4::style_context_add_provider_for_display(&display, &provider, gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION);
}
