//! Keeps the open article saved without a Save button: every couple of
//! seconds (and whenever the window closes or another article is about to
//! replace it in the editor) the current document is written to its file -
//! creating a library folder for it first if it doesn't have one yet (see
//! `library.rs`). Replaces the old single crash-recovery slot: with the
//! real file never more than a moment behind, there's nothing left to
//! recover.
//!
//! Library files are written whenever anything changed, metadata included.
//! A file opened from elsewhere is only written once its text was actually
//! edited (or on an explicit Ctrl+S / successful upload) - opening it must
//! not rewrite someone's file just because Blocksatz added its own media
//! bookkeeping to the frontmatter.

use std::path::Path;
use std::time::Duration;

use adw::prelude::*;
use gtk4::glib;

use crate::document::{self, Document};
use crate::i18n::tr;
use crate::library;
use crate::window::{self, DocContext};

const INTERVAL: Duration = Duration::from_secs(2);

/// Writes the open document to disk if needed. `force` writes even an
/// unedited file outside the library (Ctrl+S, after an upload). Returns
/// whether anything was written.
pub fn flush(ctx: &DocContext, force: bool) -> bool {
    // A change made outside Blocksatz waits for a decision (`diskwatch.rs`):
    // writing now would destroy it.
    if let Some(watch) = crate::diskwatch::current().filter(|w| w.blocks_writing()) {
        watch.refused_write();
        return false;
    }
    write(ctx, force)
}

fn write(ctx: &DocContext, force: bool) -> bool {
    let body = ctx.buffer.text(&ctx.buffer.start_iter(), &ctx.buffer.end_iter(), false).to_string();
    let doc = Document { frontmatter: ctx.frontmatter.borrow().clone(), body };
    let root = library::root();

    // Bound first: a `match` on `ctx.current_path.borrow()` would keep that
    // borrow alive through the arms, and `adopt_path` writes to it.
    let current_path = ctx.current_path.borrow().clone();
    let mut moved = false;
    let path = match current_path {
        Some(path) => path,
        None => {
            // An untouched new document stays without a folder.
            if doc.body.trim().is_empty() {
                return false;
            }
            let fallback = library::untitled_name();
            match library::create_entry(&root, library::title_hint(&doc).as_deref(), &fallback) {
                Ok(path) => {
                    adopt_path(ctx, &path);
                    moved = true;
                    path
                }
                Err(err) => return report(ctx, &err),
            }
        }
    };

    let path = match library::rename_after_title(&root, &path, &doc) {
        Ok(Some(renamed)) => {
            adopt_path(ctx, &renamed);
            moved = true;
            renamed
        }
        Ok(None) => path,
        Err(err) => return report(ctx, &err),
    };

    let serialized = document::serialize(&doc);
    let edited = doc.body != *ctx.saved_text.borrow();
    if serialized == *ctx.written.borrow() || !(force || edited || library::contains(&root, &path)) {
        return false;
    }
    if let Err(err) = std::fs::write(&path, &serialized) {
        return report(ctx, &err);
    }
    if let Some(watch) = crate::diskwatch::current() {
        watch.record_write(&path, &serialized);
    }
    *ctx.written.borrow_mut() = serialized;
    *ctx.saved_text.borrow_mut() = doc.body;
    LAST_ERROR.with(|last| last.borrow_mut().clear());
    ctx.notify_library(moved);
    true
}

/// Points the editor at `path` (a new or renamed library file).
fn adopt_path(ctx: &DocContext, path: &Path) {
    *ctx.current_path.borrow_mut() = Some(path.to_path_buf());
    ctx.preview_pane.set_doc_dir(path.parent().map(Path::to_path_buf));
    ctx.title.set_subtitle(&window::subtitle_for(Some(path), &ctx.frontmatter.borrow()));
    let _ = crate::recentfiles::record(path);
}

thread_local! {
    /// The last error shown, so a lasting problem (a full disk, a folder
    /// gone read-only) produces one toast rather than one every tick.
    static LAST_ERROR: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

fn report(ctx: &DocContext, err: &std::io::Error) -> bool {
    let message = tr("Speichern fehlgeschlagen: {err}").replace("{err}", &err.to_string());
    let repeated = LAST_ERROR.with(|last| std::mem::replace(&mut *last.borrow_mut(), message.clone()) == message);
    if !repeated {
        window::show_toast(&ctx.toast_overlay, &message);
    }
    false
}

/// Starts the periodic save and saves once more when the window closes.
pub fn wire(window: &adw::ApplicationWindow, ctx: &DocContext) {
    {
        let ctx = ctx.clone();
        glib::timeout_add_local(INTERVAL, move || {
            // First look whether the file changed outside, then write.
            match crate::diskwatch::current() {
                Some(watch) => {
                    watch.check();
                    if !watch.blocks_writing() {
                        write(&ctx, false);
                    }
                }
                None => {
                    write(&ctx, false);
                }
            }
            glib::ControlFlow::Continue
        });
    }
    let ctx = ctx.clone();
    window.connect_close_request(move |_| {
        flush(&ctx, false);
        glib::Propagation::Proceed
    });
}
