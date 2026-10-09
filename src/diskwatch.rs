//! Notices when the open article's file changes on disk outside Blocksatz
//! (another editor, a sync tool, `git checkout`) and offers to reload it -
//! without the autosave (`worksave.rs`) quietly writing over the change two
//! seconds later.
//!
//! The file's content as Blocksatz last read or wrote it is the baseline.
//! On every autosave tick the file is read again; if it differs, the change
//! came from outside. From then on nothing is written to that file until
//! the user decides, via a banner under the header bar:
//!
//! - nothing edited here since: "Neu laden" loads the file's version;
//! - edited here too (a conflict): "Konflikt lösen …" asks which version
//!   wins, with a line diff of both; taking the file's version keeps the
//!   discarded text as `<file>.lokal-<time>` next to it;
//! - the file is gone: "Wieder speichern" writes the editor's version back.
//!
//! The baseline belongs to one path and one `doc_generation`: opening
//! another article, or Blocksatz reloading this one itself, starts over
//! with whatever is on disk then, so its own writes never count as outside
//! changes.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk4::glib;

use crate::document::{self, Document};
use crate::i18n::tr;
use crate::window::{self, DocContext};

#[derive(Debug, Clone, PartialEq)]
enum Pending {
    /// The file now holds this.
    Changed(String),
    Removed,
}

#[derive(Default)]
pub struct DiskState {
    /// Path and `doc_generation` the baseline belongs to.
    key: Option<(PathBuf, u64)>,
    /// The file as Blocksatz last read or wrote it; `None` = didn't exist.
    baseline: Option<String>,
    pending: Option<Pending>,
}

thread_local! {
    /// The window's watcher (single-window app), for `worksave.rs`.
    static CURRENT: RefCell<std::rc::Weak<DiskWatch>> = const { RefCell::new(std::rc::Weak::new()) };
}

pub fn current() -> Option<Rc<DiskWatch>> {
    CURRENT.with(|current| current.borrow().upgrade())
}

/// The banner and the state behind it - one per window.
pub struct DiskWatch {
    pub banner: adw::Banner,
    state: RefCell<DiskState>,
    ctx: DocContext,
    /// The text last saved as a copy, so leaving a conflict repeatedly
    /// (Ctrl+S, switching articles) doesn't pile up identical copies.
    copied: RefCell<String>,
}

impl DiskWatch {
    pub fn new(ctx: &DocContext) -> Rc<Self> {
        let banner = adw::Banner::new("");
        let watch = Rc::new(Self { banner: banner.clone(), state: RefCell::new(DiskState::default()), ctx: ctx.clone(), copied: RefCell::new(String::new()) });
        CURRENT.with(|current| *current.borrow_mut() = Rc::downgrade(&watch));
        let weak = Rc::downgrade(&watch);
        banner.connect_button_clicked(move |_| {
            if let Some(watch) = weak.upgrade() {
                watch.on_banner_button();
            }
        });
        watch
    }

    /// While an outside change waits for a decision, the file must not be
    /// written.
    pub fn blocks_writing(&self) -> bool {
        self.state.borrow().pending.is_some()
    }

    /// `worksave.rs` wrote `content` to `path`.
    pub fn record_write(&self, path: &Path, content: &str) {
        let mut state = self.state.borrow_mut();
        state.key = Some((path.to_path_buf(), self.ctx.doc_generation.get()));
        state.baseline = Some(content.to_string());
        state.pending = None;
        drop(state);
        self.banner.set_revealed(false);
    }

    /// Compares the file with the baseline - called on every autosave tick,
    /// before anything is written.
    pub fn check(&self) {
        let Some(path) = self.ctx.current_path.borrow().clone() else {
            *self.state.borrow_mut() = DiskState::default();
            self.banner.set_revealed(false);
            return;
        };
        let key = (path.clone(), self.ctx.doc_generation.get());
        let on_disk = std::fs::read_to_string(&path).ok();
        let mut state = self.state.borrow_mut();
        if state.key.as_ref() != Some(&key) {
            *state = DiskState { key: Some(key), baseline: on_disk, pending: None };
            drop(state);
            self.banner.set_revealed(false);
            return;
        }
        if on_disk == state.baseline {
            // Changed and changed back, or never changed.
            if state.pending.take().is_some() {
                drop(state);
                self.banner.set_revealed(false);
            }
            return;
        }
        // The file now holds exactly what the editor would write: nothing
        // to decide.
        if on_disk.as_deref() == Some(self.local_serialized().as_str()) {
            state.baseline = on_disk;
            state.pending = None;
            drop(state);
            self.banner.set_revealed(false);
            return;
        }
        let pending = match on_disk {
            Some(content) => Pending::Changed(content),
            None => Pending::Removed,
        };
        if state.pending.as_ref() == Some(&pending) {
            drop(state);
            self.update_banner();
            return;
        }
        state.pending = Some(pending);
        drop(state);
        self.update_banner();
    }

    /// A write was refused while a change waits for a decision. When the
    /// editor holds text of its own (the user is leaving the article, or
    /// pressed Ctrl+S), it is kept as a copy instead of being lost.
    pub fn refused_write(&self) {
        if !self.locally_edited() || *self.copied.borrow() == self.local_serialized() {
            return;
        }
        self.save_copy();
    }

    fn save_copy(&self) -> bool {
        let Some(path) = self.ctx.current_path.borrow().clone() else { return false };
        let stamp = glib::DateTime::now_local().and_then(|now| now.format("%Y%m%d-%H%M%S")).map(|s| s.to_string()).unwrap_or_default();
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let copy = path.with_file_name(format!("{name}.lokal-{stamp}"));
        let text = self.local_serialized();
        match std::fs::write(&copy, &text) {
            Ok(()) => {
                *self.copied.borrow_mut() = text;
                let shown = copy.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                window::show_toast(&self.ctx.toast_overlay, &tr("Deine Fassung liegt als „{name}“ neben der Datei.").replace("{name}", &shown));
                true
            }
            Err(err) => {
                window::show_toast(&self.ctx.toast_overlay, &tr("Kopie deiner Fassung fehlgeschlagen: {err}").replace("{err}", &err.to_string()));
                false
            }
        }
    }

