//! Blocking WordPress REST API client (`/wp-json/wp/v2/...`).
//!
//! Deliberately blocking (`ureq`, not `reqwest`): `reqwest` needs a `tokio`
//! runtime to drive its I/O, but `oo7` (see `secrets.rs`) already commits
//! this app to `async-std`'s reactor so it composes with GLib's main loop
//! without pulling in a second async runtime. REST calls here are rare,
//! user-initiated (publish/update button), and fast enough that running
//! them on a spawned thread - not the UI thread - is simpler than reconciling
//! two executors. Callers are expected to invoke these from a background
//! thread and hand the result back to the GTK thread themselves.

use std::time::Duration;

use base64::Engine;
use serde_json::Value;

use crate::i18n::tr;

pub struct Client {
    agent: ureq::Agent,
    base_url: String,
    auth_header: String,
}

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {}: {}", self.status, self.message)
    }
}

impl std::error::Error for ApiError {}

pub type Result<T> = std::result::Result<T, ApiError>;

#[derive(Debug, Clone)]
pub struct PostResult {
    pub id: u64,
    pub link: String,
}

#[derive(Debug, Clone)]
pub struct MediaResult {
    pub id: u64,
    pub source_url: String,
    /// The uploaded file's real pixel dimensions, straight from
    /// WordPress's own upload response - `0` when WordPress didn't record
    /// them (same sentinel as `WpMediaEntry`'s own width/height), never
    /// actually expected in practice for a real image upload.
    pub width: u64,
    pub height: u64,
}

/// Only used to verify uploads in integration tests (`get_media`/
/// `delete_media` below) - the app itself only ever needs a
/// `media::WordPressMediaRef` (id + URL), read back from the locally saved
/// document, not a live re-fetch from the server.
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct MediaDetail {
    pub id: u64,
    pub source_url: String,
    pub alt_text: String,
    pub caption: String,
}

/// An existing taxonomy term with its real id - unlike
/// [`Client::list_term_names`]'s name-only list (autocomplete doesn't need
/// an id), renaming or deleting a term requires it.
#[derive(Debug, Clone)]
pub struct Term {
    pub id: u64,
    pub name: String,
    pub slug: String,
    /// The parent term's id, or `0` for a top-level term - only meaningful
    /// for a hierarchical taxonomy (`categories`; WordPress's built-in
    /// `tags` taxonomy is *not* hierarchical, so this is always `0` there).
    pub parent: u64,
}

#[derive(Debug, Clone)]
pub struct PostSummary {
    pub id: u64,
    pub title: String,
    pub status: String,
    pub date: String,
    /// The post's public permalink - used e.g. by `linkpicker.rs` to insert
    /// a real, clickable link to it, not just its id/title.
    pub link: String,
}

/// A WordPress user, for the "Autor" picker in `properties.rs` -
/// `/wp-json/wp/v2/users` only returns users who have published a post
/// unless the request is authenticated with `context=edit` (which
/// `list_users` always sends, via the same Application Password auth every
/// other request here already uses).
#[derive(Debug, Clone)]
pub struct WpUser {
    pub id: u64,
    pub name: String,
}

/// An existing WordPress media library item, for the "Aus Mediathek
/// wählen…" picker (`medialibrary.rs`) - deliberately not the full
/// `MediaDetail` shape (no caption): the picker only needs enough to show
/// and identify each item, not edit it.
#[derive(Debug, Clone)]
pub struct WpMediaItem {
    pub id: u64,
    pub source_url: String,
    pub title: String,
    pub alt_text: String,
}

/// A media library item with everything the "WordPress-Mediathek" browser
/// (`mediabrowser.rs`) shows in its details pane - unlike `WpMediaItem`,
/// covers every media type (documents, audio, video too), not just images.
#[derive(Debug, Clone, Default)]
pub struct WpMediaEntry {
    pub id: u64,
    pub title: String,
    pub source_url: String,
    /// WordPress's own coarse bucket: `"image"`, `"file"` (documents,
    /// audio and video alike - the finer split lives in `mime_type`).
    pub media_type: String,
    pub mime_type: String,
    pub alt_text: String,
    /// The media item's own caption, for `gallerydialog.rs`'s per-image
    /// caption field - not shown anywhere in `mediabrowser.rs` itself.
    pub caption: String,
    /// Site-local `"YYYY-MM-DDTHH:MM:SS"` upload date.
    pub date: String,
    /// Pixel dimensions - `0` for anything that isn't an image (or an image
    /// WordPress couldn't read the size of).
    pub width: u64,
    pub height: u64,
    /// File size in bytes, `0` when WordPress didn't record it.
    pub filesize: u64,
    /// A small, server-generated thumbnail for the grid, when WordPress
    /// made one (images only) - `None` means "show a type icon instead".
    pub thumbnail_url: Option<String>,
}

/// Filter for `Client::list_media_library` - `Documents` is WordPress's
/// own `application` media type (PDFs, office files, archives).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaFilter {
    All,
    Images,
    Documents,
    Audio,
    Video,
}

impl MediaFilter {
    fn query_value(&self) -> Option<&'static str> {
        match self {
            MediaFilter::All => None,
            MediaFilter::Images => Some("image"),
            MediaFilter::Documents => Some("application"),
            MediaFilter::Audio => Some("audio"),
            MediaFilter::Video => Some("video"),
        }
    }
}

/// Only `caption.rendered` is ever actually available (`list_media_library`
/// doesn't request `context=edit`, so `caption.raw` never comes back) - and
/// WordPress always wraps a plain-text caption in a single `<p>...</p>`,
/// which would otherwise show up literally in `gallerydialog.rs`'s
/// plain-text caption field. Only strips that one common shape; anything
/// else (a caption with its own inline HTML) is left as-is rather than
/// guessing.
fn strip_wrapping_p_tag(html: &str) -> String {
    let trimmed = html.trim();
    trimmed.strip_prefix("<p>").and_then(|s| s.strip_suffix("</p>")).unwrap_or(trimmed).trim().to_string()
}

