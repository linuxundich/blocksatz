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
use crate::preview::PreviewPane;
use crate::window::DocContext;
use crate::{langswitch, library};

/// Name of the view in the right-hand pane's stack and of its toggle.
pub const PAGE: &str = "counterpart";

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
    }
}
