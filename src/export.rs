//! Uploading an article to WordPress via `wpclient` - Markdown to
//! Gutenberg blocks, media uploads, the conflict check - for the main
//! action (`mainaction.rs`) and the release check behind it.
//!
//! `wpclient` is blocking (see its module docs for why), so the actual HTTP
//! work runs on a spawned `std::thread`, not the GTK thread. GTK widgets are
//! `!Send`, so the result comes back over a plain `std::sync::mpsc` channel
//! (`Send`-safe because it only ever carries owned strings/structs) that the
//! main thread polls with `glib::timeout_add_local` - the widget-touching
//! code all runs there, never inside the background thread.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk4::glib;

use crate::document::{self, Document, Frontmatter, PostStatus, PostType};
use crate::i18n::tr;
use crate::{media, notify, secrets, syncstate, wpclient, wpsite};

/// UI feedback hooks for the publish/delete flow (`wire_publish_button`,
/// `start_export`, `wire_delete_button`) - lets that shared logic report
/// progress/success/error without hard-coding which widgets show it, so the
/// exact same network/conflict-check/threading code can run from the
/// wizard's own status-label/link/embedded-preview-browser widgets (see
/// `wizard_publish_feedback`) or from a much thinner surface elsewhere (e.g.
/// a sidebar's toast + a `refresh()` call). The formatted message text
/// itself is always built by the shared logic, not by a callback - a
/// callback only ever renders whatever string it's given, so wording stays
/// centralized in one place regardless of how many UIs call into it.
type OnPublishSuccess = Rc<dyn Fn(&wpclient::PostResult, PostStatus)>;

/// Reads the article body/doc-dir to send, at the moment a publish button
/// is actually clicked rather than when it was wired up - the wizard's own
/// buttons are rebuilt fresh every time it opens, so a plain snapshot value
/// is equivalent there, but the document-management sidebar's buttons stay
/// wired for as long as the app runs, across many different documents and
/// edits - without this, they'd keep sending whatever content happened to
/// be current the moment the sidebar was first built. `wire_publish_button`
/// only ever calls these right before actually sending something.
pub(crate) type BodyProvider = Rc<dyn Fn() -> String>;
pub(crate) type DocDirProvider = Rc<dyn Fn() -> Option<PathBuf>>;
/// Writes the document back to disk after a successful export.
///
/// A successful send fills in `wp_post_id`, the per-image
/// `WordPressMediaRef`s and `wp_content_hash` - all of it in the shared
/// `Rc<RefCell<Frontmatter>>` and none of it in the editor's text buffer.
/// Without this, nothing ever persisted that: the "unsaved changes" check
/// compares the buffer against `saved_text`, so a frontmatter-only change
/// leaves the document looking untouched, and both autosave and the close
/// handler follow that same signal. Reopening the file therefore lost the
/// post id (the draft could no longer be updated, only published a second
/// time) and the upload refs (images looked local again and `sync_uploads`
/// re-uploaded them, creating duplicate attachments).
///
/// Deliberately takes no arguments: the caller owns the same
/// `Rc<RefCell<Frontmatter>>` this module just mutated, plus the buffer and
/// the path, so it reads the current state itself rather than being handed
/// a snapshot that could be stale by the time the upload finishes.
pub(crate) type DocumentSaver = Rc<dyn Fn()>;

#[derive(Clone)]
pub(crate) struct PublishFeedback {
    pub on_progress: Rc<dyn Fn(&str)>,
    pub on_success: OnPublishSuccess,
    pub on_error: Rc<dyn Fn(&str)>,
}

/// The post an interrupted first upload created, among `candidates`
/// (recent posts found by title): the newest one with exactly that title.
fn recovered_post_id(candidates: &[wpclient::PostSummary], title: &str) -> Option<u64> {
    candidates.iter().find(|post| post.title.trim() == title.trim()).map(|post| post.id)
}

/// One day before the failed attempt `attempt` (RFC 3339 UTC), as the
/// site-local date WordPress's `after` filter expects - a day's margin
/// covers any site time zone.
fn search_window_start(attempt: &str) -> Option<String> {
    let at = glib::DateTime::from_iso8601(attempt, None).ok()?;
    at.add_days(-1).ok()?.format("%Y-%m-%dT%H:%M:%S").ok().map(|s| s.to_string())
}

/// Appends WordPress's `preview=true` query parameter to a post's
/// permalink (`PostDetail::link`) - its documented convention for showing
/// an unpublished post's current content to a logged-in, authorized
/// session, used by the "Vorschau öffnen" button's click handler.
pub(crate) fn preview_url_for(link: &str) -> String {
    if link.contains('?') {
        format!("{link}&preview=true")
    } else {
        format!("{link}?preview=true")
    }
}

/// True when a re-fetched server content hash no longer matches the
/// locally-known baseline - i.e. the post changed on the server (most
/// likely edited directly in wp-admin) since it was last fetched or sent
/// from here. `local_hash` is `None` for a document never yet synced with
/// a server copy (see `Frontmatter::wp_content_hash`'s doc comment), in
/// which case there's no baseline to compare against, so this is never a
/// conflict.
fn has_conflicting_server_change(local_hash: Option<&str>, server_hash: &str) -> bool {
    local_hash.is_some_and(|local| local != server_hash)
}

/// Which `status` a publish-flow button sends, resolved from the live
/// `Frontmatter` at the moment the button is clicked - not when it was
/// wired up. That distinction matters for `PublishOrKeep`: the sidebar's
/// buttons stay wired for the whole app session, so a value computed once
/// at build time (before any document was even loaded, hence always
/// "no `wp_post_id` yet") made its "Aktualisieren" button force `publish`
/// and silently publish existing drafts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TargetStatus {
    /// Always send this status ("Als Entwurf hochladen", "Terminieren", ...).
    Set(PostStatus),
    /// "Veröffentlichen" for a post that doesn't exist on WordPress yet,
    /// "Aktualisieren" (status left untouched) once it does.
    PublishOrKeep,
}

impl TargetStatus {
    pub(crate) fn resolve(self, frontmatter: &Frontmatter) -> Option<PostStatus> {
        match self {
            TargetStatus::Set(status) => Some(status),
            TargetStatus::PublishOrKeep if frontmatter.wp_post_id.is_some() => None,
            TargetStatus::PublishOrKeep => Some(PostStatus::Publish),
        }
    }
}

/// Disables (`true`) or re-enables (`false`) whatever UI starts uploads
/// while one is running, so two can't race the same post.
pub(crate) type BusySetter = Rc<dyn Fn(bool)>;

