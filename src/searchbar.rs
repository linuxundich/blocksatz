//! Ctrl+F "Suchen und Ersetzen": a `Gtk.Revealer` bar sliding up from the
//! bottom of the editor pane, backed by GtkSourceView's own
//! `SearchContext`/`SearchSettings` - case-insensitive substring search
//! with wrap-around, and live highlighting of every match handled entirely
//! by GtkSourceView itself. This module only drives which match is
//! currently selected and the replace actions on top of that.
//!
//! "The current match" is deliberately not tracked as its own state -
//! it's just whatever the buffer's selection happens to be, since
//! `select_from_cursor` is the only thing that ever sets it while the bar
//! is open. That keeps `replace_current` a matter of reading
//! `buffer.selection_bounds()` fresh at click-time rather than juggling
//! `Gtk.TextMark`s that would need to survive across buffer edits.

use std::rc::Rc;

use gtk4::prelude::*;
use sourceview5::prelude::*;

use crate::i18n::tr;

pub struct SearchBar {
    pub widget: gtk4::Revealer,
    view: sourceview5::View,
    buffer: sourceview5::Buffer,
    settings: sourceview5::SearchSettings,
    context: sourceview5::SearchContext,
    search_entry: gtk4::SearchEntry,
    replace_entry: gtk4::Entry,
    count_label: gtk4::Label,
}

impl SearchBar {
    pub fn new(view: &sourceview5::View, buffer: &sourceview5::Buffer) -> Rc<Self> {
        let settings = sourceview5::SearchSettings::builder().wrap_around(true).build();
        let context = sourceview5::SearchContext::builder().buffer(buffer).settings(&settings).highlight(true).build();

        let search_entry = gtk4::SearchEntry::builder().placeholder_text(tr("Suchen…")).hexpand(true).build();
        let count_label = gtk4::Label::builder().css_classes(["dim-label"]).width_chars(10).build();

        let prev_button = gtk4::Button::from_icon_name("go-up-symbolic");
        prev_button.set_tooltip_text(Some(&tr("Vorheriger Treffer")));
        let next_button = gtk4::Button::from_icon_name("go-down-symbolic");
        next_button.set_tooltip_text(Some(&tr("Nächster Treffer (Eingabe)")));
        let nav_group = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).build();
        nav_group.add_css_class("linked");
        nav_group.append(&prev_button);
        nav_group.append(&next_button);

        let replace_entry = gtk4::Entry::builder().placeholder_text(tr("Ersetzen mit…")).hexpand(true).build();
        let replace_button = gtk4::Button::with_label(&tr("Ersetzen"));
        let replace_all_button = gtk4::Button::with_label(&tr("Alle ersetzen"));
        let close_button = gtk4::Button::from_icon_name("window-close-symbolic");
        close_button.set_tooltip_text(Some(&tr("Schließen (Esc)")));
        close_button.add_css_class("flat");

        let row = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .spacing(6)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(6)
            .margin_end(6)
            .build();
        row.append(&search_entry);
        row.append(&count_label);
        row.append(&nav_group);
        row.append(&gtk4::Separator::new(gtk4::Orientation::Vertical));
        row.append(&replace_entry);
        row.append(&replace_button);
        row.append(&replace_all_button);
        row.append(&close_button);

        let widget = gtk4::Revealer::builder().transition_type(gtk4::RevealerTransitionType::SlideUp).child(&row).build();

        let bar = Rc::new(SearchBar {
            widget,
            view: view.clone(),
            buffer: buffer.clone(),
            settings,
            context,
            search_entry: search_entry.clone(),
            replace_entry,
            count_label,
        });

        {
            let bar = bar.clone();
            search_entry.connect_search_changed(move |entry| {
                let text = entry.text();
                bar.settings.set_search_text((!text.is_empty()).then_some(text.as_str()));
                if !text.is_empty() {
                    bar.select_from_cursor(true);
                }
            });
        }
        {
            let bar = bar.clone();
            search_entry.connect_activate(move |_| bar.select_from_cursor(true));
        }
        {
            let bar = bar.clone();
            search_entry.connect_next_match(move |_| bar.select_from_cursor(true));
        }
        {
            let bar = bar.clone();
            search_entry.connect_previous_match(move |_| bar.select_from_cursor(false));
        }
        {
            let bar = bar.clone();
            search_entry.connect_stop_search(move |_| bar.close());
        }
        {
            let bar = bar.clone();
            next_button.connect_clicked(move |_| bar.select_from_cursor(true));
        }
        {
            let bar = bar.clone();
            prev_button.connect_clicked(move |_| bar.select_from_cursor(false));
        }
        {
            let bar = bar.clone();
            replace_button.connect_clicked(move |_| bar.replace_current());
        }
        {
            let bar = bar.clone();
            replace_all_button.connect_clicked(move |_| bar.replace_all());
        }
        {
            let bar = bar.clone();
            close_button.connect_clicked(move |_| bar.close());
        }
        {
            let context = bar.context.clone();
            let bar = bar.clone();
            context.connect_occurrences_count_notify(move |ctx| {
                bar.update_count_label(ctx.occurrences_count());
            });
        }

