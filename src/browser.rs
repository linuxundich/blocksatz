//! The "Browser" tab: a plain `WebKit` view with an address bar, next to
//! Chat, for consulting documentation, the live target site, or any other
//! reference page without alt-tabbing away from the editor. Deliberately
//! just a browser - no history list, bookmarks, or tabs of its own beyond
//! WebKit's own back/forward stack; a general-purpose browser already
//! does all of that better than this app needs to. Also where a link
//! clicked in the Vorschau pane ends up (`preview::PreviewPane::connect_link_clicked`,
//! wired in `window.rs`) - the preview itself refuses to navigate.

use std::path::PathBuf;

use adw::prelude::*;
use gtk4::glib;
use webkit6::prelude::*;

use crate::adblock;
use crate::i18n::tr;

const DEFAULT_URL: &str = "https://www.startpage.com/";

fn config_dir() -> PathBuf {
    let mut dir = glib::user_config_dir();
    dir.push(crate::APP_DIR);
    dir
}

fn home_url_path() -> PathBuf {
    let mut path = config_dir();
    path.push("browser_home.txt");
    path
}

/// The Browser tab's start page, configurable in Einstellungen (see
/// `browsersettings.rs`) - falls back to `DEFAULT_URL` if never set, or
/// if the setting was cleared back to empty.
fn tab_flag_path() -> PathBuf {
    let mut path = glib::user_config_dir();
    path.push(crate::APP_DIR);
    path.push("browser_tab.conf");
    path
}

/// Whether the right-hand pane offers the free "Browser" view (on unless
/// switched off in Einstellungen → Browser).
pub fn tab_enabled() -> bool {
    std::fs::read_to_string(tab_flag_path()).ok().map(|s| s.trim() != "0").unwrap_or(true)
}

pub fn set_tab_enabled(enabled: bool) {
    let path = tab_flag_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, if enabled { "1" } else { "0" });
}

pub fn load_home_url() -> String {
    std::fs::read_to_string(home_url_path()).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| DEFAULT_URL.to_string())
}

pub fn save_home_url(url: &str) {
    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::write(home_url_path(), url);
}

pub struct BrowserView {
    pub widget: gtk4::Widget,
    web_view: webkit6::WebView,
    content_manager: webkit6::UserContentManager,
}

impl BrowserView {
    /// The free browser: opens the start page right away.
    pub fn new() -> Self {
        Self::build(true)
    }

    /// A view that starts empty and only ever shows what it's told to -
    /// the blog preview under "Vorschau → Im Blog".
    pub fn new_blank() -> Self {
        Self::build(false)
    }

    fn build(load_home: bool) -> Self {
        // A `WebView`'s content manager is a construct-only property (no
        // runtime setter) - built up front so `adblock::install` can
        // attach the compiled filter to it before the `WebView` itself
        // exists, and so `set_adblock_enabled` can add/remove that same
        // filter later without needing to recreate the view.
        let content_manager = webkit6::UserContentManager::new();
        adblock::install(&content_manager);

        // The session is the app's shared one (`websession::shared`) so a
        // wp-admin login made here is also what the export dialog's
        // Live-Vorschau sees, and so it survives a restart.
        let web_view = webkit6::WebView::builder().user_content_manager(&content_manager).network_session(&crate::websession::shared()).build();
        web_view.set_hexpand(true);
        web_view.set_vexpand(true);

        let url_entry = gtk4::Entry::builder().placeholder_text(tr("Adresse eingeben oder suchen…")).hexpand(true).primary_icon_name("edit-find-symbolic").build();

        let back_button = gtk4::Button::from_icon_name("go-previous-symbolic");
        back_button.set_tooltip_text(Some(&tr("Zurück")));
        back_button.set_sensitive(false);
        let forward_button = gtk4::Button::from_icon_name("go-next-symbolic");
        forward_button.set_tooltip_text(Some(&tr("Vor")));
        forward_button.set_sensitive(false);
        let reload_button = gtk4::Button::from_icon_name("view-refresh-symbolic");
        reload_button.set_tooltip_text(Some(&tr("Neu laden")));

        let copy_url_button = gtk4::Button::from_icon_name("edit-copy-symbolic");
        copy_url_button.set_tooltip_text(Some(&tr("Aktuelle Adresse kopieren")));

        let toolbar = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).spacing(6).margin_top(6).margin_bottom(6).margin_start(6).margin_end(6).build();
        toolbar.append(&back_button);
        toolbar.append(&forward_button);
        toolbar.append(&reload_button);
        toolbar.append(&url_entry);
        toolbar.append(&copy_url_button);

