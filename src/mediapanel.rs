//! "Medienverwaltung" dialog: per-image alt text, caption, and WordPress
//! upload state for every image referenced in the current article - see
//! `media.rs` for the data model and why alt text needs three states
//! instead of Markdown's plain on/off.
//!
//! Uploads reuse `export.rs`'s local-image-resolution logic and run on a
//! spawned thread (`wpclient` is blocking), polled via `mpsc` +
//! `glib::timeout_add_local`, same pattern as `export.rs`'s publish/delete
//! flows - a failed or slow upload must never freeze the dialog or the rest
//! of the app, and never touches the locally-held article until it
//! succeeds.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk4::glib;

use crate::document::Frontmatter;
use crate::i18n::tr;
use crate::media::{self, AltText, UploadStatus};
use crate::{export, medialibrary, notify, preview, secrets, wpclient, wpsite};

pub fn open(
    parent: &adw::ApplicationWindow,
    body: String,
    frontmatter: Rc<RefCell<Frontmatter>>,
    doc_dir: Option<PathBuf>,
    preview_pane: Rc<preview::PreviewPane>,
) {
    let content = build_content(frontmatter, &body, doc_dir, preview_pane);

    let header = adw::HeaderBar::new();
    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&content));

    let dialog = adw::Dialog::builder()
        .title(tr("Medienverwaltung"))
        .content_width(560)
        .content_height(600)
        .child(&toolbar_view)
        .build();
    dialog.present(Some(parent));
}

/// Builds the Medienverwaltung's actual content (status line + per-image
/// list), independent of the dialog chrome around it - reused both by the
/// standalone `open()` above (still reachable via Ctrl+Shift+M) and by
/// `export.rs`, which embeds this same content as a tab in the publish
/// dialog so alt text/captions/uploads can be checked right before
/// publishing, not just from a separate dialog.
pub fn build_content(frontmatter: Rc<RefCell<Frontmatter>>, body: &str, doc_dir: Option<PathBuf>, preview_pane: Rc<preview::PreviewPane>) -> gtk4::Widget {
    // Re-scan so images added/removed since the last reconcile (a save, or
    // opening this panel before) are reflected immediately - metadata for
    // still-referenced images is preserved (see `media::reconcile`).
    {
        let mut fm = frontmatter.borrow_mut();
        fm.media = media::reconcile(&fm.media, body);
    }
    let item_count = frontmatter.borrow().media.len();
    preview_pane.refresh_media(&frontmatter.borrow().media);

    let content_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(12)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();

    let status_label = gtk4::Label::new(None);
    status_label.set_wrap(true);
    status_label.set_xalign(0.0);
    status_label.set_label(&summary_text(&frontmatter));

    let list_box = gtk4::ListBox::new();
    list_box.add_css_class("boxed-list");
    // Not a `MediaItem` (it's a single `Frontmatter` field set from the
    // properties dialog, never scanned from the body like the images
    // below) but shown as the list's first row anyway so there's one place
    // to check and trigger every WordPress image upload for the article.
    let featured_handles = build_featured_image_row(frontmatter.clone(), doc_dir.clone(), preview_pane.clone());
    list_box.append(&featured_handles.row);
    let mut row_handles = Vec::with_capacity(item_count);
    for index in 0..item_count {
        let handles = build_row(index, frontmatter.clone(), doc_dir.clone(), status_label.clone(), preview_pane.clone());
        list_box.append(&handles.expander);
        row_handles.push(handles);
    }

    let bulk_upload_section = build_bulk_upload_section(frontmatter.clone(), doc_dir.clone(), status_label.clone(), preview_pane.clone(), row_handles, featured_handles);

    let scroller = gtk4::ScrolledWindow::builder().child(&list_box).vexpand(true).min_content_height(360).build();

    content_box.append(&status_label);
    content_box.append(&bulk_upload_section);
    content_box.append(&scroller);
    content_box.upcast()
}

/// Which row a bulk-uploaded item's result belongs to - `Media(index)` for
/// a body image (`frontmatter.media[index]`, updated via `row_handles`),
/// or `Featured` for the article's featured image (`frontmatter
/// .featured_image`, updated via the featured row's own handles) - see
/// `build_bulk_upload_section`'s doc comment for why the featured image is
/// included in "Alle hochladen" at all despite not being a `MediaItem`.
enum BulkTarget {
    Media(usize),
    Featured,
}

