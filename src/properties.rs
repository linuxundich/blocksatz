//! "Artikel-Eigenschaften" dialog: edits the document's frontmatter
//! (title/slug/status/categories/tags/featured image) in place. Changes are
//! synced live into the shared `Frontmatter` cell as the user types, mirroring
//! how GNOME preferences dialogs apply immediately without an OK button.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk4::{gio, glib};

use crate::document::{self, parse_list, Frontmatter, PostStatus, PostType};
use crate::i18n::tr;
use crate::{aialt, autocomplete, media, preview, secrets, tagsuggest, taxonomy, termcache, wpclient, wpsite};

/// Shows/hides an alt-text-length warning icon and sets its tooltip from
/// `media::alt_text_length_warning` - same non-blocking hint as
/// `mediapanel.rs`/`imagealt.rs` use for body-image alt text, applied here
/// to the featured image's alt text.
fn update_alt_length_warning(icon: &gtk4::Image, text: &str) {
    match media::alt_text_length_warning(text) {
        Some(message) => {
            icon.set_tooltip_text(Some(&message));
            icon.set_visible(true);
        }
        None => icon.set_visible(false),
    }
}

/// Google's search results truncate a URL display around this many
/// characters - past it, the SEO recommendation is a shorter slug (or,
/// where possible, a shorter category).
const RECOMMENDED_MAX_URL_LENGTH: usize = 73;

/// Builds the full URL a post's `slug` would actually publish at (domain,
/// the post's assumed-primary i.e. first category slug, and the post slug
/// itself) and its character count, for the SEO length check.
/// `category_slug` is `None` when no category is set yet (the check still
/// runs, just without that segment - WordPress would assign a default
/// category, whose real slug isn't knowable without an authenticated
/// site-settings lookup this app doesn't have).
fn seo_url_preview(domain: &str, category_slug: Option<&str>, slug: &str) -> (String, usize) {
    let mut url = domain.trim_end_matches('/').to_string();
    if let Some(category_slug) = category_slug.filter(|s| !s.is_empty()) {
        url.push('/');
        url.push_str(category_slug);
    }
    url.push('/');
    url.push_str(slug);
    url.push('/');
    let length = url.chars().count();
    (url, length)
}

/// Updates the "URL-Länge (SEO)" row from the current title/category/slug
/// state - called once at dialog build time and again on every slug/
/// category edit (`seo_url_preview` needs both).
fn refresh_url_length_row(
    row: &adw::ActionRow,
    icon: &gtk4::Image,
    domain: &str,
    frontmatter: &Rc<RefCell<Frontmatter>>,
    category_slugs: &Rc<RefCell<HashMap<String, String>>>,
) {
    let fm = frontmatter.borrow();
    if domain.is_empty() || fm.slug.is_empty() {
        row.set_subtitle(&tr("Wird berechnet, sobald WordPress-Seite und Slug gesetzt sind."));
        icon.set_visible(false);
        return;
    }
    let category_slug = fm.categories.first().map(|name| category_slugs.borrow().get(name).cloned().unwrap_or_else(|| document::slugify(name)));
    let (url, length) = seo_url_preview(domain, category_slug.as_deref(), &fm.slug);

    icon.set_visible(true);
    if length <= RECOMMENDED_MAX_URL_LENGTH {
        row.set_subtitle(&tr("{url} ({length} Zeichen)").replace("{url}", &url).replace("{length}", &length.to_string()));
        icon.set_icon_name(Some("object-select-symbolic"));
        icon.remove_css_class("error");
        icon.add_css_class("success");
    } else {
        row.set_subtitle(
            &tr("{url} ({length} Zeichen, empfohlen: max. {max})")
                .replace("{url}", &url)
                .replace("{length}", &length.to_string())
                .replace("{max}", &RECOMMENDED_MAX_URL_LENGTH.to_string()),
        );
        icon.set_icon_name(Some("dialog-warning-symbolic"));
        icon.remove_css_class("success");
        icon.add_css_class("error");
    }
}

/// Splits `tags_text`'s comma-separated tags into `(tag, already_exists)`
/// pairs (`termcache::term_exists` - case-insensitive, matching how
/// WordPress itself treats tag names, and `run_export`'s
/// `resolve_or_create_term`, which is what would actually create a new
/// one on publish), in their original order - so a near-duplicate (e.g.
/// "KI" typed against an existing "Ki") is caught here instead of only
/// discovered as an unwanted new tag after publishing. Depends on
/// `known_tags` (`termcache`) already being populated - before its first
/// successful refresh, every typed tag shows as "new" even if it already
/// exists remotely.
fn tags_status_entries(tags_text: &str, known_tags: &[String]) -> Vec<(String, bool)> {
    parse_list(tags_text).into_iter().map(|tag| { let exists = termcache::term_exists(&tag, known_tags); (tag, exists) }).collect()
}

/// Rebuilds `flow_box` as a row of small colored pills, one per tag
/// (`tags_status_entries`) - green ("tag-pill-existing") for a tag that
/// already exists as a WordPress tag, red ("tag-pill-new") for one that
/// would create a brand new one on publish (the `.tag-pill*` classes are
/// declared once in `main.rs`'s `load_chat_bubble_css`, using libadwaita's
/// named `success`/`error` colors so they still adapt to light/dark mode).
/// A colored badge reads at a glance far better than the plain small
/// caption text this used to be. `row` (the `Adw.PreferencesRow` wrapping
/// `flow_box` - see its construction in `open`) is hidden entirely once
/// nothing's typed, not just `flow_box` itself - otherwise an empty row
/// would still show in the boxed list (a stray blank slot, or an
/// orphaned separator line above the Kategorien row below it).
fn refresh_tags_status(row: &adw::PreferencesRow, flow_box: &gtk4::FlowBox, tags_text: &str, known_tags: &[String]) {
    while let Some(child) = flow_box.first_child() {
        flow_box.remove(&child);
    }
    let entries = tags_status_entries(tags_text, known_tags);
    row.set_visible(!entries.is_empty());
    for (tag, exists) in entries {
        let pill = gtk4::Label::new(Some(&tag));
        pill.add_css_class("tag-pill");
        pill.add_css_class("caption");
        pill.add_css_class(if exists { "tag-pill-existing" } else { "tag-pill-new" });
        flow_box.append(&pill);
    }
}