        let content = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
        content.append(&toolbar);
        content.append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));
        content.append(&web_view);

        {
            let web_view = web_view.clone();
            url_entry.connect_activate(move |entry| {
                web_view.load_uri(&resolve_address(&entry.text(), &load_home_url()));
            });
        }
        {
            let web_view = web_view.clone();
            reload_button.connect_clicked(move |_| web_view.reload());
        }
        {
            let web_view = web_view.clone();
            back_button.connect_clicked(move |_| web_view.go_back());
        }
        {
            let web_view = web_view.clone();
            forward_button.connect_clicked(move |_| web_view.go_forward());
        }
        {
            let web_view = web_view.clone();
            copy_url_button.connect_clicked(move |button| {
                if let Some(uri) = web_view.uri() {
                    button.clipboard().set_text(&uri);
                }
            });
        }
        // Keeps the toolbar in sync with whatever navigation actually
        // happens - a link clicked *inside* the page, not just the
        // address bar/buttons here, still needs to update the shown URL
        // and un-gray Zurück/Vor once there's somewhere to go.
        {
            let back_button = back_button.clone();
            let forward_button = forward_button.clone();
            let url_entry = url_entry.clone();
            web_view.connect_uri_notify(move |web_view| {
                back_button.set_sensitive(web_view.can_go_back());
                forward_button.set_sensitive(web_view.can_go_forward());
                if let Some(uri) = web_view.uri() {
                    url_entry.set_text(&uri);
                }
            });
        }

        if load_home {
            web_view.load_uri(&load_home_url());
        }

        Self { widget: content.upcast(), web_view, content_manager }
    }

    /// Called from the "Browser" settings page's ad-block switch -
    /// applies live, no restart or tab reopen needed.
    pub fn set_adblock_enabled(&self, enabled: bool) {
        adblock::set_manager_enabled(&self.content_manager, enabled);
    }

    /// Loads `uri` directly, bypassing `resolve_address`'s bare-domain/
    /// search-query heuristics - for callers that already have an exact
    /// URL to show, e.g. `export.rs`'s WordPress preview link. Does not
    /// itself switch the app to the Browser tab; callers pair this with
    /// `view_stack.set_visible_child_name("browser")`.
    pub fn load_uri(&self, uri: &str) {
        self.web_view.load_uri(uri);
    }

    /// Runs `script` in the page once the next load has finished - for a
    /// step that needs this view's own (logged-in) session, like creating
    /// the autosave behind "Vorschau im Blog" (`mainaction.rs`). Call it
    /// right before the `load_uri` it belongs to.
    pub fn run_after_next_load(&self, script: String) {
        let handler: std::rc::Rc<std::cell::RefCell<Option<glib::SignalHandlerId>>> = std::rc::Rc::default();
        let handler_for_closure = handler.clone();
        let id = self.web_view.connect_load_changed(move |web_view, event| {
            if event != webkit6::LoadEvent::Finished {
                return;
            }
            web_view.evaluate_javascript(&script, None, None, gtk4::gio::Cancellable::NONE, |_| {});
            if let Some(id) = handler_for_closure.borrow_mut().take() {
                web_view.disconnect(id);
            }
        });
        *handler.borrow_mut() = Some(id);
    }
}

/// Turns whatever was typed into the address bar into a real URL to load -
/// a bare domain (`example.com`) gets `https://` prepended, and anything
/// else (containing a space, or with no dot at all) is treated as a search
/// query instead, matching how every mainstream browser's address bar
/// already behaves.
fn resolve_address(input: &str, home_url: &str) -> String {
    let input = input.trim();
    if input.is_empty() {
        return home_url.to_string();
    }
    if input.starts_with("http://") || input.starts_with("https://") {
        input.to_string()
    } else if !input.contains(' ') && input.contains('.') {
        format!("https://{input}")
    } else {
        format!("https://www.google.com/search?q={}", glib::uri_escape_string(input, None::<&str>, false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_url_is_used_unchanged() {
        assert_eq!(resolve_address("https://example.com/page", DEFAULT_URL), "https://example.com/page");
        assert_eq!(resolve_address("http://example.com", DEFAULT_URL), "http://example.com");
    }

    #[test]
    fn a_bare_domain_gets_https_prepended() {
        assert_eq!(resolve_address("example.com", DEFAULT_URL), "https://example.com");
        assert_eq!(resolve_address("www.example.com/path", DEFAULT_URL), "https://www.example.com/path");
    }

    #[test]
    fn plain_words_become_a_search_query() {
        assert_eq!(resolve_address("gutenberg block reference", DEFAULT_URL), "https://www.google.com/search?q=gutenberg%20block%20reference");
    }

    #[test]
    fn a_single_word_with_no_dot_becomes_a_search_query() {
        assert_eq!(resolve_address("wordpress", DEFAULT_URL), "https://www.google.com/search?q=wordpress");
    }

    #[test]
    fn blank_input_falls_back_to_the_configured_home_url() {
        assert_eq!(resolve_address("", "https://example.com/home"), "https://example.com/home");
        assert_eq!(resolve_address("   ", "https://example.com/home"), "https://example.com/home");
    }
}
