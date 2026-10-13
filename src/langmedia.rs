//! Images and videos of a language version (`artikel.en.md`): which ones it
//! shares with the original and which ones it has of its own.
//!
//! **Naming convention on disk:** a file's language sits before its
//! extension, like the article's own (`artikel.en.md`):
//!
//! - `name.de.webp` - the German image, `name.en.webp` - the English one
//! - `name.webp` - no language mark: the same in every language
//!
//! A translation that points at a file marked for another language (or at
//! none) *shares* it with the original: nothing is uploaded again, the
//! image comes from the original's blog (`translate::SharedMedia`). A
//! translation that points at `name.en.webp` has an image *of its own* and
//! uploads it. This module lists the images of the open translation and
//! switches each one between the two, in the text, with one click.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use adw::prelude::*;
use gtk4::{gio, glib};

use crate::document::Document;
use crate::i18n::tr;
use crate::window::DocContext;
use crate::{library, media, textimport, translatedialog};

/// Language marks recognised in file names.
const LANGS: [&str; 6] = ["de", "en", "fr", "es", "it", "nl"];

/// Extensions looked for when an image exists in the other language under
/// a different format (a `.png` source next to its `.webp`).
const EXTENSIONS: [&str; 7] = ["webp", "png", "jpg", "jpeg", "mp4", "webm", "gif"];

/// `source` without its language mark, and the mark:
/// `shots/a.de.webp` → (`shots/a.webp`, `Some("de")`).
pub fn split_lang(source: &str) -> (String, Option<&'static str>) {
    let (dir, file) = source.rsplit_once('/').map_or(("", source), |(d, f)| (d, f));
    let prefix = if dir.is_empty() { String::new() } else { format!("{dir}/") };
    if let Some((rest, ext)) = file.rsplit_once('.') {
        if let Some((stem, mark)) = rest.rsplit_once('.') {
            if let Some(lang) = LANGS.iter().find(|l| **l == mark) {
                if !stem.is_empty() {
                    return (format!("{prefix}{stem}.{ext}"), Some(lang));
                }
            }
        }
    }
    (source.to_string(), None)
}

/// `source` marked for `lang`: `a.webp` / `a.de.webp` → `a.en.webp`.
pub fn with_lang(source: &str, lang: &str) -> String {
    let (base, _) = split_lang(source);
    let (dir, file) = base.rsplit_once('/').map_or(("", base.as_str()), |(d, f)| (d, f));
    let prefix = if dir.is_empty() { String::new() } else { format!("{dir}/") };
    match file.rsplit_once('.') {
        Some((stem, ext)) => format!("{prefix}{stem}.{lang}.{ext}"),
        None => format!("{prefix}{file}.{lang}"),
    }
}

/// Where an image stands in a translation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// The original's (marked for another language or for none, or already
    /// a URL of the original's blog).
    Shared,
    /// The translation's own, marked for its language.
    Own,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// As written in the text.
    pub source: String,
    pub kind: Kind,
    /// The file of the original it stands for, if known (a local name even
    /// when the text holds the original's upload URL).
    pub local: Option<String>,
    /// A file of the translation's language that already exists next to the
    /// article (relative name).
    pub variant: Option<String>,
    /// For an own image: what to write to share the original's again - its
    /// upload URL if it has one, else its file name.
    pub original_ref: Option<String>,
}

fn is_remote(source: &str) -> bool {
    source.starts_with("http://") || source.starts_with("https://")
}

/// The existing file of `lang` for `local` in `dir`: the same format
/// first, else any other.
pub fn find_variant(dir: &Path, local: &str, lang: &str) -> Option<String> {
    let direct = with_lang(local, lang);
    if dir.join(&direct).is_file() {
        return Some(direct);
    }
    let stem = direct.rsplit_once('.').map(|(s, _)| s.to_string())?;
    EXTENSIONS.iter().map(|ext| format!("{stem}.{ext}")).find(|candidate| dir.join(candidate).is_file())
}