/// "Alle hochladen": uploads every image in the article that isn't on
/// WordPress yet (`UploadStatus::NotUploaded`/`Failed`, i.e.
/// `item.wordpress.is_none()`), plus the featured image if one is set but
/// not yet uploaded (`frontmatter.featured_image.is_some()`) - otherwise
/// "Alle hochladen" would silently skip it, and it'd only get uploaded by
/// separately finding its own row's button or by publishing outright. All
/// in one go, sequentially rather than in parallel - one background thread
/// working through the list one image at a time, same rationale as every
/// other upload here for being on a thread at all (`wpclient` is blocking)
/// plus keeping requests against the site orderly and the progress bar
/// meaningful. Already-uploaded images are left untouched (use the
/// per-row "Erneut hochladen" for those); a failure partway through
/// doesn't stop the rest - it's recorded and the loop continues, so one
/// broken image never blocks every other one from getting uploaded.
fn build_bulk_upload_section(
    frontmatter: Rc<RefCell<Frontmatter>>,
    doc_dir: Option<PathBuf>,
    status_label: gtk4::Label,
    preview_pane: Rc<preview::PreviewPane>,
    row_handles: Vec<MediaRowHandles>,
    featured: FeaturedRowHandles,
) -> gtk4::Widget {
    let section = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(6).build();

    let button_row = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).spacing(8).build();
    let bulk_button = gtk4::Button::with_label(&tr("Alle hochladen"));
    let bulk_status_label = gtk4::Label::new(None);
    bulk_status_label.set_wrap(true);
    bulk_status_label.set_xalign(0.0);
    button_row.append(&bulk_button);
    button_row.append(&bulk_status_label);

    let progress_bar = gtk4::ProgressBar::new();
    progress_bar.set_visible(false);
    progress_bar.set_show_text(true);

    section.append(&button_row);
    section.append(&progress_bar);

    let (pending_count, total_count) = {
        let fm = frontmatter.borrow();
        let pending = fm.media.iter().filter(|item| item.wordpress.is_none()).count() + usize::from(fm.featured_image.is_some());
        let total = fm.media.len() + usize::from(fm.featured_image.is_some() || fm.featured_media_id.is_some());
        (pending, total)
    };
    bulk_button.set_visible(total_count > 0);
    bulk_button.set_sensitive(pending_count > 0);
    if total_count > 0 && pending_count == 0 {
        bulk_status_label.set_label(&tr("Alle Bilder bereits hochgeladen."));
    }

    let row_handles = Rc::new(row_handles);
    let featured = Rc::new(featured);
    bulk_button.connect_clicked(move |bulk_button| {
        let mut pending: Vec<(BulkTarget, String, Option<String>, Option<String>)> = frontmatter
            .borrow()
            .media
            .iter()
            .enumerate()
            .filter(|(_, item)| item.wordpress.is_none())
            .map(|(index, item)| (BulkTarget::Media(index), item.source.clone(), item.alt.as_wordpress_value().map(str::to_string), item.caption.clone()))
            .collect();
        {
            let fm = frontmatter.borrow();
            if let Some(source) = fm.featured_image.clone() {
                pending.push((BulkTarget::Featured, source, fm.featured_image_alt.clone(), None));
            }
        }
        if pending.is_empty() {
            return;
        }
        let total = pending.len();

        bulk_button.set_sensitive(false);
        progress_bar.set_visible(true);
        progress_bar.set_fraction(0.0);
        progress_bar.set_text(Some(&tr("0 von {total} hochgeladen").replace("{total}", &total.to_string())));
        bulk_status_label.set_label(&tr("Wird hochgeladen …"));

        let site = wpsite::load();
        let doc_dir_for_thread = doc_dir.clone();
        let (tx, rx) = mpsc::channel::<BulkUploadEvent>();
        std::thread::spawn(move || {
            let password = match futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username)) {
                Ok(Some(password)) => password,
                Ok(None) => {
                    let _ = tx.send(BulkUploadEvent::Aborted { reason: tr("Kein Application Password im Schlüsselbund gefunden.") });
                    return;
                }
                Err(err) => {
                    let _ = tx.send(BulkUploadEvent::Aborted { reason: err.to_string() });
                    return;
                }
            };
            let client = wpclient::Client::new(&site.url, &site.username, &password);
            let mut failed = 0;
            for (target, source, alt_for_upload, caption_for_upload) in pending {
                let filename = std::path::Path::new(&source).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| source.clone());
                // The featured image isn't a `MediaItem` and has no
                // tracked content hash to compare against (same reasoning
                // as its own per-row upload button in
                // `build_featured_image_row`), so this only computes one
                // for a `Media` target.
                let outcome = export::upload_image_file(&client, &source, doc_dir_for_thread.as_deref())
                    .map_err(|err| err.to_string())
                    .and_then(|uploaded| {
                        if alt_for_upload.is_some() || caption_for_upload.is_some() {
                            client
                                .update_media_metadata(uploaded.id, alt_for_upload.as_deref(), caption_for_upload.as_deref())
                                .map_err(|err| err.to_string())?;
                        }
                        let content_hash = matches!(target, BulkTarget::Media(_)).then(|| media::hash_local_file(&source, doc_dir_for_thread.as_deref()).unwrap_or_default());
                        Ok((uploaded, content_hash))
                    });
                if outcome.is_err() {
                    failed += 1;
                }
                let _ = tx.send(BulkUploadEvent::Progress { target, filename, result: outcome });
            }
            let _ = tx.send(BulkUploadEvent::Done { failed });
        });

        let frontmatter = frontmatter.clone();
        let status_label = status_label.clone();
        let preview_pane = preview_pane.clone();
        let bulk_button = bulk_button.clone();
        let bulk_status_label = bulk_status_label.clone();
        let progress_bar = progress_bar.clone();
        let row_handles = row_handles.clone();
        let featured = featured.clone();
        let mut completed = 0;
        glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
            Ok(BulkUploadEvent::Progress { target, filename, result }) => {
                completed += 1;
                progress_bar.set_fraction(completed as f64 / total as f64);
                progress_bar.set_text(Some(&tr("{completed} von {total} hochgeladen").replace("{completed}", &completed.to_string()).replace("{total}", &total.to_string())));

                match result {
                    Ok((media_result, content_hash)) => match target {
                        BulkTarget::Media(index) => {
                            let reference =
                                media::WordPressMediaRef { media_id: media_result.id, url: media_result.source_url.clone(), content_hash: content_hash.unwrap_or_default(), width: media_result.width, height: media_result.height, size_slug: None };
                            if let Some(item) = frontmatter.borrow_mut().media.get_mut(index) {
                                item.wordpress = Some(reference.clone());
                            }
                            if let Some(handles) = row_handles.get(index) {
                                handles.expander.set_subtitle(&upload_status_text(&UploadStatus::Uploaded(reference)));
                                handles.upload_status_label.set_label(&tr("Erfolgreich hochgeladen."));
                                handles.upload_button.set_label(&tr("Erneut hochladen"));
                                handles.upload_button.set_sensitive(true);
                            }
                        }
                        BulkTarget::Featured => {
                            {
                                let mut fm = frontmatter.borrow_mut();
                                fm.featured_image = None;
                                fm.featured_media_id = Some(media_result.id);
                            }
                            featured.row.set_subtitle(&featured_image_status_text(&frontmatter.borrow()));
                            featured.upload_button.set_visible(false);
                        }
                    },
                    Err(err) => {
                        match target {
                            BulkTarget::Media(index) => {
                                if let Some(handles) = row_handles.get(index) {
                                    handles.expander.set_subtitle(&upload_status_text(&UploadStatus::Failed(err.clone())));
                                    handles.upload_status_label.set_label(&tr("Fehler: {err}").replace("{err}", &err));
                                    handles.upload_button.set_sensitive(true);
                                }
                            }
                            BulkTarget::Featured => {
                                featured.row.set_subtitle(&tr("Fehler beim Hochladen: {err}").replace("{err}", &err));
                                featured.upload_button.set_sensitive(true);
                            }
                        }
                        notify::send("media-upload", &tr("Upload fehlgeschlagen"), &tr("„{filename}“: {err}").replace("{filename}", &filename).replace("{err}", &err));
                    }
                }
                status_label.set_label(&summary_text(&frontmatter));
                preview_pane.refresh_media(&frontmatter.borrow().media);
                glib::ControlFlow::Continue
            }
            Ok(BulkUploadEvent::Done { failed }) => {
                progress_bar.set_visible(false);
                bulk_status_label.set_label(&if failed == 0 {
                    tr("Alle {total} Bilder erfolgreich hochgeladen.").replace("{total}", &total.to_string())
                } else {
                    tr("{ok} von {total} Bildern hochgeladen, {failed} fehlgeschlagen.")
                        .replace("{ok}", &(total - failed).to_string())
                        .replace("{total}", &total.to_string())
                        .replace("{failed}", &failed.to_string())
                });
                let remaining_pending = {
                    let fm = frontmatter.borrow();
                    fm.media.iter().filter(|item| item.wordpress.is_none()).count() + usize::from(fm.featured_image.is_some())
                };
                bulk_button.set_sensitive(remaining_pending > 0);
                if failed == 0 {
                    notify::send("media-upload", &tr("Bilder hochgeladen"), &tr("Alle {total} Bilder wurden erfolgreich hochgeladen.").replace("{total}", &total.to_string()));
                } else {
                    notify::send(
                        "media-upload",
                        &tr("Upload teilweise fehlgeschlagen"),
                        &tr("{ok} von {total} Bildern hochgeladen, {failed} fehlgeschlagen.")
                            .replace("{ok}", &(total - failed).to_string())
                            .replace("{total}", &total.to_string())
                            .replace("{failed}", &failed.to_string()),
                    );
                }
                glib::ControlFlow::Break
            }
            Ok(BulkUploadEvent::Aborted { reason }) => {
                progress_bar.set_visible(false);
                bulk_status_label.set_label(&tr("Hochladen abgebrochen: {reason}").replace("{reason}", &reason));
                bulk_button.set_sensitive(true);
                glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                progress_bar.set_visible(false);
                bulk_status_label.set_label(&tr("Interner Fehler: Upload-Thread hat kein Ergebnis geliefert."));
                bulk_button.set_sensitive(true);
                glib::ControlFlow::Break
            }
        });
    });

    section.upcast()
}

