//! The "Social Media" group of the post pane: the article's Mastodon and
//! Bluesky texts (`social.rs`), next to the other properties. A switch per
//! network, the text (empty = written by the blog's plugin), the
//! attachment and a character count; the state on the blog below, and
//! "Jetzt teilen" for an article that is already online.
//!
//! Nothing here writes to the frontmatter until the user touches a
//! control - an article whose texts were planned in wp-admin stays
//! untouched by an upload from here (`Frontmatter::social` stays `None`).

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk4::glib;

use crate::document::{Frontmatter, PostStatus, PostType};
use crate::i18n::tr;
use crate::social::{self, NetworkInfo, SocialPosts, Status};
use crate::{secrets, wpclient, wpsite};

struct NetworkRow {
    id: &'static str,
    expander: adw::ExpanderRow,
    view: gtk4::TextView,
    embed_row: adw::ComboRow,
    /// Attachment values behind `embed_row`'s entries; the first is
    /// "Voreinstellung des Blogs".
    embed_values: RefCell<Vec<String>>,
    suggest_button: gtk4::Button,
}

pub struct SocialPane {
    pub widget: gtk4::Widget,
    intro_row: adw::EntryRow,
    rows: Vec<Rc<NetworkRow>>,
    state_label: gtk4::Label,
    share_button: gtk4::Button,
    frontmatter: Rc<RefCell<Frontmatter>>,
    site: wpsite::SiteConfig,
    status: RefCell<Option<Status>>,
    /// `false` once the blog answered without the plugin.
    available: Cell<Option<bool>>,
    /// Programmatic changes don't count as the user's.
    loading: Cell<bool>,
    toasts: adw::ToastOverlay,
}

impl SocialPane {
    pub fn new(frontmatter: Rc<RefCell<Frontmatter>>, path: Option<PathBuf>, toasts: adw::ToastOverlay) -> Rc<Self> {
        let fm = frontmatter.borrow().clone();
        let site = wpsite::for_document(path.as_deref(), fm.wp_site.as_deref());
        let social = fm.social.clone().unwrap_or_default();

        let group = adw::PreferencesGroup::builder()
            .title(tr("Social Media"))
            .description(tr("Texte für Mastodon und Bluesky – das Blog teilt sie beim Veröffentlichen. Leer lassen: Das Blog schreibt sie aus Titel, Auszug und Schlagwörtern. {link} steht für den Link zum Artikel."))
            .build();

        let intro_row = adw::EntryRow::builder().title(tr("Einstieg für automatische Texte")).text(&social.intro).build();
        group.add(&intro_row);

        let mut rows = Vec::new();
        for (id, name) in social::NETWORKS {
            let entry = social.entry(id).cloned().unwrap_or_default();
            let expander = adw::ExpanderRow::builder().title(name).show_enable_switch(true).enable_expansion(social.is_enabled(id)).build();

            let view = gtk4::TextView::builder().wrap_mode(gtk4::WrapMode::WordChar).top_margin(6).bottom_margin(6).left_margin(6).right_margin(6).accepts_tab(false).build();
            view.buffer().set_text(&entry.text);
            let frame = gtk4::Frame::builder().child(&view).build();
            let scroller = gtk4::ScrolledWindow::builder().child(&frame).min_content_height(120).hscrollbar_policy(gtk4::PolicyType::Never).build();
            let suggest_button = gtk4::Button::builder().label(tr("Vorschlag vom Blog")).tooltip_text(tr("Text so, wie ihn das Blog schreiben würde – zum Weiterbearbeiten")).build();
            let auto_button = gtk4::Button::builder().label(tr("Automatisch")).tooltip_text(tr("Text leeren: Das Blog schreibt ihn beim Veröffentlichen")).build();
            let buttons = gtk4::Box::builder().spacing(6).halign(gtk4::Align::End).build();
            buttons.append(&auto_button);
            buttons.append(&suggest_button);
            let content = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(6).margin_top(8).margin_bottom(8).margin_start(12).margin_end(12).build();
            content.append(&scroller);
            content.append(&buttons);
            let text_row = adw::PreferencesRow::builder().activatable(false).selectable(false).child(&content).build();
            expander.add_row(&text_row);

            let embed_row = adw::ComboRow::builder().title(tr("Anhang")).build();
            expander.add_row(&embed_row);

            let row = Rc::new(NetworkRow { id, expander, view, embed_row, embed_values: RefCell::new(Vec::new()), suggest_button });
            group.add(&row.expander);
            {
                let row = row.clone();
                auto_button.connect_clicked(move |_| row.view.buffer().set_text(""));
            }
            rows.push(row);
        }

        let state_label = gtk4::Label::builder().xalign(0.0).wrap(true).hexpand(true).build();
        state_label.add_css_class("dim-label");
        let share_button = gtk4::Button::builder().label(tr("Jetzt teilen")).valign(gtk4::Align::Center).visible(false).build();
        share_button.add_css_class("suggested-action");
        let state_box = gtk4::Box::builder().spacing(12).margin_top(12).build();
        state_box.append(&state_label);
        state_box.append(&share_button);

        let column = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();
        column.append(&group);
        column.append(&state_box);
        column.set_visible(fm.post_type == PostType::Post);

        let this = Rc::new(SocialPane {
            widget: column.upcast(),
            intro_row,
            rows,
            state_label,
            share_button,
            frontmatter,
            site,
            status: RefCell::new(None),
            available: Cell::new(None),
            loading: Cell::new(true),
            toasts,
        });
        this.fill_embeds(&social);
        this.wire();
        this.loading.set(false);
        this.update_counts();
        this.update_state();
        this.load_status();
        this
    }

