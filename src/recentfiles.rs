//! "Zuletzt geöffnet": remembers the most recently opened/saved article
//! paths across restarts, most-recent-first, so getting back to an article
//! doesn't need re-navigating the file picker every time - same plain
//! one-entry-per-line file convention as other small config files in this
//! app (see `wpsite.rs`).

use std::path::{Path, PathBuf};

use gtk4::glib;

/// Kept short on purpose - this is a quick-access list for articles worked
/// on recently, not a full history.
const MAX_ENTRIES: usize = 10;

fn recent_path() -> PathBuf {
    let mut path = glib::user_config_dir();
    path.push(crate::APP_DIR);
    path.push("recent_files.txt");
    path
}

fn parse(contents: &str) -> Vec<PathBuf> {
    contents.lines().filter(|line| !line.trim().is_empty()).map(PathBuf::from).collect()
}

fn serialize(entries: &[PathBuf]) -> String {
    entries.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join("\n")
}

/// Moves `path` to the front of `entries`, removing any earlier occurrence
/// first (so re-opening an already-tracked file doesn't create a second,
/// stale entry further down the list) and capping the result at
/// `MAX_ENTRIES`. Pure, so this - the only part with real logic - is
/// unit-testable without touching disk.
fn with_recorded(entries: &[PathBuf], path: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = entries.iter().filter(|existing| existing.as_path() != path).cloned().collect();
    entries.insert(0, path.to_path_buf());
    entries.truncate(MAX_ENTRIES);
    entries
}

/// The most-recently-opened/saved paths, most-recent-first, filtered to
/// only those that still exist on disk - a path that was since deleted or
/// moved would otherwise be a dead, unusable entry in the list.
pub fn load() -> Vec<PathBuf> {
    let contents = std::fs::read_to_string(recent_path()).unwrap_or_default();
    parse(&contents).into_iter().filter(|path| path.exists()).collect()
}

pub fn record(path: &Path) -> std::io::Result<()> {
    let contents = std::fs::read_to_string(recent_path()).unwrap_or_default();
    let updated = with_recorded(&parse(&contents), path);
    let out_path = recent_path();
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out_path, serialize(&updated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_parse_and_serialize() {
        let entries = vec![PathBuf::from("/home/toff/a.md"), PathBuf::from("/home/toff/b.md")];
        assert_eq!(parse(&serialize(&entries)), entries);
    }

    #[test]
    fn missing_file_yields_an_empty_list() {
        assert_eq!(parse(""), Vec::<PathBuf>::new());
    }

    #[test]
    fn recording_a_new_path_puts_it_first() {
        let existing = vec![PathBuf::from("/a.md"), PathBuf::from("/b.md")];
        let updated = with_recorded(&existing, Path::new("/c.md"));
        assert_eq!(updated, vec![PathBuf::from("/c.md"), PathBuf::from("/a.md"), PathBuf::from("/b.md")]);
    }

    #[test]
    fn recording_an_already_tracked_path_moves_it_to_front_without_duplicating() {
        let existing = vec![PathBuf::from("/a.md"), PathBuf::from("/b.md"), PathBuf::from("/c.md")];
        let updated = with_recorded(&existing, Path::new("/b.md"));
        assert_eq!(updated, vec![PathBuf::from("/b.md"), PathBuf::from("/a.md"), PathBuf::from("/c.md")]);
    }

    #[test]
    fn recording_caps_the_list_at_max_entries() {
        let existing: Vec<PathBuf> = (0..MAX_ENTRIES).map(|n| PathBuf::from(format!("/{n}.md"))).collect();
        let updated = with_recorded(&existing, Path::new("/new.md"));
        assert_eq!(updated.len(), MAX_ENTRIES);
        assert_eq!(updated[0], PathBuf::from("/new.md"));
    }
}
