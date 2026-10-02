//! The "WordPress" page of the Einstellungen (settings) dialog: the list
//! of blogs (`wpsite`) and a form for adding or editing one - site URL,
//! username and its Application Password (persisted via the Secret
//! Service, see `secrets`). Saving a blog makes it the active one.
//! Composed into the settings shell by `settings.rs`, which owns the
//! actual `Adw.PreferencesDialog`.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk4::glib;

use crate::i18n::tr;
use crate::{secrets, wpsite};

/// `on_sites_changed` runs after a blog was saved, removed or activated,
/// so the main window can reload what it shows.
pub fn build_page(on_sites_changed: Rc<dyn Fn()>) -> adw::PreferencesPage {
    let sites_group = adw::PreferencesGroup::builder().title(tr("Blogs")).build();
    sites_group.set_description(Some(&tr("Das aktive Blog ist das, dessen Beiträge Blocksatz anzeigt und in das neue Artikel hochgeladen werden.")));

    let url_row = adw::EntryRow::builder().title(tr("Website-URL")).build();
    let username_row = adw::EntryRow::builder().title(tr("Benutzername")).build();
    let password_row = adw::PasswordEntryRow::builder().title("Application Password").build();
    let save_button = gtk4::Button::builder().label(tr("Speichern")).valign(gtk4::Align::Center).build();
    save_button.add_css_class("suggested-action");
    let form_group = adw::PreferencesGroup::builder().title(tr("Blog hinzufügen oder bearbeiten")).build();
    form_group.set_description(Some(&tr("Zugangsdaten werden im Schlüsselbund gespeichert, nicht als Klartext in einer Datei.")));
    form_group.set_header_suffix(Some(&save_button));
    form_group.add(&url_row);
    form_group.add(&username_row);
    form_group.add(&password_row);

    let status_label = gtk4::Label::builder().xalign(0.0).wrap(true).visible(false).build();
    status_label.add_css_class("dim-label");
    let status_group = adw::PreferencesGroup::new();
    status_group.add(&status_label);

    let warn_row = adw::SwitchRow::builder()
        .title(tr("Vor stark gestalteten Beiträgen warnen"))
        .subtitle(tr("Fragt beim Öffnen aus dem Blog nach, wenn ein Beitrag sich hier großteils nur als WordPress-Markup bearbeiten ließe."))
        .active(crate::markdowncheck::warn_enabled())
        .build();
    warn_row.connect_active_notify(|row| crate::markdowncheck::set_warn_enabled(row.is_active()));
    let building_row = adw::EntryRow::builder().title(tr("Blog-Bausteine (zählen nicht als Markup)")).text(crate::markdowncheck::building_blocks_text().as_str()).build();
    building_row.connect_changed(|row| crate::markdowncheck::set_building_blocks_text(&row.text()));
    let import_group = adw::PreferencesGroup::builder().title(tr("Beiträge aus dem Blog")).build();
    import_group.set_description(Some(&tr("Blocksatz ist für Artikel gedacht, die in Markdown geschrieben sind. Blöcke wie Inhaltsverzeichnis oder Werbeplatz, die zu deinen Artikeln gehören, trägst du als Blog-Bausteine ein (Blocknamen, durch Kommas getrennt).")));
    import_group.add(&warn_row);
    import_group.add(&building_row);

    let page = adw::PreferencesPage::builder().title("WordPress").icon_name("network-server-symbolic").build();
    page.add(&sites_group);
    page.add(&form_group);
    page.add(&import_group);
    page.add(&status_group);

    let ui = Rc::new(Ui { sites_group, url_row, username_row, password_row, status_label, rows: RefCell::new(Vec::new()), on_sites_changed });
    ui.rebuild_list();
    ui.edit(&wpsite::load());

    {
        let ui = ui.clone();
        save_button.connect_clicked(move |_| ui.save());
    }
    page
}

struct Ui {
    sites_group: adw::PreferencesGroup,
    url_row: adw::EntryRow,
    username_row: adw::EntryRow,
    password_row: adw::PasswordEntryRow,
    status_label: gtk4::Label,
    rows: RefCell<Vec<adw::ActionRow>>,
    on_sites_changed: Rc<dyn Fn()>,
}

