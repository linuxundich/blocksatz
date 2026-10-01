//! "Tastenkürzel" - this app's keyboard shortcuts in an
//! `AdwShortcutsDialog` (libadwaita 1.8+, the successor of the deprecated
//! `Gtk.ShortcutsWindow`), opened by `win.shortcuts` (Ctrl+?).

use adw::prelude::*;

use crate::i18n::tr;

/// Shows the shortcuts dialog over `parent`.
pub fn open(parent: &impl IsA<gtk4::Widget>) {
    let dialog = adw::ShortcutsDialog::new();
    // Every title below is `tr("...")` right here at its own literal - not
    // built from a shared table and translated once through a variable -
    // so `xgettext --keyword=tr` (see `po/README.md`) can actually find
    // it: it only extracts a call whose argument is a string literal, not
    // one that's been looked up into a local first.
    dialog.add(section(
        tr("Artikel"),
        &[
            (tr("Neuer Artikel"), "<Ctrl>N"),
            (tr("Neue Seite"), "<Ctrl><Alt>N"),
            (tr("Datei öffnen"), "<Ctrl>O"),
            (tr("Entwürfe im Blog"), "<Ctrl><Shift>O"),
            (tr("Sofort speichern"), "<Ctrl>S"),
            (tr("Veröffentlichen (Freigabe-Prüfung)"), "<Ctrl><Shift>P"),
            (tr("KI-Artikel schreiben"), "<Ctrl><Shift>G"),
        ],
    ));
    dialog.add(section(
        tr("Ansicht"),
        &[
            (tr("Seitenbereich ein-/ausblenden"), "F9"),
            (tr("Beitragseigenschaften"), "<Alt>Return"),
            (tr("Fokus-Schreibmodus"), "<Ctrl><Shift>F"),
            (tr("Medienverwaltung"), "<Ctrl><Shift>M"),
            (tr("WordPress-Mediathek"), "<Ctrl><Shift>L"),
        ],
    ));
    dialog.add(section(
        tr("Editor"),
        &[
            (tr("Fett"), "<Ctrl>B"),
            (tr("Kursiv"), "<Ctrl>I"),
            (tr("Link einfügen"), "<Ctrl>K"),
            (tr("Bild aus der Zwischenablage einfügen"), "<Ctrl>V"),
            (tr("Suchen und Ersetzen"), "<Ctrl>F"),
        ],
    ));
    dialog.add(section(
        tr("Allgemein"),
        &[(tr("Einstellungen"), "<Ctrl>comma"), (tr("Tastenkürzel"), "<Ctrl>question")],
    ));
    dialog.present(Some(parent));
}

fn section(title: String, shortcuts: &[(String, &str)]) -> adw::ShortcutsSection {
    let section = adw::ShortcutsSection::new(Some(&title));
    for (title, accelerator) in shortcuts {
        section.add(adw::ShortcutsItem::new(title, accelerator));
    }
    section
}
