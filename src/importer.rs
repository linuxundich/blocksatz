//! The WordPress-article-browser embedded in `docsidebar.rs`'s
//! "Durchsuchen" page: lists existing posts - or, switched via the
//! "Artikel"/"Seiten" toggle at its top, static pages - on the configured
//! site; picking one fetches its full content (raw Gutenberg block HTML),
//! resolves its category/tag ids back to names, converts the content back
//! to Markdown (`gutenberg::gutenberg_to_markdown`), and hands the result
//! to the caller to populate the editor - `docsidebar.rs` owns what happens
//! with that (filling the buffer, frontmatter, clearing `current_path`
//! since there's no local file yet, then switching back to its own
//! "Dokument" page). Each row also carries a "In den Papierkorb" button,
//! moving that post/page to WordPress's own (recoverable) trash without
//! having to open wp-admin for it.
//!
//! `build_content` returns a plain widget, not a dialog - this used to be
//! its own modal "Von WordPress öffnen" dialog, folded into the sidebar so
//! browsing local files and WordPress articles both live in one place (see
//! the sidebar's own module doc comment for why).

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk4::glib;

use crate::document::{self, Frontmatter, PostStatus, PostType};
use crate::i18n::tr;
use crate::{secrets, syncstate, wpclient, wpsite};

pub struct ImportedPost {
    pub frontmatter: Frontmatter,
    pub body: String,
}

/// One status-grouped section of the post list ("Entwürfe"/"Veröffentlicht"/
/// "Weitere") - a heading plus its own boxed-list, hidden entirely while
/// its bucket is empty (e.g. no drafts exist).
#[derive(Clone)]
struct PostGroup {
    wrap: gtk4::Box,
    list_box: gtk4::ListBox,
    posts: Rc<RefCell<Vec<wpclient::PostSummary>>>,
}

fn build_post_group(title: &str) -> PostGroup {
    let heading = gtk4::Label::builder().label(title).xalign(0.0).build();
    heading.add_css_class("heading");

    let list_box = gtk4::ListBox::new();
    list_box.add_css_class("boxed-list");

    let wrap = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(6).build();
    wrap.append(&heading);
    wrap.append(&list_box);
    wrap.set_visible(false);

    PostGroup { wrap, list_box, posts: Rc::new(RefCell::new(Vec::new())) }
}

/// Everything the browser's callbacks share - bundled into one `Rc` so a
/// row's "In den Papierkorb" button can trigger a full reload (which
/// rebuilds that very row) without threading half a dozen clones through
/// every closure. Row closures only ever hold a `Weak` to it; `build_content`
/// stashes the one strong reference as data on the returned widget (see its
/// own comment), so it lives exactly as long as that widget does.
struct ImporterCtx {
    site: wpsite::SiteConfig,
    status_label: gtk4::Label,
    groups: [PostGroup; 3],
    post_type: Cell<PostType>,
}

impl ImporterCtx {
    fn set_lists_sensitive(&self, sensitive: bool) {
        for group in &self.groups {
            group.list_box.set_sensitive(sensitive);
        }
    }
}

/// Runs `job` (given the stored Application Password) on a background
/// thread and hands its outcome to `on_done` on the GTK thread - the same
/// spawn-then-poll shape every other network call in this dialog uses.
fn run_with_password<T: Send + 'static>(
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

/// Wires one group's row activation - shared logic (fetch, convert, hand
/// off to the caller) between the "Entwürfe"/"Veröffentlicht"/"Weitere"
/// sections; every section's list gets disabled together while a post is
/// loading, not just the one it was picked from. Unlike the old modal
/// dialog (which just closed itself on a successful pick), this widget
/// stays mounted inside the sidebar and can be shown again later, so the
/// lists are re-enabled either way - a stale "still loading" state would
/// otherwise persist the next time this page is shown.
fn wire_group_row_activation(group: &PostGroup, ctx: &Rc<ImporterCtx>, on_selected: Rc<dyn Fn(ImportedPost)>) {
    let posts = group.posts.clone();
    let ctx_weak = Rc::downgrade(ctx);
    group.list_box.connect_row_activated(move |_list_box, row| {
        let Some(ctx) = ctx_weak.upgrade() else { return };
        let Some(post) = posts.borrow().get(row.index() as usize).cloned() else {
            return;
        };
        ctx.set_lists_sensitive(false);
        ctx.status_label.set_label(&tr("Lade „{title}“ …").replace("{title}", &post.title));

        let post_type = ctx.post_type.get();
        let on_selected = on_selected.clone();
        let ctx_weak = Rc::downgrade(&ctx);
        run_with_password(
            &ctx.site,
            move |site, password| fetch_and_convert(site, password, post_type, post.id),
            move |outcome| {
                let Some(ctx) = ctx_weak.upgrade() else { return };
                match outcome {
                    Ok(imported) => {
                        ctx.status_label.set_label(&tr("Ausgewählt: „{title}“").replace("{title}", &imported.frontmatter.title));
                        ctx.set_lists_sensitive(true);
                        on_selected(imported);
                    }
                    Err(err) => {
                        ctx.status_label.set_label(&tr("Fehler: {err}").replace("{err}", &err));
                        ctx.set_lists_sensitive(true);
                    }
                }
            },
        );
    });
}