/// One event per bulk-uploaded image (`Progress`, sent as each image's
/// upload finishes, success or failure), a final `Done` once the whole
/// batch has been worked through (`total - failed` gives the success count
/// without needing to send it separately), or `Aborted` when the batch
/// never even started (e.g. no Application Password in the keyring) - kept
/// distinct from `Done { failed: total }` so the status message explains
/// *why* nothing uploaded instead of just reporting every image failed
/// individually.
enum BulkUploadEvent {
    Progress { target: BulkTarget, filename: String, result: Result<(wpclient::MediaResult, Option<String>), String> },
    Done { failed: usize },
    Aborted { reason: String },
}

/// Every branch here is a complete, self-contained sentence (never grammar
/// fragments concatenated at runtime) precisely so a translation can use
/// its own language's plural/verb agreement instead of inheriting German's -
/// see `po/README.md`'s note on pluralization for why.
fn summary_text(frontmatter: &Rc<RefCell<Frontmatter>>) -> String {
    let fm = frontmatter.borrow();
    let total = fm.media.len();
    if total == 0 {
        return tr("Dieser Artikel enthält aktuell keine Bilder.");
    }
    let missing_alt = fm.media.iter().filter(|item| item.alt.is_undefined()).count();
    if missing_alt == 0 {
        return match total {
            1 => tr("1 Bild in diesem Artikel."),
            n => tr("{n} Bilder in diesem Artikel.").replace("{n}", &n.to_string()),
        };
    }
    match (missing_alt, total) {
        (1, 1) => tr("1 Bild hat noch keinen Alternativtext."),
        (1, total) => tr("1 von {total} Bildern hat noch keinen Alternativtext.").replace("{total}", &total.to_string()),
        (missing, total) => tr("{missing} von {total} Bildern haben noch keinen Alternativtext.")
            .replace("{missing}", &missing.to_string())
            .replace("{total}", &total.to_string()),
    }
}

