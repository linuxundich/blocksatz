//! The Blocksatz library: one folder per article under
//! `~/Dokumente/Blocksatz/` (the XDG documents directory), each holding an
//! `artikel.md` plus that article's images. Every article being worked on
//! lives here - new ones from their first keystroke, posts opened from
//! WordPress from the moment they're opened - so nothing depends on the
//! user picking a file name, and the sidebar can list "what am I working
//! on" without a separate index. See `docs/gui-redesign.md`, section 3.
//!
//! A folder is a language pair (`docs/translations.md`): `artikel.md` is
//! the original, `artikel.<lang>.md` (e.g. `artikel.en.md`) its
//! translation, each with frontmatter of its own. The images are shared.
//!
//! Files opened from elsewhere keep working where they are; they just
//! aren't part of the library listing.

// Parts of the state model are only read by the library sidebar and the
// main action, which arrive in the next redesign phases.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use gtk4::glib;

use crate::document::{self, Document, PostStatus};
use crate::syncstate;

/// The article file inside each library folder.
pub const ARTICLE_FILE: &str = "artikel.md";

/// After this many days a published working copy without local changes
/// drops out of "In Arbeit" (the files stay on disk).
const RETIRE_AFTER_DAYS: i64 = 30;

/// `~/Dokumente/Blocksatz` (localized documents folder), falling back to
/// `~/Documents/Blocksatz` when no XDG documents directory is configured.
pub fn root() -> PathBuf {
    glib::user_special_dir(glib::UserDirectory::Documents)
        .unwrap_or_else(|| glib::home_dir().join("Documents"))
        .join("Blocksatz")
}

/// The file of language `lang` in a library folder: `artikel.md` for the
/// original (`None`), `artikel.en.md` for an English translation.
pub fn article_file(lang: Option<&str>) -> String {
    match lang {
        Some(lang) => format!("artikel.{lang}.md"),
        None => ARTICLE_FILE.to_string(),
    }
}

/// Which file of a pair `path` is: `Some(None)` for the original,
/// `Some(Some("en"))` for a translation, `None` for any other file name.
pub fn file_lang(path: &Path) -> Option<Option<String>> {
    let name = path.file_name()?.to_str()?;
    if name == ARTICLE_FILE {
        return Some(None);
    }
    let lang = name.strip_prefix("artikel.")?.strip_suffix(".md")?;
    (!lang.is_empty() && lang.len() <= 8 && lang.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')).then(|| Some(lang.to_string()))
}

/// The other-language file next to `path` (it may not exist yet).
pub fn sibling(path: &Path, lang: Option<&str>) -> Option<PathBuf> {
    Some(path.parent()?.join(article_file(lang)))
}

/// Whether `path` is an article file inside the library at `root`.
pub fn contains(root: &Path, path: &Path) -> bool {
    file_lang(path).is_some() && path.parent().and_then(Path::parent) == Some(root)
}

/// The current local time as the folder name of an untitled article,
/// e.g. `2026-10-01-2140`.
pub fn timestamp_name(now: &glib::DateTime) -> String {
    now.format("%Y-%m-%d-%H%M").map(|s| s.to_string()).unwrap_or_else(|_| "artikel".to_string())
}

/// `timestamp_name` for right now - the folder name of an article that
/// has no title yet.
pub fn untitled_name() -> String {
    glib::DateTime::now_local().map(|now| timestamp_name(&now)).unwrap_or_else(|_| "artikel".to_string())
}

/// A folder named by `timestamp_name` (optionally with a `-2`, `-3` ...
/// suffix) - one the user never named, and so one that may still be
/// renamed after the article's title once it has one.
pub fn is_auto_named(dir_name: &str) -> bool {
    let base = dir_name.get(..15).unwrap_or("");
    let rest = dir_name.get(15..).unwrap_or("");
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let parts: Vec<&str> = base.split('-').collect();
    base.len() == 15
        && parts.len() == 4
        && [4, 2, 2, 4].iter().zip(&parts).all(|(len, part)| part.len() == *len && digits(part))
        && (rest.is_empty() || rest.strip_prefix('-').is_some_and(digits))
}

/// The article's title as far as it's known: the frontmatter title, or
/// else a first `# ` heading line that's already finished (followed by a
/// line break) - never a heading still being typed, so a folder doesn't
/// get named after half a word.
pub fn title_hint(doc: &Document) -> Option<String> {
    if !doc.frontmatter.title.trim().is_empty() {
        return Some(doc.frontmatter.title.trim().to_string());
    }
    let (first_line, _) = doc.body.trim_start().split_once('\n')?;
    first_line.strip_prefix("# ").map(str::trim).filter(|t| !t.is_empty()).map(str::to_string)
}

/// A folder name under `root` derived from `name` that doesn't exist yet:
/// `name`, else `name-2`, `name-3` ...
fn unique_dir(root: &Path, name: &str) -> PathBuf {
    let candidate = root.join(name);
    if !candidate.exists() {
        return candidate;
    }
    (2..).map(|n| root.join(format!("{name}-{n}"))).find(|p| !p.exists()).expect("unbounded range")
}

/// Creates a new, empty library folder for an article and returns the
/// path its `artikel.md` should be written to. Named after `title` when
/// there is one, else after `fallback_name` (`timestamp_name`).
pub fn create_entry(root: &Path, title: Option<&str>, fallback_name: &str) -> std::io::Result<PathBuf> {
    let slug = title.map(document::slugify).filter(|s| !s.is_empty());
    let dir = unique_dir(root, slug.as_deref().unwrap_or(fallback_name));
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(ARTICLE_FILE))
}

