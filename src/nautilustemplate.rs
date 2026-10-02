//! A "Blocksatz-Artikel.md" in the templates folder (`~/Vorlagen`), so
//! Nautilus offers a new article under "Neues Dokument" - written once,
//! on the first launch that finds the folder. A marker file remembers
//! that, so a template the user deleted stays deleted.

use std::path::{Path, PathBuf};

use gtk4::glib;

use crate::i18n::tr;

fn marker_path() -> PathBuf {
    let mut path = glib::user_config_dir();
    path.push(crate::APP_DIR);
    path.push("nautilus_template_done");
    path
}

/// What a new article from the template starts with.
fn template_text() -> String {
    format!("# {}\n\n{}\n", tr("Titel"), tr("Der erste Absatz."))
}

/// Writes the template into `dir` unless a file of that name is there.
/// `Some(path)` when the folder exists (written now or already there).
fn write_into(dir: &Path) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    let path = dir.join(format!("{}.md", tr("Blocksatz-Artikel")));
    if !path.exists() {
        std::fs::write(&path, template_text()).ok()?;
    }
    Some(path)
}

/// Once per user: the template in the XDG templates folder, if there is
/// one. Errors are silent - it's a convenience.
pub fn install_once() {
    let marker = marker_path();
    if marker.exists() {
        return;
    }
    let Some(dir) = glib::user_special_dir(glib::UserDirectory::Templates) else { return };
    if write_into(&dir).is_some() {
        if let Some(parent) = marker.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&marker, "1");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_template_once_and_keeps_an_existing_file() {
        let dir = std::env::temp_dir().join(format!("blocksatz-templates-{}", std::process::id()));
        assert_eq!(write_into(&dir), None);
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_into(&dir).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("# "));
        std::fs::write(&path, "eigene Vorlage").unwrap();
        write_into(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "eigene Vorlage");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
