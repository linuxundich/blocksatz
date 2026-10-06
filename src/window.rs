use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk4::{gdk, gio, glib};

use crate::document::{Document, Frontmatter, PostType};
use crate::i18n::tr;
use crate::{
    newarticle,
    blogposts, blogsync, counterpart, importer, langswitch, library, librarysidebar, mainaction, markdowncheck, postpane, releasecheck, syncstate, worksave,
    about, aievaluate, aiinplace, aimenu, aitasks, aiwriter, browser, chat, codeview, document, editor, export, formatting, gallerydialog, imagealt, linkpicker, media,
    mediabrowser, mediapanel, preview, recentfiles, richtext, searchbar, settings, shortcuts, stats, statusbar, termcache, themestyle, windowstate,
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
    // dragging an edge) become narrower than the toolbar's buttons
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
    // "Vorschau → Im Blog": the open article as the blog shows it.
    let blog_view = Rc::new(browser::BrowserView::new_blank());

    let view_stack = adw::ViewStack::new();
    // Homogeneous sizing (the default) makes the stack's minimum width the
    // max across ALL tabs, including hidden ones - so the Browser tab's
    // WebView alone would force a floor well above the "narrow" breakpoint
    // below, making it unreachable by resizing. Size to the visible tab
    // only instead.
    view_stack.set_hhomogeneous(false);
    // The right-hand pane (`docs/gui-redesign.md`, 5.5): three views
    // picked with one toggle group - Vorschau (rendered / Gutenberg code /
    // web), Beitrag (status and properties, `postpane.rs`) and Assistent
    // (chat / evaluation). One flat stack holds every page under its old
    // name, so code that shows e.g. "chat" or "browser" keeps working; the
    // toggle groups follow whatever page is visible.
    view_stack.add_named(&preview_pane.widget, Some("preview"));
    view_stack.add_named(&code_view.widget, Some("code"));
    view_stack.add_named(&blog_view.widget, Some("blog"));
    view_stack.add_named(&browser_view.widget, Some("browser"));
    let post_slot = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    view_stack.add_named(&post_slot, Some("post"));
    view_stack.add_named(&chat_view.widget, Some("chat"));
    view_stack.add_named(&evaluate_view.widget, Some("evaluate"));
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

    let toggle_group = |entries: &[(&str, String)]| {
        let group = adw::ToggleGroup::new();
        for (name, label) in entries {
            group.add(adw::Toggle::builder().name(*name).label(label.as_str()).build());
        }
        group
    };
    let section_toggles = toggle_group(&[("preview", tr("Vorschau")), ("post", tr("Beitrag")), ("assistant", tr("Assistent"))]);
    section_toggles.set_hexpand(true);
    // The free browser, switchable in Einstellungen → Browser.
    let browser_toggle = adw::Toggle::builder().name("browser").label(tr("Browser")).build();
    if browser::tab_enabled() {
        section_toggles.add(browser_toggle.clone());
    }
    let preview_toggles = toggle_group(&[("preview", tr("Gerendert")), ("code", tr("Code")), ("blog", tr("Im Blog"))]);
    let assistant_toggles = toggle_group(&[("chat", tr("Chat")), ("evaluate", tr("Bewertung"))]);
    for group in [&preview_toggles, &assistant_toggles] {
        group.add_css_class("flat");
        group.set_halign(gtk4::Align::Start);
        group.set_hexpand(true);
    }

    // Toggles the magazine-style article header (`preview::render_header`)
    // above the rendered body - only shown while the rendered preview is.
    let header_toggle_button = gtk4::ToggleButton::builder()
        .icon_name("document-properties-symbolic")
        .tooltip_text(tr("Artikel-Kopf in der Vorschau ein-/ausblenden"))
        .active(preview_pane.show_article_header())
        .build();
    header_toggle_button.add_css_class("flat");
    {
        let preview_pane = preview_pane.clone();
        header_toggle_button.connect_toggled(move |button| {
            preview_pane.set_show_article_header(button.is_active());
        });
    }

    let section_bar = gtk4::Box::builder().margin_top(6).margin_bottom(6).margin_start(6).margin_end(6).build();
    section_bar.append(&section_toggles);
    let sub_bar = gtk4::Box::builder().spacing(6).margin_bottom(6).margin_start(6).margin_end(6).build();
    sub_bar.append(&preview_toggles);
    sub_bar.append(&assistant_toggles);
    sub_bar.append(&header_toggle_button);

    // Which page each section last showed, to return to it.
    let last_preview_page = Rc::new(RefCell::new(String::from("preview")));
    let last_assistant_page = Rc::new(RefCell::new(String::from("chat")));
    // Set while the toggles follow the stack, so only a click on "Im Blog"
    // (not a page shown by code) loads the blog preview.
    let syncing = Rc::new(Cell::new(false));
    let sync_toggles = {
        let syncing = syncing.clone();
        let section_toggles = section_toggles.clone();
        let preview_toggles = preview_toggles.clone();
        let assistant_toggles = assistant_toggles.clone();
        let header_toggle_button = header_toggle_button.clone();
        let sub_bar = sub_bar.clone();
        let last_preview_page = last_preview_page.clone();
        let last_assistant_page = last_assistant_page.clone();
        move |page: &str| {
            syncing.set(true);
            let section = match page {
                "post" => "post",
                "browser" => "browser",
                counterpart::PAGE => counterpart::PAGE,
                "chat" | "evaluate" => "assistant",
                _ => "preview",
            };
            section_toggles.set_active_name(Some(section));
            preview_toggles.set_visible(section == "preview");
            assistant_toggles.set_visible(section == "assistant");
            sub_bar.set_visible(section == "preview" || section == "assistant");
            header_toggle_button.set_visible(page == "preview");
            match section {
                "preview" => {
                    preview_toggles.set_active_name(Some(page));
                    *last_preview_page.borrow_mut() = page.to_string();
                }
                "assistant" => {
                    assistant_toggles.set_active_name(Some(page));
                    *last_assistant_page.borrow_mut() = page.to_string();
                }
                _ => {}
            }
            syncing.set(false);
        }
    };
    sync_toggles("preview");
    view_stack.connect_visible_child_name_notify(move |stack| {
        if let Some(page) = stack.visible_child_name() {
            sync_toggles(&page);
        }
    });
    {
        let view_stack = view_stack.clone();
        section_toggles.connect_active_name_notify(move |group| {
            let page = match group.active_name().as_deref() {
                Some("post") => "post".to_string(),
                Some("browser") => "browser".to_string(),
                Some(counterpart::PAGE) => counterpart::PAGE.to_string(),
                Some("assistant") => last_assistant_page.borrow().clone(),
                _ => last_preview_page.borrow().clone(),
            };
            view_stack.set_visible_child_name(&page);
        });
    }
    for group in [&preview_toggles, &assistant_toggles] {
        let view_stack = view_stack.clone();
        let syncing = syncing.clone();
        group.connect_active_name_notify(move |group| {
            if let Some(page) = group.active_name().filter(|_| group.is_visible()) {
                view_stack.set_visible_child_name(&page);
                // Clicked "Im Blog": load the open article's blog preview.
                if page == "blog" && !syncing.get() {
                    let _ = group.activate_action("main.blog-preview", None);
                }
            }
        });
    }
    let set_browser_tab: Rc<dyn Fn(bool)> = {
        let section_toggles = section_toggles.clone();
        let browser_toggle = browser_toggle.clone();
        let view_stack = view_stack.clone();
        let present = Cell::new(browser::tab_enabled());
        Rc::new(move |enabled| {
            if enabled && !present.get() {
                section_toggles.add(browser_toggle.clone());
            } else if !enabled && present.get() {
                if view_stack.visible_child_name().as_deref() == Some("browser") {
                    view_stack.set_visible_child_name("preview");
                }
                section_toggles.remove(&browser_toggle);
            }
            present.set(enabled);
        })
    };

    let right_pane = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    right_pane.append(&section_bar);
    right_pane.append(&sub_bar);
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
        .build();
    // Keeps the split as a ratio - 50/50 to start, then whatever the user
    // drags it to - and reapplies it whenever the space the Paned gets
    // changes (window resized, library sidebar shown or hidden), instead
    // of a pixel position that only fits the width it was computed for.
    let split_ratio = Rc::new(Cell::new(saved_window_state.split_ratio));
    {
        let ratio = split_ratio.clone();
        let applying = Rc::new(Cell::new(false));
        {
            let ratio = ratio.clone();
            let applying = applying.clone();
            wide_paned.connect_max_position_notify(move |paned| {
                applying.set(true);
                paned.set_position((f64::from(paned.max_position()) * ratio.get()).round() as i32);
                applying.set(false);
            });
        }
        wide_paned.connect_position_notify(move |paned| {
            if !applying.get() && paned.max_position() > 0 {
                ratio.set((f64::from(paned.position()) / f64::from(paned.max_position())).clamp(0.15, 0.85));
            }
        });
    }
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
    narrow_view_stack.add_titled_with_icon(&adw::LayoutSlot::new("sidebar"), Some("sidebar"), &tr("Seitenbereich"), "sidebar-show-right-symbolic");
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
    // The editor, or in its place the translation start page of the
    // language switch (`langswitch.rs`).
    let editor_area = gtk4::Stack::builder().transition_type(gtk4::StackTransitionType::Crossfade).build();
    editor_area.add_named(&editor_pane, Some("editor"));
    layout_view.set_child("editor", &editor_area);
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
    // Below 860sp the library sidebar turns into an overlay. Only one
    // breakpoint applies at a time (the last matching one), so the narrow
    // breakpoint above repeats that setter.
    let sidebar_breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(adw::BreakpointConditionLengthType::MaxWidth, 860.0, adw::LengthUnit::Sp));
    let split_view = adw::OverlaySplitView::builder()
        .sidebar_position(gtk4::PackType::Start)
        .min_sidebar_width(240.0)
        .max_sidebar_width(320.0)
        .sidebar_width_unit(adw::LengthUnit::Sp)
        .build();
    sidebar_breakpoint.add_setter(&split_view, "collapsed", Some(&true.to_value()));
    narrow_breakpoint.add_setter(&split_view, "collapsed", Some(&true.to_value()));

    let title = adw::WindowTitle::new("Blocksatz", &tr("Unbenannt"));

    // New article, search and the primary menu live in the library
    // sidebar's own header bar (`librarysidebar.rs`); Save is gone since
    // articles are saved continuously (`worksave.rs`, Ctrl+S still works).
    let sidebar_toggle_button = gtk4::ToggleButton::builder().icon_name("sidebar-show-symbolic").build();
    sidebar_toggle_button.set_tooltip_text(Some(&tr("Seitenleiste ein-/ausblenden")));
    sidebar_toggle_button.set_action_name(Some("win.toggle-sidebar"));


    // The primary menu, shown in the sidebar's header bar. The editing
    // entries move into the formatting toolbar in a later redesign phase.
    let primary_menu = gio::Menu::new();
    let insert_section = gio::Menu::new();
    insert_section.append(Some(&tr("WordPress-Mediathek")), Some("win.media-library"));
    insert_section.append(Some(&tr("Galerie einfügen…")), Some("win.insert-gallery"));
    primary_menu.append_section(None, &insert_section);
    let app_section = gio::Menu::new();
    app_section.append(Some(&tr("Einstellungen")), Some("win.settings"));
    app_section.append(Some(&tr("Tastenkürzel")), Some("win.shortcuts"));
    app_section.append(Some(&tr("Über Blocksatz")), Some("win.about"));
    primary_menu.append_section(None, &app_section);

    let preview_toggle_button = gtk4::ToggleButton::builder().icon_name("sidebar-show-right-symbolic").active(true).build();
    preview_toggle_button.set_tooltip_text(Some(&tr("Seitenbereich ein-/ausblenden (F9)")));
    preview_toggle_button.set_action_name(Some("win.toggle-preview"));

    // No matching "exit" button by design: entering hides the whole header
    // bar (see `toggle-focus-mode` below), so the only way back is the same
    // shortcut - a button that vanishes along with the rest of the chrome
    // it just hid would be pointless to also draw.
    let focus_mode_toggle_button = gtk4::ToggleButton::builder().icon_name("view-fullscreen-symbolic").build();
    focus_mode_toggle_button.set_tooltip_text(Some(&tr("Fokus-Schreibmodus (Strg+Umschalt+F)")));
    focus_mode_toggle_button.set_action_name(Some("win.toggle-focus-mode"));

    // Filled with the main action (`mainaction.rs`) once the document
    // context exists; packed first so it ends up outermost on the right.
    let main_action_slot = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    // The language switch of a language pair (`langswitch.rs`), likewise.
    let lang_switch_slot = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);

    let header_bar = adw::HeaderBar::new();
    header_bar.set_title_widget(Some(&title));
    header_bar.pack_start(&sidebar_toggle_button);
    header_bar.pack_start(&lang_switch_slot);
    header_bar.pack_end(&main_action_slot);
    header_bar.pack_end(&preview_toggle_button);
    header_bar.pack_end(&focus_mode_toggle_button);

    let status_bar = Rc::new(statusbar::StatusBar::new());

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header_bar);
    toolbar_view.set_content(Some(&layout_view));
    toolbar_view.add_bottom_bar(&status_bar.widget);

    // Content area: the editor, with the blog archive page pushed on top
    // when a blog group is picked in the sidebar.
    let editor_page = adw::NavigationPage::builder().title(tr("Editor")).tag("editor").child(&toolbar_view).build();
    let nav_view = adw::NavigationView::new();
    nav_view.add(&editor_page);
    split_view.set_content(Some(&nav_view));

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&split_view));
    aitasks::set_toast_overlay(&toast_overlay);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Blocksatz")
        .default_width(saved_window_state.width)
        .default_height(saved_window_state.height)
        .maximized(saved_window_state.maximized)
        .content(&toast_overlay)
        .build();

    window.add_breakpoint(sidebar_breakpoint);
    window.add_breakpoint(narrow_breakpoint);

    {
        let split_ratio = split_ratio.clone();
        let split_view = split_view.clone();
        let right_pane = right_pane.clone();
        let view_stack = view_stack.clone();
        let saved_sidebar = saved_window_state.sidebar_visible;
        let (saved_width, saved_height) = (saved_window_state.width, saved_window_state.height);
        window.connect_close_request(move |window| {
            // A maximized window's size is the monitor's; keeping it as the
            // normal size would make the next start open too large for a
            // smaller monitor (which then drops the maximized state).
            let maximized = window.is_maximized();
            let (width, height) = if maximized { (saved_width, saved_height) } else { (window.width(), window.height()) };
            let state = windowstate::WindowState {
                width,
                height,
                maximized,
                split_ratio: split_ratio.get(),
                // In overlay mode (narrow window) the sidebar is hidden by
                // default; that says nothing about the wide-window choice.
                sidebar_visible: if split_view.is_collapsed() { saved_sidebar } else { split_view.shows_sidebar() },
                pane_visible: right_pane.is_visible(),
                // The other language's view only exists for a pair.
                pane_page: view_stack.visible_child_name().map(|n| n.to_string()).filter(|n| n != counterpart::PAGE).unwrap_or_else(|| "preview".into()),
            };
            let _ = windowstate::save(&state);
            glib::Propagation::Proceed
        });
    }

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
        let split_view = split_view.clone();
        let sidebar_before_focus = Rc::new(Cell::new(true));
        toggle_focus_mode_action.connect_activate(move |action, _| {
            let focus_mode = !action.state().and_then(|state| state.get::<bool>()).unwrap_or(false);
            action.set_state(&focus_mode.to_variant());
            if focus_mode {
                sidebar_before_focus.set(split_view.shows_sidebar());
                split_view.set_show_sidebar(false);
            } else {
                split_view.set_show_sidebar(sidebar_before_focus.get() && !split_view.is_collapsed());
            }
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
    // The body as currently on disk (or "" for a document without a file
    // yet) - `worksave.rs` uses it to tell whether a file from outside the
    // library was actually edited before writing to it.
    let saved_text: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));

    let cached_terms = termcache::load();
    let term_caches = termcache::TermCacheHandles {
        categories: Rc::new(RefCell::new(cached_terms.categories)),
        tags: Rc::new(RefCell::new(cached_terms.tags)),
        category_slugs: Rc::new(RefCell::new(cached_terms.category_slugs)),
    };
    termcache::spawn_refresh(&term_caches);
    themestyle::spawn_refresh();
    {
        let preview_pane = preview_pane.clone();
        themestyle::connect_changed(move || preview_pane.refresh());
    }

    let image_alt_menu = imagealt::install(&view, &buffer, frontmatter.clone(), current_path.clone(), preview_pane.clone());
    preview::PreviewPane::install_alt_text_menu(&preview_pane, &window, frontmatter.clone(), buffer.clone());
    preview::PreviewPane::install_image_edit_menu(&preview_pane, &window, frontmatter.clone(), buffer.clone());
    // Links: the free browser, or the system's browser when that's off.
    let open_in_browser: Rc<dyn Fn(String)> = {
        let browser_view = browser_view.clone();
        let view_stack = view_stack.clone();
        let right_pane = right_pane.clone();
        let window = window.downgrade();
        Rc::new(move |uri: String| {
            let Some(window) = window.upgrade() else { return };
            if !browser::tab_enabled() {
                gtk4::UriLauncher::new(&uri).launch(Some(&window), gio::Cancellable::NONE, |_| {});
                return;
            }
            if !right_pane.is_visible() {
                let _ = WidgetExt::activate_action(&window, "win.toggle-preview", None);
            }
            browser_view.load_uri(&uri);
            view_stack.set_visible_child_name("browser");
        })
    };
    {
        let open_in_browser = open_in_browser.clone();
        preview_pane.connect_link_clicked(move |uri| open_in_browser(uri.to_string()));
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
        written: Rc::new(RefCell::new(String::new())),
        library_listeners: Rc::new(RefCell::new(Vec::new())),
        remote: Rc::new(RefCell::new(HashMap::new())),
        blog_listeners: Rc::new(RefCell::new(Vec::new())),
        site_listeners: Rc::new(RefCell::new(Vec::new())),
        doc_generation: Rc::new(Cell::new(0)),
    };

    wire_live_preview(&buffer, &preview_pane, &stats_view, &code_view, &frontmatter);
    wire_scroll_sync(&editor_scroller, &view, &buffer, &preview_pane);
    wire_status_bar(&buffer, &status_bar);
    wire_new_action(&window, &doc_ctx);
    worksave::wire(&window, &doc_ctx);
    wire_open_action(&window, &doc_ctx);
    wire_ai_writer_action(&window, &doc_ctx);
    wire_open_path_action(&window, &doc_ctx);
    wire_save_action(&window, &doc_ctx);
    wire_library(&window, &doc_ctx, &split_view, &nav_view, &primary_menu, &open_in_browser);
    // Blog previews: "Vorschau → Im Blog".
    let open_preview_url: Rc<dyn Fn(String)> = {
        let blog_view = blog_view.clone();
        let view_stack = view_stack.clone();
        let right_pane = right_pane.clone();
        let window = window.downgrade();
        Rc::new(move |uri: String| {
            if !right_pane.is_visible() {
                if let Some(window) = window.upgrade() {
                    let _ = WidgetExt::activate_action(&window, "win.toggle-preview", None);
                }
            }
            blog_view.load_uri(&uri);
            view_stack.set_visible_child_name("blog");
        })
    };
    let post_pane = postpane::PostPane::new(&window, &doc_ctx, &term_caches, &stats_view.widget, open_in_browser.clone());
    post_slot.append(&post_pane.widget);
    let main_action = mainaction::MainAction::new(
        &window,
        &doc_ctx,
        open_preview_url,
        blog_view.clone(),
        releasecheck::LinkTarget { view_stack: view_stack.clone(), browser_view: browser_view.clone() },
    );
    main_action_slot.append(&main_action.button);
    let lang_switch = langswitch::LangSwitch::new(&window, &doc_ctx, &view, &editor_area);
    // The other language of the pair next to the editor.
    let counterpart = counterpart::Counterpart::new(&doc_ctx, &view, &editor_scroller, &view_stack, &section_toggles);
    {
        let open_in_browser = open_in_browser.clone();
        let weak = Rc::downgrade(&counterpart);
        counterpart.pane.connect_link_clicked(move |uri| {
            // The buttons above the original's changed sections.
            if !weak.upgrade().is_some_and(|c| c.handle_action(&uri)) {
                open_in_browser(uri.to_string());
            }
        });
    }
    lang_switch_slot.append(&lang_switch.widget);
    toolbar_view.add_top_bar(&main_action.banner);
    // Everything else only holds weak references to it; the window keeps
    // it alive.
    window.connect_destroy(move |_| {
        let _ = (&main_action, &post_pane, &lang_switch, &counterpart);
    });
    blogsync::wire(&window, &doc_ctx);
    // Another blog became active: its categories/tags and sync state.
    {
        let term_caches = term_caches.clone();
        let ctx = doc_ctx.clone();
        doc_ctx.site_listeners.borrow_mut().push(Rc::new(move || {
            termcache::reload(&term_caches);
            themestyle::reload();
            blogsync::refresh(&ctx);
        }));
    }

    // Restore the pane layout of the last session.
    split_view.set_show_sidebar(saved_window_state.sidebar_visible);
    // The pane's page only once the window is on screen: some pages (the
    // chat) ask for more width before the first layout than a maximized
    // window has, and the compositor then drops the maximized state.
    if view_stack.child_by_name(&saved_window_state.pane_page).is_some() {
        let view_stack = view_stack.clone();
        let page = saved_window_state.pane_page.clone();
        let restored = Cell::new(false);
        window.connect_map(move |_| {
            if !restored.replace(true) {
                let view_stack = view_stack.clone();
                let page = page.clone();
                glib::idle_add_local_once(move || view_stack.set_visible_child_name(&page));
            }
        });
    }
    if !saved_window_state.pane_visible {
        let _ = WidgetExt::activate_action(&window, "win.toggle-preview", None);
    }
    wire_properties_action(&window, &view_stack, &right_pane);
    let on_sites_changed: Rc<dyn Fn()> = {
        let ctx = doc_ctx.clone();
        Rc::new(move || ctx.notify_site_changed())
    };
    wire_settings_action(&window, &buffer, ai_menu_handles, &preview_pane, &browser_view, set_browser_tab.clone(), on_sites_changed);
    wire_about_action(&window);
    wire_media_action(&window, &buffer, &current_path, &frontmatter, &preview_pane);
    wire_insert_image_action(&window, &buffer, &current_path);
    wire_insert_media_action(&window, &buffer, &current_path);
    wire_insert_media_library_action(&window, &buffer, &frontmatter);
    wire_media_library_browser_action(&window, &buffer, &frontmatter);
    wire_insert_gallery_action(&window, &buffer, &frontmatter);
    wire_insert_post_link_action(&window, &buffer);
    wire_paste_shortcut(&view, &buffer, &current_path, &toast_overlay);
    wire_drop_target(&view, &buffer, &current_path);
    wire_find_action(&window, &search_bar);
    let shortcuts_action = gio::SimpleAction::new("shortcuts", None);
    {
        let window = window.downgrade();
        shortcuts_action.connect_activate(move |_, _| {
            if let Some(window) = window.upgrade() {
                shortcuts::open(&window);
            }
        });
    }
    window.add_action(&shortcuts_action);

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
    // Every library article is called `artikel.md` (or `artikel.en.md`);
    // its folder says more.
    let name_source = path.map(|p| if library::file_lang(p).is_some() { p.parent().unwrap_or(p) } else { p });
    name_source
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| tr("Unbenannt"))
}

