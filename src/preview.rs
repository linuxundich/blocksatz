//! The right-hand live preview pane: a `WebKit` view rendering plain HTML
//! from the Markdown source (not the Gutenberg block-comment HTML — that's
//! only generated at export time, see `gutenberg::markdown_to_gutenberg`).
//!
//! Each top-level block is wrapped in a `<div data-line="N">`, `N` being the
//! 1-indexed Markdown source line it starts on, so the editor can drive
//! scroll-sync by asking the preview to scroll a specific *source line*
//! into view rather than assuming a fixed proportion of the document - an
//! image (or an embed placeholder, see `render_embed_placeholder`) is one
//! source line but can be many times taller than a text line once
//! rendered, so a naive "scroll to the same percentage" would drift. Sync
//! runs the other way too (scrolling the preview moves the editor) via
//! `connect_scroll`/`window.currentTopLine` - see `window.rs::wire_scroll_sync`
//! for how both directions are wired together without echoing back and
//! forth.
//!
//! This is a rendered article, not a browser: a click on a link never
//! navigates the preview itself away from the article - `connect_link_clicked`
//! intercepts it and hands the URL to the caller instead (wired in
//! `window.rs` to open it in the "Browser" tab).
//!
//! The preview also follows the app's light/dark mode and offers a choice
//! of typographic styles ("Modern"/"Klassisch"/"Sepia") via a small picker
//! above the `WebView` - both are baked directly into the generated HTML's
//! `<style>` block per render, rather than relying on the page's own
//! `prefers-color-scheme` media query, since we already regenerate the
//! whole document on every change anyway.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk4::{gio, glib, pango};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use webkit6::prelude::*;

use crate::appearance;
use crate::document::{self, Frontmatter};
use crate::fontutil;
use crate::i18n::tr;
use crate::media::{self, MediaItem};
use crate::wpsite;

/// Name registered on the `WebView`'s `UserContentManager` for the reverse
/// (preview -> editor) leg of scroll-sync - see `PreviewPane::connect_scroll`.
const SCROLL_SYNC_HANDLER: &str = "scrollSync";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewStyle {
    Modern,
    Classic,
    Sepia,
}

impl PreviewStyle {
    pub const ALL: [PreviewStyle; 3] = [PreviewStyle::Modern, PreviewStyle::Classic, PreviewStyle::Sepia];

    fn id(&self) -> &'static str {
        match self {
            PreviewStyle::Modern => "modern",
            PreviewStyle::Classic => "classic",
            PreviewStyle::Sepia => "sepia",
        }
    }

    fn from_id(s: &str) -> Self {
        match s.trim() {
            "classic" => PreviewStyle::Classic,
            "sepia" => PreviewStyle::Sepia,
            _ => PreviewStyle::Modern,
        }
    }

    pub fn label(&self) -> String {
        match self {
            PreviewStyle::Modern => tr("Modern"),
            PreviewStyle::Classic => tr("Klassisch"),
            PreviewStyle::Sepia => tr("Sepia"),
        }
    }
}

fn config_dir() -> PathBuf {
    let mut dir = glib::user_config_dir();
    dir.push("blocksmith");
    dir
}

fn preview_style_path() -> PathBuf {
    let mut path = config_dir();
    path.push("preview_style.txt");
    path
}

fn load_preview_style() -> PreviewStyle {
    std::fs::read_to_string(preview_style_path()).ok().map(|s| PreviewStyle::from_id(&s)).unwrap_or(PreviewStyle::Modern)
}

fn save_preview_style(style: PreviewStyle) {
    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::write(preview_style_path(), style.id());
}

fn preview_font_path() -> PathBuf {
    let mut path = config_dir();
    path.push("preview_font.txt");
    path
}

