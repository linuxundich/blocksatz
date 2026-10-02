//! Per-image metadata (alt text, caption, WordPress upload state) that
//! Markdown's own `![alt](url)` syntax can't fully represent on its own -
//! namely the three-way distinction between "no alt text has been set yet"
//! and "an alt text of exactly nothing, on purpose" (decorative images),
//! and a caption that's independent of alt text rather than one standing
//! in for the other.
//!
//! This is the foundation for the planned `.bsm` project format (a
//! self-contained container embedding an article's media, still to come) -
//! the JSON shape here is designed to drop into that format's manifest
//! largely unchanged, and for now is persisted as one line in the existing
//! `.md` frontmatter block (`document.rs`) so none of this is lost before
//! `.bsm` exists.

use std::collections::HashMap;
use std::path::Path;

use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};
use serde_json::Value;
use sha2::Digest;

use crate::i18n::tr;

/// The three states an image's alt text can be in - collapsing "empty" and
/// "not yet decided" into one falsy value (as plain Markdown does) would
/// make every freshly-inserted image look identical to one a user
/// deliberately marked decorative, which is exactly the distinction this
/// module exists to preserve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AltText {
    /// Not yet reviewed - flagged by the accessibility check.
    Undefined,
    /// Reviewed and deliberately left blank (decorative image) - not an
    /// error.
    Empty,
    Text(String),
}

/// WCAG guidance: alt text should stay well under this many characters -
/// screen readers read it aloud in full, so a long alt text is read out as
/// a wall of prose instead of a quick image description. Not enforced (an
/// image can genuinely need a longer description), just flagged as a hint
/// in the UI - see `alt_text_length_warning` and its callers in
/// `mediapanel.rs`, `imagealt.rs`, and `properties.rs`.
pub const RECOMMENDED_MAX_ALT_TEXT_LENGTH: usize = 150;

/// A short, non-blocking hint for an unusually long alt text, `None`
/// otherwise - `text` is a caption or fuller description, not a screen-
/// reader-friendly alt text (that distinction is what `MediaItem::caption`
/// exists for).
pub fn alt_text_length_warning(text: &str) -> Option<String> {
    let length = text.chars().count();
    (length > RECOMMENDED_MAX_ALT_TEXT_LENGTH).then(|| {
        tr("Alternativtext ist lang ({length} Zeichen) - Screenreader lesen ihn vollständig vor; empfohlen: deutlich unter {max} Zeichen.")
            .replace("{length}", &length.to_string())
            .replace("{max}", &RECOMMENDED_MAX_ALT_TEXT_LENGTH.to_string())
    })
}

impl AltText {
    pub fn is_undefined(&self) -> bool {
        matches!(self, AltText::Undefined)
    }

    /// The text to send WordPress's `alt_text` field - `None` for
    /// `Undefined`, since nothing has been decided yet to send.
    pub fn as_wordpress_value(&self) -> Option<&str> {
        match self {
            AltText::Undefined => None,
            AltText::Empty => Some(""),
            AltText::Text(s) => Some(s),
        }
    }

    fn to_json_pair(&self) -> (bool, String) {
        match self {
            AltText::Undefined => (false, String::new()),
            AltText::Empty => (true, String::new()),
            AltText::Text(s) => (true, s.clone()),
        }
    }

    fn from_json_pair(defined: bool, text: &str) -> Self {
        if !defined {
            AltText::Undefined
        } else if text.is_empty() {
            AltText::Empty
        } else {
            AltText::Text(text.to_string())
        }
    }
}

/// A media file already uploaded to WordPress's media library - kept so
/// re-uploading the same image is recognized as unnecessary and never
/// creates a duplicate attachment.
#[derive(Debug, Clone, PartialEq)]
pub struct WordPressMediaRef {
    pub media_id: u64,
    pub url: String,
    /// SHA-256 (hex) of the local file's content at the moment it was
    /// uploaded - compared against the current file's hash by
    /// `sync_uploads` to tell "unchanged since upload" from "the user
    /// edited/replaced the local image" without depending on mtimes, which
    /// a plain copy/touch can bump without any real content change.
    pub content_hash: String,
    /// The uploaded file's real pixel dimensions - `0` when unknown, same
    /// sentinel as `wpclient::MediaResult`'s own width/height (which this
    /// is filled in from at upload time). A document written before this
    /// field existed reads back as `0`/`0` (see `from_json`), the same
    /// "unknown, not zero-sized" meaning it has for a fresh upload
    /// WordPress genuinely didn't report a size for.
    pub width: u64,
    pub height: u64,
    /// The image size the block uses (`large`, `full`, ...) - known for
    /// images of a post opened from the blog, written back as the block's
    /// `sizeSlug` so an upload doesn't change it.
    pub size_slug: Option<String>,
}