        bar
    }

    /// Reveals the bar, pre-filling the search field with the editor's
    /// current selection (if any single-line text is selected) - the same
    /// "select a word, hit Ctrl+F" gesture most editors support - and
    /// focuses it with its text pre-selected for easy retyping.
    pub fn open(&self) {
        if let Some((start, end)) = self.buffer.selection_bounds() {
            let selected = self.buffer.text(&start, &end, false).to_string();
            if !selected.is_empty() && !selected.contains('\n') {
                self.search_entry.set_text(&selected);
            }
        }
        self.widget.set_reveal_child(true);
        self.search_entry.grab_focus();
        self.search_entry.select_region(0, -1);
        if !self.search_entry.text().is_empty() {
            self.select_from_cursor(true);
        }
    }

    fn close(&self) {
        self.widget.set_reveal_child(false);
        self.settings.set_search_text(None);
        self.view.grab_focus();
    }

    /// Finds and selects the next (`forward`) or previous match starting
    /// from wherever the cursor/selection currently is, scrolling it into
    /// view. GtkSourceView's own wrap-around handles cycling past the
    /// start/end of the document.
    fn select_from_cursor(&self, forward: bool) {
        let iter = self.buffer.iter_at_mark(&self.buffer.get_insert());
        let found = if forward { self.context.forward(&iter) } else { self.context.backward(&iter) };
        let Some((mut start, end, _wrapped)) = found else {
            return;
        };
        self.buffer.select_range(&end, &start);
        self.view.scroll_to_iter(&mut start, 0.1, false, 0.0, 0.0);
    }

    /// Replaces the currently-selected match and advances to whatever
    /// match now follows it.
    fn replace_current(&self) {
        let replacement = self.replace_entry.text().to_string();
        if let Some((mut start, mut end)) = self.buffer.selection_bounds() {
            let _ = self.context.replace(&mut start, &mut end, &replacement);
        }
        self.select_from_cursor(true);
    }

    fn replace_all(&self) {
        let replacement = self.replace_entry.text().to_string();
        let _ = self.context.replace_all(&replacement);
    }

    fn update_count_label(&self, count: i32) {
        self.count_label.set_label(&count_label_text(count));
    }
}

/// Finds the first occurrence of `needle` (case-sensitive, exact substring -
/// unlike `SearchBar`'s own live incremental search) anywhere in `buffer`,
/// selects it and scrolls it into view. `false` if it isn't found at all -
/// e.g. `aievaluate.rs`'s "jump to finding", where the model's quoted
/// anchor text may not match verbatim if it paraphrased instead of copying.
/// A one-shot `SearchContext` of its own, not `SearchBar`'s persistent one -
/// this shouldn't touch whatever the user currently has highlighted in the
/// search bar, if it's even open at all.
pub fn jump_to_text(view: &sourceview5::View, buffer: &sourceview5::Buffer, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let settings = sourceview5::SearchSettings::builder().search_text(needle).case_sensitive(true).build();
    let context = sourceview5::SearchContext::builder().buffer(buffer).settings(&settings).build();
    let Some((mut start, end, _wrapped)) = context.forward(&buffer.start_iter()) else {
        return false;
    };
    buffer.select_range(&end, &start);
    view.scroll_to_iter(&mut start, 0.1, false, 0.0, 0.0);
    true
}

/// Pure formatting for the match-count label - `-1` means GtkSourceView is
/// still scanning asynchronously and the count isn't known yet, shown as
/// nothing rather than a misleading "0".
fn count_label_text(count: i32) -> String {
    match count {
        n if n < 0 => String::new(),
        0 => tr("Kein Treffer"),
        1 => tr("1 Treffer"),
        n => tr("{n} Treffer").replace("{n}", &n.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_label_text_is_blank_while_still_scanning() {
        assert_eq!(count_label_text(-1), "");
    }

    #[test]
    fn count_label_text_reports_no_matches() {
        assert_eq!(count_label_text(0), "Kein Treffer");
    }

    #[test]
    fn count_label_text_uses_singular_for_one_match() {
        assert_eq!(count_label_text(1), "1 Treffer");
    }

    #[test]
    fn count_label_text_uses_plural_for_several_matches() {
        assert_eq!(count_label_text(5), "5 Treffer");
    }
}
