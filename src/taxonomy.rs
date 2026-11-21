//! "Kategorien & Tags verwalten" dialog: lists every existing WordPress
//! category/tag with its real term id (`termcache`'s autocomplete cache only
//! ever holds names, which isn't enough to rename or delete) so an existing
//! term can be fixed or removed in place - until now, categories/tags could
//! only ever be *read* (autocomplete in `properties.rs`) or *created*
//! (automatically, on publish, for a name that doesn't exist yet).
//!
//! Categories (not tags - WordPress's built-in `post_tag` taxonomy isn't
//! hierarchical) additionally get a "Übergeordnete Kategorie" picker and a
//! "Neue Kategorie erstellen" row, so a parent/child relationship can
//! actually be set up from inside the app - the article's own "Kategorien"
//! field (`properties.rs`) still just takes plain names either way, since
//! hierarchy is a property of the category term itself, not something a
//! post needs to spell out when it's assigned to a child category.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk4::glib;

use crate::i18n::tr;
use crate::{secrets, termcache, wpclient};

/// Lists, edits and deletes the terms of the open document's blog - the
/// one `term_caches` belongs to (`termcache::use_site`), named in the
/// header, so an English article's dialog edits the English blog.
pub fn open(parent: &adw::ApplicationWindow, term_caches: termcache::TermCacheHandles) {
    let term_caches = termcache::pinned(&term_caches);
    let status_label = gtk4::Label::new(Some(&tr("Lade Kategorien & Tags …")));
    status_label.set_wrap(true);
    status_label.set_xalign(0.0);

    let categories_group = adw::PreferencesGroup::builder().title(tr("Kategorien")).build();
    categories_group.set_visible(false);
    let tags_group = adw::PreferencesGroup::builder().title(tr("Tags")).build();
    tags_group.set_visible(false);

    let content_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(18)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    content_box.append(&status_label);
    content_box.append(&categories_group);
    content_box.append(&tags_group);

    let scroller = gtk4::ScrolledWindow::builder().child(&content_box).vexpand(true).build();

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(&tr("Kategorien & Tags verwalten"), &termcache::site_of(&term_caches).site_id())));
    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&scroller));

    let dialog = adw::Dialog::builder()
        .title(tr("Kategorien & Tags verwalten"))
        .content_width(480)
        .content_height(560)
        .child(&toolbar_view)
        .build();
    dialog.present(Some(parent));

    let site = termcache::site_of(&term_caches);
    if site.url.is_empty() {
        status_label.set_label(&tr("Keine WordPress-Seite konfiguriert."));
        return;
    }

    let (tx, rx) = mpsc::channel::<Result<(Vec<wpclient::Term>, Vec<wpclient::Term>), String>>();
    std::thread::spawn(move || {
        let outcome = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .map_err(|err| err.to_string())
            .and_then(|maybe_password| maybe_password.ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden.")))
            .and_then(|password| {
                let client = wpclient::Client::new(&site.url, &site.username, &password);
                let categories = client.list_terms("categories").map_err(|err| err.to_string())?;
                let tags = client.list_terms("tags").map_err(|err| err.to_string())?;
                Ok((categories, tags))
            });
        let _ = tx.send(outcome);
    });

    glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
        Ok(Ok((categories, tags))) => {
            if categories.is_empty() && tags.is_empty() {
                status_label.set_label(&tr("Keine Kategorien oder Tags vorhanden."));
            } else {
                status_label.set_visible(false);
            }
            let all_categories = Rc::new(RefCell::new(categories.clone()));
            categories_group.set_visible(true);
            for term in &categories {
                let row = build_category_row(term.clone(), &all_categories, &term_caches, &dialog, &status_label, &categories_group);
                categories_group.add(&row);
            }
            categories_group.add(&build_create_category_row(all_categories, &term_caches, &dialog, &status_label, &categories_group));

            tags_group.set_visible(!tags.is_empty());
            for term in tags {
                tags_group.add(&build_tag_row(term, &term_caches, &dialog, &status_label, &tags_group));
            }
            glib::ControlFlow::Break
        }
        Ok(Err(err)) => {
            status_label.set_label(&format!("{}: {err}", tr("Fehler")));
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            status_label.set_label(&tr("Interner Fehler: kein Ergebnis vom Ladevorgang."));
            glib::ControlFlow::Break
        }
    });
}