impl WordPressMediaRef {
    /// The reference for an image placed in an article: WordPress's
    /// "large" size, like the block editor uses (the original only when the
    /// image is smaller than that) - URL, dimensions and `sizeSlug`.
    pub fn for_article(media_id: u64, sizes: &[crate::wpclient::ImageSize], content_hash: String) -> Option<WordPressMediaRef> {
        let size = crate::wpclient::pick_size(sizes, crate::wpclient::ARTICLE_IMAGE_SIZE)?;
        Some(WordPressMediaRef { media_id, url: size.url, content_hash, width: size.width, height: size.height, size_slug: Some(size.slug) })
    }
}

/// The transient state of an in-progress upload - unlike `WordPressMediaRef`,
/// this is never persisted: `Uploading` only makes sense while the app is
/// open, and a `Failed` attempt should just look like `NotUploaded` again
/// the next time the document is opened, not carry a stale error forward.
#[derive(Debug, Clone, PartialEq)]
pub enum UploadStatus {
    NotUploaded,
    Uploading,
    Uploaded(WordPressMediaRef),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MediaItem {
    /// Stable id, independent of the image's position in the document -
    /// `crates/gutenberg`/`.bsm` can reference media by id without caring
    /// where in the text (or which of several) an image appears.
    pub id: String,
    pub filename: String,
    /// The path or URL exactly as it appears in the Markdown source right
    /// now - how this item is matched back up with its `![]()` reference
    /// on every re-scan (see `reconcile`).
    pub source: String,
    pub alt: AltText,
    /// Independent of `alt` - the app never derives one from the other.
    pub caption: Option<String>,
    pub wordpress: Option<WordPressMediaRef>,
    /// The Markdown bracket text (this app's Bildunterschrift slot, see
    /// `markdown_image_text_for`) as of the last `reconcile` - never
    /// persisted, `None` until the first scan after loading. Lets
    /// `reconcile` tell "the user just edited the caption in the editor"
    /// (bracket changed since last time - adopt it) from "the bracket is
    /// merely unchanged" (keep a caption set in Medienverwaltung, which
    /// only ever writes `caption`, never the Markdown).
    pub last_markdown_caption: Option<String>,
}

impl MediaItem {
    pub fn upload_status(&self) -> UploadStatus {
        match &self.wordpress {
            Some(reference) => UploadStatus::Uploaded(reference.clone()),
            None => UploadStatus::NotUploaded,
        }
    }
}

/// Finds every `![alt](source "title")` image reference in `markdown`, in
/// document order, as `(source, alt_text_as_written, title_as_written)`
/// triples. The optional quoted `"title"` is the closest thing plain
/// Markdown has to a caption, so it seeds `MediaItem.caption` for images
/// seen for the first time (see `reconcile`) - Markdown itself still can't
/// distinguish "alt intentionally left blank" from "alt never set", so that
/// distinction is only made in `reconcile` too.
///
/// Also looks *inside* a ` ```columns `/` ```gallery `/` ```details `
/// fenced block (`crates/gutenberg`'s syntax for
/// `wp:columns`/`wp:gallery`/`wp:details`) by recursing into its raw text -
/// pulldown-cmark never re-parses a fenced code block's content as Markdown
/// on its own, so an image referenced only there would otherwise never
/// reach Medienverwaltung's alt-text/upload tracking, and
/// `export.rs::rewrite_image_urls`'s local-to-uploaded-URL substitution
/// would then have nothing to rewrite - shipping a broken local path
/// straight into the published post. ` ```pullquote ` is deliberately not
/// included: its content becomes plain RichText (see `Block::Pullquote`'s
/// doc comment), not a real block tree, so an image inside one wouldn't be
/// rewritten on export either - matching how WordPress's own pullquote
/// block doesn't support inline images.
fn scan_images(markdown: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut in_image = false;
    let mut current_source = String::new();
    let mut current_title = String::new();
    let mut current_alt = String::new();
    let mut fenced_block_text: Option<String> = None;

    for event in Parser::new(markdown) {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(lang))) if lang.as_ref() == "columns" || lang.as_ref() == "gallery" || lang.as_ref() == "details" => {
                fenced_block_text = Some(String::new());
            }
            Event::Text(text) if fenced_block_text.is_some() => {
                fenced_block_text.as_mut().expect("checked Some above").push_str(&text);
            }
            Event::End(TagEnd::CodeBlock) if fenced_block_text.is_some() => {
                out.extend(scan_images(&fenced_block_text.take().expect("checked Some above")));
            }
            Event::Start(Tag::Image { dest_url, title, .. }) => {
                in_image = true;
                current_source = dest_url.to_string();
                current_title = title.to_string();
                current_alt.clear();
            }
            Event::Text(text) if in_image => current_alt.push_str(&text),
            Event::End(TagEnd::Image) if in_image => {
                in_image = false;
                out.push((
                    std::mem::take(&mut current_source),
                    std::mem::take(&mut current_alt),
                    std::mem::take(&mut current_title),
                ));
            }
            _ => {}
        }
    }
    out
}

