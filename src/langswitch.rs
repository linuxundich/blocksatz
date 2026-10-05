//! The language switch in the header bar (`docs/translations.md`): "DE ·
//! EN" for a library article, Alt+1 / Alt+2. Switching saves the open
//! file, opens the other file of the pair (`library::Pair`) and puts the
//! cursor into the same section and paragraph. Switching to a language
//! that has no file yet shows the translation's start page in place of
//! the editor (`translatedialog::start_page`).

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk4::{gio, glib};

use crate::document::Document;
use crate::i18n::tr;
use crate::window::{self, DocContext};
use crate::{library, syncstate, wpsite};

const ORIGINAL: &str = "original";
const TRANSLATION: &str = "translation";

pub struct LangSwitch {
    pub widget: adw::ToggleGroup,
    original: adw::Toggle,
    translation: adw::Toggle,
    ctx: DocContext,
    view: sourceview5::View,
    window: glib::WeakRef<adw::ApplicationWindow>,
    /// Set while `refresh` moves the toggle, so that isn't a switch.
    updating: Cell<bool>,
    /// Editor and start page (`editor_area` in `window.rs`).
    editor_area: gtk4::Stack,
    /// The folder whose start page is shown, and its original's post id
    /// when the page was built - a new id (just uploaded) rebuilds it.
    start: RefCell<Option<(PathBuf, Option<u64>)>>,
    weak: Weak<LangSwitch>,
}