    fn wire(self: &Rc<Self>) {
        {
            let weak = Rc::downgrade(self);
            self.intro_row.connect_changed(move |row| {
                let Some(this) = weak.upgrade() else { return };
                this.edit(|s| s.intro = row.text().to_string());
            });
        }
        for row in &self.rows {
            let id = row.id;
            {
                let weak = Rc::downgrade(self);
                row.expander.connect_enable_expansion_notify(move |expander| {
                    let Some(this) = weak.upgrade() else { return };
                    let on = expander.enables_expansion();
                    this.edit(|s| s.set_enabled(id, on));
                    if on {
                        expander.set_expanded(true);
                    }
                    this.update_state();
                });
            }
            {
                let weak = Rc::downgrade(self);
                row.view.buffer().connect_changed(move |buffer| {
                    let Some(this) = weak.upgrade() else { return };
                    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
                    this.edit(|s| s.entry_mut(id).text = text);
                    this.update_counts();
                });
            }
            {
                let weak = Rc::downgrade(self);
                let row_weak = Rc::downgrade(row);
                row.embed_row.connect_selected_notify(move |combo| {
                    let (Some(this), Some(row)) = (weak.upgrade(), row_weak.upgrade()) else { return };
                    let value = row.embed_values.borrow().get(combo.selected() as usize).cloned().filter(|v| !v.is_empty());
                    this.edit(|s| s.entry_mut(id).embed = value);
                });
            }
            {
                let weak = Rc::downgrade(self);
                row.suggest_button.connect_clicked(move |button| {
                    if let Some(this) = weak.upgrade() {
                        this.suggest(id, button);
                    }
                });
            }
        }
        {
            let weak = Rc::downgrade(self);
            self.share_button.connect_clicked(move |button| {
                if let Some(this) = weak.upgrade() {
                    this.confirm_share(button);
                }
            });
        }
    }

    /// Applies a user's change; from now on Blocksatz manages the texts.
    fn edit(&self, change: impl FnOnce(&mut SocialPosts)) {
        if self.loading.get() {
            return;
        }
        let mut fm = self.frontmatter.borrow_mut();
        change(fm.social.get_or_insert_with(Default::default));
    }

    /// The attachment choices, from the blog's setup once known.
    fn fill_embeds(&self, social: &SocialPosts) {
        let status = self.status.borrow();
        for row in &self.rows {
            let info = status.as_ref().and_then(|s| s.network(row.id));
            let mut values = vec![String::new()];
            values.extend(info.map(|i| i.attachments.clone()).unwrap_or_else(|| default_attachments(row.id)));
            let default_label = match info.map(|i| i.embed_mode.as_str()).filter(|m| !m.is_empty()) {
                Some(mode) => tr("Voreinstellung des Blogs ({mode})").replace("{mode}", &embed_label(mode)),
                None => tr("Voreinstellung des Blogs"),
            };
            let labels: Vec<String> = values.iter().enumerate().map(|(i, v)| if i == 0 { default_label.clone() } else { embed_label(v) }).collect();
            let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            let current = social.entry(row.id).and_then(|e| e.embed.clone()).unwrap_or_default();
            let selected = values.iter().position(|v| *v == current).unwrap_or(0);
            *row.embed_values.borrow_mut() = values;
            row.embed_row.set_model(Some(&gtk4::StringList::new(&label_refs)));
            row.embed_row.set_selected(selected as u32);
        }
    }

