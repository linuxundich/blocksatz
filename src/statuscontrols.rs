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
pub fn build_status_row(frontmatter: &Rc<RefCell<Frontmatter>>) -> (adw::ComboRow, adw::EntryRow) {
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
        .text(current.scheduled_at.as_deref().map(document::format_scheduled_at_for_display).unwrap_or_default().as_str())
        .build();
    scheduled_row.set_visible(current.status == PostStatus::Future);

    {
        let frontmatter = frontmatter.clone();
        let scheduled_row = scheduled_row.clone();
        status_row.connect_selected_notify(move |row| {
            if let Some(status) = PostStatus::ALL.get(row.selected() as usize) {
                frontmatter.borrow_mut().status = *status;
                scheduled_row.set_visible(*status == PostStatus::Future);
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

    (status_row, scheduled_row)
}