fn media_entry_from_json(item: &Value) -> Option<WpMediaEntry> {
    let details = item.get("media_details");
    let detail_u64 = |key: &str| details.and_then(|d| d.get(key)).and_then(Value::as_u64).unwrap_or(0);
    // Prefer WordPress's own square "thumbnail" size, falling back to
    // "medium" - a site can disable either in Settings → Media.
    let thumbnail_url = ["thumbnail", "medium"].iter().find_map(|size| {
        details
            .and_then(|d| d.get("sizes"))
            .and_then(|s| s.get(*size))
            .and_then(|s| s.get("source_url"))
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    Some(WpMediaEntry {
        id: item.get("id")?.as_u64()?,
        title: post_title(item),
        source_url: item.get("source_url").and_then(Value::as_str).unwrap_or_default().to_string(),
        media_type: item.get("media_type").and_then(Value::as_str).unwrap_or_default().to_string(),
        mime_type: item.get("mime_type").and_then(Value::as_str).unwrap_or_default().to_string(),
        alt_text: item.get("alt_text").and_then(Value::as_str).unwrap_or_default().to_string(),
        caption: strip_wrapping_p_tag(item.get("caption").and_then(|c| c.get("raw").or_else(|| c.get("rendered"))).and_then(Value::as_str).unwrap_or_default()),
        date: item.get("date").and_then(Value::as_str).unwrap_or_default().to_string(),
        width: detail_u64("width"),
        height: detail_u64("height"),
        filesize: detail_u64("filesize"),
        thumbnail_url,
    })
}

#[derive(Debug, Clone)]
pub struct PostDetail {
    pub id: u64,
    pub title: String,
    /// Raw Gutenberg block-comment HTML (`content.raw`, which needs
    /// `context=edit` to get - `content.rendered` has been run through
    /// WordPress's display filters, which strip the `<!-- wp:... -->`
    /// comments a block editor needs to reconstruct the blocks).
    pub content: String,
    pub status: String,
    pub slug: String,
    pub categories: Vec<u64>,
    pub tags: Vec<u64>,
    /// The post excerpt/meta description, if one is set - empty when
    /// WordPress has nothing stored (it doesn't auto-generate one until
    /// render time, so `context=edit`'s `excerpt.raw` is genuinely blank
    /// rather than a truncated-body fallback).
    pub excerpt: String,
    /// RankMath's own post meta keys, empty when RankMath isn't active on
    /// the site or hasn't set a value for this post - see
    /// `Frontmatter::rank_math_title` and friends.
    pub rank_math_title: String,
    pub rank_math_description: String,
    pub rank_math_focus_keyword: String,
    /// `0` means no featured image is set.
    pub featured_media: u64,
    /// The post's author user id - `0` is a real, if unlikely, WordPress
    /// user id (`0` conventionally means "no author"/deleted user), same
    /// sentinel convention as `featured_media` above rather than an
    /// `Option`.
    pub author: u64,
    /// Whether the post is excluded from VG Wort's counting-pixel tracking
    /// (see `Frontmatter::vgwort_ignored`), read from the "Worthy"
    /// WordPress plugin's own `wp-worthy-pixel.ignored` REST field -
    /// `false` (its own default meaning too) when that field is absent
    /// entirely, which is exactly what happens on a site without Worthy
    /// installed, so this is a safe default rather than needing an
    /// `Option` to represent "plugin not present".
    pub vgwort_ignored: bool,
    /// WordPress's own `comment_status` field, verbatim (`"open"` or
    /// `"closed"`) - see `Frontmatter::comment_status`.
    pub comment_status: String,
    /// The parent page's id (`page` post type only) - `0` means top-level,
    /// same sentinel convention as `featured_media` above. Always
    /// requested regardless of `rest_base`; WordPress's REST API simply
    /// omits a field a post type doesn't register (`posts` has no
    /// `parent`), so this comes back `0` there rather than erroring.
    pub parent: u64,
    /// Site-local `"YYYY-MM-DDTHH:MM:SS"` - the post's publish date, or for
    /// a `status == "future"` post, its scheduled publish date/time.
    pub date: String,
    /// The post's permalink as WordPress's own `get_permalink()` computes
    /// it, even for an unpublished post - not publicly viewable as-is for
    /// anything but `status == "publish"`, but appending `?preview=true`
    /// (or `&preview=true` if it already has a query string) to it is
    /// WordPress's own convention for viewing an unpublished post's
    /// current content, given a logged-in, authorized session - see
    /// `export.rs`'s "Vorschau öffnen" button.
    pub link: String,
}

fn network_error(err: ureq::Error) -> ApiError {
    ApiError {
        status: 0,
        message: err.to_string(),
    }
}

fn error_from_body(status: u16, body_text: &str) -> ApiError {
    let message = serde_json::from_str::<Value>(body_text)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_string))
        .unwrap_or_else(|| body_text.to_string());
    ApiError { status, message }
}

fn unreadable_response(status: u16, err: impl std::fmt::Display) -> ApiError {
    ApiError {
        status,
        message: tr("Antwort nicht lesbar: {err}").replace("{err}", &err.to_string()),
    }
}