fn upload_status_text(status: &UploadStatus) -> String {
    match status {
        UploadStatus::NotUploaded => tr("Noch nicht zu WordPress hochgeladen"),
        UploadStatus::Uploading => tr("Wird hochgeladen …"),
        UploadStatus::Uploaded(reference) => tr("Bereits hochgeladen (Medien-ID {id})").replace("{id}", &reference.media_id.to_string()),
        UploadStatus::Failed(err) => tr("Fehler beim letzten Upload: {err}").replace("{err}", err),
    }
}

fn featured_image_status_text(frontmatter: &Frontmatter) -> String {
    if let Some(path) = &frontmatter.featured_image {
        return tr("Bereit zum Hochladen: {path}").replace("{path}", path);
    }
    if let Some(id) = frontmatter.featured_media_id {
        return tr("Bereits hochgeladen (Medien-ID {id})").replace("{id}", &id.to_string());
    }
    tr("Kein Aufmacherbild festgelegt - in den Artikel-Eigenschaften auswählen.")
}

/// The featured image row's own widgets, exposed the same way
/// `MediaRowHandles` exposes a body row's - so `build_bulk_upload_section`
/// can update this row too when "Alle hochladen" includes the featured
/// image (see its own doc comment).
struct FeaturedRowHandles {
    row: adw::ActionRow,
    upload_button: gtk4::Button,
}