/// The Bildunterschrift/Alternativtext currently written for the first
/// image reference matching `source`, under the syntax convention this
/// app asks users to write directly in the editor:
/// `![Bildunterschrift](bild.png "Alternativtext")` - CommonMark's
/// bracket/title slots, deliberately paired the *opposite* of their usual
/// "bracket=alt, title=caption" roles, so the caption (what an article
/// almost always needs) is the visible, always-present part, and the alt
/// text (needed less often) is the optional, tucked-away one. `(None,
/// None)` if there's no such reference; either slot independently `None`
/// if it's empty. Used to seed the Alternativtext dialog with the live
/// Markdown values instead of a possibly-stale cached `MediaItem`, now
/// that editing either field writes back into these same slots
/// (`image_text_edits_for`).
pub fn markdown_image_text_for(markdown: &str, source: &str) -> (Option<String>, Option<String>) {
    let Some((_, bracket, title)) = scan_images(markdown).into_iter().find(|(s, _, _)| s == source) else {
        return (None, None);
    };
    ((!bracket.is_empty()).then_some(bracket), (!title.is_empty()).then_some(title))
}

/// The byte-range edits needed to set the bracket text (Bildunterschrift)
/// and title (Alternativtext) of every image reference matching `source` -
/// see `markdown_image_text_for` for the convention. `caption`/`alt` of
/// `None` or empty clears that slot; both empty leaves a bare
/// `![](source)`. Every occurrence of `source` is covered (matching how
/// `reconcile` already collapses them to one `MediaItem`), and the list is
/// empty if nothing actually needs to change (both slots already match).
///
/// Unlike a plain title-only edit, this replaces the *whole* `![...](...)`
/// construct - `range` (from pulldown-cmark's own offset iterator) already
/// covers exactly that span, so there's no need to locate the bracket/
/// parenthesis boundaries by hand the way a title-only edit would.
pub fn image_text_edits_for(markdown: &str, source: &str, caption: Option<&str>, alt: Option<&str>) -> Vec<(std::ops::Range<usize>, String)> {
    let desired_caption = caption.map(str::trim).filter(|c| !c.is_empty()).unwrap_or("");
    let desired_alt = alt.map(str::trim).filter(|a| !a.is_empty());

    let events: Vec<(Event, std::ops::Range<usize>)> = Parser::new(markdown).into_offset_iter().collect();
    let mut edits = Vec::new();
    let mut i = 0;
    while i < events.len() {
        let Event::Start(Tag::Image { dest_url, title, .. }) = &events[i].0 else {
            i += 1;
            continue;
        };
        if dest_url.as_ref() != source {
            i += 1;
            continue;
        }
        let range = events[i].1.clone();
        let mut current_bracket = String::new();
        let mut j = i + 1;
        while j < events.len() {
            match &events[j].0 {
                Event::Text(text) | Event::Code(text) => current_bracket.push_str(text),
                Event::End(TagEnd::Image) => break,
                _ => {}
            }
            j += 1;
        }
        let current_alt = (!title.is_empty()).then(|| title.to_string());
        if current_bracket == desired_caption && current_alt.as_deref() == desired_alt {
            i = j + 1;
            continue;
        }
        let new_paren = match desired_alt {
            Some(a) => format!("{dest_url} \"{}\"", escape_title(a)),
            None => dest_url.to_string(),
        };
        edits.push((range, format!("![{}]({new_paren})", escape_bracket(desired_caption))));
        i = j + 1;
    }
    edits
}

/// Escapes text for use inside a Markdown link/image's bracket portion -
/// backslashes first, then the characters that would otherwise end the
/// bracket early or start a nested one.
fn escape_bracket(text: &str) -> String {
    text.replace('\\', "\\\\").replace('[', "\\[").replace(']', "\\]")
}

