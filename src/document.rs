//! Markdown file I/O with a small hand-rolled YAML-frontmatter dialect.
//!
//! A plain `.md` file with no frontmatter block round-trips as plain text.
//! Once metadata (title/slug/status/categories/tags/featured image/WP post
//! id) is set via the properties dialog, it's persisted as a `---`-delimited
//! block at the top of the file so re-opening and re-exporting the same
//! article can target the same WordPress post (`wp_post_id`).
//!
//! Deliberately not a general YAML parser: the schema is small and fixed,
//! so a tiny purpose-built parser avoids pulling in a dependency (the
//! obvious choices, `serde_yaml` and its forks, are unmaintained) for a
//! handful of scalar/list fields.

use std::path::{Path, PathBuf};

use sha2::Digest;

use crate::media::MediaItem;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PostStatus {
    #[default]
    Draft,
    Pending,
    Publish,
    /// Scheduled publishing - WordPress only actually treats a post this
    /// way if its `date` is really in the future (see `Frontmatter::
    /// scheduled_at`); given a past or missing date, WordPress silently
    /// publishes it immediately instead; `run_export` refuses to export
    /// with this status and no valid `scheduled_at` rather than let that
    /// surprise happen.
    Future,
    /// Published, but visible only to logged-in users with permission to
    /// read private posts (WordPress's own "Private" visibility) - unlike
    /// `Future`, this behaves like an immediate publish from the REST API's
    /// perspective, just with different visibility, so it needs no
    /// `scheduled_at`-style special-casing anywhere else.
    Private,
}

impl PostStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            PostStatus::Draft => "draft",
            PostStatus::Pending => "pending",
            PostStatus::Publish => "publish",
            PostStatus::Future => "future",
            PostStatus::Private => "private",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.trim() {
            "pending" => PostStatus::Pending,
            "publish" => PostStatus::Publish,
            "future" => PostStatus::Future,
            "private" => PostStatus::Private,
            _ => PostStatus::Draft,
        }
    }

    pub const ALL: [PostStatus; 5] = [PostStatus::Draft, PostStatus::Pending, PostStatus::Publish, PostStatus::Future, PostStatus::Private];

    /// Human-readable, translated label, for the properties dialog's
    /// dropdown. Each arm calls `tr()` directly on its own literal (rather
    /// than matching to a variable and translating once) so `xgettext`
    /// can actually find these strings - see `shortcuts.rs`'s `group()`
    /// for the same rule with a longer explanation.
    pub fn label(&self) -> String {
        match self {
            PostStatus::Draft => crate::i18n::tr("Entwurf"),
            PostStatus::Pending => crate::i18n::tr("Ausstehend"),
            PostStatus::Publish => crate::i18n::tr("Veröffentlicht"),
            PostStatus::Future => crate::i18n::tr("Geplant"),
            PostStatus::Private => crate::i18n::tr("Privat"),
        }
    }
}

/// Which WordPress content type an article is - a regular blog post
/// (`/wp/v2/posts`) or a static page (`/wp/v2/pages`, e.g. "Impressum",
/// "Über mich"). Pages share nearly every field with posts; the only
/// differences this app cares about are the REST endpoint and that pages
/// have no categories/tags (WordPress's `page` type doesn't register
/// either taxonomy, so sending them would just be dropped).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PostType {
    #[default]
    Post,
    Page,
}

impl PostType {
    pub fn as_str(&self) -> &'static str {
        match self {
            PostType::Post => "post",
            PostType::Page => "page",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.trim() {
            "page" => PostType::Page,
            _ => PostType::Post,
        }
    }

