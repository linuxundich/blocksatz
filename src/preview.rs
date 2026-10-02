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
//! `connect_scroll`/`window.__currentSyncState` - see `window.rs::wire_scroll_sync`
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
use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};
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
    dir.push(crate::APP_DIR);
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
                    ScrollRestore::Top,
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
    /// every body edit, which would otherwise snap the preview back to the
    /// top on every pause in typing. Restored by content, not pixels (see
    /// `rerender_preserving_scroll`). Opening a *different* document still
    /// lands at the top as expected - every document-loading call site
    /// already calls `set_doc_dir` right after `buffer.set_text`, whose own
    /// plain `rerender()` (always top) runs synchronously before this
    /// debounced update fires, so there's nothing meaningful left to
    /// preserve by the time it reads the current position.
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

    /// Scrolls the preview to the editor's position: `line` is the
    /// fractional 1-based source line at the top of the editor's viewport,
    /// `top_t`/`bottom_t` how far (0..1) the editor is into its first/last
    /// screenful, and `total_lines` the buffer's line count (the anchor for
    /// interpolating past the last block). See `render_html`'s `syncTo`.
    pub fn sync_to(&self, line: f64, top_t: f64, bottom_t: f64, total_lines: i32) {
        let script = format!("window.syncTo && window.syncTo({line:.4}, {top_t:.4}, {bottom_t:.4}, {total_lines});");
        self.web_view.evaluate_javascript(&script, None, None, gio::Cancellable::NONE, |_| {});
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

    /// Wires the reverse leg of scroll-sync: `callback` fires with the same
    /// `(line, top_t, bottom_t)` triple `sync_to` takes, whenever the user
    /// scrolls the preview itself - never for a scroll `sync_to` or a
    /// restore performed (the page's script recognizes its own scrolls, see
    /// `render_html`'s `__programmaticY`). Reported at most once per frame.
    /// Every `WebView` has a `UserContentManager` of its own, so this only
    /// needs calling once, independent of how many times the page itself
    /// gets reloaded.
    pub fn connect_scroll(&self, callback: impl Fn(f64, f64, f64) + 'static) {
        let Some(manager) = self.web_view.user_content_manager() else { return };
        manager.register_script_message_handler(SCROLL_SYNC_HANDLER, None);
        manager.connect_script_message_received(Some(SCROLL_SYNC_HANDLER), move |_manager, value| {
            let payload = value.to_str();
            let parts: Vec<f64> = payload.split(';').filter_map(|part| part.trim().parse().ok()).collect();
            if let [line, top_t, bottom_t] = parts[..] {
                callback(line, top_t, bottom_t);
            }
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
            ScrollRestore::Top,
            &self.last_frontmatter.borrow(),
            self.show_header.get(),
            appearance::current_scheme_colors(),
        );
        self.web_view.load_html(&html, base_uri(self.doc_dir.borrow().as_deref()).as_deref());
    }

    /// Same full-page reload as `rerender`, but reads the page's current
    /// scroll-sync position first and bakes it into the freshly rendered
    /// HTML (`ScrollRestore::Sync`) so the reload lands back on the same
    /// content - `load_html` always resets scroll to the top on its own,
    /// and there's no "restore scroll after this specific load finishes"
    /// hook to use instead. Restoring by source line rather than by pixel
    /// offset is what keeps the view still while typing: text growing
    /// above, or images loading after the restore, would otherwise push
    /// the content away from a fixed pixel position.
    fn rerender_preserving_scroll(&self) {
        let style = self.style.get();
        let last_markdown = self.last_markdown.clone();
        let last_media = self.last_media.clone();
        let doc_dir = self.doc_dir.borrow().clone();
        let last_frontmatter = self.last_frontmatter.clone();
        let show_header = self.show_header.get();
        let web_view = self.web_view.clone();
        self.web_view.evaluate_javascript("JSON.stringify(window.__currentSyncState ? window.__currentSyncState() : null)", None, None, gio::Cancellable::NONE, move |result| {
            let restore = result.map(|value| ScrollRestore::from_state_json(&value.to_str())).unwrap_or(ScrollRestore::Top);
            let dark = adw::StyleManager::default().is_dark();
            let html = render_html(&last_markdown.borrow(), style, dark, &last_media.borrow(), restore, &last_frontmatter.borrow(), show_header, appearance::current_scheme_colors());
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

/// Where a freshly rendered page starts out - `load_html` always resets to
/// the top, so the position is baked into the page's own startup script.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScrollRestore {
    Top,
    /// A scroll-sync position (`window.syncTo`'s fractional source line
    /// plus its top/bottom blend) - restored by *content*, not by pixel
    /// offset, and re-applied as images load, so a re-render while typing
    /// doesn't shift what's on screen.
    Sync { line: f64, top_t: f64, bottom_t: f64 },
}

impl ScrollRestore {
    fn script(&self) -> String {
        match self {
            ScrollRestore::Top => String::new(),
            ScrollRestore::Sync { line, top_t, bottom_t } => {
                format!("window.__lastSync = [{line}, {top_t}, {bottom_t}]; window.__reapplySync();")
            }
        }
    }

    /// Parses `window.__currentSyncState()`'s `[line, topT, bottomT]`.
    fn from_state_json(json: &str) -> Self {
        match serde_json::from_str::<Vec<f64>>(json).ok().as_deref() {
            Some([line, top_t, bottom_t]) if line.is_finite() && top_t.is_finite() && bottom_t.is_finite() => ScrollRestore::Sync { line: *line, top_t: *top_t, bottom_t: *bottom_t },
            _ => ScrollRestore::Top,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn render_html(markdown: &str, style: PreviewStyle, dark: bool, media: &[MediaItem], restore: ScrollRestore, frontmatter: &Frontmatter, show_header: bool, code_colors: Option<(String, String)>) -> String {
    let restore_js = restore.script();
    // Footnotes written in Markdown show like the blog's (references and
    // the list block); an opened post's unconverted ones come from the
    // frontmatter.
    let (markdown, notes) = crate::export::with_footnotes(markdown);
    let footnotes = crate::export::footnotes_meta(&notes, frontmatter.wp_footnotes.as_deref());
    let body = render_body(&markdown, media, footnotes.as_deref());
    let header = if show_header { render_header(frontmatter) } else { String::new() };
    let css = style_css(style, dark);
    let code_css = code_block_css(code_colors.as_ref().map(|(background, foreground)| (background.as_str(), foreground.as_str())));
    // The blog theme's color/gradient/font-size presets, for attribute
    // lines like `{bg=accent}` (see `themestyle`).
    let theme_css = crate::themestyle::current().preview_css();
    let playlist_hint = tr("Wiedergabeliste – die Titel erzeugt das Blog").replace('"', "");
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
.wp-block-cover__background.has-background-dim {{ opacity: .5; }}
.wp-block-cover__background.has-background-dim-10 {{ opacity: .1; }} .wp-block-cover__background.has-background-dim-20 {{ opacity: .2; }} .wp-block-cover__background.has-background-dim-30 {{ opacity: .3; }}
.wp-block-cover__background.has-background-dim-40 {{ opacity: .4; }} .wp-block-cover__background.has-background-dim-50 {{ opacity: .5; }} .wp-block-cover__background.has-background-dim-60 {{ opacity: .6; }}
.wp-block-cover__background.has-background-dim-70 {{ opacity: .7; }} .wp-block-cover__background.has-background-dim-80 {{ opacity: .8; }} .wp-block-cover__background.has-background-dim-90 {{ opacity: .9; }}
.wp-block-cover__background.has-background-dim-100 {{ opacity: 1; }}
.wp-block-cover.has-custom-content-position.is-position-bottom-left {{ align-items: flex-end; justify-content: flex-start; }}
.wp-block-cover.has-custom-content-position.is-position-top-left {{ align-items: flex-start; justify-content: flex-start; }}
.wp-block-cover {{ color: #fff; padding: 1em; box-sizing: border-box; }}
.wp-block-cover > span.img-wrap {{ position: absolute; inset: 0; z-index: 0; display: block; }}
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
.wp-block-columns {{ display: flex; flex-wrap: nowrap; gap: 2rem; margin-bottom: 1.75em; }}
.wp-block-columns.are-vertically-aligned-center {{ align-items: center; }}
.wp-block-columns.are-vertically-aligned-top {{ align-items: flex-start; }}
.wp-block-columns.are-vertically-aligned-bottom {{ align-items: flex-end; }}
.wp-block-column {{ flex-grow: 1; flex-basis: 0; min-width: 0; }}
.wp-block-column[style*="flex-basis"] {{ flex-grow: 0; }}
.wp-block-column.is-vertically-aligned-center {{ align-self: center; }}
.wp-block-columns.has-background {{ padding: 1.25em 2.375em; }}
.wp-block-buttons {{ display: flex; flex-wrap: wrap; gap: .5em; margin: 1em 0; }}
/* WordPress's own default; the blog theme's button style (themestyle)
   follows below and wins. */
.wp-block-button__link {{ display: inline-block; cursor: pointer; text-align: center; text-decoration: none; background-color: #32373c; color: #fff; border-radius: 9999px; padding: calc(.667em + 2px) calc(1.333em + 2px); }}
.wp-block-button.is-style-outline > .wp-block-button__link {{ background: transparent; color: currentColor; border: 2px solid; }}
.wp-block-group {{ margin-bottom: var(--wp--style--block-gap, 1em); }}
.wp-block-group[style*="display:flex"] > *, .wp-block-group[style*="display:grid"] > *, .wp-block-buttons > * {{ margin-top: 0; margin-bottom: 0; }}
.wp-block-gallery.has-nested-images {{ display: flex; flex-wrap: wrap; gap: 1rem; align-items: normal; }}
.wp-block-gallery.has-nested-images figure.wp-block-image {{ margin: 0; flex-grow: 1; width: calc(33.33% - .67rem); box-sizing: border-box; display: flex; flex-direction: column; }}
.wp-block-gallery.has-nested-images.columns-1 figure.wp-block-image {{ width: 100%; }}
.wp-block-gallery.has-nested-images.columns-2 figure.wp-block-image {{ width: calc(50% - .5rem); }}
.wp-block-gallery.has-nested-images.columns-4 figure.wp-block-image {{ width: calc(25% - .75rem); }}
.wp-block-gallery.has-nested-images.columns-5 figure.wp-block-image {{ width: calc(20% - .8rem); }}
.wp-block-gallery.has-nested-images.columns-6 figure.wp-block-image {{ width: calc(16.66% - .84rem); }}
.wp-block-gallery.has-nested-images > figcaption {{ flex-basis: 100%; flex-grow: 1; text-align: center; }}
.wp-block-gallery.has-nested-images figure.wp-block-image img {{ width: 100%; height: 100%; object-fit: cover; display: block; }}
.wp-block-pullquote {{ text-align: center; margin: 2rem 0; padding: 1.5rem 0; border-top: 3px solid currentColor; border-bottom: 3px solid currentColor; }}
.wp-block-pullquote blockquote {{ margin: 0; font-size: 1.5rem; font-style: italic; }}
.wp-block-pullquote cite {{ display: block; margin-top: .75rem; font-size: 1rem; font-style: normal; }}
.wp-block-quote cite {{ display: block; margin-top: .5rem; font-size: .875em; font-style: normal; opacity: .8; }}
.wp-block-preformatted, .wp-block-verse {{ white-space: pre-wrap; font-family: inherit; margin: 1.5em 0; overflow-wrap: anywhere; }}
.wp-block-preformatted {{ font-family: monospace; }}
.wp-block-details summary {{ cursor: pointer; font-weight: 600; }}
/* Browsers indent a bare `<figure>` by 40px; WordPress's own CSS resets
   that for every block (audio, video, embed, file, ...). */
figure {{ margin: 0 0 1em; }}
.wp-block-media-text {{ display: grid; grid-template-columns: 50% 1fr; align-items: center; margin: 1.5em 0; box-sizing: border-box; }}
.wp-block-media-text.has-media-on-the-right {{ grid-template-columns: 1fr 50%; }}
.wp-block-media-text > .wp-block-media-text__media {{ grid-column: 1; grid-row: 1; margin: 0; align-self: stretch; }}
.wp-block-media-text > .wp-block-media-text__content {{ grid-column: 2; grid-row: 1; padding: 0 8%; }}
.wp-block-media-text.has-media-on-the-right > .wp-block-media-text__media {{ grid-column: 2; }}
.wp-block-media-text.has-media-on-the-right > .wp-block-media-text__content {{ grid-column: 1; }}
.wp-block-media-text__media img, .wp-block-media-text__media .img-wrap {{ width: 100%; display: block; }}
.wp-block-media-text.is-image-fill-element > .wp-block-media-text__media {{ position: relative; min-height: 250px; height: 100%; }}
.wp-block-media-text.is-image-fill-element > .wp-block-media-text__media .img-wrap {{ position: absolute; inset: 0; }}
.wp-block-media-text.is-image-fill-element > .wp-block-media-text__media img {{ width: 100%; height: 100%; object-fit: cover; }}
.wp-block-media-text.is-vertically-aligned-top {{ align-items: start; }}
.wp-block-media-text.is-vertically-aligned-bottom {{ align-items: end; }}
.wp-block-file {{ display: flex; flex-wrap: wrap; align-items: center; gap: .75em; margin: 0 0 1em; }}
.wp-block-file__button {{ display: inline-block; padding: .5em 1em; border-radius: 2em; background: #32373c; color: #fff !important; text-decoration: none; font-size: .8em; }}
:root {{ --wp--preset--shadow--natural: 6px 6px 9px rgba(0,0,0,.2); --wp--preset--shadow--deep: 12px 12px 50px rgba(0,0,0,.4); --wp--preset--shadow--sharp: 6px 6px 0 rgba(0,0,0,.2); --wp--preset--shadow--outlined: 6px 6px 0 -3px #fff, 6px 6px #000; --wp--preset--shadow--crisp: 6px 6px 0 #000; }}
:where(.has-border-color), :where([style*="border-width"]), :where([style*="border-top-width"]) {{ border-style: solid; }}
.wp-block-image.has-custom-border img, .wp-block-image img.has-border-color {{ box-sizing: border-box; }}
.wp-block-playlist {{ border: 1px dashed rgba(127,127,127,.4); border-radius: 8px; padding: 1em; text-align: center; }}
.wp-block-playlist::before {{ content: "{playlist_hint}"; display: block; font-weight: 600; opacity: .75; }}
table {{ border-collapse: collapse; }}
th, td {{ border: 1px solid #ccc; padding: .4rem .6rem; }}
/* What WordPress's own block CSS does for the attributes an attribute
   line (`bg=accent`, `style=stripes`, ...) can set - the colors themselves
   come from the blog theme (`theme_css` below). */
p.has-background, h1.has-background, h2.has-background, h3.has-background, h4.has-background, h5.has-background, h6.has-background {{ padding: 1.25em 2.375em; }}
.has-drop-cap:not(:focus)::first-letter {{ float: left; font-size: 8.4em; line-height: .68; font-weight: 100; margin: .05em .1em 0 0; text-transform: uppercase; }}
.alignleft {{ float: left; margin: .3em 1.5em .3em 0; }}
.alignright {{ float: right; margin: .3em 0 .3em 1.5em; }}
.aligncenter {{ margin-left: auto; margin-right: auto; text-align: center; }}
figure.wp-block-image {{ margin: 1.5em 0; }}
figure.wp-block-image.alignleft, figure.wp-block-image.alignright {{ max-width: 50%; margin-top: .3em; margin-bottom: 1em; }}
figure.wp-block-image.alignleft {{ margin-left: 0; margin-right: 1.5em; }}
figure.wp-block-image.alignright {{ margin-left: 1.5em; margin-right: 0; }}
figure.wp-block-image.aligncenter {{ margin-left: auto; margin-right: auto; }}
.wp-block-image.is-style-rounded img {{ border-radius: 9999px; }}
.wp-element-caption {{ font-size: .875em; opacity: .75; margin-top: .5em; text-align: center; }}
figure.wp-block-table {{ margin: 1.5em 0; overflow-x: auto; }}
.wp-block-table table {{ width: 100%; }}
.wp-block-table .has-fixed-layout {{ table-layout: fixed; }}
.wp-block-table thead {{ border-bottom: 3px solid; }}
.wp-block-table tfoot {{ border-top: 3px solid; font-weight: 600; }}
.wp-block-table.is-style-stripes {{ border-bottom: 1px solid #f0f0f0; }}
.wp-block-table.is-style-stripes th, .wp-block-table.is-style-stripes td {{ border-color: transparent; }}
.wp-block-table.is-style-stripes tbody tr:nth-child(odd) {{ background-color: rgba(128,128,128,.12); }}
.wp-block-quote.is-style-plain {{ border: none; padding-left: 0; }}
.wp-block-separator.is-style-dots {{ border: none; height: auto; text-align: center; }}
.wp-block-separator.is-style-dots::before {{ content: "\00b7 \00b7 \00b7"; font-size: 1.5em; letter-spacing: 2em; }}
.wp-block-separator.is-style-wide {{ border-width: 0 0 2px; }}
.wp-block-separator.has-background {{ border: none; height: 2px; }}
.wp-block-button.is-style-outline > .wp-block-button__link, .wp-block-button.is-style-lui-outline > .wp-block-button__link {{ background: transparent; }}
.wp-block-group.has-background {{ padding: 1.25em 2.375em; }}
/* Accordion and tabs: closed/hidden like on the blog, opened by the small
   script at the end of the page. */
.wp-block-accordion-item {{ border-bottom: 1px solid rgba(128,128,128,.35); }}
.wp-block-accordion-heading {{ margin: 0; }}
.wp-block-accordion-heading__toggle {{ all: unset; display: flex; width: 100%; justify-content: space-between; align-items: center; cursor: pointer; padding: .6em 0; font: inherit; font-weight: 600; }}
.wp-block-accordion-item:not(.is-open) > .wp-block-accordion-panel {{ display: none; }}
.wp-block-accordion-item.is-open .wp-block-accordion-heading__toggle-icon {{ transform: rotate(45deg); }}
.wp-block-tab-list {{ display: flex; gap: .25em; border-bottom: 1px solid rgba(128,128,128,.35); }}
.wp-block-tab-list > button {{ all: unset; cursor: pointer; padding: .5em 1em; border-bottom: 2px solid transparent; }}
.wp-block-tab-list > button.is-active {{ border-bottom-color: currentColor; font-weight: 600; }}
.wp-block-tab-panel:not(.is-active) {{ display: none; }}
{theme_css}
{BADGE_CSS}
{EMBED_CSS}
{HEADER_CSS}
</style></head><body>{header}{body}<script>
// Scroll-sync (see `window.rs::wire_scroll_sync`). Positions are mapped
// through *fractional* source lines, interpolated between the
// `[data-line]` anchors: line 12.5 sits halfway between where line 12's
// anchor and the next anchor are rendered, so a long paragraph or a tall
// image scrolls continuously instead of the preview standing still and then
// jumping a whole block. `bottomT` (0..1) blends the mapped position toward
// the page's real bottom over the editor's last screenful, so the end is
// reached without a hard snap; `topT` does the same for the top, but only
// for the stretch above the first block (the article header), so the
// preview doesn't lag behind the editor through the whole first screen.
window.__blocks = null;
window.__totalLines = 0;
// `[line, topT, bottomT]` of the last editor-driven sync, re-applied when
// the layout shifts under it (images finishing loading, a resize) - null
// once the user scrolls the preview by hand.
window.__lastSync = null;
// A scroll this script performs itself must not be reported back to the
// editor as if the user did it: the `scroll` event it causes is recognized
// by landing on the recorded target (or arriving within the short window
// after it), not by a flag that has to be released at the right moment.
window.__programmaticY = null;
window.__ignoreScrollUntil = 0;
// Every anchor as a (source line, page y) pair: a block's start line at
// its top edge and, where it has one, its `data-line-end` at its bottom
// edge. Sorted by line and kept strictly increasing in both, so nested
// anchors (a code block's per-line spans) slot in between their block's
// own start and end.
window.__blockPositions = function() {{
  if (window.__blocks) return window.__blocks;
  const raw = [];
  for (const b of document.querySelectorAll('[data-line]')) {{
    const rect = b.getBoundingClientRect();
    raw.push({{line: parseInt(b.getAttribute('data-line'), 10), y: rect.top + window.scrollY}});
    const end = parseInt(b.getAttribute('data-line-end'), 10);
    if (end > 0) raw.push({{line: end, y: rect.bottom + window.scrollY}});
  }}
  raw.sort(function(a, b) {{ return a.line - b.line || a.y - b.y; }});
  const list = [];
  for (const a of raw) {{
    const last = list[list.length - 1];
    if (!last || (a.line > last.line && a.y >= last.y)) list.push(a);
  }}
  const lastLine = list.length ? list[list.length - 1].line : 0;
  list.push({{line: Math.max(window.__totalLines + 1, lastLine + 1), y: document.documentElement.scrollHeight}});
  window.__blocks = list;
  return list;
}};
window.__maxScroll = function() {{
  return Math.max(0, document.documentElement.scrollHeight - window.innerHeight);
}};
window.__yForLine = function(line) {{
  const bl = window.__blockPositions();
  if (line <= bl[0].line) return bl.length > 1 ? bl[0].y : 0;
  for (let i = 0; i + 1 < bl.length; i++) {{
    if (line < bl[i + 1].line) {{
      const t = (line - bl[i].line) / (bl[i + 1].line - bl[i].line);
      return bl[i].y + t * (bl[i + 1].y - bl[i].y);
    }}
  }}
  return bl[bl.length - 1].y;
}};
window.__lineForY = function(y) {{
  const bl = window.__blockPositions();
  if (y <= bl[0].y) return bl[0].line;
  for (let i = 0; i + 1 < bl.length; i++) {{
    if (y < bl[i + 1].y) {{
      const span = bl[i + 1].y - bl[i].y;
      const t = span > 0 ? (y - bl[i].y) / span : 0;
      return bl[i].line + t * (bl[i + 1].line - bl[i].line);
    }}
  }}
  return bl[bl.length - 1].line;
}};
// Instant, not a smooth animation: the editor drives this once per frame
// while it scrolls, and each call restarting a smooth animation from
// wherever the previous one had got to is exactly what made the preview
// stutter and overshoot.
window.__scrollProgrammatically = function(y) {{
  y = Math.max(0, Math.min(window.__maxScroll(), y));
  window.__programmaticY = y;
  window.__ignoreScrollUntil = performance.now() + 150;
  window.scrollTo({{top: y, behavior: 'instant'}});
}};
window.syncTo = function(line, topT, bottomT, totalLines) {{
  if (totalLines > 0 && totalLines !== window.__totalLines) {{
    window.__totalLines = totalLines;
    window.__blocks = null;
  }}
  window.__lastSync = [line, topT, bottomT];
  let y = window.__yForLine(line);
  y = y * (1 - bottomT) + window.__maxScroll() * bottomT;
  y = y - topT * window.__blockPositions()[0].y;
  window.__scrollProgrammatically(y);
}};
// The current position in `syncTo`'s terms - what a re-render restores, so
// it lands on the same *content* even if the layout above it changed.
window.__currentSyncState = function() {{
  if (window.__lastSync) return window.__lastSync;
  const y = window.scrollY;
  const page = Math.max(1, window.innerHeight);
  const rem = Math.max(0, window.__maxScroll() - y);
  return [window.__lineForY(y), y < page ? 1 - y / page : 0, rem < page ? 1 - rem / page : 0];
}};
window.__reapplySync = function() {{
  window.__blocks = null;
  if (window.__lastSync) window.syncTo(window.__lastSync[0], window.__lastSync[1], window.__lastSync[2], 0);
}};
window.addEventListener('resize', window.__reapplySync);
window.addEventListener('load', window.__reapplySync);
// `load` doesn't bubble - captured here, every image finishing loading
// (and so growing from 0 to its real height) re-anchors the position.
document.addEventListener('load', function(e) {{
  if (e.target && e.target.tagName === 'IMG') window.__reapplySync();
}}, true);
window.__reportPending = false;
window.addEventListener('scroll', function() {{
  const y = window.scrollY;
  if (window.__programmaticY !== null && (Math.abs(y - window.__programmaticY) <= 1.5 || performance.now() < window.__ignoreScrollUntil)) return;
  window.__programmaticY = null;
  window.__lastSync = null;
  if (window.__reportPending) return;
  window.__reportPending = true;
  requestAnimationFrame(function() {{
    window.__reportPending = false;
    if (!(window.webkit && window.webkit.messageHandlers.{SCROLL_SYNC_HANDLER})) return;
    const state = window.__currentSyncState();
    window.webkit.messageHandlers.{SCROLL_SYNC_HANDLER}.postMessage(state.join(';'));
  }});
}});
// Accordions and tabs from the blog work here too.
document.addEventListener('click', function(e) {{
  const toggle = e.target.closest('.wp-block-accordion-heading__toggle');
  if (toggle) {{
    toggle.closest('.wp-block-accordion-item').classList.toggle('is-open');
    window.__reapplySync && (window.__blocks = null);
    return;
  }}
  const tab = e.target.closest('.wp-block-tab-list > button');
  if (tab) {{
    const tabs = tab.closest('.wp-block-tabs');
    const buttons = Array.from(tab.parentElement.children);
    const panels = tabs.querySelectorAll(':scope > .wp-block-tab-panels > .wp-block-tab-panel');
    buttons.forEach(function(b, i) {{
      b.classList.toggle('is-active', b === tab);
      if (panels[i]) panels[i].classList.toggle('is-active', b === tab);
    }});
    window.__blocks = null;
  }}
}});
for (const tabs of document.querySelectorAll('.wp-block-tabs')) {{
  const first = tabs.querySelector('.wp-block-tab-list > button');
  const panel = tabs.querySelector('.wp-block-tab-panels > .wp-block-tab-panel');
  if (first) first.classList.add('is-active');
  if (panel) panel.classList.add('is-active');
}}
{restore_js}
</script></body></html>"#
    )
}

/// Renders the document as a sequence of `<div data-line="N">...</div>`
/// wrappers, one per top-level Markdown block, using pulldown-cmark's own
/// HTML renderer for each block's inner content so output stays consistent
/// with plain rendering.
/// The 1-based line just *after* a block's last source line - the block's
/// `data-line-end`. Scroll-sync maps the block's source lines onto its
/// rendered height and the blank lines up to the next block onto the gap
/// between them; without it, a paragraph (one logical line, however many
/// rows it wraps to) and the blank line after it got equal weight, so the
/// panes drifted apart by several rows toward the end of every paragraph.
fn block_end_line(markdown: &str, range: &std::ops::Range<usize>) -> usize {
    let last_byte = range.end.saturating_sub(1).max(range.start);
    line_number(markdown, last_byte) + 1
}

/// One rendered top-level block of the preview.
struct RenderedBlock {
    html: String,
    line: usize,
    line_end: usize,
    /// The block's Markdown source, for re-rendering it with attributes.
    source: std::ops::Range<usize>,
}

#[cfg(test)]
fn render_body_with_line_anchors(markdown: &str, media: &[MediaItem]) -> String {
    render_body(markdown, media, None)
}

/// `footnotes`: the post's footnote texts (`Frontmatter::wp_footnotes`),
/// shown where the `wp:footnotes` block sits.
fn render_body(markdown: &str, media: &[MediaItem], footnotes: Option<&str>) -> String {
    let mut blocks: Vec<RenderedBlock> = Vec::new();
    for segment in gutenberg::split_segments(markdown) {
        match segment {
            gutenberg::Segment::Markdown(range) => render_markdown_chunk(markdown, range, &mut blocks),
            // A `:::` container renders as the Gutenberg markup it will
            // become, its content included.
            gutenberg::Segment::Container { range, .. } => blocks.push(RenderedBlock {
                html: gutenberg::parse_markdown(&markdown[range.clone()]).iter().map(gutenberg::render_block).collect::<Vec<_>>().join("\n"),
                line: line_number(markdown, range.start),
                line_end: block_end_line(markdown, &range),
                source: range,
            }),
            gutenberg::Segment::Raw(range) => blocks.push(RenderedBlock {
                html: dynamic_block_placeholder(&markdown[range.clone()], footnotes).unwrap_or_else(|| markdown[range.clone()].to_string()),
                line: line_number(markdown, range.start),
                line_end: block_end_line(markdown, &range),
                source: range,
            }),
            // An attribute line restyles the block above it - rendered the
            // way WordPress will, from the Gutenberg markup, so the theme
            // preset classes (`themestyle`) apply.
            gutenberg::Segment::Attrs { attrs, range } => match blocks.last_mut() {
                Some(last) => {
                    if let Some(block) = gutenberg::parse_markdown(&markdown[last.source.clone()]).pop() {
                        last.html = gutenberg::render_block(&block.with_attrs(attrs));
                    }
                    last.line_end = block_end_line(markdown, &range);
                    last.source = last.source.start..range.end;
                }
                None => blocks.push(RenderedBlock {
                    html: format!("<p>{}</p>", glib::markup_escape_text(&markdown[range.clone()])),
                    line: line_number(markdown, range.start),
                    line_end: block_end_line(markdown, &range),
                    source: range,
                }),
            },
        }
    }

    let mut out = String::new();
    for block in blocks {
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
        let div_balance = block.html.matches("<div").count() as isize - block.html.matches("</div>").count() as isize;
        if div_balance == 0 {
            out.push_str(&format!("<div data-line=\"{}\" data-line-end=\"{}\">{}</div>\n", block.line, block.line_end, block.html));
        } else {
            out.push_str(&block.html);
        }
    }
    let laid_out = inject_layout_styles(&inject_group_flex_styles(&out), "<!-- wp:buttons ", "<div class=\"wp-block-buttons");
    wrap_images_with_badges(&rewrite_media_tags(&embed_placeholders_in_kept_blocks(&laid_out)), media)
}

/// A `wp:embed` kept as block markup (say, one with a caption) holds just
/// its URL in `.wp-block-embed__wrapper` - shown as the same placeholder
/// card a Markdown embed line gets, instead of a bare URL.
fn embed_placeholders_in_kept_blocks(html: &str) -> String {
    const MARKER: &str = "<div class=\"wp-block-embed__wrapper\">";
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find(MARKER) {
        let content_start = start + MARKER.len();
        let Some(end_rel) = rest[content_start..].find("</div>") else { break };
        let url = rest[content_start..content_start + end_rel].trim();
        out.push_str(&rest[..content_start]);
        if (url.starts_with("http://") || url.starts_with("https://")) && !url.contains('<') {
            out.push_str(&render_embed_placeholder(url));
        } else {
            out.push_str(&rest[content_start..content_start + end_rel]);
        }
        rest = &rest[content_start + end_rel..];
    }
    out.push_str(rest);
    out
}

/// The top-level blocks of one stretch of plain Markdown (`range` of the
/// whole document, so line numbers stay document-wide).
fn render_markdown_chunk(markdown: &str, range: std::ops::Range<usize>, out: &mut Vec<RenderedBlock>) {
    let offset = range.start;
    let events: Vec<(Event, std::ops::Range<usize>)> = Parser::new_ext(&markdown[range], gutenberg::markdown_options()).into_offset_iter().map(|(event, r)| (event, r.start + offset..r.end + offset)).collect();

    let mut i = 0;
    while i < events.len() {
        match &events[i].0 {
            Event::Start(Tag::CodeBlock(kind)) => {
                let kind = kind.clone();
                let end = find_matching_end(&events, i, &TagEnd::CodeBlock);
                let line = line_number(markdown, events[i].1.start);
                let line_end = block_end_line(markdown, &events[i].1);
                let html = render_code_block_with_line_anchors(&events[i..=end], &kind, line);
                out.push(RenderedBlock { html, line, line_end, source: events[i].1.clone() });
                i = end + 1;
            }
            Event::Start(tag) => {
                let end_marker = tag.to_end();
                let end = find_matching_end(&events, i, &end_marker);
                let line = line_number(markdown, events[i].1.start);
                let line_end = block_end_line(markdown, &events[i].1);
                let source = events[i].1.clone();
                let embed_url = matches!(tag, Tag::Paragraph)
                    .then(|| events[i + 1..end].iter().map(|(event, _)| event.clone()).collect::<Vec<_>>())
                    .and_then(|inner_events| gutenberg::lone_embed_url(&inner_events));
                let has_heading_attrs = matches!(tag, Tag::Heading { id, classes, attrs, .. } if id.is_some() || !classes.is_empty() || !attrs.is_empty());
                // A lone image whose caption carries markup (a credit
                // link, emphasis) shows the `<figcaption>` WordPress gets.
                let rich_caption = matches!(tag, Tag::Paragraph)
                    && matches!(events.get(i + 1).map(|(e, _)| e), Some(Event::Start(Tag::Image { .. })))
                    && events[i + 2..end].iter().any(|(e, _)| matches!(e, Event::Start(Tag::Link { .. } | Tag::Emphasis | Tag::Strong) | Event::Code(_)));
                // A quote whose last paragraph is its source (`> — Quelle`)
                // shows the `<cite>` WordPress will get.
                let cited_quote = matches!(tag, Tag::BlockQuote(_))
                    .then(|| gutenberg::parse_markdown(&markdown[source.clone()]).pop())
                    .flatten()
                    .filter(|block| matches!(block, gutenberg::Block::BlockQuote { citation: Some(_), .. }));
                let html = match embed_url {
                    Some(url) => render_embed_placeholder(&url),
                    // `## Titel {#anker color=accent}` - through Gutenberg,
                    // like an attribute line.
                    None if rich_caption => gutenberg::parse_markdown(&markdown[source.clone()]).pop().map(|block| gutenberg::render_block(&block)).unwrap_or_default(),
                    None if cited_quote.is_some() => cited_quote.as_ref().map(gutenberg::render_block).unwrap_or_default(),
                    None if has_heading_attrs => gutenberg::parse_markdown(&markdown[source.clone()]).pop().map(|block| gutenberg::render_block(&block)).unwrap_or_default(),
                    None => {
                        let mut inner = String::new();
                        pulldown_cmark::html::push_html(&mut inner, events[i..=end].iter().map(|(event, _)| event.clone()));
                        inner
                    }
                };
                out.push(RenderedBlock { html, line, line_end, source });
                i = end + 1;
            }
            Event::Rule => {
                let line = line_number(markdown, events[i].1.start);
                out.push(RenderedBlock { html: "<hr/>".to_string(), line, line_end: line + 1, source: events[i].1.clone() });
                i += 1;
            }
            _ => i += 1,
        }
    }
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
/// straight onto the group's own `<div>`. Handles `"type":"flex"`
/// (row/stack) and `"type":"grid"` (with or without `columnCount`).
fn inject_group_flex_styles(html: &str) -> String {
    inject_layout_styles(html, "<!-- wp:group ", "<div class=\"wp-block-group")
}

/// `inject_group_flex_styles` for any block with a `layout` attribute:
/// `comment` opens its block comment, `div` starts its element.
fn inject_layout_styles(html: &str, comment: &str, div: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(marker) = rest.find(comment) {
        out.push_str(&rest[..marker]);
        let Some(comment_end_rel) = rest[marker..].find("-->") else {
            out.push_str(&rest[marker..]);
            return out;
        };
        let attrs_start = marker + comment.len();
        let comment_end = marker + comment_end_rel + 3;
        let style = flex_style_for_group_attrs(&rest[attrs_start..marker + comment_end_rel]);
        out.push_str(&rest[marker..comment_end]);
        rest = &rest[comment_end..];

        let Some(style) = style else { continue };
        let Some(div_start) = rest.find(div) else { continue };
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
    if attrs.contains("\"type\":\"grid\"") {
        let columns = attrs.split("\"columnCount\":").nth(1).map(|rest| rest.chars().take_while(char::is_ascii_digit).collect::<String>()).filter(|n| !n.is_empty());
        return Some(match columns {
            Some(n) => format!("display:grid;grid-template-columns:repeat({n},minmax(0,1fr));gap:var(--wp--style--block-gap,.5em);"),
            None => "display:grid;grid-template-columns:repeat(auto-fill,minmax(12rem,1fr));gap:var(--wp--style--block-gap,.5em);".to_string(),
        });
    }
    if !attrs.contains("\"type\":\"flex\"") {
        return None;
    }
    let vertical = attrs.contains("\"orientation\":\"vertical\"");
    let mut style = String::from("display:flex;gap:var(--wp--style--block-gap,.5em);");
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
.embed-placeholder .embed-url { font-size: .8em; opacity: .55; word-break: break-all; max-width: 90%; }
.embed-placeholder.dynamic-placeholder { aspect-ratio: auto; min-height: 5rem; border-style: dashed; }
.marker-line { display: flex; align-items: center; gap: 1em; margin: 1.5em 0; font-size: .8em; text-transform: uppercase; letter-spacing: .08em; opacity: .55; }
.marker-line::before, .marker-line::after { content: \"\"; flex: 1; border-top: 1px dashed currentColor; }
.wp-block-footnotes { font-size: .875em; border-top: 1px solid rgba(127,127,127,.3); padding-top: 1em; }";

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

/// A block the blog renders itself (latest posts, a table of contents, an
/// ad slot, ...) has no visible markup of its own - shown as a labeled
/// card instead of nothing. `None` for verbatim markup with content.
fn dynamic_block_placeholder(raw: &str, footnotes: Option<&str>) -> Option<String> {
    let name = raw.trim_start().strip_prefix("<!-- wp:")?.split_whitespace().next()?.trim_end_matches("/-->").to_string();
    // Blocks with a look of their own but no text.
    match name.as_str() {
        "more" => return Some(format!("<div class=\"marker-line\"><span>{}</span></div>", glib::markup_escape_text(&more_label(raw)))),
        "nextpage" => return Some(format!("<div class=\"marker-line\"><span>{}</span></div>", glib::markup_escape_text(&tr("Seitenumbruch")))),
        "footnotes" => return Some(render_footnotes(footnotes)),
        "shortcode" => return Some(render_shortcode(raw)),
        "html" | "spacer" | "separator" => return None,
        _ => {}
    }
    let mut visible = String::new();
    let mut has_element = false;
    let mut rest = raw;
    while let Some(start) = rest.find('<') {
        visible.push_str(&rest[..start]);
        let is_comment = rest[start..].starts_with("<!--");
        let tag_end = if is_comment { rest[start..].find("-->").map(|e| start + e + 3) } else { rest[start..].find('>').map(|e| start + e + 1) };
        let Some(end) = tag_end else { break };
        let tag = &rest[start..end];
        if tag.starts_with("<img") || tag.starts_with("<iframe") || tag.starts_with("<video") || tag.starts_with("<svg") || tag.starts_with("<input") || tag.starts_with("<hr") {
            return None;
        }
        has_element |= !is_comment;
        rest = &rest[end..];
    }
    visible.push_str(rest);
    // An element without text (a spacer, an empty group) is layout, not
    // a block the blog fills in - except for these, whose content is
    // nested blocks the server renders.
    // Their visible text in the saved markup is only a fallback (a query
    // loop's "Keine Beiträge gefunden.") - the blog shows other content.
    let server_filled = matches!(name.as_str(), "query" | "social-links" | "navigation" | "comments" | "post-template");
    if !server_filled && (!visible.trim().is_empty() || has_element) {
        return None;
    }
    let label = match name.as_str() {
        "latest-posts" => tr("Neueste Beiträge"),
        "latest-comments" => tr("Neueste Kommentare"),
        "query" => tr("Abfrage-Loop"),
        "archives" => tr("Archive"),
        "categories" => tr("Kategorien"),
        "tag-cloud" => tr("Schlagwörter-Wolke"),
        "calendar" => tr("Kalender"),
        "search" => tr("Suche"),
        "page-list" => tr("Seitenliste"),
        "social-links" => tr("Social-Media-Links"),
        "rss" => tr("RSS-Feed"),
        "block" => tr("Synchronisiertes Muster"),
        "lui/toc" => tr("Inhaltsverzeichnis"),
        "lui-ads/slot" => tr("Werbeplatz"),
        "icon" => tr("Symbol"),
        "loginout" => tr("Anmelden/Abmelden"),
        "post-time-to-read" => tr("Lesezeit"),
        "post-author" | "post-author-name" => tr("Autor"),
        "post-terms" => tr("Kategorien und Schlagwörter"),
        "post-date" => tr("Datum"),
        "avatar" => tr("Avatar"),
        "breadcrumbs" => tr("Brotkrümelnavigation"),
        "navigation" => tr("Navigation"),
        "comments" => tr("Kommentare"),
        _ => name.clone(),
    };
    // Social links name their services.
    let services: Vec<&str> = raw.split("\"service\":\"").skip(1).filter_map(|rest| rest.split('"').next()).collect();
    let detail = if services.is_empty() { tr("{name} – wird vom Blog erzeugt").replace("{name}", &name) } else { services.join(" · ") };
    Some(format!(
        "<div class=\"embed-placeholder dynamic-placeholder\"><span class=\"embed-label\">{}</span><span class=\"embed-url\">{}</span></div>",
        glib::markup_escape_text(&label),
        glib::markup_escape_text(&detail)
    ))
}

/// A `wp:shortcode` block: `[audio]`/`[video]` with a `src` play like the
/// player WordPress puts there, `[embed]` gets the embed card; anything
/// else is a placeholder naming the shortcode.
fn render_shortcode(raw: &str) -> String {
    let code = raw.lines().filter(|line| !line.trim_start().starts_with("<!--")).collect::<Vec<_>>().join("\n");
    let code = code.trim();
    let name: String = code.trim_start_matches('[').chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
    let attr = |key: &str| -> Option<String> {
        let start = code.find(&format!("{key}=\""))? + key.len() + 2;
        Some(code[start..start + code[start..].find('"')?].to_string())
    };
    match name.as_str() {
        "audio" | "video" => {
            let src = attr("src").or_else(|| ["mp3", "ogg", "m4a", "wav", "mp4", "webm", "ogv"].iter().find_map(|ext| attr(ext)));
            if let Some(src) = src {
                let tag = name.as_str();
                return format!("<figure class=\"wp-block-{tag}\"><{tag} controls src=\"{}\"></{tag}></figure>", glib::markup_escape_text(&src));
            }
        }
        "embed" => {
            let url = code.split(']').nth(1).and_then(|rest| rest.split("[/embed").next()).map(str::trim).unwrap_or("");
            if url.starts_with("http") {
                return render_embed_placeholder(url);
            }
        }
        _ => {}
    }
    format!(
        "<div class=\"embed-placeholder dynamic-placeholder\"><span class=\"embed-label\">{}</span><span class=\"embed-url\">{}</span></div>",
        glib::markup_escape_text(&tr("Shortcode [{name}]").replace("{name}", &name)),
        glib::markup_escape_text(&tr("{name} – wird vom Blog erzeugt").replace("{name}", "shortcode"))
    )
}

/// "Weiterlesen" or the custom text of a `wp:more` block.
fn more_label(raw: &str) -> String {
    raw.split("\"customText\":\"").nth(1).and_then(|rest| rest.split('"').next()).filter(|t| !t.is_empty()).map(str::to_string).unwrap_or_else(|| tr("Weiterlesen"))
}

/// The `wp:footnotes` block: the footnote texts from the post meta,
/// numbered in the order WordPress stores them.
fn render_footnotes(footnotes: Option<&str>) -> String {
    let items: Vec<(String, String)> = footnotes
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|note| Some((note["id"].as_str()?.to_string(), note["content"].as_str()?.to_string())))
        .collect();
    if items.is_empty() {
        return format!("<div class=\"embed-placeholder dynamic-placeholder\"><span class=\"embed-label\">{}</span></div>", glib::markup_escape_text(&tr("Fußnoten")));
    }
    let list: String = items
        .iter()
        // Footnote contents are inline HTML written in the block editor.
        .map(|(id, content)| format!("<li id=\"{}\">{content} <a href=\"#{}-link\">↩︎</a></li>", glib::markup_escape_text(id), glib::markup_escape_text(id)))
        .collect();
    format!("<ol class=\"wp-block-footnotes\">{list}</ol>")
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
/// without depending on this machine's own `$XDG_CONFIG_HOME/blocksatz/
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
        // An image inside a `<figure>` (Gutenberg markup: an attributed
        // image, a gallery, media & text, a kept block) already has its own
        // `<figcaption>` - or deliberately none.
        let in_figure = out.rfind("<figure").is_some_and(|open| out.rfind("</figure>").is_none_or(|close| close < open));
        if let Some(caption) = item.filter(|_| !in_figure).and_then(|item| item.caption.as_deref()).filter(|c| !c.is_empty()) {
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
/// so the scroll-sync interpolation (`window.__blockPositions`) - which
/// walks every `[data-line]` element in document order, not just top-level
/// blocks - has fine-grained anchors inside long code samples too.
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
        "preformatted" | "verse" => gutenberg::parse_fenced_pre(lang, text),
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
    fn attribute_line_restyles_the_block_above_and_is_not_shown() {
        let out = render_body_with_line_anchors("Erster Absatz.\n\nHinweis.\n{bg=accent color=base}\n\nDanach.\n", &[]);
        assert!(out.contains("class=\"has-base-color has-accent-background-color has-text-color has-background\""), "{out}");
        assert!(!out.contains("{bg="), "{out}");
        assert!(out.contains("data-line=\"3\" data-line-end=\"5\""), "{out}");
        assert!(out.contains("<p>Danach.</p>"), "{out}");
    }

    #[test]
    fn container_renders_as_gutenberg_markup() {
        let out = render_body_with_line_anchors(":::: accordion\n::: item \"Frage\" {open}\nAntwort.\n:::\n::::\n\nDanach.\n", &[]);
        assert!(out.contains("<div class=\"wp-block-accordion-item is-open\">"), "{out}");
        assert!(out.contains("<span class=\"wp-block-accordion-heading__toggle-title\">Frage</span>"), "{out}");
        assert!(!out.contains(":::"), "{out}");
        assert!(out.contains("<div data-line=\"7\""), "{out}");
    }

    #[test]
    fn kept_embed_shows_a_placeholder_and_figure_images_get_no_extra_caption() {
        let markdown = "<!-- wp:embed {\"url\":\"https://vimeo.com/1\"} -->\n<figure class=\"wp-block-embed\"><div class=\"wp-block-embed__wrapper\">\nhttps://vimeo.com/1\n</div><figcaption class=\"wp-element-caption\">Vimeo</figcaption></figure>\n<!-- /wp:embed -->\n\n<!-- wp:image -->\n<figure class=\"wp-block-image\"><img src=\"cat.png\" alt=\"\"/></figure>\n<!-- /wp:image -->\n";
        let mut item = media_item("cat.png", "cat.png", crate::media::AltText::Undefined, false);
        item.caption = Some("Aus der Galerie".to_string());
        let out = render_body_with_line_anchors(markdown, &[item]);
        assert!(out.contains("embed-placeholder"), "{out}");
        assert!(!out.contains("img-caption"), "{out}");
    }

    #[test]
    fn a_caption_with_a_link_shows_it() {
        let out = render_body("![Foto: [Name](https://example.org/)](bild.png)\n", &[], None);
        assert!(out.contains("<figcaption class=\"wp-element-caption\">Foto: <a href=\"https://example.org/\">Name</a></figcaption>"), "{out}");
    }

    #[test]
    fn a_quote_with_its_source_shows_the_cite() {
        let out = render_body("> Zitat.\n>\n> — Cicero, *De finibus*\n", &[], None);
        assert!(out.contains("<cite>Cicero, <em>De finibus</em></cite></blockquote>"), "{out}");
        let plain = render_body("> Nur ein Zitat.\n", &[], None);
        assert!(!plain.contains("<cite>") && plain.contains("<blockquote>"), "{plain}");
    }

    #[test]
    fn markers_spacers_and_footnotes_are_not_generic_placeholders() {
        let markdown = "<!-- wp:more {\"customText\":\"Mehr\"} -->\n<!--more Mehr-->\n<!-- /wp:more -->\n\n<!-- wp:spacer -->\n<div style=\"height:50px\" aria-hidden=\"true\" class=\"wp-block-spacer\"></div>\n<!-- /wp:spacer -->\n\n<!-- wp:footnotes /-->\n";
        let out = render_body(markdown, &[], Some(r#"[{"id":"a1","content":"Erste <em>Fußnote</em>"}]"#));
        assert!(out.contains("<span>Mehr</span>"), "{out}");
        assert!(out.contains("wp-block-spacer"), "{out}");
        assert!(out.contains("<li id=\"a1\">Erste <em>Fußnote</em>"), "{out}");
        assert!(!out.contains("dynamic-placeholder"), "{out}");
    }

    #[test]
    fn shortcodes_and_query_loops_render_like_the_blog() {
        let markdown = "<!-- wp:shortcode -->\n[audio src=\"https://example.org/a.mp3\"]\n<!-- /wp:shortcode -->\n\n<!-- wp:shortcode -->\n[contact-form id=\"3\"]\n<!-- /wp:shortcode -->\n\n<!-- wp:query -->\n<div class=\"wp-block-query\"><!-- wp:query-no-results -->\n<p>Keine Beiträge gefunden.</p>\n<!-- /wp:query-no-results --></div>\n<!-- /wp:query -->\n";
        let out = render_body(markdown, &[], None);
        assert!(out.contains("<audio controls src=\"https://example.org/a.mp3\">"), "{out}");
        assert!(out.contains("Shortcode [contact-form]"), "{out}");
        assert!(!out.contains("Keine Beiträge gefunden"), "{out}");
    }

    #[test]
    fn dynamic_blocks_get_a_placeholder() {
        let out = render_body_with_line_anchors("<!-- wp:latest-posts {\"postsToShow\":5} /-->\n\n<!-- wp:html -->\n<p>Sichtbar</p>\n<!-- /wp:html -->\n", &[]);
        assert!(out.contains("dynamic-placeholder"), "{out}");
        assert!(out.contains("Sichtbar"), "{out}");
        assert_eq!(out.matches("dynamic-placeholder").count(), 1, "{out}");
    }

    #[test]
    fn heading_attributes_are_not_shown_as_text() {
        let out = render_body_with_line_anchors("## Titel {#anker color=accent}\n", &[]);
        assert!(out.contains("id=\"anker\""), "{out}");
        assert!(out.contains("has-accent-color"), "{out}");
        assert!(!out.contains("{#anker"), "{out}");
    }

    #[test]
    fn verbatim_block_with_blank_lines_stays_one_block() {
        let markdown = "<!-- wp:group -->\n<div class=\"wp-block-group\"><!-- wp:paragraph -->\n<p>Eins</p>\n<!-- /wp:paragraph -->\n\n<!-- wp:paragraph -->\n<p>Zwei</p>\n<!-- /wp:paragraph --></div>\n<!-- /wp:group -->\n\nText\n";
        let out = render_body_with_line_anchors(markdown, &[]);
        assert!(out.starts_with("<div data-line=\"1\" data-line-end=\"10\"><!-- wp:group -->"), "{out}");
        assert!(out.contains("<div data-line=\"11\""), "{out}");
    }

    #[test]
    fn single_paragraph_is_tagged_with_its_line() {
        let out = render_body_with_line_anchors("Hello world.\n", &[]);
        assert_eq!(out, "<div data-line=\"1\" data-line-end=\"2\"><p>Hello world.</p>\n</div>\n");
    }

    #[test]
    fn blocks_separated_by_blank_lines_get_their_own_starting_line() {
        let markdown = "# Title\n\nSecond paragraph.\n\nThird paragraph.\n";
        let out = render_body_with_line_anchors(markdown, &[]);
        assert_eq!(
            out,
            "<div data-line=\"1\" data-line-end=\"2\"><h1>Title</h1>\n</div>\n\
             <div data-line=\"3\" data-line-end=\"4\"><p>Second paragraph.</p>\n</div>\n\
             <div data-line=\"5\" data-line-end=\"6\"><p>Third paragraph.</p>\n</div>\n"
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
        assert!(out.contains("<div data-line=\"1\" data-line-end=\"2\"><p>Intro text.</p>"));
        assert!(out.contains("<div data-line=\"3\" data-line-end=\"4\"><p><span class=\"img-wrap\"><img src=\"cat.png\" alt=\"a cat\""), "{out}");
        assert!(out.contains("<div data-line=\"5\" data-line-end=\"6\"><p>Outro text.</p>"));
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
        assert!(out.contains("<div data-line=\"3\" data-line-end=\"4\"><div class=\"embed-placeholder\">"), "{out}");
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
    fn full_html_embeds_the_interpolating_scroll_sync_script() {
        let html = render_html("Hello", PreviewStyle::Modern, false, &[], ScrollRestore::Top, &Frontmatter::default(), false, None);
        assert!(html.contains("window.syncTo = function(line, topT, bottomT, totalLines)"), "{html}");
        assert!(html.contains("window.__lineForY = function(y)"), "{html}");
        assert!(html.contains("messageHandlers.scrollSync"), "{html}");
        assert!(html.contains("behavior: 'instant'"), "{html}");
        assert!(!html.contains("behavior: 'smooth'"), "a per-frame sync must not restart smooth animations: {html}");
    }

    #[test]
    fn scroll_restore_parses_the_page_state_and_falls_back_to_top() {
        assert_eq!(ScrollRestore::from_state_json("[12.5,0,0.25]"), ScrollRestore::Sync { line: 12.5, top_t: 0.0, bottom_t: 0.25 });
        assert_eq!(ScrollRestore::from_state_json("null"), ScrollRestore::Top);
        assert_eq!(ScrollRestore::from_state_json("[1,2]"), ScrollRestore::Top);
        assert_eq!(ScrollRestore::from_state_json("garbage"), ScrollRestore::Top);
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
        let html = render_html("Hello", PreviewStyle::Modern, true, &[], ScrollRestore::Top, &Frontmatter::default(), false, Some(("#282a36".to_string(), "#f8f8f2".to_string())));
        // The style's own hardcoded dark-mode `pre` background (see
        // `style_css`) must lose the cascade to the scheme's, which is
        // appended after it - not just be present somewhere in the page.
        let scheme_rule_pos = html.find("pre { background: #282a36;").expect("scheme override present");
        let style_rule_pos = html.find("pre { background: #2d2d2d;").expect("style default present");
        assert!(scheme_rule_pos > style_rule_pos, "{html}");
    }

    #[test]
    fn full_html_keeps_the_style_default_code_colors_without_a_scheme() {
        let html = render_html("Hello", PreviewStyle::Modern, true, &[], ScrollRestore::Top, &Frontmatter::default(), false, None);
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
                size_slug: None,
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
        assert!(out.contains("<div data-line=\"3\" data-line-end=\"4\"><hr/></div>"));
    }

    #[test]
    fn multiline_code_block_tags_each_line_individually() {
        // The whole point: a long fenced code block used to be one opaque
        // scroll-sync anchor, so scrolling through it in the editor never
        // moved the preview at all until you scrolled past the block
        // entirely. Each line inside it needs its own `data-line` now.
        let markdown = "Intro.\n\n```bash\nfirst\nsecond\nthird\n```\n\nOutro.\n";
        let out = render_body_with_line_anchors(markdown, &[]);
        assert!(out.contains("<div data-line=\"3\" data-line-end=\"8\">"), "{out}");
        assert!(out.contains("<span data-line=\"4\">first</span>"), "{out}");
        assert!(out.contains("<span data-line=\"5\">second</span>"), "{out}");
        assert!(out.contains("<span data-line=\"6\">third</span>"), "{out}");
        assert!(out.contains("<div data-line=\"9\" data-line-end=\"10\">"), "{out}");
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
    fn full_html_anchors_blocks_to_their_source_line() {
        let html = render_html("Hello", PreviewStyle::Modern, false, &[], ScrollRestore::Top, &Frontmatter::default(), false, None);
        assert!(html.contains("data-line=\"1\""));
    }

    #[test]
    fn full_html_restores_a_sync_position_by_content() {
        let html = render_html("Hello", PreviewStyle::Modern, false, &[], ScrollRestore::Sync { line: 7.5, top_t: 0.0, bottom_t: 0.0 }, &Frontmatter::default(), false, None);
        assert!(html.contains("window.__lastSync = [7.5, 0, 0]; window.__reapplySync();"), "{html}");
    }

    #[test]
    fn preview_style_id_round_trips() {
        for style in PreviewStyle::ALL {
            assert_eq!(PreviewStyle::from_id(style.id()), style);
        }
    }

    #[test]
    fn item_index_for_image_uri_matches_a_local_file_uri_back_to_its_source() {
        let dir = std::env::temp_dir().join(format!("blocksatz-preview-uri-match-test-{}", std::process::id()));
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
        let shown = render_html("Hello", PreviewStyle::Modern, false, &[], ScrollRestore::Top, &frontmatter, true, None);
        let hidden = render_html("Hello", PreviewStyle::Modern, false, &[], ScrollRestore::Top, &frontmatter, false, None);
        assert!(shown.contains("<h1 class=\"article-header-title\">"), "{shown}");
        assert!(!hidden.contains("<h1 class=\"article-header-title\">"), "{hidden}");
    }
}
