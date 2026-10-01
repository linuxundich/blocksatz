use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk4::{gdk, gio, glib};

use crate::document::{Document, Frontmatter, PostType};
use crate::i18n::tr;
use crate::{
    about, aievaluate, aiinplace, aimenu, aitasks, aiwriter, autosave, browser, chat, codeview, docsidebar, document, editor, export, formatting, gallerydialog, imagealt, linkpicker, media,
    mediabrowser, medialibrary, mediapanel, preview, properties, recentfiles, richtext, searchbar, settings, shortcuts, stats, statusbar, termcache, windowstate,
};

const DEBOUNCE_MS: u64 = 250;

pub fn build(app: &adw::Application, initial_path: Option<PathBuf>) -> adw::ApplicationWindow {
    let saved_window_state = windowstate::load();

    let (editor_scroller, view, buffer, spelling_menu) = editor::build();
    // Moved up from its own original spot further down (still just as
    // valid there) - `EvaluateView::new` below needs it for the article
    // title, and every other reader of it already just clones an `Rc`
    // regardless of exactly where in this function it was created.
    let frontmatter: Rc<RefCell<Frontmatter>> = Rc::new(RefCell::new(Frontmatter::default()));
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
    let inplace_bar = aiinplace::InPlaceBar::new(&view, &buffer);
    editor_pane.append(&inplace_bar.widget);

    let chat_view = Rc::new(chat::ChatView::new(&buffer));
    let code_view = Rc::new(codeview::CodeView::new());
    let evaluate_view = Rc::new(aievaluate::EvaluateView::new(&view, &buffer, frontmatter.clone()));
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
    view_stack.add_titled_with_icon(&evaluate_view.widget, Some("evaluate"), &tr("Bewertung"), "edit-find-symbolic");
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
    // Cloned (a GObject reference, not a deep copy - both names point at
    // the same breakpoint) since `window.add_breakpoint` below takes
    // ownership of `narrow_breakpoint` itself; `docsidebar::build` needs to
    // add one more setter to this *same* breakpoint later, once the
    // sidebar's own `Adw.OverlaySplitView` exists, so both adaptive layers
    // (this one, and the sidebar's own collapse-to-overlay) agree on
    // exactly the same "narrow" width rather than fighting over two
    // separate breakpoints.
    let narrow_breakpoint_for_sidebar = narrow_breakpoint.clone();

    let title = adw::WindowTitle::new("Blocksatz", &tr("Unbenannt"));

    let new_button = gtk4::Button::from_icon_name("document-new-symbolic");
    new_button.set_tooltip_text(Some(&tr("Neu (Strg+N)")));
    new_button.set_action_name(Some("win.new"));

    // No headerbar button for "win.open" (it still exists, still has its
    // Ctrl+O shortcut, still used by the sidebar's own "Datei öffnen…"
    // row) - redundant with that row once the sidebar exists, per direct
    // user feedback after shipping the sidebar the first time.
    // Replaces the old "Zuletzt geöffnet" popover and "Von WordPress
    // öffnen" modal dialog buttons that used to sit here - both folded
    // into the sidebar's own "Durchsuchen" page (`docsidebar.rs`) instead,
    // along with the currently-open document's publish state ("Dokument"
    // page) that used to need a trip through the Eigenschaften dialog and
    // the export wizard. Wired to the stateful `win.toggle-sidebar` action
    // near `docsidebar::build`'s own call site below, the same
    // `set_action_name`-on-a-`ToggleButton` recipe `preview_toggle_button`
    // already uses.
    let sidebar_toggle_button = gtk4::ToggleButton::builder().icon_name("sidebar-show-symbolic").build();
    sidebar_toggle_button.set_tooltip_text(Some(&tr("Dokumentverwaltung ein-/ausblenden")));
    sidebar_toggle_button.set_action_name(Some("win.toggle-sidebar"));

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
    // GNOME hamburger icon for exactly this, and "Über Blocksatz" needs
    // *some* home now that it exists; Ctrl+, still opens Einstellungen
    // directly, since that's the action-level shortcut, independent of
    // how the button itself triggers it.
    let primary_menu = gio::Menu::new();
    let new_section = gio::Menu::new();
    new_section.append(Some(&tr("Neue Seite")), Some("win.new-page"));
    new_section.append(Some(&tr("WordPress-Mediathek")), Some("win.media-library"));
    new_section.append(Some(&tr("Galerie einfügen…")), Some("win.insert-gallery"));
    new_section.append(Some(&tr("KI-Artikel schreiben…")), Some("win.ai-write"));
    primary_menu.append_section(None, &new_section);
    let app_section = gio::Menu::new();
    app_section.append(Some(&tr("Einstellungen")), Some("win.settings"));
    app_section.append(Some(&tr("Tastenkürzel")), Some("win.show-help-overlay"));
    app_section.append(Some(&tr("Über Blocksatz")), Some("win.about"));
    primary_menu.append_section(None, &app_section);

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
    header_bar.pack_start(&sidebar_toggle_button);
    header_bar.pack_start(&new_button);
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
    // Content set later, once `docsidebar::build` has wrapped `layout_view`
    // in the sidebar's own `Adw.OverlaySplitView` - setting it here first
    // would parent `layout_view` into `toolbar_view` immediately, and
    // `Adw.OverlaySplitView`'s own `content` setter asserts its widget has
    // *no* parent yet (it doesn't reparent for you).
    toolbar_view.add_bottom_bar(&status_bar.widget);

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&toolbar_view));
    aitasks::set_toast_overlay(&toast_overlay);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Blocksatz")
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
    let ai_menu_handles = aimenu::install(&view, &buffer, &view_stack, chat_view.clone(), &spelling_menu, image_alt_menu.upcast_ref(), inplace_bar.clone());

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
    wire_ai_writer_action(&window, &doc_ctx);
    wire_open_path_action(&window, &doc_ctx);
    wire_save_action(&window, &doc_ctx);
    let doc_sidebar_extras = docsidebar::DocSidebarExtras { view_stack: view_stack.clone(), browser_view: browser_view.clone() };
    let doc_sidebar = docsidebar::build(&window, &doc_ctx, &doc_sidebar_extras, &layout_view);
    toolbar_view.set_content(Some(&doc_sidebar.split_view));
    narrow_breakpoint_for_sidebar.add_setter(&doc_sidebar.split_view, "collapsed", Some(&true.to_value()));
    let toggle_sidebar_action = gio::SimpleAction::new_stateful("toggle-sidebar", None, &false.to_variant());
    {
        let split_view = doc_sidebar.split_view.clone();
        toggle_sidebar_action.connect_activate(move |action, _| {
            let visible = !action.state().and_then(|state| state.get::<bool>()).unwrap_or(false);
            action.set_state(&visible.to_variant());
            split_view.set_show_sidebar(visible);
        });
    }
    window.add_action(&toggle_sidebar_action);
    let open_from_wordpress_action = gio::SimpleAction::new("open-from-wordpress", None);
    {
        let split_view = doc_sidebar.split_view.clone();
        let toggle_sidebar_action = toggle_sidebar_action.clone();
        let show_wordpress_posts = doc_sidebar.show_wordpress_posts.clone();
        open_from_wordpress_action.connect_activate(move |_, _| {
            toggle_sidebar_action.set_state(&true.to_variant());
            split_view.set_show_sidebar(true);
            show_wordpress_posts();
        });
    }
    window.add_action(&open_from_wordpress_action);
    wire_properties_action(&window, &buffer, &frontmatter, &term_caches, &current_path, &preview_pane);
    wire_settings_action(&window, &buffer, ai_menu_handles, &preview_pane, &browser_view);
    wire_about_action(&window);
    wire_publish_action(&window, &buffer, &current_path, &frontmatter, &preview_pane, &view_stack, &browser_view, document_saver(&doc_ctx));
    wire_media_action(&window, &buffer, &current_path, &frontmatter, &preview_pane);
    wire_insert_image_action(&window, &buffer, &current_path);
    wire_insert_media_action(&window, &buffer, &current_path);
    wire_insert_media_library_action(&window, &buffer, &frontmatter);
    wire_media_library_browser_action(&window, &buffer, &frontmatter);
    wire_insert_gallery_action(&window, &buffer, &frontmatter);
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

