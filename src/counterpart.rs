//! The other language of a language pair (`library::Pair`) as a view in
//! the right-hand pane: "EN" while the original is open, "DE" while the
//! translation is. It renders the other file like the preview does and
//! follows the editor section by section (`langswitch::map_line`), so the
//! paragraph being worked on is always next to its counterpart.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk4::glib;

use crate::document::{self, Document};
use crate::i18n::tr;
use crate::preview::PreviewPane;
use crate::window::DocContext;
use crate::{langswitch, library, translate};

/// Name of the view in the right-hand pane's stack and of its toggle.
pub const PAGE: &str = "counterpart";

/// Links in the notes above changed sections (`note_html`).
const ACTION_SCHEME: &str = "blocksatz-action:";

#[derive(Default, PartialEq)]
struct Marks {
    changed: Vec<(usize, usize)>,
    done: Vec<(usize, usize)>,
    /// 1-based line of the section, HTML of its note.
    notes: Vec<(usize, String)>,
    key: String,
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The note above changed section `index`: what changed in it since the
/// translation was last brought up to date (when that state is known), and
/// the buttons "Kopieren" and "Erledigt".
fn note_html(index: usize, old: Option<&str>, current: &str) -> String {
    let title = if old.is_some() { tr("Seit der Übersetzung geändert") } else { tr("Neu oder geändert") };
    let mut html = format!(
        "<div class=\"bar\"><b>{}</b><a href=\"{ACTION_SCHEME}copy/{index}\">{}</a><a href=\"{ACTION_SCHEME}done/{index}\">{}</a></div>",
        escape(&title),
        escape(&tr("Kopieren")),
        escape(&tr("Erledigt"))
    );
    if let Some(old) = old {
        html.push_str("<div class=\"diff\">");
        let diff = translate::word_diff(old.trim(), current.trim());
        let last = diff.len().saturating_sub(1);
        for (n, (kind, text)) in diff.iter().enumerate() {
            match kind {
                translate::Change::Inserted => html.push_str(&format!("<ins>{}</ins>", escape(text))),
                // Its whitespace is in the inserted text that follows.
                translate::Change::Deleted => html.push_str(&format!("<del>{}</del> ", escape(text.trim_end()))),
                // Unchanged text only as context around the changes.
                translate::Change::Same => html.push_str(&escape(&context(text, n == 0, n == last))),
            }
        }
        html.push_str("</div>");
    }
    html
}

/// Unchanged words between changes, shortened to a few on each side.
fn context(text: &str, first: bool, last: bool) -> String {
    const KEEP: usize = 6;
    let words: Vec<&str> = text.split_inclusive(char::is_whitespace).collect();
    if words.len() <= KEEP * 2 + 2 {
        return text.to_string();
    }
    let head: String = if first { String::new() } else { words[..KEEP].concat() };
    let tail: String = if last { String::new() } else { words[words.len() - KEEP..].concat() };
    match (first, last) {
        (true, true) => String::new(),
        (true, false) => format!("… {tail}"),
        (false, true) => format!("{head}…"),
        (false, false) => format!("{head}… {tail}"),
    }
}

pub struct Counterpart {
    pub pane: Rc<PreviewPane>,
    /// The section toggle; in `section_toggles` only while there is a
    /// counterpart.
    toggle: adw::Toggle,
    section_toggles: adw::ToggleGroup,
    view_stack: adw::ViewStack,
    present: Cell<bool>,
    /// The file shown and its text as last rendered.
    shown: RefCell<Option<(PathBuf, String)>>,
    ctx: DocContext,
    view: sourceview5::View,
    sync_pending: Cell<bool>,
    /// What the view marks while a translation is open (empty while the
    /// original is): the original's sections changed since the translation,
    /// those already translated, and a note above each changed one.
    marks: RefCell<Marks>,
    weak: Weak<Counterpart>,
}

impl Counterpart {
    pub fn new(ctx: &DocContext, view: &sourceview5::View, scroller: &gtk4::ScrolledWindow, view_stack: &adw::ViewStack, section_toggles: &adw::ToggleGroup) -> Rc<Self> {
        let pane = Rc::new(PreviewPane::new());
        view_stack.add_named(&pane.widget, Some(PAGE));
        let toggle = adw::Toggle::builder().name(PAGE).label("EN").build();

        let this = Rc::new_cyclic(|weak| Counterpart {
            pane,
            toggle,
            section_toggles: section_toggles.clone(),
            view_stack: view_stack.clone(),
            present: Cell::new(false),
            shown: RefCell::new(None),
            ctx: ctx.clone(),
            view: view.clone(),
            sync_pending: Cell::new(false),
            marks: RefCell::new(Marks::default()),
            weak: weak.clone(),
        });

        {
            let weak = this.weak.clone();
            ctx.add_library_listener(Rc::new(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.refresh();
                }
            }));
        }
        // Follows the editor's scrolling and cursor, once per frame.
        {
            let weak = this.weak.clone();
            scroller.vadjustment().connect_value_changed(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.schedule_sync();
                }
            });
        }
        {
            let weak = this.weak.clone();
            ctx.buffer.connect_cursor_position_notify(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.schedule_sync();
                }
            });
        }
        {
            let weak = this.weak.clone();
            view_stack.connect_visible_child_name_notify(move |stack| {
                if stack.visible_child_name().as_deref() == Some(PAGE) {
                    if let Some(this) = weak.upgrade() {
                        this.schedule_sync();
                    }
                }
            });
        }
        this.refresh();
        this
    }

    /// The other file of the open file's pair, if it exists.
    fn other_file(&self) -> Option<PathBuf> {
        let path = self.ctx.current_path.borrow().clone()?;
        if !library::contains(&library::root(), &path) {
            return None;
        }
        let pair = library::read_pair(path.parent()?)?;
        let on_translation = library::file_lang(&path)?.is_some();
        let other = if on_translation { pair.original() } else { pair.files.iter().find(|e| library::file_lang(&e.path).is_some_and(|l| l.is_some())) }?;
        Some(other.path.clone())
    }

    /// Shows or hides the view and re-renders it when the other file
    /// changed on disk.
    pub fn refresh(&self) {
        let other = self.other_file().and_then(|p| document::read(&p).ok().map(|d| (p, d)));
        let Some((path, doc)) = other else {
            *self.shown.borrow_mut() = None;
            if self.present.replace(false) {
                if self.view_stack.visible_child_name().as_deref() == Some(PAGE) {
                    self.view_stack.set_visible_child_name("preview");
                }
                self.section_toggles.remove(&self.toggle);
            }
            return;
        };
        let lang = doc.frontmatter.lang.clone().or_else(|| library::file_lang(&path).flatten()).unwrap_or_else(|| "de".into());
        self.toggle.set_label(Some(&lang.to_uppercase()));
        if !self.present.replace(true) {
            self.section_toggles.add(self.toggle.clone());
        }
        // Showing the original next to its translation: what changed since,
        // what's translated already.
        let marks = if self.ctx.frontmatter.borrow().translation.is_some() { self.marks_for(&doc) } else { Marks::default() };
        let marks_changed = *self.marks.borrow() != marks;
        *self.marks.borrow_mut() = marks;
        if marks_changed {
            self.schedule_sync();
        }
        let unchanged = self.shown.borrow().as_ref().is_some_and(|(p, text)| *p == path && *text == document::serialize(&doc));
        if unchanged {
            return;
        }
        *self.shown.borrow_mut() = Some((path.clone(), document::serialize(&doc)));
        self.render(&path, &doc);
        self.schedule_sync();
        // Once the new page has loaded, or the highlight lands on nothing.
        let weak = self.weak.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
            if let Some(this) = weak.upgrade() {
                this.schedule_sync();
            }
        });
    }

    /// Marks for `original` next to the open translation.
    fn marks_for(&self, original: &Document) -> Marks {
        let translation = self.ctx.current_document();
        let Some(link) = translation.frontmatter.translation.clone() else { return Marks::default() };
        let hashed = translate::body_with_uploaded_images(original);
        let sections = translate::split_sections(&hashed);
        // `body_with_uploaded_images` only swaps URLs: the lines are `body`'s.
        let ranges = translate::section_ranges(&original.body);
        let changed_index: Vec<usize> = sections.iter().enumerate().filter(|(_, s)| !s.trim().is_empty() && !link.source_sections.contains(&translate::section_hash(s))).map(|(i, _)| i).collect();
        let basis = self.ctx.current_path.borrow().as_deref().map(crate::translatedialog::load_basis).unwrap_or_default();
        // Translated: neither the original's text now nor as it was, and
        // not reading as German.
        let theirs = translate::split_sections(&translation.body);
        let translated: Vec<bool> = translate::translated_sections(&hashed, &translation.body)
            .into_iter()
            .enumerate()
            .map(|(i, t)| {
                let Some(mine) = theirs.get(i) else { return false };
                let old = translate::old_section(&basis, &link.source_sections, i, &sections[i]);
                t && old.is_none_or(|o| o.trim() != mine.trim()) && !translate::reads_like_source(mine, "de")
            })
            .collect();
        let notes: Vec<(usize, String)> = changed_index
            .iter()
            .filter_map(|&i| {
                let (start, _) = *ranges.get(i)?;
                let old = translate::old_section(&basis, &link.source_sections, i, &sections[i]);
                Some((start, note_html(i, old.map(String::as_str), &sections[i])))
            })
            .collect();
        let key = format!("{:?}", notes.iter().map(|(l, h)| (l, translate::section_hash(h))).collect::<Vec<_>>());
        Marks {
            changed: changed_index.iter().filter_map(|&i| ranges.get(i).copied()).collect(),
            done: translated.iter().enumerate().filter(|(i, t)| **t && !changed_index.contains(i)).filter_map(|(i, _)| ranges.get(i).copied()).collect(),
            notes,
            key,
        }
    }

    /// A click on a note's button: `blocksatz-action:copy/3` or `…:done/3`.
    /// Returns whether the URI was one of these.
    pub fn handle_action(&self, uri: &str) -> bool {
        let Some(action) = uri.strip_prefix(ACTION_SCHEME) else { return false };
        let Some((verb, index)) = action.split_once('/').and_then(|(v, i)| i.parse::<usize>().ok().map(|i| (v, i))) else { return true };
        match verb {
            "copy" => crate::translatedialog::copy_original_section(&self.ctx, Some(index)),
            "done" => {
                crate::translatedialog::section_done(&self.ctx, index);
                self.refresh();
            }
            _ => {}
        }
        true
    }

    fn render(&self, path: &Path, doc: &Document) {
        self.pane.set_article_header(&doc.frontmatter);
        self.pane.set_doc_dir(path.parent().map(Path::to_path_buf));
        self.pane.update(&doc.body, &doc.frontmatter.media);
    }

    fn schedule_sync(&self) {
        if !self.present.get() || self.view_stack.visible_child_name().as_deref() != Some(PAGE) || self.sync_pending.replace(true) {
            return;
        }
        let weak = self.weak.clone();
        glib::idle_add_local_once(move || {
            if let Some(this) = weak.upgrade() {
                this.sync_pending.set(false);
                this.sync();
            }
        });
    }

    /// Scrolls the view to the counterpart of the editor's cursor line -
    /// or of its top line, when the cursor is scrolled out of sight.
    fn sync(&self) {
        let Some(other_text) = self.shown.borrow().as_ref().map(|(_, text)| document::parse(text).body) else { return };
        let buffer = &self.ctx.buffer;
        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
        let visible = self.view.visible_rect();
        let cursor = buffer.iter_at_mark(&buffer.get_insert());
        let cursor_y = self.view.iter_location(&cursor).y();
        let line = if cursor_y >= visible.y() && cursor_y <= visible.y() + visible.height() {
            cursor.line()
        } else {
            self.view.line_at_y(visible.y()).0.line()
        };
        let other_line = langswitch::map_line(&text, line.max(0) as usize, &other_text);
        let total = other_text.lines().count() as i32;
        let top_t = if other_line == 0 { 1.0 } else { 0.0 };
        // A little above the paragraph, so the heading before it shows too.
        self.pane.sync_to((other_line as f64 - 1.0).max(1.0), top_t, 0.0, total);
        self.pane.highlight_line(other_line as i32 + 1);
        let marks = self.marks.borrow();
        self.pane.mark_sections(&marks.changed, &marks.done);
        self.pane.show_section_notes(&marks.key, &marks.notes);
    }
}
