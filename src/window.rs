use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk4::{gdk, gio, glib};

use crate::document::{Document, Frontmatter, PostType};
use crate::i18n::tr;
use crate::{
    about, aimenu, autosave, browser, chat, codeview, document, editor, export, formatting, imagealt, importer, linkpicker, media, medialibrary, mediapanel,
    preview, properties, recentfiles, richtext, searchbar, settings, shortcuts, stats, statusbar, termcache, windowstate,
};

const DEBOUNCE_MS: u64 = 250;

pub fn build(app: &adw::Application, initial_path: Option<PathBuf>) -> adw::ApplicationWindow {
    let saved_window_state = windowstate::load();

    let (editor_scroller, view, buffer, spelling_menu) = editor::build();
    let preview_pane = Rc::new(preview::PreviewPane::new());
    let stats_view = Rc::new(stats::StatsView::new());

    let toolbar = formatting::build(&view, &buffer);
    formatting::install_shortcuts(&view, &buffer);
    let search_bar = searchbar::SearchBar::new(&view, &buffer);
    let toolbar_separator = gtk4::Separator::new(gtk4::Orientation::Horizontal);
    let editor_pane = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    // Below the "narrow" breakpoint the window can (via a tiling WM, or by
    // dragging an edge) become narrower than the toolbar's ~17 buttons
    // naturally need - wrapped in a horizontal-only `Gtk.ScrolledWindow`,
    // the overflow is still reachable by scrolling instead of silently
    // clipped off the edge.
    let toolbar_scroller = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Automatic)
        .vscrollbar_policy(gtk4::PolicyType::Never)
        .child(&toolbar)
        .build();
    editor_pane.append(&toolbar_scroller);
    editor_pane.append(&toolbar_separator);
    editor_pane.append(&editor_scroller);
    editor_scroller.set_vexpand(true);
    // `search_bar.widget` is a `Gtk.Revealer` sliding up from the bottom -
    // its `SlideUp` transition only collapses *height* while hidden, so its
    // full natural *width* (two entries plus "Alle ersetzen" etc.) was
    // still setting editor_pane's minimum width even with the bar
    // invisible. Same fix as the toolbar above: a horizontal-only
    // `Gtk.ScrolledWindow` lets it scroll instead of enforcing that width.
    let search_bar_scroller = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Automatic)
        .vscrollbar_policy(gtk4::PolicyType::Never)
        .child(&search_bar.widget)
        .build();
    editor_pane.append(&search_bar_scroller);

    let chat_view = Rc::new(chat::ChatView::new(&buffer));
    let code_view = Rc::new(codeview::CodeView::new());
    let browser_view = Rc::new(browser::BrowserView::new());

    let view_stack = adw::ViewStack::new();
    // Homogeneous sizing (the default) makes the stack's minimum width the
    // max across ALL tabs, including hidden ones - so the Browser tab's
    // WebView alone would force a floor well above the "narrow" breakpoint
    // below, making it unreachable by resizing. Size to the visible tab
    // only instead.
    view_stack.set_hhomogeneous(false);
    view_stack.add_titled_with_icon(&preview_pane.widget, Some("preview"), &tr("Vorschau"), "view-reveal-symbolic");
    view_stack.add_titled_with_icon(&code_view.widget, Some("code"), &tr("Gutenberg-Code"), "text-x-generic-symbolic");
    view_stack.add_titled_with_icon(&stats_view.widget, Some("stats"), &tr("Statistik"), "view-list-symbolic");
    view_stack.add_titled_with_icon(&chat_view.widget, Some("chat"), &tr("Chat"), "chat-message-new-symbolic");
    view_stack.add_titled_with_icon(&browser_view.widget, Some("browser"), &tr("Browser"), "web-browser-symbolic");
    {
        // The active provider/model may have changed in Einstellungen since
        // the Chat tab was built (or since it was last shown), so refresh
        // its provider label/model picker every time it becomes visible.
        let chat_view = chat_view.clone();
        view_stack.connect_visible_child_name_notify(move |stack| {
            if stack.visible_child_name().as_deref() == Some("chat") {
                chat_view.refresh();
            }
        });
    }
    // `Adw.InlineViewSwitcher` renders all tabs as one seamless linked pill
    // (unlike `Adw.ViewSwitcher`, which only highlights the active tab and
    // leaves the others as loose, ungrouped buttons). Held in a plain
    // `Gtk.Box` with the exact same margins/spacing as `formatting::build`'s
    // toolbar - not an `Adw.HeaderBar`, which carries its own themed
    // background and height that never quite matched the editor's toolbar
    // (and differently so across themes/styles) - so the two toolbar rows
    // above each pane read as one consistent design regardless of theme.
    let view_switcher = adw::InlineViewSwitcher::builder().stack(&view_stack).build();
    let switcher_bar = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();
    switcher_bar.append(&view_switcher);

    // Toggles the magazine-style article header (`preview::render_header`)
    // above the rendered body - only relevant to the Vorschau tab, so only
    // shown while it's the active one, same live-visibility trick the chat
    // refresh above uses.
    let header_toggle_button = gtk4::ToggleButton::builder()
        .icon_name("document-properties-symbolic")
        .tooltip_text(tr("Artikel-Kopf in der Vorschau ein-/ausblenden"))
        .active(preview_pane.show_article_header())
        .visible(view_stack.visible_child_name().as_deref() == Some("preview"))
        .build();
    {
        let preview_pane = preview_pane.clone();
        header_toggle_button.connect_toggled(move |button| {
            preview_pane.set_show_article_header(button.is_active());
        });
    }
    {
        let header_toggle_button = header_toggle_button.clone();
        view_stack.connect_visible_child_name_notify(move |stack| {
            header_toggle_button.set_visible(stack.visible_child_name().as_deref() == Some("preview"));
        });
    }
    switcher_bar.append(&header_toggle_button);

    let right_pane = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    right_pane.append(&switcher_bar);
    right_pane.append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));
    right_pane.append(&view_stack);
    view_stack.set_vexpand(true);

    // Two interchangeable arrangements of the exact same `editor_pane`/
    // `right_pane` widgets, switched by `layout_view` below rather than
    // built as two separate widget trees - `Adw.MultiLayoutView` moves the
    // real widgets between `Adw.LayoutSlot` placeholders itself, so there's
    // still only ever one `editor_pane`/`right_pane` instance (each can
    // only have one parent at a time in GTK either way).
    //
    // "wide": today's side-by-side `Gtk.Paned`, unchanged.
    let wide_paned = gtk4::Paned::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .start_child(&adw::LayoutSlot::new("editor"))
        .end_child(&adw::LayoutSlot::new("sidebar"))
        .resize_start_child(true)
        .resize_end_child(true)
        // Both children must stay shrinkable - not just resizable - or the
        // Paned's natural minimum width sets the window's own minimum
        // width, which would sit above the "narrow" breakpoint below and
        // make it physically unreachable by resizing (a tiling WM or a
        // dragged edge could never get the window narrow enough for the
        // breakpoint to fire). Below the breakpoint the narrow layout is
        // swapped in anyway, so this Paned is never what's on screen at a
        // width small enough for the shrinking to look cramped.
        .shrink_start_child(true)
        .shrink_end_child(true)
        // Half of whatever width the window is about to open at (restored
        // or default, see `saved_window_state` above) - not a fixed pixel
        // value, so the 50/50 split holds regardless of the actual size.
        .position(saved_window_state.width / 2)
        .build();
    let wide_layout = adw::Layout::new(&wide_paned);
    wide_layout.set_name(Some("wide"));

    // "narrow": a tiling-WM-width or tablet-width window can't fit two
    // full panes side by side usefully - one pane at a time instead,
    // switched via the same `Adw.InlineViewSwitcher` style the sidebar's
    // own Vorschau/Gutenberg-Code/Statistik/Chat/Browser tabs already use,
    // so it reads as the same interaction pattern rather than a
    // one-off. `right_pane`'s "Vorschau ein-/ausblenden" visibility toggle
    // still works here (it just hides that widget wherever it currently
    // lives), though a hidden-but-still-selected "Vorschau" tab in this
    // narrow switcher shows an empty page rather than collapsing away the
    // way the wide `Gtk.Paned` does - a minor, rare edge case (hiding the
    // preview *and* being narrow at once) not worth extra machinery for.
    let narrow_view_stack = adw::ViewStack::new();
    // Same reasoning as `view_stack` above: without this, the wider of the
    // two pages (usually "sidebar", since it embeds `view_stack` itself)
    // would set the floor for both, defeating the point of a narrow layout.
    narrow_view_stack.set_hhomogeneous(false);
    narrow_view_stack.add_titled_with_icon(&adw::LayoutSlot::new("editor"), Some("editor"), &tr("Editor"), "text-editor-symbolic");
    narrow_view_stack.add_titled_with_icon(&adw::LayoutSlot::new("sidebar"), Some("sidebar"), &tr("Vorschau"), "view-reveal-symbolic");
    narrow_view_stack.set_vexpand(true);
    let narrow_switcher = adw::InlineViewSwitcher::builder().stack(&narrow_view_stack).build();
    let narrow_switcher_bar = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .halign(gtk4::Align::Center)
        .margin_top(6)
        .margin_bottom(6)
        .build();
    narrow_switcher_bar.append(&narrow_switcher);
    let narrow_box = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    narrow_box.append(&narrow_switcher_bar);
    narrow_box.append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));
    narrow_box.append(&narrow_view_stack);
    let narrow_layout = adw::Layout::new(&narrow_box);
    narrow_layout.set_name(Some("narrow"));

    let layout_view = adw::MultiLayoutView::new();
    layout_view.set_child("editor", &editor_pane);
    layout_view.set_child("sidebar", &right_pane);
    layout_view.add_layout(wide_layout);
    layout_view.add_layout(narrow_layout);
    layout_view.set_layout_name("wide");

    // `Sp` ("scale-independent pixels", GNOME's recommended unit for
    // breakpoints - it scales with the user's text-size/accessibility
    // settings rather than always meaning the same physical pixel count)
    // rather than `Px`. 700 leaves a wide `Gtk.Paned` split comfortably
    // usable (two ~350sp panes) right down to the threshold; narrower
    // than that - a tiled quarter of a typical monitor, or a Linux
    // tablet in portrait - collapses to the single-pane switcher instead.
    let narrow_condition = adw::BreakpointCondition::new_length(adw::BreakpointConditionLengthType::MaxWidth, 700.0, adw::LengthUnit::Sp);
    let narrow_breakpoint = adw::Breakpoint::new(narrow_condition);
    narrow_breakpoint.add_setter(&layout_view, "layout-name", Some(&"narrow".to_value()));

    let title = adw::WindowTitle::new("Blocksmith", &tr("Unbenannt"));

    let new_button = gtk4::Button::from_icon_name("document-new-symbolic");
    new_button.set_tooltip_text(Some(&tr("Neu (Strg+N)")));
    new_button.set_action_name(Some("win.new"));

    let open_button = gtk4::Button::from_icon_name("document-open-symbolic");
    open_button.set_tooltip_text(Some(&tr("Öffnen (Strg+O)")));
    open_button.set_action_name(Some("win.open"));

    let recent_button = gtk4::MenuButton::new();
    recent_button.set_icon_name("document-open-recent-symbolic");
    recent_button.set_tooltip_text(Some(&tr("Zuletzt geöffnet")));
    let recent_list = gtk4::ListBox::new();
    recent_list.add_css_class("boxed-list");
    let recent_scroller = gtk4::ScrolledWindow::builder().child(&recent_list).min_content_width(320).max_content_height(360).propagate_natural_height(true).build();
    let recent_popover = gtk4::Popover::new();
    recent_popover.set_child(Some(&recent_scroller));
    recent_button.set_popover(Some(&recent_popover));

    let open_from_wp_button = gtk4::Button::from_icon_name("folder-remote-symbolic");
    open_from_wp_button.set_tooltip_text(Some(&tr("Von WordPress öffnen (Strg+Umschalt+O)")));
    open_from_wp_button.set_action_name(Some("win.open-from-wordpress"));

    let save_button = gtk4::Button::from_icon_name("document-save-symbolic");
    save_button.set_tooltip_text(Some(&tr("Speichern (Strg+S)")));
    save_button.set_action_name(Some("win.save"));

    let properties_button = gtk4::Button::from_icon_name("document-properties-symbolic");
    properties_button.set_tooltip_text(Some(&tr("Artikel-Eigenschaften")));
    properties_button.set_action_name(Some("win.properties"));

    let media_button = gtk4::Button::from_icon_name("image-x-generic-symbolic");
    media_button.set_tooltip_text(Some(&tr("Medienverwaltung (Strg+Umschalt+M)")));
    media_button.set_action_name(Some("win.media-manager"));

    // A real primary menu (rather than the plain "win.settings"-bound
    // button this used to be) - "open-menu-symbolic" is the conventional
    // GNOME hamburger icon for exactly this, and "Über Blocksmith" needs
    // *some* home now that it exists; Ctrl+, still opens Einstellungen
    // directly, since that's the action-level shortcut, independent of
    // how the button itself triggers it.
    let primary_menu = gio::Menu::new();
    let new_section = gio::Menu::new();
    new_section.append(Some(&tr("Neue Seite")), Some("win.new-page"));
    primary_menu.append_section(None, &new_section);
    primary_menu.append(Some(&tr("Einstellungen")), Some("win.settings"));
    primary_menu.append(Some(&tr("Tastenkürzel")), Some("win.show-help-overlay"));
    primary_menu.append(Some(&tr("Über Blocksmith")), Some("win.about"));

    let settings_button = gtk4::MenuButton::new();
    settings_button.set_icon_name("open-menu-symbolic");
    settings_button.set_tooltip_text(Some(&tr("Hauptmenü (Strg+,)")));
    settings_button.set_menu_model(Some(&primary_menu));

    let preview_toggle_button = gtk4::ToggleButton::builder().icon_name("sidebar-show-right-symbolic").active(true).build();
    preview_toggle_button.set_tooltip_text(Some(&tr("Vorschau ein-/ausblenden")));
    preview_toggle_button.set_action_name(Some("win.toggle-preview"));

    // No matching "exit" button by design: entering hides the whole header
    // bar (see `toggle-focus-mode` below), so the only way back is the same
    // shortcut - a button that vanishes along with the rest of the chrome
    // it just hid would be pointless to also draw.
    let focus_mode_toggle_button = gtk4::ToggleButton::builder().icon_name("view-fullscreen-symbolic").build();
    focus_mode_toggle_button.set_tooltip_text(Some(&tr("Fokus-Schreibmodus (Strg+Umschalt+F)")));
    focus_mode_toggle_button.set_action_name(Some("win.toggle-focus-mode"));

    let publish_button = gtk4::Button::from_icon_name("send-to-symbolic");
    publish_button.set_tooltip_text(Some(&tr("Artikel exportieren (Strg+Umschalt+P)")));
    publish_button.set_action_name(Some("win.publish"));
    publish_button.add_css_class("suggested-action");

    let header_bar = adw::HeaderBar::new();
    header_bar.set_title_widget(Some(&title));
    header_bar.pack_start(&new_button);
    header_bar.pack_start(&open_button);
    header_bar.pack_start(&recent_button);
    header_bar.pack_start(&open_from_wp_button);
    header_bar.pack_start(&save_button);
    header_bar.pack_end(&settings_button);
    header_bar.pack_end(&properties_button);
    header_bar.pack_end(&media_button);
    header_bar.pack_end(&preview_toggle_button);
    header_bar.pack_end(&focus_mode_toggle_button);
    header_bar.pack_end(&publish_button);

    let status_bar = Rc::new(statusbar::StatusBar::new());

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header_bar);
    toolbar_view.set_content(Some(&layout_view));
    toolbar_view.add_bottom_bar(&status_bar.widget);

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&toolbar_view));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Blocksmith")
        .default_width(saved_window_state.width)
        .default_height(saved_window_state.height)
        .maximized(saved_window_state.maximized)
        .content(&toast_overlay)
        .build();

    window.add_breakpoint(narrow_breakpoint);

    window.connect_close_request(|window| {
        let state = windowstate::WindowState {
            width: window.width(),
            height: window.height(),
            maximized: window.is_maximized(),
        };
        let _ = windowstate::save(&state);
        glib::Propagation::Proceed
    });

    // A `Gtk.Paned` gives its other child the full width once one side is
    // hidden (no stray empty gap or handle) - so collapsing the whole right
    // pane is just a visibility toggle, not a position/size dance. Bound as
    // a stateful action (rather than a plain signal handler) so the toggle
    // button's own pressed-in state stays in sync automatically, the same
    // way every other header-bar button here is wired through `win.*`.
    let toggle_preview_action = gio::SimpleAction::new_stateful("toggle-preview", None, &true.to_variant());
    {
        let right_pane = right_pane.clone();
        toggle_preview_action.connect_activate(move |action, _| {
            let visible = !action.state().and_then(|state| state.get::<bool>()).unwrap_or(true);
            action.set_state(&visible.to_variant());
            right_pane.set_visible(visible);
        });
    }
    window.add_action(&toggle_preview_action);

    // Hides everything but the editor text itself: the header bar and
    // status bar via `Adw.ToolbarView`'s own animated reveal (built
    // exactly for this "collapse chrome to fullscreen content" pattern),
    // plus the formatting toolbar and the right-hand pane, which aren't
    // part of that toolbar view at all. Restoring the right pane defers to
    // `toggle_preview_action`'s own state rather than unconditionally
    // showing it again, so a preview the user had already hidden on
    // purpose stays hidden after leaving focus mode instead of reappearing.
    let toggle_focus_mode_action = gio::SimpleAction::new_stateful("toggle-focus-mode", None, &false.to_variant());
    {
        let toolbar_view = toolbar_view.clone();
        let toolbar = toolbar.clone();
        let toolbar_separator = toolbar_separator.clone();
        let right_pane = right_pane.clone();
        let toggle_preview_action = toggle_preview_action.clone();
        toggle_focus_mode_action.connect_activate(move |action, _| {
            let focus_mode = !action.state().and_then(|state| state.get::<bool>()).unwrap_or(false);
            action.set_state(&focus_mode.to_variant());
            toolbar_view.set_reveal_top_bars(!focus_mode);
            toolbar_view.set_reveal_bottom_bars(!focus_mode);
            toolbar.set_visible(!focus_mode);
            toolbar_separator.set_visible(!focus_mode);
            let preview_wanted = toggle_preview_action.state().and_then(|state| state.get::<bool>()).unwrap_or(true);
            right_pane.set_visible(!focus_mode && preview_wanted);
        });
    }
    window.add_action(&toggle_focus_mode_action);

    let current_path: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
    let frontmatter: Rc<RefCell<Frontmatter>> = Rc::new(RefCell::new(Frontmatter::default()));
    // What's currently safely on disk (or, for a still-unsaved document,
    // just ""): the baseline `wire_live_preview`'s autosave tick compares
    // the buffer against, so loading an already-saved article doesn't
    // immediately manufacture a bogus "unsaved changes" recovery snapshot
    // for content that was never actually edited - see `autosave.rs`.
    let saved_text: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));

    let cached_terms = termcache::load();
    let term_caches = termcache::TermCacheHandles {
        categories: Rc::new(RefCell::new(cached_terms.categories)),
        tags: Rc::new(RefCell::new(cached_terms.tags)),
        category_slugs: Rc::new(RefCell::new(cached_terms.category_slugs)),
    };
    termcache::spawn_refresh(&term_caches);

    let image_alt_menu = imagealt::install(&view, &buffer, frontmatter.clone(), current_path.clone(), preview_pane.clone());
    preview::PreviewPane::install_alt_text_menu(&preview_pane, &window, frontmatter.clone(), buffer.clone());
    preview::PreviewPane::install_image_edit_menu(&preview_pane, &window, frontmatter.clone(), buffer.clone());
    {
        let browser_view = browser_view.clone();
        let view_stack = view_stack.clone();
        preview_pane.connect_link_clicked(move |uri| {
            browser_view.load_uri(&uri);
            view_stack.set_visible_child_name("browser");
        });
    }
    let ai_menu_handles = aimenu::install(&view, &buffer, &view_stack, chat_view.clone(), &spelling_menu, image_alt_menu.upcast_ref());

    let doc_ctx = DocContext {
        buffer: buffer.clone(),
        current_path: current_path.clone(),
        frontmatter: frontmatter.clone(),
        title: title.clone(),
        toast_overlay: toast_overlay.clone(),
        preview_pane: preview_pane.clone(),
        saved_text: saved_text.clone(),
    };

    wire_live_preview(&buffer, &preview_pane, &stats_view, &code_view, &frontmatter, &current_path, &saved_text);
    wire_scroll_sync(&editor_scroller, &view, &buffer, &preview_pane);
    wire_status_bar(&buffer, &status_bar);
    wire_new_action(&window, &buffer, &current_path, &frontmatter, &title, &preview_pane, &saved_text);
    wire_open_action(&window, &doc_ctx);
    wire_open_path_action(&window, &doc_ctx);
    wire_open_from_wordpress_action(&window, &buffer, &current_path, &frontmatter, &title, &preview_pane, &saved_text);
    wire_save_action(&window, &doc_ctx);
    let recent_files_widgets = RecentFilesWidgets {
        button: recent_button,
        popover: recent_popover,
        list: recent_list,
    };
    wire_recent_files_button(&recent_files_widgets, &doc_ctx);
    wire_properties_action(&window, &buffer, &frontmatter, &term_caches, &current_path, &preview_pane);
    wire_settings_action(&window, &buffer, ai_menu_handles, &preview_pane, &browser_view);
    wire_about_action(&window);
    wire_publish_action(&window, &buffer, &current_path, &frontmatter, &preview_pane, &view_stack, &browser_view);
    wire_media_action(&window, &buffer, &current_path, &frontmatter, &preview_pane);
    wire_insert_image_action(&window, &buffer, &current_path);
    wire_insert_media_action(&window, &buffer, &current_path);
    wire_insert_media_library_action(&window, &buffer, &frontmatter);
    wire_insert_post_link_action(&window, &buffer);
    wire_paste_shortcut(&view, &buffer, &current_path, &toast_overlay);
    wire_drop_target(&view, &buffer, &current_path);
    wire_startup_recovery(&window, &buffer, &current_path, &frontmatter, &title, &preview_pane);
    wire_find_action(&window, &search_bar);
    window.set_help_overlay(Some(&shortcuts::build()));

    if let Some(path) = initial_path {
        open_document_at_path(path, &doc_ctx);
    }

    window
}

