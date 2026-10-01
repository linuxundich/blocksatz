//! How a working copy relates to its WordPress post - the state model of
//! `docs/gui-redesign.md` (section 4). Two independent dimensions:
//!
//! - the post's WordPress status (`PostStatus`, or "only local" when it was
//!   never uploaded), and
//! - its sync state: in sync, local changes not uploaded yet, changed on
//!   the server, both (conflict), or gone from the server.
//!
//! Everything here is pure (no GTK, no network), so it can back the
//! library sidebar's per-row state and the main action's label alike.

// Parts of the state model are only read by the library sidebar and the
// main action, which arrive in the next redesign phases.
#![allow(dead_code)]

use sha2::Digest;

use crate::document::{self, Document, Frontmatter, PostStatus};

/// Hash over everything a user can change that ends up on WordPress - the
/// Markdown body plus the metadata that gets sent - and nothing that only
/// records sync bookkeeping (`wp_*` fields, the featured media id, and each
/// image's upload reference), since those change *because of* an upload
/// and must not make a just-synced document look edited.
///
/// Media items are reduced to `(source, alt, caption)` and sorted, so
/// re-running `media::reconcile` (which can assign fresh internal ids) on
/// an otherwise unchanged document doesn't change the fingerprint either.
pub fn fingerprint(doc: &Document) -> String {
    let mut media: Vec<String> = doc
        .frontmatter
        .media
        .iter()
        .map(|item| format!("{}\u{1f}{:?}\u{1f}{:?}", item.source, item.alt, item.caption))
        .collect();
    media.sort();

    let editable = Frontmatter {
        wp_post_id: None,
        wp_content_hash: None,
        wp_site: None,
        wp_modified_gmt: None,
        wp_synced_hash: None,
        wp_synced_at: None,
        featured_media_id: None,
        media: Vec::new(),
        ..doc.frontmatter.clone()
    };
    let mut hasher = sha2::Sha256::new();
    hasher.update(document::serialize(&Document { frontmatter: editable, body: doc.body.clone() }).as_bytes());
    for item in media {
        hasher.update([0x1e]);
        hasher.update(item.as_bytes());
    }
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Records `doc` as being in sync with WordPress right now - called after
/// an import and after every successful upload, with the server's own
/// `modified_gmt` from that same response.
pub fn mark_synced(doc: &mut Document, site_id: &str, modified_gmt: &str, now_rfc3339: &str) {
    doc.frontmatter.wp_site = Some(site_id.to_string());
    doc.frontmatter.wp_modified_gmt = (!modified_gmt.is_empty()).then(|| modified_gmt.to_string());
    doc.frontmatter.wp_synced_at = Some(now_rfc3339.to_string());
    doc.frontmatter.wp_synced_hash = None;
    doc.frontmatter.wp_synced_hash = Some(fingerprint(doc));
}

/// The current time as RFC 3339 UTC, for `mark_synced`.
pub fn now_rfc3339() -> String {
    gtk4::glib::DateTime::now_utc().and_then(|now| now.format_iso8601()).map(|s| s.to_string()).unwrap_or_default()
}

/// What a list fetch says about the post on the server right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Remote {
    /// Not fetched (yet), or the site is unreachable - judge from local
    /// data alone.
    Unknown,
    /// The post exists; its current `modified_gmt` and status.
    Present { modified_gmt: String, status: PostStatus },
    /// Deleted or moved to the trash on the server.
    Gone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    /// Never uploaded.
    LocalOnly,
    InSync,
    /// Edited here since the last upload.
    LocalChanges,
    /// Edited on the server (wp-admin) since the last sync; nothing local.
    RemoteChanged,
    /// Edited on both sides.
    Conflict,
    /// The post no longer exists on the server.
    RemoteGone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostState {
    /// `None` while the document was never uploaded.
    pub status: Option<PostStatus>,
    pub sync: SyncState,
}

/// Whether the local document changed since its last sync. A document
/// without a recorded fingerprint (synced before `wp_synced_hash`
/// existed) counts as changed - uploading once records one, and treating
/// it as unchanged could hide real edits.
pub fn has_local_changes(doc: &Document) -> bool {
    match &doc.frontmatter.wp_synced_hash {
        Some(hash) => *hash != fingerprint(doc),
        None => true,
    }
}

/// Combines local and remote knowledge into one `PostState`.
pub fn state(doc: &Document, remote: &Remote) -> PostState {
    let fm = &doc.frontmatter;
    if fm.wp_post_id.is_none() {
        return PostState { status: None, sync: SyncState::LocalOnly };
    }
    let local_changes = has_local_changes(doc);
    let (status, sync) = match remote {
        Remote::Gone => (fm.status, SyncState::RemoteGone),
        Remote::Unknown => (fm.status, if local_changes { SyncState::LocalChanges } else { SyncState::InSync }),
        Remote::Present { modified_gmt, status } => {
            // Only "newer than what we last saw" counts: WordPress bumps
            // `modified_gmt` on every save, but an unknown baseline (older
            // working copies) can't tell an edit from our own last upload.
            let remote_changes = fm.wp_modified_gmt.as_ref().is_some_and(|known| modified_gmt > known);
            let sync = match (local_changes, remote_changes) {
                (false, false) => SyncState::InSync,
                (true, false) => SyncState::LocalChanges,
                (false, true) => SyncState::RemoteChanged,
                (true, true) => SyncState::Conflict,
            };
            (*status, sync)
        }
    };
    PostState { status: Some(status), sync }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{AltText, MediaItem};

    fn doc(body: &str) -> Document {
        Document { frontmatter: Frontmatter { title: "Titel".into(), ..Frontmatter::default() }, body: body.into() }
    }

    fn synced(body: &str, status: PostStatus) -> Document {
        let mut d = doc(body);
        d.frontmatter.wp_post_id = Some(7);
        d.frontmatter.status = status;
        mark_synced(&mut d, "example.org", "2026-10-01T10:00:00", "2026-10-01T10:00:05Z");
        d
    }

    fn present(modified_gmt: &str, status: PostStatus) -> Remote {
        Remote::Present { modified_gmt: modified_gmt.into(), status }
    }

    #[test]
    fn never_uploaded_is_local_only() {
        assert_eq!(state(&doc("Text"), &Remote::Unknown), PostState { status: None, sync: SyncState::LocalOnly });
    }

    #[test]
    fn freshly_synced_is_in_sync() {
        let d = synced("Text", PostStatus::Draft);
        assert!(!has_local_changes(&d));
        assert_eq!(state(&d, &present("2026-10-01T10:00:00", PostStatus::Draft)).sync, SyncState::InSync);
        assert_eq!(state(&d, &Remote::Unknown).sync, SyncState::InSync);
    }

    #[test]
    fn body_and_metadata_edits_are_local_changes() {
        let mut d = synced("Text", PostStatus::Draft);
        d.body.push_str("mehr");
        assert_eq!(state(&d, &Remote::Unknown).sync, SyncState::LocalChanges);

        let mut d = synced("Text", PostStatus::Draft);
        d.frontmatter.tags.push("Linux".into());
        assert!(has_local_changes(&d));
    }

    #[test]
    fn sync_bookkeeping_does_not_count_as_a_change() {
        let mut d = synced("![Bild](a.png)\n", PostStatus::Draft);
        d.frontmatter.media = vec![MediaItem {
            id: "1".into(),
            filename: "a.png".into(),
            source: "a.png".into(),
            alt: AltText::Text("Ein Bild".into()),
            caption: None,
            wordpress: None,
            last_markdown_caption: None,
        }];
        mark_synced(&mut d, "example.org", "2026-10-01T10:00:00", "2026-10-01T10:00:05Z");
        d.frontmatter.wp_content_hash = Some("anders".into());
        d.frontmatter.featured_media_id = Some(99);
        d.frontmatter.media[0].id = "neu".into();
        assert!(!has_local_changes(&d));

        d.frontmatter.media[0].alt = AltText::Text("Anderer Text".into());
        assert!(has_local_changes(&d));
    }

    /// What happens right after an import: `importer.rs` reconciles the
    /// media list once, and the live preview reconciles it again on the
    /// first tick - that second pass must not look like an edit.
    #[test]
    fn reconciling_media_again_after_an_import_is_not_a_change() {
        let body = "Text\n\n![Ein Pi](https://example.org/pi.jpg)\n\n![](lokal.png \"Unterschrift\")\n";
        let mut d = doc(body);
        d.frontmatter.wp_post_id = Some(5);
        d.frontmatter.media = crate::media::reconcile(&[], body);
        mark_synced(&mut d, "example.org", "2026-10-01T10:00:00", "2026-10-01T10:00:05Z");

        d.frontmatter.media = crate::media::reconcile(&d.frontmatter.media, body);
        assert!(!has_local_changes(&d));
        let reparsed = document::parse(&document::serialize(&d));
        let mut reparsed = Document { frontmatter: reparsed.frontmatter, body: reparsed.body };
        reparsed.frontmatter.media = crate::media::reconcile(&reparsed.frontmatter.media, &reparsed.body);
        assert!(!has_local_changes(&reparsed));
    }

    #[test]
    fn newer_server_modification_is_a_remote_change_or_conflict() {
        let d = synced("Text", PostStatus::Draft);
        assert_eq!(state(&d, &present("2026-10-02T08:00:00", PostStatus::Draft)).sync, SyncState::RemoteChanged);

        let mut edited = d.clone();
        edited.body.push('!');
        assert_eq!(state(&edited, &present("2026-10-02T08:00:00", PostStatus::Draft)).sync, SyncState::Conflict);
    }

    #[test]
    fn remote_status_wins_over_the_local_one() {
        // Scheduled post went live on its own.
        let d = synced("Text", PostStatus::Future);
        let s = state(&d, &present("2026-10-01T10:00:00", PostStatus::Publish));
        assert_eq!(s.status, Some(PostStatus::Publish));
    }

    #[test]
    fn deleted_on_the_server_is_gone() {
        let d = synced("Text", PostStatus::Publish);
        assert_eq!(state(&d, &Remote::Gone).sync, SyncState::RemoteGone);
    }

    #[test]
    fn documents_synced_before_fingerprints_existed_count_as_changed() {
        let mut d = doc("Text");
        d.frontmatter.wp_post_id = Some(3);
        assert_eq!(state(&d, &Remote::Unknown).sync, SyncState::LocalChanges);
    }
}