/// Wraps `group` in the same margin/clamp/scroller shell every tab of this
/// dialog uses - a `ScrolledWindow` so an unusually tall tab (a long list
/// of autocomplete suggestions, a narrow window) still degrades to
/// scrolling instead of clipping, even though the whole point of splitting
/// into tabs is that this normally isn't needed.
fn tab_page(group: &adw::PreferencesGroup) -> gtk4::Widget {
    let content = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    content.append(group);
    // Matches `dialog`'s own `content_width` below - widened together with
    // it from the original 480 once five tabs' full labels ("Veröffentlichung",
    // "Kategorien & Tags", ...) no longer fit the switcher at that width and
    // started truncating with an ellipsis (a GNOME HIG violation on its
    // own: a tab whose label you can't actually read).
    let clamp = adw::Clamp::builder().maximum_size(600).child(&content).build();
    gtk4::ScrolledWindow::builder().child(&clamp).vexpand(true).build().upcast()
}

/// A genuine multi-line text box for a field that regularly holds a full
/// sentence rather than a couple of words ("Auszug / Meta-Beschreibung",
/// "SEO-Beschreibung") - `Adw.EntryRow` is strictly single-line and just
/// scrolls sideways past its own width, which a wider dialog only ever
/// partially fixes; wrapping text is the actual fix. `gtk4::TextView` in a
/// `gtk4::Frame` (the same "real textbox" look `aialt.rs`'s generated-alt-
/// text editor already uses), wrapped in an `Adw.PreferencesRow` - like
/// `refresh_tags_status`'s pill row - so it still renders as a proper
/// boxed-list row (matching padding/rounded card) instead of the
/// disconnected footer-content look a plain widget gets from
/// `Adw.PreferencesGroup::add`. Its own small caption label stands in for
/// `Adw.EntryRow`'s floating title, since `Adw.PreferencesRow` has no
/// title chrome of its own.
///
/// `vexpand(true)` on the row/box/scroller all the way down: without it,
/// splitting "Veröffentlichung" out of "Allgemein" (see `open`'s own
/// comment on that) just left the freed vertical space as dead white
/// space below a fixed-height `min_content_height(72)` box instead of
/// actually using it - the caller also needs to set `vexpand(true)` on
/// the `Adw.PreferencesGroup` this row is added to, or a plain
/// `Gtk.Box`/`Gtk.ListBox` parent won't hand this row the extra room to
/// begin with. `min_content_height` stays as a floor (~3 lines), not a
/// fixed size, for a tab where nothing else pushes this row that tall.
fn build_textarea_row(title: &str, initial_text: &str) -> (adw::PreferencesRow, gtk4::TextView) {
    let text_view = gtk4::TextView::builder().wrap_mode(gtk4::WrapMode::WordChar).top_margin(6).bottom_margin(6).left_margin(6).right_margin(6).build();
    text_view.buffer().set_text(initial_text);

    let frame = gtk4::Frame::new(None);
    frame.set_child(Some(&text_view));
    let scroller = gtk4::ScrolledWindow::builder().child(&frame).min_content_height(72).vexpand(true).build();

    let title_label = gtk4::Label::builder().label(title).xalign(0.0).build();
    title_label.add_css_class("caption");
    title_label.add_css_class("dim-label");

    let content = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(6)
        .margin_start(12)
        .margin_end(12)
        .margin_top(8)
        .margin_bottom(8)
        .vexpand(true)
        .build();
    content.append(&title_label);
    content.append(&scroller);

    let row = adw::PreferencesRow::new();
    row.set_vexpand(true);
    row.set_activatable(false);
    row.set_selectable(false);
    row.set_child(Some(&content));
    (row, text_view)
}