fn show_toast(overlay: &adw::ToastOverlay, message: &str) {
    overlay.add_toast(adw::Toast::new(message));
}

fn subtitle_for(path: Option<&Path>, frontmatter: &Frontmatter) -> String {
    if !frontmatter.title.is_empty() {
        return frontmatter.title.clone();
    }
    path.and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| tr("Unbenannt"))
}

fn wire_live_preview(
    buffer: &sourceview5::Buffer,
    preview_pane: &Rc<preview::PreviewPane>,
    stats_view: &Rc<stats::StatsView>,
    code_view: &Rc<codeview::CodeView>,
    frontmatter: &Rc<RefCell<Frontmatter>>,
    current_path: &Rc<RefCell<Option<PathBuf>>>,
    saved_text: &Rc<RefCell<String>>,
) {
    let debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));

    let preview_pane_clone = preview_pane.clone();
    let stats_view_clone = stats_view.clone();
    let code_view_clone = code_view.clone();
    let frontmatter_clone = frontmatter.clone();
    let current_path_clone = current_path.clone();
    let saved_text_clone = saved_text.clone();
    let debounce_clone = debounce.clone();
    buffer.connect_changed(move |buf| {
        if let Some(id) = debounce_clone.borrow_mut().take() {
            id.remove();
        }
        let text = buf.text(&buf.start_iter(), &buf.end_iter(), false).to_string();
        let preview_pane = preview_pane_clone.clone();
        let stats_view = stats_view_clone.clone();
        let code_view = code_view_clone.clone();
        let frontmatter = frontmatter_clone.clone();
        let current_path = current_path_clone.clone();
        let saved_text = saved_text_clone.clone();
        let debounce_inner = debounce_clone.clone();
        let id = glib::timeout_add_local(Duration::from_millis(DEBOUNCE_MS), move || {
            // Reconciled here (not just relying on whatever the media list
            // already holds) so the badges the preview draws - and any
            // other feature reading `frontmatter.media` afterward, like
            // Medienverwaltung - see a brand-new `![]()` reference right
            // away, not only once some other dialog happens to reconcile it.
            let media_items = {
                let mut fm = frontmatter.borrow_mut();
                fm.media = media::reconcile(&fm.media, &text);
                fm.media.clone()
            };
            preview_pane.update_preserving_scroll(&text, &media_items);
            stats_view.update(&text);
            code_view.update(&text, &media_items);
            // Piggybacks on this same debounce instead of running its own
            // timer - see `autosave.rs`. Skipped when the text still
            // matches what's already safely on disk (e.g. right after
            // opening a file, whose own `buffer.set_text` also runs through
            // this same `changed` signal), so opening-and-not-editing an
            // article never manufactures a bogus recovery prompt.
            if text != *saved_text.borrow() {
                autosave::save(&frontmatter.borrow(), &text, current_path.borrow().as_deref());
            }
            *debounce_inner.borrow_mut() = None;
            glib::ControlFlow::Break
        });
        *debounce_clone.borrow_mut() = Some(id);
    });

    preview_pane.update("", &frontmatter.borrow().media.clone());
    stats_view.update("");
    code_view.update("", &frontmatter.borrow().media.clone());
}