    fn update_counts(&self) {
        let fm = self.frontmatter.borrow();
        let status = self.status.borrow();
        let link_len = status.as_ref().map(|s| s.planned_link.chars().count()).filter(|n| *n > 0).unwrap_or_else(|| social::estimated_link_len(&self.site.url, &fm.slug));
        for row in &self.rows {
            let buffer = row.view.buffer();
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
            let fallback = default_info(row.id);
            let info = status.as_ref().and_then(|s| s.network(row.id)).unwrap_or(&fallback);
            let subtitle = if text.trim().is_empty() {
                tr("Automatischer Text vom Blog")
            } else {
                let used = social::count(&text, info, link_len);
                let line = tr("{used} von {limit} Zeichen").replace("{used}", &used.to_string()).replace("{limit}", &info.limit.to_string());
                if used > info.limit { format!("⚠ {line}") } else { line }
            };
            row.expander.set_subtitle(&subtitle);
        }
    }

    /// The line under the group: what the blog will do or has done.
    fn update_state(&self) {
        let fm = self.frontmatter.borrow();
        let online = matches!(fm.wp_status, Some(PostStatus::Publish | PostStatus::Private));
        let enabled: Vec<String> = fm.social.as_ref().map(|s| s.enabled.clone()).unwrap_or_default();
        let status = self.status.borrow();
        let mut lines = Vec::new();

        match self.available.get() {
            Some(false) => {
                lines.push(tr("Auf diesem Blog ist WP Social Publisher (ab 1.10) nicht aktiv – die Texte bleiben nur in der Datei."));
            }
            _ => {
                if let Some(status) = status.as_ref() {
                    for info in &status.networks {
                        if info.posted {
                            lines.push(tr("{network}: geteilt").replace("{network}", &info.name));
                        } else if !info.configured {
                            lines.push(tr("{network}: im Blog nicht eingerichtet").replace("{network}", &info.name));
                        }
                    }
                    for (id, ok, message) in &status.results {
                        if !ok {
                            lines.push(tr("{network}: nicht geteilt – {msg}").replace("{network}", &display_name(id)).replace("{msg}", message));
                        }
                    }
                    if !online && !status.planned.is_empty() {
                        let names: Vec<String> = status.planned.iter().map(|n| display_name(n)).collect();
                        lines.push(tr("Im Blog vorgemerkt: {networks}").replace("{networks}", &names.join(", ")));
                    }
                }
                if !online && !enabled.is_empty() {
                    lines.push(if fm.status == PostStatus::Future || fm.wp_status == Some(PostStatus::Future) {
                        tr("Gehen mit dem Hochladen ans Blog und hinaus, sobald der Artikel zum Termin erscheint.")
                    } else {
                        tr("Gehen mit dem Hochladen ans Blog und hinaus, sobald der Artikel veröffentlicht wird.")
                    });
                }
            }
        }
        self.state_label.set_label(&lines.join("\n"));
        self.state_label.set_visible(!lines.is_empty());

        let unposted = enabled.iter().any(|id| !status.as_ref().and_then(|s| s.network(id)).is_some_and(|i| i.posted));
        self.share_button.set_visible(online && fm.wp_post_id.is_some() && self.available.get() == Some(true) && unposted);
        let linked = fm.wp_post_id.is_some() && self.available.get() != Some(false);
        for row in &self.rows {
            row.suggest_button.set_sensitive(linked);
            row.suggest_button.set_tooltip_text(Some(&if linked { tr("Text so, wie ihn das Blog schreiben würde – zum Weiterbearbeiten") } else { tr("Erst nach dem ersten Hochladen") }));
        }
    }

    /// Asks the blog for the plugin's setup and the article's state.
    pub fn load_status(self: &Rc<Self>) {
        if self.site.url.is_empty() {
            return;
        }
        let post_id = self.frontmatter.borrow().wp_post_id;
        let weak = Rc::downgrade(self);
        self.in_background(
            move |client| social::fetch_status(&client, post_id),
            move |outcome| {
                let Some(this) = weak.upgrade() else { return };
                match outcome {
                    Ok(Some(status)) => {
                        this.available.set(Some(true));
                        *this.status.borrow_mut() = Some(status);
                        this.loading.set(true);
                        let social = this.frontmatter.borrow().social.clone().unwrap_or_default();
                        this.fill_embeds(&social);
                        this.loading.set(false);
                    }
                    Ok(None) => this.available.set(Some(false)),
                    // Offline or not set up: judge from local data alone.
                    Err(_) => {}
                }
                this.update_counts();
                this.update_state();
            },
        );
    }

    fn suggest(self: &Rc<Self>, id: &'static str, button: &gtk4::Button) {
        let Some(post_id) = self.frontmatter.borrow().wp_post_id else { return };
        let intro = self.intro_row.text().to_string();
        button.set_sensitive(false);
        let weak = Rc::downgrade(self);
        let button = button.clone();
        self.in_background(
            move |client| social::generate(&client, post_id, &intro),
            move |outcome| {
                button.set_sensitive(true);
                let Some(this) = weak.upgrade() else { return };
                match outcome {
                    Ok(texts) => match texts.into_iter().find(|(n, _)| n == id) {
                        Some((_, text)) => {
                            if let Some(row) = this.rows.iter().find(|r| r.id == id) {
                                row.view.buffer().set_text(&text);
                            }
                        }
                        None => this.toast(&tr("Das Blog hat für {network} keinen Vorschlag – ist das Netzwerk dort eingerichtet?").replace("{network}", &display_name(id))),
                    },
                    Err(err) => this.toast(&tr("Kein Vorschlag vom Blog: {err}").replace("{err}", &err)),
                }
            },
        );
    }

