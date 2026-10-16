//! `Typ`/`Status` publish-state controls, live-bound to a document's
//! `Frontmatter`, for the "Artikel-Eigenschaften" dialog (`properties.rs`).

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::document::{self, Frontmatter, PostStatus, PostType};
use crate::i18n::tr;

/// Builds the `Typ` (Artikel/Seite) `ComboRow`, live-bound to
/// `frontmatter.post_type` - locked once the document is linked to an
/// existing WordPress item (`wp_post_id` set), since WordPress can't
/// convert a post into a page in place; switching then would just make the
/// next export try to update a nonexistent page with that post's id. A
/// caller that needs to react to a type change itself (e.g. `properties.rs`'s
/// taxonomy-page/parent-row visibility, which don't exist on every surface
/// this row is used from) connects its own additional
/// `connect_selected_notify` handler on the returned row - GTK signals
/// support more than one listener.
pub fn build_type_row(frontmatter: &Rc<RefCell<Frontmatter>>) -> adw::ComboRow {
    let current = frontmatter.borrow().clone();
    let labels: Vec<String> = PostType::ALL.iter().map(|t| t.label()).collect();
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let row = adw::ComboRow::builder()
        .title(tr("Typ"))
        .model(&gtk4::StringList::new(&label_refs))
        .selected(PostType::ALL.iter().position(|t| *t == current.post_type).unwrap_or(0) as u32)
        .sensitive(current.wp_post_id.is_none())
        .build();
    if current.wp_post_id.is_some() {
        row.set_subtitle(&tr("Bereits mit WordPress verknüpft - nicht mehr änderbar"));
    }

    let frontmatter = frontmatter.clone();
    row.connect_selected_notify(move |row| {
        if let Some(post_type) = PostType::ALL.get(row.selected() as usize) {
            frontmatter.borrow_mut().post_type = *post_type;
        }
    });
    row
}

/// Builds the `Status` `ComboRow` and its conditionally-visible `Termin`
/// `EntryRow`, live-bound to `frontmatter.status`/`scheduled_at`. Always
/// built and returned together since the entry row's visibility depends on
/// the combo row's own value.
pub fn build_status_row(frontmatter: &Rc<RefCell<Frontmatter>>) -> (adw::ComboRow, adw::EntryRow, adw::EntryRow) {
    let current = frontmatter.borrow().clone();
    let labels: Vec<String> = PostStatus::ALL.iter().map(|s| s.label()).collect();
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let selected_index = PostStatus::ALL.iter().position(|s| *s == current.status).unwrap_or(0);
    let status_row = adw::ComboRow::builder()
        .title(tr("Status"))
        .model(&gtk4::StringList::new(&label_refs))
        .selected(selected_index as u32)
        .build();

    let scheduled_row = adw::EntryRow::builder()
        .title(tr("Veröffentlichungstermin (JJJJ-MM-TT HH:MM)"))
        .tooltip_text(tr("Ein Datum in der Vergangenheit datiert den Beitrag zurück; er bleibt bzw. geht online."))
        .text(current.scheduled_at.as_deref().map(document::format_scheduled_at_for_display).unwrap_or_default().as_str())
        .build();
    scheduled_row.set_visible(current.status == PostStatus::Future);

    // The publish date of a post that is online: changed here, it goes up
    // with "Datum hochladen" - only into the past.
    let online = |status: PostStatus, fm: &Frontmatter| matches!(status, PostStatus::Publish | PostStatus::Private) && fm.wp_post_id.is_some() && matches!(fm.wp_status, Some(PostStatus::Publish | PostStatus::Private));
    let published_row = adw::EntryRow::builder()
        .title(tr("Veröffentlicht am (JJJJ-MM-TT HH:MM)"))
        .tooltip_text(tr("Nur Daten in der Vergangenheit - ein späteres Datum nähme den Beitrag offline; dafür gibt es „Geplant“."))
        .text(current.publish_date.as_deref().or(current.wp_published_at.as_deref()).map(document::format_scheduled_at_for_display).unwrap_or_default().as_str())
        .build();
    published_row.set_visible(online(current.status, &current));

    {
        let frontmatter = frontmatter.clone();
        let scheduled_row = scheduled_row.clone();
        let published_row = published_row.clone();
        status_row.connect_selected_notify(move |row| {
            if let Some(status) = PostStatus::ALL.get(row.selected() as usize) {
                let mut fm = frontmatter.borrow_mut();
                fm.status = *status;
                scheduled_row.set_visible(*status == PostStatus::Future);
                published_row.set_visible(online(*status, &fm));
            }
        });
    }
    {
        let frontmatter = frontmatter.clone();
        scheduled_row.connect_changed(move |row| {
            let text = row.text().to_string();
            let parsed = document::parse_scheduled_at(&text);
            row.remove_css_class("error");
            if !text.trim().is_empty() && parsed.is_none() {
                row.add_css_class("error");
            }
            frontmatter.borrow_mut().scheduled_at = parsed;
        });
    }

    {
        let frontmatter = frontmatter.clone();
        published_row.connect_changed(move |row| {
            let text = row.text().to_string();
            let parsed = document::parse_scheduled_at(&text);
            let in_future = parsed.as_deref().is_some_and(|date| date > now_site_time().as_str());
            row.remove_css_class("error");
            if (!text.trim().is_empty() && parsed.is_none()) || in_future {
                row.add_css_class("error");
            }
            let mut fm = frontmatter.borrow_mut();
            fm.publish_date = parsed.filter(|_| !in_future).filter(|date| Some(date) != fm.wp_published_at.as_ref());
        });
    }

    (status_row, scheduled_row, published_row)
}

/// Now in local time as `YYYY-MM-DDTHH:MM:SS` - the blog's site time,
/// assuming the blog runs in the writer's time zone.
fn now_site_time() -> String {
    gtk4::glib::DateTime::now_local().ok().and_then(|now| now.format("%Y-%m-%dT%H:%M:%S").ok()).map(|s| s.to_string()).unwrap_or_default()
}