/// The bottom status bar: word count/reading time for the whole document
/// (debounced on `changed`, same rhythm as the preview/stats/code panels),
/// plus the same two numbers for the current selection - tracked via
/// `mark-set`, since that's the signal that fires for both cursor moves
/// and selection drags, with its own (shorter) debounce since it fires far
/// more often than `changed` while the user is just moving the cursor.
fn wire_status_bar(buffer: &sourceview5::Buffer, status_bar: &Rc<statusbar::StatusBar>) {
    const SELECTION_DEBOUNCE_MS: u64 = 120;

    let debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let status_bar_clone = status_bar.clone();
    let debounce_clone = debounce.clone();
    buffer.connect_changed(move |buf| {
        if let Some(id) = debounce_clone.borrow_mut().take() {
            id.remove();
        }
        let text = buf.text(&buf.start_iter(), &buf.end_iter(), false).to_string();
        let status_bar = status_bar_clone.clone();
        let debounce_inner = debounce_clone.clone();
        let id = glib::timeout_add_local(Duration::from_millis(DEBOUNCE_MS), move || {
            status_bar.update_document(&text);
            *debounce_inner.borrow_mut() = None;
            glib::ControlFlow::Break
        });
        *debounce_clone.borrow_mut() = Some(id);
    });
    status_bar.update_document("");

    let selection_debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let status_bar_clone = status_bar.clone();
    buffer.connect_mark_set(move |buf, _iter, _mark| {
        if let Some(id) = selection_debounce.borrow_mut().take() {
            id.remove();
        }
        let selection = buf.selection_bounds().map(|(start, end)| buf.text(&start, &end, false).to_string());
        let status_bar = status_bar_clone.clone();
        let selection_debounce_inner = selection_debounce.clone();
        let id = glib::timeout_add_local(Duration::from_millis(SELECTION_DEBOUNCE_MS), move || {
            status_bar.update_selection(selection.as_deref());
            *selection_debounce_inner.borrow_mut() = None;
            glib::ControlFlow::Break
        });
        *selection_debounce.borrow_mut() = Some(id);
    });
}