/// The upload behind every publish control - `wire_publish_button`'s
/// buttons and the main action alike. See `wire_publish_button` for the
/// conflict check that runs first.
#[allow(clippy::too_many_arguments)]
pub(crate) fn publish(
    target: TargetStatus,
    frontmatter: &Rc<RefCell<Frontmatter>>,
    get_body: &BodyProvider,
    get_doc_dir: &DocDirProvider,
    feedback: &PublishFeedback,
    dialog_parent: &gtk4::Widget,
    save_document: &DocumentSaver,
    set_busy: &BusySetter,
) {
    let (post_id, local_hash, rest_base, target_status) = {
        let fm = frontmatter.borrow();
        (fm.wp_post_id, fm.wp_content_hash.clone(), fm.post_type.rest_base(), target.resolve(&fm))
    };
    let Some(post_id) = post_id.filter(|_| local_hash.is_some()) else {
        start_export(target_status, frontmatter, &get_body(), &get_doc_dir(), feedback, set_busy, save_document);
        return;
    };

    set_busy(true);
    (feedback.on_progress)(&tr("Prüfe auf Änderungen auf WordPress …"));

    // The blog this working copy belongs to, not necessarily the active one.
    let site = wpsite::for_site_id(frontmatter.borrow().wp_site.as_deref());
    let (tx, rx) = mpsc::channel::<Result<String, String>>();
    std::thread::spawn(move || {
        let outcome = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .map_err(|err| err.to_string())
            .and_then(|maybe_password| maybe_password.ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden.")))
            .and_then(|password| {
                wpclient::Client::new(&site.url, &site.username, &password)
                    .get_item(rest_base, post_id)
                    .map(|detail| document::content_hash(&detail.content))
                    .map_err(|err| err.to_string())
            });
        let _ = tx.send(outcome);
    });

    let frontmatter = frontmatter.clone();
    let get_body = get_body.clone();
    let get_doc_dir = get_doc_dir.clone();
    let feedback = feedback.clone();
    let dialog_parent = dialog_parent.clone();
    let save_document = save_document.clone();
    let set_busy = set_busy.clone();
    glib::timeout_add_local(Duration::from_millis(150), move || {
        let proceed_directly = |feedback: &PublishFeedback| {
            start_export(target_status, &frontmatter, &get_body(), &get_doc_dir(), feedback, &set_busy, &save_document);
        };
        match rx.try_recv() {
            Ok(Ok(server_hash)) => {
                if has_conflicting_server_change(local_hash.as_deref(), &server_hash) {
                    let confirm = adw::AlertDialog::new(
                        Some(&tr("Artikel wurde extern geändert")),
                        Some(&tr(
                            "Der Artikel wurde seit dem letzten Abruf/Senden direkt auf WordPress geändert - z. B. in wp-admin. Trotzdem mit der lokalen Version überschreiben?",
                        )),
                    );
                    confirm.add_response("cancel", &tr("Abbrechen"));
                    confirm.add_response("overwrite", &tr("Überschreiben"));
                    confirm.set_response_appearance("overwrite", adw::ResponseAppearance::Destructive);
                    confirm.set_default_response(Some("cancel"));
                    confirm.set_close_response("cancel");

                    let frontmatter = frontmatter.clone();
                    let get_body = get_body.clone();
                    let get_doc_dir = get_doc_dir.clone();
                    let feedback = feedback.clone();
                    let save_document = save_document.clone();
                    let set_busy = set_busy.clone();
                    confirm.connect_response(None, move |_, response| {
                        if response == "overwrite" {
                            start_export(target_status, &frontmatter, &get_body(), &get_doc_dir(), &feedback, &set_busy, &save_document);
                        } else {
                            (feedback.on_progress)(&tr("Abgebrochen - lokale Änderungen wurden nicht gesendet."));
                            set_busy(false);
                        }
                    });
                    confirm.present(Some(&dialog_parent));
                } else {
                    proceed_directly(&feedback);
                }
                glib::ControlFlow::Break
            }
            Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                // Fail-open: the conflict check itself is a secondary
                // safety net, not the actual publish attempt - if *it*
                // can't complete (a transient network/auth hiccup), the
                // real publish attempt right after will surface its own,
                // more specific error if something is genuinely wrong.
                // Refusing to publish at all just because this extra
                // check failed would trade a rare conflict risk for a
                // much more common "can't publish edits at all" one.
                proceed_directly(&feedback);
                glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        }
    });
}

/// Actually sends the article to WordPress - the second half of
/// `publish`, split out so it can be invoked either immediately (no
/// conflict to check, or nothing to check against) or from the
/// confirmation dialog's "Überschreiben" response.
pub(crate) fn start_export(
    target_status: Option<PostStatus>,
    frontmatter: &Rc<RefCell<Frontmatter>>,
    body: &str,
    doc_dir: &Option<PathBuf>,
    feedback: &PublishFeedback,
    set_busy: &BusySetter,
    save_document: &DocumentSaver,
) {
    set_busy(true);
    (feedback.on_progress)(&tr("Wird gesendet …"));

    // Recorded on disk *before* the request, so an interrupted first upload
    // is recognized on the next try (see `Frontmatter::wp_pending_create`).
    let creating = {
        let mut fm = frontmatter.borrow_mut();
        let creating = fm.wp_post_id.is_none();
        if creating && fm.wp_pending_create.is_none() {
            fm.wp_pending_create = Some(syncstate::now_rfc3339());
        }
        creating
    };
    if creating {
        save_document();
    }

    // Updates go to the blog the post lives on; a new post to the active one.
    let site = wpsite::for_site_id(frontmatter.borrow().wp_site.as_deref());
    let mut current_fm = frontmatter.borrow().clone();
    if let Some(target_status) = target_status {
        current_fm.status = target_status;
    }
    let body = body.to_string();
    let sent_body = body.clone();
    let site_id = site.site_id();
    let doc_dir = doc_dir.clone();

    let (tx, rx) = mpsc::channel::<Result<(wpclient::PostResult, Vec<media::MediaItem>, Option<String>), String>>();
    std::thread::spawn(move || {
        let outcome = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .map_err(|err| err.to_string())
            .and_then(|maybe_password| maybe_password.ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden.")))
            .and_then(|password| run_export(&site, &password, &mut current_fm, target_status, &body, doc_dir.as_deref()))
            .map(|post| (post, current_fm.media, current_fm.wp_content_hash));
        let _ = tx.send(outcome);
    });

    let frontmatter = frontmatter.clone();
    let feedback = feedback.clone();
    let set_busy = set_busy.clone();
    let save_document = save_document.clone();
    glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
        Ok(Ok((post, media, content_hash))) => {
            let (title, final_status) = {
                let mut fm = frontmatter.borrow_mut();
                fm.wp_post_id = Some(post.id);
                fm.media = media;
                if let Some(target_status) = target_status {
                    fm.status = target_status;
                }
                fm.wp_content_hash = content_hash;
                fm.wp_pending_create = None;
                // Fingerprinted against the body as it was *sent*: anything
                // typed while the upload ran is a local change still to go.
                let mut synced = Document { frontmatter: fm.clone(), body: sent_body.clone() };
                syncstate::mark_synced(&mut synced, &site_id, &post.modified_gmt, &syncstate::now_rfc3339());
                *fm = synced.frontmatter;
                (fm.title.clone(), fm.status)
            };
            // After the `borrow_mut` above has ended: `save_document`
            // borrows the same `RefCell` again, and doing this inside the
            // block would panic.
            save_document();
            set_busy(false);
            (feedback.on_success)(&post, final_status);
            // Reflects what actually happened rather than always claiming
            // "Veröffentlicht" - `target_status` being `None` means the
            // status was deliberately left untouched (see `TargetStatus`),
            // so the notification says "Aktualisiert" instead.
            let action_label = target_status.map(|s| s.label()).unwrap_or_else(|| tr("Aktualisiert"));
            notify::send("export", &action_label, &tr("„{title}“ wurde erfolgreich gesendet.").replace("{title}", &title));
            glib::ControlFlow::Break
        }
        Ok(Err(err)) => {
            set_busy(false);
            (feedback.on_error)(&tr("Fehler: {err}").replace("{err}", &err));
            notify::send("export", &tr("Veröffentlichen fehlgeschlagen"), &err);
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            set_busy(false);
            (feedback.on_error)(&tr("Interner Fehler: Export-Thread hat kein Ergebnis geliefert."));
            glib::ControlFlow::Break
        }
    });
}