pub(crate) fn show_toast(overlay: &adw::ToastOverlay, message: &str) {
    overlay.add_toast(adw::Toast::new(message));
}

pub(crate) fn subtitle_for(path: Option<&Path>, frontmatter: &Frontmatter) -> String {
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

/// How long the editor->preview direction ignores the editor's own
/// `vadjustment` after the *preview->editor* direction last moved it. The
/// user is scrolling the preview during that time; the editor merely
/// follows, and its layout settling (GTK validating line heights as they
/// come into view) must not be sent back to fight the user's gesture.
const SCROLL_SYNC_ECHO_GUARD_MS: u64 = 200;

/// Wires scroll-sync in both directions between the editor and the preview.
///
/// Both directions exchange the same position: the *fractional* 1-based
/// source line at the top of the viewport (line 12 scrolled a third of the
/// way past = 12.33), plus two 0..1 blend factors for how far the viewport
/// is into the first/last screenful. The preview interpolates between its
/// block anchors for fractional lines (see `preview::render_html`'s
/// `syncTo`), so the two panes track each other continuously instead of the
/// preview holding still through a long paragraph and then jumping a whole
/// block; the bottom blend pulls the position toward the other pane's real
/// bottom over the last screenful, so the ends line up without a hard snap -
/// a tall image near the end makes the preview's remaining scroll range
/// disproportionate to the editor's, which a pure line mapping can't
/// absorb. The top blend only brings the preview's article header (above
/// the first block) into view; the editor has no such header, so the
/// reverse direction doesn't need it.
///
/// Editor -> preview: every `vadjustment` change schedules one sync for
/// the next main-loop idle (coalescing a burst of kinetic-scroll updates
/// into one per frame); the preview jumps there instantly - it follows at
/// frame rate, so an animated scroll per update would only stutter. The
/// line lookup goes through the real widget (`visible_rect`, `line_at_y`,
/// `line_yrange`) rather than a scroll fraction, because word wrap gives
/// the editor no fixed pixels-per-line ratio.
///
/// Preview -> editor: the page reports the user's own scrolling (at most
/// once per frame, and never for a scroll it performed itself - see
/// `render_html`'s `__programmaticY`), and the editor's `vadjustment` is
/// set to the matching pixel offset. For the reverse echo, the editor
/// direction is muted for `SCROLL_SYNC_ECHO_GUARD_MS` after each such move.
///
/// Moving the cursor deliberately doesn't sync on its own: the preview
/// follows what the editor *shows*, and the cursor is always inside that -
/// pulling the cursor's block to the preview's top on every click or
/// keystroke was the main source of the preview jumping around while the
/// editor itself stood still.
fn wire_scroll_sync(scroller: &gtk4::ScrolledWindow, view: &sourceview5::View, buffer: &sourceview5::Buffer, preview_pane: &Rc<preview::PreviewPane>) {
    let ignore_editor_scroll_until: Rc<Cell<Instant>> = Rc::new(Cell::new(Instant::now()));
    let sync_pending: Rc<Cell<bool>> = Rc::new(Cell::new(false));

    {
        let scroller_for_sync = scroller.clone();
        let view = view.clone();
        let buffer = buffer.clone();
        let preview_pane = preview_pane.clone();
        let ignore_editor_scroll_until = ignore_editor_scroll_until.clone();
        scroller.vadjustment().connect_value_changed(move |_adjustment| {
            if Instant::now() < ignore_editor_scroll_until.get() || sync_pending.replace(true) {
                return;
            }
            let scroller = scroller_for_sync.clone();
            let view = view.clone();
            let buffer = buffer.clone();
            let preview_pane = preview_pane.clone();
            let sync_pending = sync_pending.clone();
            glib::idle_add_local_once(move || {
                sync_pending.set(false);
                let (line, top_t, bottom_t) = editor_sync_position(&scroller, &view);
                preview_pane.sync_to(line, top_t, bottom_t, buffer.line_count());
            });
        });
    }

    let scroller = scroller.clone();
    let view = view.clone();
    let buffer = buffer.clone();
    preview_pane.connect_scroll(move |line, _top_t, bottom_t| {
        ignore_editor_scroll_until.set(Instant::now() + Duration::from_millis(SCROLL_SYNC_ECHO_GUARD_MS));
        scroll_editor_to(&scroller, &view, &buffer, line, bottom_t);
    });
}

/// How far (0..1) a viewport at `value` is into its first and its last
/// screenful - the blend factors `wire_scroll_sync` describes. A document
/// shorter than one screen is at both ends at once.
fn edge_blend(value: f64, page: f64, upper: f64) -> (f64, f64) {
    if page <= 0.0 {
        return (0.0, 0.0);
    }
    let remaining = (upper - value - page).max(0.0);
    let top_t = if value < page { 1.0 - value.max(0.0) / page } else { 0.0 };
    let bottom_t = if remaining < page { 1.0 - remaining / page } else { 0.0 };
    (top_t, bottom_t)
}

/// The editor's scroll position as `(fractional 1-based line at the top of
/// the viewport, top blend, bottom blend)`.
fn editor_sync_position(scroller: &gtk4::ScrolledWindow, view: &sourceview5::View) -> (f64, f64, f64) {
    let adjustment = scroller.vadjustment();
    let top_y = view.visible_rect().y();
    let (iter, line_top) = view.line_at_y(top_y);
    let (_, line_height) = view.line_yrange(&iter);
    let fraction = if line_height > 0 { (f64::from(top_y - line_top) / f64::from(line_height)).clamp(0.0, 1.0) } else { 0.0 };
    let (top_t, bottom_t) = edge_blend(adjustment.value(), adjustment.page_size(), adjustment.upper());
    (f64::from(iter.line()) + 1.0 + fraction, top_t, bottom_t)
}

/// The inverse of `editor_sync_position`: scrolls the editor so the
/// fractional source `line` sits at the top of its viewport, blended toward
/// its real bottom by `bottom_t`. (The preview's header region already maps
/// to line 1, i.e. the editor's top, so there's no top blend to undo.)
fn scroll_editor_to(scroller: &gtk4::ScrolledWindow, view: &sourceview5::View, buffer: &sourceview5::Buffer, line: f64, bottom_t: f64) {
    let adjustment = scroller.vadjustment();
    let whole = line.floor();
    let fraction = (line - whole).clamp(0.0, 1.0);
    let target_line = (whole as i32 - 1).clamp(0, buffer.end_iter().line());
    let Some(iter) = buffer.iter_at_line(target_line) else { return };
    let (line_y, line_height) = view.line_yrange(&iter);
    // `line_yrange` is in buffer coordinates; the adjustment's value can be
    // offset from those by the view's top margin.
    let offset = adjustment.value() - f64::from(view.visible_rect().y());
    let max = (adjustment.upper() - adjustment.page_size()).max(0.0);
    let mapped = f64::from(line_y) + fraction * f64::from(line_height) + offset;
    let blended = mapped * (1.0 - bottom_t) + max * bottom_t;
    adjustment.set_value(blended.clamp(0.0, max));
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
/// `pub(crate)` (struct and fields both) so `docsidebar.rs` can reuse this
/// directly rather than needing a second, field-for-field-identical bundle
/// kept in sync by hand.
#[derive(Clone)]
pub(crate) struct DocContext {
    pub(crate) buffer: sourceview5::Buffer,
    pub(crate) current_path: Rc<RefCell<Option<PathBuf>>>,
    pub(crate) frontmatter: Rc<RefCell<Frontmatter>>,
    pub(crate) title: adw::WindowTitle,
    pub(crate) toast_overlay: adw::ToastOverlay,
    pub(crate) preview_pane: Rc<preview::PreviewPane>,
    pub(crate) saved_text: Rc<RefCell<String>>,
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
pub(crate) fn open_document_at_path(path: PathBuf, ctx: &DocContext) {
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

        save_as(&window, &ctx, doc, || {});
    });
    window.add_action(&action);
}

/// Writes the document back to disk with whatever is currently in the
/// frontmatter and the buffer - handed to `export.rs` so a successful
/// publish can persist the `wp_post_id` and the per-image upload refs it
/// just received. Without a local path there is nothing to write to (a
/// document opened straight from WordPress and never saved locally), and
/// the call is a no-op.
pub(crate) fn document_saver(ctx: &DocContext) -> export::DocumentSaver {
    let ctx = ctx.clone();
    std::rc::Rc::new(move || {
        let Some(path) = ctx.current_path.borrow().clone() else {
            return;
        };
        let body = ctx.buffer.text(&ctx.buffer.start_iter(), &ctx.buffer.end_iter(), false).to_string();
        let doc = Document {
            frontmatter: ctx.frontmatter.borrow().clone(),
            body,
        };
        if let Err(err) = document::write(&path, &doc) {
            show_toast(&ctx.toast_overlay, &tr("Speichern fehlgeschlagen: {err}").replace("{err}", &err.to_string()));
            return;
        }
        *ctx.saved_text.borrow_mut() = doc.body.clone();
        autosave::clear();
    })
}

/// Prompts for a place on disk and writes `doc` there - the "no
/// `current_path` yet" half of `wire_save_action`'s own logic, pulled out
/// so `docsidebar.rs`'s "Lokal speichern unter…" row (shown exactly when
/// `current_path` is `None` - a brand new document, or one opened from
/// WordPress and never yet given a local copy) can trigger the same
/// dialog-and-write behavior as a plain `Ctrl+S` on such a document,
/// without duplicating it. `on_saved` fires only on an actual successful
/// write - `wire_save_action`'s own call site has nothing to react to and
/// passes a no-op; the sidebar passes its own `refresh()` so that row
/// disappears (and the rest of the "Dokument" page updates) the moment
/// `current_path` actually becomes `Some`.
pub(crate) fn save_as(window: &adw::ApplicationWindow, ctx: &DocContext, doc: Document, on_saved: impl Fn() + 'static) {
    let dialog = gtk4::FileDialog::builder()
        .title(tr("Markdown-Datei speichern"))
        .initial_name("artikel.md")
        .build();

    let ctx = ctx.clone();
    dialog.save(Some(window), gio::Cancellable::NONE, move |result| {
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
        on_saved();
    });
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

#[allow(clippy::too_many_arguments)]
fn wire_publish_action(
    window: &adw::ApplicationWindow,
    buffer: &sourceview5::Buffer,
    current_path: &Rc<RefCell<Option<PathBuf>>>,
    frontmatter: &Rc<RefCell<Frontmatter>>,
    preview_pane: &Rc<preview::PreviewPane>,
    view_stack: &adw::ViewStack,
    browser_view: &Rc<browser::BrowserView>,
    save_document: export::DocumentSaver,
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
        export::open(&window, body, frontmatter.clone(), doc_dir, preview_pane.clone(), &view_stack, &browser_view, save_document.clone());
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
            insert_wordpress_image(&buffer, &frontmatter, item.id, &item.source_url, &item.alt_text, 0, 0);
        });
    });
    window.add_action(&action);
}