const SCROLL_SYNC_THROTTLE_MS: u64 = 60;

/// How long the editor->preview direction ignores the editor's own
/// `vadjustment` after the *preview->editor* direction just moved it - long
/// enough to absorb `scroll_to_iter`'s layout settling, short enough that a
/// genuine user scroll starting right after is never mistaken for the echo
/// of a sync that was already applied. See `wire_scroll_sync`'s doc comment
/// for why each direction needs its own echo guard.
const SCROLL_SYNC_ECHO_GUARD_MS: u64 = 250;

/// Wires scroll-sync in both directions between the editor and the preview.
///
/// Editor -> preview: on every editor scroll, finds the source line
/// currently at the top of the editor's viewport and asks the preview (via
/// `preview::render_html`'s embedded `scrollToLine`) to bring the block
/// starting at or before that line to its own top. The line lookup goes
/// through the real widget (`TextView::visible_rect` + `line_at_y`) rather
/// than a scroll-fraction-times-line-count estimate - the editor has word
/// wrap enabled (`editor::build`'s `WrapMode::WordChar`), so a document
/// mixing long wrapped paragraphs with short lines has no fixed
/// pixels-per-line ratio a fraction-based estimate could rely on;
/// `line_at_y` asks GTK's own layout directly, sidestepping that entirely.
/// (An earlier attempt at pixel-based lookup via `TextView::iter_at_location`
/// at `x=0` ran into that call unreliably returning `None` once the
/// line-number gutter is showing - `line_at_y` takes no `x` at all, so that
/// specific failure mode doesn't apply here.)
///
/// Preview -> editor: the rendered page's own `scroll` listener (see
/// `render_html`'s script) reports the source line nearest the top of the
/// preview's viewport back through `PreviewPane::connect_scroll`, and this
/// scrolls the editor to show that same line at its own top
/// (`TextView::scroll_to_iter`).
///
/// Each direction's *own* programmatic scroll would otherwise immediately
/// trigger the *other* direction's listener, which re-syncs back, which
/// re-triggers the first again - an infinite echo. Both directions guard
/// against this, but differently, matched to how each round-trip actually
/// happens: the preview's own script sets `__suppressScrollEcho` around
/// every scroll *it* performs (a same-document, synchronous-enough JS
/// concern, so a short timeout-based flag inside the page itself is
/// simplest); the editor side instead uses `ignore_editor_scroll_until`
/// here, set right before `sync_preview_to_editor` moves the editor, since
/// the trigger for the echo (`vadjustment`'s `value-changed`) fires on the
/// Rust side, not inside the WebView.
///
/// Both directions are throttled (not debounced): a debounce only fires
/// once scrolling has *stopped*, so the other side sits frozen for the
/// whole gesture and then snaps to the final position - exactly the "jumps
/// instead of scrolling" symptom this was built to avoid. A throttle
/// instead fires at most once per interval *while* scrolling continues
/// (leading edge immediately, a single trailing-edge call queued for
/// whatever's left of the window so the final position is never dropped),
/// so each side visibly tracks the other the whole time. The preview's own
/// debounce for its outgoing messages lives in its script (`render_html`);
/// only the editor->preview leg needs a matching one here in Rust.
fn wire_scroll_sync(scroller: &gtk4::ScrolledWindow, view: &sourceview5::View, buffer: &sourceview5::Buffer, preview_pane: &Rc<preview::PreviewPane>) {
    let throttle_interval = Duration::from_millis(SCROLL_SYNC_THROTTLE_MS);
    let last_synced: Rc<Cell<Instant>> = Rc::new(Cell::new(Instant::now() - throttle_interval));
    let trailing: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let ignore_editor_scroll_until: Rc<Cell<Instant>> = Rc::new(Cell::new(Instant::now()));
    let scroller = scroller.clone();
    let view = view.clone();
    let buffer = buffer.clone();
    let preview_pane = preview_pane.clone();

    {
        let view = view.clone();
        let buffer = buffer.clone();
        let preview_pane = preview_pane.clone();
        let ignore_editor_scroll_until = ignore_editor_scroll_until.clone();
        scroller.vadjustment().connect_value_changed(move |_adjustment| {
            if Instant::now() < ignore_editor_scroll_until.get() {
                return;
            }
            let elapsed = last_synced.get().elapsed();
            if elapsed >= throttle_interval {
                if let Some(id) = trailing.borrow_mut().take() {
                    id.remove();
                }
                sync_editor_to_preview(&view, &buffer, &preview_pane);
                last_synced.set(Instant::now());
                return;
            }
            if trailing.borrow().is_some() {
                return;
            }
            let view = view.clone();
            let buffer = buffer.clone();
            let preview_pane = preview_pane.clone();
            let last_synced = last_synced.clone();
            let trailing_inner = trailing.clone();
            let id = glib::timeout_add_local(throttle_interval - elapsed, move || {
                sync_editor_to_preview(&view, &buffer, &preview_pane);
                last_synced.set(Instant::now());
                *trailing_inner.borrow_mut() = None;
                glib::ControlFlow::Break
            });
            *trailing.borrow_mut() = Some(id);
        });
    }

    // Same throttle shape as the scroll listener above, but its own
    // `last_synced`/`trailing` pair (not shared) - typing or moving the
    // cursor with the arrow keys never touches `vadjustment` unless it
    // also happens to scroll the view, so without this the preview only
    // ever followed where the editor *happened to be scrolled*, not where
    // the user was actually working if that was already on-screen (e.g.
    // typing in the middle of a tall visible paragraph). Cursor-driven and
    // scroll-driven syncs targeting the same line in quick succession are
    // harmless - the second call just re-confirms the first.
    {
        let buffer_for_cursor = buffer.clone();
        let preview_pane_for_cursor = preview_pane.clone();
        let ignore_editor_scroll_until = ignore_editor_scroll_until.clone();
        let cursor_last_synced: Rc<Cell<Instant>> = Rc::new(Cell::new(Instant::now() - throttle_interval));
        let cursor_trailing: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        buffer.connect_cursor_position_notify(move |_buffer| {
            if Instant::now() < ignore_editor_scroll_until.get() {
                return;
            }
            let elapsed = cursor_last_synced.get().elapsed();
            if elapsed >= throttle_interval {
                if let Some(id) = cursor_trailing.borrow_mut().take() {
                    id.remove();
                }
                sync_cursor_to_preview(&buffer_for_cursor, &preview_pane_for_cursor);
                cursor_last_synced.set(Instant::now());
                return;
            }
            if cursor_trailing.borrow().is_some() {
                return;
            }
            let buffer_for_cursor = buffer_for_cursor.clone();
            let preview_pane_for_cursor = preview_pane_for_cursor.clone();
            let cursor_last_synced = cursor_last_synced.clone();
            let cursor_trailing_inner = cursor_trailing.clone();
            let id = glib::timeout_add_local(throttle_interval - elapsed, move || {
                sync_cursor_to_preview(&buffer_for_cursor, &preview_pane_for_cursor);
                cursor_last_synced.set(Instant::now());
                *cursor_trailing_inner.borrow_mut() = None;
                glib::ControlFlow::Break
            });
            *cursor_trailing.borrow_mut() = Some(id);
        });
    }

    preview_pane.connect_scroll(move |line| {
        ignore_editor_scroll_until.set(Instant::now() + Duration::from_millis(SCROLL_SYNC_ECHO_GUARD_MS));
        sync_preview_to_editor(&view, &buffer, line);
    });
}