fn run_export(
    site: &wpsite::SiteConfig,
    password: &str,
    frontmatter: &mut Frontmatter,
    target_status: Option<PostStatus>,
    body: &str,
    doc_dir: Option<&Path>,
) -> Result<wpclient::PostResult, String> {
    // WordPress only actually schedules a post if its `date` is genuinely
    // in the future - given a missing or past date, it silently publishes
    // immediately instead of scheduling (see `PostStatus::Future`'s doc
    // comment), so this is checked up front rather than letting that
    // surprise happen after an otherwise-successful export.
    if target_status == Some(PostStatus::Future) && frontmatter.scheduled_at.is_none() {
        return Err(tr("Für den Status „Geplant“ muss ein gültiger Veröffentlichungstermin gesetzt sein."));
    }

    let client = wpclient::Client::new(&site.url, &site.username, password);

    // Without a frontmatter title, a leading `# Heading` is the title (as
    // the sidebar and window title already show it) rather than an extra
    // H1 at the top of the post.
    let body = match document::split_title_heading(body) {
        Some((title, rest)) if frontmatter.title.trim().is_empty() => {
            frontmatter.title = title;
            rest
        }
        _ => body,
    };

    frontmatter.media = media::reconcile(&frontmatter.media, body);
    let uploaded_urls = media::sync_uploads(&client, &mut frontmatter.media, doc_dir)?;

    let (body, footnotes) = with_footnotes(body);
    let mut blocks = gutenberg::parse_markdown(&body);
    apply_media_metadata(&mut blocks, &frontmatter.media);
    rewrite_image_urls(&mut blocks, &uploaded_urls);
    let content = gutenberg::render_blocks(&blocks);

    let mut payload = serde_json::json!({
        "title": frontmatter.title,
        "content": content,
    });
    // Pages don't have categories/tags at all (WordPress registers neither
    // taxonomy for the `page` type) - skipped entirely rather than
    // resolving/creating terms that would then just be silently dropped.
    if frontmatter.post_type == PostType::Post {
        let mut category_ids = Vec::new();
        for name in &frontmatter.categories {
            category_ids.push(client.resolve_or_create_term("categories", name).map_err(|err| err.to_string())?);
        }
        let mut tag_ids = Vec::new();
        for name in &frontmatter.tags {
            tag_ids.push(client.resolve_or_create_term("tags", name).map_err(|err| err.to_string())?);
        }
        payload["categories"] = serde_json::json!(category_ids);
        payload["tags"] = serde_json::json!(tag_ids);
    }
    // Pages only, mirroring the categories/tags guard above - WordPress's
    // hierarchical-pages `parent` field doesn't exist on posts at all. Sent
    // unconditionally (unlike `author_id`'s "only when set"): `parent_id`
    // being `None` is itself a meaningful choice (top-level) that needs to
    // reach the server as `parent: 0`, not be left as a silent no-op that
    // could leave a stale parent from before in place.
    if frontmatter.post_type == PostType::Page {
        payload["parent"] = serde_json::json!(frontmatter.parent_id.unwrap_or(0));
    }
    // Omitted entirely (not just "left at whatever `frontmatter.status`
    // says") when `target_status` is `None` - WordPress's REST API leaves
    // an existing post's status untouched when the field is absent from
    // the request, which is exactly the "Aktualisieren" button's contract
    // (see `wire_publish_button`'s doc comment). Sending it unconditionally
    // here, even with the locally-known status, would risk re-asserting a
    // stale value if `Frontmatter.status` ever drifted from the post's real
    // status on the server.
    if let Some(target_status) = target_status {
        payload["status"] = serde_json::Value::String(target_status.as_str().to_string());
    }
    if !frontmatter.slug.is_empty() {
        payload["slug"] = serde_json::Value::String(frontmatter.slug.clone());
    }
    if let Some(excerpt) = &frontmatter.excerpt {
        payload["excerpt"] = serde_json::Value::String(excerpt.clone());
    }
    // RankMath registers these meta keys with `show_in_rest`, so they're
    // writable the same way as any other post meta; sent only when set, so
    // an unset field never overwrites a value already set directly in
    // RankMath's own editor. Harmless against a site without RankMath -
    // WordPress's REST API silently drops an unrecognized meta key rather
    // than erroring.
    let mut meta = serde_json::Map::new();
    if let Some(title) = &frontmatter.rank_math_title {
        meta.insert("rank_math_title".to_string(), serde_json::Value::String(title.clone()));
    }
    if let Some(description) = &frontmatter.rank_math_description {
        meta.insert("rank_math_description".to_string(), serde_json::Value::String(description.clone()));
    }
    if let Some(keyword) = &frontmatter.rank_math_focus_keyword {
        meta.insert("rank_math_focus_keyword".to_string(), serde_json::Value::String(keyword.clone()));
    }
    // Footnotes written in Markdown, or the ones an opened post brought
    // along unconverted.
    if let Some(footnotes) = footnotes_meta(&footnotes, frontmatter.wp_footnotes.as_deref()) {
        meta.insert("footnotes".to_string(), serde_json::Value::String(footnotes));
    }
    // A translation's link to its original, as the post meta the
    // companion plugin lui-translations registers (hreflang, language
    // switcher). Like the RankMath keys: dropped silently by a site
    // without that plugin.
    if let Some(link) = &frontmatter.translation {
        meta.insert("lui_source_id".to_string(), serde_json::json!(link.source_id));
        meta.insert("lui_source_hash".to_string(), serde_json::json!(link.source_hash));
        meta.insert("lui_source_translated".to_string(), serde_json::json!(link.translated_at));
        meta.insert("lui_source_reviewed".to_string(), serde_json::json!(link.reviewed));
    }
    if !meta.is_empty() {
        payload["meta"] = serde_json::Value::Object(meta);
    }
    if target_status == Some(PostStatus::Future) {
        if let Some(scheduled_at) = &frontmatter.scheduled_at {
            payload["date"] = serde_json::Value::String(scheduled_at.clone());
        }
    }
    if let Some(path) = &frontmatter.featured_image {
        // Normally already uploaded via Medienverwaltung's own "Aufmacherbild"
        // row (`mediapanel::build_featured_image_row`) before publishing gets
        // this far, which clears `featured_image` in favor of
        // `featured_media_id` below. This is the fallback for a
        // `featured_image` set but never manually uploaded: upload it now and
        // use the resulting media id. Unlike body images, this always
        // re-uploads rather than going through `media::sync_uploads`'s hash
        // check - the featured image isn't a `MediaItem` at all (it's a
        // single Frontmatter field, never scanned from the Markdown body),
        // so it has no tracked content hash to compare against.
        let media = upload_image_file(&client, path, doc_dir).map_err(|err| err.to_string())?;
        payload["featured_media"] = serde_json::json!(media.id);
    } else if let Some(id) = frontmatter.featured_media_id {
        // Nothing new was set, but the document carries an existing
        // featured image from importing this post (see `importer.rs`) -
        // keep it rather than silently clearing it on re-export.
        payload["featured_media"] = serde_json::json!(id);
    }
    // Only sent when explicitly set (same "only when set" reasoning as the
    // RankMath meta fields above) - an unset `author_id` leaves the post's
    // existing author untouched rather than resetting it to whichever user
    // the Application Password belongs to. Setting it to a *different*
    // user requires that Application Password's own user to have
    // WordPress's `edit_others_posts` capability; a lower-privileged user
    // gets a normal, readable `rest_cannot_edit_others`-style error back
    // through the existing error path below, not a silent no-op.
    if let Some(author_id) = frontmatter.author_id {
        payload["author"] = serde_json::json!(author_id);
    }
    // Unlike `author`/`status` above, sent unconditionally rather than only
    // when explicitly touched - `false` (not ignored/tracked as normal) is
    // a safe default that matches how a post behaves before this is ever
    // touched at all, so there's no "unset" state worth distinguishing.
    // Silently dropped by WordPress if the "Worthy" plugin (or any plugin
    // registering this field) isn't installed on the target site, the same
    // way an unrecognized `meta` key is - see `Frontmatter::vgwort_ignored`.
    payload["wp-worthy-pixel"] = serde_json::json!({ "ignored": frontmatter.vgwort_ignored });
    // Only sent when explicitly set (same "only when set" reasoning as
    // `author_id` above) - an unset comment status leaves the post's
    // existing one untouched rather than resetting it to WordPress's own
    // site-wide default on every export.
    if let Some(open) = frontmatter.comment_status {
        payload["comment_status"] = serde_json::Value::String(if open { "open" } else { "closed" }.to_string());
    }

    let rest_base = frontmatter.post_type.rest_base();
    // A previous first upload may have created the post without its answer
    // arriving (network drop): look for it before creating a second one.
    if frontmatter.wp_post_id.is_none() && !frontmatter.title.trim().is_empty() {
        if let Some(after) = frontmatter.wp_pending_create.as_deref().and_then(search_window_start) {
            if let Ok(candidates) = client.find_recent_by_title(rest_base, &frontmatter.title, &after) {
                frontmatter.wp_post_id = recovered_post_id(&candidates, &frontmatter.title);
            }
        }
    }
    let result = match frontmatter.wp_post_id {
        Some(id) => client.update_item(rest_base, id, &payload),
        None => client.create_item(rest_base, &payload),
    }
    .map_err(|err| err.to_string())?;
    // `content` is exactly what the server now stores (WordPress's REST API
    // persists the `content` field verbatim, it doesn't re-serialize it on
    // save) - recording its hash here, not just on import, is what lets the
    // *next* update's conflict check (`wire_publish_button`) compare a
    // fresh fetch against a baseline that's actually still in sync, instead
    // of against a stale one from whenever this document was last opened.
    frontmatter.wp_content_hash = Some(document::content_hash(&content));
    Ok(result)
}

/// Renders `markdown` as Gutenberg block-comment HTML with Medienverwaltung's
/// alt-text/caption edits (`media`) already overlaid on top - the same
/// `apply_media_metadata` step `run_export` applies right before actually
/// publishing, exposed here so every *preview* of "the HTML that would be
/// sent" (the export dialog's own "Vorschau" tab, and the standalone
/// "Gutenberg-Code" tab in `codeview.rs`) shows the same thing that would
/// really be published, instead of silently reverting to whatever alt/
/// title text happens to be written literally in the Markdown source (a
/// plain `gutenberg::markdown_to_gutenberg` call has no way to know about
/// `Frontmatter.media` at all). Doesn't also apply `rewrite_image_urls` -
/// that substitutes a local path for its *uploaded* WordPress URL, which
/// only exists once an upload has actually happened, so showing the local
/// path here is the correct preview before that point.
pub(crate) fn gutenberg_preview_html(markdown: &str, media: &[media::MediaItem]) -> String {
    let mut blocks = gutenberg::parse_markdown(&with_footnotes(markdown).0);
    apply_media_metadata(&mut blocks, media);
    gutenberg::render_blocks(&blocks)
}

/// Markdown footnotes (`gutenberg::footnotes`): the body with WordPress's
/// references in place of `[^1]`, and the notes. The list block is added
/// at the end when the body has none.
pub(crate) fn with_footnotes(markdown: &str) -> (String, Vec<gutenberg::footnotes::Footnote>) {
    let (mut body, notes) = gutenberg::footnotes::extract(markdown);
    if !notes.is_empty() && !body.contains("<!-- wp:footnotes") {
        body = format!("{}\n\n{}\n", body.trim_end(), gutenberg::footnotes::LIST_BLOCK);
    }
    (body, notes)
}

