//! The "Browser" page of the Einstellungen dialog - the Browser tab's
//! start page and its basic ad-blocker (see `browser.rs`/`adblock.rs`),
//! both persisted immediately on change like `appearance.rs`'s own
//! live-apply settings. No separate save button needed: these are a
//! plain string and a plain bool, no network call or secret involved,
//! unlike `connection.rs`'s Application Password field.

use std::rc::Rc;

use adw::prelude::*;

use crate::browser::{self, BrowserView};
use crate::i18n::tr;

/// `on_tab_toggled` shows or hides the "Browser" view of the right-hand
/// pane right away.
pub fn build_page(browser_view: &Rc<BrowserView>, on_tab_toggled: Rc<dyn Fn(bool)>) -> adw::PreferencesPage {
    let tab_row = adw::SwitchRow::builder().title(tr("Browser im Seitenbereich")).subtitle(tr("Ein eigener Browser neben Vorschau, Beitrag und Assistent – für Recherche und wp-admin.")).active(browser::tab_enabled()).build();
    let home_url_row = adw::EntryRow::builder().title(tr("Startseite")).text(browser::load_home_url().as_str()).build();

    let adblock_row = adw::SwitchRow::builder().title(tr("Werbung blockieren")).subtitle(tr("Blockiert bekannte Werbe- und Tracking-Domains, wie GNOME Web es tut.")).active(crate::adblock::is_enabled()).build();

    let group = adw::PreferencesGroup::builder().title(tr("Browser")).build();
    group.set_description(Some(&tr("Der Browser öffnet Links aus der Vorschau, die Linkprüfung und wp-admin.")));
    group.add(&tab_row);
    group.add(&home_url_row);
    group.add(&adblock_row);

    let page = adw::PreferencesPage::builder().title(tr("Browser")).icon_name("web-browser-symbolic").build();
    page.add(&group);

    tab_row.connect_active_notify(move |row| {
        browser::set_tab_enabled(row.is_active());
        on_tab_toggled(row.is_active());
    });

    home_url_row.connect_changed(move |row| {
        browser::save_home_url(&row.text());
    });

    {
        let browser_view = browser_view.clone();
        adblock_row.connect_active_notify(move |row| {
            let enabled = row.is_active();
            crate::adblock::set_enabled(enabled);
            browser_view.set_adblock_enabled(enabled);
        });
    }

    page
}