/// Renames an auto-named library folder after the article's title, once
/// that's known. Returns the article's new path, or `None` when nothing
/// was renamed (not in the library, already named, no title yet).
pub fn rename_after_title(root: &Path, path: &Path, doc: &Document) -> std::io::Result<Option<PathBuf>> {
    // The folder is named after the original, never after a translation.
    if !contains(root, path) || file_lang(path) != Some(None) {
        return Ok(None);
    }
    let Some(dir) = path.parent() else { return Ok(None) };
    let Some(dir_name) = dir.file_name().and_then(|n| n.to_str()) else { return Ok(None) };
    if !is_auto_named(dir_name) {
        return Ok(None);
    }
    let Some(slug) = title_hint(doc).map(|t| document::slugify(&t)).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let target = unique_dir(root, &slug);
    std::fs::rename(dir, &target)?;
    Ok(Some(target.join(ARTICLE_FILE)))
}

/// One article in the library.
#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub document: Document,
}

/// The article files of one library folder: the original first, then the
/// translations by language.
fn folder_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(files) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut paths: Vec<PathBuf> = files.filter_map(Result::ok).map(|f| f.path()).filter(|p| p.is_file() && file_lang(p).is_some()).collect();
    paths.sort_by_key(|p| file_lang(p).flatten());
    paths
}

/// Every article file in the library - originals and translations -
/// unsorted. Unreadable files are skipped.
pub fn scan(root: &Path) -> Vec<Entry> {
    scan_pairs(root).into_iter().flat_map(|pair| pair.files).collect()
}

/// One library folder: an article and its translations.
#[derive(Debug, Clone)]
pub struct Pair {
    pub dir: PathBuf,
    /// The original (if the folder has one) first, then the translations.
    pub files: Vec<Entry>,
}

impl Pair {
    pub fn original(&self) -> Option<&Entry> {
        self.files.iter().find(|e| file_lang(&e.path) == Some(None))
    }

    /// The file a click on the folder opens: the original, else the first
    /// translation.
    pub fn primary(&self) -> &Entry {
        self.original().unwrap_or(&self.files[0])
    }

