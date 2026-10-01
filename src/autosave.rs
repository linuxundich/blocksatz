//! Local crash-recovery: while the buffer holds unsaved text, a periodic
//! snapshot of the current article is written to a single recovery slot -
//! this app only ever has one document open at a time (see `window.rs`), so
//! there's no need for anything keyed by document. Piggybacks on the same
//! debounced buffer-`changed` tick that already drives the live preview
//! (`window.rs`'s `wire_live_preview`) rather than running its own timer.
//! Cleared the moment its content is safely on disk (a real save) or no
//! longer relevant (a fresh "Neu", or a different article was just opened).

use std::path::{Path, PathBuf};

use gtk4::glib;

use crate::document::{self, Document, Frontmatter};

fn snapshot_path() -> PathBuf {
    let mut dir = glib::user_config_dir();
    dir.push(crate::APP_DIR);
    dir.push("autosave.md");
    dir
}

/// Where the snapshot should be saved back to on a real Ctrl+S - a plain
/// sidecar file (same convention as `windowstate.rs`/`recentfiles.rs`)
/// rather than a pseudo frontmatter field, since it's an editor-local
/// detail, not part of the article's own WordPress-facing metadata.
fn source_path_marker() -> PathBuf {
    let mut dir = glib::user_config_dir();
    dir.push(crate::APP_DIR);
    dir.push("autosave_source.txt");
    dir
}

/// A recovery snapshot, ready to load into the editor.
pub struct Recovered {
    pub frontmatter: Frontmatter,
    pub body: String,
    pub original_path: Option<PathBuf>,
}

/// A snapshot with nothing but empty/whitespace text isn't worth ever
/// writing or recovering - a brand-new, still-untouched document would
/// otherwise manufacture a recovery prompt out of nothing on next launch.
fn worth_saving(body: &str) -> bool {
    !body.trim().is_empty()
}

/// Loads the recovery snapshot, if any.
pub fn recover() -> Option<Recovered> {
    let doc = document::read(&snapshot_path()).ok()?;
    let original_path = std::fs::read_to_string(source_path_marker()).ok().map(PathBuf::from);
    Some(Recovered {
        frontmatter: doc.frontmatter,
        body: doc.body,
        original_path,
    })
}

/// Writes (or refreshes) the recovery snapshot for the current article.
/// Best-effort - a failed autosave write isn't worth interrupting the user
/// over, unlike a failed explicit Save.
pub fn save(frontmatter: &Frontmatter, body: &str, original_path: Option<&Path>) {
    if !worth_saving(body) {
        return;
    }
    let doc = Document {
        frontmatter: frontmatter.clone(),
        body: body.to_string(),
    };
    let path = snapshot_path();
    let Some(parent) = path.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    if document::write(&path, &doc).is_err() {
        return;
    }
    let marker = source_path_marker();
    match original_path {
        Some(p) => {
            let _ = std::fs::write(marker, p.to_string_lossy().as_bytes());
        }
        None => {
            let _ = std::fs::remove_file(marker);
        }
    }
}

/// Discards the recovery snapshot.
pub fn clear() {
    let _ = std::fs::remove_file(snapshot_path());
    let _ = std::fs::remove_file(source_path_marker());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worth_saving_is_false_for_empty_or_whitespace_only_text() {
        assert!(!worth_saving(""));
        assert!(!worth_saving("   \n\t"));
    }

    #[test]
    fn worth_saving_is_true_for_real_content() {
        assert!(worth_saving("# Hallo"));
    }
}
