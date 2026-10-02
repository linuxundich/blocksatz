//! Fetching an existing post from WordPress for editing: its full content
//! (raw Gutenberg block HTML) converted back to Markdown
//! (`gutenberg::gutenberg_to_markdown`), category/tag ids resolved back to
//! names, and the sync baseline recorded (`syncstate::mark_synced`). The
//! list to pick from is the blog archive page (`blogposts.rs`); what
//! happens with the result - a working copy in the library - is up to the
//! caller.

use std::sync::mpsc;
use std::time::Duration;

use gtk4::glib;

use crate::document::{self, Frontmatter, PostStatus, PostType};
use crate::i18n::tr;
use crate::{secrets, syncstate, wpclient, wpsite};

pub struct ImportedPost {
    pub frontmatter: Frontmatter,
    pub body: String,
}

/// Runs `job` with the site's Application Password on a worker thread and
/// hands its result to `on_done` back on the main loop.
pub(crate) fn run_with_password<T: Send + 'static>(
    site: &wpsite::SiteConfig,
    job: impl FnOnce(&wpsite::SiteConfig, &str) -> Result<T, String> + Send + 'static,
    on_done: impl Fn(Result<T, String>) + 'static,
) {
    let site = site.clone();
    let (tx, rx) = mpsc::channel::<Result<T, String>>();
    std::thread::spawn(move || {
        let outcome = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .map_err(|err| err.to_string())
            .and_then(|maybe_password| maybe_password.ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden.")))
            .and_then(|password| job(&site, &password));
        let _ = tx.send(outcome);
    });
    glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
        Ok(outcome) => {
            on_done(outcome);
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            on_done(Err(tr("Interner Fehler: Lade-Thread hat kein Ergebnis geliefert.")));
            glib::ControlFlow::Break
        }
    });
}

pub(crate) fn status_display(status: &str) -> String {
    match status {
        "publish" => tr("Veröffentlicht"),
        "draft" => tr("Entwurf"),
        "pending" => tr("Ausstehend"),
        "future" => tr("Geplant"),
        "private" => tr("Privat"),
        other => other.to_string(),
    }
}

