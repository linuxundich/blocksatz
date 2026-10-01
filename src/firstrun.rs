//! "Willkommen bei Blocksatz" first-run wizard: a carousel dialog shown
//! once, the very first time the app launches, collecting the WordPress
//! connection - the one setting almost every feature in this app already
//! gates on (`site.url.is_empty()` shows up in a dozen places), and the
//! only one without a sensible zero-config default (unlike Erscheinungsbild/
//! Sprache, which already just follow the system). Modeled on GNOME's own
//! welcome/tour screens, reusing the exact same `Adw.Carousel` + centered
//! dot indicator + Zurück/Weiter bottom bar shape `export.rs`'s "Artikel
//! exportieren" wizard already established for a linear step flow - see
//! that module's own `wizard_nav_bar` for the reasoning over
//! `Adw.NavigationView`. Not shared code with it directly: that bar is
//! sized/tuned for export's own three dense, functional steps, not this
//! wizard's shorter, StatusPage-flavored ones.
//!
//! Whether to show it at all is a single marker file under
//! `glib::user_config_dir()/blocksatz/` - this app's established plain-file
//! settings convention (`wpsite.rs`/`appearance.rs`), not `Gio.Settings`,
//! which nothing here uses. Written once the dialog closes, by "Fertig"
//! *or* "Überspringen" alike, so a deliberate skip doesn't re-show the
//! wizard on every future launch the way re-checking
//! `wpsite::load().url.is_empty()` on its own would.

use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk4::glib;

use crate::i18n::tr;
use crate::{secrets, wpclient, wpsite};

fn marker_path() -> std::path::PathBuf {
    let mut path = glib::user_config_dir();
    path.push(crate::APP_DIR);
    path.push("onboarding_done");
    path
}

/// `true` the very first time the app is ever launched (or after the
/// marker file was manually removed) - `open` marks it seen regardless of
/// whether the wizard's connection step was actually filled in and saved,
/// or skipped outright, so it never nags on a later launch either way.
///
/// Also `false` for a site that's already configured even without the
/// marker file present - someone upgrading from a version before this
/// wizard existed has already done exactly what it would ask for, so
/// showing it would just be re-asking a question that's already answered.
/// Marks it seen right away in that case too, so this check only ever
/// needs to actually read `wpsite::load()` once.
pub fn should_show() -> bool {
    if marker_path().exists() {
        return false;
    }
    if !wpsite::load().url.is_empty() {
        mark_seen();
        return false;
    }
    true
}

fn mark_seen() {
    if let Some(dir) = marker_path().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(marker_path(), "done = true\n");
}