    pub fn get(&self, lang: Option<&str>) -> Option<&Entry> {
        self.files.iter().find(|e| file_lang(&e.path).flatten().as_deref() == lang)
    }
}

/// Every library folder with at least one readable article file.
pub fn scan_pairs(root: &Path) -> Vec<Pair> {
    let Ok(dirs) = std::fs::read_dir(root) else { return Vec::new() };
    dirs.filter_map(Result::ok).map(|entry| entry.path()).filter(|dir| dir.is_dir()).filter_map(|dir| read_pair(&dir)).collect()
}

/// The pair in one folder, if it holds a readable article file.
pub fn read_pair(dir: &Path) -> Option<Pair> {
    let files: Vec<Entry> = folder_files(dir).into_iter().filter_map(|path| document::read(&path).ok().map(|document| Entry { path, document })).collect();
    (!files.is_empty()).then(|| Pair { dir: dir.to_path_buf(), files })
}

/// Moves translations that still live in a folder of their own (as
/// Blocksatz 0.67 made them) next to their original, as
/// `artikel.<lang>.md`, together with their images. Skipped when the
/// original isn't in the library or already has a file for that language.
/// Returns how many were moved.
pub fn migrate_translations(root: &Path) -> usize {
    let pairs = scan_pairs(root);
    let mut moved = 0;
    for pair in &pairs {
        let (Some(original), [only]) = (pair.original(), pair.files.as_slice()) else { continue };
        let Some(link) = original.document.frontmatter.translation.as_ref() else { continue };
        let Some(target) = pairs.iter().find(|p| {
            p.dir != pair.dir
                && p.original().is_some_and(|o| {
                    let fm = &o.document.frontmatter;
                    o.document.frontmatter.translation.is_none() && fm.wp_post_id == Some(link.source_id) && fm.wp_site.as_deref().is_none_or(|s| s == link.source_site)
                })
        }) else {
            continue;
        };
        let lang = if link.lang.is_empty() { "en" } else { link.lang.as_str() };
        let dest = target.dir.join(article_file(Some(lang)));
        if dest.exists() {
            continue;
        }
        let mut doc = only.document.clone();
        if doc.frontmatter.lang.is_none() {
            doc.frontmatter.lang = Some(lang.to_string());
        }
        // Its own images move along unless the original has a file of
        // that name already (then it's the same picture).
        let Ok(files) = std::fs::read_dir(&pair.dir) else { continue };
        for file in files.filter_map(Result::ok).map(|f| f.path()) {
            if file == only.path {
                continue;
            }
            if let Some(name) = file.file_name() {
                let to = target.dir.join(name);
                if !to.exists() {
                    let _ = std::fs::rename(&file, &to);
                }
            }
        }
        if document::write(&dest, &doc).is_ok() && std::fs::remove_file(&only.path).is_ok() {
            let _ = gtk4::prelude::FileExt::trash(&gtk4::gio::File::for_path(&pair.dir), gtk4::gio::Cancellable::NONE);
            moved += 1;
        }
    }
    moved
}

/// The working copy of WordPress post `post_id` on site `site_id`, if the
/// library already has one. Working copies synced before `wp_site`
/// existed match any site.
pub fn find_by_post_id(root: &Path, site_id: &str, post_id: u64) -> Option<PathBuf> {
    scan(root)
        .into_iter()
        .find(|entry| {
            let fm = &entry.document.frontmatter;
            fm.wp_post_id == Some(post_id) && fm.wp_site.as_deref().is_none_or(|site| site == site_id)
        })
        .map(|entry| entry.path)
}