/// Escapes text for use as a Markdown title's quoted content - backslashes
/// first, then double quotes, matching CommonMark's own backslash-escaping
/// rule.
fn escape_title(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Applies `image_text_edits_for` and returns the rewritten Markdown - a
/// test helper only; `imagealt.rs`'s live sync applies the same edits
/// directly to the `sourceview5::Buffer` instead, so a keystroke in the
/// dialog doesn't round-trip through a full string rebuild.
#[cfg(test)]
fn set_markdown_image_text(markdown: &str, source: &str, caption: Option<&str>, alt: Option<&str>) -> String {
    let edits = image_text_edits_for(markdown, source, caption, alt);
    if edits.is_empty() {
        return markdown.to_string();
    }
    let mut out = String::with_capacity(markdown.len());
    let mut last_end = 0;
    for (range, replacement) in edits {
        out.push_str(&markdown[last_end..range.start]);
        out.push_str(&replacement);
        last_end = range.end;
    }
    out.push_str(&markdown[last_end..]);
    out
}

/// Rebuilds the media list from the document's current text, preserving
/// every existing item's metadata (alt/caption/WordPress link) as long as
/// its image is still referenced - matched by `source`, not position, so
/// reordering images in the text doesn't lose their metadata. An image no
/// longer present is dropped; a newly-added one starts as `Undefined`
/// unless the Markdown itself already carries a non-empty alt text (e.g.
/// pasted from elsewhere), which is taken as an initial `Text` value, and
/// likewise seeds its caption from the Markdown bracket text if one is
/// present. An existing item's caption follows later edits to that bracket
/// text too (see `MediaItem::last_markdown_caption`) - otherwise a caption
/// typed straight into the editor after inserting `![](bild.png)` never
/// reached the preview or the published `<figcaption>`.
///
/// The same `source` referenced more than once in the body (a logo, a
/// divider image, reused several times) collapses to a single `MediaItem`,
/// since every place that reads this list afterward (the preview's
/// badges, Medienverwaltung's per-row editing, `sync_uploads`) already
/// matches by `source` alone and only ever expects one entry per source.
/// Without this, a second occurrence got its own separate, short-lived
/// `MediaItem` that the *next* reconcile would silently clobber with a
/// clone of whichever one `existing.iter().find()` happened to hit first -
/// losing that occurrence's own alt text/caption, and making the preview
/// show one occurrence's alt-defined badge on the other, unedited one too.
pub fn reconcile(existing: &[MediaItem], markdown: &str) -> Vec<MediaItem> {
    let mut next_serial = existing
        .iter()
        .filter_map(|item| item.id.strip_prefix("media-").and_then(|n| n.parse::<u32>().ok()))
        .max()
        .unwrap_or(0)
        + 1;

    let mut seen_sources = std::collections::HashSet::new();
    scan_images(markdown)
        .into_iter()
        .filter(|(source, _, _)| seen_sources.insert(source.clone()))
        .map(|(source, markdown_bracket, markdown_title)| {
            if let Some(found) = existing.iter().find(|item| item.source == source) {
                let mut item = found.clone();
                match &found.last_markdown_caption {
                    Some(previous) if *previous == markdown_bracket => {}
                    // Edited directly in the editor since the last scan -
                    // the Markdown wins, including clearing it.
                    Some(_) => item.caption = (!markdown_bracket.is_empty()).then(|| markdown_bracket.clone()),
                    // First scan since loading: keep a stored caption, but
                    // fill in one that only ever reached the Markdown.
                    None => {
                        if item.caption.is_none() && !markdown_bracket.is_empty() {
                            item.caption = Some(markdown_bracket.clone());
                        }
                    }
                }
                item.last_markdown_caption = Some(markdown_bracket);
                item
            } else {
                let filename = source.rsplit(['/', '\\']).next().unwrap_or(&source).to_string();
                // This app's own convention - the opposite of CommonMark's
                // usual bracket=alt/title=caption pairing, see
                // `markdown_image_text_for`'s doc comment for why: the
                // bracket text (what's actually visible when just typing
                // `![]()`) becomes the caption, and the title (the part
                // most people never bother with) becomes the alt text.
                let caption = (!markdown_bracket.is_empty()).then(|| markdown_bracket.clone());
                let alt = if markdown_title.is_empty() { AltText::Undefined } else { AltText::Text(markdown_title) };
                let id = format!("media-{next_serial:03}");
                next_serial += 1;
                MediaItem { id, filename, source, alt, caption, wordpress: None, last_markdown_caption: Some(markdown_bracket) }
            }
        })
        .collect()
}

fn hash_bytes(bytes: &[u8]) -> String {
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Resolves `source` against `doc_dir` (see `export::resolve_local_path`),
/// reads it, and hashes its content - the same hash `sync_uploads` compares
/// against, exposed separately for callers (like `mediapanel.rs`'s manual
/// upload button) that already know which single item they just uploaded
/// and only need to record its hash, not decide whether to upload at all.
pub fn hash_local_file(source: &str, doc_dir: Option<&Path>) -> std::io::Result<String> {
    let resolved = crate::export::resolve_local_path(source, doc_dir);
    let bytes = std::fs::read(resolved)?;
    Ok(hash_bytes(&bytes))
}

/// Ensures every local image in `items` is uploaded to WordPress with its
/// current filename/alt text/caption, updating each item's `wordpress` ref
/// in place, and returns a `source -> URL` map the caller uses to rewrite
/// `wp:image` blocks before publishing. An image already uploaded with a
/// content hash matching its current file is left untouched entirely - "bei
/// Bedarf hochladen", not on every single export. An already-remote source
/// (`http(s)://`, e.g. from a WordPress-imported article) is left alone
/// too, silently, since there's nothing local to upload; a LOCAL path that
/// fails to read is a real error and is surfaced, not skipped, since it
/// would otherwise publish a broken image reference with no explanation.
///
/// WordPress's REST API has no way to replace an existing attachment's file
/// in place, so a changed image is uploaded as a brand new attachment, and
/// the superseded one is then deleted so the media library doesn't
/// accumulate orphaned duplicates every time an image is edited and
/// republished. That delete is best-effort: it runs after the new upload
/// has already succeeded, so a failure to delete the old one is not itself
/// treated as an export failure.
pub fn sync_uploads(
    client: &crate::wpclient::Client,
    items: &mut [MediaItem],
    doc_dir: Option<&Path>,
) -> Result<HashMap<String, String>, String> {
    let mut urls = HashMap::new();

    for item in items.iter_mut() {
        if item.source.starts_with("http://") || item.source.starts_with("https://") {
            continue;
        }

        let resolved = crate::export::resolve_local_path(&item.source, doc_dir);
        let bytes = std::fs::read(&resolved)
            .map_err(|err| tr("Bild {path} nicht lesbar: {err}").replace("{path}", &resolved.display().to_string()).replace("{err}", &err.to_string()))?;
        let current_hash = hash_bytes(&bytes);

        if let Some(existing) = &item.wordpress {
            if existing.content_hash == current_hash {
                urls.insert(item.source.clone(), existing.url.clone());
                continue;
            }
        }

        let previous = item.wordpress.take();
        let compressed = crate::imagecompress::maybe_compress(&bytes, &item.filename);
        let media = client
            .upload_media(&compressed.bytes, &compressed.filename, compressed.mime_type)
            .map_err(|err| err.to_string())?;
        client
            .update_media_metadata(media.id, item.alt.as_wordpress_value(), item.caption.as_deref())
            .map_err(|err| err.to_string())?;

        if let Some(previous) = previous {
            let _ = client.delete_media(previous.media_id);
        }

        // The article links the "large" size, like WordPress's block
        // editor - not the original file (`WordPressMediaRef::for_article`).
        let reference = WordPressMediaRef::for_article(media.id, &media.sizes, current_hash.clone())
            .unwrap_or(WordPressMediaRef { media_id: media.id, url: media.source_url, content_hash: current_hash, width: media.width, height: media.height, size_slug: None });
        urls.insert(item.source.clone(), reference.url.clone());
        item.wordpress = Some(reference);
    }

    Ok(urls)
}

pub fn to_json(items: &[MediaItem]) -> Value {
    Value::Array(
        items
            .iter()
            .map(|item| {
                let (alt_defined, alt_text) = item.alt.to_json_pair();
                let mut object = serde_json::json!({
                    "id": item.id,
                    "filename": item.filename,
                    "source": item.source,
                    "alt": alt_text,
                    "altDefined": alt_defined,
                });
                if let Some(caption) = &item.caption {
                    object["caption"] = Value::String(caption.clone());
                }
                if let Some(wp) = &item.wordpress {
                    object["wordpress"] = serde_json::json!({ "mediaId": wp.media_id, "url": wp.url, "contentHash": wp.content_hash, "width": wp.width, "height": wp.height });
                    if let Some(size) = &wp.size_slug {
                        object["wordpress"]["sizeSlug"] = Value::String(size.clone());
                    }
                }
                object
            })
            .collect(),
    )
}

pub fn from_json(value: &Value) -> Vec<MediaItem> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let id = item.get("id")?.as_str()?.to_string();
                    let filename = item.get("filename").and_then(Value::as_str).unwrap_or_default().to_string();
                    let source = item.get("source")?.as_str()?.to_string();
                    let alt_defined = item.get("altDefined").and_then(Value::as_bool).unwrap_or(false);
                    let alt_text = item.get("alt").and_then(Value::as_str).unwrap_or_default();
                    let caption = item.get("caption").and_then(Value::as_str).map(str::to_string);
                    let wordpress = item.get("wordpress").and_then(|wp| {
                        Some(WordPressMediaRef {
                            media_id: wp.get("mediaId")?.as_u64()?,
                            url: wp.get("url").and_then(Value::as_str).unwrap_or_default().to_string(),
                            // Missing on a document saved before content-hash
                            // tracking existed - an empty hash never matches
                            // a real file's hash, so `sync_uploads` treats
                            // it as "changed" and re-uploads once on the
                            // next export, safely picking up hash tracking
                            // from then on. Not data-destructive, just one
                            // redundant upload for pre-existing images.
                            content_hash: wp.get("contentHash").and_then(Value::as_str).unwrap_or_default().to_string(),
                            width: wp.get("width").and_then(Value::as_u64).unwrap_or(0),
                            height: wp.get("height").and_then(Value::as_u64).unwrap_or(0),
                            size_slug: wp.get("sizeSlug").and_then(Value::as_str).map(str::to_string),
                        })
                    });
                    Some(MediaItem {
                        id,
                        filename,
                        source,
                        alt: AltText::from_json_pair(alt_defined, alt_text),
                        caption,
                        wordpress,
                        last_markdown_caption: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn to_json_string(items: &[MediaItem]) -> String {
    to_json(items).to_string()
}

pub fn from_json_str(s: &str) -> Vec<MediaItem> {
    serde_json::from_str::<Value>(s).map(|value| from_json(&value)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_text_length_warning_is_none_for_a_normal_length_text() {
        assert_eq!(alt_text_length_warning("Eine schlafende Katze auf einem Sofa"), None);
    }

    #[test]
    fn alt_text_length_warning_flags_text_past_the_recommended_length() {
        let long_text = "a".repeat(RECOMMENDED_MAX_ALT_TEXT_LENGTH + 1);
        let warning = alt_text_length_warning(&long_text).expect("should warn");
        assert!(warning.contains(&(RECOMMENDED_MAX_ALT_TEXT_LENGTH + 1).to_string()));
    }

    #[test]
    fn scan_finds_images_in_order_with_their_alt_text_and_title() {
        let markdown = "Text.\n\n![first alt](a.png \"first title\")\n\nMore.\n\n![](b.png)\n";
        let found = scan_images(markdown);
        assert_eq!(
            found,
            vec![
                ("a.png".to_string(), "first alt".to_string(), "first title".to_string()),
                ("b.png".to_string(), String::new(), String::new()),
            ]
        );
    }

    #[test]
    fn markdown_image_text_for_reads_bracket_as_caption_and_title_as_alt() {
        let markdown = "![Unsere Katze schläft](cat.png \"Eine graue Katze auf einem Sofa\")\n";
        assert_eq!(
            markdown_image_text_for(markdown, "cat.png"),
            (Some("Unsere Katze schläft".to_string()), Some("Eine graue Katze auf einem Sofa".to_string()))
        );
    }

    #[test]
    fn markdown_image_text_for_leaves_alt_none_without_a_title() {
        let markdown = "![Unsere Katze schläft](cat.png)\n";
        assert_eq!(markdown_image_text_for(markdown, "cat.png"), (Some("Unsere Katze schläft".to_string()), None));
    }

    #[test]
    fn markdown_image_text_for_is_none_none_when_the_source_is_not_referenced() {
        let markdown = "![Unsere Katze schläft](cat.png)\n";
        assert_eq!(markdown_image_text_for(markdown, "dog.png"), (None, None));
    }

    #[test]
    fn set_markdown_image_text_sets_bracket_and_title_where_there_were_none() {
        let markdown = "Intro.\n\n![](cat.png)\n\nOutro.\n";
        let out = set_markdown_image_text(markdown, "cat.png", Some("Unsere Katze"), Some("Eine graue Katze auf einem Sofa"));
        assert_eq!(out, "Intro.\n\n![Unsere Katze](cat.png \"Eine graue Katze auf einem Sofa\")\n\nOutro.\n");
    }

    #[test]
    fn set_markdown_image_text_replaces_existing_bracket_and_title() {
        let markdown = "![old caption](cat.png \"old alt\")\n";
        let out = set_markdown_image_text(markdown, "cat.png", Some("new caption"), Some("new alt"));
        assert_eq!(out, "![new caption](cat.png \"new alt\")\n");
    }

    #[test]
    fn set_markdown_image_text_can_set_caption_without_an_alt_text() {
        let markdown = "![](cat.png)\n";
        let out = set_markdown_image_text(markdown, "cat.png", Some("Unsere Katze"), None);
        assert_eq!(out, "![Unsere Katze](cat.png)\n");
    }

    #[test]
    fn set_markdown_image_text_removes_the_title_for_no_alt_text() {
        let markdown = "![Unsere Katze](cat.png \"old alt\")\n";
        let out = set_markdown_image_text(markdown, "cat.png", Some("Unsere Katze"), None);
        assert_eq!(out, "![Unsere Katze](cat.png)\n");
    }

    #[test]
    fn set_markdown_image_text_is_a_no_op_when_both_already_match() {
        let markdown = "![Unsere Katze](cat.png \"Eine Katze\")\n";
        assert_eq!(set_markdown_image_text(markdown, "cat.png", Some("Unsere Katze"), Some("Eine Katze")), markdown);
    }

    #[test]
    fn set_markdown_image_text_is_unchanged_when_the_source_is_not_referenced() {
        let markdown = "![a cat](cat.png)\n";
        assert_eq!(set_markdown_image_text(markdown, "dog.png", Some("x"), None), markdown);
    }

    #[test]
    fn set_markdown_image_text_updates_every_occurrence_of_the_same_source() {
        let markdown = "![a](cat.png)\n\n![b](cat.png \"old\")\n";
        let out = set_markdown_image_text(markdown, "cat.png", Some("new"), Some("alt"));
        assert_eq!(out, "![new](cat.png \"alt\")\n\n![new](cat.png \"alt\")\n");
    }

    #[test]
    fn set_markdown_image_text_escapes_double_quotes_in_the_alt_text() {
        let markdown = "![](cat.png)\n";
        let out = set_markdown_image_text(markdown, "cat.png", None, Some("a \"good\" cat"));
        assert_eq!(out, "![](cat.png \"a \\\"good\\\" cat\")\n");
    }

    #[test]
    fn set_markdown_image_text_escapes_brackets_in_the_caption() {
        let markdown = "![](cat.png)\n";
        let out = set_markdown_image_text(markdown, "cat.png", Some("a [good] cat"), None);
        assert_eq!(out, "![a \\[good\\] cat](cat.png)\n");
    }

    #[test]
    fn image_text_edits_for_round_trips_through_set_markdown_image_text_and_markdown_image_text_for() {
        let markdown = "![](cat.png)\n";
        let out = set_markdown_image_text(markdown, "cat.png", Some("a \"good\" cat"), Some("alt \"text\""));
        assert_eq!(markdown_image_text_for(&out, "cat.png"), (Some("a \"good\" cat".to_string()), Some("alt \"text\"".to_string())));
    }

    #[test]
    fn scan_finds_images_inside_a_fenced_gallery_block() {
        let markdown = "```gallery\n![first](a.jpg)\n![second](b.jpg)\n```\n";
        let found = scan_images(markdown);
        assert_eq!(
            found,
            vec![
                ("a.jpg".to_string(), "first".to_string(), String::new()),
                ("b.jpg".to_string(), "second".to_string(), String::new()),
            ]
        );
    }

    #[test]
    fn scan_finds_images_inside_a_fenced_columns_block() {
        let markdown = "```columns\n![left image](left.jpg)\n+++\nJust text, no image.\n```\n";
        let found = scan_images(markdown);
        assert_eq!(found, vec![("left.jpg".to_string(), "left image".to_string(), String::new())]);
    }

    #[test]
    fn scan_finds_images_inside_a_fenced_details_block() {
        let markdown = "```details\nMehr anzeigen\n+++\n![hidden image](hidden.jpg)\n```\n";
        let found = scan_images(markdown);
        assert_eq!(found, vec![("hidden.jpg".to_string(), "hidden image".to_string(), String::new())]);
    }

    #[test]
    fn scan_does_not_look_inside_a_fenced_pullquote_block() {
        let markdown = "```pullquote\n![not tracked](untracked.jpg)\n```\n";
        assert!(scan_images(markdown).is_empty());
    }

    #[test]
    fn scan_does_not_mistake_an_ordinary_fenced_code_block_for_a_gallery() {
        let markdown = "```rust\nlet img = \"![not an image](fake.png)\";\n```\n";
        assert!(scan_images(markdown).is_empty());
    }

    #[test]
    fn reconcile_creates_new_items_with_undefined_alt_for_a_bare_image_reference() {
        let items = reconcile(&[], "![](photo.jpg)\n");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].alt, AltText::Undefined);
        assert_eq!(items[0].filename, "photo.jpg");
        assert_eq!(items[0].caption, None);
        assert_eq!(items[0].wordpress, None);
    }

    #[test]
    fn reconcile_seeds_the_caption_from_the_bracket_text() {
        let items = reconcile(&[], "![a red barn](barn.jpg)\n");
        assert_eq!(items[0].caption, Some("a red barn".to_string()));
    }

    #[test]
    fn reconcile_seeds_text_alt_from_the_markdown_title() {
        let items = reconcile(&[], "![a red barn](barn.jpg \"A red barn at dusk\")\n");
        assert_eq!(items[0].alt, AltText::Text("A red barn at dusk".to_string()));
    }

    #[test]
    fn reconcile_leaves_alt_undefined_without_a_title_even_with_bracket_text() {
        let items = reconcile(&[], "![a red barn](barn.jpg)\n");
        assert_eq!(items[0].alt, AltText::Undefined, "bracket text seeds the caption now, not alt - alt only comes from an explicit title");
    }

    #[test]
    fn reconcile_seeds_caption_and_alt_independently_when_both_are_present() {
        let items = reconcile(&[], "![a red barn](barn.jpg \"A red barn at dusk\")\n");
        assert_eq!(items[0].caption, Some("a red barn".to_string()));
        assert_eq!(items[0].alt, AltText::Text("A red barn at dusk".to_string()));
    }

    #[test]
    fn reconcile_does_not_overwrite_an_existing_item_caption_from_markdown_title() {
        let existing = vec![MediaItem {
            id: "media-001".to_string(),
            filename: "barn.jpg".to_string(),
            source: "barn.jpg".to_string(),
            alt: AltText::Text("a red barn".to_string()),
            caption: Some("User-edited caption".to_string()),
            wordpress: None,
            last_markdown_caption: None,
        }];
        let updated = reconcile(&existing, "![a red barn](barn.jpg \"a different markdown title\")\n");
        assert_eq!(updated[0].caption, Some("User-edited caption".to_string()));
    }

    #[test]
    fn reconcile_preserves_metadata_for_still_referenced_images() {
        let existing = vec![MediaItem {
            id: "media-001".to_string(),
            filename: "cat.png".to_string(),
            source: "cat.png".to_string(),
            alt: AltText::Text("A cat".to_string()),
            caption: Some("My cat".to_string()),
            wordpress: Some(WordPressMediaRef {
                media_id: 42,
                url: "https://example.com/cat.png".to_string(),
                content_hash: "deadbeef".to_string(),
                width: 0,
                height: 0,
                size_slug: None,
            }),
            last_markdown_caption: None,
        }];
        let updated = reconcile(&existing, "![something else entirely](cat.png)\n");
        let expected = vec![MediaItem { last_markdown_caption: Some("something else entirely".to_string()), ..existing[0].clone() }];
        assert_eq!(updated, expected, "metadata for an image matched by source must survive re-scanning, even if the markdown alt text differs");
    }

    #[test]
    fn reconcile_picks_up_a_caption_typed_into_an_inserted_image_bracket() {
        // Insert-image toolbar button writes `![](x.png)`, then the user
        // types the caption straight into the editor.
        let items = reconcile(&[], "![](x.png)\n");
        assert_eq!(items[0].caption, None);
        let items = reconcile(&items, "![FIXME](x.png)\n");
        assert_eq!(items[0].caption, Some("FIXME".to_string()));
        let items = reconcile(&items, "![Endgültig](x.png)\n");
        assert_eq!(items[0].caption, Some("Endgültig".to_string()));
        let items = reconcile(&items, "![](x.png)\n");
        assert_eq!(items[0].caption, None, "clearing the bracket in the editor clears the caption too");
    }

    #[test]
    fn reconcile_keeps_a_medienverwaltung_caption_while_the_bracket_is_unchanged() {
        let mut items = reconcile(&[], "![FIXME](x.png)\n");
        items[0].caption = Some("Set in Medienverwaltung".to_string());
        let items = reconcile(&items, "![FIXME](x.png)\n\nMore text.\n");
        assert_eq!(items[0].caption, Some("Set in Medienverwaltung".to_string()));
    }

    #[test]
    fn reconcile_fills_in_a_caption_that_only_reached_the_markdown_on_first_scan_after_loading() {
        let loaded = from_json_str(&to_json_string(&reconcile(&[], "![](x.png)\n")));
        let items = reconcile(&loaded, "![FIXME](x.png)\n");
        assert_eq!(items[0].caption, Some("FIXME".to_string()));
    }

    #[test]
    fn reconcile_drops_items_no_longer_referenced() {
        let existing = vec![MediaItem {
            id: "media-001".to_string(),
            filename: "gone.png".to_string(),
            source: "gone.png".to_string(),
            alt: AltText::Empty,
            caption: None,
            wordpress: None,
            last_markdown_caption: None,
        }];
        let updated = reconcile(&existing, "No images here anymore.\n");
        assert!(updated.is_empty());
    }

    #[test]
    fn reconcile_assigns_increasing_ids_and_never_reuses_one() {
        let existing = vec![MediaItem {
            id: "media-005".to_string(),
            filename: "a.png".to_string(),
            source: "a.png".to_string(),
            alt: AltText::Undefined,
            caption: None,
            wordpress: None,
            last_markdown_caption: None,
        }];
        let updated = reconcile(&existing, "![](a.png)\n\n![](b.png)\n");
        assert_eq!(updated[0].id, "media-005");
        assert_eq!(updated[1].id, "media-006");
    }

    #[test]
    fn reconcile_collapses_the_same_source_referenced_twice_into_one_item() {
        let markdown = "![](logo.png)\n\nSome text.\n\n![](logo.png)\n";
        let items = reconcile(&[], markdown);
        assert_eq!(items.len(), 1, "expected one item, not one per occurrence: {items:?}");
    }

    #[test]
    fn reconcile_does_not_let_a_repeated_image_lose_its_edited_alt_text_on_the_next_pass() {
        let markdown = "![](logo.png)\n\nSome text.\n\n![](logo.png)\n";
        let mut items = reconcile(&[], markdown);
        items[0].alt = AltText::Text("a logo".to_string());

        let reconciled_again = reconcile(&items, markdown);
        assert_eq!(reconciled_again.len(), 1);
        assert_eq!(reconciled_again[0].alt, AltText::Text("a logo".to_string()));
    }

    #[test]
    fn json_round_trips_all_three_alt_states() {
        let items = vec![
            MediaItem { id: "media-001".into(), filename: "a.png".into(), source: "a.png".into(), alt: AltText::Undefined, caption: None, wordpress: None, last_markdown_caption: None },
            MediaItem { id: "media-002".into(), filename: "b.png".into(), source: "b.png".into(), alt: AltText::Empty, caption: None, wordpress: None, last_markdown_caption: None },
            MediaItem {
                id: "media-003".into(),
                filename: "c.png".into(),
                source: "c.png".into(),
                alt: AltText::Text("A description".into()),
                caption: Some("A caption".into()),
                wordpress: Some(WordPressMediaRef { media_id: 7, url: "https://example.com/c.png".into(), content_hash: "abc123".into(), width: 0, height: 0, size_slug: None }),
                last_markdown_caption: None,
            },
        ];
        let round_tripped = from_json_str(&to_json_string(&items));
        assert_eq!(round_tripped, items);
    }

    #[test]
    fn deliberately_empty_alt_is_distinct_from_undefined_after_a_round_trip() {
        let items = vec![
            MediaItem { id: "media-001".into(), filename: "a.png".into(), source: "a.png".into(), alt: AltText::Undefined, caption: None, wordpress: None, last_markdown_caption: None },
            MediaItem { id: "media-002".into(), filename: "b.png".into(), source: "b.png".into(), alt: AltText::Empty, caption: None, wordpress: None, last_markdown_caption: None },
        ];
        let round_tripped = from_json_str(&to_json_string(&items));
        assert!(round_tripped[0].alt.is_undefined());
        assert!(!round_tripped[1].alt.is_undefined());
        assert_eq!(round_tripped[1].alt.as_wordpress_value(), Some(""));
    }

    #[test]
    fn corrupt_or_missing_json_yields_an_empty_list_rather_than_panicking() {
        assert_eq!(from_json_str("not json at all"), Vec::new());
        assert_eq!(from_json_str(""), Vec::new());
    }
}