/// Builds and presents the wizard over `parent` (already `.present()`-ed -
/// see `main.rs`): Willkommen, WordPress-Verbindung (the same fields/save
/// calls `connection.rs`'s settings page already uses, plus a "Verbindung
/// testen" step that page has never had), and a closing Fertig page.
pub fn open(parent: &adw::ApplicationWindow) {
    let config = wpsite::load();

    let welcome_page = adw::StatusPage::builder()
        .icon_name("de.linuxundich.Blocksatz")
        .title(tr("Willkommen bei Blocksatz"))
        .description(tr(
            "Schreibe Artikel in Markdown und veröffentliche sie direkt als native WordPress-Gutenberg-Blöcke - mit Live-Vorschau, KI-Unterstützung und allem, was ein Artikel sonst noch braucht.",
        ))
        .vexpand(true)
        .hexpand(true)
        .build();

    let url_row = adw::EntryRow::builder().title(tr("Website-URL")).text(config.url.as_str()).build();
    let username_row = adw::EntryRow::builder().title(tr("Benutzername")).text(config.username.as_str()).build();
    let password_row = adw::PasswordEntryRow::builder().title("Application Password").build();

    let connection_group = adw::PreferencesGroup::builder().title(tr("WordPress-Verbindung")).build();
    connection_group.set_description(Some(&tr(
        "Zugangsdaten werden im Schlüsselbund gespeichert, nicht als Klartext. Ein Application Password legst du im WordPress-Backend unter Profil → Anwendungspasswörter an.",
    )));
    connection_group.add(&url_row);
    connection_group.add(&username_row);
    connection_group.add(&password_row);

    let test_button = gtk4::Button::with_label(&tr("Verbindung testen und speichern"));
    test_button.add_css_class("suggested-action");
    test_button.set_halign(gtk4::Align::Start);

    let connection_status = gtk4::Label::builder().xalign(0.0).wrap(true).build();
    connection_status.add_css_class("dim-label");
    connection_status.set_visible(false);

    // `valign(Center)`, not `vexpand`, on the inner box - it's the
    // *scroller* around it that needs to fill the carousel page (see
    // above), but the content itself should sit centered in that space
    // like its Willkommen/Fertig StatusPage siblings do, not pinned to
    // the top with all the leftover room dumped below it. Still scrolls
    // normally top-down if the content ever needs more height than the
    // page has.
    let connection_content = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(12).valign(gtk4::Align::Center).build();
    connection_content.append(&connection_group);
    connection_content.append(&test_button);
    connection_content.append(&connection_status);
    let connection_scroller = gtk4::ScrolledWindow::builder().child(&connection_content).hscrollbar_policy(gtk4::PolicyType::Never).vexpand(true).hexpand(true).build();
    connection_scroller.set_margin_top(24);
    connection_scroller.set_margin_bottom(24);
    connection_scroller.set_margin_start(24);
    connection_scroller.set_margin_end(24);

    let finish_button = gtk4::Button::with_label(&tr("Los geht's"));
    finish_button.add_css_class("suggested-action");
    finish_button.add_css_class("pill");
    let done_page = adw::StatusPage::builder()
        .icon_name("object-select-symbolic")
        .title(tr("Bereit."))
        .description(tr("Die Verbindung lässt sich jederzeit in den Einstellungen ändern."))
        .child(&finish_button)
        .vexpand(true)
        .hexpand(true)
        .build();

    let pages: Vec<gtk4::Widget> = vec![welcome_page.clone().upcast(), connection_scroller.clone().upcast(), done_page.clone().upcast()];
    let carousel = adw::Carousel::builder().vexpand(true).hexpand(true).interactive(false).build();
    for page in &pages {
        carousel.append(page);
    }

    let nav_bar = wizard_nav_bar(&carousel, pages);

    let content_box = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
    content_box.append(&carousel);
    content_box.append(&nav_bar);

    let skip_button = gtk4::Button::with_label(&tr("Überspringen"));
    skip_button.add_css_class("flat");
    let header = adw::HeaderBar::new();
    header.pack_end(&skip_button);

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&content_box));

    let dialog = adw::Dialog::builder().title(tr("Willkommen")).content_width(560).content_height(620).child(&toolbar_view).build();

    // Every way out (Skip, Fertig, Escape, the header's own close button,
    // clicking the backdrop) ends up here exactly once - marking the
    // wizard seen doesn't need to special-case which one it was.
    dialog.connect_closed(|_| mark_seen());
    {
        let dialog = dialog.clone();
        skip_button.connect_clicked(move |_| {
            dialog.close();
        });
    }
    {
        let dialog = dialog.clone();
        finish_button.connect_clicked(move |_| {
            dialog.close();
        });
    }
    {
        let url_row = url_row.clone();
        let username_row = username_row.clone();
        let password_row = password_row.clone();
        let status = connection_status.clone();
        test_button.connect_clicked(move |button| {
            let url = url_row.text().to_string();
            let username = username_row.text().to_string();
            let password = password_row.text().to_string();

            if url.trim().is_empty() || username.trim().is_empty() {
                status.set_label(&tr("Bitte Website-URL und Benutzername angeben."));
                status.set_visible(true);
                return;
            }

            if let Err(err) = wpsite::save(&wpsite::SiteConfig { url: url.clone(), username: username.clone() }) {
                status.set_label(&tr("Fehler beim Speichern: {err}").replace("{err}", &err.to_string()));
                status.set_visible(true);
                return;
            }

            button.set_sensitive(false);
            status.set_label(&tr("Wird gespeichert und getestet …"));
            status.set_visible(true);

            let button = button.clone();
            let status = status.clone();
            glib::MainContext::default().spawn_local(async move {
                if let Err(err) = secrets::store_app_password(&url, &username, &password).await {
                    status.set_label(&tr("Fehler beim Speichern des Passworts: {err}").replace("{err}", &err.to_string()));
                    button.set_sensitive(true);
                    return;
                }
                test_connection(url, username, password, button, status);
            });
        });
    }

    dialog.present(Some(parent));
}

