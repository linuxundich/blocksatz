//! "Tastenkürzel" - this app's keyboard shortcuts in a native
//! `Gtk.ShortcutsWindow`. Wired up via `Gtk.ApplicationWindow`'s standard
//! `help-overlay` property (`window.rs`), so GTK itself provides the
//! conventional `win.show-help-overlay` action - no bespoke action or
//! `open()` function needed here, just the window to hand it.

use crate::i18n::tr;

pub fn build() -> gtk4::ShortcutsWindow {
    let window = gtk4::ShortcutsWindow::builder().modal(true).build();

    let section = gtk4::ShortcutsSection::builder().section_name("main").build();
    // Every title below is `tr("...")` right here at its own literal - not
    // built from a shared table and translated once through a variable -
    // so `xgettext --keyword=tr` (see `po/README.md`) can actually find
    // it: it only extracts a call whose argument is a string literal, not
    // one that's been looked up into a local first.
    section.add_group(&group(
        tr("Allgemein"),
        &[
            (tr("Neuer Artikel"), "<Ctrl>N"),
            (tr("Neue Seite"), "<Ctrl><Alt>N"),
            (tr("Öffnen"), "<Ctrl>O"),
            (tr("Von WordPress öffnen"), "<Ctrl><Shift>O"),
            (tr("Speichern"), "<Ctrl>S"),
            (tr("Artikel exportieren"), "<Ctrl><Shift>P"),
            (tr("Medienverwaltung"), "<Ctrl><Shift>M"),
            (tr("WordPress-Mediathek"), "<Ctrl><Shift>L"),
            (tr("Fokus-Schreibmodus"), "<Ctrl><Shift>F"),
            (tr("Einstellungen"), "<Ctrl>comma"),
        ],
    ));
    section.add_group(&group(
        tr("Editor"),
        &[
            (tr("Fett"), "<Ctrl>B"),
            (tr("Kursiv"), "<Ctrl>I"),
            (tr("Link einfügen"), "<Ctrl>K"),
            (tr("Bild aus der Zwischenablage einfügen"), "<Ctrl>V"),
            (tr("Suchen und Ersetzen"), "<Ctrl>F"),
        ],
    ));
    window.add_section(&section);
    window
}

fn group(title: String, shortcuts: &[(String, &str)]) -> gtk4::ShortcutsGroup {
    let group = gtk4::ShortcutsGroup::builder().title(title).build();
    for (title, accelerator) in shortcuts {
        group.add_shortcut(&gtk4::ShortcutsShortcut::builder().title(title.as_str()).accelerator(*accelerator).build());
    }
    group
}