/// Builds the WordPress-article-browser widget embedded in the sidebar's
/// "Durchsuchen" page - status label, "Artikel"/"Seiten" toggle + refresh
/// button, and the three status-grouped lists. `on_selected` fires once a
/// row's post/page has actually been fetched and converted; the caller
/// decides what that means (fill the editor, switch pages, ...) - this
/// function's own job ends at handing over the `ImportedPost`.
pub fn build_content(on_selected: impl Fn(ImportedPost) + 'static) -> gtk4::Widget {
    let site = wpsite::load();

    let status_label = gtk4::Label::new(None);
    status_label.set_wrap(true);
    status_label.set_xalign(0.0);

    // Drafts first - that's what the user is most likely mid-way through
    // and looking for - then published, then anything else (pending
    // review, scheduled, private) in a catch-all last section.
    let drafts_group = build_post_group(&tr("Entwürfe"));
    let published_group = build_post_group(&tr("Veröffentlicht"));
    let other_group = build_post_group(&tr("Weitere"));

    let lists_container = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(18).build();
    lists_container.append(&drafts_group.wrap);
    lists_container.append(&published_group.wrap);
    lists_container.append(&other_group.wrap);

    let list_scroller = gtk4::ScrolledWindow::builder().child(&lists_container).vexpand(true).build();

    let refresh_button = gtk4::Button::from_icon_name("view-refresh-symbolic");
    refresh_button.set_tooltip_text(Some(&tr("Aktualisieren")));
    refresh_button.add_css_class("flat");

    // "Artikel" / "Seiten" - two linked toggle buttons rather than a
    // dropdown, since there are exactly two choices and switching between
    // them is the whole point of this control.
    let posts_toggle = gtk4::ToggleButton::builder().label(tr("Artikel")).active(true).hexpand(true).build();
    let pages_toggle = gtk4::ToggleButton::builder().label(tr("Seiten")).group(&posts_toggle).hexpand(true).build();
    let type_switcher = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).hexpand(true).build();
    type_switcher.add_css_class("linked");
    type_switcher.append(&posts_toggle);
    type_switcher.append(&pages_toggle);

    let toolbar = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).spacing(6).build();
    toolbar.append(&type_switcher);
    toolbar.append(&refresh_button);

    let content_box = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(12).build();
    content_box.append(&toolbar);
    content_box.append(&status_label);
    content_box.append(&list_scroller);

    if site.url.is_empty() {
        status_label.set_label(&tr("Keine WordPress-Verbindung eingerichtet - bitte zuerst in den Einstellungen konfigurieren."));
        type_switcher.set_sensitive(false);
        refresh_button.set_sensitive(false);
        return content_box.upcast();
    }

    let ctx = Rc::new(ImporterCtx {
        site,
        status_label,
        groups: [drafts_group, published_group, other_group],
        post_type: Cell::new(PostType::Post),
    });

    load_posts(&ctx);
    {
        let ctx_weak = Rc::downgrade(&ctx);
        refresh_button.connect_clicked(move |_| {
            if let Some(ctx) = ctx_weak.upgrade() {
                load_posts(&ctx);
            }
        });
    }
    {
        let ctx_weak = Rc::downgrade(&ctx);
        pages_toggle.connect_toggled(move |toggle| {
            let Some(ctx) = ctx_weak.upgrade() else { return };
            ctx.post_type.set(if toggle.is_active() { PostType::Page } else { PostType::Post });
            load_posts(&ctx);
        });
    }

    let on_selected: Rc<dyn Fn(ImportedPost)> = Rc::new(on_selected);
    for group in &ctx.groups {
        wire_group_row_activation(group, &ctx, on_selected.clone());
    }

    // Every closure above only holds a `Weak` - stashing the one strong
    // reference as data on the widget itself (rather than a dialog's
    // `connect_closed`, which no longer exists here) keeps the context
    // alive for exactly as long as this widget tree does.
    unsafe {
        content_box.set_data("importer-ctx", ctx);
    }

    content_box.upcast()
}