/// Runs on a background thread - `wpclient::Client` is blocking (see its
/// own module docs for why) - and polls the result back via `mpsc` +
/// `glib::timeout_add_local`, the same shape every other network call in
/// this codebase already uses.
fn test_connection(url: String, username: String, password: String, button: gtk4::Button, status: gtk4::Label) {
    let (tx, rx) = mpsc::channel::<Result<usize, String>>();
    std::thread::spawn(move || {
        let outcome = wpclient::Client::new(&url, &username, &password).list_posts().map(|posts| posts.len()).map_err(|err| err.to_string());
        let _ = tx.send(outcome);
    });
    glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
        Ok(outcome) => {
            button.set_sensitive(true);
            match outcome {
                Ok(count) => status.set_label(&if count == 0 {
                    tr("Verbindung erfolgreich - gespeichert.")
                } else {
                    tr("Verbindung erfolgreich, {n} Artikel gefunden - gespeichert.").replace("{n}", &count.to_string())
                }),
                Err(err) => status.set_label(&tr("Gespeichert, aber die Verbindung schlug fehl: {err}").replace("{err}", &err)),
            }
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            button.set_sensitive(true);
            status.set_label(&tr("Interner Fehler: kein Ergebnis vom Verbindungstest."));
            glib::ControlFlow::Break
        }
    });
}

/// Same shape as `export.rs`'s own `wizard_nav_bar` - see that module's
/// doc comment for the full reasoning (GNOME Tour-style linear step flow,
/// non-animated `scroll_to` since each step is a real functional page, not
/// a decorative slide).
fn wizard_nav_bar(carousel: &adw::Carousel, pages: Vec<gtk4::Widget>) -> gtk4::Widget {
    let indicator = adw::CarouselIndicatorDots::builder().carousel(carousel).build();
    let indicator_box = gtk4::Box::builder().hexpand(true).halign(gtk4::Align::Center).build();
    indicator_box.append(&indicator);

    let back_button = gtk4::Button::with_label(&tr("Zurück"));
    back_button.set_visible(false);
    let next_button = gtk4::Button::with_label(&tr("Weiter"));
    next_button.add_css_class("suggested-action");

    {
        let carousel = carousel.clone();
        let pages = pages.clone();
        back_button.connect_clicked(move |_| {
            let pos = carousel.position().round() as usize;
            if pos > 0 {
                carousel.scroll_to(&pages[pos - 1], false);
            }
        });
    }
    {
        let carousel = carousel.clone();
        let pages = pages.clone();
        next_button.connect_clicked(move |_| {
            let pos = carousel.position().round() as usize;
            if pos + 1 < pages.len() {
                carousel.scroll_to(&pages[pos + 1], false);
            }
        });
    }
    {
        let back_button = back_button.clone();
        let next_button = next_button.clone();
        let last_index = pages.len().saturating_sub(1) as u32;
        carousel.connect_page_changed(move |_carousel, index| {
            back_button.set_visible(index > 0);
            next_button.set_visible(index < last_index);
        });
    }

    let bar = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(12)
        .margin_top(6)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    bar.append(&back_button);
    bar.append(&indicator_box);
    bar.append(&next_button);
    bar.upcast()
}