/// The featured image is a single `Frontmatter` field, not a `MediaItem`
/// scanned from the body, so it can't reuse `build_row`'s per-item alt-
/// text/caption editing - it only ever needs an upload button (plus, now,
/// a library picker). Once uploaded (or picked from the library),
/// `featured_image` (the pending local path) is cleared in favor of
/// `featured_media_id` (the resulting WordPress id), so `export.rs`'s own
/// automatic upload-on-publish never re-uploads it a second time.
fn build_featured_image_row(frontmatter: Rc<RefCell<Frontmatter>>, doc_dir: Option<PathBuf>, preview_pane: Rc<preview::PreviewPane>) -> FeaturedRowHandles {
    let row = adw::ActionRow::builder().title(tr("Aufmacherbild")).use_markup(false).build();
    row.set_subtitle(&featured_image_status_text(&frontmatter.borrow()));

    let upload_button = gtk4::Button::with_label(&tr("Zu WordPress hochladen"));
    upload_button.set_valign(gtk4::Align::Center);
    upload_button.set_visible(frontmatter.borrow().featured_image.is_some());
    row.add_suffix(&upload_button);

    // Always visible (unlike `upload_button` above, which only makes sense
    // once a local file is actually pending) - picking straight from the
    // library is a valid way to set the featured image from scratch, not
    // just an alternative to uploading a pending local one. Added after
    // `upload_button` in the box (see `add_suffix`'s append order) but
    // built first for `row.add_suffix` to still read naturally left-to-
    // right; the click handler below just needs `upload_button` already
    // in scope to hide it once a library pick supersedes a pending upload.
    let library_button = gtk4::Button::from_icon_name("folder-remote-symbolic");
    library_button.set_tooltip_text(Some(&tr("Aus Mediathek wählen…")));
    library_button.set_valign(gtk4::Align::Center);
    library_button.add_css_class("flat");
    row.add_suffix(&library_button);
    {
        let frontmatter = frontmatter.clone();
        let row = row.clone();
        let preview_pane = preview_pane.clone();
        let upload_button = upload_button.clone();
        library_button.connect_clicked(move |button| {
            let Some(window) = button.root().and_then(|root| root.downcast::<gtk4::Window>().ok()) else {
                return;
            };
            let frontmatter = frontmatter.clone();
            let row = row.clone();
            let preview_pane = preview_pane.clone();
            let upload_button = upload_button.clone();
            medialibrary::open(&window, move |item| {
                {
                    let mut fm = frontmatter.borrow_mut();
                    fm.featured_image = None;
                    fm.featured_media_id = Some(item.id);
                    if !item.alt_text.trim().is_empty() {
                        fm.featured_image_alt = Some(item.alt_text.clone());
                    }
                }
                row.set_subtitle(&featured_image_status_text(&frontmatter.borrow()));
                upload_button.set_visible(false);
                preview_pane.set_article_header(&frontmatter.borrow());
            });
        });
    }

    {
        let preview_pane = preview_pane.clone();
        let frontmatter = frontmatter.clone();
        let row = row.clone();
        let upload_button_for_click = upload_button.clone();
        upload_button.connect_clicked(move |_| {
            let (source, alt) = {
                let fm = frontmatter.borrow();
                let Some(source) = fm.featured_image.clone() else { return };
                (source, fm.featured_image_alt.clone())
            };

            upload_button_for_click.set_sensitive(false);
            row.set_subtitle(&tr("Wird hochgeladen …"));

            let site = wpsite::load();
            let doc_dir = doc_dir.clone();
            let (tx, rx) = mpsc::channel::<Result<wpclient::MediaResult, String>>();
            std::thread::spawn(move || {
                let outcome = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
                    .map_err(|err| err.to_string())
                    .and_then(|maybe_password| {
                        maybe_password.ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden."))
                    })
                    .and_then(|password| {
                        let client = wpclient::Client::new(&site.url, &site.username, &password);
                        let uploaded = export::upload_image_file(&client, &source, doc_dir.as_deref()).map_err(|err| err.to_string())?;
                        if alt.is_some() {
                            client.update_media_metadata(uploaded.id, alt.as_deref(), None).map_err(|err| err.to_string())?;
                        }
                        Ok(uploaded)
                    });
                let _ = tx.send(outcome);
            });

            let frontmatter = frontmatter.clone();
            let row = row.clone();
            let upload_button = upload_button_for_click.clone();
            let preview_pane = preview_pane.clone();
            glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
                Ok(Ok(media_result)) => {
                    {
                        let mut fm = frontmatter.borrow_mut();
                        fm.featured_image = None;
                        fm.featured_media_id = Some(media_result.id);
                    }
                    row.set_subtitle(&featured_image_status_text(&frontmatter.borrow()));
                    upload_button.set_visible(false);
                    preview_pane.set_article_header(&frontmatter.borrow());
                    notify::send("media-upload", &tr("Aufmacherbild hochgeladen"), &tr("Das Aufmacherbild wurde erfolgreich hochgeladen."));
                    glib::ControlFlow::Break
                }
                Ok(Err(err)) => {
                    row.set_subtitle(&tr("Fehler beim Hochladen: {err}").replace("{err}", &err));
                    upload_button.set_sensitive(true);
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    row.set_subtitle(&tr("Interner Fehler: Upload-Thread hat kein Ergebnis geliefert."));
                    upload_button.set_sensitive(true);
                    glib::ControlFlow::Break
                }
            });
        });
    }

    FeaturedRowHandles { row, upload_button }
}