impl LangSwitch {
    pub fn new(window: &adw::ApplicationWindow, ctx: &DocContext, view: &sourceview5::View, editor_area: &gtk4::Stack) -> Rc<Self> {
        let widget = adw::ToggleGroup::new();
        widget.add_css_class("round");
        let original = adw::Toggle::builder().name(ORIGINAL).label("DE").build();
        let translation = adw::Toggle::builder().name(TRANSLATION).label("EN").build();
        widget.add(original.clone());
        widget.add(translation.clone());
        widget.set_visible(false);

        let this = Rc::new_cyclic(|weak| LangSwitch {
            widget: widget.clone(),
            original,
            translation,
            ctx: ctx.clone(),
            view: view.clone(),
            window: window.downgrade(),
            updating: Cell::new(false),
            editor_area: editor_area.clone(),
            start: RefCell::new(None),
            weak: weak.clone(),
        });

        {
            let weak = this.weak.clone();
            widget.connect_active_name_notify(move |group| {
                let Some(this) = weak.upgrade() else { return };
                if this.updating.get() {
                    return;
                }
                let translation = group.active_name().as_deref() == Some(TRANSLATION);
                // The toggle follows the file actually open, set by `refresh`.
                this.refresh();
                this.switch_to(translation);
            });
        }
        for (name, translation) in [("lang-original", false), ("lang-translation", true)] {
            let action = gio::SimpleAction::new(name, None);
            let weak = this.weak.clone();
            action.connect_activate(move |_, _| {
                if let Some(this) = weak.upgrade() {
                    if this.widget.is_visible() {
                        this.switch_to(translation);
                    }
                }
            });
            window.add_action(&action);
        }
        {
            let weak = this.weak.clone();
            ctx.add_library_listener(Rc::new(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.refresh();
                }
            }));
        }
        this.refresh();
        this
    }

    /// The pair the open file belongs to, and which side is open.
    fn current(&self) -> Option<(PathBuf, bool)> {
        let path = self.ctx.current_path.borrow().clone()?;
        if !library::contains(&library::root(), &path) {
            return None;
        }
        let translation = library::file_lang(&path)?.is_some();
        Some((path, translation))
    }

    /// Shows the switch for a library article while a second blog exists,
    /// with the languages and states of both files.
    pub fn refresh(&self) {
        let current = self.current().filter(|_| wpsite::load_all().sites.len() > 1);
        let Some((path, on_translation)) = current else {
            self.hide_start();
            self.widget.set_visible(false);
            return;
        };
        // The start page belongs to one folder's original; anything else
        // open now (the new translation, another article) replaces it.
        let start = self.start.borrow().clone();
        if let Some((dir, post_id)) = start {
            if on_translation || path.parent() != Some(dir.as_path()) {
                self.hide_start();
            } else if self.ctx.frontmatter.borrow().wp_post_id != post_id {
                self.show_start();
            }
        }
        let on_translation = on_translation || self.start.borrow().is_some();
        let dir = path.parent().unwrap_or(&path).to_path_buf();
        let pair = library::read_pair(&dir);
        let open_doc = self.ctx.current_document();
        let file_of = |translation: bool| -> Option<(PathBuf, Document)> {
            if translation == on_translation {
                return Some((path.clone(), open_doc.clone()));
            }
            let pair = pair.as_ref()?;
            let entry = if translation { pair.files.iter().find(|e| library::file_lang(&e.path).is_some_and(|l| l.is_some())) } else { pair.original() }?;
            Some((entry.path.clone(), entry.document.clone()))
        };
        let original = file_of(false);
        let translated = file_of(true);
        let original_lang = original.as_ref().and_then(|(_, d)| d.frontmatter.lang.clone()).unwrap_or_else(|| "de".into());
        let translation_lang = translated.as_ref().and_then(|(p, d)| d.frontmatter.lang.clone().or_else(|| library::file_lang(p).flatten())).unwrap_or_else(|| "en".into());

        self.original.set_label(Some(&original_lang.to_uppercase()));
        self.translation.set_label(Some(&translation_lang.to_uppercase()));
        self.original.set_tooltip(&self.tooltip(&original_lang, original.as_ref().map(|(_, d)| d), "Alt+1"));
        self.translation.set_tooltip(&self.tooltip(&translation_lang, translated.as_ref().map(|(_, d)| d), "Alt+2"));

        self.updating.set(true);
        self.widget.set_active_name(Some(if on_translation { TRANSLATION } else { ORIGINAL }));
        self.updating.set(false);
        self.widget.set_visible(true);
    }

    fn tooltip(&self, lang: &str, doc: Option<&Document>, accel: &str) -> String {
        let state = match doc {
            Some(doc) => {
                let remote = self.ctx.remote_for(&doc.frontmatter);
                let site = doc.frontmatter.wp_site.clone().unwrap_or_else(|| wpsite::load().site_id());
                format!("{site} · {}", crate::mainaction::state_text(doc, syncstate::state(doc, &remote)))
            }
            None => tr("Noch keine Fassung - übersetzen"),
        };
        format!("{} ({accel})\n{state}", lang.to_uppercase())
    }

    /// Opens the other language of the pair at the same place, or starts
    /// the translation when it doesn't exist yet.
    fn switch_to(&self, translation: bool) {
        let Some((path, on_translation)) = self.current() else { return };
        // Back from the start page: the original is still open.
        if !translation && self.start.borrow().is_some() {
            self.hide_start();
            self.refresh();
            return;
        }
        if translation == on_translation {
            return;
        }
        let target = if translation {
            library::read_pair(path.parent().unwrap_or(&path)).and_then(|pair| pair.files.into_iter().find(|e| library::file_lang(&e.path).is_some_and(|l| l.is_some())).map(|e| e.path))
        } else {
            library::sibling(&path, None).filter(|p| p.is_file())
        };
        let Some(target) = target else {
            if translation {
                self.show_start();
                self.refresh();
            } else {
                window::show_toast(&self.ctx.toast_overlay, &tr("Zu dieser Übersetzung liegt das Original nicht im Ordner."));
            }
            return;
        };

        let text = self.ctx.buffer.text(&self.ctx.buffer.start_iter(), &self.ctx.buffer.end_iter(), false).to_string();
        let cursor = self.ctx.buffer.iter_at_mark(&self.ctx.buffer.get_insert()).offset();
        let place = position_in(&text, cursor as usize);
        window::open_document_at_path(target, &self.ctx);

        let text = self.ctx.buffer.text(&self.ctx.buffer.start_iter(), &self.ctx.buffer.end_iter(), false).to_string();
        let offset = offset_of(&text, place) as i32;
        let iter = self.ctx.buffer.iter_at_offset(offset);
        self.ctx.buffer.place_cursor(&iter);
        // After the new text is laid out, or the scroll lands short.
        let view = self.view.clone();
        let buffer = self.ctx.buffer.clone();
        glib::idle_add_local_once(move || {
            let mark = buffer.get_insert();
            view.scroll_to_mark(&mark, 0.0, true, 0.0, 0.25);
            view.grab_focus();
        });
    }
}

