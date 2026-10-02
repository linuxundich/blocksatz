//! The "WordPress" page of the Einstellungen (settings) dialog: the list
//! of blogs (`wpsite`) and a form for adding or editing one - site URL,
//! username and its Application Password (persisted via the Secret
//! Service, see `secrets`). Saving a blog makes it the active one.
//! Composed into the settings shell by `settings.rs`, which owns the
//! actual `Adw.PreferencesDialog`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

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

    // The connection check: runs after saving, or on demand.
    let check_icon = gtk4::Image::builder().visible(false).build();
    let check_spinner = adw::Spinner::builder().visible(false).build();
    let check_button = gtk4::Button::builder().label(tr("Prüfen")).valign(gtk4::Align::Center).build();
    let check_row = adw::ActionRow::builder().title(tr("Verbindung")).subtitle(tr("Noch nicht geprüft")).build();
    check_row.add_prefix(&check_icon);
    check_row.add_suffix(&check_spinner);
    check_row.add_suffix(&check_button);
    form_group.add(&check_row);

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

    let ui = Rc::new(Ui { sites_group, url_row, username_row, password_row, status_label, check_row, check_icon, check_spinner, check_button: check_button.clone(), rows: RefCell::new(Vec::new()), on_sites_changed });
    ui.rebuild_list();
    ui.edit(&wpsite::load());

    {
        let ui = ui.clone();
        save_button.connect_clicked(move |_| ui.save());
    }
    {
        let ui = ui.clone();
        check_button.connect_clicked(move |_| ui.check_connection());
    }
    page
}

struct Ui {
    sites_group: adw::PreferencesGroup,
    url_row: adw::EntryRow,
    username_row: adw::EntryRow,
    password_row: adw::PasswordEntryRow,
    status_label: gtk4::Label,
    check_row: adw::ActionRow,
    check_icon: gtk4::Image,
    check_spinner: adw::Spinner,
    check_button: gtk4::Button,
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
        self.show_check(CheckState::Unchecked);
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
            let stored = secrets::store_app_password(&site.url, &site.username, &password).await;
            match &stored {
                Ok(()) => ui.status(&tr("„{site}“ gespeichert und als aktives Blog gewählt.").replace("{site}", &site.site_id())),
                Err(err) => ui.status(&tr("Fehler beim Speichern des Passworts: {err}").replace("{err}", &err.to_string())),
            }
            ui.rebuild_list();
            (ui.on_sites_changed)();
            if stored.is_ok() {
                ui.check_connection();
            }
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

    /// Asks the blog who the entered credentials belong to (the form's
    /// current values, saved or not) and shows the outcome in the
    /// "Verbindung" row.
    fn check_connection(self: &Rc<Self>) {
        let url = self.url_row.text().trim().trim_end_matches('/').to_string();
        let username = self.username_row.text().trim().to_string();
        let password = self.password_row.text().to_string();
        if url.is_empty() || username.is_empty() || password.is_empty() {
            self.show_check(CheckState::Failed(tr("Website-URL, Benutzername und Application Password eintragen.")));
            return;
        }
        self.show_check(CheckState::Running);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::wpclient::Client::new(&url, &username, &password).current_user());
        });
        let ui = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
            Ok(result) => {
                if let Some(ui) = ui.upgrade() {
                    ui.show_check(match result {
                        Ok(user) => check_outcome(&user),
                        Err(err) => CheckState::Failed(explain_error(&err)),
                    });
                }
                glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
        });
    }

    fn show_check(&self, state: CheckState) {
        let running = matches!(state, CheckState::Running);
        self.check_spinner.set_visible(running);
        self.check_button.set_sensitive(!running);
        for class in ["success", "warning", "error"] {
            self.check_icon.remove_css_class(class);
        }
        let (icon, class, subtitle) = match state {
            CheckState::Unchecked => (None, None, tr("Noch nicht geprüft")),
            CheckState::Running => (None, None, tr("Verbindung wird geprüft …")),
            CheckState::Ok(text) => (Some("object-select-symbolic"), Some("success"), text),
            CheckState::Limited(text) => (Some("dialog-warning-symbolic"), Some("warning"), text),
            CheckState::Failed(text) => (Some("dialog-error-symbolic"), Some("error"), text),
        };
        self.check_icon.set_visible(icon.is_some());
        if let Some(icon) = icon {
            self.check_icon.set_icon_name(Some(icon));
        }
        if let Some(class) = class {
            self.check_icon.add_css_class(class);
        }
        self.check_row.set_subtitle(&subtitle);
    }

    fn status(&self, message: &str) {
        self.status_label.set_label(message);
        self.status_label.set_visible(true);
    }
}

enum CheckState {
    Unchecked,
    Running,
    Ok(String),
    /// Signed in, but without the rights Blocksatz needs for everything.
    Limited(String),
    Failed(String),
}

fn check_outcome(user: &crate::wpclient::CurrentUser) -> CheckState {
    let who = tr("Verbunden als {name}").replace("{name}", &user.name);
    match (user.can_publish, user.can_upload) {
        (true, true) => CheckState::Ok(tr("{who} – darf veröffentlichen und Medien hochladen").replace("{who}", &who)),
        (false, _) => CheckState::Limited(tr("{who}, darf aber nicht veröffentlichen – nur Entwürfe zur Prüfung").replace("{who}", &who)),
        (true, false) => CheckState::Limited(tr("{who}, darf aber keine Medien hochladen").replace("{who}", &who)),
    }
}

/// A connection error in words: what went wrong and what to check.
fn explain_error(err: &crate::wpclient::ApiError) -> String {
    match err.status {
        0 => tr("Blog nicht erreichbar: {err}").replace("{err}", &err.message),
        401 | 403 => tr("Anmeldung abgelehnt – Benutzername oder Application Password stimmen nicht."),
        404 => tr("Unter dieser Adresse gibt es keine WordPress-REST-API – stimmt die Website-URL?"),
        status if (200..300).contains(&status) => tr("Unter dieser Adresse antwortet kein WordPress – stimmt die Website-URL?"),
        status => tr("Fehler {status}: {err}").replace("{status}", &status.to_string()).replace("{err}", &err.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wpclient::{ApiError, CurrentUser};

    #[test]
    fn errors_say_what_to_check() {
        let err = |status| ApiError { status, message: "x".into() };
        assert!(explain_error(&err(0)).starts_with("Blog nicht erreichbar"));
        assert!(explain_error(&err(401)).contains("Application Password"));
        assert!(explain_error(&err(404)).contains("Website-URL"));
        assert!(explain_error(&err(200)).contains("kein WordPress"));
        assert!(explain_error(&err(500)).starts_with("Fehler 500"));
    }

    #[test]
    fn missing_rights_are_a_warning_not_a_failure() {
        let user = |can_publish, can_upload| CurrentUser { name: "Christoph".into(), can_publish, can_upload };
        assert!(matches!(check_outcome(&user(true, true)), CheckState::Ok(text) if text.contains("Christoph")));
        assert!(matches!(check_outcome(&user(false, true)), CheckState::Limited(_)));
        assert!(matches!(check_outcome(&user(true, false)), CheckState::Limited(_)));
    }
}
