//! Converts Markdown into WordPress Gutenberg block-comment HTML.
//!
//! Pipeline: `pulldown-cmark` event stream -> [`Block`] tree -> block-comment
//! annotated HTML (`<!-- wp:paragraph -->...`). Kept free of any GTK
//! dependency so it can be unit tested and reused headlessly.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

mod reverse;
pub use reverse::{gutenberg_to_markdown, render_gallery_fence};

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Paragraph { html: String },
    Heading { level: u8, html: String },
    List { ordered: bool, items: Vec<Vec<Block>> },
    BlockQuote { blocks: Vec<Block> },
    CodeBlock { lang: Option<String>, text: String },
    Image { url: String, alt: String, title: Option<String> },
    /// `wp:video` - see `as_lone_media` for how a Markdown image reference
    /// ends up here instead of `Image`.
    Video { url: String },
    /// `wp:audio` - see `as_lone_media`.
    Audio { url: String },
    /// A bare URL alone on its own line - CommonMark's only way to write
    /// "embed this", the same way `![alt](url)` alone is its only way to
    /// write a block-level image (see `as_lone_image`). Maps to WordPress's
    /// `core/embed`, a *dynamic* block: WordPress re-fetches/re-renders the
    /// actual embed HTML from `url` at display time regardless of what's
    /// saved here, so only `url` itself needs to round-trip correctly -
    /// the type/provider info this crate adds is a cosmetic nicety for the
    /// block editor's own immediate preview, not load-bearing.
    Embed { url: String },
    ThematicBreak,
    Table {
        alignments: Vec<ColumnAlignment>,
        header: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    /// `wp:columns` - side-by-side columns, each an independent block list.
    /// Markdown has no native syntax for this, so it's written as a fenced
    /// ` ```columns ` block whose content is split into columns on a line
    /// containing exactly `+++`, each side re-parsed as ordinary Markdown -
    /// see `parse_fenced_columns`.
    Columns { columns: Vec<Vec<Block>> },
    /// `wp:buttons` - one or more call-to-action buttons. Written as a
    /// fenced ` ```buttons ` block containing one Markdown link per line -
    /// see `parse_fenced_buttons`.
    Buttons { buttons: Vec<ButtonItem> },
    /// `wp:gallery` - a photo gallery. Written as a fenced ` ```gallery `
    /// block containing one Markdown image reference per line, each
    /// optionally carrying a caption via CommonMark's own image title
    /// syntax (`![alt](url "caption")` - same convention `Image.title`
    /// uses) - and, only when it differs from `GallerySettings::default`,
    /// an options line ahead of a `+++` separator (same shape `Pullquote`/
    /// `Details` use for their own optional second section) - see
    /// `parse_fenced_gallery`.
    Gallery { images: Vec<GalleryImage>, settings: GallerySettings },
    /// `wp:pullquote` - a highlighted, larger-type quote pulled out of the
    /// article, with an optional attribution. Unlike `wp:quote` this isn't
    /// an `InnerBlocks` container in WordPress - it's plain RichText, so
    /// each paragraph here becomes a bare `<p>`, not a nested
    /// `wp:paragraph`. Written as a fenced ` ```pullquote ` block, split
    /// into quote text and citation on a line containing exactly `+++`
    /// (same convention as `Columns`) - see `parse_fenced_pullquote`.
    Pullquote { paragraphs: Vec<String>, citation: Option<String> },
    /// `wp:details` - a native collapsible disclosure widget (a `<summary>`
    /// plus a hidden body that *is* a real `InnerBlocks` container, unlike
    /// `Pullquote` above). Written as a fenced ` ```details ` block, split
    /// into summary and body on a line containing exactly `+++` (same
    /// convention as `Columns`) - see `parse_fenced_details`.
    Details { summary: String, blocks: Vec<Block> },
    /// Passthrough for constructs not (yet) mapped to a specific Gutenberg
    /// block (footnotes, definition lists, ...) and for raw HTML the author
    /// wrote directly in the Markdown source. Also how `gutenberg_to_markdown`
    /// (`reverse.rs`) represents a block-comment it doesn't recognize - there
    /// `html` is the ENTIRE original `<!-- wp:name -->...<!-- /wp:name -->`
    /// comment, not just its inner HTML, and `render_block` below re-emits
    /// that form byte-for-byte instead of wrapping it in a fresh `wp:html`.
    RawHtml { html: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ButtonItem {
    pub text: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GalleryImage {
    pub url: String,
    pub alt: String,
    pub caption: Option<String>,
}

/// `wp:gallery`'s own attributes, plus each image's `sizeSlug` (WordPress
/// sets the same size on every image in a gallery, so this crate's simpler
/// data model keeps it here at the gallery level rather than per-image).
#[derive(Debug, Clone, PartialEq)]
pub struct GallerySettings {
    /// `None` is WordPress's own "Auto" - the number of columns adapts to
    /// however many images there are, up to a theme-defined max.
    pub columns: Option<u8>,
    /// `wp:gallery`'s `imageCrop` attribute - `true` (WordPress's own
    /// default) squares every thumbnail; `false` keeps each image's
    /// original aspect ratio.
    pub cropped: bool,
    /// `"none"` or `"media"` (link each image to its own full-size file) -
    /// WordPress also offers `"attachment"` (its own attachment page), not
    /// modeled here since this crate never has a page to link to.
    pub link_to: String,
    pub size_slug: String,
}

impl Default for GallerySettings {
    fn default() -> Self {
        GallerySettings { columns: None, cropped: true, link_to: "none".to_string(), size_slug: "large".to_string() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColumnAlignment {
    None,
    Left,
    Center,
    Right,
}

impl From<Alignment> for ColumnAlignment {
    fn from(a: Alignment) -> Self {
        match a {
            Alignment::None => ColumnAlignment::None,
            Alignment::Left => ColumnAlignment::Left,
            Alignment::Center => ColumnAlignment::Center,
            Alignment::Right => ColumnAlignment::Right,
        }
    }
}

/// Parse Markdown into a `Block` tree.
pub fn parse_markdown(md: &str) -> Vec<Block> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    let events: Vec<Event> = Parser::new_ext(md, options).collect();
    parse_blocks(&events, 0, events.len())
}

/// Render a `Block` tree as Gutenberg block-comment HTML, ready to hand to
/// the WordPress REST API as a post's `content`.
pub fn render_blocks(blocks: &[Block]) -> String {
    blocks.iter().map(render_block).collect::<Vec<_>>().join("\n\n")
}

/// Convenience one-shot: Markdown source -> Gutenberg block-comment HTML.
pub fn markdown_to_gutenberg(md: &str) -> String {
    render_blocks(&parse_markdown(md))
}

// ---------------------------------------------------------------------
// Parsing: pulldown-cmark event stream -> Block tree
// ---------------------------------------------------------------------

fn is_container_block_tag(tag: &Tag) -> bool {
    matches!(
        tag,
        Tag::Paragraph
            | Tag::Heading { .. }
            | Tag::BlockQuote(_)
            | Tag::List(_)
            | Tag::CodeBlock(_)
            | Tag::HtmlBlock
            | Tag::Table(_)
    )
}

/// Find the index of the `End` event matching the `Start` event at `start`,
/// using `Tag::to_end()` for depth counting. `Start`/`End` pairs are always
/// balanced by construction, so a Start only ever affects depth for the
/// specific `end_marker` it would itself produce.
fn find_matching_end(events: &[Event], start: usize, end_marker: &TagEnd) -> usize {
    let mut depth = 0usize;
    let mut j = start;
    while j < events.len() {
        match &events[j] {
            Event::Start(t) if &t.to_end() == end_marker => depth += 1,
            Event::End(e) if e == end_marker => {
                depth -= 1;
                if depth == 0 {
                    return j;
                }
            }
            _ => {}
        }
        j += 1;
    }
    events.len().saturating_sub(1)
}

fn inline_html(events: &[Event]) -> String {
    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, events.iter().cloned());
    out.trim().to_string()
}

fn collect_text(events: &[Event]) -> String {
    let mut s = String::new();
    for e in events {
        match e {
            Event::Text(t) | Event::Code(t) => s.push_str(t),
            Event::SoftBreak | Event::HardBreak => s.push('\n'),
            _ => {}
        }
    }
    s
}

fn collect_raw_html(events: &[Event]) -> String {
    let mut s = String::new();
    for e in events {
        if let Event::Html(t) = e {
            s.push_str(t);
        }
    }
    s.trim_end().to_string()
}

fn heading_level_num(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// A lone image is CommonMark's only way to express a "block-level" media
/// reference: `![alt](url)` on its own line parses as a Paragraph
/// containing exactly one inline Image. Detect that shape so it becomes a
/// `wp:image`/`wp:video`/`wp:audio` block instead of a paragraph wrapping
/// an `<img>` - Markdown has no dedicated video/audio syntax of its own, so
/// this app reuses image syntax for all local media and dispatches on the
/// url's file extension, the same way "Bild einfügen" and "Video/Audio
/// einfügen" both just insert a plain `![]()` reference regardless of type.
fn as_lone_media(events: &[Event]) -> Option<Block> {
    let Some(Event::Start(Tag::Image { dest_url, title, .. })) = events.first() else {
        return None;
    };
    let Some(Event::End(TagEnd::Image)) = events.last() else {
        return None;
    };
    match media_kind(dest_url) {
        MediaKind::Video => Some(Block::Video { url: dest_url.to_string() }),
        MediaKind::Audio => Some(Block::Audio { url: dest_url.to_string() }),
        MediaKind::Image => {
            let alt = collect_text(&events[1..events.len() - 1]);
            let title = if title.is_empty() { None } else { Some(title.to_string()) };
            Some(Block::Image {
                url: dest_url.to_string(),
                alt,
                title,
            })
        }
    }
}

enum MediaKind {
    Image,
    Video,
    Audio,
}

/// Classifies a media url by its file extension (ignoring any query string
/// or fragment) - an unrecognized extension is treated as an image, the
/// long-standing default for this app.
fn media_kind(url: &str) -> MediaKind {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    match path.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
        "mp4" | "webm" | "ogv" | "mov" => MediaKind::Video,
        "mp3" | "wav" | "ogg" | "m4a" | "flac" => MediaKind::Audio,
        _ => MediaKind::Image,
    }
}

/// A lone embeddable URL is written either as plain bare text (pulldown-cmark
/// doesn't autolink bare URLs, so this arrives as one `Event::Text`) or as an
/// explicit CommonMark autolink `<https://...>` (a `Tag::Link` whose visible
/// text is exactly its own destination). Anything else - including a normal
/// `[text](url)` link, which is clearly meant as inline prose, not a
/// standalone embed - falls through to a regular paragraph.
fn as_lone_embed(events: &[Event]) -> Option<Block> {
    let url = match events {
        [Event::Text(t)] => t.to_string(),
        [Event::Start(Tag::Link { dest_url, .. }), Event::Text(t), Event::End(TagEnd::Link)] if t.as_ref() == dest_url.as_ref() => dest_url.to_string(),
        _ => return None,
    };
    let url = url.trim();
    (url.starts_with("http://") || url.starts_with("https://")).then(|| Block::Embed { url: url.to_string() })
}

/// Same detection as `as_lone_embed`, exposed for the live preview
/// (`preview.rs`), which wants to show a placeholder for exactly the
/// paragraphs this crate turns into a `wp:embed` block at export time -
/// returns just the URL rather than a `Block`, so the preview doesn't need
/// to know about this crate's block tree at all.
pub fn lone_embed_url(events: &[Event]) -> Option<String> {
    match as_lone_embed(events)? {
        Block::Embed { url } => Some(url),
        _ => unreachable!("as_lone_embed only ever returns Block::Embed"),
    }
}

/// The provider's display type (`"video"`, `"rich"`, ...) and slug (e.g.
/// `"youtube"`) for a known embed URL - `None` for an unrecognized host,
/// matching `render_embed`'s own "still works, just more generic" fallback.
/// Exposed for the live preview to label its placeholder the same way this
/// crate's own export-time embed attributes would.
pub fn embed_provider(url: &str) -> Option<(&'static str, &'static str)> {
    embed_provider_for(url).map(|p| (p.type_, p.slug))
}

/// Splits a ` ```columns ` block's raw text into one section per column, on
/// any line containing exactly `+++` - chosen over Markdown's own `---`
/// thematic break so a real thematic break can still be written *inside* a
/// column without being mistaken for a column separator. Each section is
/// re-parsed as ordinary Markdown, so a column can hold anything a normal
/// article body can (paragraphs, images, lists, ...).
fn parse_fenced_columns(text: &str) -> Block {
    Block::Columns {
        columns: split_on_plus_separator(text).iter().map(|s| parse_markdown(s)).collect(),
    }
}

/// Splits a ` ```buttons ` block's raw text into one button per Markdown
/// link found in it (one per line is the intended usage, but this scans the
/// whole block rather than requiring exactly one link per line). A link's
/// visible text becomes the button's label, stripped of any inline
/// formatting - matching how alt text is handled elsewhere, since
/// WordPress's own button block only ever holds plain text.
fn parse_fenced_buttons(text: &str) -> Block {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let events: Vec<Event> = Parser::new_ext(text, options).collect();
    let mut buttons = Vec::new();
    let mut i = 0;
    while i < events.len() {
        if let Event::Start(Tag::Link { dest_url, .. }) = &events[i] {
            let end = find_matching_end(&events, i, &TagEnd::Link);
            buttons.push(ButtonItem {
                text: collect_text(&events[i + 1..end]),
                url: dest_url.to_string(),
            });
            i = end + 1;
        } else {
            i += 1;
        }
    }
    Block::Buttons { buttons }
}

/// Splits a ` ```gallery ` block's raw text on a `+++` line (see
/// `split_on_plus_separator`) into an optional settings line and the image
/// list, same "one optional second section" shape as `parse_fenced_pullquote`/
/// `parse_fenced_details` - except here it's the *first* section that's
/// optional, so a gallery with no `+++` at all (every gallery this crate
/// wrote before `GallerySettings` existed, or one hand-written without
/// caring about them) is just its plain image list with every setting at
/// its default.
fn parse_fenced_gallery(text: &str) -> Block {
    let sections = split_on_plus_separator(text);
    match sections.get(1) {
        Some(images_text) => Block::Gallery { images: parse_gallery_images(images_text), settings: parse_gallery_settings_line(sections[0].trim()) },
        None => Block::Gallery { images: parse_gallery_images(&sections[0]), settings: GallerySettings::default() },
    }
}

/// One image per Markdown image reference found in `text` (one per line is
/// the intended usage, same scanning approach as `parse_fenced_buttons`) -
/// a title (`![alt](url "caption")`, same convention `as_lone_media` uses
/// for a body image's own caption) becomes that image's gallery caption.
fn parse_gallery_images(text: &str) -> Vec<GalleryImage> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let events: Vec<Event> = Parser::new_ext(text, options).collect();
    let mut images = Vec::new();
    let mut i = 0;
    while i < events.len() {
        if let Event::Start(Tag::Image { dest_url, title, .. }) = &events[i] {
            let end = find_matching_end(&events, i, &TagEnd::Image);
            images.push(GalleryImage {
                alt: collect_text(&events[i + 1..end]),
                url: dest_url.to_string(),
                caption: (!title.is_empty()).then(|| title.to_string()),
            });
            i = end + 1;
        } else {
            i += 1;
        }
    }
    images
}

/// Parses a gallery's optional settings line - space-separated `key=value`
/// tokens, an unknown key or an unparseable value simply left at its
/// default rather than rejecting the whole line (keeps a hand-edited or
/// slightly-stale line harmless instead of silently losing every setting
/// over one typo).
fn parse_gallery_settings_line(line: &str) -> GallerySettings {
    let mut settings = GallerySettings::default();
    for token in line.split_whitespace() {
        let Some((key, value)) = token.split_once('=') else { continue };
        match key {
            "columns" => settings.columns = value.parse().ok(),
            "crop" => settings.cropped = value == "true",
            "link" => settings.link_to = value.to_string(),
            "size" => settings.size_slug = value.to_string(),
            _ => {}
        }
    }
    settings
}

/// Splits a fenced block's raw text into sections on any line containing
/// exactly `+++` - the same separator convention `parse_fenced_columns`
/// uses, shared here by `parse_fenced_pullquote` and `parse_fenced_details`
/// since both need one "primary" section plus one optional second section.
fn split_on_plus_separator(text: &str) -> Vec<String> {
    let mut sections: Vec<String> = vec![String::new()];
    for line in text.lines() {
        if line.trim() == "+++" {
            sections.push(String::new());
        } else {
            let current = sections.last_mut().expect("sections always has at least one element");
            current.push_str(line);
            current.push('\n');
        }
    }
    sections
}

/// Splits a ` ```pullquote ` block's raw text into quote text and an
/// optional citation on a `+++` line (see `split_on_plus_separator`). The
/// quote text is parsed as ordinary Markdown and flattened to one HTML
/// string per paragraph (`block_inner_html`), matching how WordPress's own
/// pullquote RichText field holds a bare `<p>` per paragraph rather than a
/// nested block tree.
fn parse_fenced_pullquote(text: &str) -> Block {
    let sections = split_on_plus_separator(text);
    let citation = sections.get(1).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let paragraphs = parse_markdown(&sections[0]).iter().map(block_inner_html).collect();
    Block::Pullquote { paragraphs, citation }
}

/// Splits a ` ```details ` block's raw text into a summary and body on a
/// `+++` line (see `split_on_plus_separator`). The summary is flattened to
/// a single inline HTML string (`<summary>` holds plain RichText, not a
/// block tree); the body is parsed as ordinary Markdown into a real `Block`
/// tree, since `wp:details`'s body *is* an `InnerBlocks` container.
fn parse_fenced_details(text: &str) -> Block {
    let sections = split_on_plus_separator(text);
    let summary = parse_markdown(&sections[0]).first().map(block_inner_html).unwrap_or_default();
    let blocks = sections.get(1).map(|s| parse_markdown(s)).unwrap_or_default();
    Block::Details { summary, blocks }
}

fn parse_blocks(events: &[Event], mut i: usize, stop: usize) -> Vec<Block> {
    let mut blocks = Vec::new();
    while i < stop {
        match &events[i] {
            Event::Rule => {
                blocks.push(Block::ThematicBreak);
                i += 1;
            }
            Event::Start(tag) if is_container_block_tag(tag) => {
                let end_marker = tag.to_end();
                let end = find_matching_end(events, i, &end_marker);
                match tag {
                    Tag::Paragraph => {
                        let inner = &events[i + 1..end];
                        blocks.push(as_lone_media(inner).or_else(|| as_lone_embed(inner)).unwrap_or_else(|| Block::Paragraph {
                            html: inline_html(inner),
                        }));
                    }
                    Tag::Heading { level, .. } => {
                        let inner = &events[i + 1..end];
                        blocks.push(Block::Heading {
                            level: heading_level_num(*level),
                            html: inline_html(inner),
                        });
                    }
                    Tag::BlockQuote(_) => {
                        blocks.push(Block::BlockQuote {
                            blocks: parse_blocks(events, i + 1, end),
                        });
                    }
                    Tag::List(start_num) => {
                        blocks.push(Block::List {
                            ordered: start_num.is_some(),
                            items: parse_list_items(events, i + 1, end),
                        });
                    }
                    Tag::CodeBlock(kind) => {
                        let lang = match kind {
                            CodeBlockKind::Fenced(lang) if !lang.is_empty() => {
                                Some(lang.to_string())
                            }
                            _ => None,
                        };
                        let text = collect_text(&events[i + 1..end]);
                        blocks.push(match lang.as_deref() {
                            Some("columns") => parse_fenced_columns(&text),
                            Some("buttons") => parse_fenced_buttons(&text),
                            Some("gallery") => parse_fenced_gallery(&text),
                            Some("pullquote") => parse_fenced_pullquote(&text),
                            Some("details") => parse_fenced_details(&text),
                            _ => Block::CodeBlock { lang, text },
                        });
                    }
                    Tag::HtmlBlock => {
                        blocks.push(Block::RawHtml {
                            html: collect_raw_html(&events[i + 1..end]),
                        });
                    }
                    Tag::Table(aligns) => {
                        blocks.push(parse_table(events, i, end, aligns));
                    }
                    _ => unreachable!("is_container_block_tag guards this match"),
                }
                i = end + 1;
            }
            _ => {
                // Bare inline run: covers tight list items (no Paragraph
                // wrapper emitted by pulldown-cmark) and any other stray
                // inline content at block position.
                let run_start = i;
                while i < stop {
                    match &events[i] {
                        Event::Rule => break,
                        Event::Start(t) if is_container_block_tag(t) => break,
                        _ => i += 1,
                    }
                }
                let html = inline_html(&events[run_start..i]);
                if !html.is_empty() {
                    blocks.push(Block::Paragraph { html });
                }
            }
        }
    }
    blocks
}

fn parse_list_items(events: &[Event], mut i: usize, stop: usize) -> Vec<Vec<Block>> {
    let mut items = Vec::new();
    while i < stop {
        if matches!(&events[i], Event::Start(Tag::Item)) {
            let end = find_matching_end(events, i, &TagEnd::Item);
            items.push(parse_blocks(events, i + 1, end));
            i = end + 1;
        } else {
            i += 1;
        }
    }
    items
}

fn parse_table(events: &[Event], start: usize, end: usize, aligns: &[Alignment]) -> Block {
    let alignments: Vec<ColumnAlignment> = aligns.iter().map(|a| (*a).into()).collect();
    let mut header = Vec::new();
    let mut rows = Vec::new();
    let mut i = start + 1;
    while i < end {
        match &events[i] {
            Event::Start(Tag::TableHead) => {
                let head_end = find_matching_end(events, i, &TagEnd::TableHead);
                header = parse_table_row_cells(events, i + 1, head_end);
                i = head_end + 1;
            }
            Event::Start(Tag::TableRow) => {
                let row_end = find_matching_end(events, i, &TagEnd::TableRow);
                rows.push(parse_table_row_cells(events, i + 1, row_end));
                i = row_end + 1;
            }
            _ => i += 1,
        }
    }
    Block::Table {
        alignments,
        header,
        rows,
    }
}

fn parse_table_row_cells(events: &[Event], mut i: usize, end: usize) -> Vec<String> {
    let mut cells = Vec::new();
    while i < end {
        if matches!(&events[i], Event::Start(Tag::TableCell)) {
            let cell_end = find_matching_end(events, i, &TagEnd::TableCell);
            cells.push(inline_html(&events[i + 1..cell_end]));
            i = cell_end + 1;
        } else {
            i += 1;
        }
    }
    cells
}

// ---------------------------------------------------------------------
// Rendering: Block tree -> Gutenberg block-comment HTML
// ---------------------------------------------------------------------

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

fn escape_json_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

struct EmbedProvider {
    host_contains: &'static str,
    type_: &'static str,
    slug: &'static str,
}

/// WordPress's own oEmbed provider list is much larger than this - these
/// are just the handful common enough to be worth naming explicitly for a
/// nicer immediate block-editor preview (see `Block::Embed`'s doc comment
/// for why an unrecognized provider still works, just more generically).
const EMBED_PROVIDERS: &[EmbedProvider] = &[
    EmbedProvider { host_contains: "youtube.com", type_: "video", slug: "youtube" },
    EmbedProvider { host_contains: "youtu.be", type_: "video", slug: "youtube" },
    EmbedProvider { host_contains: "vimeo.com", type_: "video", slug: "vimeo" },
    EmbedProvider { host_contains: "twitter.com", type_: "rich", slug: "twitter" },
    EmbedProvider { host_contains: "x.com", type_: "rich", slug: "twitter" },
    EmbedProvider { host_contains: "instagram.com", type_: "rich", slug: "instagram" },
    EmbedProvider { host_contains: "soundcloud.com", type_: "rich", slug: "soundcloud" },
    EmbedProvider { host_contains: "open.spotify.com", type_: "rich", slug: "spotify" },
];

fn embed_provider_for(url: &str) -> Option<&'static EmbedProvider> {
    EMBED_PROVIDERS.iter().find(|p| url.contains(p.host_contains))
}

fn render_media_tag(tag: &str, url: &str) -> String {
    wrap(tag, None, &format!("<figure class=\"wp-block-{tag}\"><{tag} controls src=\"{}\"></{tag}></figure>", escape_html(url)))
}

fn render_embed(url: &str) -> String {
    let provider = embed_provider_for(url);
    let attrs = match provider {
        Some(p) => format!(
            "{{\"url\":\"{}\",\"type\":\"{}\",\"providerNameSlug\":\"{}\",\"responsive\":true}}",
            escape_json_string(url),
            p.type_,
            p.slug
        ),
        None => format!("{{\"url\":\"{}\"}}", escape_json_string(url)),
    };
    let classes = match provider {
        Some(p) => format!("wp-block-embed is-type-{} is-provider-{} wp-block-embed-{}", p.type_, p.slug, p.slug),
        None => "wp-block-embed".to_string(),
    };
    wrap(
        "embed",
        Some(attrs),
        &format!("<figure class=\"{classes}\"><div class=\"wp-block-embed__wrapper\">\n{}\n</div></figure>", escape_html(url)),
    )
}

fn wrap(name: &str, attrs: Option<String>, content: &str) -> String {
    let attrs_str = attrs.map(|a| format!(" {a}")).unwrap_or_default();
    format!("<!-- wp:{name}{attrs_str} -->\n{content}\n<!-- /wp:{name} -->")
}

fn block_inner_html(block: &Block) -> String {
    match block {
        Block::Paragraph { html } => html.clone(),
        other => render_block(other),
    }
}

fn render_list_item(blocks: &[Block]) -> String {
    if blocks.is_empty() {
        return wrap("list-item", None, "<li></li>");
    }
    let mut content = block_inner_html(&blocks[0]);
    for b in &blocks[1..] {
        content.push('\n');
        content.push_str(&render_block(b));
    }
    wrap("list-item", None, &format!("<li>{content}</li>"))
}

fn render_list(ordered: bool, items: &[Vec<Block>]) -> String {
    let tag = if ordered { "ol" } else { "ul" };
    let attrs = ordered.then(|| "{\"ordered\":true}".to_string());
    let items_html = items
        .iter()
        .map(|item| render_list_item(item))
        .collect::<Vec<_>>()
        .join("\n");
    wrap(
        "list",
        attrs,
        &format!("<{tag} class=\"wp-block-list\">\n{items_html}\n</{tag}>"),
    )
}

fn align_style(alignments: &[ColumnAlignment], idx: usize) -> &'static str {
    match alignments.get(idx) {
        Some(ColumnAlignment::Left) => " style=\"text-align:left\"",
        Some(ColumnAlignment::Center) => " style=\"text-align:center\"",
        Some(ColumnAlignment::Right) => " style=\"text-align:right\"",
        _ => "",
    }
}

fn render_table(alignments: &[ColumnAlignment], header: &[String], rows: &[Vec<String>]) -> String {
    let thead = if header.is_empty() {
        String::new()
    } else {
        let cells: String = header
            .iter()
            .enumerate()
            .map(|(idx, h)| format!("<th{}>{h}</th>", align_style(alignments, idx)))
            .collect();
        format!("<thead><tr>{cells}</tr></thead>")
    };
    let body_rows: String = rows
        .iter()
        .map(|row| {
            let cells: String = row
                .iter()
                .enumerate()
                .map(|(idx, c)| format!("<td{}>{c}</td>", align_style(alignments, idx)))
                .collect();
            format!("<tr>{cells}</tr>")
        })
        .collect();
    wrap(
        "table",
        None,
        &format!("<figure class=\"wp-block-table\"><table><tbody>{body_rows}</tbody></table></figure>")
            .replacen("<tbody>", &format!("{thead}<tbody>"), 1),
    )
}

fn render_columns(columns: &[Vec<Block>]) -> String {
    let inner = columns
        .iter()
        .map(|col| wrap("column", None, &format!("<div class=\"wp-block-column\">{}</div>", render_blocks(col))))
        .collect::<Vec<_>>()
        .join("\n\n");
    wrap("columns", None, &format!("<div class=\"wp-block-columns\">\n{inner}\n</div>"))
}

fn render_buttons(buttons: &[ButtonItem]) -> String {
    let inner = buttons
        .iter()
        .map(|b| {
            wrap(
                "button",
                None,
                &format!(
                    "<div class=\"wp-block-button\"><a class=\"wp-block-button__link wp-element-button\" href=\"{}\">{}</a></div>",
                    escape_html(&b.url),
                    escape_html(&b.text)
                ),
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    wrap("buttons", None, &format!("<div class=\"wp-block-buttons\">\n{inner}\n</div>"))
}

fn render_gallery(images: &[GalleryImage], settings: &GallerySettings) -> String {
    let inner = images
        .iter()
        .map(|img| {
            let img_tag = format!("<img src=\"{}\" alt=\"{}\"/>", escape_html(&img.url), escape_html(&img.alt));
            // WordPress links each image to its own full-size file for
            // `linkTo:"media"` - this crate has no separate "full size" URL
            // of its own to link to instead, so the image's own `url` (the
            // same one `<img src>` already uses) doubles as that target.
            let linked_img = if settings.link_to == "media" { format!("<a href=\"{}\">{img_tag}</a>", escape_html(&img.url)) } else { img_tag };
            let figcaption = img
                .caption
                .as_ref()
                .filter(|c| !c.is_empty())
                .map(|c| format!("<figcaption class=\"wp-element-caption\">{}</figcaption>", escape_html(c)))
                .unwrap_or_default();
            wrap(
                "image",
                Some(format!("{{\"sizeSlug\":\"{}\",\"linkDestination\":\"{}\"}}", settings.size_slug, if settings.link_to == "media" { "media" } else { "none" })),
                &format!("<figure class=\"wp-block-image size-{}\">{linked_img}{figcaption}</figure>", settings.size_slug),
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let columns_class = settings.columns.map(|n| format!("columns-{n}")).unwrap_or_else(|| "columns-default".to_string());
    let crop_class = if settings.cropped { " is-cropped" } else { "" };
    let mut attrs = format!("\"linkTo\":\"{}\"", settings.link_to);
    if let Some(columns) = settings.columns {
        attrs.push_str(&format!(",\"columns\":{columns}"));
    }
    if !settings.cropped {
        attrs.push_str(",\"imageCrop\":false");
    }
    wrap(
        "gallery",
        Some(format!("{{{attrs}}}")),
        &format!("<figure class=\"wp-block-gallery has-nested-images {columns_class}{crop_class}\">\n{inner}\n</figure>"),
    )
}

fn render_pullquote(paragraphs: &[String], citation: &Option<String>) -> String {
    let text = paragraphs.iter().map(|p| format!("<p>{p}</p>")).collect::<Vec<_>>().join("");
    let cite = citation
        .as_ref()
        .filter(|c| !c.is_empty())
        .map(|c| format!("<cite>{}</cite>", escape_html(c)))
        .unwrap_or_default();
    wrap(
        "pullquote",
        None,
        &format!("<figure class=\"wp-block-pullquote\"><blockquote>{text}{cite}</blockquote></figure>"),
    )
}

fn render_details(summary: &str, blocks: &[Block]) -> String {
    let inner = render_blocks(blocks);
    wrap(
        "details",
        None,
        &format!("<details class=\"wp-block-details\"><summary>{summary}</summary>\n{inner}</details>"),
    )
}

fn render_block(block: &Block) -> String {
    match block {
        Block::Paragraph { html } => wrap("paragraph", None, &format!("<p>{html}</p>")),
        Block::Heading { level, html } => {
            let attrs = (*level != 2).then(|| format!("{{\"level\":{level}}}"));
            wrap("heading", attrs, &format!("<h{level}>{html}</h{level}>"))
        }
        Block::List { ordered, items } => render_list(*ordered, items),
        Block::BlockQuote { blocks } => {
            let inner = render_blocks(blocks);
            wrap(
                "quote",
                None,
                &format!("<blockquote class=\"wp-block-quote\">{inner}</blockquote>"),
            )
        }
        Block::CodeBlock { lang: _, text } => wrap(
            "code",
            None,
            &format!(
                "<pre class=\"wp-block-code\"><code>{}</code></pre>",
                escape_html(text.trim_end_matches('\n'))
            ),
        ),
        Block::Image { url, alt, title } => {
            // A markdown image "title" is the caption - rendered as a real
            // `<figcaption>` inside the figure, matching WordPress's own
            // image block markup, so it actually shows up on the published
            // page. An `<img title="">` attribute (this used to emit one)
            // is just an invisible hover tooltip, never a visible caption.
            let figcaption = title
                .as_ref()
                .filter(|t| !t.is_empty())
                .map(|t| format!("<figcaption class=\"wp-element-caption\">{}</figcaption>", escape_html(t)))
                .unwrap_or_default();
            wrap(
                "image",
                None,
                &format!(
                    "<figure class=\"wp-block-image\"><img src=\"{}\" alt=\"{}\"/>{figcaption}</figure>",
                    escape_html(url),
                    escape_html(alt)
                ),
            )
        }
        Block::Video { url } => render_media_tag("video", url),
        Block::Audio { url } => render_media_tag("audio", url),
        Block::Embed { url } => render_embed(url),
        Block::ThematicBreak => wrap(
            "separator",
            None,
            "<hr class=\"wp-block-separator has-alpha-channel-opacity\"/>",
        ),
        Block::Table {
            alignments,
            header,
            rows,
        } => render_table(alignments, header, rows),
        Block::Columns { columns } => render_columns(columns),
        Block::Buttons { buttons } => render_buttons(buttons),
        Block::Gallery { images, settings } => render_gallery(images, settings),
        Block::Pullquote { paragraphs, citation } => render_pullquote(paragraphs, citation),
        Block::Details { summary, blocks } => render_details(summary, blocks),
        // WordPress's "Weiterlesen" marker is, unusually among Gutenberg
        // blocks, still just the bare `<!--more-->` HTML comment as its own
        // inner content - `pulldown-cmark` already hands that to us as an
        // ordinary raw-HTML block, so recognizing this one exact case here
        // is enough; everything else still passes through as `wp:html`.
        Block::RawHtml { html } if html.trim() == "<!--more-->" => wrap("more", None, "<!--more-->"),
        // A block-comment `gutenberg_to_markdown` (reverse.rs) couldn't
        // recognize and kept verbatim, wrapper comment included (see its
        // `make_block` fallback) - re-emit it exactly as WordPress
        // originally wrote it rather than nesting it inside a fresh
        // `wp:html` block, which would strip its identity/attrs and, for a
        // dynamic block (a Synced Pattern reference, Page Break, ...),
        // leave it inert as inert literal comment text instead of the
        // actual functioning block.
        Block::RawHtml { html } if html.trim_start().starts_with("<!-- wp:") => html.trim().to_string(),
        Block::RawHtml { html } => wrap("html", None, html.trim()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraph_becomes_wp_paragraph() {
        assert_eq!(
            markdown_to_gutenberg("Hello **world**."),
            "<!-- wp:paragraph -->\n<p>Hello <strong>world</strong>.</p>\n<!-- /wp:paragraph -->"
        );
    }

    #[test]
    fn heading_level_two_has_no_attrs() {
        assert_eq!(
            markdown_to_gutenberg("## Title"),
            "<!-- wp:heading -->\n<h2>Title</h2>\n<!-- /wp:heading -->"
        );
    }

    #[test]
    fn heading_level_three_carries_level_attr() {
        assert_eq!(
            markdown_to_gutenberg("### Sub"),
            "<!-- wp:heading {\"level\":3} -->\n<h3>Sub</h3>\n<!-- /wp:heading -->"
        );
    }

    #[test]
    fn unordered_list_uses_list_item_children() {
        let out = markdown_to_gutenberg("- one\n- two\n");
        assert_eq!(
            out,
            "<!-- wp:list -->\n<ul class=\"wp-block-list\">\n\
             <!-- wp:list-item -->\n<li>one</li>\n<!-- /wp:list-item -->\n\
             <!-- wp:list-item -->\n<li>two</li>\n<!-- /wp:list-item -->\n\
             </ul>\n<!-- /wp:list -->"
        );
    }

    #[test]
    fn ordered_list_gets_ordered_attribute() {
        let out = markdown_to_gutenberg("1. first\n2. second\n");
        assert!(out.starts_with("<!-- wp:list {\"ordered\":true} -->\n<ol"));
        assert!(out.contains("<li>first</li>"));
    }

    #[test]
    fn nested_list_sits_inside_parent_li() {
        let out = markdown_to_gutenberg("- a\n  - nested\n- b\n");
        assert!(out.contains("<li>a\n<!-- wp:list -->"));
        assert!(out.contains("<li>nested</li>"));
    }

    #[test]
    fn blockquote_wraps_child_paragraph_block() {
        let out = markdown_to_gutenberg("> quoted text");
        assert_eq!(
            out,
            "<!-- wp:quote -->\n<blockquote class=\"wp-block-quote\">\
             <!-- wp:paragraph -->\n<p>quoted text</p>\n<!-- /wp:paragraph --></blockquote>\n\
             <!-- /wp:quote -->"
        );
    }

    #[test]
    fn fenced_code_block_becomes_wp_code() {
        let out = markdown_to_gutenberg("```rust\nlet x = 1;\n```");
        assert_eq!(
            out,
            "<!-- wp:code -->\n<pre class=\"wp-block-code\"><code>let x = 1;</code></pre>\n<!-- /wp:code -->"
        );
    }

    #[test]
    fn lone_image_line_becomes_wp_image() {
        let out = markdown_to_gutenberg("![a cat](https://example.com/cat.png)");
        assert_eq!(
            out,
            "<!-- wp:image -->\n<figure class=\"wp-block-image\">\
             <img src=\"https://example.com/cat.png\" alt=\"a cat\"/></figure>\n<!-- /wp:image -->"
        );
    }

    #[test]
    fn lone_image_line_with_a_title_gets_a_visible_figcaption() {
        let out = markdown_to_gutenberg("![a cat](https://example.com/cat.png \"A very good cat\")");
        assert_eq!(
            out,
            "<!-- wp:image -->\n<figure class=\"wp-block-image\">\
             <img src=\"https://example.com/cat.png\" alt=\"a cat\"/>\
             <figcaption class=\"wp-element-caption\">A very good cat</figcaption></figure>\n<!-- /wp:image -->"
        );
    }

    #[test]
    fn lone_video_reference_becomes_wp_video() {
        let out = markdown_to_gutenberg("![](clip.mp4)");
        assert_eq!(out, "<!-- wp:video -->\n<figure class=\"wp-block-video\"><video controls src=\"clip.mp4\"></video></figure>\n<!-- /wp:video -->");
    }

    #[test]
    fn lone_audio_reference_becomes_wp_audio() {
        let out = markdown_to_gutenberg("![](song.mp3)");
        assert_eq!(out, "<!-- wp:audio -->\n<figure class=\"wp-block-audio\"><audio controls src=\"song.mp3\"></audio></figure>\n<!-- /wp:audio -->");
    }

    #[test]
    fn lone_bare_url_line_becomes_wp_embed_with_known_provider() {
        let out = markdown_to_gutenberg("https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        assert_eq!(
            out,
            "<!-- wp:embed {\"url\":\"https://www.youtube.com/watch?v=dQw4w9WgXcQ\",\"type\":\"video\",\"providerNameSlug\":\"youtube\",\"responsive\":true} -->\n\
             <figure class=\"wp-block-embed is-type-video is-provider-youtube wp-block-embed-youtube\">\
             <div class=\"wp-block-embed__wrapper\">\nhttps://www.youtube.com/watch?v=dQw4w9WgXcQ\n</div></figure>\n<!-- /wp:embed -->"
        );
    }

    #[test]
    fn lone_autolink_url_line_becomes_wp_embed() {
        let out = markdown_to_gutenberg("<https://x.com/someuser/status/12345>");
        assert!(out.starts_with("<!-- wp:embed {\"url\":\"https://x.com/someuser/status/12345\""));
    }

    #[test]
    fn lone_url_from_an_unknown_provider_becomes_a_generic_wp_embed() {
        let out = markdown_to_gutenberg("https://example.com/some-article");
        assert_eq!(
            out,
            "<!-- wp:embed {\"url\":\"https://example.com/some-article\"} -->\n\
             <figure class=\"wp-block-embed\"><div class=\"wp-block-embed__wrapper\">\nhttps://example.com/some-article\n</div></figure>\n<!-- /wp:embed -->"
        );
    }

    #[test]
    fn lone_embed_url_matches_a_bare_url_paragraph() {
        let events: Vec<Event> = Parser::new("https://www.youtube.com/watch?v=dQw4w9WgXcQ").collect();
        // Strip the paragraph Start/End wrapper, the same slice `parse_blocks`
        // itself passes to `as_lone_embed`.
        let inner = &events[1..events.len() - 1];
        assert_eq!(lone_embed_url(inner), Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_string()));
    }

    #[test]
    fn lone_embed_url_is_none_for_ordinary_prose() {
        let events: Vec<Event> = Parser::new("Just a normal sentence.").collect();
        let inner = &events[1..events.len() - 1];
        assert_eq!(lone_embed_url(inner), None);
    }

    #[test]
    fn embed_provider_recognizes_youtube_and_vimeo() {
        assert_eq!(embed_provider("https://www.youtube.com/watch?v=x"), Some(("video", "youtube")));
        assert_eq!(embed_provider("https://youtu.be/x"), Some(("video", "youtube")));
        assert_eq!(embed_provider("https://vimeo.com/123456"), Some(("video", "vimeo")));
    }

    #[test]
    fn embed_provider_is_none_for_an_unknown_host() {
        assert_eq!(embed_provider("https://example.com/some-article"), None);
    }

    #[test]
    fn a_url_used_as_link_text_stays_a_normal_link() {
        let out = markdown_to_gutenberg("[Video ansehen](https://www.youtube.com/watch?v=dQw4w9WgXcQ)");
        assert_eq!(
            out,
            "<!-- wp:paragraph -->\n<p><a href=\"https://www.youtube.com/watch?v=dQw4w9WgXcQ\">Video ansehen</a></p>\n<!-- /wp:paragraph -->"
        );
    }

    #[test]
    fn thematic_break_becomes_wp_separator() {
        let out = markdown_to_gutenberg("---");
        assert_eq!(
            out,
            "<!-- wp:separator -->\n<hr class=\"wp-block-separator has-alpha-channel-opacity\"/>\n<!-- /wp:separator -->"
        );
    }

    #[test]
    fn table_becomes_wp_table() {
        let out = markdown_to_gutenberg("| A | B |\n|---|---|\n| 1 | 2 |\n");
        assert_eq!(
            out,
            "<!-- wp:table -->\n<figure class=\"wp-block-table\"><table><thead><tr><th>A</th><th>B</th></tr></thead><tbody><tr><td>1</td><td>2</td></tr></tbody></table></figure>\n<!-- /wp:table -->"
        );
    }

    #[test]
    fn raw_html_block_is_passed_through() {
        let out = markdown_to_gutenberg("<div class=\"embed\">hi</div>");
        assert_eq!(
            out,
            "<!-- wp:html -->\n<div class=\"embed\">hi</div>\n<!-- /wp:html -->"
        );
    }

    #[test]
    fn lone_more_marker_becomes_wp_more() {
        let out = markdown_to_gutenberg("Erster Absatz.\n\n<!--more-->\n\nZweiter Absatz.");
        assert_eq!(
            out,
            "<!-- wp:paragraph -->\n<p>Erster Absatz.</p>\n<!-- /wp:paragraph -->\n\n\
             <!-- wp:more -->\n<!--more-->\n<!-- /wp:more -->\n\n\
             <!-- wp:paragraph -->\n<p>Zweiter Absatz.</p>\n<!-- /wp:paragraph -->"
        );
    }

    #[test]
    fn fenced_columns_block_becomes_wp_columns() {
        let out = markdown_to_gutenberg("```columns\nColumn A text.\n+++\nColumn B text.\n```");
        assert_eq!(
            out,
            "<!-- wp:columns -->\n<div class=\"wp-block-columns\">\n\
             <!-- wp:column -->\n<div class=\"wp-block-column\"><!-- wp:paragraph -->\n<p>Column A text.</p>\n<!-- /wp:paragraph --></div>\n<!-- /wp:column -->\n\n\
             <!-- wp:column -->\n<div class=\"wp-block-column\"><!-- wp:paragraph -->\n<p>Column B text.</p>\n<!-- /wp:paragraph --></div>\n<!-- /wp:column -->\n\
             </div>\n<!-- /wp:columns -->"
        );
    }

    #[test]
    fn fenced_buttons_block_becomes_wp_buttons() {
        let out = markdown_to_gutenberg("```buttons\n[Get Started](https://example.com/start)\n[Learn More](https://example.com/more)\n```");
        assert_eq!(
            out,
            "<!-- wp:buttons -->\n<div class=\"wp-block-buttons\">\n\
             <!-- wp:button -->\n<div class=\"wp-block-button\"><a class=\"wp-block-button__link wp-element-button\" href=\"https://example.com/start\">Get Started</a></div>\n<!-- /wp:button -->\n\n\
             <!-- wp:button -->\n<div class=\"wp-block-button\"><a class=\"wp-block-button__link wp-element-button\" href=\"https://example.com/more\">Learn More</a></div>\n<!-- /wp:button -->\n\
             </div>\n<!-- /wp:buttons -->"
        );
    }

    #[test]
    fn fenced_gallery_block_becomes_wp_gallery() {
        let out = markdown_to_gutenberg("```gallery\n![First](one.jpg)\n![Second](two.jpg)\n```");
        assert_eq!(
            out,
            "<!-- wp:gallery {\"linkTo\":\"none\"} -->\n<figure class=\"wp-block-gallery has-nested-images columns-default is-cropped\">\n\
             <!-- wp:image {\"sizeSlug\":\"large\",\"linkDestination\":\"none\"} -->\n<figure class=\"wp-block-image size-large\"><img src=\"one.jpg\" alt=\"First\"/></figure>\n<!-- /wp:image -->\n\n\
             <!-- wp:image {\"sizeSlug\":\"large\",\"linkDestination\":\"none\"} -->\n<figure class=\"wp-block-image size-large\"><img src=\"two.jpg\" alt=\"Second\"/></figure>\n<!-- /wp:image -->\n\
             </figure>\n<!-- /wp:gallery -->"
        );
    }

    #[test]
    fn fenced_gallery_with_custom_settings_becomes_wp_gallery_with_matching_attrs() {
        let out = markdown_to_gutenberg("```gallery\ncolumns=4 crop=false link=media size=full\n+++\n![First](one.jpg)\n```");
        assert_eq!(
            out,
            "<!-- wp:gallery {\"linkTo\":\"media\",\"columns\":4,\"imageCrop\":false} -->\n<figure class=\"wp-block-gallery has-nested-images columns-4\">\n\
             <!-- wp:image {\"sizeSlug\":\"full\",\"linkDestination\":\"media\"} -->\n<figure class=\"wp-block-image size-full\"><a href=\"one.jpg\"><img src=\"one.jpg\" alt=\"First\"/></a></figure>\n<!-- /wp:image -->\n\
             </figure>\n<!-- /wp:gallery -->"
        );
    }

    #[test]
    fn parse_gallery_settings_line_ignores_unknown_keys_and_bad_values() {
        let settings = parse_gallery_settings_line("columns=oops crop=false bogus=1 size=full");
        assert_eq!(settings.columns, None);
        assert!(!settings.cropped);
        assert_eq!(settings.link_to, "none");
        assert_eq!(settings.size_slug, "full");
    }

    #[test]
    fn parse_gallery_settings_line_on_an_empty_string_is_the_default() {
        assert_eq!(parse_gallery_settings_line(""), GallerySettings::default());
    }

    #[test]
    fn fenced_pullquote_block_becomes_wp_pullquote_with_citation() {
        let out = markdown_to_gutenberg("```pullquote\nA striking quote.\n+++\nJane Doe\n```");
        assert_eq!(
            out,
            "<!-- wp:pullquote -->\n<figure class=\"wp-block-pullquote\"><blockquote>\
             <p>A striking quote.</p><cite>Jane Doe</cite></blockquote></figure>\n<!-- /wp:pullquote -->"
        );
    }

    #[test]
    fn fenced_pullquote_block_without_citation_omits_cite_tag() {
        let out = markdown_to_gutenberg("```pullquote\nNo attribution here.\n```");
        assert_eq!(
            out,
            "<!-- wp:pullquote -->\n<figure class=\"wp-block-pullquote\"><blockquote>\
             <p>No attribution here.</p></blockquote></figure>\n<!-- /wp:pullquote -->"
        );
    }

    #[test]
    fn fenced_details_block_becomes_wp_details() {
        let out = markdown_to_gutenberg("```details\nWie funktioniert das?\n+++\nSo funktioniert das.\n```");
        assert_eq!(
            out,
            "<!-- wp:details -->\n<details class=\"wp-block-details\"><summary>Wie funktioniert das?</summary>\n\
             <!-- wp:paragraph -->\n<p>So funktioniert das.</p>\n<!-- /wp:paragraph --></details>\n<!-- /wp:details -->"
        );
    }

    #[test]
    fn multiple_blocks_are_joined_with_blank_line() {
        let out = markdown_to_gutenberg("# Title\n\nSome text.\n");
        assert_eq!(
            out,
            "<!-- wp:heading {\"level\":1} -->\n<h1>Title</h1>\n<!-- /wp:heading -->\n\n\
             <!-- wp:paragraph -->\n<p>Some text.</p>\n<!-- /wp:paragraph -->"
        );
    }
}
