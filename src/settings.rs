//! "Einstellungen" (app settings, as opposed to `properties.rs`'s per-article
//! metadata): a standard `Adw.PreferencesDialog` shell holding one page per
//! concern - appearance (`appearance::build_page`), WordPress connection
//! (`connection::build_page`), the chat's LLM provider/model/system prompt
//! (`chatsettings::build_page`), the per-task AI model assignment and
//! capability check (`modelsettings::build_page`), and the editor context menu's AI prompts
//! (`promptsettings::build_page`). Further settings pages would just be
//! additional `dialog.add(...)` calls here.

use std::rc::Rc;

use adw::prelude::*;

use crate::aimenu::AiMenuHandles;
use crate::browser::BrowserView;
use crate::i18n::tr;
use crate::{appearance, browsersettings, chatsettings, connection, modelsettings, preview, promptsettings};

pub fn open(
    parent: &adw::ApplicationWindow,
    buffer: &sourceview5::Buffer,
    ai_menu_handles: &AiMenuHandles,
    preview_pane: &Rc<preview::PreviewPane>,
    browser_view: &Rc<BrowserView>,
    on_browser_tab_toggled: Rc<dyn Fn(bool)>,
    on_sites_changed: Rc<dyn Fn()>,
) {
    // Wide enough that the "Farbe" page's 4-per-line style-scheme grid
    // (matching GNOME Builder's own layout) isn't cramped at its default,
    // content-driven size.
    let dialog = adw::PreferencesDialog::builder().title(tr("Einstellungen")).content_width(1000).content_height(760).build();
    dialog.add(&appearance::build_page(buffer, preview_pane.clone()));
    dialog.add(&browsersettings::build_page(browser_view, on_browser_tab_toggled));
    dialog.add(&connection::build_page(on_sites_changed));
    dialog.add(&chatsettings::build_page());
    dialog.add(&modelsettings::build_page());
    dialog.add(&promptsettings::build_page(ai_menu_handles.custom_prompts_menu.clone()));
    dialog.present(Some(parent));
}