/// A saved Pango font description (e.g. `"Cantarell 11"`) if the user has
/// picked one - each `PreviewStyle`'s own font stays the default until
/// this is set, so choosing a style still looks like that style out of
/// the box.
fn load_preview_font_override() -> Option<String> {
    std::fs::read_to_string(preview_font_path()).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn save_preview_font_override(desc: &str) {
    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::write(preview_font_path(), desc);
}

fn reset_preview_font_override() {
    let _ = std::fs::remove_file(preview_font_path());
}

fn article_header_enabled_path() -> PathBuf {
    let mut path = config_dir();
    path.push("article_header_enabled.txt");
    path
}

/// Whether the magazine-style article header (see `render_header`) shows
/// above the rendered body - same "1"/"0" flag-file shape as
/// `adblock::is_enabled`. Defaults to on: it surfaces exactly the fields
/// "Artikel-Eigenschaften" already collects, so showing it out of the box
/// is more useful than a silent opt-in nobody would think to look for.
fn load_show_header() -> bool {
    std::fs::read_to_string(article_header_enabled_path()).ok().map(|s| s.trim() != "0").unwrap_or(true)
}

fn save_show_header(enabled: bool) {
    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::write(article_header_enabled_path(), if enabled { "1" } else { "0" });
}

/// The "Vorschau" tab: a style picker above a `WebKit` view. Remembers the
/// last rendered Markdown so it can re-render immediately when the style
/// changes (picked in Einstellungen, see `appearance::build_page`) or the
/// app's light/dark mode flips, without needing the editor buffer passed
/// back in.
pub struct PreviewPane {
    pub widget: gtk4::Widget,
    web_view: webkit6::WebView,
    style: Rc<Cell<PreviewStyle>>,
    last_markdown: Rc<RefCell<String>>,
    /// The document's current per-image metadata (alt text, upload state),
    /// already reconciled against `last_markdown` by the caller - used to
    /// draw the upload-status/alt/format badges in each image's bottom-right
    /// corner (see `wrap_images_with_badges`).
    last_media: Rc<RefCell<Vec<MediaItem>>>,
    /// The current article's own directory, if it has one (an unsaved or
    /// WordPress-imported-but-not-yet-saved document has none) - passed to
    /// the `WebView` as its base URI so a relative `<img src="photo.png">`
    /// resolves against it, matching where "Bild einfügen"/Medienverwaltung
    /// already expect a local image to live (see
    /// `document::image_reference`). Without this the image reference is
    /// technically correct but the preview simply can't load it - a
    /// `WebView` has no other way to know which folder "here" means.
    doc_dir: Rc<RefCell<Option<PathBuf>>>,
    /// A snapshot of the article's metadata, for the magazine-style header
    /// (`render_header`) - set explicitly via `set_article_header` whenever
    /// a caller has fresh `Frontmatter` in hand (document load/new/import,
    /// "Artikel-Eigenschaften" closing), not tied to `update`/
    /// `update_preserving_scroll`'s own body-edit debounce, since none of
    /// the fields the header shows (title, excerpt, categories, tags,
    /// slug, featured image) are ever derived from the body text itself.
    last_frontmatter: Rc<RefCell<Frontmatter>>,
    show_header: Rc<Cell<bool>>,
}

impl PreviewPane {
    pub fn new() -> Self {
        let web_view = webkit6::WebView::new();
        web_view.set_hexpand(true);
        web_view.set_vexpand(true);

        // This is a rendered article preview, not a browsing session - the
        // navigation-history items WebKit puts in its default context menu
        // (Zurück/Vor/Anhalten) never apply to anything here and would just
        // be confusing dead controls. Same for its default image actions
        // ("Bild in neuem Fenster öffnen"/"speichern unter"/"kopieren"/
        // "Bildadresse kopieren") - saving/copying the rendered file itself
        // makes no sense for an embedded article image; the app's own
        // image-specific items (alt text, "Bild bearbeiten…") are added
        // separately by `install_alt_text_menu`/`install_image_edit_menu`.
        web_view.connect_context_menu(|_web_view, context_menu, _hit_test_result| {
            for item in context_menu.items() {
                if matches!(
                    item.stock_action(),
                    webkit6::ContextMenuAction::GoBack
                        | webkit6::ContextMenuAction::GoForward
                        | webkit6::ContextMenuAction::Stop
                        | webkit6::ContextMenuAction::OpenImageInNewWindow
                        | webkit6::ContextMenuAction::DownloadImageToDisk
                        | webkit6::ContextMenuAction::CopyImageToClipboard
                        | webkit6::ContextMenuAction::CopyImageUrlToClipboard
                ) {
                    context_menu.remove(&item);
                }
            }
            false
        });

        let style = Rc::new(Cell::new(load_preview_style()));
        let last_markdown: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        let last_media: Rc<RefCell<Vec<MediaItem>>> = Rc::new(RefCell::new(Vec::new()));
        let doc_dir: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
        let last_frontmatter: Rc<RefCell<Frontmatter>> = Rc::new(RefCell::new(Frontmatter::default()));
        let show_header = Rc::new(Cell::new(load_show_header()));

        {
            let web_view = web_view.clone();
            let style = style.clone();
            let last_markdown = last_markdown.clone();
            let last_media = last_media.clone();
            let doc_dir = doc_dir.clone();
            let last_frontmatter = last_frontmatter.clone();
            let show_header = show_header.clone();
            adw::StyleManager::default().connect_dark_notify(move |style_manager| {
                let html = render_html(
                    &last_markdown.borrow(),
                    style.get(),
                    style_manager.is_dark(),
                    &last_media.borrow(),
                    0.0,
                    &last_frontmatter.borrow(),
                    show_header.get(),
                    appearance::current_scheme_colors(),
                );
                web_view.load_html(&html, base_uri(doc_dir.borrow().as_deref()).as_deref());
            });
        }

        Self {
            widget: web_view.clone().upcast(),
            web_view,
            style,
            last_markdown,
            last_media,
            doc_dir,
            last_frontmatter,
            show_header,
        }
    }

    /// `media` should already be reconciled against `markdown` (see
    /// `media::reconcile`) - the caller owns the canonical `Frontmatter`,
    /// this pane only needs a snapshot to draw badges from.
    pub fn update(&self, markdown: &str, media: &[MediaItem]) {
        *self.last_markdown.borrow_mut() = markdown.to_string();
        *self.last_media.borrow_mut() = media.to_vec();
        self.rerender();
    }

    /// Same as `update`, but keeps the current scroll position instead of
    /// resetting to the top - for `window.rs`'s live-preview debounce on
    /// every body edit. Without this, each debounced re-render (every body
    /// keystroke, ~`DEBOUNCE_MS` after typing pauses) snapped the preview
    /// back to the top regardless of where the editor's cursor-sync
    /// (`window.rs::sync_cursor_to_preview`) had *just* scrolled it to -
    /// harmless when that sync only reacted to actual scrolling (which
    /// rarely lines up with a content reload happening on the very same
    /// keystroke), but jarring once the preview started tracking the
    /// cursor while typing too: every pause became a visible snap back to
    /// line one before the next cursor-sync tick corrected it again one
    /// frame later. Opening a *different* document still lands at the top
    /// as expected - every document-loading call site already calls
    /// `set_doc_dir` right after `buffer.set_text`, whose own plain
    /// `rerender()` (always top, not preserved) runs synchronously before
    /// this debounced update fires, so there's nothing meaningful left to
    /// preserve by the time it reads the current scroll position.
    pub fn update_preserving_scroll(&self, markdown: &str, media: &[MediaItem]) {
        *self.last_markdown.borrow_mut() = markdown.to_string();
        *self.last_media.borrow_mut() = media.to_vec();
        self.rerender_preserving_scroll();
    }

    /// Re-renders with a fresh media snapshot without touching the last
    /// rendered Markdown - for callers that mutate `Frontmatter.media`
    /// directly (Medienverwaltung's upload button, the manual/AI alt-text
    /// dialogs) rather than by editing the article text, so the badges
    /// this pane draws don't go stale until the next keystroke happens to
    /// re-trigger `update`. Keeps the current scroll position rather than
    /// jumping to the top - unlike a body edit, this is always triggered by
    /// something the user did while looking at one specific spot (applying
    /// an AI-generated alt text, editing a caption), so landing back at the
    /// top of the article would be disorienting rather than expected.
    pub fn refresh_media(&self, media: &[MediaItem]) {
        *self.last_media.borrow_mut() = media.to_vec();
        self.rerender_preserving_scroll();
    }

    /// Called whenever the article's own file location changes (opened,
    /// saved for the first time, reset by "Neu"/importing from WordPress) -
    /// re-renders immediately so a folder that just became known (or just
    /// stopped being known) takes effect right away, not only on the next
    /// edit.
    pub fn set_doc_dir(&self, doc_dir: Option<PathBuf>) {
        *self.doc_dir.borrow_mut() = doc_dir;
        self.rerender();
    }

    /// Adds a "Bildbeschriftung bearbeiten…" item to the context menu when
    /// right-clicking directly on a rendered image - the preview-side entry
    /// point into `imagealt::open_dialog_for_index`, the same consolidated
    /// dialog (manual entry plus AI-generate buttons for both fields) the
    /// editor-side context menu's own "Bildbeschriftung bearbeiten…" item
    /// opens (`imagealt.rs`), triggered from the image's `![alt](src)` line
    /// instead. A second `context-menu` handler alongside the one `new()`
    /// already installs (which only trims the default navigation items),
    /// since `frontmatter` isn't available yet at construction time - both
    /// handlers run against the same `ContextMenu` on every right-click.
    ///
    /// Takes `&Rc<Self>` rather than `&self` - both dialogs need to hold
    /// onto this same pane (as an owned `Rc`) to refresh its badges/caption
    /// once applied, and a plain `&self` has no `Rc` of itself to hand out.
    pub fn install_alt_text_menu(preview_pane: &Rc<Self>, window: &impl IsA<gtk4::Window>, frontmatter: Rc<RefCell<Frontmatter>>, buffer: sourceview5::Buffer) {
        let window: gtk4::Window = window.clone().upcast();
        let doc_dir = preview_pane.doc_dir.clone();
        let last_markdown = preview_pane.last_markdown.clone();
        let preview_pane = preview_pane.clone();
        preview_pane.web_view.clone().connect_context_menu(move |_web_view, context_menu, hit_test_result| {
            if !hit_test_result.context_is_image() {
                return false;
            }
            let Some(image_uri) = hit_test_result.image_uri() else { return false };

            // Re-reconcile here (not just relying on whatever's already in
            // `frontmatter.media`) so a just-inserted image that hasn't been
            // through Medienverwaltung/the export dialog yet still gets a
            // working menu entry, not a silently-missing one.
            {
                let mut fm = frontmatter.borrow_mut();
                fm.media = media::reconcile(&fm.media, &last_markdown.borrow());
            }
            let doc_dir_value = doc_dir.borrow().clone();
            let Some(index) = item_index_for_image_uri(&frontmatter.borrow().media, &image_uri, doc_dir_value.as_deref()) else {
                return false;
            };

            let edit_action = gio::SimpleAction::new("edit-alt-text", None);
            {
                let frontmatter = frontmatter.clone();
                let window = window.clone();
                let doc_dir_value = doc_dir_value.clone();
                let buffer = buffer.clone();
                let preview_pane = preview_pane.clone();
                edit_action.connect_activate(move |_, _| {
                    crate::imagealt::open_dialog_for_index(&window, &frontmatter, index, &buffer, doc_dir_value.clone(), &preview_pane);
                });
            }
            let edit_item = webkit6::ContextMenuItem::from_gaction(&edit_action, &tr("Bildbeschriftung bearbeiten…"), None);
            context_menu.append(&edit_item);
            false
        });
    }

    /// Adds a "Bild bearbeiten…" item to the context menu when right-
    /// clicking directly on a rendered image whose source is a *local*
    /// file (`imageedit::is_local`) - editing a remote source (e.g. an
    /// image from a WordPress-imported article) would have nothing to
    /// write back to, so it's simply not offered rather than shown and
    /// then failing. Same dual-handler structure as
    /// `install_alt_text_menu` - see that method's doc comment.
    pub fn install_image_edit_menu(preview_pane: &Rc<Self>, window: &impl IsA<gtk4::Window>, frontmatter: Rc<RefCell<Frontmatter>>, buffer: sourceview5::Buffer) {
        let window: gtk4::Window = window.clone().upcast();
        let doc_dir = preview_pane.doc_dir.clone();
        let last_markdown = preview_pane.last_markdown.clone();
        preview_pane.web_view.clone().connect_context_menu(move |_web_view, context_menu, hit_test_result| {
            if !hit_test_result.context_is_image() {
                return false;
            }
            let Some(image_uri) = hit_test_result.image_uri() else { return false };

            {
                let mut fm = frontmatter.borrow_mut();
                fm.media = media::reconcile(&fm.media, &last_markdown.borrow());
            }
            let doc_dir_value = doc_dir.borrow().clone();
            let Some(index) = item_index_for_image_uri(&frontmatter.borrow().media, &image_uri, doc_dir_value.as_deref()) else {
                return false;
            };
            if !crate::imageedit::is_local(&frontmatter.borrow().media[index].source) {
                return false;
            }

            let action = gio::SimpleAction::new("edit-image", None);
            {
                let frontmatter = frontmatter.clone();
                let window = window.clone();
                let doc_dir_value = doc_dir_value.clone();
                let buffer = buffer.clone();
                action.connect_activate(move |_, _| {
                    crate::imageedit::open(&window, frontmatter.clone(), index, doc_dir_value.clone(), buffer.clone());
                });
            }
            let item = webkit6::ContextMenuItem::from_gaction(&action, &tr("Bild bearbeiten…"), None);
            context_menu.append(&item);
            false
        });
    }

    pub fn scroll_to_line(&self, line: i32) {
        self.web_view.evaluate_javascript(&format!("window.scrollToLine && window.scrollToLine({line});"), None, None, gio::Cancellable::NONE, |_| {});
    }

    /// Snaps the preview to its true top/bottom rather than "the block
    /// starting at line N" - `scroll_to_line`'s closest-preceding-block
    /// mapping is the right general-purpose sync, but at either end of the
    /// editor it systematically falls short: once the editor is scrolled as
    /// far as it goes, the line sitting at the *top* of its viewport (what
    /// `sync_editor_to_preview` reports) is rarely the article's actual
    /// last block, especially when a tall element (an image, an embed
    /// placeholder) makes the preview's scrollable range disproportionate
    /// to the editor's. `window.rs::sync_editor_to_preview` calls this
    /// instead of `scroll_to_line` specifically when the editor's own
    /// `vadjustment` is already at its min/max.
    pub fn scroll_to_edge(&self, bottom: bool) {
        let edge = if bottom { "bottom" } else { "top" };
        self.web_view.evaluate_javascript(&format!("window.scrollToEdge && window.scrollToEdge('{edge}');"), None, None, gio::Cancellable::NONE, |_| {});
    }

    /// The preview is a rendered *article*, not a browsing session (see the
    /// module doc comment) - a click on a link inside it must never
    /// navigate the preview itself away from the article. `callback` fires
    /// with the clicked link's URL instead, so the caller can open it
    /// somewhere actually meant for browsing (`window.rs` wires this to the
    /// "Browser" tab: load the URL there and switch to it). Only an actual
    /// link click is intercepted this way (`NavigationType::LinkClicked`) -
    /// the initial `load_html` (and any other internal navigation) is left
    /// alone so the article keeps rendering normally.
    pub fn connect_link_clicked(&self, callback: impl Fn(String) + 'static) {
        self.web_view.connect_decide_policy(move |_web_view, decision, decision_type| {
            if decision_type != webkit6::PolicyDecisionType::NavigationAction {
                return false;
            }
            let Some(nav_decision) = decision.downcast_ref::<webkit6::NavigationPolicyDecision>() else {
                return false;
            };
            let Some(action) = nav_decision.navigation_action() else {
                return false;
            };
            if action.navigation_type() != webkit6::NavigationType::LinkClicked {
                return false;
            }
            let Some(uri) = action.request().and_then(|request| request.uri()) else {
                return false;
            };
            nav_decision.ignore();
            callback(uri.to_string());
            true
        });
    }

    /// Wires the reverse leg of scroll-sync: `callback` fires with a source
    /// line number whenever the user scrolls the preview itself (not when a
    /// `scroll_to_line`/`scroll_to_edge` call from this side moves it - the
    /// rendered HTML's own script guards against echoing those back, see
    /// `render_html`'s `__suppressScrollEcho`). Two sentinel values stand
    /// in for "the preview is at its true top/bottom" rather than a real
    /// line number - `-1` for top, `-2` for bottom - the same "snap to the
    /// real edge instead of the nearest block" case `scroll_to_edge`'s own
    /// doc comment explains, just detected on the preview's side of the
    /// sync instead of the editor's. Every `WebView` has a
    /// `UserContentManager` of its own, so this only needs calling once,
    /// independent of how many times the page itself gets reloaded
    /// (`register_script_message_handler` isn't tied to a specific loaded
    /// document).
    pub fn connect_scroll(&self, callback: impl Fn(i32) + 'static) {
        let Some(manager) = self.web_view.user_content_manager() else { return };
        manager.register_script_message_handler(SCROLL_SYNC_HANDLER, None);
        manager.connect_script_message_received(Some(SCROLL_SYNC_HANDLER), move |_manager, value| {
            callback(value.to_int32());
        });
    }

    pub fn style(&self) -> PreviewStyle {
        self.style.get()
    }

    /// Called from the "Erscheinungsbild" settings page's style picker:
    /// persists the choice and re-renders immediately with the
    /// last-known Markdown, the same way an editor color-scheme change
    /// applies live without needing the dialog closed.
    pub fn set_style(&self, style: PreviewStyle) {
        self.style.set(style);
        save_preview_style(style);
        self.rerender();
    }

    /// The saved custom font, if any - `None` means each `PreviewStyle`
    /// still uses its own built-in font.
    pub fn font_override(&self) -> Option<String> {
        load_preview_font_override()
    }

    pub fn is_font_customized(&self) -> bool {
        load_preview_font_override().is_some()
    }

    pub fn set_font_override(&self, desc: &str) {
        save_preview_font_override(desc);
        self.rerender();
    }

    pub fn reset_font_override(&self) {
        reset_preview_font_override();
        self.rerender();
    }

    /// Snapshots `frontmatter` for the magazine-style header (`render_header`)
    /// and re-renders right away, preserving scroll - called from every
    /// place that loads/replaces the document (open/new/import/startup
    /// recovery) and from "Artikel-Eigenschaften" closing, *not* from the
    /// body-edit debounce (`update`/`update_preserving_scroll`), since none
    /// of the fields shown here ever change by editing the body text - see
    /// `last_frontmatter`'s own doc comment on the struct.
    pub fn set_article_header(&self, frontmatter: &Frontmatter) {
        *self.last_frontmatter.borrow_mut() = frontmatter.clone();
        self.rerender_preserving_scroll();
    }

    pub fn show_article_header(&self) -> bool {
        self.show_header.get()
    }

    /// Called from the Vorschau tab's own header-toggle button
    /// (`window.rs`) - persists the choice and re-renders immediately, the
    /// same way `set_style` does for the typography picker.
    pub fn set_show_article_header(&self, enabled: bool) {
        self.show_header.set(enabled);
        save_show_header(enabled);
        self.rerender();
    }

    /// Called from "Erscheinungsbild"'s scheme grid whenever the user picks
    /// a different editor color scheme - re-renders with that scheme's own
    /// colors immediately, the same way `set_style` applies a typography
    /// change live. Public (unlike `rerender`) since the scheme itself is
    /// owned by `appearance.rs`, not this pane.
    pub fn refresh(&self) {
        self.rerender();
    }

    fn rerender(&self) {
        let dark = adw::StyleManager::default().is_dark();
        let html = render_html(
            &self.last_markdown.borrow(),
            self.style.get(),
            dark,
            &self.last_media.borrow(),
            0.0,
            &self.last_frontmatter.borrow(),
            self.show_header.get(),
            appearance::current_scheme_colors(),
        );
        self.web_view.load_html(&html, base_uri(self.doc_dir.borrow().as_deref()).as_deref());
    }

    /// Same full-page reload as `rerender`, but reads the `WebView`'s
    /// current scroll position first and bakes it into the freshly
    /// rendered HTML (via `render_html`'s `scroll_y`) so the reload lands
    /// back where it started - `load_html` always resets scroll to the top
    /// on its own, and there's no "restore scroll after this specific load
    /// finishes" hook to use instead, so the position is baked directly
    /// into the page's own startup script rather than applied from the
    /// Rust side after the fact.
    fn rerender_preserving_scroll(&self) {
        let style = self.style.get();
        let last_markdown = self.last_markdown.clone();
        let last_media = self.last_media.clone();
        let doc_dir = self.doc_dir.borrow().clone();
        let last_frontmatter = self.last_frontmatter.clone();
        let show_header = self.show_header.get();
        let web_view = self.web_view.clone();
        self.web_view.evaluate_javascript("window.scrollY", None, None, gio::Cancellable::NONE, move |result| {
            let scroll_y = result.map(|value| value.to_double()).unwrap_or(0.0);
            let dark = adw::StyleManager::default().is_dark();
            let html = render_html(&last_markdown.borrow(), style, dark, &last_media.borrow(), scroll_y, &last_frontmatter.borrow(), show_header, appearance::current_scheme_colors());
            web_view.load_html(&html, base_uri(doc_dir.as_deref()).as_deref());
        });
    }
}

/// A `file://` URI for `dir`, suitable as a `WebView` base URI - built via
/// `gio::File` rather than a hand-formatted `format!("file://{}", ...)` so
/// a directory path containing characters that need percent-encoding is
/// still handled correctly.
pub(crate) fn base_uri(dir: Option<&Path>) -> Option<String> {
    let dir = dir?;
    let uri = gio::File::for_path(dir).uri();
    Some(if uri.ends_with('/') { uri.to_string() } else { format!("{uri}/") })
}

/// Matches a WebKit-resolved image URI back to the `MediaItem` it renders.
/// A `file://` URI (the normal case for a locally-referenced image, since
/// the preview's base URI - see `base_uri` above - turns a relative
/// `![](photo.png)` into an absolute `file://` address before WebKit ever
/// sees it) is converted back to a plain path and compared against each
/// item's Markdown source resolved the same way `export::resolve_local_path`
/// resolves it for upload/hashing; anything else (a remote `http(s)://` URL,
/// `gio::File::path()` returns `None` for those) is compared directly, since
/// a remote source is never rewritten.
fn item_index_for_image_uri(items: &[MediaItem], image_uri: &str, doc_dir: Option<&Path>) -> Option<usize> {
    if let Some(path) = gio::File::for_uri(image_uri).path() {
        return items.iter().position(|item| crate::export::resolve_local_path(&item.source, doc_dir) == path);
    }
    items.iter().position(|item| item.source == image_uri)
}

/// One typographic style's CSS, in its light and dark variant. Baked
/// directly into the generated document (see the module docs for why),
/// not left to a `prefers-color-scheme` media query.
fn style_css(style: PreviewStyle, dark: bool) -> &'static str {
    match (style, dark) {
        (PreviewStyle::Modern, false) => {
            "body { font-family: -apple-system, Cantarell, sans-serif; max-width: 46rem; margin: 2rem auto; padding: 0 1rem; line-height: 1.6; color: #1e1e1e; background: #ffffff; }
             pre { background: #f2f2f2; padding: .75rem; border-radius: 6px; overflow-x: auto; }
             code { background: #f2f2f2; padding: .1rem .3rem; border-radius: 4px; }
             pre code { background: none; padding: 0; }
             blockquote { border-left: 4px solid #ccc; margin-left: 0; padding-left: 1rem; color: #555; }
             a { color: #1c71d8; }
             hr { border: none; border-top: 1px solid #ccc; }"
        }
        (PreviewStyle::Modern, true) => {
            "body { font-family: -apple-system, Cantarell, sans-serif; max-width: 46rem; margin: 2rem auto; padding: 0 1rem; line-height: 1.6; color: #e3e3e3; background: #1e1e1e; }
             pre { background: #2d2d2d; padding: .75rem; border-radius: 6px; overflow-x: auto; }
             code { background: #2d2d2d; padding: .1rem .3rem; border-radius: 4px; }
             pre code { background: none; padding: 0; }
             blockquote { border-left: 4px solid #555; margin-left: 0; padding-left: 1rem; color: #aaa; }
             a { color: #78aeed; }
             hr { border: none; border-top: 1px solid #444; }"
        }
        (PreviewStyle::Classic, false) => {
            "body { font-family: Georgia, 'Times New Roman', serif; max-width: 38rem; margin: 2rem auto; padding: 0 1rem; line-height: 1.7; color: #222222; background: #ffffff; text-align: justify; }
             h1, h2, h3 { font-family: Georgia, serif; }
             p { text-indent: 1.5em; margin: 0 0 .2em 0; }
             blockquote { font-style: italic; border-left: 2px solid #999; padding-left: 1rem; color: #444; }
             pre { background: #f5f0e6; padding: .75rem; border: 1px solid #ddd; overflow-x: auto; text-indent: 0; }
             code { background: #f5f0e6; padding: .1rem .3rem; }
             pre code { background: none; padding: 0; }
             a { color: #8a3324; }
             hr { border: none; border-top: 1px double #999; margin: 2rem 0; }"
        }
        (PreviewStyle::Classic, true) => {
            "body { font-family: Georgia, 'Times New Roman', serif; max-width: 38rem; margin: 2rem auto; padding: 0 1rem; line-height: 1.7; color: #dddddd; background: #181818; text-align: justify; }
             h1, h2, h3 { font-family: Georgia, serif; }
             p { text-indent: 1.5em; margin: 0 0 .2em 0; }
             blockquote { font-style: italic; border-left: 2px solid #666; padding-left: 1rem; color: #bbb; }
             pre { background: #242220; padding: .75rem; border: 1px solid #3a3a3a; overflow-x: auto; text-indent: 0; }
             code { background: #242220; padding: .1rem .3rem; }
             pre code { background: none; padding: 0; }
             a { color: #e0947e; }
             hr { border: none; border-top: 1px double #555; margin: 2rem 0; }"
        }
        (PreviewStyle::Sepia, false) => {
            "body { font-family: Georgia, serif; max-width: 40rem; margin: 2rem auto; padding: 0 1rem; line-height: 1.7; color: #5b4636; background: #f4ecd8; }
             pre { background: #ece0c8; padding: .75rem; border-radius: 6px; overflow-x: auto; }
             code { background: #ece0c8; padding: .1rem .3rem; border-radius: 4px; }
             pre code { background: none; padding: 0; }
             blockquote { border-left: 4px solid #c8b78e; margin-left: 0; padding-left: 1rem; color: #7a6650; }
             a { color: #8a5a2b; }
             hr { border: none; border-top: 1px solid #c8b78e; }"
        }
        (PreviewStyle::Sepia, true) => {
            "body { font-family: Georgia, serif; max-width: 40rem; margin: 2rem auto; padding: 0 1rem; line-height: 1.7; color: #d9c9a3; background: #2b2418; }
             pre { background: #3a3223; padding: .75rem; border-radius: 6px; overflow-x: auto; }
             code { background: #3a3223; padding: .1rem .3rem; border-radius: 4px; }
             pre code { background: none; padding: 0; }
             blockquote { border-left: 4px solid #5c4f34; margin-left: 0; padding-left: 1rem; color: #c2ab7e; }
             a { color: #d4a15f; }
             hr { border: none; border-top: 1px solid #5c4f34; }"
        }
    }
}

/// Overrides `style_css`'s own hardcoded `pre`/`code` colors with
/// `code_colors` (the active editor scheme's "text" style, see
/// `appearance::current_scheme_colors`) via plain CSS cascade - appended
/// after `style_css`'s own block, so a later same-specificity rule simply
/// wins, rather than rewriting each of the six `style_css` variants by
/// hand. Only the two color properties are overridden; each style's own
/// padding/border-radius/etc. for `pre`/`code` is untouched. `None` (no
/// resolvable scheme) leaves every style's own hardcoded fallback colors
/// exactly as they were before this existed.
fn code_block_css(code_colors: Option<(&str, &str)>) -> String {
    let Some((background, foreground)) = code_colors else { return String::new() };
    format!(
        "pre {{ background: {background}; color: {foreground}; }}
         code {{ background: {background}; color: {foreground}; }}
         pre code {{ background: none; color: inherit; }}"
    )
}

#[allow(clippy::too_many_arguments)]
pub fn render_html(markdown: &str, style: PreviewStyle, dark: bool, media: &[MediaItem], scroll_y: f64, frontmatter: &Frontmatter, show_header: bool, code_colors: Option<(String, String)>) -> String {
    let body = render_body_with_line_anchors(markdown, media);
    let header = if show_header { render_header(frontmatter) } else { String::new() };
    let css = style_css(style, dark);
    let code_css = code_block_css(code_colors.as_ref().map(|(background, foreground)| (background.as_str(), foreground.as_str())));
    // A user-picked font (if any) overrides just the two font properties,
    // applied after the style's own block so the cascade lets it win
    // while everything else the style defines (colors, indentation,
    // justification, ...) stays intact.
    let font_override_css = load_preview_font_override()
        .map(|desc| format!("body {{ {} }}", fontutil::css_declarations(&pango::FontDescription::from_string(&desc))))
        .unwrap_or_default();

    format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><style>
{css}
{code_css}
{font_override_css}
img {{ max-width: 100%; height: auto; }}
video, audio, iframe {{ max-width: 100%; }}
/* WebKitGTK has no PDF plugin, so `wp:file`'s inline-preview `<object>`
   (`<object class="wp-block-file__embed" style="width:100%;height:NNpx">`)
   renders as a large blank/black box with a broken-plugin glyph instead of
   the PDF - hidden outright rather than merely size-capped, since an empty
   box that size serves no purpose. The block's own "Herunterladen" link
   (a sibling `<a>`, not part of this element) stays visible either way,
   already the only way to actually reach the file from this preview. */
object.wp-block-file__embed {{ display: none; }}
/* `wp:cover`'s children (the background image/video, the color-overlay
   span, and the actual text content) are meant to sit layered on top of
   each other, filling the block's own `min-height` - but that layering is
   normally done by WordPress's own core block CSS, which this preview
   never loads. Without it they're just plain block-level elements stacking
   one after another in normal flow: the (empty, since it's an `<img>` or a
   `background-image`-only `<div>` with no content of its own) background
   layers collapse to zero height, the actual text top-aligns inside the
   `min-height` box, and everything below it - the box's own unfilled
   height - shows up as a large blank gap before the next block. Restores
   the real position/flex structure (a stable, standard part of Gutenberg's
   own block markup, unlike a theme's specific gradient/color presets,
   which this preview still can't resolve and so still won't show).
   `has-text-align-*` alongside it - `wp:paragraph`'s own alignment is a
   class, not an inline style, so it needs the same kind of rule to have
   any visible effect at all. */
.wp-block-cover {{ position: relative; display: flex; align-items: center; justify-content: center; overflow: hidden; }}
.wp-block-cover__image-background, .wp-block-cover__video-background {{ position: absolute; inset: 0; width: 100%; height: 100%; object-fit: cover; background-size: cover; background-position: 50% 50%; z-index: 0; }}
.wp-block-cover__background {{ position: absolute; inset: 0; z-index: 1; }}
.wp-block-cover__inner-container {{ position: relative; z-index: 1; width: 100%; }}
.has-text-align-center {{ text-align: center; }}
.has-text-align-left {{ text-align: left; }}
.has-text-align-right {{ text-align: right; }}
/* Same missing-WordPress-core-CSS problem as `.wp-block-cover` above, for
   the blocks `render_special_fenced_block` now renders as their real
   Gutenberg markup instead of raw fence text: columns/buttons/gallery lay
   themselves out with flex, and are otherwise just plain stacked
   block-level elements without it. `.wp-block-details` needs no layout
   CSS at all - `<details>`/`<summary>` are native, already-interactive
   HTML elements, unlike every other block on this list. */
.wp-block-columns {{ display: flex; flex-wrap: wrap; gap: 2rem; }}
.wp-block-column {{ flex: 1; min-width: 0; }}
.wp-block-buttons {{ display: flex; flex-wrap: wrap; gap: .5rem; }}
.wp-block-button__link {{ display: inline-block; padding: .6rem 1.2rem; border: 2px solid currentColor; border-radius: 4px; text-decoration: none; color: inherit; }}
.wp-block-gallery.has-nested-images {{ display: flex; flex-wrap: wrap; gap: 1rem; }}
.wp-block-gallery.has-nested-images figure.wp-block-image {{ margin: 0; flex: 1 1 240px; }}
.wp-block-gallery.has-nested-images figure.wp-block-image img {{ width: 100%; height: 100%; object-fit: cover; display: block; }}
.wp-block-pullquote {{ text-align: center; margin: 2rem 0; padding: 1.5rem 0; border-top: 3px solid currentColor; border-bottom: 3px solid currentColor; }}
.wp-block-pullquote blockquote {{ margin: 0; font-size: 1.5rem; font-style: italic; }}
.wp-block-pullquote cite {{ display: block; margin-top: .75rem; font-size: 1rem; font-style: normal; }}
.wp-block-details summary {{ cursor: pointer; font-weight: 600; }}
table {{ border-collapse: collapse; }}
th, td {{ border: 1px solid #ccc; padding: .4rem .6rem; }}
{BADGE_CSS}
{EMBED_CSS}
{HEADER_CSS}
</style></head><body>{header}{body}<script>
// `__suppressScrollEcho` distinguishes a scroll *this script* performs
// (restoring position after a reload, or `scrollToLine` driven by the
// editor) from one the user just did by hand with the mouse/trackpad - the
// DOM's plain `scroll` event can't tell those apart on its own, so every
// programmatic scroll below sets this first and clears it shortly after,
// and the listener that reports back to the editor (the reverse leg of
// scroll-sync, see `PreviewPane::connect_scroll`) skips reporting while
// it's set. Without this, every editor-driven preview scroll would
// immediately echo back and nudge the editor again.
window.__suppressScrollEcho = false;
window.__scrollEchoReleaseTimer = null;
// Arms the release of `__suppressScrollEcho` for a smooth-scroll call that
// just started - `scrollend` fires once the animation genuinely settles
// (its duration scales with distance, so a fixed short delay would let the
// reverse-sync listener see mid-animation positions and jitter the editor
// while the preview is still gliding); the timeout alongside it is only a
// safety net in case `scrollend` never fires for some reason.
window.__armScrollEchoRelease = function() {{
  window.__suppressScrollEcho = true;
  if (window.__scrollEchoReleaseTimer) {{ clearTimeout(window.__scrollEchoReleaseTimer); }}
  const release = function() {{
    window.__suppressScrollEcho = false;
    window.__scrollEchoReleaseTimer = null;
  }};
  window.addEventListener('scrollend', release, {{once: true}});
  window.__scrollEchoReleaseTimer = setTimeout(release, 1000);
}};
window.scrollToLine = function(line) {{
  const blocks = document.querySelectorAll('[data-line]');
  let target = null;
  for (const b of blocks) {{
    if (parseInt(b.getAttribute('data-line'), 10) <= line) {{ target = b; }} else {{ break; }}
  }}
  if (target) {{
    window.__armScrollEchoRelease();
    target.scrollIntoView({{block: 'start', behavior: 'smooth'}});
  }}
}};
// Snaps to the page's true top/bottom rather than a block boundary - see
// `PreviewPane::scroll_to_edge`'s doc comment for why `scrollToLine` alone
// can't reliably reach either end.
window.scrollToEdge = function(edge) {{
  window.__armScrollEchoRelease();
  window.scrollTo({{top: edge === 'bottom' ? document.body.scrollHeight : 0, behavior: 'smooth'}});
}};
// The reverse of `scrollToLine`'s search: which block is at (or just above)
// the current scroll position, i.e. what the user is looking at right now.
window.currentTopLine = function() {{
  const blocks = document.querySelectorAll('[data-line]');
  let target = null;
  for (const b of blocks) {{
    if (b.offsetTop <= window.scrollY + 2) {{ target = b; }} else {{ break; }}
  }}
  return target ? parseInt(target.getAttribute('data-line'), 10) : 1;
}};
window.__scrollSyncTimer = null;
window.addEventListener('scroll', function() {{
  if (window.__suppressScrollEcho) return;
  if (window.__scrollSyncTimer) clearTimeout(window.__scrollSyncTimer);
  window.__scrollSyncTimer = setTimeout(function() {{
    if (window.webkit && window.webkit.messageHandlers.{SCROLL_SYNC_HANDLER}) {{
      // Sentinels for "already at the true top/bottom" (-1/-2) - see
      // `PreviewPane::connect_scroll`'s doc comment for why the editor
      // side needs to know this instead of just the nearest line.
      const atBottom = (window.scrollY + window.innerHeight) >= (document.body.scrollHeight - 2);
      const atTop = window.scrollY <= 2;
      const payload = atBottom ? -2 : (atTop ? -1 : window.currentTopLine());
      window.webkit.messageHandlers.{SCROLL_SYNC_HANDLER}.postMessage(payload);
    }}
  }}, 80);
}});
if ({scroll_y} > 0) {{ window.__suppressScrollEcho = true; window.scrollTo(0, {scroll_y}); setTimeout(function() {{ window.__suppressScrollEcho = false; }}, 200); }}
</script></body></html>"#
    )
}

/// Renders the document as a sequence of `<div data-line="N">...</div>`
/// wrappers, one per top-level Markdown block, using pulldown-cmark's own
/// HTML renderer for each block's inner content so output stays consistent
/// with plain rendering.
fn render_body_with_line_anchors(markdown: &str, media: &[MediaItem]) -> String {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let events: Vec<(Event, std::ops::Range<usize>)> = Parser::new_ext(markdown, options).into_offset_iter().collect();

    let mut out = String::new();
    let mut i = 0;
    while i < events.len() {
        match &events[i].0 {
            Event::Start(Tag::CodeBlock(kind)) => {
                let kind = kind.clone();
                let end = find_matching_end(&events, i, &TagEnd::CodeBlock);
                let line = line_number(markdown, events[i].1.start);
                let inner = render_code_block_with_line_anchors(&events[i..=end], &kind, line);
                out.push_str(&format!("<div data-line=\"{line}\">{inner}</div>\n"));
                i = end + 1;
            }
            Event::Start(tag) => {
                let end_marker = tag.to_end();
                let end = find_matching_end(&events, i, &end_marker);
                let line = line_number(markdown, events[i].1.start);
                let embed_url = matches!(tag, Tag::Paragraph)
                    .then(|| events[i + 1..end].iter().map(|(event, _)| event.clone()).collect::<Vec<_>>())
                    .and_then(|inner_events| gutenberg::lone_embed_url(&inner_events));
                let inner = match embed_url {
                    Some(url) => render_embed_placeholder(&url),
                    None => {
                        let mut inner = String::new();
                        pulldown_cmark::html::push_html(&mut inner, events[i..=end].iter().map(|(event, _)| event.clone()));
                        inner
                    }
                };
                // A raw HTML block (e.g. one "paragraph" of a `wp:group`'s
                // own verbatim-preserved markup, see `inject_group_flex_styles`)
                // can have its own `<div>` split from its matching `</div>`
                // by a blank line in the source markdown - each side lands
                // in a *different* top-level block here. Wrapping a block
                // like that in our own `<div data-line>` would insert a
                // second, unrelated div boundary between them, prematurely
                // closing the real one (a bare `</div>` always closes the
                // innermost *currently open* div, which would be ours, not
                // theirs) and cutting its later children out of it entirely
                // - fatal for anything, like a flex/grid `wp:group`, that
                // depends on its children actually being its DOM children.
                // Leaving an unbalanced block unwrapped lets its real div
                // tag reach across block boundaries intact; the wrapped
                // blocks in between still nest correctly *inside* it, since
                // each of those, individually, opens and closes exactly as
                // many divs as it has.
                let div_balance = inner.matches("<div").count() as isize - inner.matches("</div>").count() as isize;
                if div_balance == 0 {
                    out.push_str(&format!("<div data-line=\"{line}\">{inner}</div>\n"));
                } else {
                    out.push_str(&inner);
                }
                i = end + 1;
            }
            Event::Rule => {
                let line = line_number(markdown, events[i].1.start);
                out.push_str(&format!("<div data-line=\"{line}\"><hr/></div>\n"));
                i += 1;
            }
            _ => i += 1,
        }
    }
    wrap_images_with_badges(&rewrite_media_tags(&inject_group_flex_styles(&out)), media)
}

/// `wp:group`'s flex/grid layout is driven entirely by its `layout` JSON
/// attribute plus a page-level `<style>` block WordPress generates
/// separately, keyed to a per-block `wp-container-*` class - neither of
/// which makes it into a post's own saved content at all, so this can't be
/// fixed the way `.wp-block-cover`'s own missing-core-CSS problem was
/// (real, stable class names to hang static rules off). `wp:group` isn't a
/// block this crate recognizes at all, so its whole comment (JSON attrs
/// included) already survives verbatim in the raw HTML via `make_block`'s
/// "unrecognized block" fallback (`crates/gutenberg`'s `reverse.rs`) -
/// read directly here instead, and turned into an inline `style` written
/// straight onto the group's own `<div>`. Only `"type":"flex"` (the one
/// shape actually seen in real content so far) is handled; a `"grid"`
/// layout is left alone rather than guessed at with nothing to check it
/// against.
fn inject_group_flex_styles(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(marker) = rest.find("<!-- wp:group ") {
        out.push_str(&rest[..marker]);
        let Some(comment_end_rel) = rest[marker..].find("-->") else {
            out.push_str(&rest[marker..]);
            return out;
        };
        let attrs_start = marker + "<!-- wp:group ".len();
        let comment_end = marker + comment_end_rel + 3;
        let style = flex_style_for_group_attrs(&rest[attrs_start..marker + comment_end_rel]);
        out.push_str(&rest[marker..comment_end]);
        rest = &rest[comment_end..];

        let Some(style) = style else { continue };
        let Some(div_start) = rest.find("<div class=\"wp-block-group") else { continue };
        let Some(tag_end_rel) = rest[div_start..].find('>') else { continue };
        let tag_end = div_start + tag_end_rel;
        let tag = &rest[..tag_end];
        match tag.find(" style=\"").and_then(|style_attr| tag[style_attr + " style=\"".len()..].find('"').map(|end| style_attr + " style=\"".len() + end)) {
            // An existing `style` attribute (from border/background/spacing
            // support) needs our declarations appended *inside* its quotes -
            // a second, separate `style=""` attribute on the same tag would
            // just be ignored by the HTML parser.
            Some(closing_quote) => {
                out.push_str(&tag[..closing_quote]);
                out.push_str(&style);
                out.push_str(&tag[closing_quote..]);
            }
            None => {
                out.push_str(tag);
                out.push_str(&format!(" style=\"{style}\""));
            }
        }
        out.push('>');
        rest = &rest[tag_end + 1..];
    }
    out.push_str(rest);
    out
}

/// The inline `style` value for a `wp:group` comment's JSON attrs, or
/// `None` if its `layout.type` isn't `"flex"` (includes the common case of
/// no `layout` at all, WordPress's own default "constrained" layout, which
/// needs no flex styling here since normal block flow already matches it).
fn flex_style_for_group_attrs(attrs: &str) -> Option<String> {
    if !attrs.contains("\"type\":\"flex\"") {
        return None;
    }
    let vertical = attrs.contains("\"orientation\":\"vertical\"");
    let mut style = String::from("display:flex;");
    style.push_str(if vertical { "flex-direction:column;align-items:flex-start;" } else { "flex-direction:row;align-items:center;" });
    style.push_str(if attrs.contains("\"flexWrap\":\"nowrap\"") { "flex-wrap:nowrap;" } else { "flex-wrap:wrap;" });
    if let Some(justify) = extract_json_string_value(attrs, "justifyContent") {
        // WordPress's own "left"/"right" aren't valid CSS `justify-content`
        // keywords - every other value it uses (`center`/`space-between`/
        // `flex-start`/`flex-end`/`space-around`) already is, so those pass
        // through unchanged.
        let css_value = match justify.as_str() {
            "left" => "flex-start",
            "right" => "flex-end",
            other => other,
        };
        style.push_str(&format!("justify-content:{css_value};"));
    }
    Some(style)
}

/// Reads a `"key":"value"` string out of a JSON-ish attrs string - the
/// same hand-rolled-scanner approach `crates/gutenberg`'s own
/// `extract_json_string` uses, not a full JSON parser, since this crate
/// only ever needs one or two known keys out of a small, well-defined
/// attrs shape.
fn extract_json_string_value(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = json.find(&needle)? + needle.len();
    let end = json[start..].find('"')? + start;
    Some(json[start..end].to_string())
}

/// Rewrites `<img>` tags whose `src` is a local video/audio file (by
/// extension - mirrors `crates/gutenberg`'s own export-time classification,
/// `as_lone_media`/`media_kind`, since this app reuses `![]()` image syntax
/// for all local media) into real `<video controls>`/`<audio controls>`
/// tags, so the live preview shows an actual player instead of a broken
/// image icon. Runs before `wrap_images_with_badges`, so a converted tag -
/// no longer an `<img>` - is simply left alone by it like any other
/// non-image element; upload/alt/format badges don't apply to video/audio.
fn rewrite_media_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find("<img ") {
        out.push_str(&rest[..start]);
        let Some(tag_end_rel) = rest[start..].find('>') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let tag_end = start + tag_end_rel + 1;
        let tag = &rest[start..tag_end];
        let src = extract_attr(tag, "src").unwrap_or_default();
        match media_tag_for(&src) {
            Some(media_tag) => out.push_str(&format!("<{media_tag} controls src=\"{src}\"></{media_tag}>")),
            None => out.push_str(tag),
        }
        rest = &rest[tag_end..];
    }
    out.push_str(rest);
    out
}

/// The HTML tag a media `src` should become - `None` means "not
/// video/audio, leave the `<img>` as it is".
fn media_tag_for(src: &str) -> Option<&'static str> {
    match document::media_reference_kind(src) {
        document::MediaReferenceKind::Video => Some("video"),
        document::MediaReferenceKind::Audio => Some("audio"),
        document::MediaReferenceKind::Image => None,
    }
}

const EMBED_CSS: &str = ".embed-placeholder { display: flex; flex-direction: column; align-items: center; justify-content: center; gap: .5rem; aspect-ratio: 16 / 9; max-width: 100%; margin: 1rem auto; padding: 1rem; box-sizing: border-box; text-align: center; border: 1px solid rgba(127, 127, 127, 0.3); border-radius: 8px; background: rgba(127, 127, 127, 0.08); }
.embed-placeholder .embed-icon { font-size: 2.5rem; opacity: .6; }
.embed-placeholder .embed-label { font-weight: 600; opacity: .85; }
.embed-placeholder .embed-url { font-size: .8em; opacity: .55; word-break: break-all; max-width: 90%; }";

/// A lone embeddable URL (see `gutenberg::lone_embed_url`) renders as a
/// fixed-aspect-ratio placeholder card instead of a live embed - matching
/// the user-facing request this was built for ("zeige die Vorschau als
/// Platzhalter an"), and avoiding a live `WebView` silently loading a
/// third-party iframe/script on every keystroke. Sized with a real
/// `aspect-ratio` (16:9, the same shape WordPress's own block editor gives
/// an embed) rather than a fixed pixel height, so it takes up roughly the
/// space the real embed will - the reason scroll-sync doesn't need any
/// special-casing for this block, see the module doc comment's note on
/// `data-line` anchoring by source line rather than proportion.
fn render_embed_placeholder(url: &str) -> String {
    let (icon, label) = embed_placeholder_label(url);
    format!(
        "<div class=\"embed-placeholder\"><span class=\"embed-icon\">{icon}</span><span class=\"embed-label\">{}</span><span class=\"embed-url\">{}</span></div>",
        glib::markup_escape_text(&label),
        glib::markup_escape_text(url)
    )
}

/// An icon glyph and display label for a lone embed URL - German for the
/// generic fallback (matching this app's UI language elsewhere), but the
/// known providers' own names are proper nouns and stay untranslated.
fn embed_placeholder_label(url: &str) -> (&'static str, String) {
    match gutenberg::embed_provider(url) {
        Some((_, "youtube")) => ("▶", tr("YouTube-Video")),
        Some((_, "vimeo")) => ("▶", tr("Vimeo-Video")),
        Some((_, "twitter")) => ("🔗", "Twitter/X".to_string()),
        Some((_, "instagram")) => ("🔗", "Instagram".to_string()),
        Some((_, "soundcloud")) => ("🎵", "SoundCloud".to_string()),
        Some((_, "spotify")) => ("🎵", "Spotify".to_string()),
        _ => ("🔗", tr("Eingebetteter Inhalt")),
    }
}

const BADGE_CSS: &str = ".img-wrap { position: relative; display: inline-block; max-width: 100%; }
.img-wrap img { display: block; }
.img-badges { position: absolute; right: 6px; bottom: 6px; display: flex; gap: 4px; }
.img-badge { background: rgba(0, 0, 0, 0.65); color: #fff; font: 11px/1.4 -apple-system, Cantarell, sans-serif; font-weight: 600; letter-spacing: .02em; padding: 2px 6px; border-radius: 4px; }
.img-caption { display: block; text-align: center; font-size: .85em; opacity: .7; margin-top: .35em; }";

/// Style-agnostic (see `BADGE_CSS`/`EMBED_CSS` above for the same
/// reasoning) - no color or font-family of its own, so it inherits
/// whichever `PreviewStyle` is active; `rgba(127, 127, 127, α)` neutrals
/// and `opacity` for secondary text work the same way across all three
/// styles' light/dark variants. `text-align`/`text-indent` are reset
/// explicitly on the title/excerpt since Classic's `body { text-align:
/// justify }` / `p { text-indent: 1.5em }` would otherwise bleed into them.
/// Categories and tags share one `.article-header-taxonomy` row - each its
/// own `.article-header-categories`/`.article-header-tags` flex group
/// (`gap`, not per-chip margins, so wrapping stays even at any width),
/// with `justify-content: space-between` on the shared row pushing
/// categories to the start and tags to the end when both groups are
/// present (a lone group just sits at the start - nothing to be "the other
/// side" of). Both share `.article-header-chip`'s pill shape -
/// `.article-header-tag` only swaps the fill for an outline, so tags read
/// as a visually distinct but equally deliberate group, not unstyled
/// leftover text next to a "real" filled category chip.
const HEADER_CSS: &str = ".article-header { margin: 0 0 2.5rem 0; padding-bottom: 1.75rem; border-bottom: 1px solid rgba(127, 127, 127, 0.25); }
.article-header-image { display: block; width: 100%; max-height: 22rem; object-fit: cover; border-radius: 8px; margin: 0 0 1.25rem 0; }
.article-header-title { font-size: 2rem; font-weight: 700; line-height: 1.25; margin: 0 0 .6rem 0; text-align: left; text-indent: 0; }
.article-header-excerpt { font-size: 1.1em; opacity: .75; margin: 0 0 1rem 0; text-align: left; text-indent: 0; }
.article-header-meta { display: flex; flex-wrap: wrap; align-items: center; gap: .4rem; font-size: .82em; opacity: .6; margin: 0 0 1rem 0; }
.article-header-meta a { opacity: 1; overflow-wrap: anywhere; }
.article-header-taxonomy { display: flex; flex-wrap: wrap; justify-content: space-between; align-items: center; gap: .5rem 1rem; margin: 0; }
.article-header-categories, .article-header-tags { display: flex; flex-wrap: wrap; align-items: center; gap: .4rem; }
.article-header-chip { display: inline-flex; align-items: center; background: rgba(127, 127, 127, 0.18); border-radius: 999px; padding: .2rem .75rem; font-size: .78em; font-weight: 500; line-height: 1.4; white-space: nowrap; }
.article-header-tag { background: transparent; border: 1px solid rgba(127, 127, 127, 0.35); }";

/// The magazine-style header shown above the article body (see
/// `render_html`'s `show_header`) - the same fields "Artikel-Eigenschaften"
/// (`properties.rs`) collects, so this doubles as a live "how would this
/// look as a teaser" summary of that dialog. Empty for a document with no
/// title yet (a brand new, still-untitled article) rather than an
/// empty-looking box with nothing but a status chip in it.
fn render_header(frontmatter: &Frontmatter) -> String {
    if frontmatter.title.trim().is_empty() {
        return String::new();
    }

    let mut html = String::from("<header class=\"article-header\">");

    if let Some(image) = frontmatter.featured_image.as_deref().filter(|s| !s.is_empty()) {
        html.push_str(&format!(
            "<img class=\"article-header-image\" src=\"{}\" alt=\"{}\">",
            glib::markup_escape_text(image),
            glib::markup_escape_text(frontmatter.featured_image_alt.as_deref().unwrap_or_default())
        ));
    }

    html.push_str(&format!("<h1 class=\"article-header-title\">{}</h1>", glib::markup_escape_text(&frontmatter.title)));

    if let Some(excerpt) = frontmatter.excerpt.as_deref().filter(|s| !s.trim().is_empty()) {
        html.push_str(&format!("<p class=\"article-header-excerpt\">{}</p>", glib::markup_escape_text(excerpt)));
    }

    let mut meta_parts = vec![format!("<span>{}</span>", glib::markup_escape_text(&frontmatter.status.label()))];
    if let Some(url) = article_url_preview(frontmatter) {
        meta_parts.push(format!("<a href=\"{}\">{}</a>", glib::markup_escape_text(&url), glib::markup_escape_text(&url)));
    }
    html.push_str(&format!("<div class=\"article-header-meta\">{}</div>", meta_parts.join(" · ")));

    if !frontmatter.categories.is_empty() || !frontmatter.tags.is_empty() {
        html.push_str("<div class=\"article-header-taxonomy\">");
        if !frontmatter.categories.is_empty() {
            let chips: String = frontmatter.categories.iter().map(|name| format!("<span class=\"article-header-chip\">{}</span>", glib::markup_escape_text(name))).collect();
            html.push_str(&format!("<div class=\"article-header-categories\">{chips}</div>"));
        }
        if !frontmatter.tags.is_empty() {
            let chips: String = frontmatter
                .tags
                .iter()
                .map(|name| format!("<span class=\"article-header-chip article-header-tag\">#{}</span>", glib::markup_escape_text(name)))
                .collect();
            html.push_str(&format!("<div class=\"article-header-tags\">{chips}</div>"));
        }
        html.push_str("</div>");
    }

    html.push_str("</header>");
    html
}

/// A best-effort article URL from the configured WordPress site's domain
/// plus the article's own slug - deliberately simpler than
/// `properties.rs`'s own `seo_url_preview` (no category-slug prefix, which
/// would need that dialog's term cache plumbed all the way into the
/// preview pane just for this), so it's a rough preview rather than the
/// guaranteed-exact permalink. `None` when there's nothing to build one
/// from (no WordPress connection configured yet, or no slug set) rather
/// than showing a broken-looking partial URL. A real link - clicking it
/// goes through the same `connect_link_clicked` interception every other
/// preview link already does, opening it in the Browser tab instead of
/// navigating the preview itself away from the article.
fn article_url_preview(frontmatter: &Frontmatter) -> Option<String> {
    build_article_url_preview(&wpsite::load().url, &frontmatter.slug)
}

/// The pure part of `article_url_preview`, split out so it's testable
/// without depending on this machine's own `$XDG_CONFIG_HOME/blocksmith/
/// wordpress.conf` - `wpsite::load()` reads real on-disk state, which
/// would make a test asserting "no site configured" fail on any machine
/// (this one included) that actually has one set up.
fn build_article_url_preview(domain: &str, slug: &str) -> Option<String> {
    if domain.is_empty() || slug.is_empty() {
        return None;
    }
    Some(format!("{}/{}/", domain.trim_end_matches('/'), slug))
}

/// Wraps every `<img ...>` tag in `html` with a `.img-wrap` container and,
/// when the image matches a tracked `MediaItem`, a bottom-right badge
/// cluster (see `badges_html`) plus a visible caption underneath it if one
/// is set - a lightweight string-level pass rather than a full HTML parser
/// dependency, matching this module's existing preference for direct
/// string manipulation over pulling in another crate for something this
/// narrow.
///
/// The caption comes from the matching `MediaItem.caption`, not from the
/// freshly-parsed HTML's own `title` attribute (pulldown-cmark's default
/// rendering of `![Bildunterschrift](src "Alternativtext")`, this app's
/// own bracket/title convention - see `media::markdown_image_text_for`'s
/// doc comment) - `MediaItem` is what both Medienverwaltung and the
/// editor's "Bildbeschriftung bearbeiten…" dialog write a caption edit
/// into first, immediately, while the dialog's own write-back into the
/// Markdown body itself only lands once its dialog closes (see
/// `imagealt::apply_image_text_to_buffer`), so reading the raw HTML
/// attribute here would leave a caption edited in either place invisible
/// in the preview until well after the edit was actually made.
fn wrap_images_with_badges(html: &str, media: &[MediaItem]) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find("<img ") {
        out.push_str(&rest[..start]);
        let Some(tag_end_rel) = rest[start..].find('>') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let tag_end = start + tag_end_rel + 1;
        let tag = &rest[start..tag_end];
        let src = extract_attr(tag, "src").map(|s| unescape_html_attr(&s)).unwrap_or_default();
        let item = media.iter().find(|item| item.source == src);

        out.push_str("<span class=\"img-wrap\">");
        out.push_str(tag);
        out.push_str(&badges_html(&src, media));
        out.push_str("</span>");
        if let Some(caption) = item.and_then(|item| item.caption.as_deref()).filter(|c| !c.is_empty()) {
            out.push_str("<span class=\"img-caption\">");
            out.push_str(&glib::markup_escape_text(caption));
            out.push_str("</span>");
        }
        rest = &rest[tag_end..];
    }
    out.push_str(rest);
    out
}

/// Pulls `attr="value"` out of a single HTML tag's source text - pulldown-
/// cmark always quotes attribute values with `"`, so this doesn't need to
/// handle the unquoted/single-quoted forms a general HTML parser would.
fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = tag.find(&needle)? + needle.len();
    let end = start + tag[start..].find('"')?;
    Some(tag[start..end].to_string())
}

/// Undoes pulldown-cmark's HTML-entity escaping of the `src` attribute, so
/// it can be compared against a plain `MediaItem.source` string again.
fn unescape_html_attr(value: &str) -> String {
    value.replace("&amp;", "&").replace("&quot;", "\"").replace("&lt;", "<").replace("&gt;", ">")
}

/// The badge cluster for one image, in the fixed order Upload-Status → Alt
/// → Bildformat whenever more than one applies - empty (no wrapper span at
/// all) if none of the three apply, e.g. an image with no matching
/// `MediaItem` or an unrecognized/missing file extension.
fn badges_html(src: &str, media: &[MediaItem]) -> String {
    let Some(item) = media.iter().find(|item| item.source == src) else {
        return String::new();
    };

    let mut badges: Vec<(String, String)> = Vec::new();
    if item.wordpress.is_some() {
        badges.push(("↑".to_string(), tr("Bereits zu WordPress hochgeladen")));
    }
    // The tooltip shows the actual alt text, not a generic sentence - a
    // quick way to check *what* was set, without opening Medienverwaltung,
    // and a clear visual proof (different text per image) that this badge
    // really is per-image data, not a shared/static label.
    match &item.alt {
        media::AltText::Text(text) => badges.push((tr("Alt"), text.clone())),
        media::AltText::Empty => badges.push((tr("Alt"), tr("Bewusst ohne Alternativtext (dekoratives Bild)"))),
        media::AltText::Undefined => {}
    }
    if let Some(format) = image_format_label(&item.filename) {
        badges.push((format, tr("Bildformat")));
    }
    if badges.is_empty() {
        return String::new();
    }

    let mut html = String::from("<span class=\"img-badges\">");
    for (label, title) in badges {
        html.push_str(&format!(
            "<span class=\"img-badge\" title=\"{}\">{}</span>",
            glib::markup_escape_text(&title),
            glib::markup_escape_text(&label)
        ));
    }
    html.push_str("</span>");
    html
}

/// The uppercased file extension (`"cat.png"` → `"PNG"`), or `None` for a
/// filename with no extension to show at all.
fn image_format_label(filename: &str) -> Option<String> {
    if !filename.contains('.') {
        return None;
    }
    filename.rsplit('.').next().map(str::to_uppercase)
}

/// Fenced/indented code blocks can span dozens of lines - if the whole
/// block were a single scroll-sync anchor (like every other block type
/// gets), the preview would sit completely frozen while the editor
/// scrolls through it, only jumping once you scroll past the block
/// entirely. Each *line* of the block's content gets its own anchor
/// instead (nested `<span data-line="N">` inside the shared `<pre><code>`),
/// so `window.scrollToLine`'s "closest preceding data-line" search - which
/// already walks every `[data-line]` element in document order, not just
/// top-level blocks - keeps working smoothly inside long code samples too.
fn render_code_block_with_line_anchors(events: &[(Event, std::ops::Range<usize>)], kind: &CodeBlockKind, block_start_line: usize) -> String {
    let mut code = String::new();
    for (event, _) in events {
        if let Event::Text(text) = event {
            code.push_str(text);
        }
    }

    if let CodeBlockKind::Fenced(info) = kind {
        if let Some(lang) = info.split_whitespace().next().filter(|lang| !lang.is_empty()) {
            if let Some(html) = render_special_fenced_block(lang, &code) {
                return html;
            }
        }
    }

    let lang_class = match kind {
        CodeBlockKind::Fenced(info) => info
            .split_whitespace()
            .next()
            .filter(|lang| !lang.is_empty())
            .map(|lang| format!(" class=\"language-{}\"", glib::markup_escape_text(lang))),
        CodeBlockKind::Indented => None,
    };

    let mut html = format!("<pre><code{}>", lang_class.unwrap_or_default());
    // pulldown-cmark's code content always ends with a trailing newline;
    // drop the empty element `split('\n')` would otherwise produce for it.
    for (index, line_text) in code.strip_suffix('\n').unwrap_or(&code).split('\n').enumerate() {
        let line = block_start_line + 1 + index;
        html.push_str(&format!("<span data-line=\"{line}\">{}</span>\n", glib::markup_escape_text(line_text)));
    }
    html.push_str("</code></pre>");
    html
}

/// This app's own five fenced-code "languages" (`gallery`/`columns`/
/// `buttons`/`pullquote`/`details`) - CommonMark has no native syntax for
/// side-by-side columns, a button row, a photo gallery, a pulled quote or a
/// collapsible disclosure, so `crates/gutenberg` uses a fenced block for
/// each instead (see that crate's own `Block` doc comments). Reuses that
/// crate's real parser and renderer directly, so the preview shows the
/// actual intended Gutenberg block - real image grid, real columns, a
/// styled button, a big pulled quote, a native `<details>` - instead of
/// falling back to a generic fenced-code-block dump of the raw fence text,
/// `+++` separator and all, which is what happens to a language this
/// doesn't recognize either. `None` for anything else, so the caller falls
/// back to that generic `<pre><code>` path unchanged.
fn render_special_fenced_block(lang: &str, text: &str) -> Option<String> {
    let block = match lang {
        "columns" => gutenberg::parse_fenced_columns(text),
        "buttons" => gutenberg::parse_fenced_buttons(text),
        "gallery" => gutenberg::parse_fenced_gallery(text),
        "pullquote" => gutenberg::parse_fenced_pullquote(text),
        "details" => gutenberg::parse_fenced_details(text),
        _ => return None,
    };
    Some(gutenberg::render_block(&block))
}

fn line_number(markdown: &str, byte_offset: usize) -> usize {
    markdown[..byte_offset].matches('\n').count() + 1
}

/// Same `Tag::to_end()` depth-counting trick as `crates/gutenberg` uses -
/// duplicated rather than shared, since this module's concern (HTML with
/// line anchors for scroll-sync) is unrelated to that crate's (Gutenberg
/// block-comment conversion).
fn find_matching_end(events: &[(Event, std::ops::Range<usize>)], start: usize, end_marker: &TagEnd) -> usize {
    let mut depth = 0usize;
    let mut j = start;
    while j < events.len() {
        match &events[j].0 {
            Event::Start(t) if &t.to_end() == end_marker => depth += 1,
            Event::End(e) if e == end_marker => {
                depth -= 1;
                if depth == 0 {
                    return j;
                }
            }
            _ => {}
        }
        j += 1;
    }
    events.len().saturating_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_paragraph_is_tagged_with_its_line() {
        let out = render_body_with_line_anchors("Hello world.\n", &[]);
        assert_eq!(out, "<div data-line=\"1\"><p>Hello world.</p>\n</div>\n");
    }

    #[test]
    fn blocks_separated_by_blank_lines_get_their_own_starting_line() {
        let markdown = "# Title\n\nSecond paragraph.\n\nThird paragraph.\n";
        let out = render_body_with_line_anchors(markdown, &[]);
        assert_eq!(
            out,
            "<div data-line=\"1\"><h1>Title</h1>\n</div>\n\
             <div data-line=\"3\"><p>Second paragraph.</p>\n</div>\n\
             <div data-line=\"5\"><p>Third paragraph.</p>\n</div>\n"
        );
    }

    #[test]
    fn image_only_line_is_tagged_with_its_single_source_line_despite_render_height() {
        // The whole point of anchoring by source line rather than by
        // proportion of total lines: this image is one line of Markdown,
        // but renders far taller than a text line - scroll-sync must still
        // key off "line 3", not some fraction of the document's line count.
        let markdown = "Intro text.\n\n![a cat](cat.png)\n\nOutro text.\n";
        let out = render_body_with_line_anchors(markdown, &[]);
        assert!(out.contains("<div data-line=\"1\"><p>Intro text.</p>"));
        assert!(out.contains("<div data-line=\"3\"><p><span class=\"img-wrap\"><img src=\"cat.png\" alt=\"a cat\""), "{out}");
        assert!(out.contains("<div data-line=\"5\"><p>Outro text.</p>"));
    }

    #[test]
    fn a_multi_paragraph_raw_html_group_keeps_its_div_open_across_blank_lines() {
        // A real `wp:group` (`crates/gutenberg` doesn't recognize the block,
        // so its whole comment+markup survives as raw HTML - see
        // `inject_group_flex_styles`'s own doc comment) with more than one
        // paragraph inside it: each paragraph is its own blank-line-separated
        // "block" as far as the Markdown parser is concerned, splitting the
        // group's opening `<div>` from its closing `</div>` across multiple
        // top-level events here. Wrapping the opening fragment in its own
        // `<div data-line>` used to prematurely close the real div (a bare
        // `</div>` always closes the innermost currently-open div) and cut
        // every later paragraph out of it - exactly the bug that made
        // `wp:group`'s flex/grid layout never actually apply to more than
        // one child in the live preview.
        let markdown = "<!-- wp:group {\"layout\":{\"type\":\"flex\"}} -->\n<div class=\"wp-block-group\"><!-- wp:paragraph -->\n<p>First</p>\n<!-- /wp:paragraph -->\n\n<!-- wp:paragraph -->\n<p>Second</p>\n<!-- /wp:paragraph --></div>\n<!-- /wp:group -->\n";
        let out = render_body_with_line_anchors(markdown, &[]);
        let group_open = out.find("<div class=\"wp-block-group\"").expect("group div");
        let group_close = out.find("</div>\n<!-- /wp:group -->").expect("group's own closing div, right before the comment");
        let between = &out[group_open..group_close];
        assert!(between.contains("First"), "{out}");
        assert!(between.contains("Second"), "{out}");
        // Exactly the pair opened above and its match - no stray wrapper
        // `</div>` landing inside the span and closing it early.
        assert_eq!(between.matches("<div").count(), between.matches("</div>").count() + 1, "{out}");
    }

    #[test]
    fn a_lone_youtube_url_line_becomes_an_embed_placeholder() {
        let markdown = "Intro text.\n\nhttps://www.youtube.com/watch?v=dQw4w9WgXcQ\n\nOutro text.\n";
        let out = render_body_with_line_anchors(markdown, &[]);
        assert!(out.contains("<div data-line=\"3\"><div class=\"embed-placeholder\">"), "{out}");
        assert!(out.contains("YouTube-Video"), "{out}");
        assert!(out.contains("https://www.youtube.com/watch?v=dQw4w9WgXcQ"), "{out}");
        // No live iframe/script is ever loaded for this - just a static card.
        assert!(!out.contains("<iframe"), "{out}");
    }

    #[test]
    fn a_lone_vimeo_url_line_becomes_an_embed_placeholder() {
        let out = render_body_with_line_anchors("https://vimeo.com/123456\n", &[]);
        assert!(out.contains("Vimeo-Video"), "{out}");
    }

    #[test]
    fn a_lone_url_from_an_unknown_provider_gets_a_generic_placeholder() {
        let out = render_body_with_line_anchors("https://example.com/some-article\n", &[]);
        assert!(out.contains("Eingebetteter Inhalt"), "{out}");
    }

    #[test]
    fn a_normal_link_inside_a_sentence_is_not_treated_as_an_embed() {
        let out = render_body_with_line_anchors("Check out [this video](https://www.youtube.com/watch?v=x) sometime.\n", &[]);
        assert!(!out.contains("embed-placeholder"), "{out}");
        assert!(out.contains("<a href=\"https://www.youtube.com/watch?v=x\">"), "{out}");
    }

    #[test]
    fn full_html_embeds_the_reverse_scroll_sync_script() {
        let html = render_html("Hello", PreviewStyle::Modern, false, &[], 0.0, &Frontmatter::default(), false, None);
        assert!(html.contains("window.currentTopLine = function()"));
        assert!(html.contains("messageHandlers.scrollSync"));
        assert!(html.contains("__suppressScrollEcho"));
    }

    #[test]
    fn full_html_embeds_the_scroll_to_edge_script_and_its_sentinels() {
        let html = render_html("Hello", PreviewStyle::Modern, false, &[], 0.0, &Frontmatter::default(), false, None);
        assert!(html.contains("window.scrollToEdge = function(edge)"), "{html}");
        assert!(html.contains("document.body.scrollHeight"), "{html}");
        assert!(html.contains("atBottom ? -2"), "{html}");
    }

    #[test]
    fn scroll_to_line_and_scroll_to_edge_both_glide_smoothly() {
        let html = render_html("Hello", PreviewStyle::Modern, false, &[], 0.0, &Frontmatter::default(), false, None);
        assert!(html.contains("behavior: 'smooth'"), "{html}");
        assert!(html.contains("scrollend"), "{html}");
    }

    #[test]
    fn code_block_css_is_empty_without_a_resolvable_scheme() {
        assert_eq!(code_block_css(None), "");
    }

    #[test]
    fn code_block_css_overrides_pre_and_code_colors() {
        let css = code_block_css(Some(("#282a36", "#f8f8f2")));
        assert!(css.contains("pre { background: #282a36; color: #f8f8f2; }"), "{css}");
        assert!(css.contains("code { background: #282a36; color: #f8f8f2; }"), "{css}");
    }

    #[test]
    fn full_html_prefers_the_active_scheme_colors_over_the_style_defaults() {
        let html = render_html("Hello", PreviewStyle::Modern, true, &[], 0.0, &Frontmatter::default(), false, Some(("#282a36".to_string(), "#f8f8f2".to_string())));
        // The style's own hardcoded dark-mode `pre` background (see
        // `style_css`) must lose the cascade to the scheme's, which is
        // appended after it - not just be present somewhere in the page.
        let scheme_rule_pos = html.find("pre { background: #282a36;").expect("scheme override present");
        let style_rule_pos = html.find("pre { background: #2d2d2d;").expect("style default present");
        assert!(scheme_rule_pos > style_rule_pos, "{html}");
    }

    #[test]
    fn full_html_keeps_the_style_default_code_colors_without_a_scheme() {
        let html = render_html("Hello", PreviewStyle::Modern, true, &[], 0.0, &Frontmatter::default(), false, None);
        assert!(html.contains("pre { background: #2d2d2d;"), "{html}");
        assert!(!html.contains("pre { background: none"), "{html}");
    }

    fn media_item(source: &str, filename: &str, alt: crate::media::AltText, uploaded: bool) -> MediaItem {
        MediaItem {
            id: "media-001".to_string(),
            filename: filename.to_string(),
            source: source.to_string(),
            alt,
            caption: None,
            wordpress: uploaded.then_some(crate::media::WordPressMediaRef {
                media_id: 1,
                url: "https://example.com/cat.png".to_string(),
                content_hash: "abc".to_string(),
                width: 0,
                height: 0,
            }),
            last_markdown_caption: None,
        }
    }

    #[test]
    fn an_image_with_no_matching_media_item_gets_no_badges() {
        let markdown = "![a cat](cat.png)\n";
        let out = render_body_with_line_anchors(markdown, &[]);
        assert!(!out.contains("img-badges"), "{out}");
    }

    #[test]
    fn badges_appear_in_upload_alt_format_order_when_all_three_apply() {
        let item = media_item("cat.png", "cat.png", crate::media::AltText::Text("a cat".into()), true);
        let out = render_body_with_line_anchors("![a cat](cat.png)\n", std::slice::from_ref(&item));
        let badges_start = out.find("img-badges").expect("expected a badge cluster");
        let upload_pos = out.find('↑').expect("expected the upload badge");
        let alt_pos = out.find(">Alt<").expect("expected the alt badge");
        let format_pos = out.find(">PNG<").expect("expected the format badge");
        assert!(badges_start < upload_pos && upload_pos < alt_pos && alt_pos < format_pos, "{out}");
    }

    #[test]
    fn only_the_applicable_badges_are_shown() {
        let item = media_item("cat.png", "cat.png", crate::media::AltText::Undefined, false);
        let out = render_body_with_line_anchors("![a cat](cat.png)\n", std::slice::from_ref(&item));
        assert!(!out.contains('↑'), "{out}");
        assert!(!out.contains(">Alt<"), "{out}");
        assert!(out.contains(">PNG<"), "{out}");
    }

    #[test]
    fn deliberately_empty_alt_still_counts_as_defined() {
        let item = media_item("cat.png", "cat.png", crate::media::AltText::Empty, false);
        let out = render_body_with_line_anchors("![](cat.png)\n", std::slice::from_ref(&item));
        assert!(out.contains(">Alt<"), "{out}");
    }

    #[test]
    fn the_alt_badge_tooltip_is_the_actual_alt_text_not_a_generic_sentence() {
        let item = media_item("cat.png", "cat.png", crate::media::AltText::Text("eine rote Katze".into()), false);
        let out = render_body_with_line_anchors("![](cat.png)\n", std::slice::from_ref(&item));
        assert!(out.contains("title=\"eine rote Katze\""), "{out}");
    }

    #[test]
    fn a_caption_renders_as_a_visible_span_next_to_the_image() {
        let mut item = media_item("cat.png", "cat.png", crate::media::AltText::Undefined, false);
        item.caption = Some("Unsere Katze".to_string());
        let out = render_body_with_line_anchors("![](cat.png)\n", std::slice::from_ref(&item));
        assert!(out.contains("<span class=\"img-caption\">Unsere Katze</span>"), "{out}");
    }

    #[test]
    fn no_caption_span_when_none_is_set() {
        let item = media_item("cat.png", "cat.png", crate::media::AltText::Undefined, false);
        let out = render_body_with_line_anchors("![](cat.png)\n", std::slice::from_ref(&item));
        assert!(!out.contains("img-caption"), "{out}");
    }

    #[test]
    fn image_format_label_uppercases_the_extension() {
        assert_eq!(image_format_label("photo.webp"), Some("WEBP".to_string()));
        assert_eq!(image_format_label("photo.PNG"), Some("PNG".to_string()));
        assert_eq!(image_format_label("no-extension"), None);
    }

    #[test]
    fn media_tag_for_recognizes_video_and_audio_extensions() {
        assert_eq!(media_tag_for("clip.mp4"), Some("video"));
        assert_eq!(media_tag_for("clip.MOV"), Some("video"));
        assert_eq!(media_tag_for("song.mp3"), Some("audio"));
        assert_eq!(media_tag_for("song.mp3?ver=2"), Some("audio"));
        assert_eq!(media_tag_for("photo.png"), None);
    }

    #[test]
    fn rewrite_media_tags_turns_a_video_img_into_a_real_video_tag() {
        let html = "<p><img src=\"clip.mp4\" alt=\"\" /></p>";
        assert_eq!(rewrite_media_tags(html), "<p><video controls src=\"clip.mp4\"></video></p>");
    }

    #[test]
    fn rewrite_media_tags_leaves_ordinary_images_untouched() {
        let html = "<p><img src=\"photo.png\" alt=\"\" /></p>";
        assert_eq!(rewrite_media_tags(html), html);
    }

    #[test]
    fn thematic_break_is_tagged() {
        let out = render_body_with_line_anchors("Text.\n\n---\n\nMore text.\n", &[]);
        assert!(out.contains("<div data-line=\"3\"><hr/></div>"));
    }

    #[test]
    fn multiline_code_block_tags_each_line_individually() {
        // The whole point: a long fenced code block used to be one opaque
        // scroll-sync anchor, so scrolling through it in the editor never
        // moved the preview at all until you scrolled past the block
        // entirely. Each line inside it needs its own `data-line` now.
        let markdown = "Intro.\n\n```bash\nfirst\nsecond\nthird\n```\n\nOutro.\n";
        let out = render_body_with_line_anchors(markdown, &[]);
        assert!(out.contains("<div data-line=\"3\">"), "{out}");
        assert!(out.contains("<span data-line=\"4\">first</span>"), "{out}");
        assert!(out.contains("<span data-line=\"5\">second</span>"), "{out}");
        assert!(out.contains("<span data-line=\"6\">third</span>"), "{out}");
        assert!(out.contains("<div data-line=\"9\">"), "{out}");
    }

    #[test]
    fn fenced_code_block_language_becomes_a_css_class() {
        let out = render_body_with_line_anchors("```rust\nfn main() {}\n```\n", &[]);
        assert!(out.contains("class=\"language-rust\""), "{out}");
    }

    #[test]
    fn code_block_content_is_html_escaped() {
        let out = render_body_with_line_anchors("```\n<script>alert(1)</script>\n```\n", &[]);
        assert!(out.contains("&lt;script&gt;"), "{out}");
        assert!(!out.contains("<script>"), "{out}");
    }

    #[test]
    fn pullquote_fence_renders_as_a_real_pullquote_not_raw_fence_text() {
        let out = render_body_with_line_anchors("```pullquote\nA striking quote.\n+++\nJane Doe\n```\n", &[]);
        assert!(out.contains("<figure class=\"wp-block-pullquote\">"), "{out}");
        assert!(out.contains("<blockquote>"), "{out}");
        assert!(out.contains("<cite>Jane Doe</cite>"), "{out}");
        assert!(!out.contains("+++"), "{out}");
    }

    #[test]
    fn gallery_fence_renders_as_a_real_gallery() {
        let out = render_body_with_line_anchors("```gallery\n![First](one.jpg)\n![Second](two.jpg)\n```\n", &[]);
        assert!(out.contains("class=\"wp-block-gallery"), "{out}");
        assert!(out.contains("src=\"one.jpg\""), "{out}");
        assert!(out.contains("src=\"two.jpg\""), "{out}");
    }

    #[test]
    fn columns_fence_renders_as_real_columns() {
        let out = render_body_with_line_anchors("```columns\nLeft side.\n+++\nRight side.\n```\n", &[]);
        assert!(out.contains("class=\"wp-block-columns\">"), "{out}");
        assert!(out.contains("class=\"wp-block-column\">"), "{out}");
        assert!(out.contains("Left side."), "{out}");
        assert!(out.contains("Right side."), "{out}");
    }

    #[test]
    fn buttons_fence_renders_as_real_buttons() {
        let out = render_body_with_line_anchors("```buttons\n[Los geht's](https://example.com/)\n```\n", &[]);
        assert!(out.contains("class=\"wp-block-buttons\">"), "{out}");
        assert!(out.contains("wp-block-button__link"), "{out}");
        assert!(out.contains("href=\"https://example.com/\""), "{out}");
    }

    #[test]
    fn details_fence_renders_as_a_native_details_element() {
        let out = render_body_with_line_anchors("```details\nMehr anzeigen\n+++\nThe hidden body.\n```\n", &[]);
        assert!(out.contains("<details class=\"wp-block-details\">"), "{out}");
        assert!(out.contains("<summary>Mehr anzeigen</summary>"), "{out}");
        assert!(out.contains("The hidden body."), "{out}");
    }

    #[test]
    fn render_special_fenced_block_is_none_for_an_unrelated_language() {
        assert_eq!(render_special_fenced_block("bash", "echo hi"), None);
    }

    #[test]
    fn flex_style_for_group_attrs_handles_row_wrap_and_justify() {
        let style = flex_style_for_group_attrs(r#"{"type":"flex","flexWrap":"nowrap","justifyContent":"space-between"}"#).unwrap();
        assert!(style.contains("display:flex;"), "{style}");
        assert!(style.contains("flex-direction:row;"), "{style}");
        assert!(style.contains("flex-wrap:nowrap;"), "{style}");
        assert!(style.contains("justify-content:space-between;"), "{style}");
    }

    #[test]
    fn flex_style_for_group_attrs_handles_vertical_orientation() {
        let style = flex_style_for_group_attrs(r#"{"type":"flex","orientation":"vertical"}"#).unwrap();
        assert!(style.contains("flex-direction:column;"), "{style}");
        assert!(style.contains("flex-wrap:wrap;"), "{style}"); // default, no flexWrap given
    }

    #[test]
    fn flex_style_for_group_attrs_maps_left_and_right_to_valid_css_keywords() {
        assert!(flex_style_for_group_attrs(r#"{"type":"flex","justifyContent":"left"}"#).unwrap().contains("justify-content:flex-start;"));
        assert!(flex_style_for_group_attrs(r#"{"type":"flex","justifyContent":"right"}"#).unwrap().contains("justify-content:flex-end;"));
    }

    #[test]
    fn flex_style_for_group_attrs_is_none_for_a_constrained_layout() {
        assert_eq!(flex_style_for_group_attrs(r#"{"type":"constrained"}"#), None);
        assert_eq!(flex_style_for_group_attrs(""), None);
    }

    #[test]
    fn inject_group_flex_styles_writes_a_fresh_style_attribute() {
        let html = "<!-- wp:group {\"layout\":{\"type\":\"flex\"}} --><div class=\"wp-block-group\">content</div><!-- /wp:group -->";
        let out = inject_group_flex_styles(html);
        assert!(out.contains("<div class=\"wp-block-group\" style=\"display:flex;"), "{out}");
    }

    #[test]
    fn inject_group_flex_styles_appends_inside_an_existing_style_attribute() {
        let html = "<!-- wp:group {\"layout\":{\"type\":\"flex\"}} --><div class=\"wp-block-group\" style=\"border-width:1px;\">content</div><!-- /wp:group -->";
        let out = inject_group_flex_styles(html);
        assert!(out.contains("style=\"border-width:1px;display:flex;"), "{out}");
        // Exactly one `style=` attribute, not two.
        assert_eq!(out.matches("style=\"").count(), 1, "{out}");
    }

    #[test]
    fn inject_group_flex_styles_leaves_a_non_flex_group_untouched() {
        let html = "<!-- wp:group {\"layout\":{\"type\":\"constrained\"}} --><div class=\"wp-block-group\">content</div><!-- /wp:group -->";
        assert_eq!(inject_group_flex_styles(html), html);
    }

    #[test]
    fn extract_json_string_value_reads_a_known_key() {
        assert_eq!(extract_json_string_value(r#"{"type":"flex","justifyContent":"center"}"#, "justifyContent").as_deref(), Some("center"));
        assert_eq!(extract_json_string_value(r#"{"type":"flex"}"#, "justifyContent"), None);
    }

    #[test]
    fn full_html_embeds_the_scroll_to_line_script() {
        let html = render_html("Hello", PreviewStyle::Modern, false, &[], 0.0, &Frontmatter::default(), false, None);
        assert!(html.contains("window.scrollToLine = function(line)"));
        assert!(html.contains("data-line=\"1\""));
    }

    #[test]
    fn full_html_restores_a_positive_scroll_position() {
        let html = render_html("Hello", PreviewStyle::Modern, false, &[], 240.0, &Frontmatter::default(), false, None);
        assert!(html.contains("window.scrollTo(0, 240)"), "{html}");
    }

    #[test]
    fn full_html_guards_the_scroll_restore_for_zero() {
        let html = render_html("Hello", PreviewStyle::Modern, false, &[], 0.0, &Frontmatter::default(), false, None);
        assert!(html.contains("if (0 > 0) { window.__suppressScrollEcho = true; window.scrollTo(0, 0);"), "{html}");
    }

    #[test]
    fn preview_style_id_round_trips() {
        for style in PreviewStyle::ALL {
            assert_eq!(PreviewStyle::from_id(style.id()), style);
        }
    }

    #[test]
    fn item_index_for_image_uri_matches_a_local_file_uri_back_to_its_source() {
        let dir = std::env::temp_dir().join(format!("blocksmith-preview-uri-match-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cat.png"), b"fake png bytes").unwrap();

        let items = vec![MediaItem {
            id: "media-001".to_string(),
            filename: "cat.png".to_string(),
            source: "cat.png".to_string(),
            alt: crate::media::AltText::Undefined,
            caption: None,
            wordpress: None,
            last_markdown_caption: None,
        }];
        let uri = gio::File::for_path(dir.join("cat.png")).uri();

        assert_eq!(item_index_for_image_uri(&items, &uri, Some(&dir)), Some(0));
        assert_eq!(item_index_for_image_uri(&items, &uri, None), None, "no doc_dir means the source can't resolve to this path");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn item_index_for_image_uri_matches_a_remote_url_directly() {
        let items = vec![MediaItem {
            id: "media-001".to_string(),
            filename: "cat.png".to_string(),
            source: "https://example.com/cat.png".to_string(),
            alt: crate::media::AltText::Undefined,
            caption: None,
            wordpress: None,
            last_markdown_caption: None,
        }];
        assert_eq!(item_index_for_image_uri(&items, "https://example.com/cat.png", None), Some(0));
        assert_eq!(item_index_for_image_uri(&items, "https://example.com/dog.png", None), None);
    }

    #[test]
    fn every_style_has_both_a_light_and_dark_variant() {
        for style in PreviewStyle::ALL {
            assert!(!style_css(style, false).is_empty());
            assert!(!style_css(style, true).is_empty());
            assert_ne!(style_css(style, false), style_css(style, true));
        }
    }

    #[test]
    fn render_header_is_empty_for_a_document_with_no_title() {
        assert_eq!(render_header(&Frontmatter::default()), "");
        let untitled = Frontmatter { title: "   ".to_string(), ..Frontmatter::default() };
        assert_eq!(render_header(&untitled), "", "a whitespace-only title is still untitled");
    }

    #[test]
    fn render_header_includes_the_title_excerpt_categories_and_tags() {
        let frontmatter = Frontmatter {
            title: "Ein Testartikel".to_string(),
            excerpt: Some("Ein kurzer Auszug.".to_string()),
            categories: vec!["GNU/Linux".to_string()],
            tags: vec!["gnome".to_string(), "rust".to_string()],
            ..Frontmatter::default()
        };
        let html = render_header(&frontmatter);
        assert!(html.contains("<h1 class=\"article-header-title\">Ein Testartikel</h1>"), "{html}");
        assert!(html.contains("Ein kurzer Auszug."), "{html}");
        assert!(html.contains(">GNU/Linux<"), "{html}");
        assert!(html.contains(">#gnome<"), "{html}");
        assert!(html.contains(">#rust<"), "{html}");
    }

    #[test]
    fn render_header_puts_categories_and_tags_in_one_shared_row() {
        let frontmatter = Frontmatter {
            title: "Ein Testartikel".to_string(),
            categories: vec!["GNU/Linux".to_string()],
            tags: vec!["gnome".to_string()],
            ..Frontmatter::default()
        };
        let html = render_header(&frontmatter);
        let taxonomy_rows = html.matches("class=\"article-header-taxonomy\"").count();
        assert_eq!(taxonomy_rows, 1, "categories and tags must share one row, not two separate ones: {html}");
        assert!(html.contains("<div class=\"article-header-categories\">"), "{html}");
        assert!(html.contains("<div class=\"article-header-tags\">"), "{html}");
    }

    #[test]
    fn render_header_escapes_html_in_every_field() {
        let frontmatter = Frontmatter {
            title: "<script>alert(1)</script>".to_string(),
            excerpt: Some("<b>fett</b>".to_string()),
            categories: vec!["<i>Kategorie</i>".to_string()],
            ..Frontmatter::default()
        };
        let html = render_header(&frontmatter);
        assert!(!html.contains("<script>"), "{html}");
        assert!(!html.contains("<b>fett</b>"), "{html}");
        assert!(!html.contains("<i>Kategorie</i>"), "{html}");
    }

    #[test]
    fn render_header_omits_the_featured_image_when_none_is_set() {
        let frontmatter = Frontmatter { title: "Ein Testartikel".to_string(), ..Frontmatter::default() };
        assert!(!render_header(&frontmatter).contains("article-header-image"));
    }

    #[test]
    fn render_header_includes_the_featured_image_when_set() {
        let frontmatter = Frontmatter {
            title: "Ein Testartikel".to_string(),
            featured_image: Some("aufmacher.webp".to_string()),
            featured_image_alt: Some("Ein Aufmacherbild".to_string()),
            ..Frontmatter::default()
        };
        let html = render_header(&frontmatter);
        assert!(html.contains("class=\"article-header-image\" src=\"aufmacher.webp\""), "{html}");
        assert!(html.contains("alt=\"Ein Aufmacherbild\""), "{html}");
    }

    #[test]
    fn render_header_shows_the_status_label() {
        let frontmatter = Frontmatter { title: "Ein Testartikel".to_string(), status: document::PostStatus::Draft, ..Frontmatter::default() };
        assert!(render_header(&frontmatter).contains(&document::PostStatus::Draft.label()));
    }

    #[test]
    fn build_article_url_preview_joins_the_domain_and_slug() {
        assert_eq!(build_article_url_preview("https://linuxundich.de", "mein-artikel"), Some("https://linuxundich.de/mein-artikel/".to_string()));
    }

    #[test]
    fn build_article_url_preview_strips_a_trailing_slash_from_the_domain() {
        assert_eq!(build_article_url_preview("https://linuxundich.de/", "mein-artikel"), Some("https://linuxundich.de/mein-artikel/".to_string()));
    }

    #[test]
    fn build_article_url_preview_is_none_without_a_domain_or_slug() {
        assert_eq!(build_article_url_preview("", "mein-artikel"), None);
        assert_eq!(build_article_url_preview("https://linuxundich.de", ""), None);
    }

    #[test]
    fn show_header_false_omits_the_header_markup_from_the_full_page() {
        // Both renders include `HEADER_CSS` (the `.article-header-title`
        // selector) in their `<style>` block regardless of `show_header` -
        // only the `<header>` tag itself is conditional - so the assertion
        // below checks for the actual rendered element, not the class name
        // alone, which would find a false positive in the CSS either way.
        let frontmatter = Frontmatter { title: "Ein Testartikel".to_string(), ..Frontmatter::default() };
        let shown = render_html("Hello", PreviewStyle::Modern, false, &[], 0.0, &frontmatter, true, None);
        let hidden = render_html("Hello", PreviewStyle::Modern, false, &[], 0.0, &frontmatter, false, None);
        assert!(shown.contains("<h1 class=\"article-header-title\">"), "{shown}");
        assert!(!hidden.contains("<h1 class=\"article-header-title\">"), "{hidden}");
    }
}