impl Client {
    pub fn new(base_url: &str, username: &str, password: &str) -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .build();
        let credentials = format!("{username}:{password}");
        let auth_header = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(credentials.as_bytes())
        );
        Self {
            agent: ureq::Agent::new_with_config(config),
            base_url: base_url.trim_end_matches('/').to_string(),
            auth_header,
        }
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/wp-json/wp/v2/{path}", self.base_url)
    }

    fn get_json(&self, url: &str) -> Result<Value> {
        self.get_json_with_total_pages(url).map(|(value, _)| value)
    }

    /// Like `get_json`, but also returns the `X-WP-TotalPages` header a
    /// collection endpoint sends alongside a paginated response - `1` if
    /// the header is missing or unparseable (a single-item endpoint, or a
    /// site that doesn't send it), which is also the right answer for "how
    /// many pages" when there's only one.
    fn get_json_with_total_pages(&self, url: &str) -> Result<(Value, u32)> {
        let mut response = self
            .agent
            .get(url)
            .header("Authorization", self.auth_header.as_str())
            .call()
            .map_err(network_error)?;
        let status = response.status().as_u16();
        let total_pages = response
            .headers()
            .get("x-wp-totalpages")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(1);
        let body_text = response.body_mut().read_to_string().unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(error_from_body(status, &body_text));
        }
        let value = serde_json::from_str(&body_text).map_err(|err| unreadable_response(status, err))?;
        Ok((value, total_pages))
    }

    /// Fetches every page of a `per_page=100` collection endpoint (a
    /// taxonomy's terms here), stopping once `X-WP-TotalPages` says there's
    /// nothing left instead of silently truncating at the first 100 -
    /// without this, a blog with more than 100 tags could never surface a
    /// tag sorting past that cutoff (WordPress's default term order is
    /// alphabetical, so this bites any tag alphabetically after roughly the
    /// hundredth).
    fn get_all_pages(&self, path: &str, query: &str) -> Result<Vec<Value>> {
        let mut all = Vec::new();
        let mut page = 1u32;
        loop {
            let url = format!("{}?per_page=100&page={page}&{query}", self.endpoint(path));
            let (value, total_pages) = self.get_json_with_total_pages(&url)?;
            if let Some(items) = value.as_array() {
                all.extend(items.iter().cloned());
            }
            if page >= total_pages.max(1) {
                break;
            }
            page += 1;
        }
        Ok(all)
    }

    /// Uploads a local file to the media library, returning its id and the
    /// URL WordPress will actually serve it from (used to rewrite `wp:image`
    /// blocks whose source was a local path before export).
    pub fn upload_media(&self, bytes: &[u8], filename: &str, mime_type: &str) -> Result<MediaResult> {
        let mut response = self
            .agent
            .post(self.endpoint("media"))
            .header("Authorization", self.auth_header.as_str())
            .header("Content-Type", mime_type)
            .header("Content-Disposition", format!("attachment; filename=\"{filename}\"").as_str())
            .send(bytes)
            .map_err(network_error)?;

        let status = response.status().as_u16();
        let body_text = response.body_mut().read_to_string().unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(error_from_body(status, &body_text));
        }
        let value: Value = serde_json::from_str(&body_text).map_err(|err| unreadable_response(status, err))?;
        let id = value
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| ApiError { status, message: tr("Keine Medien-ID in der Antwort") })?;
        let source_url = value.get("source_url").and_then(Value::as_str).unwrap_or_default().to_string();
        let width = value.get("media_details").and_then(|d| d.get("width")).and_then(Value::as_u64).unwrap_or(0);
        let height = value.get("media_details").and_then(|d| d.get("height")).and_then(Value::as_u64).unwrap_or(0);
        Ok(MediaResult { id, source_url, width, height })
    }

    /// Sets alt text and/or caption on an already-uploaded media item -
    /// `upload_media`'s `POST /media` only accepts the raw file bytes, so
    /// this is always a separate follow-up call, same endpoint (WordPress
    /// treats a `POST` to an existing attachment id as an update).
    pub fn update_media_metadata(&self, media_id: u64, alt_text: Option<&str>, caption: Option<&str>) -> Result<()> {
        let mut payload = serde_json::json!({});
        if let Some(alt) = alt_text {
            payload["alt_text"] = serde_json::json!(alt);
        }
        if let Some(caption) = caption {
            payload["caption"] = serde_json::json!(caption);
        }
        let mut response = self
            .agent
            .post(self.endpoint(&format!("media/{media_id}")))
            .header("Authorization", self.auth_header.as_str())
            .send_json(&payload)
            .map_err(network_error)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let body_text = response.body_mut().read_to_string().unwrap_or_default();
            return Err(error_from_body(status, &body_text));
        }
        Ok(())
    }

    /// Fetches a media item's current alt text/caption back from the server
    /// (`context=edit` for the raw, untranslated `caption.raw` rather than
    /// the display-filtered `caption.rendered`) - used to verify an upload's
    /// metadata actually landed, not just that the `POST` returned 2xx.
    #[cfg(test)]
    pub fn get_media(&self, media_id: u64) -> Result<MediaDetail> {
        let url = format!("{}?context=edit", self.endpoint(&format!("media/{media_id}")));
        let value = self.get_json(&url)?;
        let id = value.get("id").and_then(Value::as_u64).unwrap_or(media_id);
        let source_url = value.get("source_url").and_then(Value::as_str).unwrap_or_default().to_string();
        let alt_text = value.get("alt_text").and_then(Value::as_str).unwrap_or_default().to_string();
        let caption = value
            .get("caption")
            .and_then(|c| c.get("raw"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Ok(MediaDetail { id, source_url, alt_text, caption })
    }

    /// Permanently deletes a media item (bypassing trash, which media
    /// attachments don't support anyway). Used both for integration-test
    /// cleanup and by `media::sync_uploads` to remove the now-superseded
    /// attachment after a changed local image is re-uploaded as a new one -
    /// WordPress's REST API has no way to replace an existing attachment's
    /// file in place.
    pub fn delete_media(&self, media_id: u64) -> Result<()> {
        let url = format!("{}?force=true", self.endpoint(&format!("media/{media_id}")));
        let mut response = self
            .agent
            .delete(url)
            .header("Authorization", self.auth_header.as_str())
            .call()
            .map_err(network_error)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let body_text = response.body_mut().read_to_string().unwrap_or_default();
            return Err(error_from_body(status, &body_text));
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn create_post(&self, payload: &Value) -> Result<PostResult> {
        self.create_item("posts", payload)
    }

    #[cfg(test)]
    pub fn update_post(&self, post_id: u64, payload: &Value) -> Result<PostResult> {
        self.update_item("posts", post_id, payload)
    }

    /// Like `create_post`, but for any post-like REST collection -
    /// `"posts"` or `"pages"` (see `document::PostType::rest_base`).
    pub fn create_item(&self, rest_base: &str, payload: &Value) -> Result<PostResult> {
        self.send_post_payload(self.endpoint(rest_base), payload)
    }

    pub fn update_item(&self, rest_base: &str, id: u64, payload: &Value) -> Result<PostResult> {
        self.send_post_payload(self.endpoint(&format!("{rest_base}/{id}")), payload)
    }

    /// Moves a post or page to WordPress's trash (a `DELETE` *without*
    /// `force=true`) - unlike `delete_post`, recoverable from wp-admin's
    /// own "Papierkorb" view, which is the only deletion the UI offers.
    pub fn trash_item(&self, rest_base: &str, id: u64) -> Result<()> {
        let mut response = self
            .agent
            .delete(self.endpoint(&format!("{rest_base}/{id}")))
            .header("Authorization", self.auth_header.as_str())
            .call()
            .map_err(network_error)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let body_text = response.body_mut().read_to_string().unwrap_or_default();
            return Err(error_from_body(status, &body_text));
        }
        Ok(())
    }

    /// Permanently deletes a post (bypassing trash). Mainly useful for
    /// cleaning up after integration tests against a real site.
    #[cfg(test)]
    pub fn delete_post(&self, post_id: u64) -> Result<()> {
        self.delete_item("posts", post_id)
    }

    /// Like `delete_post`, for any post-like REST collection.
    pub fn delete_item(&self, rest_base: &str, id: u64) -> Result<()> {
        let url = format!("{}?force=true", self.endpoint(&format!("{rest_base}/{id}")));
        let mut response = self
            .agent
            .delete(url)
            .header("Authorization", self.auth_header.as_str())
            .call()
            .map_err(network_error)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let body_text = response.body_mut().read_to_string().unwrap_or_default();
            return Err(error_from_body(status, &body_text));
        }
        Ok(())
    }

    /// Lists every existing term name for a taxonomy (`"categories"` or
    /// `"tags"`), for autocomplete suggestions.
    pub fn list_term_names(&self, taxonomy: &str) -> Result<Vec<String>> {
        let items = self.get_all_pages(taxonomy, "_fields=name")?;
        Ok(items.iter().filter_map(|item| item.get("name").and_then(Value::as_str).map(str::to_string)).collect())
    }

    /// Resolves a taxonomy term id back to its name (the REST API gives
    /// posts' categories/tags as ids; opening an existing post for editing
    /// needs their names for the frontmatter).
    pub fn get_term_name(&self, taxonomy: &str, id: u64) -> Result<String> {
        let url = format!("{}?_fields=name", self.endpoint(&format!("{taxonomy}/{id}")));
        let value = self.get_json(&url)?;
        Ok(value.get("name").and_then(Value::as_str).unwrap_or_default().to_string())
    }

    /// Lists every existing term - id, name and slug, unlike
    /// `list_term_names` (name only) - for the "Kategorien & Tags
    /// verwalten" dialog (needs real ids to rename or delete a term) and
    /// for caching a category's real slug (`termcache.rs`), which can
    /// differ from what `document::slugify` would derive from its name.
    pub fn list_terms(&self, taxonomy: &str) -> Result<Vec<Term>> {
        let items = self.get_all_pages(taxonomy, "orderby=name&_fields=id,name,slug,parent")?;
        Ok(items
            .iter()
            .filter_map(|item| {
                Some(Term {
                    id: item.get("id")?.as_u64()?,
                    name: item.get("name").and_then(Value::as_str)?.to_string(),
                    slug: item.get("slug").and_then(Value::as_str).unwrap_or_default().to_string(),
                    parent: item.get("parent").and_then(Value::as_u64).unwrap_or(0),
                })
            })
            .collect())
    }

    /// Resolves a WordPress user id back to their display name - same
    /// shape as `get_term_name`, used when importing an existing post
    /// (`importer.rs`) so its author's name shows immediately in "Artikel-
    /// Eigenschaften" without waiting on that dialog's own `list_users()`
    /// fetch.
    pub fn get_user_name(&self, id: u64) -> Result<String> {
        let url = format!("{}?_fields=name", self.endpoint(&format!("users/{id}")));
        let value = self.get_json(&url)?;
        Ok(value.get("name").and_then(Value::as_str).unwrap_or_default().to_string())
    }

    /// Renames an existing taxonomy term in place.
    pub fn rename_term(&self, taxonomy: &str, id: u64, new_name: &str) -> Result<()> {
        let mut response = self
            .agent
            .post(self.endpoint(&format!("{taxonomy}/{id}")))
            .header("Authorization", self.auth_header.as_str())
            .send_json(serde_json::json!({ "name": new_name }))
            .map_err(network_error)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let body_text = response.body_mut().read_to_string().unwrap_or_default();
            return Err(error_from_body(status, &body_text));
        }
        Ok(())
    }

    /// Re-parents an existing taxonomy term - `parent: 0` moves it back to
    /// the top level. Only meaningful for a hierarchical taxonomy
    /// (`categories`); sending it for `tags` is simply ignored by
    /// WordPress rather than erroring, since `post_tag` doesn't register a
    /// `parent` REST field at all, but the "Kategorien & Tags verwalten"
    /// dialog only ever calls this for categories.
    pub fn update_term_parent(&self, taxonomy: &str, id: u64, parent: u64) -> Result<()> {
        let mut response = self
            .agent
            .post(self.endpoint(&format!("{taxonomy}/{id}")))
            .header("Authorization", self.auth_header.as_str())
            .send_json(serde_json::json!({ "parent": parent }))
            .map_err(network_error)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let body_text = response.body_mut().read_to_string().unwrap_or_default();
            return Err(error_from_body(status, &body_text));
        }
        Ok(())
    }

    /// Creates a brand new taxonomy term, optionally as a child of `parent`
    /// (`0` for a top-level term) - unlike `resolve_or_create_term`, which
    /// silently reuses an existing term of the same name and always
    /// creates a new one as top-level, this is for the "Kategorien & Tags
    /// verwalten" dialog's explicit "Neue Kategorie erstellen" flow, where
    /// creating a *specific*, possibly-nested category is the whole point,
    /// not an implicit side effect of typing a name into an article.
    pub fn create_term(&self, taxonomy: &str, name: &str, parent: u64) -> Result<u64> {
        let mut response = self
            .agent
            .post(self.endpoint(taxonomy))
            .header("Authorization", self.auth_header.as_str())
            .send_json(serde_json::json!({ "name": name, "parent": parent }))
            .map_err(network_error)?;
        let status = response.status().as_u16();
        let body_text = response.body_mut().read_to_string().unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(error_from_body(status, &body_text));
        }
        let value: Value = serde_json::from_str(&body_text).map_err(|err| unreadable_response(status, err))?;
        value.get("id").and_then(Value::as_u64).ok_or_else(|| ApiError { status, message: tr("Keine Term-ID in der Antwort") })
    }

    /// Permanently deletes a taxonomy term (categories/tags have no trash,
    /// unlike posts - `force=true` is required, same as `delete_post`/
    /// `delete_media`).
    pub fn delete_term(&self, taxonomy: &str, id: u64) -> Result<()> {
        let url = format!("{}?force=true", self.endpoint(&format!("{taxonomy}/{id}")));
        let mut response = self
            .agent
            .delete(url)
            .header("Authorization", self.auth_header.as_str())
            .call()
            .map_err(network_error)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let body_text = response.body_mut().read_to_string().unwrap_or_default();
            return Err(error_from_body(status, &body_text));
        }
        Ok(())
    }

    /// Lists the most recent posts (any status the authenticated user can
    /// see), for the "Von WordPress öffnen" picker. Doesn't paginate beyond
    /// the first 50 - fine for finding a recent article to edit.
    pub fn list_posts(&self) -> Result<Vec<PostSummary>> {
        self.list_items("posts")
    }

    /// Like `list_posts`, for any post-like REST collection (`"posts"` or
    /// `"pages"`).
    pub fn list_items(&self, rest_base: &str) -> Result<Vec<PostSummary>> {
        let url = format!(
            "{}?per_page=50&orderby=date&order=desc&context=edit&status=publish,future,draft,pending,private&_fields=id,title,status,date,link",
            self.endpoint(rest_base)
        );
        let value = self.get_json(&url)?;
        Ok(value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        Some(PostSummary {
                            id: item.get("id")?.as_u64()?,
                            title: post_title(item),
                            status: item.get("status").and_then(Value::as_str).unwrap_or_default().to_string(),
                            date: item.get("date").and_then(Value::as_str).unwrap_or_default().to_string(),
                            link: item.get("link").and_then(Value::as_str).unwrap_or_default().to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Fetches a post's full content (as raw Gutenberg block-comment HTML,
    /// via `context=edit` - see [`PostDetail::content`]) plus the metadata
    /// needed to populate the properties dialog after converting it back to
    /// Markdown.
    #[cfg(test)]
    pub fn get_post(&self, id: u64) -> Result<PostDetail> {
        self.get_item("posts", id)
    }

    /// Like `get_post`, for any post-like REST collection - a page simply
    /// comes back without `categories`/`tags` (WordPress doesn't register
    /// either taxonomy for pages), which parses as two empty lists below.
    pub fn get_item(&self, rest_base: &str, id: u64) -> Result<PostDetail> {
        let url = format!(
            "{}?context=edit&_fields=id,title,content,excerpt,status,slug,categories,tags,featured_media,author,wp-worthy-pixel,comment_status,date,meta,link,parent",
            self.endpoint(&format!("{rest_base}/{id}"))
        );
        let value = self.get_json(&url)?;
        let u64_array = |key: &str| -> Vec<u64> {
            value.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default()
        };
        // RankMath registers its own meta keys with `show_in_rest`, so
        // they come back in the normal `meta` object like any other
        // registered post meta - empty (not missing) when RankMath isn't
        // active on the site at all, since WordPress's REST API always
        // includes `meta` as an object, just without unregistered keys.
        let meta_str = |key: &str| -> String { value.get("meta").and_then(|m| m.get(key)).and_then(Value::as_str).unwrap_or_default().to_string() };
        Ok(PostDetail {
            id,
            title: post_title(&value),
            content: value.get("content").and_then(|c| c.get("raw")).and_then(Value::as_str).unwrap_or_default().to_string(),
            excerpt: value.get("excerpt").and_then(|c| c.get("raw")).and_then(Value::as_str).unwrap_or_default().to_string(),
            rank_math_title: meta_str("rank_math_title"),
            rank_math_description: meta_str("rank_math_description"),
            rank_math_focus_keyword: meta_str("rank_math_focus_keyword"),
            status: value.get("status").and_then(Value::as_str).unwrap_or("draft").to_string(),
            slug: value.get("slug").and_then(Value::as_str).unwrap_or_default().to_string(),
            categories: u64_array("categories"),
            tags: u64_array("tags"),
            featured_media: value.get("featured_media").and_then(Value::as_u64).unwrap_or(0),
            author: value.get("author").and_then(Value::as_u64).unwrap_or(0),
            vgwort_ignored: value.get("wp-worthy-pixel").and_then(|p| p.get("ignored")).and_then(Value::as_bool).unwrap_or(false),
            comment_status: value.get("comment_status").and_then(Value::as_str).unwrap_or("open").to_string(),
            date: value.get("date").and_then(Value::as_str).unwrap_or_default().to_string(),
            link: value.get("link").and_then(Value::as_str).unwrap_or_default().to_string(),
            parent: value.get("parent").and_then(Value::as_u64).unwrap_or(0),
        })
    }

    /// Lists the site's WordPress users, for the "Autor" picker in
    /// "Artikel-Eigenschaften" - `context=edit` so the response includes
    /// every user the authenticated Application Password can see, not just
    /// ones with a published post (the REST API's default, unauthenticated
    /// behavior for this endpoint).
    pub fn list_users(&self) -> Result<Vec<WpUser>> {
        let items = self.get_all_pages("users", "context=edit&orderby=name&_fields=id,name")?;
        Ok(items
            .iter()
            .filter_map(|item| {
                Some(WpUser {
                    id: item.get("id")?.as_u64()?,
                    name: item.get("name").and_then(Value::as_str)?.to_string(),
                })
            })
            .collect())
    }

    /// Lists existing image attachments in the WordPress media library, most
    /// recent first, for the "Aus Mediathek wählen…" picker - `search`
    /// narrows by filename/title (WordPress's own `?search=` on this
    /// endpoint). Doesn't paginate beyond the first 60, same reasoning as
    /// `list_posts`: fine for finding a recent/known upload, and a media
    /// library can run into the thousands where full pagination would be
    /// slow and mostly pointless for this picker's actual use.
    pub fn list_media(&self, search: Option<&str>) -> Result<Vec<WpMediaItem>> {
        let mut url = format!(
            "{}?per_page=60&orderby=date&order=desc&media_type=image&_fields=id,source_url,title,alt_text",
            self.endpoint("media")
        );
        if let Some(search) = search.filter(|s| !s.trim().is_empty()) {
            url.push_str(&format!("&search={}", percent_encode(search.trim())));
        }
        let value = self.get_json(&url)?;
        Ok(value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        Some(WpMediaItem {
                            id: item.get("id")?.as_u64()?,
                            source_url: item.get("source_url").and_then(Value::as_str)?.to_string(),
                            title: post_title(item),
                            alt_text: item.get("alt_text").and_then(Value::as_str).unwrap_or_default().to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// One page (`page`, 1-based) of the full media library, most recent
    /// first, optionally narrowed by type and/or `search` - returns the
    /// items plus WordPress's `X-WP-TotalPages`, so the browser can offer
    /// "Mehr laden" instead of fetching a possibly thousands-strong library
    /// up front.
    pub fn list_media_library(&self, filter: MediaFilter, search: Option<&str>, page: u32) -> Result<(Vec<WpMediaEntry>, u32)> {
        let mut url = format!(
            "{}?per_page=48&page={page}&orderby=date&order=desc&_fields=id,date,title,source_url,media_type,mime_type,alt_text,caption,media_details",
            self.endpoint("media")
        );
        if let Some(media_type) = filter.query_value() {
            url.push_str(&format!("&media_type={media_type}"));
        }
        if let Some(search) = search.filter(|s| !s.trim().is_empty()) {
            url.push_str(&format!("&search={}", percent_encode(search.trim())));
        }
        let (value, total_pages) = self.get_json_with_total_pages(&url)?;
        let items = value.as_array().map(|items| items.iter().filter_map(media_entry_from_json).collect()).unwrap_or_default();
        Ok((items, total_pages))
    }

    fn send_post_payload(&self, url: String, payload: &Value) -> Result<PostResult> {
        let mut response = self
            .agent
            .post(url)
            .header("Authorization", self.auth_header.as_str())
            .send_json(payload)
            .map_err(network_error)?;

        let status = response.status().as_u16();
        let body_text = response.body_mut().read_to_string().unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(error_from_body(status, &body_text));
        }
        let value: Value = serde_json::from_str(&body_text).map_err(|err| unreadable_response(status, err))?;
        let id = value
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| ApiError { status, message: tr("Keine Post-ID in der Antwort") })?;
        let link = value.get("link").and_then(Value::as_str).unwrap_or_default().to_string();
        Ok(PostResult { id, link })
    }

    /// Resolves a category/tag name to its term id, creating the term if no
    /// exact match exists yet. WordPress's REST API wants term ids, not
    /// names, in a post's `categories`/`tags` arrays.
    pub fn resolve_or_create_term(&self, taxonomy: &str, name: &str) -> Result<u64> {
        let search_url = format!("{}?search={}", self.endpoint(taxonomy), percent_encode(name));
        let mut response = self
            .agent
            .get(&search_url)
            .header("Authorization", self.auth_header.as_str())
            .call()
            .map_err(network_error)?;
        let status = response.status().as_u16();
        let body_text = response.body_mut().read_to_string().unwrap_or_default();
        if (200..300).contains(&status) {
            if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&body_text) {
                let existing = items
                    .iter()
                    .find(|item| item.get("name").and_then(Value::as_str) == Some(name))
                    .and_then(|item| item.get("id").and_then(Value::as_u64));
                if let Some(id) = existing {
                    return Ok(id);
                }
            }
        }

        let mut response = self
            .agent
            .post(self.endpoint(taxonomy))
            .header("Authorization", self.auth_header.as_str())
            .send_json(serde_json::json!({ "name": name }))
            .map_err(network_error)?;
        let status = response.status().as_u16();
        let body_text = response.body_mut().read_to_string().unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(error_from_body(status, &body_text));
        }
        let value: Value = serde_json::from_str(&body_text).map_err(|err| unreadable_response(status, err))?;
        value
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| ApiError { status, message: tr("Kein Term-ID für \"{name}\" erhalten").replace("{name}", name) })
    }
}

/// A post's `title` is `{"raw": "...", "rendered": "..."}` in `context=edit`
/// responses (and just `{"rendered": "..."}` otherwise) - prefer `raw`
/// since `rendered` may have HTML entities substituted in.
fn post_title(post: &Value) -> String {
    post.get("title")
        .and_then(|t| t.get("raw").or_else(|| t.get("rendered")))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Minimal percent-encoding for a query parameter value - not a general URL
/// encoder, just enough for category/tag names in a `?search=` query.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{secrets, wpsite};

    #[test]
    fn media_entry_from_json_reads_details_and_thumbnail() {
        let item = serde_json::json!({
            "id": 7,
            "date": "2026-09-01T10:00:00",
            "title": { "rendered": "Screenshot" },
            "source_url": "https://example.com/wp-content/uploads/shot.png",
            "media_type": "image",
            "mime_type": "image/png",
            "alt_text": "Ein Fenster",
            "media_details": {
                "width": 1920, "height": 1080, "filesize": 12345,
                "sizes": { "thumbnail": { "source_url": "https://example.com/shot-150x150.png" } }
            }
        });
        let entry = media_entry_from_json(&item).expect("parsed");
        assert_eq!(entry.id, 7);
        assert_eq!(entry.title, "Screenshot");
        assert_eq!((entry.width, entry.height, entry.filesize), (1920, 1080, 12345));
        assert_eq!(entry.thumbnail_url.as_deref(), Some("https://example.com/shot-150x150.png"));
    }

    #[test]
    fn media_entry_from_json_without_sizes_has_no_thumbnail() {
        let item = serde_json::json!({ "id": 8, "media_type": "file", "mime_type": "application/pdf", "media_details": {} });
        let entry = media_entry_from_json(&item).expect("parsed");
        assert_eq!(entry.thumbnail_url, None);
        assert_eq!(entry.width, 0);
    }

    /// Backs the categories/tags autocomplete in `properties.rs` - checks
    /// it actually gets real term names back, not just a 200 with an empty
    /// or malformed body.
    #[test]
    #[ignore]
    fn list_term_names_against_real_site() {
        let config = wpsite::load();
        assert!(!config.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&config.url, &config.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");
        let client = Client::new(&config.url, &config.username, &password);

        let categories = client.list_term_names("categories").expect("list_term_names(categories) failed");
        assert!(!categories.is_empty(), "expected at least one existing category on the real site");
        assert!(categories.iter().any(|c| c == "Allgemein"), "expected the real site's known 'Allgemein' category, got {categories:?}");
    }

    /// Backs the "Autor" picker in `properties.rs` - checks the Application
    /// Password's own user comes back with a real id/name (it's always a
    /// valid, listable user of the site it belongs to), and that
    /// `get_user_name` resolves that same id back to the same name.
    #[test]
    #[ignore]
    fn list_users_against_real_site() {
        let config = wpsite::load();
        assert!(!config.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&config.url, &config.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");
        let client = Client::new(&config.url, &config.username, &password);

        let users = client.list_users().expect("list_users failed");
        assert!(!users.is_empty(), "expected at least one WordPress user on the real site");
        let matching_username = users.iter().find(|u| u.name.eq_ignore_ascii_case(&config.username) || !u.name.is_empty());
        let user = matching_username.expect("expected at least one user with a non-empty name");
        let name = client.get_user_name(user.id).expect("get_user_name failed");
        assert_eq!(name, user.name);
    }

    /// Backs "Kategorien & Tags verwalten"'s category-hierarchy feature -
    /// creates a child category under a real existing one ("Allgemein"),
    /// confirms `list_terms` reports the right `parent`, re-parents it back
    /// to top-level and confirms that too, then cleans up.
    #[test]
    #[ignore]
    fn category_hierarchy_round_trips_against_real_site() {
        let config = wpsite::load();
        assert!(!config.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&config.url, &config.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");
        let client = Client::new(&config.url, &config.username, &password);

        let categories = client.list_terms("categories").expect("list_terms failed");
        let parent = categories.iter().find(|c| c.name == "Allgemein").expect("expected the real site's known 'Allgemein' category");

        let child_id = client.create_term("categories", "Blocksatz Hierarchy Test", parent.id).expect("create_term failed");
        let refetched = client.list_terms("categories").expect("list_terms failed");
        let child = refetched.iter().find(|c| c.id == child_id).expect("expected the just-created category to come back from list_terms");
        assert_eq!(child.parent, parent.id, "expected the new category to be parented under 'Allgemein'");

        client.update_term_parent("categories", child_id, 0).expect("update_term_parent failed");
        let refetched_again = client.list_terms("categories").expect("list_terms failed");
        let child_again = refetched_again.iter().find(|c| c.id == child_id).expect("expected the category to still be there");
        assert_eq!(child_again.parent, 0, "expected the category to be back at the top level");

        client.delete_term("categories", child_id).expect("cleanup delete_term failed");
    }

    /// Backs the "Aus Mediathek wählen…" picker (`medialibrary.rs`) - checks
    /// it gets real media items back, each with an actual `source_url`.
    #[test]
    #[ignore]
    fn list_media_against_real_site() {
        let config = wpsite::load();
        assert!(!config.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&config.url, &config.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");
        let client = Client::new(&config.url, &config.username, &password);

        let items = client.list_media(None).expect("list_media failed");
        assert!(!items.is_empty(), "expected at least one existing media item on the real site");
        assert!(items.iter().all(|item| !item.source_url.is_empty()), "expected every media item to have a source_url: {items:?}");
    }

    /// Exercises create -> resolve/create term -> media upload -> update ->
    /// delete against the real, already-configured WordPress site (see
    /// `wpsite::load()`/`secrets::load_app_password`) rather than a mock.
    /// Ignored by default since it needs live credentials and a reachable
    /// site; run explicitly with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn full_round_trip_against_real_site() {
        let config = wpsite::load();
        assert!(!config.url.is_empty(), "no WordPress site configured (run the connection dialog first)");

        let password = futures_lite::future::block_on(secrets::load_app_password(&config.url, &config.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");

        let client = Client::new(&config.url, &config.username, &password);

        let category_id = client
            .resolve_or_create_term("categories", "Blocksatz Test")
            .expect("category resolve/create failed");

        let media = client
            .upload_media(b"not a real png, just bytes for the upload test", "blocksatz-test.txt", "text/plain")
            .expect("media upload failed");
        assert!(media.id > 0);

        let created = client
            .create_post(&serde_json::json!({
                "title": "Blocksatz integration test post",
                "content": "<!-- wp:paragraph -->\n<p>Created by an automated test, safe to delete.</p>\n<!-- /wp:paragraph -->",
                "status": "draft",
                "categories": [category_id],
            }))
            .expect("create_post failed");
        assert!(created.id > 0);

        let updated = client
            .update_post(
                created.id,
                &serde_json::json!({
                    "title": "Blocksatz integration test post (updated)",
                }),
            )
            .expect("update_post failed");
        assert_eq!(updated.id, created.id);

        client.delete_post(created.id).expect("cleanup delete_post failed");
    }

    /// Exercises the "Kategorien & Tags verwalten" dialog's full flow
    /// against the real site: create -> list (must include it, with a
    /// matching id) -> rename -> verify -> delete -> verify gone.
    #[test]
    #[ignore]
    fn term_management_round_trip_against_real_site() {
        let config = wpsite::load();
        assert!(!config.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&config.url, &config.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");
        let client = Client::new(&config.url, &config.username, &password);

        let term_id = client
            .resolve_or_create_term("categories", "Blocksatz Term-Test")
            .expect("resolve_or_create_term failed");

        let listed = client.list_terms("categories").expect("list_terms failed");
        assert!(listed.iter().any(|t| t.id == term_id && t.name == "Blocksatz Term-Test"), "created term not found in list_terms: {listed:?}");

        client.rename_term("categories", term_id, "Blocksatz Term-Test (umbenannt)").expect("rename_term failed");
        let renamed_name = client.get_term_name("categories", term_id).expect("get_term_name after rename failed");
        assert_eq!(renamed_name, "Blocksatz Term-Test (umbenannt)");

        client.delete_term("categories", term_id).expect("delete_term failed");
        let after_delete = client.list_terms("categories").expect("list_terms after delete failed");
        assert!(!after_delete.iter().any(|t| t.id == term_id), "term still present after delete_term: {after_delete:?}");
    }

    /// A 1x1 transparent PNG - real image bytes, not a text stand-in, so
    /// this exercises the same upload path a real screenshot would take.
    const ONE_PIXEL_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
        0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63,
        0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60,
        0x82,
    ];

    /// Exercises the standalone per-image media workflow from
    /// `mediapanel.rs`: upload -> transmit alt text/caption -> read them
    /// back from the server (not just check the `POST` returned 2xx) ->
    /// clean up. Covers "WordPress upload", "alt text transmitted to
    /// WordPress", "caption transmitted to WordPress", and "WordPress media
    /// id saved" from the media-management spec.
    #[test]
    #[ignore]
    fn media_upload_with_alt_text_and_caption_round_trips_against_real_site() {
        let config = wpsite::load();
        assert!(!config.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&config.url, &config.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");
        let client = Client::new(&config.url, &config.username, &password);

        let media = client.upload_media(ONE_PIXEL_PNG, "blocksatz-media-test.png", "image/png").expect("media upload failed");
        assert!(media.id > 0);
        assert!(!media.source_url.is_empty());

        client
            .update_media_metadata(media.id, Some("Ein einzelnes Testpixel"), Some("Testunterschrift"))
            .expect("update_media_metadata failed");

        let detail = client.get_media(media.id).expect("get_media failed");
        assert_eq!(detail.id, media.id);
        assert_eq!(detail.source_url, media.source_url);
        assert_eq!(detail.alt_text, "Ein einzelnes Testpixel");
        assert_eq!(detail.caption, "Testunterschrift");

        client.delete_media(media.id).expect("cleanup delete_media failed");
    }

    /// A deliberately empty alt text (decorative image) must reach WordPress
    /// as an empty string, not be skipped or rejected as an error - the same
    /// distinction `media::AltText::Empty` exists to preserve locally.
    #[test]
    #[ignore]
    fn media_deliberately_empty_alt_text_is_sent_as_an_empty_string() {
        let config = wpsite::load();
        assert!(!config.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&config.url, &config.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");
        let client = Client::new(&config.url, &config.username, &password);

        let media = client.upload_media(ONE_PIXEL_PNG, "blocksatz-decorative-test.png", "image/png").expect("media upload failed");

        client.update_media_metadata(media.id, Some(""), None).expect("update_media_metadata with empty alt failed");

        let detail = client.get_media(media.id).expect("get_media failed");
        assert_eq!(detail.alt_text, "");

        client.delete_media(media.id).expect("cleanup delete_media failed");
    }

    /// A failed metadata update against a nonexistent attachment id must
    /// surface as a plain `Err`, never a panic - `mediapanel.rs` relies on
    /// exactly this to keep a failed upload from touching (let alone
    /// losing) the locally held article.
    #[test]
    #[ignore]
    fn update_media_metadata_on_an_unknown_id_fails_cleanly() {
        let config = wpsite::load();
        assert!(!config.url.is_empty(), "no WordPress site configured (run the connection dialog first)");
        let password = futures_lite::future::block_on(secrets::load_app_password(&config.url, &config.username))
            .expect("keyring lookup failed")
            .expect("no application password stored for this site/user");
        let client = Client::new(&config.url, &config.username, &password);

        let result = client.update_media_metadata(u64::MAX, Some("does not matter"), None);
        assert!(result.is_err(), "expected updating a nonexistent media id to fail, not silently succeed");
    }
}