/// Maps the cursor's current line onto the preview - the same top/bottom
/// edge-snapping `sync_editor_to_preview` does, just keyed off where the
/// cursor actually is rather than the top of the visible viewport (see
/// `wire_scroll_sync`'s doc comment on why typing needs its own trigger).
fn sync_cursor_to_preview(buffer: &sourceview5::Buffer, preview_pane: &preview::PreviewPane) {
    let cursor_line = buffer.iter_at_mark(&buffer.get_insert()).line();
    let last_line = buffer.end_iter().line();
    if cursor_line >= last_line {
        preview_pane.scroll_to_edge(true);
    } else if cursor_line <= 0 {
        preview_pane.scroll_to_edge(false);
    } else {
        preview_pane.scroll_to_line(cursor_line + 1);
    }
}

/// Maps the editor's current scroll position onto the preview - `line_at_y`
/// against the top *and* bottom of the editor's own visible rect (see
/// `wire_scroll_sync`'s doc comment for why a real widget lookup beats a
/// scroll-fraction estimate), snapped to the preview's true top/bottom
/// instead whenever the visible range already reaches line 0 or the
/// buffer's last line.
///
/// Deliberately compares *logical* line numbers, not `vadjustment`'s pixel
/// `value`/`upper`/`page_size` - `GtkTextView` only validates the exact
/// pixel height of lines near the viewport, so `upper()` is an *estimate*
/// for a document that hasn't been fully scrolled through yet, and stays
/// stale (too small) for a while after a big jump like Ctrl+End. Comparing
/// `value + page_size` against that stale `upper` undershot "are we at the
/// bottom" by a wide margin in exactly the case this whole check exists
/// for; the buffer's own line count doesn't have that problem.
fn sync_editor_to_preview(view: &sourceview5::View, buffer: &sourceview5::Buffer, preview_pane: &preview::PreviewPane) {
    let visible_rect = view.visible_rect();
    let (top_iter, _) = view.line_at_y(visible_rect.y());
    let (bottom_iter, _) = view.line_at_y(visible_rect.y() + visible_rect.height());
    if bottom_iter.line() >= buffer.end_iter().line() {
        preview_pane.scroll_to_edge(true);
        return;
    }
    if top_iter.line() <= 0 {
        preview_pane.scroll_to_edge(false);
        return;
    }
    preview_pane.scroll_to_line(top_iter.line() + 1);
}

/// `line` is either a real source line, or one of the two sentinels
/// `preview::PreviewPane::connect_scroll`'s doc comment describes (`-1`
/// top, `-2` bottom) - reported by the preview's own script when *it* is
/// at its true top/bottom, so the editor snaps to its real top/bottom too.
/// Scrolls the buffer's own start/end iter into view (`yalign` 0.0/1.0)
/// rather than setting `vadjustment`'s pixel value directly, for the same
/// "logical position, not a possibly-stale pixel estimate" reason
/// `sync_editor_to_preview` compares line numbers instead of `upper()`.
fn sync_preview_to_editor(view: &sourceview5::View, buffer: &sourceview5::Buffer, line: i32) {
    match line {
        -2 => view.scroll_to_iter(&mut buffer.end_iter(), 0.0, true, 0.0, 1.0),
        -1 => view.scroll_to_iter(&mut buffer.start_iter(), 0.0, true, 0.0, 0.0),
        _ => {
            let target_line = (line - 1).clamp(0, buffer.end_iter().line());
            let Some(mut iter) = buffer.iter_at_line(target_line) else { return };
            view.scroll_to_iter(&mut iter, 0.0, true, 0.0, 0.0)
        }
    };
}

fn wire_new_action(
    window: &adw::ApplicationWindow,
    buffer: &sourceview5::Buffer,
    current_path: &Rc<RefCell<Option<PathBuf>>>,
    frontmatter: &Rc<RefCell<Frontmatter>>,
    title: &adw::WindowTitle,
    preview_pane: &Rc<preview::PreviewPane>,
    saved_text: &Rc<RefCell<String>>,
) {
    // "new" starts a blank blog post, "new-page" a blank static WordPress
    // page - identical apart from the frontmatter's `post_type`.
    for (name, post_type) in [("new", PostType::Post), ("new-page", PostType::Page)] {
        let action = gio::SimpleAction::new(name, None);
        let buffer = buffer.clone();
        let current_path = current_path.clone();
        let frontmatter = frontmatter.clone();
        let title = title.clone();
        let preview_pane = preview_pane.clone();
        let saved_text = saved_text.clone();
        action.connect_activate(move |_, _| {
            buffer.set_text("");
            *current_path.borrow_mut() = None;
            *frontmatter.borrow_mut() = Frontmatter { post_type, ..Frontmatter::default() };
            title.set_subtitle(&match post_type {
                PostType::Post => tr("Unbenannt"),
                PostType::Page => tr("Unbenannte Seite"),
            });
            preview_pane.set_doc_dir(None);
            preview_pane.set_article_header(&frontmatter.borrow());
            *saved_text.borrow_mut() = String::new();
            autosave::clear();
        });
        window.add_action(&action);
    }
}

/// The handles almost every "open something new into the editor" action
/// needs, bundled purely to keep those functions' parameter counts down
/// (clippy::too_many_arguments) - the same fix already used for
/// `RecentFilesWidgets`. All fields are reference-counted/GObject handles,
/// so cloning the whole bundle is as cheap as cloning any one field.
#[derive(Clone)]
struct DocContext {
    buffer: sourceview5::Buffer,
    current_path: Rc<RefCell<Option<PathBuf>>>,
    frontmatter: Rc<RefCell<Frontmatter>>,
    title: adw::WindowTitle,
    toast_overlay: adw::ToastOverlay,
    preview_pane: Rc<preview::PreviewPane>,
    saved_text: Rc<RefCell<String>>,
}

fn wire_open_action(window: &adw::ApplicationWindow, ctx: &DocContext) {
    let action = gio::SimpleAction::new("open", None);
    let ctx = ctx.clone();
    let window_weak = window.downgrade();
    action.connect_activate(move |_, _| {
        let Some(window) = window_weak.upgrade() else {
            return;
        };

        let filter = gtk4::FileFilter::new();
        filter.add_suffix("md");
        filter.set_name(Some("Markdown"));
        let filters = gio::ListStore::new::<gtk4::FileFilter>();
        filters.append(&filter);

        let dialog = gtk4::FileDialog::builder()
            .title(tr("Markdown-Datei öffnen"))
            .filters(&filters)
            .build();

        let ctx = ctx.clone();
        dialog.open(Some(&window), gio::Cancellable::NONE, move |result| {
            let Ok(file) = result else { return };
            let Some(path) = file.path() else { return };
            open_document_at_path(path, &ctx);
        });
    });
    window.add_action(&action);
}

/// Backs `main.rs`'s `Gio::Application::connect_open` (the desktop
/// double-click/"Open With" path, now that the `.desktop` file declares
/// `MimeType=text/markdown;`): a plain string-parameter action so the
/// already-running primary instance can be told to open a path via
/// `window.activate_action("win.open-path", Some(&path.to_variant()))`,
/// the same GAction plumbing every other window-level command here already
/// uses, rather than reaching into the window for its private `DocContext`.
fn wire_open_path_action(window: &adw::ApplicationWindow, ctx: &DocContext) {
    let action = gio::SimpleAction::new("open-path", Some(&String::static_variant_type()));
    let ctx = ctx.clone();
    action.connect_activate(move |_, param| {
        let Some(path_str) = param.and_then(glib::Variant::str) else { return };
        open_document_at_path(PathBuf::from(path_str), &ctx);
    });
    window.add_action(&action);
}