/// The `footnotes` meta to send: the Markdown notes, else what an opened
/// post brought along unconverted.
pub(crate) fn footnotes_meta(notes: &[gutenberg::footnotes::Footnote], kept: Option<&str>) -> Option<String> {
    if notes.is_empty() {
        kept.map(str::to_string)
    } else {
        Some(gutenberg::footnotes::meta_json(notes))
    }
}

/// Overlays each image block's alt text/caption with the corresponding
/// `MediaItem`'s (matched by `source`, i.e. the block's still-original,
/// pre-`rewrite_image_urls` url - so this must run before that) - so an
/// edit made in Medienverwaltung actually reaches the published post's
/// HTML. Without this, `blocks` only ever carries whatever alt/title text
/// happens to be written literally in the Markdown source, since
/// Medienverwaltung's alt-text/caption editors (`mediapanel.rs`) only ever
/// update `Frontmatter.media`, never the body itself; the only place that
/// data otherwise reaches WordPress is `media::sync_uploads`'s
/// `update_media_metadata` call, which sets the *attachment's* alt
/// text/caption in the media library, not the `<img>`/`<figcaption>` baked
/// into this post's own content - and WordPress never re-reads an
/// attachment's current metadata into an already-published block.
/// `AltText::Undefined` (nothing decided yet) deliberately leaves the
/// parsed alt alone rather than blanking it.
fn apply_media_metadata(blocks: &mut [gutenberg::Block], media: &[media::MediaItem]) {
    let mut counts = std::collections::HashMap::new();
    count_image_sources(blocks, &mut counts);
    apply_media_metadata_with(blocks, media, &counts);
}

/// How often each image source appears as an image block.
fn count_image_sources(blocks: &[gutenberg::Block], counts: &mut std::collections::HashMap<String, usize>) {
    for block in blocks {
        match block {
            gutenberg::Block::Image { url, .. } => *counts.entry(url.clone()).or_default() += 1,
            gutenberg::Block::Gallery { images, .. } => images.iter().for_each(|image| *counts.entry(image.url.clone()).or_default() += 1),
            gutenberg::Block::BlockQuote { blocks, .. } | gutenberg::Block::Details { blocks, .. } | gutenberg::Block::Container { blocks, .. } => count_image_sources(blocks, counts),
            gutenberg::Block::List { items, .. } => items.iter().for_each(|item| count_image_sources(item, counts)),
            gutenberg::Block::Columns { columns } => columns.iter().for_each(|column| count_image_sources(column, counts)),
            gutenberg::Block::Styled { block, .. } => count_image_sources(std::slice::from_ref(block.as_ref()), counts),
            _ => {}
        }
    }
}

/// The same image used more than once can carry a different alt text and
/// caption at each place, while `Frontmatter.media` keeps one entry per
/// source - there, each occurrence keeps what its own Markdown says
/// instead of all getting the first one's.
fn apply_media_metadata_with(blocks: &mut [gutenberg::Block], media: &[media::MediaItem], counts: &std::collections::HashMap<String, usize>) {
    for block in blocks.iter_mut() {
        let mut size_slug = None;
        match block {
            gutenberg::Block::Image { url, alt, title, media_id, width, height, .. } => {
                if let Some(item) = media.iter().find(|item| &item.source == url) {
                    size_slug = item.wordpress.as_ref().and_then(|wp| wp.size_slug.clone());
                    if counts.get(url.as_str()).copied().unwrap_or(0) <= 1 {
                        if let Some(text) = item.alt.as_wordpress_value() {
                            *alt = text.to_string();
                        }
                        // The block's caption is inline HTML from the Markdown
                        // (links, emphasis); Medienverwaltung keeps its text.
                        // Only a different text replaces it.
                        if item.caption.as_deref() != title.as_deref().map(caption_text).as_deref() {
                            *title = item.caption.as_deref().map(gutenberg::escape_html);
                        }
                    }
                    if let Some(wp) = &item.wordpress {
                        *media_id = Some(wp.media_id);
                        *width = wp.width;
                        *height = wp.height;
                    }
                }
            }
            gutenberg::Block::BlockQuote { blocks, .. } => apply_media_metadata_with(blocks, media, counts),
            gutenberg::Block::List { items, .. } => {
                for item in items.iter_mut() {
                    apply_media_metadata_with(item, media, counts);
                }
            }
            gutenberg::Block::Columns { columns } => {
                for column in columns.iter_mut() {
                    apply_media_metadata_with(column, media, counts);
                }
            }
            gutenberg::Block::Details { blocks, .. } => apply_media_metadata_with(blocks, media, counts),
            gutenberg::Block::Gallery { images, .. } => {
                for image in images.iter_mut() {
                    if let Some(wp) = media.iter().find(|item| item.source == image.url).and_then(|item| item.wordpress.as_ref()) {
                        image.media_id = Some(wp.media_id);
                    }
                }
            }
            gutenberg::Block::Styled { block, .. } => apply_media_metadata_with(std::slice::from_mut(block.as_mut()), media, counts),
            gutenberg::Block::Container { kind, params, blocks, .. } => {
                apply_container_image_metadata(kind, params, media);
                apply_media_metadata_with(blocks, media, counts);
            }
            _ => {}
        }
        // The image size the blog had (`sizeSlug`, `size-large`).
        if let Some(size) = size_slug {
            let image = std::mem::replace(block, gutenberg::Block::ThematicBreak);
            *block = image.with_attrs(gutenberg::BlockAttrs { size_slug: Some(size), ..Default::default() });
        }
    }
}

/// A cover's or media-text's image: the attachment id (for `wp-image-…`
/// and `srcset`), the size the upload links and the alt text from
/// Medienverwaltung.
fn apply_container_image_metadata(kind: &str, params: &mut gutenberg::ContainerParams, media: &[media::MediaItem]) {
    if !gutenberg::CONTAINER_IMAGE_KINDS.contains(&kind) || params.get("type") == Some("video") {
        return;
    }
    let Some(item) = params.get("image").and_then(|image| media.iter().find(|item| item.source == image)) else { return };
    if let Some(wp) = &item.wordpress {
        params.set("id", Some(wp.media_id.to_string()));
        if kind == "media-text" {
            if let Some(size) = &wp.size_slug {
                params.set("size", Some(size.clone()));
            }
        }
    }
    // An `alt=` written in the header wins - the same file can appear
    // elsewhere with another alt text, the media list keeps only one.
    if kind == "media-text" && params.get("alt").is_none() {
        if let Some(alt) = item.alt.as_wordpress_value() {
            params.set("alt", Some(alt.to_string()));
        }
    }
}

/// The text of a caption's inline HTML, as Medienverwaltung keeps it.
fn caption_text(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&")
}

/// Recursively substitutes `wp:image`/`wp:video`/`wp:audio` blocks' source
/// with the WordPress URL `media::sync_uploads` resolved for it, wherever
/// the block's current url is a key in `urls` - an already-remote url (not
/// tracked by `sync_uploads` at all, e.g. an embed) simply has no matching
/// key and is left as-is.
fn rewrite_image_urls(blocks: &mut [gutenberg::Block], urls: &std::collections::HashMap<String, String>) {
    for block in blocks.iter_mut() {
        match block {
            gutenberg::Block::Image { url, link, .. } => {
                if let Some(new_url) = urls.get(url) {
                    // A link to the image's own file follows it.
                    if link.as_deref() == Some(url.as_str()) {
                        *link = Some(new_url.clone());
                    }
                    *url = new_url.clone();
                }
            }
            gutenberg::Block::Video { url, .. } | gutenberg::Block::Audio { url, .. } => {
                if let Some(new_url) = urls.get(url) {
                    *url = new_url.clone();
                }
            }
            gutenberg::Block::BlockQuote { blocks, .. } => rewrite_image_urls(blocks, urls),
            gutenberg::Block::List { items, .. } => {
                for item in items.iter_mut() {
                    rewrite_image_urls(item, urls);
                }
            }
            gutenberg::Block::Columns { columns } => {
                for column in columns.iter_mut() {
                    rewrite_image_urls(column, urls);
                }
            }
            gutenberg::Block::Gallery { images, .. } => {
                for image in images.iter_mut() {
                    if let Some(new_url) = urls.get(&image.url) {
                        image.url = new_url.clone();
                    }
                }
            }
            gutenberg::Block::Details { blocks, .. } => rewrite_image_urls(blocks, urls),
            gutenberg::Block::Styled { block, .. } => rewrite_image_urls(std::slice::from_mut(block.as_mut()), urls),
            gutenberg::Block::Container { kind, params, blocks, .. } => {
                if gutenberg::CONTAINER_IMAGE_KINDS.contains(&kind.as_str()) {
                    if let Some(new_url) = params.get("image").and_then(|image| urls.get(image)) {
                        params.set("image", Some(new_url.clone()));
                    }
                }
                rewrite_image_urls(blocks, urls);
            }
            _ => {}
        }
    }
}