fn wire_live_preview(
    buffer: &sourceview5::Buffer,
    preview_pane: &Rc<preview::PreviewPane>,
    stats_view: &Rc<stats::StatsView>,
    code_view: &Rc<codeview::CodeView>,
    frontmatter: &Rc<RefCell<Frontmatter>>,
) {
    let debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));

    let preview_pane_clone = preview_pane.clone();
    let stats_view_clone = stats_view.clone();
    let code_view_clone = code_view.clone();
    let frontmatter_clone = frontmatter.clone();
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
        scroller.vadjustment().connect_value_changed(move |adjustment| {
            // Reaching either end always syncs, even inside the echo guard:
            // a late report from the preview must not leave it short of the
            // top while the editor sits there.
            let at_edge = adjustment.value() <= 0.0 || adjustment.value() >= adjustment.upper() - adjustment.page_size() - 0.5;
            if (!at_edge && Instant::now() < ignore_editor_scroll_until.get()) || sync_pending.replace(true) {
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
    if adjustment.value() <= 0.0 {
        // The very top: the preview's top too, whatever its first block is.
        return (1.0, 1.0, 0.0);
    }
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

fn wire_new_action(window: &adw::ApplicationWindow, ctx: &DocContext) {
    // "new"/"new-page" open the "Neuer Artikel" dialog (`newarticle.rs`),
    // "new-from-file" asks for a text file first, "new-with-files" takes
    // dropped files. "new-blank" empties the editor without asking - for
    // closing a trashed article.
    for (name, post_type) in [("new", PostType::Post), ("new-page", PostType::Page)] {
        let action = gio::SimpleAction::new(name, None);
        let ctx = ctx.clone();
        let window_weak = window.downgrade();
        action.connect_activate(move |_, _| {
            let Some(window) = window_weak.upgrade() else { return };
            newarticle::present(&window, &ctx, post_type, Vec::new());
        });
        window.add_action(&action);
    }

    let action = gio::SimpleAction::new("new-from-file", None);
    let window_weak = window.downgrade();
    {
        let ctx = ctx.clone();
        action.connect_activate(move |_, _| {
            let Some(window) = window_weak.upgrade() else { return };
            newarticle::present_from_file(&window, &ctx);
        });
    }
    window.add_action(&action);

    let action = gio::SimpleAction::new("new-with-files", Some(&Vec::<String>::static_variant_type()));
    let window_weak = window.downgrade();
    {
        let ctx = ctx.clone();
        action.connect_activate(move |_, param| {
            let Some(window) = window_weak.upgrade() else { return };
            let paths: Vec<PathBuf> = param.and_then(|p| p.get::<Vec<String>>()).unwrap_or_default().into_iter().map(PathBuf::from).collect();
            newarticle::present(&window, &ctx, PostType::Post, paths);
        });
    }
    window.add_action(&action);

    let action = gio::SimpleAction::new("new-blank", None);
    {
        let ctx = ctx.clone();
        action.connect_activate(move |_, _| start_blank(&ctx, PostType::Post));
    }
    window.add_action(&action);
}

/// Empties the editor for a new, untitled article of `post_type`. The
/// article being replaced is saved first; the new one gets its library
/// folder once something is typed (`worksave.rs`).
pub(crate) fn start_blank(ctx: &DocContext, post_type: PostType) {
    worksave::flush(ctx, false);
    ctx.buffer.set_text("");
    *ctx.current_path.borrow_mut() = None;
    *ctx.frontmatter.borrow_mut() = Frontmatter { post_type, ..Frontmatter::default() };
    ctx.title.set_subtitle(&match post_type {
        PostType::Post => tr("Unbenannt"),
        PostType::Page => tr("Unbenannte Seite"),
    });
    ctx.preview_pane.set_doc_dir(None);
    ctx.preview_pane.set_article_header(&ctx.frontmatter.borrow());
    *ctx.saved_text.borrow_mut() = String::new();
    *ctx.written.borrow_mut() = String::new();
    ctx.bump_generation();
    ctx.notify_library(true);
}

/// Wires the library sidebar, the blog archive page and the actions that
/// navigate between them and the editor.
fn wire_library(window: &adw::ApplicationWindow, ctx: &DocContext, split_view: &adw::OverlaySplitView, nav_view: &adw::NavigationView, primary_menu: &gio::Menu, open_url: &Rc<dyn Fn(String)>) {
    let posts_page = blogposts::BlogPostsPage::new(
        {
            let ctx = ctx.clone();
            let nav_view = nav_view.clone();
            let open_url = open_url.clone();
            Rc::new(move |imported| {
                open_imported_post(&ctx, imported, &open_url);
                nav_view.pop_to_tag("editor");
            })
        },
        {
            let toast_overlay = ctx.toast_overlay.clone();
            Rc::new(move |message: &str| show_toast(&toast_overlay, message))
        },
        {
            let ctx = ctx.clone();
            Rc::new(move || ctx.notify_blog())
        },
    );

    // In overlay mode the sidebar gets out of the way once something was
    // picked in it.
    let hide_if_overlay = {
        let split_view = split_view.clone();
        move || {
            if split_view.is_collapsed() {
                split_view.set_show_sidebar(false);
            }
        }
    };
    let show_posts: Rc<dyn Fn(blogposts::BlogFilter)> = {
        let nav_view = nav_view.clone();
        let posts_page = posts_page.clone();
        let hide_if_overlay = hide_if_overlay.clone();
        Rc::new(move |filter| {
            posts_page.show(filter);
            if nav_view.visible_page().and_then(|page| page.tag()).as_deref() != Some("posts") {
                nav_view.push(&posts_page.page);
            }
            hide_if_overlay();
        })
    };
    // Translations from before language pairs move next to their original.
    let migrated = library::migrate_translations(&library::root());
    if migrated > 0 {
        show_toast(&ctx.toast_overlay, &tr("{n} Übersetzungen in den Ordner ihres Originals verschoben.").replace("{n}", &migrated.to_string()));
    }
    let sidebar = librarysidebar::LibrarySidebar::new(
        window,
        ctx,
        primary_menu.upcast_ref(),
        {
            let nav_view = nav_view.clone();
            Rc::new(move || {
                nav_view.pop_to_tag("editor");
                hide_if_overlay();
            })
        },
        show_posts.clone(),
    );
    split_view.set_sidebar(Some(&sidebar.widget));
    split_view.set_show_sidebar(true);
    {
        let sidebar = sidebar.clone();
        nav_view.connect_visible_page_notify(move |nav_view| {
            if nav_view.visible_page().and_then(|page| page.tag()).as_deref() == Some("editor") {
                sidebar.show_document();
            }
        });
    }

    let toggle_sidebar_action = gio::SimpleAction::new_stateful("toggle-sidebar", None, &split_view.shows_sidebar().to_variant());
    {
        let split_view = split_view.clone();
        toggle_sidebar_action.connect_activate(move |action, _| {
            let visible = !action.state().and_then(|state| state.get::<bool>()).unwrap_or(false);
            split_view.set_show_sidebar(visible);
        });
    }
    {
        let toggle_sidebar_action = toggle_sidebar_action.clone();
        split_view.connect_show_sidebar_notify(move |split_view| toggle_sidebar_action.set_state(&split_view.shows_sidebar().to_variant()));
    }
    window.add_action(&toggle_sidebar_action);

    // The archive page shows the previous blog's posts after a switch.
    {
        let nav_view = nav_view.clone();
        ctx.site_listeners.borrow_mut().push(Rc::new(move || {
            nav_view.pop_to_tag("editor");
        }));
    }

    // Ctrl+Shift+O: the blog's drafts, the usual place to pick up work.
    let open_from_wordpress_action = gio::SimpleAction::new("open-from-wordpress", None);
    {
        let sidebar = sidebar.clone();
        open_from_wordpress_action.connect_activate(move |_, _| {
            sidebar.show_filter(blogposts::BlogFilter::Drafts);
            show_posts(blogposts::BlogFilter::Drafts);
        });
    }
    // The sidebar and the archive page stay alive through the closures
    // above, which the window's actions and widgets own.
    window.add_action(&open_from_wordpress_action);
}

/// Opens a post fetched from WordPress through its working copy in the
/// library: an existing one for the same post is reopened as it is (it may
/// hold local changes - those must not be overwritten by the server copy),
/// otherwise a new library folder is created from `imported`.
pub(crate) fn open_imported_post(ctx: &DocContext, imported: importer::ImportedPost, open_url: &Rc<dyn Fn(String)>) {
    let root = library::root();
    let site_id = imported.frontmatter.wp_site.clone().unwrap_or_else(|| crate::wpsite::load().site_id());
    if let Some(existing) = imported.frontmatter.wp_post_id.and_then(|id| library::find_by_post_id(&root, &site_id, id)) {
        open_document_at_path(existing, ctx);
        show_toast(&ctx.toast_overlay, &tr("Vorhandene Arbeitskopie geöffnet."));
        return;
    }
    // More than plain Markdown: the editor points it out, and a heavily
    // designed post asks first whether wp-admin isn't the better place
    // (`docs/markdown-naehe.md`).
    let assessment = markdowncheck::assess(&imported.body);
    let mut imported = imported;
    imported.frontmatter.markdown_hint = assessment.closeness != gutenberg::Closeness::Plain;
    if assessment.closeness == gutenberg::Closeness::Heavy && markdowncheck::warn_enabled() {
        let admin_url = imported.frontmatter.wp_post_id.map(|id| format!("{}/wp-admin/post.php?post={id}&action=edit", crate::wpsite::for_site_id(Some(&site_id)).url.trim_end_matches('/')));
        let imported = Rc::new(RefCell::new(Some(imported)));
        let on_open: Rc<dyn Fn()> = {
            let ctx = ctx.clone();
            let imported = imported.clone();
            Rc::new(move || {
                if let Some(imported) = imported.borrow_mut().take() {
                    create_working_copy(&ctx, imported);
                }
            })
        };
        let on_admin: Rc<dyn Fn()> = {
            let open_url = open_url.clone();
            Rc::new(move || {
                if let Some(url) = &admin_url {
                    open_url(url.clone());
                }
            })
        };
        let parent = ctx.toast_overlay.root().map(|root| root.upcast::<gtk4::Widget>());
        markdowncheck::confirm_heavy_open(parent.as_ref(), &assessment, on_open, on_admin);
        return;
    }
    create_working_copy(ctx, imported);
}

/// A new library folder for a post fetched from the blog, opened.
fn create_working_copy(ctx: &DocContext, imported: importer::ImportedPost) {
    let root = library::root();
    let doc = Document { frontmatter: imported.frontmatter, body: imported.body };
    let title = (!doc.frontmatter.title.is_empty()).then_some(doc.frontmatter.title.as_str());
    let written = library::create_entry(&root, title, &library::untitled_name()).and_then(|path| document::write(&path, &doc).map(|()| path));
    match written {
        Ok(path) => open_document_at_path(path, ctx),
        Err(err) => show_toast(&ctx.toast_overlay, &tr("Arbeitskopie konnte nicht angelegt werden: {err}").replace("{err}", &err.to_string())),
    }
}

/// The handles almost every "open something new into the editor" action
/// needs, bundled purely to keep those functions' parameter counts down
/// (clippy::too_many_arguments) - the same fix already used for
/// `RecentFilesWidgets`. All fields are reference-counted/GObject handles,
/// so cloning the whole bundle is as cheap as cloning any one field.
/// `pub(crate)` so the library sidebar and `worksave.rs` share it.
/// Called with `structural` - see `DocContext::notify_library`.
pub(crate) type LibraryListener = Rc<dyn Fn(bool)>;
/// See `DocContext::notify_blog`.
pub(crate) type BlogListener = Rc<dyn Fn()>;

#[derive(Clone)]
pub(crate) struct DocContext {
    pub(crate) buffer: sourceview5::Buffer,
    pub(crate) current_path: Rc<RefCell<Option<PathBuf>>>,
    pub(crate) frontmatter: Rc<RefCell<Frontmatter>>,
    pub(crate) title: adw::WindowTitle,
    pub(crate) toast_overlay: adw::ToastOverlay,
    pub(crate) preview_pane: Rc<preview::PreviewPane>,
    pub(crate) saved_text: Rc<RefCell<String>>,
    /// The serialized document exactly as last written to (or read from)
    /// `current_path` - `worksave.rs` skips writing while nothing differs.
    pub(crate) written: Rc<RefCell<String>>,
    /// Everyone who shows the open article's state (library sidebar, main
    /// action); see `notify_library`.
    pub(crate) library_listeners: Rc<RefCell<Vec<LibraryListener>>>,
    /// What the last sync check (`blogsync.rs`) found on the server, by
    /// post id.
    pub(crate) remote: Rc<RefCell<HashMap<u64, syncstate::Remote>>>,
    /// Called after something changed on the blog itself (an upload, a
    /// post trashed or restored) - the sidebar's counters reload.
    pub(crate) blog_listeners: Rc<RefCell<Vec<BlogListener>>>,
    /// Called after the active blog changed (sidebar switcher, settings).
    pub(crate) site_listeners: Rc<RefCell<Vec<BlogListener>>>,
    /// Bumped whenever the editor gets another article (or the same one
    /// replaced wholesale), so views bound to the old one rebuild.
    pub(crate) doc_generation: Rc<Cell<u64>>,
}

impl DocContext {
    /// Tells the library sidebar the open article changed: `structural`
    /// when a library entry appeared, moved or another article was opened
    /// (rescan), otherwise just its text/state (update the row in place).
    pub(crate) fn notify_library(&self, structural: bool) {
        let listeners = self.library_listeners.borrow().clone();
        for listener in listeners {
            listener(structural);
        }
    }

    /// Marks that the editor now holds a different article.
    pub(crate) fn bump_generation(&self) {
        self.doc_generation.set(self.doc_generation.get() + 1);
    }

    /// The active blog changed: everything showing blog data reloads.
    pub(crate) fn notify_site_changed(&self) {
        let listeners = self.site_listeners.borrow().clone();
        for listener in listeners {
            listener();
        }
    }

    pub(crate) fn notify_blog(&self) {
        let listeners = self.blog_listeners.borrow().clone();
        for listener in listeners {
            listener();
        }
    }

    pub(crate) fn add_library_listener(&self, listener: LibraryListener) {
        self.library_listeners.borrow_mut().push(listener);
    }

    /// The server state last seen for the post behind `frontmatter`.
    pub(crate) fn remote_for(&self, frontmatter: &Frontmatter) -> syncstate::Remote {
        frontmatter.wp_post_id.and_then(|id| self.remote.borrow().get(&id).cloned()).unwrap_or(syncstate::Remote::Unknown)
    }

    /// The open article as it is in the editor right now.
    pub(crate) fn current_document(&self) -> Document {
        Document { frontmatter: self.frontmatter.borrow().clone(), body: self.buffer.text(&self.buffer.start_iter(), &self.buffer.end_iter(), false).to_string() }
    }
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
    worksave::flush(ctx, false);
    match document::read(&path) {
        Ok(doc) => {
            *ctx.written.borrow_mut() = document::serialize(&doc);
            ctx.buffer.set_text(&doc.body);
            ctx.title.set_subtitle(&subtitle_for(Some(&path), &doc.frontmatter));
            *ctx.saved_text.borrow_mut() = doc.body.clone();
            crate::editor::follow_language(doc.frontmatter.lang.as_deref().or(library::file_lang(&path).flatten().as_deref()));
            *ctx.frontmatter.borrow_mut() = doc.frontmatter;
            let doc_dir = path.parent().map(Path::to_path_buf);
            let _ = recentfiles::record(&path);
            register_recent_file(&path);
            *ctx.current_path.borrow_mut() = Some(path);
            ctx.preview_pane.set_doc_dir(doc_dir);
            ctx.preview_pane.set_article_header(&ctx.frontmatter.borrow());
            ctx.bump_generation();
            ctx.notify_library(true);
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

/// Ctrl+S: everything is saved continuously anyway (`worksave.rs`); this
/// just does it right now, including for an unedited file from outside
/// the library whose metadata changed.
fn wire_save_action(window: &adw::ApplicationWindow, ctx: &DocContext) {
    let action = gio::SimpleAction::new("save", None);
    let ctx = ctx.clone();
    action.connect_activate(move |_, _| {
        worksave::flush(&ctx, true);
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
        worksave::flush(&ctx, true);
    })
}

/// "Eigenschaften": the article's properties now live in the right-hand
/// pane's "Beitrag" view (`postpane.rs`) - show the pane and that view.
fn wire_properties_action(window: &adw::ApplicationWindow, view_stack: &adw::ViewStack, right_pane: &gtk4::Box) {
    let action = gio::SimpleAction::new("properties", None);
    let view_stack = view_stack.clone();
    let right_pane = right_pane.clone();
    let window_weak = window.downgrade();
    action.connect_activate(move |_, _| {
        if !right_pane.is_visible() {
            if let Some(window) = window_weak.upgrade() {
                let _ = WidgetExt::activate_action(&window, "win.toggle-preview", None);
            }
        }
        view_stack.set_visible_child_name("post");
    });
    window.add_action(&action);
}

fn wire_settings_action(
    window: &adw::ApplicationWindow,
    buffer: &sourceview5::Buffer,
    ai_menu_handles: aimenu::AiMenuHandles,
    preview_pane: &Rc<preview::PreviewPane>,
    browser_view: &Rc<browser::BrowserView>,
    set_browser_tab: Rc<dyn Fn(bool)>,
    on_sites_changed: Rc<dyn Fn()>,
) {
    let action = gio::SimpleAction::new("settings", None);
    let buffer = buffer.clone();
    let preview_pane = preview_pane.clone();
    let browser_view = browser_view.clone();
    let window_weak = window.downgrade();
    action.connect_activate(move |_, _| {
        if let Some(window) = window_weak.upgrade() {
            settings::open(&window, &buffer, &ai_menu_handles, &preview_pane, &browser_view, set_browser_tab.clone(), on_sites_changed.clone());
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
            let reference = document::adopt_file(&path, doc_dir.as_deref());
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
            let reference = document::adopt_file(&path, doc_dir.as_deref());
            formatting::insert_image(&buffer, &reference);
        });
    });
    window.add_action(&action);
}

/// "Aus WordPress-Mediathek …": the full media browser (`mediabrowser.rs`),
/// whose "In Artikel einfügen" takes images, videos and audio alike (see
/// `insert_wordpress_media`).
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
        let on_insert: Rc<dyn Fn(crate::wpclient::WpMediaEntry)> = Rc::new(move |entry| insert_wordpress_media(&buffer, &frontmatter, &entry));
        mediabrowser::open(&window, Some(on_insert));
    });
    window.add_action(&action);
}

/// Inserts a media library item: an image as `insert_wordpress_image`
/// does, a video or audio file as `![](url)` - the engine makes a video or
/// audio block of it by its file extension.
fn insert_wordpress_media(buffer: &sourceview5::Buffer, frontmatter: &Rc<RefCell<Frontmatter>>, entry: &crate::wpclient::WpMediaEntry) {
    if entry.media_type == "image" {
        insert_wordpress_image(buffer, frontmatter, entry.id, &entry.sizes, &entry.source_url, &entry.alt_text);
    } else {
        formatting::insert_image(buffer, &entry.source_url);
    }
}

/// Inserts an image that already lives in the WordPress media library -
/// shared by "Aus Mediathek wählen…" and the "WordPress-Mediathek"
/// browser's "In Artikel einfügen" (see `wire_insert_media_library_action`
/// for why the `MediaItem` gets patched right away).
fn insert_wordpress_image(buffer: &sourceview5::Buffer, frontmatter: &Rc<RefCell<Frontmatter>>, media_id: u64, sizes: &[crate::wpclient::ImageSize], source_url: &str, alt_text: &str) {
    // The "large" size, like WordPress's block editor (`for_article`).
    let reference = media::WordPressMediaRef::for_article(media_id, sizes, String::new()).unwrap_or(media::WordPressMediaRef { media_id, url: source_url.to_string(), content_hash: String::new(), width: 0, height: 0, size_slug: None });
    formatting::insert_image(buffer, &reference.url);
    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
    let mut fm = frontmatter.borrow_mut();
    fm.media = media::reconcile(&fm.media, &text);
    if let Some(media_item) = fm.media.iter_mut().find(|m| m.source == reference.url) {
        media_item.wordpress = Some(reference.clone());
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
                worksave::flush(&ctx, false);
                ctx.buffer.set_text(&article.body);
                *ctx.current_path.borrow_mut() = None;
                *ctx.frontmatter.borrow_mut() = Frontmatter { title: article.title.clone(), ..Frontmatter::default() };
                ctx.title.set_subtitle(&subtitle_for(None, &ctx.frontmatter.borrow()));
                ctx.preview_pane.set_doc_dir(None);
                ctx.preview_pane.set_article_header(&ctx.frontmatter.borrow());
                // Empty baseline: the generated text exists nowhere else
                // yet - `worksave.rs` gives it a library folder next tick.
                *ctx.saved_text.borrow_mut() = String::new();
                *ctx.written.borrow_mut() = String::new();
                ctx.bump_generation();
                ctx.notify_library(true);
                ctx.toast_overlay.add_toast(adw::Toast::new(&tr("KI-Entwurf als neues Dokument angelegt - bitte prüfen.")));
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
        let on_insert: Rc<dyn Fn(crate::wpclient::WpMediaEntry)> = Rc::new(move |entry| insert_wordpress_media(&buffer, &frontmatter, &entry));
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
                    media_item.wordpress = Some(media::WordPressMediaRef { media_id: *media_id, url: url.clone(), content_hash: String::new(), width: *width, height: *height, size_slug: None });
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
            let reference = document::adopt_file(&path, doc_dir.as_deref());
            formatting::insert_image(&buffer, &reference);
        }
        true
    });
    view.add_controller(drop_target);
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