/// Inserts an image that already lives in the WordPress media library -
/// shared by "Aus Mediathek wählen…" and the "WordPress-Mediathek"
/// browser's "In Artikel einfügen" (see `wire_insert_media_library_action`
/// for why the `MediaItem` gets patched right away).
fn insert_wordpress_image(buffer: &sourceview5::Buffer, frontmatter: &Rc<RefCell<Frontmatter>>, media_id: u64, source_url: &str, alt_text: &str, width: u64, height: u64) {
    formatting::insert_image(buffer, source_url);
    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
    let mut fm = frontmatter.borrow_mut();
    fm.media = media::reconcile(&fm.media, &text);
    if let Some(media_item) = fm.media.iter_mut().find(|m| m.source == source_url) {
        media_item.wordpress = Some(media::WordPressMediaRef { media_id, url: source_url.to_string(), content_hash: String::new(), width, height });
        if !alt_text.trim().is_empty() {
            media_item.alt = media::AltText::Text(alt_text.to_string());
        }
    }
}

/// Opens "KI-Artikel schreiben" (`aiwriter.rs`) - the result either
/// replaces the editor with a fresh, unsaved document (title taken from the
/// generated heading) or is inserted at the cursor.
fn wire_ai_writer_action(window: &adw::ApplicationWindow, ctx: &DocContext) {
    let action = gio::SimpleAction::new("ai-write", None);
    let ctx = ctx.clone();
    let window_weak = window.downgrade();
    action.connect_activate(move |_, _| {
        let Some(window) = window_weak.upgrade() else {
            return;
        };
        let ctx = ctx.clone();
        aiwriter::open(window.upcast_ref::<gtk4::Window>(), move |article, mode| match mode {
            aiwriter::ApplyMode::InsertAtCursor => {
                ctx.buffer.insert_at_cursor(&article.body);
            }
            aiwriter::ApplyMode::NewDocument => {
                ctx.buffer.set_text(&article.body);
                *ctx.current_path.borrow_mut() = None;
                *ctx.frontmatter.borrow_mut() = Frontmatter { title: article.title.clone(), ..Frontmatter::default() };
                ctx.title.set_subtitle(&subtitle_for(None, &ctx.frontmatter.borrow()));
                ctx.preview_pane.set_doc_dir(None);
                ctx.preview_pane.set_article_header(&ctx.frontmatter.borrow());
                // Empty baseline: the generated text exists nowhere else,
                // so it must count as unsaved (and be autosaved).
                *ctx.saved_text.borrow_mut() = String::new();
                ctx.toast_overlay.add_toast(adw::Toast::new(&tr("KI-Entwurf als neues Dokument angelegt - bitte prüfen und speichern.")));
            }
        });
    });
    window.add_action(&action);
}