/// Loads the article at `path` into the editor - shared by the "Öffnen"
/// file-dialog callback and every "Zuletzt geöffnet" popover row, since
/// both need to do exactly the same thing with a path once they have one.
/// Records the path into `recentfiles` on success, so opening the same
/// article twice keeps it at the front of that list rather than piling up
/// a duplicate entry.
fn open_document_at_path(path: PathBuf, ctx: &DocContext) {
    match document::read(&path) {
        Ok(doc) => {
            ctx.buffer.set_text(&doc.body);
            ctx.title.set_subtitle(&subtitle_for(Some(&path), &doc.frontmatter));
            *ctx.saved_text.borrow_mut() = doc.body.clone();
            *ctx.frontmatter.borrow_mut() = doc.frontmatter;
            let doc_dir = path.parent().map(Path::to_path_buf);
            let _ = recentfiles::record(&path);
            register_recent_file(&path);
            *ctx.current_path.borrow_mut() = Some(path);
            ctx.preview_pane.set_doc_dir(doc_dir);
            ctx.preview_pane.set_article_header(&ctx.frontmatter.borrow());
            autosave::clear();
        }
        Err(err) => show_toast(&ctx.toast_overlay, &tr("Öffnen fehlgeschlagen: {err}").replace("{err}", &err.to_string())),
    }
}

/// Registers `path` with GLib's shared `Gio::RecentManager` - the
/// system-wide "recently used" list GNOME Files' "Zuletzt verwendet" view
/// and other apps' own file-open dialogs read from, distinct from
/// `recentfiles`'s own private "Zuletzt geöffnet" popover list above.
/// Best-effort: a failure here (e.g. no recent-files store available)
/// isn't worth surfacing to the user over.
fn register_recent_file(path: &Path) {
    let uri = gio::File::for_path(path).uri();
    gtk4::RecentManager::default().add_item(&uri);
}

/// The three widgets making up the "Zuletzt geöffnet" popover - bundled
/// into one struct purely to keep `wire_recent_files_button`'s parameter
/// count down, since they're always constructed and passed together.
struct RecentFilesWidgets {
    button: gtk4::MenuButton,
    popover: gtk4::Popover,
    list: gtk4::ListBox,
}

/// Rebuilds the "Zuletzt geöffnet" popover's row list every time the button
/// becomes active (about to show its popover) - simpler than tracking
/// whether the list changed since it was last shown, and cheap enough that
/// rebuilding a ten-entry list on every click is not worth avoiding.
fn wire_recent_files_button(widgets: &RecentFilesWidgets, ctx: &DocContext) {
    let ctx = ctx.clone();
    let recent_list = widgets.list.clone();
    let recent_popover = widgets.popover.clone();
    widgets.button.connect_active_notify(move |button| {
        if !button.is_active() {
            return;
        }
        while let Some(child) = recent_list.first_child() {
            recent_list.remove(&child);
        }

        let entries = recentfiles::load();
        if entries.is_empty() {
            let row = adw::ActionRow::builder().title(tr("Keine zuletzt geöffneten Artikel")).activatable(false).build();
            recent_list.append(&row);
            return;
        }

        for path in entries {
            let filename = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.display().to_string());
            let parent = path.parent().map(|p| p.display().to_string()).unwrap_or_default();
            let row = adw::ActionRow::builder().title(filename).subtitle(parent).activatable(true).use_markup(false).build();
            {
                let ctx = ctx.clone();
                let recent_popover = recent_popover.clone();
                row.connect_activated(move |_| {
                    recent_popover.popdown();
                    open_document_at_path(path.clone(), &ctx);
                });
            }
            recent_list.append(&row);
        }
    });
}

fn wire_open_from_wordpress_action(
    window: &adw::ApplicationWindow,
    buffer: &sourceview5::Buffer,
    current_path: &Rc<RefCell<Option<PathBuf>>>,
    frontmatter: &Rc<RefCell<Frontmatter>>,
    title: &adw::WindowTitle,
    preview_pane: &Rc<preview::PreviewPane>,
    saved_text: &Rc<RefCell<String>>,
) {
    let action = gio::SimpleAction::new("open-from-wordpress", None);
    let buffer = buffer.clone();
    let current_path = current_path.clone();
    let frontmatter = frontmatter.clone();
    let title = title.clone();
    let preview_pane = preview_pane.clone();
    let saved_text = saved_text.clone();
    let window_weak = window.downgrade();
    action.connect_activate(move |_, _| {
        let Some(window) = window_weak.upgrade() else {
            return;
        };
        let buffer = buffer.clone();
        let current_path = current_path.clone();
        let frontmatter = frontmatter.clone();
        let title = title.clone();
        let preview_pane = preview_pane.clone();
        let saved_text = saved_text.clone();
        importer::open(&window, move |imported| {
            buffer.set_text(&imported.body);
            title.set_subtitle(&subtitle_for(None, &imported.frontmatter));
            // Baseline set to the just-imported text (not left stale, and
            // not cleared to "") so a crash with zero local edits since the
            // import doesn't manufacture a recovery snapshot for content
            // that's trivially re-importable from the same WordPress post.
            *saved_text.borrow_mut() = imported.body.clone();
            *frontmatter.borrow_mut() = imported.frontmatter;
            *current_path.borrow_mut() = None;
            preview_pane.set_doc_dir(None);
            preview_pane.set_article_header(&frontmatter.borrow());
            autosave::clear();
        });
    });
    window.add_action(&action);
}

fn wire_save_action(window: &adw::ApplicationWindow, ctx: &DocContext) {
    let action = gio::SimpleAction::new("save", None);
    let ctx = ctx.clone();
    let window_weak = window.downgrade();
    action.connect_activate(move |_, _| {
        let Some(window) = window_weak.upgrade() else {
            return;
        };
        let body = ctx.buffer.text(&ctx.buffer.start_iter(), &ctx.buffer.end_iter(), false).to_string();
        let doc = Document {
            frontmatter: ctx.frontmatter.borrow().clone(),
            body,
        };

        if let Some(path) = ctx.current_path.borrow().clone() {
            if let Err(err) = document::write(&path, &doc) {
                show_toast(&ctx.toast_overlay, &tr("Speichern fehlgeschlagen: {err}").replace("{err}", &err.to_string()));
            } else {
                *ctx.saved_text.borrow_mut() = doc.body.clone();
                autosave::clear();
            }
            return;
        }

        let dialog = gtk4::FileDialog::builder()
            .title(tr("Markdown-Datei speichern"))
            .initial_name("artikel.md")
            .build();

        let ctx = ctx.clone();
        dialog.save(Some(&window), gio::Cancellable::NONE, move |result| {
            let Ok(file) = result else { return };
            let Some(path) = file.path() else { return };
            if let Err(err) = document::write(&path, &doc) {
                show_toast(&ctx.toast_overlay, &tr("Speichern fehlgeschlagen: {err}").replace("{err}", &err.to_string()));
                return;
            }
            ctx.title.set_subtitle(&subtitle_for(Some(&path), &doc.frontmatter));
            let doc_dir = path.parent().map(Path::to_path_buf);
            let _ = recentfiles::record(&path);
            register_recent_file(&path);
            *ctx.current_path.borrow_mut() = Some(path);
            ctx.preview_pane.set_doc_dir(doc_dir);
            *ctx.saved_text.borrow_mut() = doc.body.clone();
            autosave::clear();
        });
    });
    window.add_action(&action);
}

fn wire_properties_action(
    window: &adw::ApplicationWindow,
    buffer: &sourceview5::Buffer,
    frontmatter: &Rc<RefCell<Frontmatter>>,
    term_caches: &termcache::TermCacheHandles,
    current_path: &Rc<RefCell<Option<PathBuf>>>,
    preview_pane: &Rc<preview::PreviewPane>,
) {
    let action = gio::SimpleAction::new("properties", None);
    let buffer = buffer.clone();
    let frontmatter = frontmatter.clone();
    let term_caches = term_caches.clone();
    let current_path = current_path.clone();
    let preview_pane = preview_pane.clone();
    let window_weak = window.downgrade();
    action.connect_activate(move |_, _| {
        if let Some(window) = window_weak.upgrade() {
            let doc_dir = current_path.borrow().as_ref().and_then(|p| p.parent().map(Path::to_path_buf));
            let body = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
            properties::open(&window, body, frontmatter.clone(), term_caches.clone(), doc_dir, preview_pane.clone());
        }
    });
    window.add_action(&action);
}

fn wire_settings_action(
    window: &adw::ApplicationWindow,
    buffer: &sourceview5::Buffer,
    ai_menu_handles: aimenu::AiMenuHandles,
    preview_pane: &Rc<preview::PreviewPane>,
    browser_view: &Rc<browser::BrowserView>,
) {
    let action = gio::SimpleAction::new("settings", None);
    let buffer = buffer.clone();
    let preview_pane = preview_pane.clone();
    let browser_view = browser_view.clone();
    let window_weak = window.downgrade();
    action.connect_activate(move |_, _| {
        if let Some(window) = window_weak.upgrade() {
            settings::open(&window, &buffer, &ai_menu_handles, &preview_pane, &browser_view);
        }
    });
    window.add_action(&action);
}