/// The per-row widgets the bulk "Alle hochladen" button (see
/// `build_bulk_upload_section`) needs to update as each image's upload
/// finishes, alongside mutating `Frontmatter.media` itself - without this,
/// a row would keep showing "Noch nicht hochgeladen" until the dialog was
/// closed and reopened, even though the upload behind it had already
/// succeeded.
struct MediaRowHandles {
    expander: adw::ExpanderRow,
    upload_button: gtk4::Button,
    upload_status_label: gtk4::Label,
}

/// Shows/hides an alt-text-length warning icon and sets its tooltip from
/// `media::alt_text_length_warning` - shared by the initial per-row state
/// and every subsequent edit.
fn update_alt_length_warning(icon: &gtk4::Image, text: &str) {
    match media::alt_text_length_warning(text) {
        Some(message) => {
            icon.set_tooltip_text(Some(&message));
            icon.set_visible(true);
        }
        None => icon.set_visible(false),
    }
}

fn build_row(
    index: usize,
    frontmatter: Rc<RefCell<Frontmatter>>,
    doc_dir: Option<PathBuf>,
    status_label: gtk4::Label,
    preview_pane: Rc<preview::PreviewPane>,
) -> MediaRowHandles {
    let item = frontmatter.borrow().media[index].clone();

    let expander = adw::ExpanderRow::builder().title(item.filename.clone()).use_markup(false).build();
    expander.set_subtitle(&upload_status_text(&item.upload_status()));

    let alt_switch_row = adw::SwitchRow::builder()
        .title(tr("Alternativtext definieren"))
        .subtitle(tr("Aus lassen für rein dekorative Bilder - das ist kein Fehler"))
        .active(!item.alt.is_undefined())
        .build();

    let alt_entry_row = adw::EntryRow::builder().title(tr("Alternativtext")).build();
    if let AltText::Text(text) = &item.alt {
        alt_entry_row.set_text(text);
    }
    alt_entry_row.set_visible(alt_switch_row.is_active());

    // Non-blocking hint for an unusually long alt text (see
    // `media::alt_text_length_warning`) - a suffix icon with a tooltip,
    // never something that blocks saving, since some images genuinely need
    // a longer description.
    let alt_length_warning_icon = gtk4::Image::from_icon_name("dialog-warning-symbolic");
    alt_length_warning_icon.add_css_class("warning");
    alt_length_warning_icon.set_visible(false);
    alt_entry_row.add_suffix(&alt_length_warning_icon);
    update_alt_length_warning(&alt_length_warning_icon, &alt_entry_row.text());

    {
        let frontmatter = frontmatter.clone();
        let alt_entry_row = alt_entry_row.clone();
        let alt_length_warning_icon = alt_length_warning_icon.clone();
        let status_label = status_label.clone();
        let preview_pane = preview_pane.clone();
        alt_switch_row.connect_active_notify(move |row| {
            let active = row.is_active();
            alt_entry_row.set_visible(active);
            if let Some(item) = frontmatter.borrow_mut().media.get_mut(index) {
                item.alt = if active {
                    let text = alt_entry_row.text().to_string();
                    if text.is_empty() {
                        AltText::Empty
                    } else {
                        AltText::Text(text)
                    }
                } else {
                    AltText::Undefined
                };
            }
            update_alt_length_warning(&alt_length_warning_icon, &alt_entry_row.text());
            status_label.set_label(&summary_text(&frontmatter));
            preview_pane.refresh_media(&frontmatter.borrow().media);
        });
    }
    {
        let frontmatter = frontmatter.clone();
        let alt_switch_row = alt_switch_row.clone();
        let alt_length_warning_icon = alt_length_warning_icon.clone();
        let status_label = status_label.clone();
        let preview_pane = preview_pane.clone();
        alt_entry_row.connect_changed(move |row| {
            if !alt_switch_row.is_active() {
                return;
            }
            let text = row.text().to_string();
            update_alt_length_warning(&alt_length_warning_icon, &text);
            if let Some(item) = frontmatter.borrow_mut().media.get_mut(index) {
                item.alt = if text.is_empty() { AltText::Empty } else { AltText::Text(text) };
            }
            status_label.set_label(&summary_text(&frontmatter));
            preview_pane.refresh_media(&frontmatter.borrow().media);
        });
    }

    let caption_row = adw::EntryRow::builder().title(tr("Bildunterschrift")).text(item.caption.as_deref().unwrap_or("")).build();
    {
        let frontmatter = frontmatter.clone();
        let preview_pane = preview_pane.clone();
        caption_row.connect_changed(move |row| {
            let text = row.text().to_string();
            if let Some(item) = frontmatter.borrow_mut().media.get_mut(index) {
                item.caption = (!text.is_empty()).then_some(text);
            }
            preview_pane.refresh_media(&frontmatter.borrow().media);
        });
    }

    let upload_button = gtk4::Button::with_label(&if item.wordpress.is_some() { tr("Erneut hochladen") } else { tr("Zu WordPress hochladen") });
    let upload_status_label = gtk4::Label::new(None);
    upload_status_label.set_xalign(0.0);
    upload_status_label.set_wrap(true);

    let upload_row_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(12)
        .margin_end(12)
        .build();
    upload_row_box.append(&upload_button);
    upload_row_box.append(&upload_status_label);
    let upload_row = gtk4::ListBoxRow::builder().child(&upload_row_box).activatable(false).selectable(false).build();

    {
        let frontmatter = frontmatter.clone();
        let expander = expander.clone();
        let upload_button_for_click = upload_button.clone();
        let upload_status_label = upload_status_label.clone();
        let filename = item.filename.clone();
        upload_button.connect_clicked(move |_| {
            let (source, alt_for_upload, caption_for_upload, previous) = {
                let fm = frontmatter.borrow();
                let Some(item) = fm.media.get(index) else { return };
                (item.source.clone(), item.alt.as_wordpress_value().map(str::to_string), item.caption.clone(), item.wordpress.clone())
            };

            upload_button_for_click.set_sensitive(false);
            upload_status_label.set_label(&tr("Wird hochgeladen …"));
            expander.set_subtitle(&upload_status_text(&UploadStatus::Uploading));

            let site = wpsite::load();
            let doc_dir = doc_dir.clone();
            let (tx, rx) = mpsc::channel::<Result<(wpclient::MediaResult, String), String>>();
            std::thread::spawn(move || {
                let outcome = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
                    .map_err(|err| err.to_string())
                    .and_then(|maybe_password| {
                        maybe_password.ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden."))
                    })
                    .and_then(|password| {
                        let client = wpclient::Client::new(&site.url, &site.username, &password);
                        let uploaded = export::upload_image_file(&client, &source, doc_dir.as_deref()).map_err(|err| err.to_string())?;
                        client
                            .update_media_metadata(uploaded.id, alt_for_upload.as_deref(), caption_for_upload.as_deref())
                            .map_err(|err| err.to_string())?;
                        // WordPress can't replace an existing attachment's file
                        // in place, so a re-upload (the "Erneut hochladen" case)
                        // always creates a new one - clean up the superseded
                        // attachment so the media library doesn't accumulate
                        // orphaned duplicates. Best-effort: the new upload
                        // already succeeded, a failed cleanup shouldn't fail this.
                        if let Some(previous) = previous {
                            let _ = client.delete_media(previous.media_id);
                        }
                        let content_hash = media::hash_local_file(&source, doc_dir.as_deref()).unwrap_or_default();
                        Ok((uploaded, content_hash))
                    });
                let _ = tx.send(outcome);
            });

            let frontmatter = frontmatter.clone();
            let expander = expander.clone();
            let upload_button = upload_button_for_click.clone();
            let upload_status_label = upload_status_label.clone();
            let preview_pane = preview_pane.clone();
            let filename = filename.clone();
            glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
                Ok(Ok((media_result, content_hash))) => {
                    let reference = media::WordPressMediaRef { media_id: media_result.id, url: media_result.source_url.clone(), content_hash, width: media_result.width, height: media_result.height, size_slug: None };
                    if let Some(item) = frontmatter.borrow_mut().media.get_mut(index) {
                        item.wordpress = Some(reference.clone());
                    }
                    preview_pane.refresh_media(&frontmatter.borrow().media);
                    expander.set_subtitle(&upload_status_text(&UploadStatus::Uploaded(reference)));
                    upload_status_label.set_label(&tr("Erfolgreich hochgeladen."));
                    notify::send("media-upload", &tr("Bild hochgeladen"), &tr("„{filename}“ wurde erfolgreich hochgeladen.").replace("{filename}", &filename));
                    upload_button.set_label(&tr("Erneut hochladen"));
                    upload_button.set_sensitive(true);
                    glib::ControlFlow::Break
                }
                Ok(Err(err)) => {
                    expander.set_subtitle(&upload_status_text(&UploadStatus::Failed(err.clone())));
                    upload_status_label.set_label(&tr("Fehler: {err}").replace("{err}", &err));
                    notify::send("media-upload", &tr("Upload fehlgeschlagen"), &tr("„{filename}“: {err}").replace("{filename}", &filename).replace("{err}", &err));
                    upload_button.set_sensitive(true);
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    upload_status_label.set_label(&tr("Interner Fehler: Upload-Thread hat kein Ergebnis geliefert."));
                    upload_button.set_sensitive(true);
                    glib::ControlFlow::Break
                }
            });
        });
    }

    expander.add_row(&alt_switch_row);
    expander.add_row(&alt_entry_row);
    expander.add_row(&caption_row);
    expander.add_row(&upload_row);
    MediaRowHandles { expander, upload_button, upload_status_label }
}