    fn confirm_share(self: &Rc<Self>, button: &gtk4::Button) {
        let Some(social) = self.frontmatter.borrow().social.clone() else { return };
        let status = self.status.borrow();
        let names: Vec<String> = social.enabled.iter().filter(|id| !status.as_ref().and_then(|s| s.network(id)).is_some_and(|i| i.posted)).map(|id| display_name(id)).collect();
        drop(status);
        let dialog = adw::AlertDialog::new(Some(&tr("Jetzt teilen?")), Some(&tr("Der Artikel geht sofort auf {networks} hinaus. Das lässt sich hier nicht rückgängig machen.").replace("{networks}", &names.join(", "))));
        dialog.add_responses(&[("cancel", &tr("Abbrechen")), ("share", &tr("Teilen"))]);
        dialog.set_response_appearance("share", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            if response != "share" {
                return;
            }
            if let Some(this) = weak.upgrade() {
                this.share(social.clone());
            }
        });
        dialog.present(Some(button));
    }

    fn share(self: &Rc<Self>, social: SocialPosts) {
        let Some(post_id) = self.frontmatter.borrow().wp_post_id else { return };
        self.share_button.set_sensitive(false);
        let weak = Rc::downgrade(self);
        self.in_background(
            move |client| social::share(&client, post_id, &social),
            move |outcome| {
                let Some(this) = weak.upgrade() else { return };
                this.share_button.set_sensitive(true);
                match outcome {
                    Ok((results, status)) => {
                        let lines: Vec<String> = results
                            .iter()
                            .map(|(id, ok, msg)| if *ok { tr("{network}: geteilt.").replace("{network}", &display_name(id)) } else { tr("{network}: nicht geteilt – {msg}").replace("{network}", &display_name(id)).replace("{msg}", msg) })
                            .collect();
                        this.toast(&lines.join(" "));
                        if let Some(status) = status {
                            *this.status.borrow_mut() = Some(status);
                        }
                    }
                    Err(err) => this.toast(&tr("Teilen fehlgeschlagen: {err}").replace("{err}", &err)),
                }
                this.update_state();
            },
        );
    }

    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::builder().title(text).timeout(8).build());
    }

    /// Runs `work` with a client for the article's blog on a worker
    /// thread and hands the outcome to `done` on the main loop.
    fn in_background<T: Send + 'static>(&self, work: impl FnOnce(wpclient::Client) -> Result<T, String> + Send + 'static, done: impl FnOnce(Result<T, String>) + 'static) {
        let site = self.site.clone();
        let (tx, rx) = mpsc::channel::<Result<T, String>>();
        std::thread::spawn(move || {
            let outcome = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
                .map_err(|err| err.to_string())
                .and_then(|p| p.ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden.")))
                .and_then(|password| work(wpclient::Client::new(&site.url, &site.username, &password)));
            let _ = tx.send(outcome);
        });
        let mut done = Some(done);
        glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
            Ok(outcome) => {
                if let Some(done) = done.take() {
                    done(outcome);
                }
                glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
        });
    }

    /// After an upload: the blog's answer may have changed what's planned.
    pub fn after_sync(self: &Rc<Self>) {
        self.update_state();
        self.load_status();
    }
}

fn display_name(id: &str) -> String {
    social::NETWORKS.iter().find(|(n, _)| *n == id).map(|(_, name)| name.to_string()).unwrap_or_else(|| id.to_string())
}

fn default_attachments(id: &str) -> Vec<String> {
    match id {
        "bluesky" => vec!["card".into(), "image".into(), "none".into()],
        _ => vec!["image".into(), "none".into()],
    }
}

/// Limits as the plugin ships them, until the blog answered.
fn default_info(id: &str) -> NetworkInfo {
    match id {
        "mastodon" => NetworkInfo { id: id.into(), limit: 500, url_length: 23, ..Default::default() },
        _ => NetworkInfo { id: id.into(), limit: 300, ..Default::default() },
    }
}

fn embed_label(mode: &str) -> String {
    match mode {
        "card" => tr("Linkvorschau"),
        "image" => tr("Beitragsbild"),
        "none" => tr("Kein Anhang"),
        other => other.to_string(),
    }
}