fn wire_about_action(window: &adw::ApplicationWindow) {
    let action = gio::SimpleAction::new("about", None);
    let window_weak = window.downgrade();
    action.connect_activate(move |_, _| {
        if let Some(window) = window_weak.upgrade() {
            about::open(&window);
        }
    });
    window.add_action(&action);
}

fn wire_publish_action(
    window: &adw::ApplicationWindow,
    buffer: &sourceview5::Buffer,
    current_path: &Rc<RefCell<Option<PathBuf>>>,
    frontmatter: &Rc<RefCell<Frontmatter>>,
    preview_pane: &Rc<preview::PreviewPane>,
    view_stack: &adw::ViewStack,
    browser_view: &Rc<browser::BrowserView>,
) {
    let action = gio::SimpleAction::new("publish", None);
    let buffer = buffer.clone();
    let current_path = current_path.clone();
    let frontmatter = frontmatter.clone();
    let preview_pane = preview_pane.clone();
    let view_stack = view_stack.clone();
    let browser_view = browser_view.clone();
    let window_weak = window.downgrade();
    action.connect_activate(move |_, _| {
        let Some(window) = window_weak.upgrade() else {
            return;
        };
        let body = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
        let doc_dir = current_path.borrow().as_ref().and_then(|p| p.parent().map(Path::to_path_buf));
        export::open(&window, body, frontmatter.clone(), doc_dir, preview_pane.clone(), &view_stack, &browser_view);
    });
    window.add_action(&action);
}

fn wire_insert_image_action(window: &adw::ApplicationWindow, buffer: &sourceview5::Buffer, current_path: &Rc<RefCell<Option<PathBuf>>>) {
    let action = gio::SimpleAction::new("insert-image", None);
    let buffer = buffer.clone();
    let current_path = current_path.clone();
    let window_weak = window.downgrade();
    action.connect_activate(move |_, _| {
        let Some(window) = window_weak.upgrade() else {
            return;
        };

        let filter = gtk4::FileFilter::new();
        filter.add_mime_type("image/*");
        filter.set_name(Some(&tr("Bilder")));
        let filters = gio::ListStore::new::<gtk4::FileFilter>();
        filters.append(&filter);

        let dialog = gtk4::FileDialog::builder().title(tr("Bild einfügen")).filters(&filters).build();

        let buffer = buffer.clone();
        let doc_dir = current_path.borrow().as_ref().and_then(|p| p.parent().map(Path::to_path_buf));
        dialog.open(Some(&window), gio::Cancellable::NONE, move |result| {
            let Ok(file) = result else { return };
            let Some(path) = file.path() else { return };
            let reference = document::image_reference(&path, doc_dir.as_deref());
            formatting::insert_image(&buffer, &reference);
        });
    });
    window.add_action(&action);
}

/// Same insertion mechanism as "Bild einfügen" - `formatting::insert_image`
/// just inserts a plain `![]()` reference regardless of file type, and
/// `crates/gutenberg` dispatches on the url's extension at export time (see
/// `as_lone_media`) - only the file-picker's filter differs here.
fn wire_insert_media_action(window: &adw::ApplicationWindow, buffer: &sourceview5::Buffer, current_path: &Rc<RefCell<Option<PathBuf>>>) {
    let action = gio::SimpleAction::new("insert-media", None);
    let buffer = buffer.clone();
    let current_path = current_path.clone();
    let window_weak = window.downgrade();
    action.connect_activate(move |_, _| {
        let Some(window) = window_weak.upgrade() else {
            return;
        };

        let filter = gtk4::FileFilter::new();
        filter.add_mime_type("video/*");
        filter.add_mime_type("audio/*");
        filter.set_name(Some("Video/Audio"));
        let filters = gio::ListStore::new::<gtk4::FileFilter>();
        filters.append(&filter);

        let dialog = gtk4::FileDialog::builder().title(tr("Video/Audio einfügen")).filters(&filters).build();

        let buffer = buffer.clone();
        let doc_dir = current_path.borrow().as_ref().and_then(|p| p.parent().map(Path::to_path_buf));
        dialog.open(Some(&window), gio::Cancellable::NONE, move |result| {
            let Ok(file) = result else { return };
            let Some(path) = file.path() else { return };
            let reference = document::image_reference(&path, doc_dir.as_deref());
            formatting::insert_image(&buffer, &reference);
        });
    });
    window.add_action(&action);
}

/// Opens the WordPress media-library picker (`medialibrary::open`) and
/// inserts the picked image the same way "Bild einfügen" does - then
/// immediately reconciles the body and patches the resulting `MediaItem`
/// with the picked item's real WordPress id/URL/alt text. Without this
/// patch, a plain `media::reconcile` pass would leave the new item's
/// `wordpress` field `None` (it only knows the image's Markdown reference,
/// not that it's already been uploaded), which would make Medienverwaltung's
/// "Zu WordPress hochladen"/"Alle hochladen" try to upload it again as if
/// it were a brand new local file.
fn wire_insert_media_library_action(window: &adw::ApplicationWindow, buffer: &sourceview5::Buffer, frontmatter: &Rc<RefCell<Frontmatter>>) {
    let action = gio::SimpleAction::new("insert-media-library", None);
    let buffer = buffer.clone();
    let frontmatter = frontmatter.clone();
    let window_weak = window.downgrade();
    action.connect_activate(move |_, _| {
        let Some(window) = window_weak.upgrade() else {
            return;
        };
        let buffer = buffer.clone();
        let frontmatter = frontmatter.clone();
        medialibrary::open(window.upcast_ref::<gtk4::Window>(), move |item| {
            formatting::insert_image(&buffer, &item.source_url);
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
            let mut fm = frontmatter.borrow_mut();
            fm.media = media::reconcile(&fm.media, &text);
            if let Some(media_item) = fm.media.iter_mut().find(|m| m.source == item.source_url) {
                media_item.wordpress = Some(media::WordPressMediaRef { media_id: item.id, url: item.source_url.clone(), content_hash: String::new() });
                if !item.alt_text.trim().is_empty() {
                    media_item.alt = media::AltText::Text(item.alt_text.clone());
                }
            }
        });
    });
    window.add_action(&action);
}

fn wire_insert_post_link_action(window: &adw::ApplicationWindow, buffer: &sourceview5::Buffer) {
    let action = gio::SimpleAction::new("insert-post-link", None);
    let buffer = buffer.clone();
    let window_weak = window.downgrade();
    action.connect_activate(move |_, _| {
        if let Some(window) = window_weak.upgrade() {
            linkpicker::open(&window, &buffer);
        }
    });
    window.add_action(&action);
}