    /// Edited in Blocksatz since the file was last read or written?
    fn locally_edited(&self) -> bool {
        self.local_serialized() != *self.ctx.written.borrow()
    }

    fn local_serialized(&self) -> String {
        let body = self.ctx.buffer.text(&self.ctx.buffer.start_iter(), &self.ctx.buffer.end_iter(), false).to_string();
        document::serialize(&Document { frontmatter: self.ctx.frontmatter.borrow().clone(), body })
    }

    /// Text and button follow the situation: the user may keep typing
    /// while the banner is up, turning a plain reload into a conflict.
    fn update_banner(&self) {
        let pending = self.state.borrow().pending.clone();
        let (title, button) = match pending {
            None => {
                self.banner.set_revealed(false);
                return;
            }
            Some(Pending::Removed) => (tr("Die Datei wurde außerhalb von Blocksatz gelöscht oder verschoben."), tr("Wieder speichern")),
            Some(Pending::Changed(_)) if self.locally_edited() => {
                (tr("Die Datei wurde außerhalb von Blocksatz geändert, und du hast hier weitergeschrieben."), tr("Konflikt lösen …"))
            }
            Some(Pending::Changed(_)) => (tr("Die Datei wurde außerhalb von Blocksatz geändert."), tr("Neu laden")),
        };
        self.banner.set_title(&title);
        self.banner.set_button_label(Some(&button));
        self.banner.set_revealed(true);
    }

    fn on_banner_button(self: &Rc<Self>) {
        let pending = self.state.borrow().pending.clone();
        match pending {
            None => self.banner.set_revealed(false),
            Some(Pending::Removed) => self.keep_mine(),
            Some(Pending::Changed(_)) if self.locally_edited() => self.ask(),
            Some(Pending::Changed(_)) => self.load_from_disk(false),
        }
    }

    /// The conflict: which version wins.
    fn ask(self: &Rc<Self>) {
        let Some(parent) = self.banner.root().and_downcast::<gtk4::Window>() else { return };
        let dialog = adw::AlertDialog::new(
            Some(&tr("Welche Fassung soll gelten?")),
            Some(&tr("Die Datei wurde außerhalb von Blocksatz geändert, und du hast hier weitergeschrieben. Bei „Fassung der Datei laden“ bleibt dein Text als Kopie neben der Datei erhalten.")),
        );
        dialog.add_response("cancel", &tr("Abbrechen"));
        dialog.add_response("compare", &tr("Vergleichen …"));
        dialog.add_response("disk", &tr("Fassung der Datei laden"));
        dialog.add_response("mine", &tr("Meine Fassung behalten"));
        dialog.set_response_appearance("mine", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("compare"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(watch) = weak.upgrade() else { return };
            match response {
                "compare" => watch.compare(),
                "disk" => watch.load_from_disk(true),
                "mine" => watch.keep_mine(),
                _ => {}
            }
        });
        dialog.present(Some(&parent));
    }

    fn compare(self: &Rc<Self>) {
        let Some(parent) = self.banner.root().and_downcast::<gtk4::Window>() else { return };
        let Some(Pending::Changed(on_disk)) = self.state.borrow().pending.clone() else { return };
        let disk_doc = document::parse(&on_disk);
        let disk_text = titled(&disk_doc.frontmatter.title, &disk_doc.body);
        let local_body = self.ctx.buffer.text(&self.ctx.buffer.start_iter(), &self.ctx.buffer.end_iter(), false).to_string();
        let local_text = titled(&self.ctx.frontmatter.borrow().title, &local_body);
        let weak = Rc::downgrade(self);
        crate::compare::open_with(&parent, &crate::compare::Labels::disk(), &disk_text, &local_text, true, move |choice| {
            let Some(watch) = weak.upgrade() else { return };
            match choice {
                crate::compare::Choice::TakeBlog => watch.load_from_disk(true),
                crate::compare::Choice::KeepMine => watch.keep_mine(),
            }
        });
    }

    /// Loads the file's version. With `keep_copy` the editor's text is
    /// first saved as `<file>.lokal-<time>` beside it - not a `.md`, so the
    /// library doesn't list it as an article or a language version.
    fn load_from_disk(&self, keep_copy: bool) {
        let Some(path) = self.ctx.current_path.borrow().clone() else { return };
        if keep_copy && self.locally_edited() && *self.copied.borrow() != self.local_serialized() && !self.save_copy() {
            return;
        }
        // Reloading bumps `doc_generation`, which resets the baseline; the
        // pending change must not block the flush `open_document_at_path`
        // does first - there is nothing of ours left to write.
        *self.ctx.written.borrow_mut() = self.local_serialized();
        window::open_document_at_path(path, &self.ctx);
        self.state.borrow_mut().pending = None;
        self.banner.set_revealed(false);
    }

    /// Writes the editor's version over the file (or back, if it's gone).
    fn keep_mine(&self) {
        self.state.borrow_mut().pending = None;
        self.ctx.written.borrow_mut().clear();
        crate::worksave::flush(&self.ctx, true);
        self.banner.set_revealed(false);
    }
}

/// Title as a heading over the body, so a changed title shows in the diff.
fn titled(title: &str, body: &str) -> String {
    if title.trim().is_empty() { body.to_string() } else { format!("# {}\n\n{}", title.trim(), body) }
}