/// The images and videos of a translation's `body`, each once.
pub fn entries(body: &str, lang: &str, original: Option<&Document>, dir: Option<&Path>) -> Vec<Entry> {
    // The original's media by upload URL and by language-free name.
    let mut by_url: HashMap<&str, &str> = HashMap::new();
    let mut by_base: HashMap<String, (&str, Option<&str>)> = HashMap::new();
    if let Some(original) = original {
        for item in &original.frontmatter.media {
            let url = item.wordpress.as_ref().map(|wp| wp.url.as_str()).filter(|u| !u.is_empty());
            if let Some(url) = url {
                by_url.insert(url, &item.source);
            }
            if split_lang(&item.source).1 != Some(lang_static(lang)) {
                by_base.insert(split_lang(&item.source).0, (&item.source, url));
            }
        }
    }
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for (source, _, _) in media::scan_images(body) {
        if source.is_empty() || seen.contains(&source) {
            continue;
        }
        seen.push(source.clone());
        let (base, mark) = split_lang(&source);
        let own = !is_remote(&source) && mark == Some(lang_static(lang));
        let local = if is_remote(&source) { by_url.get(source.as_str()).map(|s| s.to_string()) } else { Some(source.clone()) };
        let original_ref = own.then(|| by_base.get(&base)).flatten().map(|(file, url)| url.map_or_else(|| file.to_string(), str::to_string));
        let variant = match (&local, dir, own) {
            (Some(local), Some(dir), false) => find_variant(dir, local, lang),
            _ => None,
        };
        out.push(Entry { source, kind: if own { Kind::Own } else { Kind::Shared }, local, variant, original_ref });
    }
    out
}

fn lang_static(lang: &str) -> &'static str {
    LANGS.iter().find(|l| **l == lang).copied().unwrap_or("")
}

/// The body with every `from` → `to` of `map` written in.
pub fn switch(body: &str, map: &HashMap<String, String>) -> String {
    textimport::rewrite_sources(body, map)
}

/// When a language version starts from the original's text: images the
/// original has in this language already (`a.en.webp` next to `a.de.webp`)
/// are used right away. `body` names local files or the original's URLs.
pub fn use_existing_variants(body: &str, lang: &str, original: &Document, dir: &Path) -> String {
    let map: HashMap<String, String> = entries(body, lang, Some(original), Some(dir)).into_iter().filter_map(|e| e.variant.map(|v| (e.source, v))).collect();
    switch(body, &map)
}

