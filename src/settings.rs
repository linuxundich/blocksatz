//! "Einstellungen" (app settings, as opposed to `properties.rs`'s per-article
//! metadata): a dialog with the pages as vertical tabs in a sidebar -
//! appearance (`appearance::build_page`), the browser, the WordPress
//! connection (`connection::build_page`), translation, the chat's LLM
//! provider/model/system prompt (`chatsettings::build_page`), the per-task
//! AI model assignment and capability check (`modelsettings::build_page`),
//! and the editor context menu's AI prompts (`promptsettings::build_page`).
//! Every page is a plain `Adw.PreferencesPage`; its title and icon become
//! the tab. In a narrow dialog the sidebar and the page become two steps
//! of an `Adw.NavigationSplitView`.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;

use crate::aimenu::AiMenuHandles;
use crate::browser::BrowserView;
use crate::i18n::tr;
use crate::{appearance, browsersettings, chatsettings, connection, modelsettings, preview, promptsettings};

thread_local! {
    /// The tab shown last, so reopening the dialog lands there again.
    static LAST_PAGE: Cell<usize> = const { Cell::new(0) };
}

pub fn open(
    parent: &adw::ApplicationWindow,
    ai_menu_handles: &AiMenuHandles,
    preview_pane: &Rc<preview::PreviewPane>,
    browser_view: &Rc<BrowserView>,
    on_browser_tab_toggled: Rc<dyn Fn(bool)>,
    on_sites_changed: Rc<dyn Fn()>,
) {
    let pages = [
        appearance::build_page(preview_pane.clone()),
        browsersettings::build_page(browser_view, on_browser_tab_toggled),
        connection::build_page(on_sites_changed),
        crate::translationsettings::build_page(),
        chatsettings::build_page(),
        modelsettings::build_page(),
        promptsettings::build_page(ai_menu_handles.custom_prompts_menu.clone()),
    ];

    let stack = gtk4::Stack::builder().transition_type(gtk4::StackTransitionType::Crossfade).build();
    let sidebar_list = gtk4::ListBox::new();
    sidebar_list.add_css_class("navigation-sidebar");
    for (index, page) in pages.iter().enumerate() {
        stack.add_named(page, Some(&index.to_string()));
        let row_box = gtk4::Box::builder().spacing(12).margin_top(6).margin_bottom(6).margin_start(6).margin_end(6).build();
        if let Some(icon) = page.icon_name() {
            row_box.append(&gtk4::Image::from_icon_name(&icon));
        }
        row_box.append(&gtk4::Label::builder().label(page.title()).xalign(0.0).ellipsize(gtk4::pango::EllipsizeMode::End).build());
        sidebar_list.append(&row_box);
    }

    let sidebar_scroller = gtk4::ScrolledWindow::builder().hscrollbar_policy(gtk4::PolicyType::Never).vexpand(true).child(&sidebar_list).build();
    let sidebar_view = adw::ToolbarView::new();
    sidebar_view.add_top_bar(&adw::HeaderBar::new());
    sidebar_view.set_content(Some(&sidebar_scroller));
    let sidebar_page = adw::NavigationPage::builder().title(tr("Einstellungen")).child(&sidebar_view).build();

    let content_view = adw::ToolbarView::new();
    content_view.add_top_bar(&adw::HeaderBar::new());
    content_view.set_content(Some(&stack));
    let content_page = adw::NavigationPage::builder().title(tr("Einstellungen")).child(&content_view).build();

    let split_view = adw::NavigationSplitView::builder()
        .sidebar(&sidebar_page)
        .content(&content_page)
        .min_sidebar_width(200.0)
        .max_sidebar_width(260.0)
        .show_content(true)
        .build();

    let dialog = adw::Dialog::builder().title(tr("Einstellungen")).content_width(1000).content_height(760).child(&split_view).build();
    let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(adw::BreakpointConditionLengthType::MaxWidth, 600.0, adw::LengthUnit::Sp));
    narrow.add_setter(&split_view, "collapsed", Some(&true.to_value()));
    dialog.add_breakpoint(narrow);

    {
        let stack = stack.clone();
        let content_page = content_page.clone();
        let split_view = split_view.clone();
        sidebar_list.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let index = row.index().max(0) as usize;
            stack.set_visible_child_name(&index.to_string());
            if let Some(page) = stack.visible_child().and_downcast::<adw::PreferencesPage>() {
                content_page.set_title(&page.title());
            }
            LAST_PAGE.with(|last| last.set(index));
            split_view.set_show_content(true);
        });
    }
    let start = LAST_PAGE.with(Cell::get).min(pages.len() - 1);
    sidebar_list.select_row(sidebar_list.row_at_index(start as i32).as_ref());

    dialog.present(Some(parent));
}