/// Every id transitively parented under `root` (children, grandchildren, …).
/// Used by `parent_options` to keep a category from being re-parented under
/// one of its own descendants, which would create a cycle WordPress's own
/// admin UI also refuses to let you create.
fn descendant_ids(categories: &[wpclient::Term], root: u64) -> HashSet<u64> {
    let mut result = HashSet::new();
    let mut frontier = vec![root];
    while let Some(id) = frontier.pop() {
        for category in categories {
            if category.parent == id && result.insert(category.id) {
                frontier.push(category.id);
            }
        }
    }
    result
}

/// The labels and matching ids for a "Übergeordnete Kategorie" `ComboRow` -
/// index 0 is always "Keine (oberste Ebene)" (`None`); every other index is
/// `Some(id)` for one of `categories`, excluding `exclude` itself and any
/// of its own descendants. Shared between each existing category's own
/// picker and the "Neue Kategorie erstellen" row (which passes
/// `exclude: None`, since a brand new category has no descendants yet).
fn parent_options(categories: &[wpclient::Term], exclude: Option<u64>) -> (Vec<String>, Vec<Option<u64>>) {
    let excluded_ids: HashSet<u64> = match exclude {
        Some(id) => descendant_ids(categories, id).into_iter().chain(std::iter::once(id)).collect(),
        None => HashSet::new(),
    };
    let mut labels = vec![tr("Keine (oberste Ebene)")];
    let mut ids = vec![None];
    for category in categories {
        if excluded_ids.contains(&category.id) {
            continue;
        }
        labels.push(category.name.clone());
        ids.push(Some(category.id));
    }
    (labels, ids)
}