/// "Bilder dieser Fassung": the translation's images, switchable between
/// the original's and its own.
pub fn open(window: &adw::ApplicationWindow, ctx: &DocContext) {
    let Some(path) = ctx.current_path.borrow().clone() else { return };
    let Some(Some(lang)) = library::file_lang(&path) else {
        crate::window::show_toast(&ctx.toast_overlay, &tr("Das gibt es nur in einer Sprachfassung, nicht im Original."));
        return;
    };
    let Some(dir) = path.parent().map(Path::to_path_buf) else { return };
    let doc = ctx.current_document();
    let original = translatedialog::find_original(&doc, Some(&path)).map(|(_, d)| d);

    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    group.set_title(&tr("Bilder und Videos"));
    group.set_description(Some(
        &tr("Mit der Sprache im Dateinamen: bild.de.webp für Deutsch, bild.en.webp für Englisch, bild.webp für beide. Was nicht auf {lang} endet, kommt aus dem Blog des Originals und wird nicht noch einmal hochgeladen.").replace("{lang}", &format!(".{lang}")),
    ));
    page.add(&group);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page));
    let dialog = adw::Dialog::builder().title(tr("Bilder dieser Fassung")).content_width(620).content_height(560).child(&toolbar).build();

    let rows: std::rc::Rc<std::cell::RefCell<Vec<adw::ActionRow>>> = Default::default();
    let rebuild: std::rc::Rc<std::cell::RefCell<Option<std::rc::Rc<dyn Fn()>>>> = Default::default();
    let build = {
        let (ctx, group, rows, window, dir, lang, original, rebuild) = (ctx.clone(), group.clone(), rows.clone(), window.clone(), dir.clone(), lang.clone(), original.clone(), rebuild.clone());
        std::rc::Rc::new(move || {
            for row in rows.borrow_mut().drain(..) {
                group.remove(&row);
            }
            let buffer = ctx.buffer.clone();
            let body = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
            let list = entries(&body, &lang, original.as_ref(), Some(&dir));
            if list.is_empty() {
                let row = adw::ActionRow::builder().title(tr("Diese Fassung hat keine Bilder oder Videos.")).build();
                group.add(&row);
                rows.borrow_mut().push(row);
            }
            for entry in list {
                let row = adw::ActionRow::builder().title(glib::markup_escape_text(&display_name(&entry.source))).title_lines(1).build();
                row.set_subtitle(&match entry.kind {
                    Kind::Shared => tr("Wie im Original – aus dessen Blog, kein neuer Upload"),
                    Kind::Own => tr("Eigene Fassung – wird in den Blog dieser Sprache hochgeladen"),
                });
                if !is_remote(&entry.source) {
                    let thumb = gtk4::Picture::for_filename(dir.join(&entry.source));
                    thumb.set_size_request(56, 40);
                    thumb.set_content_fit(gtk4::ContentFit::Cover);
                    row.add_prefix(&thumb);
                }
                let again = rebuild.clone();
                let apply: std::rc::Rc<dyn Fn(HashMap<String, String>)> = {
                    let ctx = ctx.clone();
                    std::rc::Rc::new(move |map| {
                        replace_in_buffer(&ctx.buffer, &map);
                        if let Some(rebuild) = again.borrow().as_ref() {
                            rebuild();
                        }
                    })
                };
                match entry.kind {
                    Kind::Shared => {
                        if let Some(variant) = entry.variant.clone() {
                            let button = gtk4::Button::builder().label(tr("{file} nehmen").replace("{file}", &display_name(&variant))).valign(gtk4::Align::Center).build();
                            button.add_css_class("suggested-action");
                            let (source, apply) = (entry.source.clone(), apply.clone());
                            button.connect_clicked(move |_| apply(HashMap::from([(source.clone(), variant.clone())])));
                            row.add_suffix(&button);
                        }
                        let button = gtk4::Button::builder().label(tr("Eigene Fassung wählen …")).valign(gtk4::Align::Center).build();
                        let (source, local, apply, window, dir, lang) = (entry.source.clone(), entry.local.clone(), apply.clone(), window.clone(), dir.clone(), lang.clone());
                        button.connect_clicked(move |_| choose_own(&window, &dir, &lang, &source, local.as_deref(), apply.clone()));
                        row.add_suffix(&button);
                    }
                    Kind::Own => {
                        let button = gtk4::Button::builder().label(tr("Wie im Original")).valign(gtk4::Align::Center).sensitive(entry.original_ref.is_some()).build();
                        if entry.original_ref.is_none() {
                            button.set_tooltip_text(Some(&tr("Das Original hat keine Datei dazu (gleicher Name ohne .{lang})").replace("{lang}", &lang)));
                        }
                        if let Some(original_ref) = entry.original_ref.clone() {
                            let source = entry.source.clone();
                            button.connect_clicked(move |_| apply(HashMap::from([(source.clone(), original_ref.clone())])));
                        }
                        row.add_suffix(&button);
                    }
                }
                group.add(&row);
                rows.borrow_mut().push(row);
            }
        })
    };
    *rebuild.borrow_mut() = Some(build.clone());
    build();
    dialog.present(Some(window));
}

fn display_name(source: &str) -> String {
    source.rsplit('/').next().unwrap_or(source).to_string()
}

/// Writes `map` into the text as one step (one Ctrl+Z), keeping the cursor.
fn replace_in_buffer(buffer: &sourceview5::Buffer, map: &HashMap<String, String>) {
    let old = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
    let new = switch(&old, map);
    if new == old {
        return;
    }
    let offset = buffer.iter_at_mark(&buffer.get_insert()).offset();
    buffer.begin_user_action();
    buffer.set_text(&new);
    buffer.end_user_action();
    buffer.place_cursor(&buffer.iter_at_offset(offset.min(buffer.char_count())));
}

/// Picks the picture or video made for this language and puts it next to
/// the article as `name.<lang>.ext` (a copy unless it is there already).
fn choose_own(window: &adw::ApplicationWindow, dir: &Path, lang: &str, source: &str, local: Option<&str>, apply: std::rc::Rc<dyn Fn(HashMap<String, String>)>) {
    let filter = gtk4::FileFilter::new();
    filter.add_mime_type("image/*");
    filter.add_mime_type("video/*");
    filter.set_name(Some(&tr("Bilder und Videos")));
    let filters = gio::ListStore::new::<gtk4::FileFilter>();
    filters.append(&filter);
    let dialog = gtk4::FileDialog::builder().title(tr("Bild für diese Sprache wählen")).filters(&filters).build();
    dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
    if let Some(local) = local {
        dialog.set_initial_name(Some(&display_name(&with_lang(local, lang))));
    }
    let (dir, lang, source) = (dir.to_path_buf(), lang.to_string(), source.to_string());
    dialog.open(Some(window), gio::Cancellable::NONE, move |result| {
        let Ok(file) = result else { return };
        let Some(picked) = file.path() else { return };
        match import_variant(&dir, &lang, &picked) {
            Ok(name) => apply(HashMap::from([(source.clone(), name)])),
            Err(err) => eprintln!("Blocksatz: {err}"),
        }
    });
}