/// Opens the full "WordPress-Mediathek" browser (`mediabrowser.rs`).
fn wire_media_library_browser_action(window: &adw::ApplicationWindow, buffer: &sourceview5::Buffer, frontmatter: &Rc<RefCell<Frontmatter>>) {
    let action = gio::SimpleAction::new("media-library", None);
    let buffer = buffer.clone();
    let frontmatter = frontmatter.clone();
    let window_weak = window.downgrade();
    action.connect_activate(move |_, _| {
        let Some(window) = window_weak.upgrade() else {
            return;
        };
        let buffer = buffer.clone();
        let frontmatter = frontmatter.clone();
        let on_insert: Rc<dyn Fn(crate::wpclient::WpMediaEntry)> = Rc::new(move |entry| {
            insert_wordpress_image(&buffer, &frontmatter, entry.id, &entry.source_url, &entry.alt_text, entry.width, entry.height);
        });
        mediabrowser::open(&window, Some(on_insert));
    });
    window.add_action(&action);
}

/// Opens "Galerie einfügen" (`gallerydialog.rs`) - inserts the fenced
/// ` ```gallery ``` ` block text at the cursor, then marks each image as
/// already-uploaded (same reasoning as `insert_wordpress_image`) so export
/// doesn't try to re-upload a file that's already sitting in the media
/// library.
fn wire_insert_gallery_action(window: &adw::ApplicationWindow, buffer: &sourceview5::Buffer, frontmatter: &Rc<RefCell<Frontmatter>>) {
    let action = gio::SimpleAction::new("insert-gallery", None);
    let buffer = buffer.clone();
    let frontmatter = frontmatter.clone();
    let window_weak = window.downgrade();
    action.connect_activate(move |_, _| {
        let Some(window) = window_weak.upgrade() else {
            return;
        };
        let buffer = buffer.clone();
        let frontmatter = frontmatter.clone();
        let on_insert: gallerydialog::OnInsertGallery = Rc::new(move |fenced, media_refs| {
            buffer.insert_at_cursor(&format!("\n\n{fenced}\n\n"));
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
            let mut fm = frontmatter.borrow_mut();
            fm.media = media::reconcile(&fm.media, &text);
            for (media_id, url, width, height) in &media_refs {
                if let Some(media_item) = fm.media.iter_mut().find(|m| &m.source == url) {
                    media_item.wordpress = Some(media::WordPressMediaRef { media_id: *media_id, url: url.clone(), content_hash: String::new(), width: *width, height: *height });
                }
            }
        });
        gallerydialog::open(&window, on_insert);
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
            &tr("Von „{name}“ wurde ein nicht gespeicherter Stand gefunden - vermutlich nach einem Absturz oder weil Blocksatz ohne zu speichern beendet wurde. Wiederherstellen?")
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