/// Whether a working copy has been published, unchanged, for longer than
/// `RETIRE_AFTER_DAYS` - such entries drop out of "In Arbeit".
pub fn is_retired(doc: &Document, now: &glib::DateTime) -> bool {
    let fm = &doc.frontmatter;
    if fm.wp_post_id.is_none() || fm.status != PostStatus::Publish || syncstate::has_local_changes(doc) {
        return false;
    }
    let Some(synced_at) = fm.wp_synced_at.as_deref().and_then(|s| glib::DateTime::from_iso8601(s, None).ok()) else {
        return false;
    };
    now.difference(&synced_at).as_days() >= RETIRE_AFTER_DAYS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Frontmatter;

    /// A fresh, empty directory under the system temp dir, removed again
    /// when the guard drops.
    struct TempRoot(PathBuf);
    impl TempRoot {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("blocksatz-library-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempRoot(dir)
        }
    }
    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn doc(title: &str, body: &str) -> Document {
        Document { frontmatter: Frontmatter { title: title.into(), ..Frontmatter::default() }, body: body.into() }
    }

    #[test]
    fn auto_named_folders_are_recognized() {
        assert!(is_auto_named("2026-10-01-2140"));
        assert!(is_auto_named("2026-10-01-2140-3"));
        assert!(!is_auto_named("raspberry-pi-5"));
        assert!(!is_auto_named("2026-10-01"));
        assert!(!is_auto_named("2026-10-01-2140-x"));
    }

    #[test]
    fn title_hint_ignores_a_heading_still_being_typed() {
        assert_eq!(title_hint(&doc("", "# Raspb")), None);
        assert_eq!(title_hint(&doc("", "# Raspberry Pi 5\nText")).as_deref(), Some("Raspberry Pi 5"));
        assert_eq!(title_hint(&doc("Aus dem Frontmatter", "# Anders\n")).as_deref(), Some("Aus dem Frontmatter"));
        assert_eq!(title_hint(&doc("", "Kein Titel\n")), None);
    }

    #[test]
    fn entries_get_unique_folders() {
        let root = TempRoot::new("unique");
        let a = create_entry(&root.0, Some("Über GNOME"), "x").unwrap();
        let b = create_entry(&root.0, Some("Über GNOME"), "x").unwrap();
        let c = create_entry(&root.0, None, "2026-10-01-2140").unwrap();
        assert_eq!(a, root.0.join("uber-gnome").join(ARTICLE_FILE));
        assert_eq!(b, root.0.join("uber-gnome-2").join(ARTICLE_FILE));
        assert_eq!(c, root.0.join("2026-10-01-2140").join(ARTICLE_FILE));
        assert!(contains(&root.0, &a));
        assert!(!contains(&root.0, Path::new("/tmp/artikel.md")));
    }

    #[test]
    fn auto_named_folder_is_renamed_once_a_title_is_known() {
        let root = TempRoot::new("rename");
        let path = create_entry(&root.0, None, "2026-10-01-2140").unwrap();
        std::fs::write(path.parent().unwrap().join("bild.png"), b"png").unwrap();

        assert_eq!(rename_after_title(&root.0, &path, &doc("", "# Halb")).unwrap(), None);
        let renamed = rename_after_title(&root.0, &path, &doc("", "# Fedora 45\n")).unwrap().unwrap();
        assert_eq!(renamed, root.0.join("fedora-45").join(ARTICLE_FILE));
        assert!(renamed.parent().unwrap().join("bild.png").exists());
        // Named folders are left alone.
        assert_eq!(rename_after_title(&root.0, &renamed, &doc("Neuer Titel", "")).unwrap(), None);
    }

    #[test]
    fn working_copy_is_found_by_post_id_and_site() {
        let root = TempRoot::new("find");
        let path = create_entry(&root.0, Some("Beitrag"), "x").unwrap();
        let mut d = doc("Beitrag", "Text");
        d.frontmatter.wp_post_id = Some(42);
        d.frontmatter.wp_site = Some("example.org".into());
        document::write(&path, &d).unwrap();

        assert_eq!(find_by_post_id(&root.0, "example.org", 42), Some(path));
        assert_eq!(find_by_post_id(&root.0, "anderes.blog", 42), None);
        assert_eq!(find_by_post_id(&root.0, "example.org", 43), None);
    }

    #[test]
    fn pair_files_are_recognized_by_name() {
        assert_eq!(file_lang(Path::new("/x/artikel.md")), Some(None));
        assert_eq!(file_lang(Path::new("/x/artikel.en.md")), Some(Some("en".into())));
        assert_eq!(file_lang(Path::new("/x/artikel.pt-br.md")), Some(Some("pt-br".into())));
        assert_eq!(file_lang(Path::new("/x/notizen.md")), None);
        assert_eq!(file_lang(Path::new("/x/artikel.EN.md")), None);
        assert_eq!(sibling(Path::new("/x/artikel.md"), Some("en")), Some(PathBuf::from("/x/artikel.en.md")));
    }

    #[test]
    fn a_folder_with_both_languages_is_one_pair() {
        let root = TempRoot::new("pairs");
        let path = create_entry(&root.0, Some("Beitrag"), "x").unwrap();
        document::write(&path, &doc("Beitrag", "Text")).unwrap();
        let en = sibling(&path, Some("en")).unwrap();
        document::write(&en, &doc("Post", "Text")).unwrap();
        assert!(contains(&root.0, &en));

        let pairs = scan_pairs(&root.0);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].primary().path, path);
        assert_eq!(pairs[0].get(Some("en")).map(|e| e.path.clone()), Some(en.clone()));
        assert_eq!(scan(&root.0).len(), 2);
        // A translation never renames the folder.
        assert_eq!(rename_after_title(&root.0, &en, &doc("Anders", "")).unwrap(), None);
    }

    #[test]
    fn a_translation_in_its_own_folder_moves_next_to_its_original() {
        let root = TempRoot::new("migrate");
        let original = create_entry(&root.0, Some("Beitrag"), "x").unwrap();
        let mut d = doc("Beitrag", "Text");
        d.frontmatter.wp_post_id = Some(42);
        d.frontmatter.wp_site = Some("example.org".into());
        document::write(&original, &d).unwrap();

        let old = create_entry(&root.0, Some("Post"), "x").unwrap();
        let mut t = doc("Post", "Text");
        t.frontmatter.wp_site = Some("example.org/en".into());
        t.frontmatter.translation = Some(document::TranslationLink { lang: "en".into(), source_site: "example.org".into(), source_id: 42, ..Default::default() });
        document::write(&old, &t).unwrap();
        std::fs::write(old.parent().unwrap().join("titel.png"), b"png").unwrap();

        assert_eq!(migrate_translations(&root.0), 1);
        let en = sibling(&original, Some("en")).unwrap();
        let moved = document::read(&en).unwrap();
        assert_eq!(moved.frontmatter.title, "Post");
        assert_eq!(moved.frontmatter.lang.as_deref(), Some("en"));
        assert!(original.parent().unwrap().join("titel.png").exists());
        assert!(!old.exists());
        assert_eq!(scan_pairs(&root.0).len(), 1);
        assert_eq!(migrate_translations(&root.0), 0);
    }

    #[test]
    fn published_unchanged_copies_retire_after_30_days() {
        let mut d = doc("Beitrag", "Text");
        d.frontmatter.wp_post_id = Some(1);
        d.frontmatter.status = PostStatus::Publish;
        syncstate::mark_synced(&mut d, "example.org", "2026-08-01T10:00:00", "2026-08-01T10:00:00Z");
        let soon = glib::DateTime::from_iso8601("2026-08-20T10:00:00Z", None).unwrap();
        let later = glib::DateTime::from_iso8601("2026-09-01T10:00:00Z", None).unwrap();
        assert!(!is_retired(&d, &soon));
        assert!(is_retired(&d, &later));

        let mut edited = d.clone();
        edited.body.push('!');
        assert!(!is_retired(&edited, &later));

        let mut draft = d.clone();
        draft.frontmatter.status = PostStatus::Draft;
        assert!(!is_retired(&draft, &later));
    }
}