/// Intercepts Ctrl+V on the editor view for two clipboard shapes
/// GtkSourceView's own paste handling can't do anything useful with on its
/// own:
///
/// - An image (a screenshot, or "Copy Image" from a browser - not a file
///   picked via a dialog, which already goes through
///   `wire_insert_image_action`) is saved as a new PNG file directly in the
///   article's own folder and inserted as a Markdown image reference -
///   GtkSourceView has no text form for an image at all and would just do
///   nothing.
/// - Rich text (formatted content copied from a browser, word processor,
///   or anywhere else that puts a `text/html` clipboard entry alongside
///   its plain-text one) is converted to Markdown (`richtext.rs`) and
///   inserted in its place - GtkSourceView's default paste only ever takes
///   the plain-text entry, silently dropping every bit of formatting.
///
/// An image takes priority when both are somehow present (matches how
/// "Copy Image" from a browser already behaves - most such copies don't
/// carry HTML at all). A clipboard with neither is left completely
/// untouched - `glib::Propagation::Proceed` lets the normal paste run.
fn wire_paste_shortcut(view: &sourceview5::View, buffer: &sourceview5::Buffer, current_path: &Rc<RefCell<Option<PathBuf>>>, toast_overlay: &adw::ToastOverlay) {
    let controller = gtk4::EventControllerKey::new();
    let buffer = buffer.clone();
    let current_path = current_path.clone();
    let toast_overlay = toast_overlay.clone();
    let view_weak = view.downgrade();
    controller.connect_key_pressed(move |_, key, _, state| {
        if key != gdk::Key::v || !state.contains(gdk::ModifierType::CONTROL_MASK) {
            return glib::Propagation::Proceed;
        }
        let Some(view) = view_weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let clipboard = view.clipboard();
        let mime_types = clipboard.formats().mime_types();

        if document::mime_types_contain_image(&mime_types) {
            let Some(doc_dir) = current_path.borrow().as_ref().and_then(|p| p.parent().map(Path::to_path_buf)) else {
                show_toast(&toast_overlay, &tr("Bitte den Artikel zuerst speichern, um Bilder einzufügen."));
                return glib::Propagation::Stop;
            };
            let buffer = buffer.clone();
            let toast_overlay = toast_overlay.clone();
            clipboard.read_texture_async(gio::Cancellable::NONE, move |result| {
                let texture = match result {
                    Ok(Some(texture)) => texture,
                    _ => {
                        show_toast(&toast_overlay, &tr("Bild konnte nicht aus der Zwischenablage gelesen werden."));
                        return;
                    }
                };
                let path = document::unique_pasted_image_path(&doc_dir, |p| p.exists());
                if let Err(err) = texture.save_to_png(&path) {
                    show_toast(&toast_overlay, &tr("Bild konnte nicht gespeichert werden: {err}").replace("{err}", &err.to_string()));
                    return;
                }
                let reference = document::image_reference(&path, Some(&doc_dir));
                formatting::insert_image(&buffer, &reference);
            });
            return glib::Propagation::Stop;
        }

        if mime_types.iter().any(|m| m.as_str() == "text/html") {
            let buffer = buffer.clone();
            let toast_overlay = toast_overlay.clone();
            clipboard.read_async(&["text/html"], glib::Priority::DEFAULT, gio::Cancellable::NONE, move |result| {
                let Ok((stream, _mime_type)) = result else {
                    show_toast(&toast_overlay, &tr("Formatierter Text konnte nicht aus der Zwischenablage gelesen werden."));
                    return;
                };
                let sink = gio::MemoryOutputStream::new_resizable();
                let buffer = buffer.clone();
                let toast_overlay = toast_overlay.clone();
                let sink_for_read = sink.clone();
                sink.splice_async(
                    &stream,
                    gio::OutputStreamSpliceFlags::CLOSE_SOURCE | gio::OutputStreamSpliceFlags::CLOSE_TARGET,
                    glib::Priority::DEFAULT,
                    gio::Cancellable::NONE,
                    move |result| {
                        if result.is_err() {
                            show_toast(&toast_overlay, &tr("Formatierter Text konnte nicht aus der Zwischenablage gelesen werden."));
                            return;
                        }
                        let Some(html) = richtext::decode_clipboard_html(&sink_for_read.steal_as_bytes()) else {
                            show_toast(&toast_overlay, &tr("Formatierter Text aus der Zwischenablage hat ein unbekanntes Format."));
                            return;
                        };
                        match richtext::html_to_markdown(&html) {
                            Ok(markdown) => formatting::insert_pasted_text(&buffer, &markdown),
                            Err(err) => show_toast(&toast_overlay, &tr("Formatierter Text konnte nicht umgewandelt werden: {err}").replace("{err}", &err)),
                        }
                    },
                );
            });
            return glib::Propagation::Stop;
        }

        glib::Propagation::Proceed
    });
    view.add_controller(controller);
}

/// Accepts one or more local files dropped onto the editor from a file
/// manager - same insertion mechanism as "Bild einfügen"/"Video/Audio
/// einfügen" and clipboard paste (`document::image_reference` +
/// `formatting::insert_image`), just triggered by a `Gtk.DropTarget`
/// instead of a picker dialog or Ctrl+V. Unlike the clipboard-paste case, no
/// new file needs to be written to disk, so this works even before the
/// article has ever been saved (`doc_dir` is simply `None` then, and
/// `image_reference` falls back to the file's absolute path).
fn wire_drop_target(view: &sourceview5::View, buffer: &sourceview5::Buffer, current_path: &Rc<RefCell<Option<PathBuf>>>) {
    let drop_target = gtk4::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    let buffer = buffer.clone();
    let current_path = current_path.clone();
    let view_weak = view.downgrade();
    drop_target.connect_drop(move |_target, value, x, y| {
        let Some(view) = view_weak.upgrade() else {
            return false;
        };
        let Ok(file_list) = value.get::<gdk::FileList>() else {
            return false;
        };
        let files = file_list.files();
        if files.is_empty() {
            return false;
        }

        let (buffer_x, buffer_y) = view.window_to_buffer_coords(gtk4::TextWindowType::Widget, x as i32, y as i32);
        if let Some((iter, _trailing)) = view.iter_at_position(buffer_x, buffer_y) {
            buffer.place_cursor(&iter);
        }

        let doc_dir = current_path.borrow().as_ref().and_then(|p| p.parent().map(Path::to_path_buf));
        for (index, file) in files.iter().enumerate() {
            let Some(path) = file.path() else { continue };
            if index > 0 {
                let mut iter = buffer.iter_at_mark(&buffer.get_insert());
                buffer.insert(&mut iter, "\n");
            }
            let reference = document::image_reference(&path, doc_dir.as_deref());
            formatting::insert_image(&buffer, &reference);
        }
        true
    });
    view.add_controller(drop_target);
}

/// Offers to restore a leftover autosave snapshot from a previous run - a
/// crash, or the app being quit without saving - found on launch. Declining
/// discards it outright; there's no "ask me again later", since the
/// snapshot itself is the only copy of that unsaved text and leaving it
/// around unresolved would just repeat the same prompt on every future
/// launch until it's dealt with one way or the other.
fn wire_startup_recovery(
    window: &adw::ApplicationWindow,
    buffer: &sourceview5::Buffer,
    current_path: &Rc<RefCell<Option<PathBuf>>>,
    frontmatter: &Rc<RefCell<Frontmatter>>,
    title: &adw::WindowTitle,
    preview_pane: &Rc<preview::PreviewPane>,
) {
    let Some(recovered) = autosave::recover() else {
        return;
    };
    let name = recovered
        .original_path
        .as_deref()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| tr("einem unbenannten Artikel"));
    let dialog = adw::AlertDialog::new(
        Some(&tr("Nicht gespeicherter Stand gefunden")),
        Some(
            &tr("Von „{name}“ wurde ein nicht gespeicherter Stand gefunden - vermutlich nach einem Absturz oder weil Blocksmith ohne zu speichern beendet wurde. Wiederherstellen?")
                .replace("{name}", &name),
        ),
    );
    dialog.add_response("discard", &tr("Verwerfen"));
    dialog.add_response("restore", &tr("Wiederherstellen"));
    dialog.set_response_appearance("restore", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("restore"));
    dialog.set_close_response("discard");

    let buffer = buffer.clone();
    let current_path = current_path.clone();
    let frontmatter = frontmatter.clone();
    let title = title.clone();
    let preview_pane = preview_pane.clone();
    dialog.connect_response(None, move |_, response| {
        if response != "restore" {
            autosave::clear();
            return;
        }
        buffer.set_text(&recovered.body);
        title.set_subtitle(&subtitle_for(recovered.original_path.as_deref(), &recovered.frontmatter));
        let doc_dir = recovered.original_path.as_deref().and_then(|p| p.parent().map(Path::to_path_buf));
        *frontmatter.borrow_mut() = recovered.frontmatter.clone();
        *current_path.borrow_mut() = recovered.original_path.clone();
        preview_pane.set_doc_dir(doc_dir);
        preview_pane.set_article_header(&frontmatter.borrow());
        // `saved_text` deliberately stays at its initial "" here: this
        // restored text is exactly the unsaved content the snapshot was
        // protecting, so it should read as dirty (and keep being
        // autosaved) until an explicit Save writes it out for real.
    });
    dialog.present(Some(window));
}

fn wire_find_action(window: &adw::ApplicationWindow, search_bar: &Rc<searchbar::SearchBar>) {
    let action = gio::SimpleAction::new("find", None);
    let search_bar = search_bar.clone();
    action.connect_activate(move |_, _| search_bar.open());
    window.add_action(&action);
}

fn wire_media_action(
    window: &adw::ApplicationWindow,
    buffer: &sourceview5::Buffer,
    current_path: &Rc<RefCell<Option<PathBuf>>>,
    frontmatter: &Rc<RefCell<Frontmatter>>,
    preview_pane: &Rc<preview::PreviewPane>,
) {
    let action = gio::SimpleAction::new("media-manager", None);
    let buffer = buffer.clone();
    let current_path = current_path.clone();
    let frontmatter = frontmatter.clone();
    let preview_pane = preview_pane.clone();
    let window_weak = window.downgrade();
    action.connect_activate(move |_, _| {
        let Some(window) = window_weak.upgrade() else {
            return;
        };
        let body = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
        let doc_dir = current_path.borrow().as_ref().and_then(|p| p.parent().map(Path::to_path_buf));
        mediapanel::open(&window, body, frontmatter.clone(), doc_dir, preview_pane.clone());
    });
    window.add_action(&action);
}
