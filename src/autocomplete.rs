//! Attaches suggestion-popover autocomplete to a comma-separated `EntryRow`
//! (used for the categories/tags fields in `properties.rs`), suggesting
//! from a shared term list. That list's contents (fetching from WordPress,
//! on-disk caching, manual refresh) are `termcache`'s concern, not this
//! module's - this is purely the popover/matching UI wired to whatever
//! `Rc<RefCell<Vec<String>>>` it's handed.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

const MAX_SUGGESTIONS: usize = 8;

/// Suggests terms matching the text after the last comma, replacing that
/// segment (keeping everything before it intact) when a suggestion is
/// picked - so categories/tags stay comma-separated as the user builds up
/// the list.
pub fn attach(entry: &adw::EntryRow, terms: Rc<RefCell<Vec<String>>>) {
    let popover = gtk4::Popover::builder().autohide(false).has_arrow(true).build();
    popover.set_parent(entry);

    let list_box = gtk4::ListBox::new();
    list_box.add_css_class("boxed-list");
    popover.set_child(Some(&list_box));

    let entry_for_activate = entry.clone();
    let popover_for_activate = popover.clone();
    list_box.connect_row_activated(move |_, row| {
        let Some(label) = row.child().and_downcast::<gtk4::Label>() else {
            return;
        };
        let suggestion = label.text().to_string();
        let current = entry_for_activate.text().to_string();
        let prefix = current.rfind(',').map(|idx| format!("{} ", &current[..=idx])).unwrap_or_default();
        let new_text = format!("{prefix}{suggestion}, ");
        entry_for_activate.set_text(&new_text);
        entry_for_activate.set_position(new_text.chars().count() as i32);
        popover_for_activate.popdown();
    });

    // `autohide(false)` keeps typing in the entry (an autohiding popover
    // grabs the keyboard), so closing is done here: Escape, a click
    // anywhere outside the entry and the list, or leaving the field.
    {
        let popover = popover.clone();
        let keys = gtk4::EventControllerKey::new();
        keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk4::gdk::Key::Escape && popover.is_visible() {
                popover.popdown();
                return gtk4::glib::Propagation::Stop;
            }
            gtk4::glib::Propagation::Proceed
        });
        entry.add_controller(keys);
    }
    {
        let popover = popover.clone();
        let entry_weak = entry.downgrade();
        let focus = gtk4::EventControllerFocus::new();
        focus.connect_leave(move |_| {
            let popover = popover.clone();
            let entry_weak = entry_weak.clone();
            // A click into the list moves focus there first; only close
            // when focus went somewhere else entirely.
            gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
                let Some(entry) = entry_weak.upgrade() else { return };
                let inside = |w: &gtk4::Widget| w.has_focus() || w.focus_child().is_some();
                if !inside(entry.upcast_ref()) && !inside(popover.upcast_ref()) {
                    popover.popdown();
                }
            });
        });
        entry.add_controller(focus);
    }
    {
        let popover = popover.clone();
        let entry_weak = entry.downgrade();
        // Added to the window while the field exists, removed with it -
        // the properties are rebuilt per article.
        let installed: Rc<RefCell<Option<(gtk4::Widget, gtk4::GestureClick)>>> = Rc::default();
        {
            let installed = installed.clone();
            entry.connect_unrealize(move |_| {
                if let Some((root, click)) = installed.borrow_mut().take() {
                    root.remove_controller(&click);
                }
            });
        }
        entry.connect_realize(move |entry| {
            let Some(root) = entry.root() else { return };
            let click = gtk4::GestureClick::new();
            click.set_propagation_phase(gtk4::PropagationPhase::Capture);
            let popover = popover.clone();
            let entry_weak = entry_weak.clone();
            let root_widget: gtk4::Widget = root.clone().upcast();
            click.connect_pressed(move |_, _, x, y| {
                if !popover.is_visible() {
                    return;
                }
                let Some(entry) = entry_weak.upgrade() else { return };
                let hit = root_widget.pick(x, y, gtk4::PickFlags::DEFAULT);
                let on = |target: &gtk4::Widget| hit.as_ref().is_some_and(|w| w == target || w.is_ancestor(target));
                if !on(entry.upcast_ref()) {
                    popover.popdown();
                }
            });
            root.add_controller(click.clone());
            if let Some((old_root, old)) = installed.borrow_mut().replace((root.upcast(), click)) {
                old_root.remove_controller(&old);
            }
        });
    }

    let list_box_for_changed = list_box.clone();
    let popover_for_changed = popover.clone();
    entry.connect_changed(move |row| {
        let text = row.text().to_string();
        let current_segment = text.rsplit(',').next().unwrap_or("").trim().to_lowercase();

        while let Some(child) = list_box_for_changed.first_child() {
            list_box_for_changed.remove(&child);
        }

        if current_segment.is_empty() {
            popover_for_changed.popdown();
            return;
        }

        let typed = text.rsplit(',').next().unwrap_or("").trim().to_string();
        let matches = suggestions(&terms.borrow(), &current_segment);

        // Nothing to offer beyond what is already typed, exactly so.
        if matches.is_empty() || (matches.len() == 1 && matches[0] == typed) {
            popover_for_changed.popdown();
            return;
        }

        for term in &matches {
            let label = gtk4::Label::builder()
                .label(term)
                .xalign(0.0)
                .margin_top(6)
                .margin_bottom(6)
                .margin_start(10)
                .margin_end(10)
                .build();
            let list_row = gtk4::ListBoxRow::new();
            list_row.set_child(Some(&label));
            list_box_for_changed.append(&list_row);
        }
        popover_for_changed.popup();
    });
}

/// Terms containing `segment` (lowercase): the exact term first, then
/// those starting with it (shortest first), then the rest - so typing
/// "gnome" offers "GNOME" before "GNOME Shell" and "Desktop GNOME".
fn suggestions(terms: &[String], segment: &str) -> Vec<String> {
    let mut matches: Vec<(u8, usize, &String)> = terms
        .iter()
        .filter_map(|term| {
            let lower = term.to_lowercase();
            let rank = if lower == segment {
                0
            } else if lower.starts_with(segment) {
                1
            } else if lower.contains(segment) {
                2
            } else {
                return None;
            };
            Some((rank, term.chars().count(), term))
        })
        .collect();
    matches.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)).then_with(|| a.2.to_lowercase().cmp(&b.2.to_lowercase())));
    matches.into_iter().take(MAX_SUGGESTIONS).map(|(_, _, term)| term.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn exact_term_comes_first() {
        let all = terms(&["GNOME Do", "GNOME Erweiterung", "Desktop GNOME", "GNOME", "GNOME Shell", "KDE"]);
        assert_eq!(suggestions(&all, "gnome"), terms(&["GNOME", "GNOME Do", "GNOME Shell", "GNOME Erweiterung", "Desktop GNOME"]));
    }

    #[test]
    fn exact_term_shows_even_among_many_longer_ones() {
        let mut all: Vec<String> = (0..20).map(|i| format!("GNOME Thema {i:02}")).collect();
        all.push("GNOME".into());
        let found = suggestions(&all, "gnome");
        assert_eq!(found.len(), MAX_SUGGESTIONS);
        assert_eq!(found[0], "GNOME");
    }

    #[test]
    fn nothing_matches() {
        assert!(suggestions(&terms(&["KDE", "Arch Linux"]), "gnome").is_empty());
    }
}