pub(crate) fn fetch_and_convert(site: &wpsite::SiteConfig, password: &str, post_type: PostType, post_id: u64) -> Result<ImportedPost, String> {
    let client = wpclient::Client::new(&site.url, &site.username, password);
    let detail = client.get_item(post_type.rest_base(), post_id).map_err(|err| err.to_string())?;

    let mut categories = Vec::new();
    for id in &detail.categories {
        categories.push(client.get_term_name("categories", *id).map_err(|err| err.to_string())?);
    }
    let mut tags = Vec::new();
    for id in &detail.tags {
        tags.push(client.get_term_name("tags", *id).map_err(|err| err.to_string())?);
    }
    // Best-effort only - a deleted/inaccessible author id shouldn't block
    // opening the rest of an otherwise perfectly importable post, so a
    // lookup failure just leaves the name blank (the id itself is still
    // kept below) rather than surfacing as an import error.
    let author_name = (detail.author != 0).then(|| client.get_user_name(detail.author).ok()).flatten();
    // Best-effort, same reasoning as `author_name` above - an inaccessible
    // or already-deleted parent page shouldn't block importing this one;
    // its id is kept below regardless, just without a cached title.
    let parent_name = (detail.parent != 0).then(|| client.get_item(post_type.rest_base(), detail.parent).ok().map(|d| d.title)).flatten();

    let body = gutenberg::gutenberg_to_markdown(&detail.content);
    let is_future = detail.status == "future";
    let frontmatter = Frontmatter {
        title: detail.title,
        post_type,
        slug: detail.slug,
        status: PostStatus::from_str(&detail.status),
        scheduled_at: is_future.then_some(detail.date).filter(|d| !d.is_empty()),
        categories,
        tags,
        excerpt: (!detail.excerpt.is_empty()).then_some(detail.excerpt),
        rank_math_title: (!detail.rank_math_title.is_empty()).then_some(detail.rank_math_title),
        rank_math_description: (!detail.rank_math_description.is_empty()).then_some(detail.rank_math_description),
        rank_math_focus_keyword: (!detail.rank_math_focus_keyword.is_empty()).then_some(detail.rank_math_focus_keyword),
        wp_footnotes: (!detail.footnotes.is_empty() && detail.footnotes != "[]").then_some(detail.footnotes),
        featured_image: None,
        featured_image_alt: None,
        wp_post_id: Some(detail.id),
        // The just-fetched content is, by definition, in sync with the
        // server right now - establishes a baseline so `export.rs`'s
        // revision-conflict check can detect a *later* external edit,
        // right from the moment this post is opened rather than only
        // after the first local publish.
        wp_content_hash: Some(document::content_hash(&detail.content)),
        // Filled in by `mark_synced` below, together with the fingerprint.
        wp_site: None,
        wp_modified_gmt: None,
        wp_synced_hash: None,
        wp_synced_at: None,
        wp_pending_create: None,
        featured_media_id: (detail.featured_media != 0).then_some(detail.featured_media),
        author_id: (detail.author != 0).then_some(detail.author),
        author_name,
        parent_id: (detail.parent != 0).then_some(detail.parent),
        parent_name,
        vgwort_ignored: detail.vgwort_ignored,
        comment_status: Some(detail.comment_status == "open"),
        media: crate::media::reconcile(&[], &body),
    };

    let mut doc = document::Document { frontmatter, body };
    syncstate::mark_synced(&mut doc, &site.site_id(), &detail.modified_gmt, &syncstate::now_rfc3339());
    Ok(ImportedPost { frontmatter: doc.frontmatter, body: doc.body })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads an existing, real, already-published post that's known (as of
    /// writing) to carry categories, tags, AND a featured image - checking
    /// that `fetch_and_convert` actually surfaces all three, not just the
    /// content. Read-only (no mutation of the site), but pinned to a
    /// specific post id, so it'll need updating if that post ever changes
    /// categories/tags/featured image, or is deleted.
    #[test]
    #[ignore]
    fn fetch_and_convert_surfaces_categories_tags_and_featured_image() {
        let site = wpsite::load();
        assert!(!site.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");

        // "Rutile 0.2.2 – Eine moderne Alternative zu Tilix" on
        // linuxundich.de: category "GNU/Linux", tags including "Gnome", and
        // a featured image (media id 45270 as of writing).
        let imported = fetch_and_convert(&site, &password, PostType::Post, 45269).expect("fetch_and_convert failed");

        assert!(!imported.frontmatter.categories.is_empty(), "expected at least one category, got none");
        assert!(!imported.frontmatter.tags.is_empty(), "expected at least one tag, got none");
        assert!(
            imported.frontmatter.tags.iter().any(|t| t == "Gnome"),
            "expected tag 'Gnome' among {:?}",
            imported.frontmatter.tags
        );
        assert!(imported.frontmatter.featured_media_id.is_some(), "expected a featured_media_id, got None");
    }

    /// Creates a real post on the configured WordPress site (content
    /// generated by our own forward converter, exercising most block
    /// types), fetches it back, and converts it to Markdown - checking
    /// that actual WordPress storage/serving of the content (not just our
    /// own in-memory forward+reverse conversion) round-trips cleanly.
    /// Ignored by default; run explicitly with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn open_existing_post_round_trips_through_real_wordpress() {
        let site = wpsite::load();
        assert!(!site.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");
        let client = wpclient::Client::new(&site.url, &site.username, &password);

        let markdown = "# Rundreise-Test\n\n\
             Ein **fetter** Text mit [Link](https://example.com).\n\n\
             - eins\n- zwei\n\n\
             > Zitat\n\n\
             ```\ncode\n```\n\n\
             ![alt](https://example.com/x.png)\n\n\
             ---\n\n\
             | A | B |\n|---|---|\n| 1 | 2 |\n";
        let content = gutenberg::markdown_to_gutenberg(markdown);

        let created = client
            .create_post(&serde_json::json!({
                "title": "Blocksatz round-trip test",
                "content": content,
                "status": "draft",
            }))
            .expect("create_post failed");

        let imported = fetch_and_convert(&site, &password, PostType::Post, created.id).expect("fetch_and_convert failed");

        assert_eq!(imported.frontmatter.title, "Blocksatz round-trip test");
        assert_eq!(imported.frontmatter.wp_post_id, Some(created.id));
        assert!(imported.body.contains("# Rundreise-Test"), "body was:\n{}", imported.body);
        assert!(imported.body.contains("**fetter**"), "body was:\n{}", imported.body);
        assert!(imported.body.contains("[Link](https://example.com)"), "body was:\n{}", imported.body);
        assert!(imported.body.contains("- eins"), "body was:\n{}", imported.body);
        assert!(imported.body.contains("> Zitat"), "body was:\n{}", imported.body);
        assert!(imported.body.contains("```\ncode\n```"), "body was:\n{}", imported.body);
        assert!(imported.body.contains("![alt](https://example.com/x.png)"), "body was:\n{}", imported.body);
        assert!(imported.body.contains("---"), "body was:\n{}", imported.body);
        assert!(imported.body.contains("| A | B |"), "body was:\n{}", imported.body);

        client.delete_post(created.id).expect("cleanup delete_post failed");
    }

    #[test]
    #[ignore]
    fn list_posts_against_real_site() {
        let site = wpsite::load();
        assert!(!site.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");
        let client = wpclient::Client::new(&site.url, &site.username, &password);

        let posts = client.list_posts().expect("list_posts failed");
        assert!(!posts.is_empty(), "expected at least one existing post on the real site");
        assert!(
            posts.iter().any(|p| p.link.starts_with("http")),
            "expected at least one post to carry a real permalink, got {:?}",
            posts.iter().map(|p| &p.link).collect::<Vec<_>>()
        );
    }
}