/// One editable category row - an `Adw.ExpanderRow` (unlike a tag's plain
/// `build_tag_row`) since there's more than just a name to edit now: a
/// nested "Name" entry (rename - same "re-send the whole current text,
/// only once it actually differs" idiom as before) and a "Übergeordnete
/// Kategorie" picker, which applies immediately on selection (no separate
/// save step, matching `properties.rs`'s own `status_row` ComboRow) and
/// reverts itself if the server rejects the change. Delete stays reachable
/// from the collapsed header, same as before.
fn build_category_row(
    term: wpclient::Term,
    all_categories: &Rc<RefCell<Vec<wpclient::Term>>>,
    term_caches: &termcache::TermCacheHandles,
    dialog: &adw::Dialog,
    status_label: &gtk4::Label,
    group: &adw::PreferencesGroup,
) -> adw::ExpanderRow {
    let term_id = term.id;
    let expander = adw::ExpanderRow::builder().title(glib::markup_escape_text(&term.name).as_str()).build();

    let name_row = adw::EntryRow::builder().title(tr("Name")).text(term.name.as_str()).build();
    let original_name = Rc::new(RefCell::new(term.name.clone()));

    let save_button = gtk4::Button::from_icon_name("document-save-symbolic");
    save_button.set_tooltip_text(Some(&tr("Umbenennen")));
    save_button.set_valign(gtk4::Align::Center);
    save_button.add_css_class("flat");
    save_button.set_sensitive(false);
    name_row.add_suffix(&save_button);

    let delete_button = gtk4::Button::from_icon_name("user-trash-symbolic");
    delete_button.set_tooltip_text(Some(&tr("Löschen")));
    delete_button.set_valign(gtk4::Align::Center);
    delete_button.add_css_class("flat");
    expander.add_suffix(&delete_button);

    {
        let save_button = save_button.clone();
        let original_name = original_name.clone();
        name_row.connect_changed(move |row| {
            let text = row.text();
            save_button.set_sensitive(!text.trim().is_empty() && text.as_str() != original_name.borrow().as_str());
        });
    }

    {
        let name_row = name_row.clone();
        let original_name = original_name.clone();
        let term_caches = term_caches.clone();
        let status_label = status_label.clone();
        let save_button_for_click = save_button.clone();
        let delete_button_for_click = delete_button.clone();
        let expander = expander.clone();
        save_button.connect_clicked(move |_| {
            let new_name = name_row.text().to_string();
            save_button_for_click.set_sensitive(false);
            delete_button_for_click.set_sensitive(false);
            name_row.set_sensitive(false);
            status_label.set_visible(false);

            let site = termcache::site_of(&term_caches);
            let (tx, rx) = mpsc::channel::<Result<(), String>>();
            let new_name_for_thread = new_name.clone();
            std::thread::spawn(move || {
                let outcome = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
                    .map_err(|err| err.to_string())
                    .and_then(|maybe_password| maybe_password.ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden.")))
                    .and_then(|password| {
                        wpclient::Client::new(&site.url, &site.username, &password)
                            .rename_term("categories", term_id, &new_name_for_thread)
                            .map_err(|err| err.to_string())
                    });
                let _ = tx.send(outcome);
            });

            let name_row = name_row.clone();
            let original_name = original_name.clone();
            let term_caches = term_caches.clone();
            let status_label = status_label.clone();
            let save_button = save_button_for_click.clone();
            let delete_button = delete_button_for_click.clone();
            let expander = expander.clone();
            glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
                Ok(Ok(())) => {
                    *original_name.borrow_mut() = new_name.clone();
                    expander.set_title(&glib::markup_escape_text(&new_name));
                    name_row.set_sensitive(true);
                    delete_button.set_sensitive(true);
                    save_button.set_sensitive(false);
                    termcache::spawn_refresh(&term_caches);
                    glib::ControlFlow::Break
                }
                Ok(Err(err)) => {
                    name_row.set_sensitive(true);
                    delete_button.set_sensitive(true);
                    save_button.set_sensitive(true);
                    status_label.set_label(&format!("{}: {err}", tr("Fehler beim Umbenennen")));
                    status_label.set_visible(true);
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    name_row.set_sensitive(true);
                    delete_button.set_sensitive(true);
                    save_button.set_sensitive(true);
                    status_label.set_label(&tr("Interner Fehler: kein Ergebnis vom Umbenennen-Thread."));
                    status_label.set_visible(true);
                    glib::ControlFlow::Break
                }
            });
        });
    }

    let (parent_labels, parent_ids) = parent_options(&all_categories.borrow(), Some(term_id));
    let parent_label_refs: Vec<&str> = parent_labels.iter().map(String::as_str).collect();
    let initial_parent = (term.parent != 0).then_some(term.parent);
    let initial_index = parent_ids.iter().position(|id| *id == initial_parent).unwrap_or(0) as u32;
    let parent_row = adw::ComboRow::builder().title(tr("Übergeordnete Kategorie")).model(&gtk4::StringList::new(&parent_label_refs)).selected(initial_index).build();

    let last_good_index = Rc::new(Cell::new(initial_index));
    let updating = Rc::new(Cell::new(false));
    {
        let status_label = status_label.clone();
        let last_good_index = last_good_index.clone();
        let updating = updating.clone();
        let term_caches = term_caches.clone();
        parent_row.connect_selected_notify(move |row| {
            if updating.get() {
                return;
            }
            let new_index = row.selected();
            let new_parent = parent_ids.get(new_index as usize).copied().flatten().unwrap_or(0);
            row.set_sensitive(false);
            status_label.set_visible(false);

            let site = termcache::site_of(&term_caches);
            let (tx, rx) = mpsc::channel::<Result<(), String>>();
            std::thread::spawn(move || {
                let outcome = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
                    .map_err(|err| err.to_string())
                    .and_then(|maybe_password| maybe_password.ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden.")))
                    .and_then(|password| {
                        wpclient::Client::new(&site.url, &site.username, &password)
                            .update_term_parent("categories", term_id, new_parent)
                            .map_err(|err| err.to_string())
                    });
                let _ = tx.send(outcome);
            });

            let row = row.clone();
            let status_label = status_label.clone();
            let last_good_index = last_good_index.clone();
            let updating = updating.clone();
            glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
                Ok(Ok(())) => {
                    last_good_index.set(new_index);
                    row.set_sensitive(true);
                    glib::ControlFlow::Break
                }
                Ok(Err(err)) => {
                    updating.set(true);
                    row.set_selected(last_good_index.get());
                    updating.set(false);
                    row.set_sensitive(true);
                    status_label.set_label(&format!("{}: {err}", tr("Fehler beim Ändern der übergeordneten Kategorie")));
                    status_label.set_visible(true);
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    updating.set(true);
                    row.set_selected(last_good_index.get());
                    updating.set(false);
                    row.set_sensitive(true);
                    status_label.set_label(&tr("Interner Fehler: kein Ergebnis vom Ändern-Thread."));
                    status_label.set_visible(true);
                    glib::ControlFlow::Break
                }
            });
        });
    }

    expander.add_row(&name_row);
    expander.add_row(&parent_row);

    {
        let dialog = dialog.clone();
        let expander_for_delete = expander.clone();
        let term_caches = term_caches.clone();
        let status_label = status_label.clone();
        let group = group.clone();
        delete_button.connect_clicked(move |delete_button| {
            let confirm = adw::AlertDialog::new(
                Some(&tr("Term wirklich löschen?")),
                Some(&tr("Der Begriff wird unwiderruflich von der WordPress-Seite gelöscht.")),
            );
            confirm.add_response("cancel", &tr("Abbrechen"));
            confirm.add_response("delete", &tr("Löschen"));
            confirm.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
            confirm.set_default_response(Some("cancel"));
            confirm.set_close_response("cancel");

            let expander = expander_for_delete.clone();
            let term_caches = term_caches.clone();
            let status_label = status_label.clone();
            let delete_button = delete_button.clone();
            let group = group.clone();
            confirm.connect_response(None, move |_, response| {
                if response != "delete" {
                    return;
                }
                expander.set_sensitive(false);
                delete_button.set_sensitive(false);
                status_label.set_visible(false);

                let site = termcache::site_of(&term_caches);
                let (tx, rx) = mpsc::channel::<Result<(), String>>();
                std::thread::spawn(move || {
                    let outcome = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
                        .map_err(|err| err.to_string())
                        .and_then(|maybe_password| maybe_password.ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden.")))
                        .and_then(|password| {
                            wpclient::Client::new(&site.url, &site.username, &password)
                                .delete_term("categories", term_id)
                                .map_err(|err| err.to_string())
                        });
                    let _ = tx.send(outcome);
                });

                let expander = expander.clone();
                let term_caches = term_caches.clone();
                let status_label = status_label.clone();
                let delete_button = delete_button.clone();
                let group = group.clone();
                glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
                    Ok(Ok(())) => {
                        group.remove(&expander);
                        termcache::spawn_refresh(&term_caches);
                        glib::ControlFlow::Break
                    }
                    Ok(Err(err)) => {
                        expander.set_sensitive(true);
                        delete_button.set_sensitive(true);
                        status_label.set_label(&format!("{}: {err}", tr("Fehler beim Löschen")));
                        status_label.set_visible(true);
                        glib::ControlFlow::Break
                    }
                    Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        expander.set_sensitive(true);
                        delete_button.set_sensitive(true);
                        status_label.set_label(&tr("Interner Fehler: kein Ergebnis vom Lösch-Thread."));
                        status_label.set_visible(true);
                        glib::ControlFlow::Break
                    }
                });
            });
            confirm.present(Some(&dialog));
        });
    }

    expander
}