impl LangSwitch {
    /// (Re)builds the start page for the open original and shows it.
    fn show_start(&self) {
        let (Some(window), Some((path, _))) = (self.window.upgrade(), self.current()) else { return };
        if let Some(old) = self.editor_area.child_by_name("start") {
            self.editor_area.remove(&old);
        }
        let lang = library::read_pair(path.parent().unwrap_or(&path))
            .and_then(|pair| pair.files.iter().find_map(|e| library::file_lang(&e.path).flatten()))
            .or_else(|| wpsite::load_all().sites.iter().find_map(wpsite::site_lang))
            .unwrap_or_else(|| "en".into());
        let page = crate::translatedialog::start_page(&window, &self.ctx, &lang);
        self.editor_area.add_named(&page, Some("start"));
        self.editor_area.set_visible_child_name("start");
        *self.start.borrow_mut() = Some((path.parent().unwrap_or(&path).to_path_buf(), self.ctx.frontmatter.borrow().wp_post_id));
    }

    fn hide_start(&self) {
        if self.start.borrow_mut().take().is_some() {
            self.editor_area.set_visible_child_name("editor");
            if let Some(page) = self.editor_area.child_by_name("start") {
                self.editor_area.remove(&page);
            }
        }
    }
}

/// Where in an article a character offset is: (number of headings before
/// it, number of blocks after that heading). Both languages have the same
/// headings and the same paragraphs, so the pair maps onto the other file.
fn position_in(text: &str, char_offset: usize) -> (usize, usize) {
    let mut headings = 0;
    let mut blocks = 0;
    let mut in_block = false;
    let mut in_fence = false;
    let mut seen = 0;
    for line in text.split_inclusive('\n') {
        if seen > char_offset {
            break;
        }
        seen += line.chars().count();
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
        }
        if !in_fence && trimmed.starts_with('#') && !trimmed.starts_with("#[") {
            headings += 1;
            blocks = 0;
            in_block = false;
            continue;
        }
        if trimmed.is_empty() && !in_fence {
            in_block = false;
        } else if !in_block {
            in_block = true;
            blocks += 1;
        }
    }
    (headings, blocks)
}

/// The character offset where `position_in`'s place starts in `text` -
/// the closest one when the other file has fewer headings or blocks.
fn offset_of(text: &str, (headings, blocks): (usize, usize)) -> usize {
    let mut seen_headings = 0;
    let mut seen_blocks = 0;
    let mut in_block = false;
    let mut in_fence = false;
    let mut offset = 0;
    let mut best = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
        }
        let is_heading = !in_fence && trimmed.starts_with('#') && !trimmed.starts_with("#[");
        if is_heading {
            seen_headings += 1;
            seen_blocks = 0;
            in_block = false;
            if seen_headings > headings {
                break;
            }
            if seen_headings == headings {
                best = offset;
            }
        } else if trimmed.is_empty() && !in_fence {
            in_block = false;
        } else if !in_block {
            in_block = true;
            seen_blocks += 1;
            if seen_headings == headings && blocks > 0 && seen_blocks <= blocks {
                best = offset;
            }
        }
        offset += line.chars().count();
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    const DE: &str = "Erster Absatz.\n\nZweiter Absatz.\n\n## Teil\n\nDritter Absatz.\n\n```sh\n# kein Titel\n```\n\nVierter Absatz,\nzweite Zeile.\n";
    const EN: &str = "First paragraph.\n\nSecond paragraph.\n\n## Part\n\nThird paragraph.\n\n```sh\n# no heading\n```\n\nFourth paragraph,\nsecond line.\n";

    fn at(text: &str, needle: &str) -> usize {
        text[..text.find(needle).unwrap()].chars().count()
    }

    #[test]
    fn the_cursor_lands_in_the_same_paragraph_of_the_other_language() {
        for (de, en) in [("Erster", "First"), ("Zweiter", "Second"), ("## Teil", "## Part"), ("Dritter", "Third"), ("Vierter", "Fourth")] {
            let place = position_in(DE, at(DE, de) + 2);
            assert_eq!(offset_of(EN, place), at(EN, en), "{de}");
        }
        // The start of a paragraph and its second line mean that paragraph.
        assert_eq!(offset_of(EN, position_in(DE, at(DE, "Dritter"))), at(EN, "Third"));
        assert_eq!(offset_of(EN, position_in(DE, at(DE, "zweite Zeile"))), at(EN, "Fourth"));
    }

    #[test]
    fn a_shorter_other_file_gets_the_closest_place() {
        let place = position_in(DE, at(DE, "Vierter"));
        assert_eq!(offset_of("Nur ein Absatz.\n", place), 0);
        assert_eq!(offset_of("", place), 0);
    }
}