fn load_posts(ctx: &Rc<ImporterCtx>) {
    let post_type = ctx.post_type.get();
    ctx.status_label.set_label(&match post_type {
        PostType::Post => tr("Lade Artikel …"),
        PostType::Page => tr("Lade Seiten …"),
    });
    let ctx_weak = Rc::downgrade(ctx);
    run_with_password(
        &ctx.site,
        move |site, password| {
            wpclient::Client::new(&site.url, &site.username, password).list_items(post_type.rest_base()).map_err(|err| err.to_string())
        },
        move |outcome| {
            let Some(ctx) = ctx_weak.upgrade() else { return };
            // The user may have flipped the "Artikel"/"Seiten" toggle again
            // while this request was in flight - a stale result for the
            // other type must not overwrite the newer one's list.
            if ctx.post_type.get() != post_type {
                return;
            }
            match outcome {
                Ok(fetched) => {
                    let mut buckets: [Vec<wpclient::PostSummary>; 3] = [Vec::new(), Vec::new(), Vec::new()];
                    for post in fetched {
                        let bucket = match post.status.as_str() {
                            "draft" => 0,
                            "publish" => 1,
                            _ => 2,
                        };
                        buckets[bucket].push(post);
                    }
                    let total: usize = buckets.iter().map(Vec::len).sum();
                    for (index, (group, posts)) in ctx.groups.iter().zip(buckets).enumerate() {
                        populate_group(group, &posts, index == 2, &Rc::downgrade(&ctx));
                        *group.posts.borrow_mut() = posts;
                    }
                    ctx.set_lists_sensitive(true);
                    let message = match post_type {
                        PostType::Post => tr("{n} Artikel gefunden. Zum Öffnen auswählen."),
                        PostType::Page => tr("{n} Seiten gefunden. Zum Öffnen auswählen."),
                    };
                    ctx.status_label.set_label(&message.replace("{n}", &total.to_string()));
                }
                Err(err) => {
                    ctx.status_label.set_label(&tr("Fehler beim Laden: {err}").replace("{err}", &err));
                }
            }
        },
    );
}

/// Rebuilds one group's rows from `posts` and shows/hides the whole group
/// depending on whether it has anything to show. `show_status` includes
/// the status word in each row's subtitle (used for the catch-all "Weitere"
/// group, which mixes several statuses) - the "Entwürfe"/"Veröffentlicht"
/// groups don't need it, since their heading already says which.
fn populate_group(group: &PostGroup, posts: &[wpclient::PostSummary], show_status: bool, ctx: &Weak<ImporterCtx>) {
    while let Some(child) = group.list_box.first_child() {
        group.list_box.remove(&child);
    }
    for post in posts {
        let date = post.date.split('T').next().unwrap_or(&post.date);
        let subtitle = if show_status { format!("{} · {date}", status_display(&post.status)) } else { date.to_string() };
        let row = adw::ActionRow::builder().title(glib::markup_escape_text(&post.title).as_str()).subtitle(subtitle).activatable(true).build();

        let trash_button = gtk4::Button::from_icon_name("user-trash-symbolic");
        trash_button.set_tooltip_text(Some(&tr("In den Papierkorb")));
        trash_button.set_valign(gtk4::Align::Center);
        trash_button.add_css_class("flat");
        let ctx = ctx.clone();
        let post = post.clone();
        trash_button.connect_clicked(move |button| confirm_trash(button, &ctx, &post));
        row.add_suffix(&trash_button);

        group.list_box.append(&row);
    }
    group.wrap.set_visible(!posts.is_empty());
}

/// Asks before moving `post` to WordPress's trash - recoverable from
/// wp-admin, but still not something a stray click should do silently.
fn confirm_trash(anchor: &gtk4::Button, ctx: &Weak<ImporterCtx>, post: &wpclient::PostSummary) {
    let confirm = adw::AlertDialog::new(
        Some(&tr("In den Papierkorb verschieben?")),
        Some(&tr("„{title}“ wird in den WordPress-Papierkorb verschoben und kann dort im WordPress-Backend wiederhergestellt werden.").replace("{title}", &post.title)),
    );
    confirm.add_response("cancel", &tr("Abbrechen"));
    confirm.add_response("trash", &tr("In den Papierkorb"));
    confirm.set_response_appearance("trash", adw::ResponseAppearance::Destructive);
    confirm.set_default_response(Some("cancel"));
    confirm.set_close_response("cancel");

    let ctx = ctx.clone();
    let post = post.clone();
    confirm.connect_response(None, move |_, response| {
        if response != "trash" {
            return;
        }
        let Some(strong) = ctx.upgrade() else { return };
        strong.set_lists_sensitive(false);
        strong.status_label.set_label(&tr("Verschiebe „{title}“ in den Papierkorb …").replace("{title}", &post.title));
        let rest_base = strong.post_type.get().rest_base();
        let post_id = post.id;
        let ctx = ctx.clone();
        run_with_password(
            &strong.site,
            move |site, password| wpclient::Client::new(&site.url, &site.username, password).trash_item(rest_base, post_id).map_err(|err| err.to_string()),
            move |outcome| {
                let Some(ctx) = ctx.upgrade() else { return };
                match outcome {
                    Ok(()) => load_posts(&ctx),
                    Err(err) => {
                        ctx.status_label.set_label(&tr("Fehler: {err}").replace("{err}", &err));
                        ctx.set_lists_sensitive(true);
                    }
                }
            },
        );
    });
    confirm.present(Some(anchor));
}

fn status_display(status: &str) -> String {
    match status {
        "publish" => tr("Veröffentlicht"),
        "draft" => tr("Entwurf"),
        "pending" => tr("Ausstehend"),
        "future" => tr("Geplant"),
        "private" => tr("Privat"),
        other => other.to_string(),
    }
}

fn fetch_and_convert(site: &wpsite::SiteConfig, password: &str, post_type: PostType, post_id: u64) -> Result<ImportedPost, String> {
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