/// The "Neue Kategorie erstellen" row - its own `Übergeordnete Kategorie`
/// picker shares `parent_options` with each existing category's row above,
/// just with nothing excluded (a brand new category has no descendants
/// yet). Appends the freshly-created row directly (using the id `create_term`
/// hands back) rather than re-fetching the whole dialog - `all_categories`
/// is updated too, so creating a *second* new category in the same dialog
/// session offers the first one as a selectable parent as well; existing
/// rows opened before this one keep whatever parent list they were built
/// with, a minor staleness accepted for not needing a full reload.
fn build_create_category_row(
    all_categories: Rc<RefCell<Vec<wpclient::Term>>>,
    term_caches: &termcache::TermCacheHandles,
    dialog: &adw::Dialog,
    status_label: &gtk4::Label,
    group: &adw::PreferencesGroup,
) -> adw::ExpanderRow {
    let expander = adw::ExpanderRow::builder().title(tr("Neue Kategorie erstellen…")).build();

    let name_row = adw::EntryRow::builder().title(tr("Name")).build();
    let (parent_labels, parent_ids) = parent_options(&all_categories.borrow(), None);
    let parent_label_refs: Vec<&str> = parent_labels.iter().map(String::as_str).collect();
    let parent_row = adw::ComboRow::builder().title(tr("Übergeordnete Kategorie")).model(&gtk4::StringList::new(&parent_label_refs)).selected(0).build();

    let create_button = gtk4::Button::with_label(&tr("Erstellen"));
    create_button.add_css_class("suggested-action");
    create_button.set_sensitive(false);
    let create_row_box = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).margin_top(6).margin_bottom(6).margin_start(12).margin_end(12).build();
    create_row_box.append(&create_button);
    let create_row = gtk4::ListBoxRow::builder().child(&create_row_box).activatable(false).selectable(false).build();

    {
        let create_button = create_button.clone();
        name_row.connect_changed(move |row| {
            create_button.set_sensitive(!row.text().trim().is_empty());
        });
    }

    {
        let name_row = name_row.clone();
        let parent_row = parent_row.clone();
        let create_button_for_click = create_button.clone();
        let term_caches = term_caches.clone();
        let status_label = status_label.clone();
        let group = group.clone();
        let dialog = dialog.clone();
        let expander = expander.clone();
        create_button.connect_clicked(move |_| {
            let name = name_row.text().to_string();
            let parent = parent_ids.get(parent_row.selected() as usize).copied().flatten().unwrap_or(0);

            create_button_for_click.set_sensitive(false);
            name_row.set_sensitive(false);
            parent_row.set_sensitive(false);
            status_label.set_visible(false);

            let site = termcache::site_of(&term_caches);
            let (tx, rx) = mpsc::channel::<Result<u64, String>>();
            let name_for_thread = name.clone();
            std::thread::spawn(move || {
                let outcome = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
                    .map_err(|err| err.to_string())
                    .and_then(|maybe_password| maybe_password.ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden.")))
                    .and_then(|password| {
                        wpclient::Client::new(&site.url, &site.username, &password)
                            .create_term("categories", &name_for_thread, parent)
                            .map_err(|err| err.to_string())
                    });
                let _ = tx.send(outcome);
            });

            let name_row = name_row.clone();
            let parent_row = parent_row.clone();
            let create_button = create_button_for_click.clone();
            let all_categories = all_categories.clone();
            let term_caches = term_caches.clone();
            let status_label = status_label.clone();
            let group = group.clone();
            let dialog = dialog.clone();
            let expander = expander.clone();
            let name = name.clone();
            glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
                Ok(Ok(id)) => {
                    let new_term = wpclient::Term { id, name: name.clone(), slug: String::new(), parent };
                    all_categories.borrow_mut().push(new_term.clone());
                    let row = build_category_row(new_term, &all_categories, &term_caches, &dialog, &status_label, &group);
                    group.add(&row);
                    // Move the "Neue Kategorie erstellen…" row back to the
                    // bottom, below the just-added one, so it stays the
                    // group's last entry rather than sandwiched between
                    // existing categories.
                    group.remove(&expander);
                    group.add(&expander);

                    name_row.set_text("");
                    name_row.set_sensitive(true);
                    parent_row.set_sensitive(true);
                    create_button.set_sensitive(false);
                    termcache::spawn_refresh(&term_caches);
                    glib::ControlFlow::Break
                }
                Ok(Err(err)) => {
                    name_row.set_sensitive(true);
                    parent_row.set_sensitive(true);
                    create_button.set_sensitive(true);
                    status_label.set_label(&format!("{}: {err}", tr("Fehler beim Erstellen")));
                    status_label.set_visible(true);
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    name_row.set_sensitive(true);
                    parent_row.set_sensitive(true);
                    create_button.set_sensitive(true);
                    status_label.set_label(&tr("Interner Fehler: kein Ergebnis vom Erstellen-Thread."));
                    status_label.set_visible(true);
                    glib::ControlFlow::Break
                }
            });
        });
    }

    expander.add_row(&name_row);
    expander.add_row(&parent_row);
    expander.add_row(&create_row);

    expander
}