impl Ui {
    /// One row per blog: a radio button for the active one, the row itself
    /// loads the blog into the form, a button removes it.
    fn rebuild_list(self: &Rc<Self>) {
        for row in self.rows.borrow_mut().drain(..) {
            self.sites_group.remove(&row);
        }
        let sites = wpsite::load_all();
        let active = sites.active_site().site_id();
        let mut group_leader: Option<gtk4::CheckButton> = None;
        for site in &sites.sites {
            let id = site.site_id();
            let row = adw::ActionRow::builder().title(id.as_str()).subtitle(site.username.as_str()).activatable(true).use_markup(false).build();

            let radio = gtk4::CheckButton::builder().active(id == active).valign(gtk4::Align::Center).tooltip_text(tr("Als aktives Blog verwenden")).build();
            radio.set_group(group_leader.as_ref());
            group_leader.get_or_insert_with(|| radio.clone());
            {
                let ui = Rc::downgrade(self);
                let id = id.clone();
                radio.connect_toggled(move |radio| {
                    let Some(ui) = ui.upgrade().filter(|_| radio.is_active()) else { return };
                    if let Err(err) = wpsite::set_active(&id) {
                        ui.status(&tr("Fehler beim Speichern: {err}").replace("{err}", &err.to_string()));
                        return;
                    }
                    (ui.on_sites_changed)();
                });
            }
            row.add_prefix(&radio);

            let remove = gtk4::Button::builder().icon_name("user-trash-symbolic").valign(gtk4::Align::Center).tooltip_text(tr("Blog entfernen")).build();
            remove.add_css_class("flat");
            {
                let ui = Rc::downgrade(self);
                let id = id.clone();
                remove.connect_clicked(move |_| {
                    if let Some(ui) = ui.upgrade() {
                        ui.remove(&id);
                    }
                });
            }
            row.add_suffix(&remove);

            {
                let ui = Rc::downgrade(self);
                let site = site.clone();
                row.connect_activated(move |_| {
                    if let Some(ui) = ui.upgrade() {
                        ui.edit(&site);
                    }
                });
            }
            self.sites_group.add(&row);
            self.rows.borrow_mut().push(row);
        }
        if sites.sites.is_empty() {
            let row = adw::ActionRow::builder().title(tr("Noch kein Blog eingerichtet")).build();
            row.add_css_class("dim-label");
            self.sites_group.add(&row);
            self.rows.borrow_mut().push(row);
        }
    }

    /// Fills the form with `site`, its password from the keyring.
    fn edit(&self, site: &wpsite::SiteConfig) {
        self.url_row.set_text(&site.url);
        self.username_row.set_text(&site.username);
        self.password_row.set_text("");
        if site.url.is_empty() || site.username.is_empty() {
            return;
        }
        let password_row = self.password_row.clone();
        let status_label = self.status_label.clone();
        let (url, username) = (site.url.clone(), site.username.clone());
        glib::MainContext::default().spawn_local(async move {
            match secrets::load_app_password(&url, &username).await {
                Ok(Some(password)) => password_row.set_text(&password),
                Ok(None) => {}
                Err(err) => {
                    status_label.set_label(&tr("Passwort konnte nicht geladen werden: {err}").replace("{err}", &err.to_string()));
                    status_label.set_visible(true);
                }
            }
        });
    }

    fn save(self: &Rc<Self>) {
        let site = wpsite::SiteConfig { url: self.url_row.text().trim().to_string(), username: self.username_row.text().trim().to_string() };
        if site.url.is_empty() || site.username.is_empty() {
            self.status(&tr("Website-URL und Benutzername sind nötig."));
            return;
        }
        if let Err(err) = wpsite::save(&site) {
            self.status(&tr("Fehler beim Speichern: {err}").replace("{err}", &err.to_string()));
            return;
        }
        let password = self.password_row.text().to_string();
        let ui = self.clone();
        glib::MainContext::default().spawn_local(async move {
            match secrets::store_app_password(&site.url, &site.username, &password).await {
                Ok(()) => ui.status(&tr("„{site}“ gespeichert und als aktives Blog gewählt.").replace("{site}", &site.site_id())),
                Err(err) => ui.status(&tr("Fehler beim Speichern des Passworts: {err}").replace("{err}", &err.to_string())),
            }
            ui.rebuild_list();
            (ui.on_sites_changed)();
        });
    }

    /// Removes a blog from the list (its password stays in the keyring,
    /// the working copies in the library).
    fn remove(self: &Rc<Self>, site_id: &str) {
        let mut sites = wpsite::load_all();
        sites.remove(site_id);
        if let Err(err) = wpsite::save_all(&sites) {
            self.status(&tr("Fehler beim Speichern: {err}").replace("{err}", &err.to_string()));
            return;
        }
        self.status(&tr("„{site}“ entfernt.").replace("{site}", site_id));
        self.rebuild_list();
        self.edit(&sites.active_site());
        (self.on_sites_changed)();
    }

    fn status(&self, message: &str) {
        self.status_label.set_label(message);
        self.status_label.set_visible(true);
    }
}