/// Resolves a `MediaItem.source`/Markdown image path against the
/// document's own directory - shared by every place that needs the actual
/// file behind a local reference (`upload_image_file` here, and
/// `media::sync_uploads`/`hash_local_file`).
pub(crate) fn resolve_local_path(source: &str, base_dir: Option<&Path>) -> PathBuf {
    let path = Path::new(source);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.map(|dir| dir.join(path)).unwrap_or_else(|| path.to_path_buf())
    }
}

/// Reads the actual bytes behind a `MediaItem.source`/Markdown image
/// destination - a local path resolved against the document's own
/// directory (see `resolve_local_path`), or fetched over plain HTTP for an
/// already-remote `http(s)://` source (e.g. an image in an article opened
/// via "Von WordPress öffnen", which was never local to begin with). Used
/// by `aialt.rs`, which needs the real image bytes to send to a vision-
/// capable LLM - unlike `sync_uploads`, which only ever needs to *upload*
/// a changed local file and can skip a remote source entirely.
pub(crate) fn read_image_bytes(source: &str, base_dir: Option<&Path>) -> Result<Vec<u8>, String> {
    if source.starts_with("http://") || source.starts_with("https://") {
        let config = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).build();
        let agent = ureq::Agent::new_with_config(config);
        let mut response = agent.get(source).call().map_err(|err| tr("Bild nicht abrufbar: {err}").replace("{err}", &err.to_string()))?;
        response
            .body_mut()
            .read_to_vec()
            .map_err(|err| tr("Bild nicht lesbar: {err}").replace("{err}", &err.to_string()))
    } else {
        let resolved = resolve_local_path(source, base_dir);
        std::fs::read(&resolved).map_err(|err| tr("Bild {path} nicht lesbar: {err}").replace("{path}", &resolved.display().to_string()).replace("{err}", &err.to_string()))
    }
}

/// Uploads (or, for the mediapanel "Erneut hochladen" case, re-uploads) the
/// image behind `path_str` - a local path/reference, resolved against
/// `base_dir` the usual way, *or* an already-remote `http(s)://` source
/// (e.g. one picked from the WordPress media library via
/// `medialibrary.rs`, or an image opened from an already-imported
/// article) - fetched over plain HTTP instead of read from disk, via
/// `read_image_bytes`, the same helper `aialt.rs` already relies on for
/// this exact local-vs-remote distinction. Re-uploading an already-remote
/// image is an unusual but valid thing to click "Erneut hochladen" for
/// (forces a fresh attachment), so this fixes it rather than needing the
/// mediapanel UI to hide/disable the button for a remote source instead.
pub(crate) fn upload_image_file(client: &wpclient::Client, path_str: &str, base_dir: Option<&Path>) -> wpclient::Result<wpclient::MediaResult> {
    let bytes = read_image_bytes(path_str, base_dir).map_err(|message| wpclient::ApiError { status: 0, message })?;
    let filename = image_filename(path_str);
    let compressed = crate::imagecompress::maybe_compress(&bytes, &filename);
    client.upload_media(&compressed.bytes, &compressed.filename, compressed.mime_type)
}

/// The display filename for a `MediaItem.source`/Markdown image
/// destination - the last path segment for a local path, or a remote
/// URL's last path segment with any `?query` stripped first (a cache-
/// busting `?ver=`-style parameter shouldn't end up baked into the
/// re-uploaded attachment's filename).
fn image_filename(source: &str) -> String {
    let without_query = source.split('?').next().unwrap_or(source);
    without_query.rsplit(['/', '\\']).next().filter(|s| !s.is_empty()).unwrap_or("image").to_string()
}