/// One editable tag row: renaming re-sends the whole current text as the
/// new name (the "Umbenennen" button only lights up once the text actually
/// differs from what's on the server), deleting asks for confirmation first
/// since it's unrecoverable. Both mutations re-run `termcache::spawn_refresh`
/// on success so the properties dialog's autocomplete reflects the change
/// immediately, without needing its own separate WordPress round trip here.
/// No parent picker here (unlike `build_category_row`) - WordPress's
/// built-in `post_tag` taxonomy isn't hierarchical.
fn build_tag_row(
    term: wpclient::Term,
    term_caches: &termcache::TermCacheHandles,
    dialog: &adw::Dialog,
    status_label: &gtk4::Label,
    group: &adw::PreferencesGroup,
) -> adw::EntryRow {
    let term_id = term.id;
    let taxonomy = "tags";
    let row = adw::EntryRow::builder().title(tr("Name")).text(term.name.as_str()).build();
    let original_name = Rc::new(RefCell::new(term.name));

    let save_button = gtk4::Button::from_icon_name("document-save-symbolic");
    save_button.set_tooltip_text(Some(&tr("Umbenennen")));
    save_button.set_valign(gtk4::Align::Center);
    save_button.add_css_class("flat");
    save_button.set_sensitive(false);
    row.add_suffix(&save_button);

    let delete_button = gtk4::Button::from_icon_name("user-trash-symbolic");
    delete_button.set_tooltip_text(Some(&tr("Löschen")));
    delete_button.set_valign(gtk4::Align::Center);
    delete_button.add_css_class("flat");
    row.add_suffix(&delete_button);

    {
        let save_button = save_button.clone();
        let original_name = original_name.clone();
        row.connect_changed(move |row| {
            let text = row.text();
            save_button.set_sensitive(!text.trim().is_empty() && text.as_str() != original_name.borrow().as_str());
        });
    }

    {
        let row = row.clone();
        let original_name = original_name.clone();
        let term_caches = term_caches.clone();
        let status_label = status_label.clone();
        let save_button_for_click = save_button.clone();
        let delete_button_for_click = delete_button.clone();
        save_button.connect_clicked(move |_| {
            let new_name = row.text().to_string();
            save_button_for_click.set_sensitive(false);
            delete_button_for_click.set_sensitive(false);
            row.set_sensitive(false);
            status_label.set_visible(false);

            let site = termcache::site_of(&term_caches);
            let (tx, rx) = mpsc::channel::<Result<(), String>>();
            let new_name_for_thread = new_name.clone();
            std::thread::spawn(move || {
                let outcome = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
                    .map_err(|err| err.to_string())
                    .and_then(|maybe_password| maybe_password.ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden.")))
                    .and_then(|password| {
                        wpclient::Client::new(&site.url, &site.username, &password)
                            .rename_term(taxonomy, term_id, &new_name_for_thread)
                            .map_err(|err| err.to_string())
                    });
                let _ = tx.send(outcome);
            });

            let row = row.clone();
            let original_name = original_name.clone();
            let term_caches = term_caches.clone();
            let status_label = status_label.clone();
            let save_button = save_button_for_click.clone();
            let delete_button = delete_button_for_click.clone();
            glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
                Ok(Ok(())) => {
                    *original_name.borrow_mut() = new_name.clone();
                    row.set_sensitive(true);
                    delete_button.set_sensitive(true);
                    save_button.set_sensitive(false);
                    termcache::spawn_refresh(&term_caches);
                    glib::ControlFlow::Break
                }
                Ok(Err(err)) => {
                    row.set_sensitive(true);
                    delete_button.set_sensitive(true);
                    save_button.set_sensitive(true);
                    status_label.set_label(&format!("{}: {err}", tr("Fehler beim Umbenennen")));
                    status_label.set_visible(true);
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    row.set_sensitive(true);
                    delete_button.set_sensitive(true);
                    save_button.set_sensitive(true);
                    status_label.set_label(&tr("Interner Fehler: kein Ergebnis vom Umbenennen-Thread."));
                    status_label.set_visible(true);
                    glib::ControlFlow::Break
                }
            });
        });
    }

    {
        let dialog = dialog.clone();
        let row = row.clone();
        let term_caches = term_caches.clone();
        let status_label = status_label.clone();
        let group = group.clone();
        delete_button.connect_clicked(move |delete_button| {
            let confirm = adw::AlertDialog::new(
                Some(&tr("Term wirklich löschen?")),
                Some(&tr("Der Begriff wird unwiderruflich von der WordPress-Seite gelöscht.")),
            );
            confirm.add_response("cancel", &tr("Abbrechen"));
            confirm.add_response("delete", &tr("Löschen"));
            confirm.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
            confirm.set_default_response(Some("cancel"));
            confirm.set_close_response("cancel");

            let row = row.clone();
            let term_caches = term_caches.clone();
            let status_label = status_label.clone();
            let delete_button = delete_button.clone();
            let group = group.clone();
            confirm.connect_response(None, move |_, response| {
                if response != "delete" {
                    return;
                }
                row.set_sensitive(false);
                delete_button.set_sensitive(false);
                status_label.set_visible(false);

                let site = termcache::site_of(&term_caches);
                let (tx, rx) = mpsc::channel::<Result<(), String>>();
                std::thread::spawn(move || {
                    let outcome = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
                        .map_err(|err| err.to_string())
                        .and_then(|maybe_password| maybe_password.ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden.")))
                        .and_then(|password| {
                            wpclient::Client::new(&site.url, &site.username, &password)
                                .delete_term(taxonomy, term_id)
                                .map_err(|err| err.to_string())
                        });
                    let _ = tx.send(outcome);
                });

                let row = row.clone();
                let term_caches = term_caches.clone();
                let status_label = status_label.clone();
                let delete_button = delete_button.clone();
                let group = group.clone();
                glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
                    Ok(Ok(())) => {
                        group.remove(&row);
                        termcache::spawn_refresh(&term_caches);
                        glib::ControlFlow::Break
                    }
                    Ok(Err(err)) => {
                        row.set_sensitive(true);
                        delete_button.set_sensitive(true);
                        status_label.set_label(&format!("{}: {err}", tr("Fehler beim Löschen")));
                        status_label.set_visible(true);
                        glib::ControlFlow::Break
                    }
                    Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        row.set_sensitive(true);
                        delete_button.set_sensitive(true);
                        status_label.set_label(&tr("Interner Fehler: kein Ergebnis vom Lösch-Thread."));
                        status_label.set_visible(true);
                        glib::ControlFlow::Break
                    }
                });
            });
            confirm.present(Some(&dialog));
        });
    }

    row
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(id: u64, name: &str, parent: u64) -> wpclient::Term {
        wpclient::Term { id, name: name.to_string(), slug: String::new(), parent }
    }

    #[test]
    fn descendant_ids_finds_children_and_grandchildren() {
        let categories = vec![term(1, "Netz", 0), term(2, "Politik", 1), term(3, "Datenschutz", 2), term(4, "Hardware", 0)];
        let ids = descendant_ids(&categories, 1);
        assert_eq!(ids, [2, 3].into_iter().collect::<HashSet<_>>());
    }

    #[test]
    fn descendant_ids_is_empty_for_a_leaf_category() {
        let categories = vec![term(1, "Netz", 0), term(2, "Politik", 1)];
        assert!(descendant_ids(&categories, 2).is_empty());
    }

    #[test]
    fn parent_options_excludes_the_category_itself_and_its_descendants() {
        let categories = vec![term(1, "Netz", 0), term(2, "Politik", 1), term(3, "Hardware", 0)];
        let (labels, ids) = parent_options(&categories, Some(1));
        assert_eq!(labels, vec![tr("Keine (oberste Ebene)"), "Hardware".to_string()]);
        assert_eq!(ids, vec![None, Some(3)]);
    }

    #[test]
    fn parent_options_excludes_nothing_when_creating_a_brand_new_category() {
        let categories = vec![term(1, "Netz", 0), term(2, "Politik", 1)];
        let (labels, ids) = parent_options(&categories, None);
        assert_eq!(labels.len(), 3, "{labels:?}");
        assert_eq!(ids, vec![None, Some(1), Some(2)]);
    }
}