pub fn open(
    parent: &adw::ApplicationWindow,
    body: String,
    frontmatter: Rc<RefCell<Frontmatter>>,
    term_caches: termcache::TermCacheHandles,
    doc_dir: Option<PathBuf>,
    preview_pane: Rc<preview::PreviewPane>,
) {
    let termcache::TermCacheHandles { categories: category_terms, tags: tag_terms, category_slugs } = term_caches;
    let site = wpsite::load();
    let current = frontmatter.borrow().clone();

    // "Artikel" or "Seite" - which REST collection (`/posts` vs `/pages`)
    // export targets. Locked once the document is linked to an existing
    // WordPress item (`wp_post_id` set): WordPress can't convert a post
    // into a page in place, so switching then would just make the next
    // export try to update a nonexistent page with that post's id.
    let type_labels: Vec<String> = PostType::ALL.iter().map(|t| t.label()).collect();
    let type_label_refs: Vec<&str> = type_labels.iter().map(String::as_str).collect();
    let type_row = adw::ComboRow::builder()
        .title(tr("Typ"))
        .model(&gtk4::StringList::new(&type_label_refs))
        .selected(PostType::ALL.iter().position(|t| *t == current.post_type).unwrap_or(0) as u32)
        .sensitive(current.wp_post_id.is_none())
        .build();
    if current.wp_post_id.is_some() {
        type_row.set_subtitle(&tr("Bereits mit WordPress verknüpft - nicht mehr änderbar"));
    }

    // WordPress's hierarchical-pages feature (e.g. "Impressum" nested under
    // "Über uns") - posts have no such concept, so this row only ever shows
    // for `PostType::Page` (see `type_row`'s own `connect_selected_notify`
    // below for how it toggles live as the Typ picker changes). Same
    // "insensitive placeholder until list_items() resolves, populating flag
    // guards the notify handler" shape as `author_row` above it.
    let parent_options: Rc<RefCell<Vec<wpclient::PostSummary>>> = Rc::new(RefCell::new(Vec::new()));
    let parent_populating = Rc::new(Cell::new(true));
    let initial_parent_label = current.parent_name.clone().unwrap_or_else(|| tr("Wird geladen …"));
    let parent_row = adw::ComboRow::builder()
        .title(tr("Übergeordnete Seite"))
        .model(&gtk4::StringList::new(&[tr("Keine (oberste Ebene)").as_str(), initial_parent_label.as_str()]))
        .selected(if current.parent_id.is_some() { 1 } else { 0 })
        .sensitive(false)
        .visible(current.post_type == PostType::Page)
        .build();
    {
        let site = site.clone();
        let parent_row = parent_row.clone();
        let parent_options = parent_options.clone();
        let parent_populating = parent_populating.clone();
        let current_parent_id = current.parent_id;
        let current_parent_name = current.parent_name.clone();
        let current_wp_post_id = current.wp_post_id;
        let (tx, rx) = mpsc::channel::<Result<Vec<wpclient::PostSummary>, String>>();
        std::thread::spawn(move || {
            let outcome = if site.url.is_empty() {
                Err(tr("Keine WordPress-Verbindung eingerichtet."))
            } else {
                futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
                    .map_err(|err| err.to_string())
                    .and_then(|maybe_password| maybe_password.ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden.")))
                    .and_then(|password| wpclient::Client::new(&site.url, &site.username, &password).list_items("pages").map_err(|err| err.to_string()))
            };
            // A page can't be its own parent - excluded here rather than
            // left in the list for the user to (harmlessly, WordPress would
            // just reject it) pick by mistake.
            let outcome = outcome.map(|pages: Vec<wpclient::PostSummary>| pages.into_iter().filter(|p| Some(p.id) != current_wp_post_id).collect());
            let _ = tx.send(outcome);
        });
        glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
            Ok(Ok(mut pages)) => {
                // Same "keep a since-deleted/inaccessible existing value
                // visible via its cached name" reasoning as `author_row`.
                let known = current_parent_id.is_some_and(|id| pages.iter().any(|p| p.id == id));
                if let (false, Some(id)) = (known, current_parent_id) {
                    pages.push(wpclient::PostSummary { id, title: current_parent_name.clone().unwrap_or_else(|| id.to_string()), status: String::new(), date: String::new(), link: String::new() });
                }
                let mut labels = vec![tr("Keine (oberste Ebene)")];
                labels.extend(pages.iter().map(|p| p.title.clone()));
                let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                let selected = current_parent_id
                    .and_then(|id| pages.iter().position(|p| p.id == id))
                    .map(|pos| pos as u32 + 1)
                    .unwrap_or(0);
                *parent_options.borrow_mut() = pages;
                parent_row.set_model(Some(&gtk4::StringList::new(&label_refs)));
                parent_row.set_selected(selected);
                parent_row.set_sensitive(true);
                parent_populating.set(false);
                glib::ControlFlow::Break
            }
            Ok(Err(err)) => {
                parent_row.set_model(Some(&gtk4::StringList::new(&[&tr("Keine (oberste Ebene)"), &tr("Fehler beim Laden: {err}").replace("{err}", &err)])));
                parent_row.set_selected(0);
                parent_populating.set(false);
                glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                parent_row.set_model(Some(&gtk4::StringList::new(&[&tr("Keine (oberste Ebene)"), &tr("Interner Fehler: kein Ergebnis vom Ladevorgang.")])));
                parent_row.set_selected(0);
                parent_populating.set(false);
                glib::ControlFlow::Break
            }
        });
    }
    {
        let frontmatter = frontmatter.clone();
        let parent_options = parent_options.clone();
        let parent_populating = parent_populating.clone();
        parent_row.connect_selected_notify(move |row| {
            if parent_populating.get() {
                return;
            }
            let mut fm = frontmatter.borrow_mut();
            match row.selected().checked_sub(1).and_then(|index| parent_options.borrow().get(index as usize).cloned()) {
                Some(page) => {
                    fm.parent_id = Some(page.id);
                    fm.parent_name = Some(page.title);
                }
                None => {
                    fm.parent_id = None;
                    fm.parent_name = None;
                }
            }
        });
    }

    let title_row = adw::EntryRow::builder().title(tr("Titel")).text(current.title.as_str()).build();
    let slug_row = adw::EntryRow::builder().title(tr("Slug")).text(current.slug.as_str()).build();
    let slug_generate_button = gtk4::Button::from_icon_name("view-refresh-symbolic");
    slug_generate_button.set_tooltip_text(Some(&tr("Slug aus Titel generieren")));
    slug_generate_button.set_valign(gtk4::Align::Center);
    slug_generate_button.add_css_class("flat");
    slug_row.add_suffix(&slug_generate_button);

    // SEO recommendation (see `RECOMMENDED_MAX_URL_LENGTH`): the *whole*
    // published URL, not just the slug - domain + category + slug, since
    // that's what actually shows up truncated in search results. Needs the
    // category's real WordPress slug (not a guess from its name - the two
    // can differ, e.g. a category renamed without updating its slug), so
    // this reads `category_slugs` rather than deriving it locally.
    let url_length_row = adw::ActionRow::builder().title(tr("URL-Länge (SEO)")).build();
    let url_length_icon = gtk4::Image::new();
    url_length_icon.set_valign(gtk4::Align::Center);
    url_length_row.add_suffix(&url_length_icon);

    let (excerpt_row, excerpt_view) = build_textarea_row(&tr("Auszug / Meta-Beschreibung"), &current.excerpt.clone().unwrap_or_default());
    let seo_title_row = adw::EntryRow::builder()
        .title(tr("SEO-Titel"))
        .text(current.rank_math_title.clone().unwrap_or_default().as_str())
        .build();
    let (seo_description_row, seo_description_view) = build_textarea_row(&tr("SEO-Beschreibung"), &current.rank_math_description.clone().unwrap_or_default());
    let focus_keyword_row = adw::EntryRow::builder()
        .title(tr("Fokus-Keyword"))
        .text(current.rank_math_focus_keyword.clone().unwrap_or_default().as_str())
        .build();
    let categories_row = adw::EntryRow::builder()
        .title(tr("Kategorien (Komma-getrennt)"))
        .text(current.categories.join(", ").as_str())
        .build();
    let tags_row = adw::EntryRow::builder()
        .title(tr("Tags (Komma-getrennt)"))
        .text(current.tags.join(", ").as_str())
        .build();
    // Analyzes title/body against the site's already-known tags
    // (`tag_terms`) and proposes a handful to add - see `tagsuggest.rs`.
    let tags_suggest_button = gtk4::Button::from_icon_name("chat-message-new-symbolic");
    tags_suggest_button.set_tooltip_text(Some(&tr("KI-Tags vorschlagen…")));
    tags_suggest_button.set_valign(gtk4::Align::Center);
    tags_suggest_button.add_css_class("flat");
    tags_row.add_suffix(&tags_suggest_button);
    // Shows which of the currently-typed tags already exist as a WordPress
    // tag (`tag_terms`, from `termcache`) vs which would create a brand
    // new one on publish (`run_export`'s `resolve_or_create_term`) - so a
    // near-duplicate ("KI" vs "Ki") is caught here instead of only
    // discovered as an unwanted new tag after publishing. A wrapping row
    // of small colored pills (see `refresh_tags_status`).
    //
    // Wrapped in a real `Adw.PreferencesRow` rather than adding the
    // `FlowBox` to `taxonomy_group` directly - a plain `gtk4::Widget`
    // added to a `PreferencesGroup` (the same pattern `connection.rs`'s
    // `status_label` uses) renders *outside* the boxed-list card entirely,
    // as separate footer content below it, with none of the list's own
    // padding - fine for a one-line hint, but visually disconnected for
    // something that reads as belonging to the Tags row right above it.
    // `Adw.PreferencesRow` (the plain base row type - `Adw.ActionRow`/
    // `Adw.EntryRow` etc. all build on it) instead makes this a genuine
    // list row: same rounded/bordered card, same row separator, and its
    // `child` gets the FlowBox's own margins below to match every other
    // row's internal padding. Not selectable/activatable - it's static
    // content, not something to click.
    let tags_status_flow = gtk4::FlowBox::builder()
        .halign(gtk4::Align::Start)
        .selection_mode(gtk4::SelectionMode::None)
        .row_spacing(6)
        .column_spacing(6)
        .margin_start(12)
        .margin_end(12)
        .margin_top(8)
        .margin_bottom(8)
        .build();
    let tags_status_row = adw::PreferencesRow::new();
    tags_status_row.set_activatable(false);
    tags_status_row.set_selectable(false);
    tags_status_row.set_child(Some(&tags_status_flow));
    let featured_image_row = adw::EntryRow::builder()
        .title(tr("Featured Image (Pfad)"))
        .text(current.featured_image.clone().unwrap_or_default().as_str())
        .build();
    let featured_image_picker_button = gtk4::Button::from_icon_name("insert-image-symbolic");
    featured_image_picker_button.set_tooltip_text(Some(&tr("Bild auswählen…")));
    featured_image_picker_button.set_valign(gtk4::Align::Center);
    featured_image_picker_button.add_css_class("flat");
    featured_image_row.add_suffix(&featured_image_picker_button);
    // Unlike a body image's alt text (`MediaItem.alt`, scanned from the
    // Markdown), the featured image is a single `Frontmatter` field with
    // nothing to scan it from - so it needs its own plain entry here
    // rather than piggybacking on Medienverwaltung's per-image rows. Sent
    // as the resulting WordPress attachment's `alt_text` on upload
    // (`mediapanel.rs`), same as any other image.
    let featured_image_alt_row = adw::EntryRow::builder()
        .title(tr("Alt-Text für Aufmacherbild"))
        .text(current.featured_image_alt.clone().unwrap_or_default().as_str())
        .build();
    // Same "KI-Alternativtext generieren…" flow the editor/preview context
    // menus already offer for a body image (`aialt::open`) - only useful
    // once there's actually a local file to send to the vision model, so
    // hidden whenever `featured_image` is unset (already uploaded, or
    // never set), same condition `featured_image_picker_button`'s own
    // upload-button sibling in `mediapanel.rs` uses.
    let featured_image_ai_button = gtk4::Button::from_icon_name("chat-message-new-symbolic");
    featured_image_ai_button.set_tooltip_text(Some(&tr("KI-Alternativtext generieren…")));
    featured_image_ai_button.set_valign(gtk4::Align::Center);
    featured_image_ai_button.add_css_class("flat");
    featured_image_ai_button.set_visible(current.featured_image.is_some());
    featured_image_alt_row.add_suffix(&featured_image_ai_button);
    // Non-blocking hint for an unusually long alt text (see
    // `media::alt_text_length_warning`) - a suffix icon with a tooltip,
    // matching the same warning on the per-image rows in
    // `mediapanel.rs`/`imagealt.rs`.
    let featured_image_alt_length_warning_icon = gtk4::Image::from_icon_name("dialog-warning-symbolic");
    featured_image_alt_length_warning_icon.add_css_class("warning");
    featured_image_alt_length_warning_icon.set_visible(false);
    featured_image_alt_row.add_suffix(&featured_image_alt_length_warning_icon);
    update_alt_length_warning(&featured_image_alt_length_warning_icon, &featured_image_alt_row.text());

    let status_labels: Vec<String> = PostStatus::ALL.iter().map(|s| s.label()).collect();
    let status_label_refs: Vec<&str> = status_labels.iter().map(String::as_str).collect();
    let status_model = gtk4::StringList::new(&status_label_refs);
    let selected_index = PostStatus::ALL.iter().position(|s| *s == current.status).unwrap_or(0);
    let status_row = adw::ComboRow::builder()
        .title(tr("Status"))
        .model(&status_model)
        .selected(selected_index as u32)
        .build();

    let scheduled_row = adw::EntryRow::builder()
        .title(tr("Veröffentlichungstermin (JJJJ-MM-TT HH:MM)"))
        .text(current.scheduled_at.as_deref().map(document::format_scheduled_at_for_display).unwrap_or_default().as_str())
        .build();
    scheduled_row.set_visible(current.status == PostStatus::Future);

    // "Nicht ändern" (index 0) leaves `author_id` unset, so the post keeps
    // whichever author it already has on export (see `export.rs`'s own
    // comment on why an unset `author_id` is never sent at all) - the
    // options after it are populated once `list_users()` resolves, below.
    // Starts insensitive with just the cached name (if any) or a loading
    // placeholder, since the real list isn't known yet; `author_populating`
    // guards `connect_selected_notify` from firing (and clobbering
    // `frontmatter.author_id`) while this initial, not-yet-real model is
    // still showing.
    let author_options: Rc<RefCell<Vec<wpclient::WpUser>>> = Rc::new(RefCell::new(Vec::new()));
    let author_populating = Rc::new(Cell::new(true));
    let initial_author_label = current.author_name.clone().unwrap_or_else(|| tr("Wird geladen …"));
    let author_row = adw::ComboRow::builder()
        .title(tr("Autor"))
        .model(&gtk4::StringList::new(&[tr("Nicht ändern").as_str(), initial_author_label.as_str()]))
        .selected(if current.author_id.is_some() { 1 } else { 0 })
        .sensitive(false)
        .build();

    {
        let site = site.clone();
        let author_row = author_row.clone();
        let author_options = author_options.clone();
        let author_populating = author_populating.clone();
        let current_author_id = current.author_id;
        let current_author_name = current.author_name.clone();
        let (tx, rx) = mpsc::channel::<Result<Vec<wpclient::WpUser>, String>>();
        std::thread::spawn(move || {
            let outcome = if site.url.is_empty() {
                Err(tr("Keine WordPress-Verbindung eingerichtet."))
            } else {
                futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
                    .map_err(|err| err.to_string())
                    .and_then(|maybe_password| maybe_password.ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden.")))
                    .and_then(|password| wpclient::Client::new(&site.url, &site.username, &password).list_users().map_err(|err| err.to_string()))
            };
            let _ = tx.send(outcome);
        });
        glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
            Ok(Ok(mut users)) => {
                // The currently-set author might not be among `users` (a
                // deleted account, or one the API's `context=edit` still
                // can't see for permission reasons) - appended as a
                // synthetic extra entry from the cached name rather than
                // silently dropped, so the picker never shows a name-less
                // "Autor" for a document that had one.
                let known = current_author_id.is_some_and(|id| users.iter().any(|u| u.id == id));
                if let (false, Some(id)) = (known, current_author_id) {
                    users.push(wpclient::WpUser { id, name: current_author_name.clone().unwrap_or_else(|| id.to_string()) });
                }
                let mut labels = vec![tr("Nicht ändern")];
                labels.extend(users.iter().map(|u| u.name.clone()));
                let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                let selected = current_author_id
                    .and_then(|id| users.iter().position(|u| u.id == id))
                    .map(|pos| pos as u32 + 1)
                    .unwrap_or(0);
                *author_options.borrow_mut() = users;
                author_row.set_model(Some(&gtk4::StringList::new(&label_refs)));
                author_row.set_selected(selected);
                author_row.set_sensitive(true);
                author_populating.set(false);
                glib::ControlFlow::Break
            }
            Ok(Err(err)) => {
                author_row.set_model(Some(&gtk4::StringList::new(&[&tr("Nicht ändern"), &tr("Fehler beim Laden: {err}").replace("{err}", &err)])));
                author_row.set_selected(0);
                author_populating.set(false);
                glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                author_row.set_model(Some(&gtk4::StringList::new(&[&tr("Nicht ändern"), &tr("Interner Fehler: kein Ergebnis vom Ladevorgang.")])));
                author_row.set_selected(0);
                author_populating.set(false);
                glib::ControlFlow::Break
            }
        });
    }
    {
        let frontmatter = frontmatter.clone();
        let author_options = author_options.clone();
        let author_populating = author_populating.clone();
        author_row.connect_selected_notify(move |row| {
            if author_populating.get() {
                return;
            }
            let mut fm = frontmatter.borrow_mut();
            match row.selected().checked_sub(1).and_then(|index| author_options.borrow().get(index as usize).cloned()) {
                Some(user) => {
                    fm.author_id = Some(user.id);
                    fm.author_name = Some(user.name);
                }
                None => {
                    fm.author_id = None;
                    fm.author_name = None;
                }
            }
        });
    }

    // Sent unconditionally on export as the "Worthy" WordPress plugin's own
    // `wp-worthy-pixel.ignored` REST field (see `export.rs`) - harmless on
    // a site without that plugin. Active (the default) means "tracked as
    // normal"; see the Statistik tab for how close the article already is
    // to Worthy's own minimum length before a counting pixel is even
    // eligible to report at all.
    let vgwort_row = adw::SwitchRow::builder()
        .title(tr("VG-Wort-Zählmarke"))
        .subtitle(tr("Aus lassen, um diesen Artikel bewusst von der VG-Wort-Zählung auszuschließen."))
        .active(!current.vgwort_ignored)
        .build();
    {
        let frontmatter = frontmatter.clone();
        vgwort_row.connect_active_notify(move |row| {
            frontmatter.borrow_mut().vgwort_ignored = !row.is_active();
        });
    }

    // "Nicht ändern" (index 0) leaves `comment_status` unset, so the post
    // keeps whatever comment status it already has on export (see
    // `export.rs`'s own "only sent when set" comment) - no fetch needed
    // here, unlike the Autor picker above, since these three options are
    // fixed rather than coming from the site.
    let comment_status_row = adw::ComboRow::builder()
        .title(tr("Kommentare"))
        .model(&gtk4::StringList::new(&[&tr("Nicht ändern"), &tr("Offen"), &tr("Geschlossen")]))
        .selected(match current.comment_status {
            None => 0,
            Some(true) => 1,
            Some(false) => 2,
        })
        .build();
    {
        let frontmatter = frontmatter.clone();
        comment_status_row.connect_selected_notify(move |row| {
            frontmatter.borrow_mut().comment_status = match row.selected() {
                1 => Some(true),
                2 => Some(false),
                _ => None,
            };
        });
    }

    let refresh_button = gtk4::Button::from_icon_name("view-refresh-symbolic");
    refresh_button.set_tooltip_text(Some(&tr("Kategorien & Tags von WordPress aktualisieren")));
    refresh_button.add_css_class("flat");
    let term_caches = termcache::TermCacheHandles {
        categories: category_terms.clone(),
        tags: tag_terms.clone(),
        category_slugs: category_slugs.clone(),
    };
    {
        let term_caches = term_caches.clone();
        refresh_button.connect_clicked(move |_| {
            termcache::spawn_refresh(&term_caches);
        });
    }

    let manage_terms_button = gtk4::Button::from_icon_name("document-edit-symbolic");
    manage_terms_button.set_tooltip_text(Some(&tr("Kategorien & Tags verwalten…")));
    manage_terms_button.add_css_class("flat");
    {
        let parent = parent.clone();
        let term_caches = term_caches.clone();
        manage_terms_button.connect_clicked(move |_| {
            taxonomy::open(&parent, term_caches.clone());
        });
    }

    // Split into five tabs, the same `Adw.InlineViewSwitcher` pattern
    // `export.rs`'s own dialog already uses (Vorschau/Medien/Links) - this
    // dialog grew to 16 fields across incremental additions (Autor,
    // VG-Wort, Kommentare, ...), which meant one long scrolling list even
    // though most edits only ever touch a handful of fields at once.
    // Grouped by how often they're actually used together: article content
    // (title/slug/excerpt) and publishing workflow (status/schedule/
    // author/comments/VG-Wort) are each their own tab rather than one
    // combined "Allgemein" - category/tag management, the featured image,
    // and RankMath SEO - each meaningfully optional or used in its own
    // separate moment - get their own tab too, instead of permanently
    // taking up scroll space.
    // "Allgemein" now holds only what's actually written for this
    // specific article - title/slug/excerpt. Status, scheduling, author,
    // comments and VG-Wort are workflow/publishing settings, not article
    // content, and used to all sit crammed into this one tab together -
    // fine as single-line rows, but once "Auszug" became a real multi-
    // line textarea (see `build_textarea_row`), that no longer fit this
    // dialog's height without scrolling. Split into its own
    // "Veröffentlichung" tab below instead of just enlarging the dialog
    // again.
    // `vexpand(true)` so the group (and, through it, `excerpt_row`'s own
    // `vexpand(true)` - see `build_textarea_row`) actually gets handed the
    // tab page's leftover vertical space instead of leaving it as dead
    // white space below a short, fixed-height boxed list.
    let general_group = adw::PreferencesGroup::builder().title(tr("Allgemein")).vexpand(true).build();
    general_group.add(&type_row);
    general_group.add(&parent_row);
    general_group.add(&title_row);
    general_group.add(&slug_row);
    general_group.add(&excerpt_row);

    let publishing_group = adw::PreferencesGroup::builder().title(tr("Veröffentlichung")).build();
    publishing_group.add(&status_row);
    publishing_group.add(&scheduled_row);
    publishing_group.add(&author_row);
    publishing_group.add(&comment_status_row);
    publishing_group.add(&vgwort_row);

    let taxonomy_header_suffix = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    taxonomy_header_suffix.append(&manage_terms_button);
    taxonomy_header_suffix.append(&refresh_button);
    let taxonomy_group = adw::PreferencesGroup::builder().title(tr("Kategorien & Tags")).build();
    taxonomy_group.set_header_suffix(Some(&taxonomy_header_suffix));
    taxonomy_group.add(&categories_row);
    taxonomy_group.add(&tags_row);
    taxonomy_group.add(&tags_status_row);

    autocomplete::attach(&categories_row, category_terms);
    autocomplete::attach(&tags_row, tag_terms.clone());
    refresh_tags_status(&tags_status_row, &tags_status_flow, &tags_row.text(), &tag_terms.borrow());

    let image_group = adw::PreferencesGroup::builder().title(tr("Aufmacherbild")).build();
    image_group.add(&featured_image_row);
    image_group.add(&featured_image_alt_row);

    // Same "only matters on a site with RankMath active" reasoning as
    // before for keeping these visually/organizationally distinct - now
    // additionally covers the URL-length hint, which is SEO-flavored
    // regardless of RankMath specifically.
    // Same `vexpand(true)` reasoning as `general_group` above, for
    // `seo_description_row`.
    let seo_group = adw::PreferencesGroup::builder().title(tr("RankMath SEO")).vexpand(true).build();
    seo_group.add(&url_length_row);
    seo_group.add(&seo_title_row);
    seo_group.add(&seo_description_row);
    seo_group.add(&focus_keyword_row);

    let view_stack = adw::ViewStack::new();
    view_stack.add_titled_with_icon(&tab_page(&general_group), Some("general"), &tr("Allgemein"), "document-properties-symbolic");
    view_stack.add_titled_with_icon(&tab_page(&publishing_group), Some("publishing"), &tr("Veröffentlichung"), "send-symbolic");
    let taxonomy_page = view_stack.add_titled_with_icon(&tab_page(&taxonomy_group), Some("taxonomy"), &tr("Kategorien & Tags"), "tag-symbolic");
    // Pages have no categories/tags (see `PostType`), so the whole tab is
    // hidden for one rather than offering fields export would ignore.
    taxonomy_page.set_visible(current.post_type == PostType::Post);
    view_stack.add_titled_with_icon(&tab_page(&image_group), Some("image"), &tr("Bild"), "image-x-generic-symbolic");
    view_stack.add_titled_with_icon(&tab_page(&seo_group), Some("seo"), &tr("SEO"), "edit-find-symbolic");
    view_stack.set_vexpand(true);

    // A plain `Gtk.Box` row below the header bar, not `header`'s own
    // title widget - the same reasoning `window.rs`'s own
    // `Adw.InlineViewSwitcher` already follows: it's a seamless linked
    // pill, not the loose per-tab buttons `Adw.HeaderBar` centers via
    // `Adw.ViewSwitcher`, so nesting it as the header's title widget left
    // it flush against the header's own start edge instead of properly
    // centered - it needs its own row to center itself in. `header` is
    // left with no custom title widget, so it shows the dialog's own
    // `.title()` centered instead, same as `export.rs`'s wizard dialog.
    let view_switcher = adw::InlineViewSwitcher::builder().stack(&view_stack).build();
    let switcher_bar = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .halign(gtk4::Align::Center)
        .margin_top(6)
        .margin_bottom(6)
        .build();
    switcher_bar.append(&view_switcher);

    // No extra `Gtk.Separator` below `switcher_bar` - `Adw.ToolbarView`
    // already draws its own border under the top-bar stack once the
    // content below can scroll, so adding one here just doubled it up
    // into two thin lines stacked right on top of each other.
    let header = adw::HeaderBar::new();
    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.add_top_bar(&switcher_bar);
    toolbar_view.set_content(Some(&view_stack));

    // 600, not the original 480 - see `tab_page`'s own comment on why.
    let dialog = adw::Dialog::builder()
        .title(tr("Artikel-Eigenschaften"))
        .content_width(600)
        .content_height(560)
        .child(&toolbar_view)
        .build();

    {
        let frontmatter = frontmatter.clone();
        title_row.connect_changed(move |row| {
            frontmatter.borrow_mut().title = row.text().to_string();
        });
    }
    {
        let frontmatter = frontmatter.clone();
        let url_length_row = url_length_row.clone();
        let url_length_icon = url_length_icon.clone();
        let domain = site.url.clone();
        let category_slugs = category_slugs.clone();
        slug_row.connect_changed(move |row| {
            frontmatter.borrow_mut().slug = row.text().to_string();
            refresh_url_length_row(&url_length_row, &url_length_icon, &domain, &frontmatter, &category_slugs);
        });
    }
    {
        let title_row = title_row.clone();
        let slug_row = slug_row.clone();
        slug_generate_button.connect_clicked(move |_| {
            // Triggers the `connect_changed` handler above, which persists it.
            slug_row.set_text(&document::slugify(&title_row.text()));
        });
    }
    {
        let frontmatter = frontmatter.clone();
        excerpt_view.buffer().connect_changed(move |buffer| {
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
            frontmatter.borrow_mut().excerpt = (!text.is_empty()).then_some(text);
        });
    }
    {
        let frontmatter = frontmatter.clone();
        seo_title_row.connect_changed(move |row| {
            let text = row.text().to_string();
            frontmatter.borrow_mut().rank_math_title = (!text.is_empty()).then_some(text);
        });
    }
    {
        let frontmatter = frontmatter.clone();
        seo_description_view.buffer().connect_changed(move |buffer| {
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
            frontmatter.borrow_mut().rank_math_description = (!text.is_empty()).then_some(text);
        });
    }
    {
        let frontmatter = frontmatter.clone();
        focus_keyword_row.connect_changed(move |row| {
            let text = row.text().to_string();
            frontmatter.borrow_mut().rank_math_focus_keyword = (!text.is_empty()).then_some(text);
        });
    }
    {
        let frontmatter = frontmatter.clone();
        let url_length_row = url_length_row.clone();
        let url_length_icon = url_length_icon.clone();
        let domain = site.url.clone();
        let category_slugs = category_slugs.clone();
        categories_row.connect_changed(move |row| {
            frontmatter.borrow_mut().categories = parse_list(&row.text());
            refresh_url_length_row(&url_length_row, &url_length_icon, &domain, &frontmatter, &category_slugs);
        });
    }
    {
        let frontmatter = frontmatter.clone();
        let tags_status_row = tags_status_row.clone();
        let tags_status_flow = tags_status_flow.clone();
        let tag_terms = tag_terms.clone();
        tags_row.connect_changed(move |row| {
            let text = row.text();
            frontmatter.borrow_mut().tags = parse_list(&text);
            refresh_tags_status(&tags_status_row, &tags_status_flow, &text, &tag_terms.borrow());
        });
    }
    {
        let parent = parent.clone();
        let body = body.clone();
        let title_row = title_row.clone();
        let tags_row = tags_row.clone();
        let tag_terms = tag_terms.clone();
        tags_suggest_button.connect_clicked(move |_| {
            let article_title = title_row.text().to_string();
            let current_tags = parse_list(&tags_row.text());
            let existing_tags = tag_terms.borrow().clone();
            let tags_row = tags_row.clone();
            tagsuggest::open(parent.upcast_ref::<gtk4::Window>(), article_title, body.clone(), existing_tags, current_tags, move |accepted| {
                let mut tags = parse_list(&tags_row.text());
                for tag in accepted {
                    if !tags.iter().any(|existing| existing.eq_ignore_ascii_case(&tag)) {
                        tags.push(tag);
                    }
                }
                // Triggers the `connect_changed` handler above, which persists it.
                tags_row.set_text(&tags.join(", "));
            });
        });
    }
    {
        let frontmatter = frontmatter.clone();
        let featured_image_ai_button = featured_image_ai_button.clone();
        featured_image_row.connect_changed(move |row| {
            let text = row.text().to_string();
            featured_image_ai_button.set_visible(!text.is_empty());
            frontmatter.borrow_mut().featured_image = (!text.is_empty()).then_some(text);
        });
    }
    {
        let frontmatter = frontmatter.clone();
        let featured_image_alt_length_warning_icon = featured_image_alt_length_warning_icon.clone();
        featured_image_alt_row.connect_changed(move |row| {
            let text = row.text().to_string();
            update_alt_length_warning(&featured_image_alt_length_warning_icon, &text);
            frontmatter.borrow_mut().featured_image_alt = (!text.is_empty()).then_some(text);
        });
    }
    {
        let parent = parent.clone();
        let frontmatter = frontmatter.clone();
        let doc_dir = doc_dir.clone();
        let featured_image_alt_row = featured_image_alt_row.clone();
        featured_image_ai_button.connect_clicked(move |_| {
            let Some(source) = frontmatter.borrow().featured_image.clone() else { return };
            let featured_image_alt_row = featured_image_alt_row.clone();
            aialt::open(parent.upcast_ref::<gtk4::Window>(), tr("Aufmacherbild"), source, doc_dir.clone(), move |text| {
                // Triggers the `connect_changed` handler above, which
                // persists it and refreshes the length-warning icon - same
                // "set text, let the existing handler do the rest" pattern
                // `featured_image_picker_button`'s own callback uses.
                featured_image_alt_row.set_text(&text);
            });
        });
    }
    {
        let parent = parent.clone();
        let featured_image_row = featured_image_row.clone();
        featured_image_picker_button.connect_clicked(move |_| {
            let filter = gtk4::FileFilter::new();
            filter.add_mime_type("image/*");
            filter.set_name(Some(&tr("Bilder")));
            let filters = gio::ListStore::new::<gtk4::FileFilter>();
            filters.append(&filter);

            let file_dialog = gtk4::FileDialog::builder().title(tr("Featured Image auswählen")).filters(&filters).build();

            let featured_image_row = featured_image_row.clone();
            let doc_dir = doc_dir.clone();
            file_dialog.open(Some(&parent), gio::Cancellable::NONE, move |result| {
                let Ok(file) = result else { return };
                let Some(path) = file.path() else { return };
                let reference = document::image_reference(&path, doc_dir.as_deref());
                // Triggers the `connect_changed` handler above, which persists it.
                featured_image_row.set_text(&reference);
            });
        });
    }
    {
        let frontmatter = frontmatter.clone();
        let parent_row = parent_row.clone();
        type_row.connect_selected_notify(move |row| {
            if let Some(post_type) = PostType::ALL.get(row.selected() as usize) {
                frontmatter.borrow_mut().post_type = *post_type;
                taxonomy_page.set_visible(*post_type == PostType::Post);
                parent_row.set_visible(*post_type == PostType::Page);
            }
        });
    }
    {
        let frontmatter = frontmatter.clone();
        let scheduled_row = scheduled_row.clone();
        status_row.connect_selected_notify(move |row| {
            if let Some(status) = PostStatus::ALL.get(row.selected() as usize) {
                frontmatter.borrow_mut().status = *status;
                scheduled_row.set_visible(*status == PostStatus::Future);
            }
        });
    }
    {
        let frontmatter = frontmatter.clone();
        scheduled_row.connect_changed(move |row| {
            let text = row.text().to_string();
            let parsed = document::parse_scheduled_at(&text);
            row.remove_css_class("error");
            if !text.trim().is_empty() && parsed.is_none() {
                row.add_css_class("error");
            }
            frontmatter.borrow_mut().scheduled_at = parsed;
        });
    }

    refresh_url_length_row(&url_length_row, &url_length_icon, &site.url, &frontmatter, &category_slugs);

    // The preview's magazine-style header shows exactly these fields, but
    // isn't refreshed live on every keystroke here (unlike this dialog's
    // own rows, which all write straight into the shared `Frontmatter`) -
    // deferred to close, matching `imagealt.rs`'s own "write back on
    // `connect_closed`, not per keystroke" reasoning for its dialog.
    dialog.connect_closed(move |_| {
        preview_pane.set_article_header(&frontmatter.borrow());
    });

    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seo_url_preview_joins_domain_category_and_slug() {
        let (url, length) = seo_url_preview("https://linuxundich.de", Some("gnu-linux"), "mein-testartikel");
        assert_eq!(url, "https://linuxundich.de/gnu-linux/mein-testartikel/");
        assert_eq!(length, url.chars().count());
    }

    #[test]
    fn seo_url_preview_omits_the_category_segment_when_none_is_set() {
        let (url, _) = seo_url_preview("https://linuxundich.de", None, "mein-testartikel");
        assert_eq!(url, "https://linuxundich.de/mein-testartikel/");
    }

    #[test]
    fn seo_url_preview_strips_a_trailing_slash_from_the_domain() {
        let (url, _) = seo_url_preview("https://linuxundich.de/", Some("news"), "post");
        assert_eq!(url, "https://linuxundich.de/news/post/");
    }

    #[test]
    fn tags_status_entries_is_empty_when_nothing_is_typed() {
        assert_eq!(tags_status_entries("", &["KI".to_string()]), vec![]);
        assert_eq!(tags_status_entries("   ", &["KI".to_string()]), vec![]);
    }

    #[test]
    fn tags_status_entries_flags_each_tag_as_existing_or_new() {
        let known = vec!["KI".to_string(), "Hardware".to_string()];
        let entries = tags_status_entries("KI, Terminal", &known);
        assert_eq!(entries, vec![("KI".to_string(), true), ("Terminal".to_string(), false)]);
    }

    #[test]
    fn tags_status_entries_matches_known_tags_case_insensitively() {
        let known = vec!["Ki".to_string()];
        assert_eq!(tags_status_entries("KI", &known), vec![("KI".to_string(), true)]);
    }
}