/// `picked` as a file of the article's folder marked for `lang`; returns
/// its name there.
fn import_variant(dir: &Path, lang: &str, picked: &Path) -> Result<String, String> {
    let file_name = picked.file_name().and_then(|n| n.to_str()).ok_or_else(|| tr("Ungültiger Dateiname."))?;
    let name = if split_lang(file_name).1 == Some(lang_static(lang)) { file_name.to_string() } else { with_lang(file_name, lang) };
    let target: PathBuf = dir.join(&name);
    if picked != target {
        std::fs::copy(picked, &target).map_err(|err| err.to_string())?;
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Frontmatter;

    fn original() -> Document {
        let body = "![A](a.de.webp)\n\n![B](b.webp)\n";
        let mut frontmatter = Frontmatter { media: media::reconcile(&[], body), ..Frontmatter::default() };
        frontmatter.media[0].wordpress = Some(media::WordPressMediaRef { media_id: 1, url: "https://x.de/a.de-1024.webp".into(), content_hash: String::new(), width: 0, height: 0, size_slug: None });
        Document { frontmatter, body: body.into() }
    }

    #[test]
    fn language_marks_are_split_off_and_put_on() {
        assert_eq!(split_lang("shots/a.de.webp"), ("shots/a.webp".to_string(), Some("de")));
        assert_eq!(split_lang("a.webp"), ("a.webp".to_string(), None));
        assert_eq!(split_lang("a.v2.png"), ("a.v2.png".to_string(), None));
        assert_eq!(with_lang("shots/a.de.webp", "en"), "shots/a.en.webp");
        assert_eq!(with_lang("a.webp", "en"), "a.en.webp");
        assert_eq!(with_lang("a.en.webp", "en"), "a.en.webp");
    }

    #[test]
    fn a_translation_shares_what_is_not_marked_for_its_language() {
        let list = entries("![x](a.de.webp)\n\n![y](b.webp)\n\n![z](c.en.webp)\n\n![x2](a.de.webp)\n", "en", Some(&original()), None);
        assert_eq!(list.iter().map(|e| e.kind).collect::<Vec<_>>(), vec![Kind::Shared, Kind::Shared, Kind::Own]);
        assert_eq!(list.len(), 3, "each image once");
    }

    #[test]
    fn an_own_image_knows_the_originals_to_share_again() {
        let list = entries("![x](a.en.webp)\n\n![y](b.en.webp)\n", "en", Some(&original()), None);
        assert_eq!(list[0].original_ref.as_deref(), Some("https://x.de/a.de-1024.webp"), "uploaded: its URL");
        assert_eq!(list[1].original_ref.as_deref(), Some("b.webp"), "not uploaded: its file");
    }

    #[test]
    fn the_originals_upload_url_stands_for_its_local_file() {
        let list = entries("![x](https://x.de/a.de-1024.webp)\n", "en", Some(&original()), None);
        assert_eq!(list[0].kind, Kind::Shared);
        assert_eq!(list[0].local.as_deref(), Some("a.de.webp"));
    }

    #[test]
    fn an_existing_file_of_the_language_is_found_in_any_format() {
        let dir = std::env::temp_dir().join(format!("blocksatz-langmedia-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.en.png"), b"x").unwrap();
        assert_eq!(find_variant(&dir, "a.de.webp", "en").as_deref(), Some("a.en.png"));
        std::fs::write(dir.join("a.en.webp"), b"x").unwrap();
        assert_eq!(find_variant(&dir, "a.de.webp", "en").as_deref(), Some("a.en.webp"));
        assert_eq!(find_variant(&dir, "b.de.webp", "en"), None);
        let body = use_existing_variants("![x](a.de.webp)\n![y](https://x.de/a.de-1024.webp)\n![z](b.de.webp)\n", "en", &original(), &dir);
        assert!(body.contains("](a.en.webp)") && body.contains("](b.de.webp)"), "{body}");
        assert!(!body.contains("x.de/a.de"), "the URL of a shared image with an English file is swapped too: {body}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