    /// The REST API collection this type lives under (`wp/v2/{rest_base}`).
    pub fn rest_base(&self) -> &'static str {
        match self {
            PostType::Post => "posts",
            PostType::Page => "pages",
        }
    }

    pub const ALL: [PostType; 2] = [PostType::Post, PostType::Page];

    /// Human-readable, translated label - same "each arm calls `tr()` on its
    /// own literal" rule as `PostStatus::label`.
    pub fn label(&self) -> String {
        match self {
            PostType::Post => crate::i18n::tr("Blogartikel"),
            PostType::Page => crate::i18n::tr("Seite"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Frontmatter {
    pub title: String,
    /// Post or page - `Post` (the default) is never written to the
    /// frontmatter, so every file written before this field existed keeps
    /// round-tripping byte-for-byte.
    pub post_type: PostType,
    pub slug: String,
    pub status: PostStatus,
    /// When `status` is `PostStatus::Future`: the publish date/time, as the
    /// normalized `"YYYY-MM-DDTHH:MM:00"` form `parse_scheduled_at`
    /// produces - sent as-is as the WordPress REST API's `date` field.
    pub scheduled_at: Option<String>,
    pub categories: Vec<String>,
    pub tags: Vec<String>,
    /// The post excerpt/meta description - sent as WordPress's own
    /// `excerpt` field, which is what RSS feeds, social share cards, and
    /// most themes' archive views actually show. Left unset, WordPress
    /// falls back to an auto-truncated (often mid-sentence) chunk of the
    /// body instead.
    pub excerpt: Option<String>,
    /// RankMath SEO plugin fields, sent as the WordPress post meta keys
    /// RankMath itself registers (`rank_math_title`/
    /// `rank_math_description`/`rank_math_focus_keyword`) - only
    /// meaningful on a site that actually has RankMath active. WordPress's
    /// REST API silently drops an unrecognized meta key rather than
    /// erroring, so sending these is always safe even against a site
    /// without RankMath.
    pub rank_math_title: Option<String>,
    pub rank_math_description: Option<String>,
    pub rank_math_focus_keyword: Option<String>,
    pub featured_image: Option<String>,
    /// Alt text for `featured_image` - sent as the resulting WordPress
    /// media attachment's `alt_text` on upload (`mediapanel.rs`), the same
    /// way a body image's alt text is. Independent of any body `MediaItem`
    /// even if the same file happens to also appear in the article body,
    /// since the featured image is a single `Frontmatter` field, not a
    /// `MediaItem` scanned from the body.
    pub featured_image_alt: Option<String>,
    /// Set once the document has been published/updated via the WordPress
    /// REST API (see M5), so re-exporting updates the same post.
    pub wp_post_id: Option<u64>,
    /// SHA-256 hex digest of the Gutenberg HTML content last known to
    /// actually match the WordPress server - set from the literal fetched
    /// content on import (`importer.rs`) and from the literal just-sent
    /// content after every successful publish/update (`export.rs`), so a
    /// re-fetch's hash can be compared against it right before the *next*
    /// update. A mismatch means the post changed on the server since -
    /// someone editing it directly in wp-admin, most likely - and
    /// `export.rs` warns before overwriting rather than silently
    /// clobbering that change. `None` for a document never yet synced with
    /// a server copy (a brand new article, or one opened from a `.md` file
    /// written before this field existed) - the check has no baseline to
    /// compare against then, so it's silently skipped rather than treated
    /// as a conflict.
    pub wp_content_hash: Option<String>,
    /// The WordPress media id of the post's *current* featured image, when
    /// the document was opened from an existing post (`importer.rs`) - not
    /// user-editable. `featured_image` is a local path to *upload as a new*
    /// featured image; this carries the existing remote one through
    /// unchanged on re-export until the user actually sets `featured_image`.
    pub featured_media_id: Option<u64>,
    /// The WordPress user id to attribute the post to on export - `None`
    /// leaves it unsent, so the post keeps whichever author it already has
    /// (a new post: whoever the Application Password belongs to; an
    /// existing one: whatever it already had), the same "only sent when
    /// set" reasoning as `rank_math_title` and friends. Setting this to
    /// another user requires the authenticated Application Password's own
    /// user to have WordPress's `edit_others_posts` capability (Editor/
    /// Admin) - a lower-privileged user gets a normal, readable REST API
    /// error back rather than a silent no-op.
    pub author_id: Option<u64>,
    /// The author's display name, cached alongside `author_id` purely so
    /// "Artikel-Eigenschaften" can show the right name immediately (from
    /// the saved document) without waiting on a fresh `list_users()` fetch
    /// every time the dialog opens - not itself sent to WordPress.
    pub author_name: Option<String>,
    /// Excludes the post from VG Wort's counting-pixel tracking (the
    /// German collecting society compensating authors for online text
    /// use) - `false` (the default) means "tracked as normal", matching
    /// how a post behaves before this is ever touched. Sent unconditionally
    /// on every export as the "Worthy" WordPress plugin's own
    /// `wp-worthy-pixel.ignored` REST field (see `export.rs`) - harmless on
    /// a site without that plugin installed, since WordPress silently
    /// drops an unregistered field from the request body the same way it
    /// drops an unregistered `meta` key. See `stats.rs` for the length
    /// hint (VG Wort/Worthy's own minimum length before a pixel counts at
    /// all) that this toggle sits next to conceptually, even though it
    /// lives in "Artikel-Eigenschaften" as a publish-affecting setting,
    /// not in the Statistik tab.
    pub vgwort_ignored: bool,
    /// Whether comments are open on this post - `None` leaves it unsent
    /// (same "only sent when set" reasoning as `author_id`: a new post
    /// keeps whatever WordPress's own default comment status is, an
    /// existing one keeps whatever it already had), `Some(true)`/
    /// `Some(false)` send WordPress's own `comment_status` field as
    /// `"open"`/`"closed"`.
    pub comment_status: Option<bool>,
    /// The parent page's WordPress id - `page` post type only, WordPress's
    /// own hierarchical-pages feature (e.g. "Impressum" nested under "Über
    /// uns"). `None` means top-level (no parent), sent as `parent: 0` on
    /// export - the same "explicit unset, not a sentinel zero" convention
    /// as `author_id`.
    pub parent_id: Option<u64>,
    /// The parent page's title, cached alongside `parent_id` purely so
    /// "Artikel-Eigenschaften" can show it immediately without waiting on a
    /// fresh page-list fetch - same reasoning as `author_name`.
    pub parent_name: Option<String>,
    /// Per-image alt text/caption/WordPress-upload metadata (`media.rs`) -
    /// rebuilt from the current body on every properties/media-panel open
    /// via `media::reconcile`, so this only needs to persist what a plain
    /// `![alt](url)` can't represent on its own.
    pub media: Vec<MediaItem>,
}

impl Frontmatter {
    pub fn has_metadata(&self) -> bool {
        self != &Frontmatter::default()
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Document {
    pub frontmatter: Frontmatter,
    pub body: String,
}

pub fn read(path: &Path) -> std::io::Result<Document> {
    let raw = std::fs::read_to_string(path)?;
    Ok(parse(&raw))
}

pub fn write(path: &Path, doc: &Document) -> std::io::Result<()> {
    std::fs::write(path, serialize(doc))
}

pub fn parse(input: &str) -> Document {
    // `split_inclusive` keeps each line's own newline attached, so
    // concatenating a sub-slice of `lines` reproduces the original bytes
    // exactly (including whether the file ends with a trailing newline) -
    // unlike `.lines()`, which would silently drop that information.
    let lines: Vec<&str> = input.split_inclusive('\n').collect();
    let is_delimiter = |l: &str| l.trim_end_matches(['\n', '\r']) == "---";

    if lines.first().map(|l| is_delimiter(l)) != Some(true) {
        return Document {
            frontmatter: Frontmatter::default(),
            body: input.to_string(),
        };
    }

    let Some(end) = lines[1..].iter().position(|l| is_delimiter(l)) else {
        return Document {
            frontmatter: Frontmatter::default(),
            body: input.to_string(),
        };
    };
    let end = end + 1; // index into `lines`, relative to the full slice

    let mut frontmatter = Frontmatter::default();
    for line in &lines[1..end] {
        let content = line.trim_end_matches(['\n', '\r']);
        let Some((key, value)) = content.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "title" => frontmatter.title = unquote(value),
            "slug" => frontmatter.slug = unquote(value),
            "status" => frontmatter.status = PostStatus::from_str(value),
            "post_type" => frontmatter.post_type = PostType::from_str(value),
            "scheduled_at" => {
                frontmatter.scheduled_at = (!value.is_empty()).then(|| unquote(value));
            }
            "categories" => frontmatter.categories = parse_list(value),
            "tags" => frontmatter.tags = parse_list(value),
            "excerpt" => {
                frontmatter.excerpt = (!value.is_empty()).then(|| unquote(value));
            }
            "rank_math_title" => {
                frontmatter.rank_math_title = (!value.is_empty()).then(|| unquote(value));
            }
            "rank_math_description" => {
                frontmatter.rank_math_description = (!value.is_empty()).then(|| unquote(value));
            }
            "rank_math_focus_keyword" => {
                frontmatter.rank_math_focus_keyword = (!value.is_empty()).then(|| unquote(value));
            }
            "featured_image" => {
                frontmatter.featured_image = (!value.is_empty()).then(|| unquote(value));
            }
            "featured_image_alt" => {
                frontmatter.featured_image_alt = (!value.is_empty()).then(|| unquote(value));
            }
            "wp_post_id" => frontmatter.wp_post_id = value.parse::<u64>().ok(),
            "wp_content_hash" => {
                frontmatter.wp_content_hash = (!value.is_empty()).then(|| unquote(value));
            }
            "wp_featured_media_id" => frontmatter.featured_media_id = value.parse::<u64>().ok(),
            "author_id" => frontmatter.author_id = value.parse::<u64>().ok(),
            "author_name" => {
                frontmatter.author_name = (!value.is_empty()).then(|| unquote(value));
            }
            "parent_id" => frontmatter.parent_id = value.parse::<u64>().ok(),
            "parent_name" => {
                frontmatter.parent_name = (!value.is_empty()).then(|| unquote(value));
            }
            "vgwort_ignored" => frontmatter.vgwort_ignored = value.trim() == "true",
            "comment_status" => {
                frontmatter.comment_status = match value.trim() {
                    "open" => Some(true),
                    "closed" => Some(false),
                    _ => None,
                }
            }
            "media_json" => frontmatter.media = crate::media::from_json_str(value),
            _ => {}
        }
    }

    let mut body_lines = &lines[(end + 1).min(lines.len())..];
    // Exactly one blank separator line is conventional right after the
    // closing `---` (and is what `serialize` writes) - drop it so
    // round-tripping doesn't grow an extra blank line each time.
    if body_lines.first() == Some(&"\n") || body_lines.first() == Some(&"\r\n") {
        body_lines = &body_lines[1..];
    }
    let body = body_lines.concat();

    Document { frontmatter, body }
}

pub fn serialize(doc: &Document) -> String {
    if !doc.frontmatter.has_metadata() {
        return doc.body.clone();
    }

    let fm = &doc.frontmatter;
    let mut out = String::from("---\n");
    out.push_str(&format!("title: \"{}\"\n", escape(&fm.title)));
    out.push_str(&format!("slug: \"{}\"\n", escape(&fm.slug)));
    if fm.post_type != PostType::Post {
        out.push_str(&format!("post_type: {}\n", fm.post_type.as_str()));
    }
    out.push_str(&format!("status: {}\n", fm.status.as_str()));
    if let Some(scheduled_at) = &fm.scheduled_at {
        out.push_str(&format!("scheduled_at: \"{}\"\n", escape(scheduled_at)));
    }
    out.push_str(&format!("categories: {}\n", render_list(&fm.categories)));
    out.push_str(&format!("tags: {}\n", render_list(&fm.tags)));
    if let Some(excerpt) = &fm.excerpt {
        out.push_str(&format!("excerpt: \"{}\"\n", escape(excerpt)));
    }
    if let Some(title) = &fm.rank_math_title {
        out.push_str(&format!("rank_math_title: \"{}\"\n", escape(title)));
    }
    if let Some(description) = &fm.rank_math_description {
        out.push_str(&format!("rank_math_description: \"{}\"\n", escape(description)));
    }
    if let Some(keyword) = &fm.rank_math_focus_keyword {
        out.push_str(&format!("rank_math_focus_keyword: \"{}\"\n", escape(keyword)));
    }
    if let Some(img) = &fm.featured_image {
        out.push_str(&format!("featured_image: \"{}\"\n", escape(img)));
    }
    if let Some(alt) = &fm.featured_image_alt {
        out.push_str(&format!("featured_image_alt: \"{}\"\n", escape(alt)));
    }
    if let Some(id) = fm.wp_post_id {
        out.push_str(&format!("wp_post_id: {id}\n"));
    }
    if let Some(hash) = &fm.wp_content_hash {
        out.push_str(&format!("wp_content_hash: \"{}\"\n", escape(hash)));
    }
    if let Some(id) = fm.featured_media_id {
        out.push_str(&format!("wp_featured_media_id: {id}\n"));
    }
    if let Some(id) = fm.author_id {
        out.push_str(&format!("author_id: {id}\n"));
    }
    if let Some(name) = &fm.author_name {
        out.push_str(&format!("author_name: \"{}\"\n", escape(name)));
    }
    if let Some(id) = fm.parent_id {
        out.push_str(&format!("parent_id: {id}\n"));
    }
    if let Some(name) = &fm.parent_name {
        out.push_str(&format!("parent_name: \"{}\"\n", escape(name)));
    }
    if fm.vgwort_ignored {
        out.push_str("vgwort_ignored: true\n");
    }
    if let Some(open) = fm.comment_status {
        out.push_str(&format!("comment_status: {}\n", if open { "open" } else { "closed" }));
    }
    if !fm.media.is_empty() {
        out.push_str(&format!("media_json: {}\n", crate::media::to_json_string(&fm.media)));
    }
    out.push_str("---\n\n");
    out.push_str(&doc.body);
    out
}

/// SHA-256 hex digest of `content` - shared by `importer.rs` (hashing the
/// literal content a post was just fetched with) and `export.rs` (hashing
/// the literal content just sent, and re-hashing a fresh fetch to compare
/// against `Frontmatter::wp_content_hash` before the next update). Lives
/// here rather than in `media.rs`'s own file-content hashing since this
/// hashes an in-memory Gutenberg HTML string, not a local file's bytes.
pub fn content_hash(content: &str) -> String {
    sha2::Sha256::digest(content.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn unescape(s: &str) -> String {
    s.replace("\\\"", "\"").replace("\\\\", "\\")
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let quoted = (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\''));
    if quoted && s.len() >= 2 {
        unescape(&s[1..s.len() - 1])
    } else {
        s.to_string()
    }
}

/// Parses a comma-separated list, with or without surrounding `[...]` -
/// reused both for frontmatter values and for the properties dialog's
/// plain "Tech, Rust" style text entry.
pub fn parse_list(s: &str) -> Vec<String> {
    let s = s.trim();
    let inner = s.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(s);
    inner
        .split(',')
        .map(|item| unquote(item.trim()))
        .filter(|item| !item.is_empty())
        .collect()
}

/// Turns a title into a URL slug for the properties dialog's "Slug aus
/// Titel generieren" button - mirrors WordPress's own
/// `sanitize_title()`/`remove_accents()` closely enough that a slug
/// generated here matches what WordPress would produce from the same
/// title (ä/ö/ü flattened to a/o/u and ß to ss, not the ae/oe/ue spelling
/// some other tools use), so publishing never silently ends up with a
/// different slug than the one shown here. Everything else that isn't
/// ASCII alphanumeric collapses to a single hyphen; only German special
/// characters are special-cased since the app's UI is German-only.
pub fn slugify(title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    for ch in title.chars() {
        match ch {
            'ä' | 'Ä' => slug.push('a'),
            'ö' | 'Ö' => slug.push('o'),
            'ü' | 'Ü' => slug.push('u'),
            'ß' => slug.push_str("ss"),
            c if c.is_ascii_alphanumeric() => slug.push(c.to_ascii_lowercase()),
            _ => {
                if !slug.ends_with('-') {
                    slug.push('-');
                }
            }
        }
    }
    slug.trim_matches('-').to_string()
}

fn render_list(items: &[String]) -> String {
    let rendered: Vec<String> = items.iter().map(|i| format!("\"{}\"", escape(i))).collect();
    format!("[{}]", rendered.join(", "))
}

/// Parses a `"YYYY-MM-DD HH:MM"` (or `"YYYY-MM-DDTHH:MM"`) scheduled-
/// publish date/time - the format the properties dialog's plain text entry
/// takes - into the normalized ISO 8601 form WordPress's REST API `date`
/// field expects, `"YYYY-MM-DDTHH:MM:00"`. Not a full calendar validator -
/// only checks the shape and each component's numeric range, so an
/// impossible date like 2026-02-30 still reaches WordPress, which already
/// rejects that with its own clear error; this just catches typos/garbage
/// input before ever making a network call, and before `PostStatus::Future`
/// could otherwise silently publish immediately (see its doc comment).
pub fn parse_scheduled_at(input: &str) -> Option<String> {
    let input = input.trim().replacen('T', " ", 1);
    let (date, time) = input.split_once(' ')?;

    let mut date_parts = date.splitn(4, '-');
    let year: u32 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || year < 1970 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let mut time_parts = time.splitn(3, ':');
    let hour: u32 = time_parts.next()?.parse().ok()?;
    let minute: u32 = time_parts.next()?.parse().ok()?;
    if time_parts.next().is_some() || hour > 23 || minute > 59 {
        return None;
    }

    Some(format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:00"))
}

/// The reverse of `parse_scheduled_at`'s normalization, for showing an
/// already-set `Frontmatter::scheduled_at` back in the properties dialog's
/// entry - drops the `:00` seconds `parse_scheduled_at` always adds and
/// swaps the `T` back for a space.
pub fn format_scheduled_at_for_display(iso: &str) -> String {
    iso.strip_suffix(":00").unwrap_or(iso).replacen('T', " ", 1)
}

/// Prefers a path relative to the document's own directory (so the article
/// stays portable if the folder is moved as a whole) for a locally-picked
/// file `path` - falls back to the absolute path if it lives somewhere else
/// entirely, or the document hasn't been saved yet. Shared by every place
/// that turns a `Gtk.FileDialog` result into a Markdown/frontmatter
/// reference: the editor's "Bild einfügen" and the properties dialog's
/// featured-image picker.
pub fn image_reference(path: &Path, doc_dir: Option<&Path>) -> String {
    if let Some(dir) = doc_dir {
        if let Ok(relative) = path.strip_prefix(dir) {
            return relative.to_string_lossy().replace('\\', "/");
        }
    }
    path.to_string_lossy().to_string()
}

/// Picks a filename for an image pasted from the clipboard (which, unlike a
/// file picked via a dialog, has no name of its own) - `eingefuegtes-bild.png`,
/// falling back to a numbered suffix if that (or an earlier numbered variant)
/// is already taken, so pasting several screenshots in a row never silently
/// overwrites the previous one. `exists` is injected rather than calling
/// `Path::exists` directly so this stays a pure, filesystem-free unit test.
pub fn unique_pasted_image_path(dir: &Path, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let candidate = dir.join("eingefuegtes-bild.png");
    if !exists(&candidate) {
        return candidate;
    }
    let mut n = 2;
    loop {
        let candidate = dir.join(format!("eingefuegtes-bild-{n}.png"));
        if !exists(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Whether a clipboard offering these mime types has an image on it - used
/// to decide whether Ctrl+V should paste an image (see `window.rs`) instead
/// of falling through to GtkSourceView's own text paste handling. A plain
/// prefix check rather than an exact-match list, since clipboard producers
/// advertise a wide, open-ended variety of `image/*` mime types (png, jpeg,
/// bmp, tiff, webp, and platform-specific ones) and matching the prefix
/// covers all of them without needing to keep a list in sync.
pub fn mime_types_contain_image<S: AsRef<str>>(mime_types: &[S]) -> bool {
    mime_types.iter().any(|mime| mime.as_ref().starts_with("image/"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaReferenceKind {
    Image,
    Video,
    Audio,
}

/// Classifies a media reference by its file extension (ignoring any query
/// string/fragment) - this app reuses plain `![]()` Markdown image syntax
/// for all local media (see `crates/gutenberg`'s `as_lone_media`/
/// `media_kind`, the export-side counterpart of this same classification,
/// necessarily duplicated rather than shared since that crate has no
/// dependency on this one), so a few places that only make sense for an
/// actual image (the live preview's `<img>` rendering, the editor's
/// AI-alt-text context-menu action) need to recognize and exclude the
/// video/audio case. An unrecognized extension is treated as an image, the
/// long-standing default for this app.
pub fn media_reference_kind(source: &str) -> MediaReferenceKind {
    let path = source.split(['?', '#']).next().unwrap_or(source);
    match path.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
        "mp4" | "webm" | "ogv" | "mov" => MediaReferenceKind::Video,
        "mp3" | "wav" | "ogg" | "m4a" | "flac" => MediaReferenceKind::Audio,
        _ => MediaReferenceKind::Image,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_scheduled_at_normalizes_a_space_separated_date_and_time() {
        assert_eq!(parse_scheduled_at("2026-12-24 18:30"), Some("2026-12-24T18:30:00".to_string()));
    }

    #[test]
    fn parse_scheduled_at_also_accepts_a_t_separator() {
        assert_eq!(parse_scheduled_at("2026-12-24T18:30"), Some("2026-12-24T18:30:00".to_string()));
    }

    #[test]
    fn parse_scheduled_at_rejects_an_out_of_range_month() {
        assert_eq!(parse_scheduled_at("2026-13-01 12:00"), None);
    }

    #[test]
    fn parse_scheduled_at_rejects_an_out_of_range_hour() {
        assert_eq!(parse_scheduled_at("2026-12-24 25:00"), None);
    }

    #[test]
    fn parse_scheduled_at_rejects_garbage_input() {
        assert_eq!(parse_scheduled_at("not a date"), None);
        assert_eq!(parse_scheduled_at(""), None);
    }

    #[test]
    fn format_scheduled_at_for_display_strips_seconds_and_the_t_separator() {
        assert_eq!(format_scheduled_at_for_display("2026-12-24T18:30:00"), "2026-12-24 18:30");
    }

    #[test]
    fn slugify_lowercases_and_hyphenates() {
        assert_eq!(slugify("Hallo Welt"), "hallo-welt");
    }

    #[test]
    fn slugify_flattens_german_umlauts_and_eszett_like_wordpress() {
        assert_eq!(slugify("Über Größe und Maß"), "uber-grosse-und-mass");
    }

    #[test]
    fn slugify_collapses_runs_of_punctuation_and_trims_the_ends() {
        assert_eq!(slugify("  Was?! Wirklich -- ja!  "), "was-wirklich-ja");
    }

    #[test]
    fn image_reference_prefers_a_path_relative_to_the_document_directory() {
        let reference = image_reference(Path::new("/home/toff/artikel/bilder/foto.png"), Some(Path::new("/home/toff/artikel")));
        assert_eq!(reference, "bilder/foto.png");
    }

    #[test]
    fn image_reference_falls_back_to_absolute_when_outside_the_document_directory() {
        let reference = image_reference(Path::new("/home/toff/anderswo/foto.png"), Some(Path::new("/home/toff/artikel")));
        assert_eq!(reference, "/home/toff/anderswo/foto.png");
    }

    #[test]
    fn image_reference_falls_back_to_absolute_when_the_document_has_no_directory_yet() {
        let reference = image_reference(Path::new("/home/toff/artikel/foto.png"), None);
        assert_eq!(reference, "/home/toff/artikel/foto.png");
    }

    #[test]
    fn unique_pasted_image_path_uses_the_plain_name_when_nothing_exists_yet() {
        let path = unique_pasted_image_path(Path::new("/home/toff/artikel"), |_| false);
        assert_eq!(path, Path::new("/home/toff/artikel/eingefuegtes-bild.png"));
    }

    #[test]
    fn unique_pasted_image_path_numbers_around_an_existing_file() {
        let path = unique_pasted_image_path(Path::new("/home/toff/artikel"), |p| p == Path::new("/home/toff/artikel/eingefuegtes-bild.png"));
        assert_eq!(path, Path::new("/home/toff/artikel/eingefuegtes-bild-2.png"));
    }

    #[test]
    fn unique_pasted_image_path_skips_past_several_taken_numbers() {
        let path = unique_pasted_image_path(Path::new("/tmp"), |p| {
            p == Path::new("/tmp/eingefuegtes-bild.png") || p == Path::new("/tmp/eingefuegtes-bild-2.png") || p == Path::new("/tmp/eingefuegtes-bild-3.png")
        });
        assert_eq!(path, Path::new("/tmp/eingefuegtes-bild-4.png"));
    }

    #[test]
    fn mime_types_contain_image_detects_a_common_image_mime_type() {
        assert!(mime_types_contain_image(&["text/plain", "image/png"]));
    }

    #[test]
    fn mime_types_contain_image_is_false_for_text_only_clipboard_content() {
        assert!(!mime_types_contain_image(&["text/plain", "text/html"]));
    }

    #[test]
    fn media_reference_kind_recognizes_common_video_and_audio_extensions() {
        assert_eq!(media_reference_kind("clip.mp4"), MediaReferenceKind::Video);
        assert_eq!(media_reference_kind("clip.MOV"), MediaReferenceKind::Video);
        assert_eq!(media_reference_kind("song.mp3"), MediaReferenceKind::Audio);
        assert_eq!(media_reference_kind("song.mp3?ver=2"), MediaReferenceKind::Audio);
        assert_eq!(media_reference_kind("photo.png"), MediaReferenceKind::Image);
        assert_eq!(media_reference_kind("no-extension"), MediaReferenceKind::Image);
    }

    #[test]
    fn plain_markdown_without_frontmatter_round_trips() {
        let input = "# Just a heading\n\nSome text.\n";
        let doc = parse(input);
        assert_eq!(doc.frontmatter, Frontmatter::default());
        assert_eq!(doc.body, input);
        assert_eq!(serialize(&doc), input);
    }

    #[test]
    fn full_frontmatter_parses_all_fields() {
        let input = "---\n\
                     title: \"Hello World\"\n\
                     slug: \"hello-world\"\n\
                     status: publish\n\
                     categories: [\"Tech\", \"Rust\"]\n\
                     tags: [\"gtk\", \"wordpress\"]\n\
                     excerpt: \"A short summary.\"\n\
                     rank_math_title: \"SEO Title\"\n\
                     rank_math_description: \"SEO description.\"\n\
                     rank_math_focus_keyword: \"gtk markdown editor\"\n\
                     featured_image: \"/tmp/cat.png\"\n\
                     featured_image_alt: \"a sleeping cat\"\n\
                     wp_post_id: 42\n\
                     wp_featured_media_id: 7\n\
                     wp_content_hash: \"abc123\"\n\
                     author_id: 3\n\
                     author_name: \"Jane Editor\"\n\
                     parent_id: 12\n\
                     parent_name: \"Über uns\"\n\
                     vgwort_ignored: true\n\
                     comment_status: closed\n\
                     ---\n\
                     \n\
                     Body text here.\n";
        let doc = parse(input);
        assert_eq!(doc.frontmatter.title, "Hello World");
        assert_eq!(doc.frontmatter.slug, "hello-world");
        assert_eq!(doc.frontmatter.status, PostStatus::Publish);
        assert_eq!(doc.frontmatter.categories, vec!["Tech", "Rust"]);
        assert_eq!(doc.frontmatter.tags, vec!["gtk", "wordpress"]);
        assert_eq!(doc.frontmatter.excerpt.as_deref(), Some("A short summary."));
        assert_eq!(doc.frontmatter.rank_math_title.as_deref(), Some("SEO Title"));
        assert_eq!(doc.frontmatter.rank_math_description.as_deref(), Some("SEO description."));
        assert_eq!(doc.frontmatter.rank_math_focus_keyword.as_deref(), Some("gtk markdown editor"));
        assert_eq!(doc.frontmatter.featured_image.as_deref(), Some("/tmp/cat.png"));
        assert_eq!(doc.frontmatter.featured_image_alt.as_deref(), Some("a sleeping cat"));
        assert_eq!(doc.frontmatter.wp_post_id, Some(42));
        assert_eq!(doc.frontmatter.featured_media_id, Some(7));
        assert_eq!(doc.frontmatter.wp_content_hash.as_deref(), Some("abc123"));
        assert_eq!(doc.frontmatter.author_id, Some(3));
        assert_eq!(doc.frontmatter.author_name.as_deref(), Some("Jane Editor"));
        assert_eq!(doc.frontmatter.parent_id, Some(12));
        assert_eq!(doc.frontmatter.parent_name.as_deref(), Some("Über uns"));
        assert!(doc.frontmatter.vgwort_ignored);
        assert_eq!(doc.frontmatter.comment_status, Some(false));
        assert_eq!(doc.body, "Body text here.\n");
    }

    #[test]
    fn serialize_without_metadata_stays_plain() {
        let doc = Document {
            frontmatter: Frontmatter::default(),
            body: "just text\n".to_string(),
        };
        assert_eq!(serialize(&doc), "just text\n");
    }

    #[test]
    fn serialize_then_parse_round_trips_with_metadata() {
        let doc = Document {
            frontmatter: Frontmatter {
                title: "A \"quoted\" title".to_string(),
                post_type: PostType::Post,
                slug: "a-quoted-title".to_string(),
                status: PostStatus::Pending,
                scheduled_at: Some("2026-12-24T18:30:00".to_string()),
                categories: vec!["Cat A".to_string(), "Cat B".to_string()],
                tags: vec!["one".to_string()],
                excerpt: Some("A short excerpt with \"quotes\".".to_string()),
                rank_math_title: Some("SEO Title".to_string()),
                rank_math_description: Some("An SEO description.".to_string()),
                rank_math_focus_keyword: Some("gtk markdown editor".to_string()),
                featured_image: None,
                featured_image_alt: Some("a sleeping cat".to_string()),
                wp_post_id: Some(7),
                wp_content_hash: Some("deadbeef".to_string()),
                featured_media_id: Some(99),
                author_id: Some(3),
                author_name: Some("Jane Editor".to_string()),
                parent_id: Some(12),
                parent_name: Some("Über uns".to_string()),
                vgwort_ignored: true,
                comment_status: Some(false),
                media: vec![MediaItem {
                    id: "media-001".to_string(),
                    filename: "cat.png".to_string(),
                    source: "cat.png".to_string(),
                    alt: crate::media::AltText::Text("A cat".to_string()),
                    caption: Some("My cat".to_string()),
                    wordpress: Some(crate::media::WordPressMediaRef {
                        media_id: 42,
                        url: "https://example.com/cat.png".to_string(),
                        content_hash: "deadbeef".to_string(),
                    }),
                }],
            },
            body: "Some **body**.\n".to_string(),
        };
        let round_tripped = parse(&serialize(&doc));
        assert_eq!(round_tripped, doc);
    }

    #[test]
    fn unknown_status_falls_back_to_draft() {
        assert_eq!(PostStatus::from_str("bogus"), PostStatus::Draft);
    }

    #[test]
    fn private_status_round_trips_through_as_str_and_from_str() {
        assert_eq!(PostStatus::from_str(PostStatus::Private.as_str()), PostStatus::Private);
    }

    #[test]
    fn comment_status_open_parses_as_some_true() {
        let doc = parse("---\ntitle: \"x\"\ncomment_status: open\n---\n\nBody.\n");
        assert_eq!(doc.frontmatter.comment_status, Some(true));
    }

    #[test]
    fn comment_status_garbage_parses_as_none() {
        let doc = parse("---\ntitle: \"x\"\ncomment_status: garbage\n---\n\nBody.\n");
        assert_eq!(doc.frontmatter.comment_status, None);
    }

    #[test]
    fn comment_status_open_serializes_and_round_trips() {
        let doc = Document { frontmatter: Frontmatter { title: "x".to_string(), comment_status: Some(true), ..Frontmatter::default() }, body: "Body.\n".to_string() };
        let serialized = serialize(&doc);
        assert!(serialized.contains("comment_status: open"), "{serialized}");
        assert_eq!(parse(&serialized), doc);
    }

    #[test]
    fn content_hash_is_stable_and_distinguishes_different_content() {
        assert_eq!(content_hash("<p>Hello</p>"), content_hash("<p>Hello</p>"));
        assert_ne!(content_hash("<p>Hello</p>"), content_hash("<p>Hello!</p>"));
    }

    /// The whole reason `media` exists: a decorative image's deliberately
    /// blank alt text must come back as blank-but-defined, not as
    /// "undefined" again, after a full file round trip - not just within
    /// `media.rs`'s own in-memory JSON round trip.
    #[test]
    fn deliberately_empty_alt_text_survives_a_full_file_round_trip() {
        let doc = Document {
            frontmatter: Frontmatter {
                title: "Has a decorative image".to_string(),
                media: vec![MediaItem {
                    id: "media-001".to_string(),
                    filename: "divider.png".to_string(),
                    source: "divider.png".to_string(),
                    alt: crate::media::AltText::Empty,
                    caption: None,
                    wordpress: None,
                }],
                ..Frontmatter::default()
            },
            body: "![](divider.png)\n".to_string(),
        };
        let round_tripped = parse(&serialize(&doc));
        assert_eq!(round_tripped.frontmatter.media.len(), 1);
        assert!(!round_tripped.frontmatter.media[0].alt.is_undefined());
        assert_eq!(round_tripped.frontmatter.media[0].alt.as_wordpress_value(), Some(""));
    }

    #[test]
    fn post_type_defaults_to_post_and_is_not_written() {
        let doc = Document { frontmatter: Frontmatter { title: "x".to_string(), ..Frontmatter::default() }, body: "Body.\n".to_string() };
        let serialized = serialize(&doc);
        assert!(!serialized.contains("post_type"), "{serialized}");
        assert_eq!(parse(&serialized).frontmatter.post_type, PostType::Post);
    }

    #[test]
    fn page_post_type_round_trips() {
        let doc = Document { frontmatter: Frontmatter { title: "Impressum".to_string(), post_type: PostType::Page, ..Frontmatter::default() }, body: "Body.\n".to_string() };
        let serialized = serialize(&doc);
        assert!(serialized.contains("post_type: page\n"), "{serialized}");
        assert_eq!(parse(&serialized), doc);
    }

    #[test]
    fn post_type_rest_bases() {
        assert_eq!(PostType::Post.rest_base(), "posts");
        assert_eq!(PostType::Page.rest_base(), "pages");
        assert_eq!(PostType::from_str("garbage"), PostType::Post);
    }
}
