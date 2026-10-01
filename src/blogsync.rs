//! The sync check (`docs/gui-redesign.md`, section 6): asks the blog for
//! the current status and `modified_gmt` of every working copy's post -
//! one request per post type, however many articles there are - and keeps
//! the answers in `DocContext::remote`, from which `syncstate::state`
//! derives "changed on the server", "conflict" and "gone". Runs at start,
//! when the window becomes active again (at most every two minutes) and
//! from the sidebar's refresh button. Failures leave the last known state
//! in place: offline, everything keeps working from local data.

use std::cell::Cell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use adw::prelude::*;

use crate::document::{PostStatus, PostType};
use crate::syncstate::Remote;
use crate::window::DocContext;
use crate::{importer, library, wpclient, wpsite};

const MIN_INTERVAL: Duration = Duration::from_secs(120);

thread_local! {
    static LAST_RUN: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Checks all working copies against the blog now.
pub fn refresh(ctx: &DocContext) {
    let site = wpsite::load();
    if site.url.is_empty() {
        return;
    }
    LAST_RUN.with(|last| last.set(Some(Instant::now())));

    let site_id = site.site_id();
    let mut posts = Vec::new();
    let mut pages = Vec::new();
    let mut documents: Vec<_> = library::scan(&library::root()).into_iter().map(|entry| entry.document.frontmatter).collect();
    documents.push(ctx.frontmatter.borrow().clone());
    for fm in documents {
        let Some(id) = fm.wp_post_id else { continue };
        if fm.wp_site.as_deref().is_some_and(|site| site != site_id) {
            continue;
        }
        let ids = if fm.post_type == PostType::Page { &mut pages } else { &mut posts };
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    if posts.is_empty() && pages.is_empty() {
        return;
    }

    let ctx = ctx.clone();
    importer::run_with_password(
        &site,
        move |site, password| {
            let client = wpclient::Client::new(&site.url, &site.username, password);
            let mut found = HashMap::new();
            for (rest_base, ids) in [("posts", &posts), ("pages", &pages)] {
                if ids.is_empty() {
                    continue;
                }
                for post in client.remote_states(rest_base, ids).map_err(|err| err.to_string())? {
                    found.insert(post.id, post);
                }
                for id in ids.iter() {
                    found.entry(*id).or_insert_with(|| wpclient::PostSummary { id: *id, status: "trash".into(), ..Default::default() });
                }
            }
            Ok(found)
        },
        move |outcome: Result<HashMap<u64, wpclient::PostSummary>, String>| {
            let Ok(found) = outcome else { return };
            {
                let mut remote = ctx.remote.borrow_mut();
                for (id, post) in found {
                    remote.insert(id, to_remote(&post));
                }
            }
            ctx.notify_library(true);
        },
    );
}

/// `refresh`, unless the last check is less than two minutes old - for
/// the "window became active again" trigger.
pub fn refresh_if_stale(ctx: &DocContext) {
    let stale = LAST_RUN.with(|last| last.get().is_none_or(|at| at.elapsed() >= MIN_INTERVAL));
    if stale {
        refresh(ctx);
    }
}

/// Records what an upload just told us, so the state is right before the
/// next check.
pub fn record_upload(ctx: &DocContext, post: &wpclient::PostResult, status: PostStatus) {
    ctx.remote.borrow_mut().insert(post.id, Remote::Present { modified_gmt: post.modified_gmt.clone(), status, link: post.link.clone() });
}

fn to_remote(post: &wpclient::PostSummary) -> Remote {
    if post.status == "trash" {
        Remote::Gone
    } else {
        Remote::Present { modified_gmt: post.modified_gmt.clone(), status: PostStatus::from_str(&post.status), link: post.link.clone() }
    }
}

/// Starts the checks: once now, and whenever the window becomes active.
pub fn wire(window: &adw::ApplicationWindow, ctx: &DocContext) {
    refresh(ctx);
    let ctx = ctx.clone();
    window.connect_is_active_notify(move |window| {
        if window.is_active() {
            refresh_if_stale(&ctx);
        }
    });
}