pub(crate) fn mime_from_extension(filename: &str) -> &'static str {
    match filename.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "m4a" => "audio/mp4",
        "flac" => "audio/flac",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The standard workflow: `![BU](bild.png "Alt")`, uploaded - goes to
    /// WordPress in the "large" size, like an image added in the block
    /// editor.
    #[test]
    fn an_uploaded_markdown_image_goes_out_in_the_large_size() {
        let sizes = vec![
            crate::wpclient::ImageSize { slug: "full".into(), url: "https://example.org/bild.png".into(), width: 2560, height: 1600 },
            crate::wpclient::ImageSize { slug: "large".into(), url: "https://example.org/bild-1280x800.png".into(), width: 1280, height: 800 },
            crate::wpclient::ImageSize { slug: "medium".into(), url: "https://example.org/bild-300x188.png".into(), width: 300, height: 188 },
        ];
        let mut media = media::reconcile(&[], "![Bildunterschrift](bild.png \"Alternativtext\")\n");
        media[0].wordpress = media::WordPressMediaRef::for_article(42, &sizes, "hash".into());
        let mut blocks = gutenberg::parse_markdown("![Bildunterschrift](bild.png \"Alternativtext\")\n");
        apply_media_metadata(&mut blocks, &media);
        let urls: std::collections::HashMap<String, String> = [("bild.png".to_string(), media[0].wordpress.as_ref().unwrap().url.clone())].into();
        rewrite_image_urls(&mut blocks, &urls);
        let out = gutenberg::render_blocks(&blocks);
        assert!(out.contains("\"id\":42") && out.contains("\"sizeSlug\":\"large\""), "{out}");
        assert!(out.contains("<figure class=\"wp-block-image size-large\">"), "{out}");
        assert!(out.contains("src=\"https://example.org/bild-1280x800.png\" alt=\"Alternativtext\" class=\"wp-image-42\" width=\"1280\" height=\"800\""), "{out}");
        assert!(out.contains("<figcaption class=\"wp-element-caption\">Bildunterschrift</figcaption>"), "{out}");
    }

    /// A cover's or media-text's local `image=` is uploaded like any other
    /// image and goes out with its attachment id.
    #[test]
    fn container_images_are_listed_uploaded_and_linked() {
        let sizes = |name: &str| {
            vec![
                crate::wpclient::ImageSize { slug: "full".into(), url: format!("https://example.org/{name}.png"), width: 2560, height: 1600 },
                crate::wpclient::ImageSize { slug: "large".into(), url: format!("https://example.org/{name}-1280x800.png"), width: 1280, height: 800 },
            ]
        };
        let md = "::: cover {image=titel.png dim=50}\n# Titel\n:::\n\n::: media-text {image=seite.png alt=\"Ein Bild\"}\nText\n:::\n\n![](seite.png \"Anderer Alt\")\n";
        let mut media = media::reconcile(&[], md);
        let mut sources: Vec<&str> = media.iter().map(|item| item.source.as_str()).collect();
        sources.sort_unstable();
        assert_eq!(sources, ["seite.png", "titel.png"]);
        for item in media.iter_mut() {
            item.wordpress = match item.source.as_str() {
                "titel.png" => media::WordPressMediaRef::for_article(42, &sizes("titel"), "hash".into()),
                _ => media::WordPressMediaRef::for_article(43, &sizes("seite"), "hash".into()),
            };
        }
        let mut blocks = gutenberg::parse_markdown(md);
        apply_media_metadata(&mut blocks, &media);
        let urls: std::collections::HashMap<String, String> = media.iter().map(|item| (item.source.clone(), item.wordpress.as_ref().unwrap().url.clone())).collect();
        rewrite_image_urls(&mut blocks, &urls);
        let out = gutenberg::render_blocks(&blocks);
        assert!(out.contains("<img class=\"wp-block-cover__image-background wp-image-42\" alt=\"\" src=\"https://example.org/titel-1280x800.png\""), "{out}");
        assert!(out.contains("\"mediaId\":43") && out.contains("\"mediaSizeSlug\":\"large\""), "{out}");
        assert!(out.contains("<img src=\"https://example.org/seite-1280x800.png\" alt=\"Ein Bild\" class=\"wp-image-43 size-large\"/>"), "{out}");
    }

    /// `[^1]` footnotes go out as WordPress's: references in the text,
    /// the list block at the end, the notes as `footnotes` meta.
    #[test]
    fn markdown_footnotes_become_wordpress_footnotes() {
        let md = "Ein Satz.[^1]\n\n[^1]: Die *Quelle*.\n";
        let html = gutenberg_preview_html(md, &[]);
        let id = gutenberg::footnotes::footnote_id("1");
        assert!(html.contains(&format!("<p>Ein Satz.<sup data-fn=\"{id}\" class=\"fn\"><a href=\"#{id}\" id=\"{id}-link\">1</a></sup></p>")), "{html}");
        assert!(html.trim_end().ends_with("<!-- wp:footnotes /-->"), "{html}");
        assert!(!html.contains("Quelle"), "{html}");
        let meta = footnotes_meta(&with_footnotes(md).1, Some("[{\"id\":\"alt\",\"content\":\"x\"}]")).unwrap();
        assert_eq!(meta, format!("[{{\"content\":\"Die <em>Quelle</em>.\",\"id\":\"{id}\"}}]"));
        // Without Markdown footnotes, an opened post's meta goes back.
        assert_eq!(footnotes_meta(&with_footnotes("Text").1, Some("[]")).as_deref(), Some("[]"));
        assert_eq!(footnotes_meta(&[], None), None);
    }

    /// A caption with a link survives the media list, which only keeps
    /// its text; a caption changed there replaces it.
    #[test]
    fn a_rich_caption_survives_the_media_list() {
        let md = "![Foto: [Name](https://example.org/) & Co](bild.png \"Alt\")\n";
        let mut media = media::reconcile(&[], md);
        assert_eq!(media[0].caption.as_deref(), Some("Foto: Name & Co"));
        let html = gutenberg_preview_html(md, &media);
        assert!(html.contains("<figcaption class=\"wp-element-caption\">Foto: <a href=\"https://example.org/\">Name</a> &amp; Co</figcaption>"), "{html}");
        media[0].caption = Some("Neu <b>".to_string());
        let html = gutenberg_preview_html(md, &media);
        assert!(html.contains("<figcaption class=\"wp-element-caption\">Neu &lt;b&gt;</figcaption>"), "{html}");
    }

    #[test]
    fn a_small_image_without_a_large_size_keeps_the_original() {
        let sizes = vec![crate::wpclient::ImageSize { slug: "full".into(), url: "https://example.org/klein.png".into(), width: 600, height: 400 }];
        let reference = media::WordPressMediaRef::for_article(7, &sizes, String::new()).unwrap();
        assert_eq!((reference.url.as_str(), reference.size_slug.as_deref()), ("https://example.org/klein.png", Some("full")));
    }

    #[test]
    fn an_image_used_twice_keeps_its_own_caption_at_each_place() {
        let item = media::MediaItem { id: "media-001".into(), filename: "a.png".into(), source: "a.png".into(), alt: media::AltText::Text("Alt aus der Galerie".into()), caption: Some("Bild 1".into()), wordpress: None, last_markdown_caption: None };
        let mut blocks = gutenberg::parse_markdown("```gallery\n![Bild 1](a.png)\n```\n\n![](a.png)\n");
        apply_media_metadata(&mut blocks, std::slice::from_ref(&item));
        let gutenberg::Block::Image { title, alt, .. } = &blocks[1] else { panic!("expected image") };
        assert_eq!((title.as_deref(), alt.as_str()), (None, ""));
        let mut single = gutenberg::parse_markdown("![](a.png)\n");
        apply_media_metadata(&mut single, &[item]);
        let gutenberg::Block::Image { title, .. } = &single[0] else { panic!("expected image") };
        assert_eq!(title.as_deref(), Some("Bild 1"));
    }

    #[test]
    fn publish_or_keep_publishes_a_post_that_does_not_exist_yet() {
        let fm = Frontmatter::default();
        assert_eq!(TargetStatus::PublishOrKeep.resolve(&fm), Some(PostStatus::Publish));
    }

    /// The bug this guards against: the sidebar resolved this once at
    /// build time (no document loaded yet), so updating an existing draft
    /// sent `status=publish`. Resolving against the live frontmatter must
    /// leave an existing post's status untouched - even a draft.
    #[test]
    fn publish_or_keep_leaves_an_existing_draft_alone() {
        let fm = Frontmatter { wp_post_id: Some(42), status: PostStatus::Draft, ..Frontmatter::default() };
        assert_eq!(TargetStatus::PublishOrKeep.resolve(&fm), None);
    }

    #[test]
    fn explicit_target_status_ignores_the_post_state() {
        let fm = Frontmatter { wp_post_id: Some(42), status: PostStatus::Publish, ..Frontmatter::default() };
        assert_eq!(TargetStatus::Set(PostStatus::Draft).resolve(&fm), Some(PostStatus::Draft));
        assert_eq!(TargetStatus::Set(PostStatus::Future).resolve(&Frontmatter::default()), Some(PostStatus::Future));
    }

    fn summary(id: u64, title: &str) -> wpclient::PostSummary {
        wpclient::PostSummary { id, title: title.to_string(), ..Default::default() }
    }

    #[test]
    fn an_interrupted_first_upload_is_found_by_its_exact_title() {
        let candidates = [summary(9, "Raspberry Pi 5 als NAS – Teil 2"), summary(7, "Raspberry Pi 5 als NAS"), summary(3, "Raspberry Pi 5 als NAS")];
        assert_eq!(recovered_post_id(&candidates, "Raspberry Pi 5 als NAS"), Some(7));
        assert_eq!(recovered_post_id(&candidates, "Etwas anderes"), None);
    }

    #[test]
    fn the_search_window_starts_a_day_before_the_attempt() {
        assert_eq!(search_window_start("2026-10-02T08:30:00Z").as_deref(), Some("2026-10-01T08:30:00"));
        assert_eq!(search_window_start("kaputt"), None);
    }

    #[test]
    fn no_conflict_when_there_is_no_known_baseline_to_compare_against() {
        assert!(!has_conflicting_server_change(None, "any-server-hash"));
    }

    #[test]
    fn no_conflict_when_the_server_hash_still_matches_the_baseline() {
        assert!(!has_conflicting_server_change(Some("abc123"), "abc123"));
    }

    #[test]
    fn conflict_when_the_server_hash_no_longer_matches_the_baseline() {
        assert!(has_conflicting_server_change(Some("abc123"), "def456"));
    }

    #[test]
    fn preview_url_appends_preview_true_to_a_plain_permalink() {
        assert_eq!(preview_url_for("https://example.com/?p=123"), "https://example.com/?p=123&preview=true");
    }

    #[test]
    fn preview_url_appends_preview_true_with_a_question_mark_when_the_link_has_no_query_string() {
        assert_eq!(preview_url_for("https://example.com/my-slug/"), "https://example.com/my-slug/?preview=true");
    }

    #[test]
    fn rewrite_image_urls_recurses_into_columns_and_gallery_blocks() {
        let urls: std::collections::HashMap<String, String> =
            [("local-a.png".to_string(), "https://example.com/a.png".to_string()), ("local-b.png".to_string(), "https://example.com/b.png".to_string())]
                .into_iter()
                .collect();
        let mut blocks = vec![
            gutenberg::Block::Columns {
                columns: vec![vec![gutenberg::Block::Image { url: "local-a.png".to_string(), alt: String::new(), title: None, media_id: None, width: 0, height: 0, link: None }]],
            },
            gutenberg::Block::Gallery {
                images: vec![gutenberg::GalleryImage { url: "local-b.png".to_string(), alt: String::new(), caption: None, media_id: None }],
                settings: gutenberg::GallerySettings::default(),
            },
        ];
        rewrite_image_urls(&mut blocks, &urls);
        let gutenberg::Block::Columns { columns } = &blocks[0] else { panic!("expected Columns") };
        let gutenberg::Block::Image { url, .. } = &columns[0][0] else { panic!("expected Image") };
        assert_eq!(url, "https://example.com/a.png");
        let gutenberg::Block::Gallery { images, .. } = &blocks[1] else { panic!("expected Gallery") };
        assert_eq!(images[0].url, "https://example.com/b.png");
    }

    #[test]
    fn rewrite_image_urls_recurses_into_details_blocks() {
        let urls: std::collections::HashMap<String, String> = [("local-c.png".to_string(), "https://example.com/c.png".to_string())].into_iter().collect();
        let mut blocks = vec![gutenberg::Block::Details {
            summary: "Mehr anzeigen".to_string(),
            blocks: vec![gutenberg::Block::Image { url: "local-c.png".to_string(), alt: String::new(), title: None, media_id: None, width: 0, height: 0, link: None }],
        }];
        rewrite_image_urls(&mut blocks, &urls);
        let gutenberg::Block::Details { blocks: inner, .. } = &blocks[0] else { panic!("expected Details") };
        let gutenberg::Block::Image { url, .. } = &inner[0] else { panic!("expected Image") };
        assert_eq!(url, "https://example.com/c.png");
    }

    #[test]
    fn gutenberg_preview_html_reflects_medienverwaltung_edits_not_just_the_markdown_source() {
        // Regression test: the "Gutenberg-Code" tab and the export dialog's
        // own "Vorschau" tab both used to call `gutenberg::markdown_to_gutenberg`
        // directly, which has no way to know about `Frontmatter.media` at
        // all - so a caption set in Medienverwaltung (with nothing written
        // as a Markdown image title) silently never showed up in either
        // preview, even though the real `run_export` publish path already
        // applied it correctly.
        let media = vec![media::MediaItem {
            id: "media-001".to_string(),
            filename: "cat.png".to_string(),
            source: "cat.png".to_string(),
            alt: media::AltText::Text("a red cat".to_string()),
            caption: Some("Our cat, sleeping".to_string()),
            wordpress: None,
            last_markdown_caption: None,
        }];
        let html = gutenberg_preview_html("![a red cat](cat.png)", &media);
        assert!(html.contains("<figcaption class=\"wp-element-caption\">Our cat, sleeping</figcaption>"), "{html}");
    }

    #[test]
    fn apply_media_metadata_overlays_alt_text_and_caption_from_the_matching_media_item() {
        let media = vec![media::MediaItem {
            id: "media-001".to_string(),
            filename: "cat.png".to_string(),
            source: "cat.png".to_string(),
            alt: media::AltText::Text("a red cat".to_string()),
            caption: Some("Our cat, sleeping".to_string()),
            wordpress: None,
            last_markdown_caption: None,
        }];
        let mut blocks = vec![gutenberg::Block::Image { url: "cat.png".to_string(), alt: String::new(), title: None, media_id: None, width: 0, height: 0, link: None }];
        apply_media_metadata(&mut blocks, &media);
        let gutenberg::Block::Image { alt, title, .. } = &blocks[0] else { panic!("expected Image") };
        assert_eq!(alt, "a red cat");
        assert_eq!(title.as_deref(), Some("Our cat, sleeping"));
    }

    #[test]
    fn apply_media_metadata_leaves_alt_untouched_while_undefined() {
        let media = vec![media::MediaItem {
            id: "media-001".to_string(),
            filename: "cat.png".to_string(),
            source: "cat.png".to_string(),
            alt: media::AltText::Undefined,
            caption: None,
            wordpress: None,
            last_markdown_caption: None,
        }];
        let mut blocks = vec![gutenberg::Block::Image { url: "cat.png".to_string(), alt: "from the markdown source".to_string(), title: None, media_id: None, width: 0, height: 0, link: None }];
        apply_media_metadata(&mut blocks, &media);
        let gutenberg::Block::Image { alt, .. } = &blocks[0] else { panic!("expected Image") };
        assert_eq!(alt, "from the markdown source");
    }

    #[test]
    fn apply_media_metadata_fills_in_the_uploaded_attachment_id_and_dimensions() {
        let media = vec![media::MediaItem {
            id: "media-001".to_string(),
            filename: "cat.png".to_string(),
            source: "cat.png".to_string(),
            alt: media::AltText::Undefined,
            caption: None,
            wordpress: Some(media::WordPressMediaRef { media_id: 123, url: "https://example.com/cat.png".to_string(), content_hash: "abc".to_string(), width: 640, height: 480, size_slug: None }),
            last_markdown_caption: None,
        }];
        let mut blocks = vec![gutenberg::Block::Image { url: "cat.png".to_string(), alt: String::new(), title: None, media_id: None, width: 0, height: 0, link: None }];
        apply_media_metadata(&mut blocks, &media);
        let gutenberg::Block::Image { media_id, width, height, .. } = &blocks[0] else { panic!("expected Image") };
        assert_eq!(*media_id, Some(123));
        assert_eq!((*width, *height), (640, 480));
    }

    /// Exercises the "Als Entwurf hochladen" vs "Veröffentlichen" choice
    /// directly: `run_export` must send whatever `target_status` it's given
    /// (the export-dialog buttons each force this to a specific value
    /// before calling it - see `wire_publish_button`), and a later call
    /// with a different status on the same `wp_post_id` must update it in
    /// place, not create a second post.
    #[test]
    #[ignore]
    fn run_export_respects_the_requested_post_status() {
        let site = wpsite::load();
        assert!(!site.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");
        let client = wpclient::Client::new(&site.url, &site.username, &password);

        let body = "Ein Testartikel für Entwurf/Veröffentlichen.\n";
        let mut frontmatter = Frontmatter {
            title: "Blocksatz draft/publish status test".to_string(),
            post_type: PostType::Post,
            slug: String::new(),
            status: crate::document::PostStatus::Draft,
            scheduled_at: None,
            categories: Vec::new(),
            tags: Vec::new(),
            excerpt: None,
            rank_math_title: None,
            rank_math_description: None,
            rank_math_focus_keyword: None,
            wp_footnotes: None,
            markdown_hint: false,
            featured_image: None,
            featured_image_alt: None,
            wp_post_id: None,
            wp_content_hash: None,
            wp_site: None,
            wp_modified_gmt: None,
            wp_synced_hash: None,
            wp_synced_at: None,
            wp_pending_create: None,
            lang: None,
            translation: None,
            featured_media_id: None,
            author_id: None,
            author_name: None,
            parent_id: None,
            parent_name: None,
            vgwort_ignored: false,
            comment_status: None,
            media: Vec::new(),
        };

        let created = run_export(&site, &password, &mut frontmatter, Some(crate::document::PostStatus::Draft), body, None).expect("draft export failed");
        assert_eq!(client.get_post(created.id).expect("get_post failed").status, "draft");

        frontmatter.wp_post_id = Some(created.id);
        let updated =
            run_export(&site, &password, &mut frontmatter, Some(crate::document::PostStatus::Publish), body, None).expect("publish export failed");
        assert_eq!(updated.id, created.id, "updating status must reuse the same post, not create a new one");
        assert_eq!(client.get_post(updated.id).expect("get_post failed").status, "publish");

        client.delete_post(created.id).expect("cleanup delete_post failed");
    }

    /// Regression test for the "Aktualisieren" button silently republishing
    /// a draft: `target_status: None` must send a request with no `status`
    /// field at all, so WordPress leaves the post's existing status
    /// untouched - a draft updated this way must still be a draft
    /// afterwards, not flip to published.
    #[test]
    #[ignore]
    fn run_export_with_no_target_status_leaves_the_existing_status_untouched() {
        let site = wpsite::load();
        assert!(!site.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");
        let client = wpclient::Client::new(&site.url, &site.username, &password);

        let body = "Ein Testartikel für das Aktualisieren-ohne-Statuswechsel-Verhalten.\n";
        let mut frontmatter = Frontmatter {
            title: "Blocksatz update-without-status-change test".to_string(),
            post_type: PostType::Post,
            slug: String::new(),
            status: crate::document::PostStatus::Draft,
            scheduled_at: None,
            categories: Vec::new(),
            tags: Vec::new(),
            excerpt: None,
            rank_math_title: None,
            rank_math_description: None,
            rank_math_focus_keyword: None,
            wp_footnotes: None,
            markdown_hint: false,
            featured_image: None,
            featured_image_alt: None,
            wp_post_id: None,
            wp_content_hash: None,
            wp_site: None,
            wp_modified_gmt: None,
            wp_synced_hash: None,
            wp_synced_at: None,
            wp_pending_create: None,
            lang: None,
            translation: None,
            featured_media_id: None,
            author_id: None,
            author_name: None,
            parent_id: None,
            parent_name: None,
            vgwort_ignored: false,
            comment_status: None,
            media: Vec::new(),
        };

        let created = run_export(&site, &password, &mut frontmatter, Some(crate::document::PostStatus::Draft), body, None).expect("draft export failed");
        assert_eq!(client.get_post(created.id).expect("get_post failed").status, "draft");

        frontmatter.wp_post_id = Some(created.id);
        let updated = run_export(&site, &password, &mut frontmatter, None, body, None).expect("status-less update failed");
        assert_eq!(updated.id, created.id, "updating content must reuse the same post, not create a new one");
        assert_eq!(client.get_post(updated.id).expect("get_post failed").status, "draft", "a status-less update must not change the post's status");

        client.delete_post(created.id).expect("cleanup delete_post failed");
    }

    /// Confirms `vgwort_ignored` actually reaches the real site and reads
    /// back correctly - both directions (setting it true, then flipping it
    /// back to false on the same post) - against the "Worthy" WordPress
    /// plugin's real `wp-worthy-pixel.ignored` REST field, not just that
    /// the local payload construction looks right.
    #[test]
    #[ignore]
    fn run_export_round_trips_vgwort_ignored() {
        let site = wpsite::load();
        assert!(!site.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");
        let client = wpclient::Client::new(&site.url, &site.username, &password);

        let body = "Ein Testartikel für den VG-Wort-Toggle.\n";
        let mut frontmatter = Frontmatter {
            title: "Blocksatz vgwort_ignored round-trip test".to_string(),
            post_type: PostType::Post,
            slug: String::new(),
            status: crate::document::PostStatus::Draft,
            scheduled_at: None,
            categories: Vec::new(),
            tags: Vec::new(),
            excerpt: None,
            rank_math_title: None,
            rank_math_description: None,
            rank_math_focus_keyword: None,
            wp_footnotes: None,
            markdown_hint: false,
            featured_image: None,
            featured_image_alt: None,
            wp_post_id: None,
            wp_content_hash: None,
            wp_site: None,
            wp_modified_gmt: None,
            wp_synced_hash: None,
            wp_synced_at: None,
            wp_pending_create: None,
            lang: None,
            translation: None,
            featured_media_id: None,
            author_id: None,
            author_name: None,
            parent_id: None,
            parent_name: None,
            vgwort_ignored: true,
            comment_status: None,
            media: Vec::new(),
        };

        let created = run_export(&site, &password, &mut frontmatter, Some(crate::document::PostStatus::Draft), body, None).expect("draft export failed");
        assert!(client.get_post(created.id).expect("get_post failed").vgwort_ignored, "expected the post to come back marked as VG-Wort-ignored");

        frontmatter.wp_post_id = Some(created.id);
        frontmatter.vgwort_ignored = false;
        run_export(&site, &password, &mut frontmatter, None, body, None).expect("update failed");
        assert!(!client.get_post(created.id).expect("get_post failed").vgwort_ignored, "expected the post to come back no longer VG-Wort-ignored");

        client.delete_post(created.id).expect("cleanup delete_post failed");
    }

    /// Confirms `comment_status` actually reaches the real site and reads
    /// back correctly - both directions (closing comments, then reopening
    /// them on the same post) - against WordPress's own `comment_status`
    /// field, not just that the local payload construction looks right.
    #[test]
    #[ignore]
    fn run_export_round_trips_comment_status() {
        let site = wpsite::load();
        assert!(!site.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");
        let client = wpclient::Client::new(&site.url, &site.username, &password);

        let body = "Ein Testartikel für den Kommentar-Status-Toggle.\n";
        let mut frontmatter = Frontmatter {
            title: "Blocksatz comment_status round-trip test".to_string(),
            post_type: PostType::Post,
            slug: String::new(),
            status: crate::document::PostStatus::Draft,
            scheduled_at: None,
            categories: Vec::new(),
            tags: Vec::new(),
            excerpt: None,
            rank_math_title: None,
            rank_math_description: None,
            rank_math_focus_keyword: None,
            wp_footnotes: None,
            markdown_hint: false,
            featured_image: None,
            featured_image_alt: None,
            wp_post_id: None,
            wp_content_hash: None,
            wp_site: None,
            wp_modified_gmt: None,
            wp_synced_hash: None,
            wp_synced_at: None,
            wp_pending_create: None,
            lang: None,
            translation: None,
            featured_media_id: None,
            author_id: None,
            author_name: None,
            parent_id: None,
            parent_name: None,
            vgwort_ignored: false,
            comment_status: Some(false),
            media: Vec::new(),
        };

        let created = run_export(&site, &password, &mut frontmatter, Some(crate::document::PostStatus::Draft), body, None).expect("draft export failed");
        assert_eq!(client.get_post(created.id).expect("get_post failed").comment_status, "closed");

        frontmatter.wp_post_id = Some(created.id);
        frontmatter.comment_status = Some(true);
        run_export(&site, &password, &mut frontmatter, None, body, None).expect("update failed");
        assert_eq!(client.get_post(created.id).expect("get_post failed").comment_status, "open");

        client.delete_post(created.id).expect("cleanup delete_post failed");
    }

    /// Exercises `run_export`'s own composition (local image path
    /// resolution + upload, category/tag name resolution, frontmatter ->
    /// REST payload mapping) against the real, already-configured
    /// WordPress site - not just `wpclient`'s lower-level calls, which
    /// `wpclient::tests` already covers. Ignored by default; run explicitly
    /// with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn run_export_with_local_image_and_terms_against_real_site() {
        let site = wpsite::load();
        assert!(!site.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");

        let doc_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let body = "# Blocksatz export test\n\nSome **text** with a local image below.\n\n![a red pixel](pixel.png)\n";

        let mut frontmatter = Frontmatter {
            title: "Blocksatz export test post".to_string(),
            post_type: PostType::Post,
            slug: String::new(),
            status: crate::document::PostStatus::Draft,
            scheduled_at: None,
            categories: vec!["Blocksatz Export Test".to_string()],
            tags: vec!["blocksatz-test".to_string()],
            excerpt: None,
            rank_math_title: None,
            rank_math_description: None,
            rank_math_focus_keyword: None,
            wp_footnotes: None,
            markdown_hint: false,
            featured_image: None,
            featured_image_alt: None,
            wp_post_id: None,
            wp_content_hash: None,
            wp_site: None,
            wp_modified_gmt: None,
            wp_synced_hash: None,
            wp_synced_at: None,
            wp_pending_create: None,
            lang: None,
            translation: None,
            featured_media_id: None,
            author_id: None,
            author_name: None,
            parent_id: None,
            parent_name: None,
            vgwort_ignored: false,
            comment_status: None,
            media: Vec::new(),
        };

        let created =
            run_export(&site, &password, &mut frontmatter, Some(crate::document::PostStatus::Draft), body, Some(&doc_dir)).expect("run_export failed");
        assert!(created.id > 0);

        // `run_export` reconciles + uploads media as a side effect - confirm
        // it actually tracked and uploaded the one local image, not just
        // that the post itself was created.
        assert_eq!(frontmatter.media.len(), 1);
        let uploaded = frontmatter.media[0].wordpress.clone().expect("expected the local image to have been uploaded");
        assert!(uploaded.media_id > 0);
        assert!(!uploaded.content_hash.is_empty());

        // Cleanup: the post, the uploaded media item, and the category/tag
        // terms `run_export` created.
        let client = wpclient::Client::new(&site.url, &site.username, &password);
        client.delete_post(created.id).expect("cleanup delete_post failed");
        client.delete_media(uploaded.media_id).expect("cleanup delete_media failed");
    }

    /// Exercises the duplicate-upload fix directly: exporting the same
    /// unchanged local image twice must reuse the same WordPress media id
    /// both times, not create a second attachment.
    #[test]
    #[ignore]
    fn run_export_does_not_reupload_an_unchanged_local_image() {
        let site = wpsite::load();
        assert!(!site.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");

        let doc_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let body = "# Blocksatz re-export test\n\n![a red pixel](pixel.png)\n";

        let mut frontmatter = Frontmatter {
            title: "Blocksatz re-export test post".to_string(),
            post_type: PostType::Post,
            slug: String::new(),
            status: crate::document::PostStatus::Draft,
            scheduled_at: None,
            categories: Vec::new(),
            tags: Vec::new(),
            excerpt: None,
            rank_math_title: None,
            rank_math_description: None,
            rank_math_focus_keyword: None,
            wp_footnotes: None,
            markdown_hint: false,
            featured_image: None,
            featured_image_alt: None,
            wp_post_id: None,
            wp_content_hash: None,
            wp_site: None,
            wp_modified_gmt: None,
            wp_synced_hash: None,
            wp_synced_at: None,
            wp_pending_create: None,
            lang: None,
            translation: None,
            featured_media_id: None,
            author_id: None,
            author_name: None,
            parent_id: None,
            parent_name: None,
            vgwort_ignored: false,
            comment_status: None,
            media: Vec::new(),
        };

        let first =
            run_export(&site, &password, &mut frontmatter, Some(crate::document::PostStatus::Draft), body, Some(&doc_dir)).expect("first run_export failed");
        let first_media_id = frontmatter.media[0].wordpress.clone().expect("expected an upload on the first export").media_id;

        // Re-export the identical body/frontmatter (as an update, since
        // `wp_post_id` now carries over) - the image content hasn't
        // changed, so this must NOT create a second media attachment.
        frontmatter.wp_post_id = Some(first.id);
        let _second = run_export(&site, &password, &mut frontmatter, Some(crate::document::PostStatus::Draft), body, Some(&doc_dir))
            .expect("second run_export failed");
        let second_media_id = frontmatter.media[0].wordpress.clone().expect("expected the ref to survive re-export").media_id;

        assert_eq!(first_media_id, second_media_id, "re-exporting an unchanged local image must reuse the same WordPress media id");

        let client = wpclient::Client::new(&site.url, &site.username, &password);
        client.delete_post(first.id).expect("cleanup delete_post failed");
        client.delete_media(first_media_id).expect("cleanup delete_media failed");
    }

    #[test]
    fn read_image_bytes_reads_a_local_file_relative_to_the_doc_dir() {
        let dir = std::env::temp_dir().join(format!("blocksatz-read-image-bytes-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("photo.png"), b"not a real png, just test bytes").unwrap();

        let bytes = read_image_bytes("photo.png", Some(&dir)).expect("expected the local file to be readable");
        assert_eq!(bytes, b"not a real png, just test bytes");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_image_bytes_reports_a_readable_error_for_a_missing_local_file() {
        let dir = std::env::temp_dir().join(format!("blocksatz-read-image-bytes-missing-test-{}", std::process::id()));
        let err = read_image_bytes("nope.png", Some(&dir)).expect_err("expected a missing file to be an error");
        assert!(err.contains("nope.png"), "{err}");
    }

    #[test]
    fn image_filename_takes_the_last_path_segment_of_a_local_path() {
        assert_eq!(image_filename("cat.png"), "cat.png");
        assert_eq!(image_filename("photos/2026/cat.png"), "cat.png");
    }

    #[test]
    fn image_filename_takes_the_last_path_segment_of_a_remote_url_without_its_query() {
        assert_eq!(image_filename("https://example.com/wp-content/uploads/2026/01/cat.png?ver=2"), "cat.png");
        assert_eq!(image_filename("https://example.com/wp-content/uploads/2026/01/cat.png"), "cat.png");
    }
}
