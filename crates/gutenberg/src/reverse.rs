//! Converts WordPress Gutenberg block-comment HTML back into Markdown - the
//! reverse of [`crate::markdown_to_gutenberg`] - so an existing WordPress
//! article can be pulled into Blocksatz, edited as Markdown, and pushed
//! back. Parses into the same [`Block`] tree the forward direction uses,
//! then renders that tree as Markdown text instead of Gutenberg HTML.
//!
//! Deliberately a hand-rolled comment/tag scanner, not a general HTML
//! parser: Gutenberg's block-comment grammar is simple and well-defined
//! (`<!-- wp:name {attrs}? -->...<!-- /wp:name -->` or a self-closing
//! `<!-- wp:name /-->`), and the HTML *inside* each block is exactly what
//! WordPress's own block library renders for that block type. Round-trip
//! fidelity is solid for content this crate produced (or plain, standard
//! Gutenberg blocks); anything unrecognized (a third-party plugin block, a
//! Synced Pattern reference, Page Break, Query Loop, ...) is kept verbatim,
//! full original block comment included, rather than silently losing
//! content - see `make_block`'s fallback arm.

use crate::{Block, ButtonItem, ColumnAlignment, GalleryImage, GallerySettings};

pub fn gutenberg_to_markdown(html: &str) -> String {
    render_markdown(&parse_gutenberg_blocks(html))
}

// ---------------------------------------------------------------------
// Comment scanning
// ---------------------------------------------------------------------

struct ParsedComment<'a> {
    closing: bool,
    name: &'a str,
    attrs: Option<&'a str>,
    self_closing: bool,
}

/// Finds the next HTML comment at or after `from`, returning its trimmed
/// inner text plus the byte range of the whole `<!-- ... -->` comment.
fn next_comment(html: &str, from: usize) -> Option<(&str, usize, usize)> {
    let start = html[from..].find("<!--")? + from;
    let inner_start = start + 4;
    let end_rel = html[inner_start..].find("-->")?;
    let inner_end = inner_start + end_rel;
    Some((html[inner_start..inner_end].trim(), start, inner_end + 3))
}

fn parse_wp_comment(inner: &str) -> Option<ParsedComment<'_>> {
    let closing = inner.starts_with('/');
    let body = if closing { &inner[1..] } else { inner };
    let body = body.strip_prefix("wp:")?;
    let self_closing = body.trim_end().ends_with('/');
    let body = if self_closing { body.trim_end().trim_end_matches('/').trim_end() } else { body };
    let (name, attrs) = match body.find(char::is_whitespace) {
        Some(idx) => (&body[..idx], Some(body[idx..].trim())),
        None => (body, None),
    };
    Some(ParsedComment { closing, name, attrs, self_closing })
}

fn push_stray(blocks: &mut Vec<Block>, html: &str) {
    let trimmed = html.trim();
    if !trimmed.is_empty() {
        blocks.push(Block::RawHtml { html: trimmed.to_string() });
    }
}

/// Depth-aware search (nested blocks of the *same* name, e.g. `wp:list`
/// inside `wp:list`, are common) for the closing comment matching the
/// opening block whose content starts at `start`.
fn find_block_end(html: &str, start: usize, name: &str) -> Option<(usize, usize)> {
    let mut depth = 1i32;
    let mut pos = start;
    loop {
        let (inner, cstart, cend) = next_comment(html, pos)?;
        if let Some(p) = parse_wp_comment(inner) {
            if p.name == name && !p.self_closing {
                if p.closing {
                    depth -= 1;
                    if depth == 0 {
                        return Some((cstart, cend));
                    }
                } else {
                    depth += 1;
                }
            }
        }
        pos = cend;
    }
}

fn parse_gutenberg_blocks(html: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut pos = 0;
    while pos < html.len() {
        let Some((inner, cstart, cend)) = next_comment(html, pos) else {
            push_stray(&mut blocks, &html[pos..]);
            break;
        };
        let Some(parsed) = parse_wp_comment(inner) else {
            pos = cend;
            continue;
        };
        if parsed.closing {
            pos = cend;
            continue;
        }
        if cstart > pos {
            push_stray(&mut blocks, &html[pos..cstart]);
        }
        if parsed.self_closing {
            blocks.push(make_block(parsed.name, parsed.attrs, "", &html[cstart..cend]));
            pos = cend;
            continue;
        }
        match find_block_end(html, cend, parsed.name) {
            Some((inner_end, after)) => {
                blocks.push(make_block(parsed.name, parsed.attrs, &html[cend..inner_end], &html[cstart..after]));
                pos = after;
            }
            None => {
                blocks.push(make_block(parsed.name, parsed.attrs, &html[cend..], &html[cstart..]));
                pos = html.len();
            }
        }
    }
    blocks
}

/// `raw` is the whole original block-comment span (opening comment through
/// closing comment, or the single comment for a self-closing block) -
/// unused by every recognized block type below, which only need `inner`,
/// but load-bearing for the `_` fallback: a *self-closing* unrecognized
/// block (a Synced Pattern reference, a Page Break, ...) has no `inner` at
/// all, so falling back to `inner` there would silently turn it into an
/// empty block - see the fallback arm below and `render_block` in `lib.rs`.
fn make_block(name: &str, attrs: Option<&str>, inner: &str, raw: &str) -> Block {
    match name {
        "paragraph" => Block::Paragraph {
            html: inline_html_to_markdown(&strip_wrapper_tag(inner, "p")),
        },
        "heading" => {
            let level = detect_heading_level(inner).unwrap_or(2);
            let html = inline_html_to_markdown(&strip_wrapper_tag(inner, &format!("h{level}")));
            Block::Heading { level, html }
        }
        "list" => {
            let ordered = attr_flag(attrs, "ordered");
            let list_tag = if ordered { "ol" } else { "ul" };
            let items = parse_list_items(&strip_wrapper_tag(inner, list_tag));
            Block::List { ordered, items }
        }
        "quote" => Block::BlockQuote {
            blocks: parse_gutenberg_blocks(&strip_wrapper_tag(inner, "blockquote")),
        },
        "code" => {
            let code = strip_wrapper_tag(&strip_wrapper_tag(inner, "pre"), "code");
            Block::CodeBlock {
                lang: None,
                text: unescape_entities(code.trim()),
            }
        }
        "image" => Block::Image {
            url: extract_attr(inner, "src").unwrap_or_default(),
            alt: extract_attr(inner, "alt").unwrap_or_default(),
            title: extract_attr(inner, "title").filter(|t| !t.is_empty()),
            // Plain Markdown has no slot to carry these through a
            // round-trip (see the field's own doc comment in `lib.rs`) -
            // re-acquired fresh from `Frontmatter.media` on the next
            // export instead, the same as importing never recovers a
            // body image's own WordPress upload state in general.
            media_id: None,
            width: 0,
            height: 0,
        },
        "separator" => Block::ThematicBreak,
        "video" => Block::Video { url: extract_attr(inner, "src").unwrap_or_default() },
        "audio" => Block::Audio { url: extract_attr(inner, "src").unwrap_or_default() },
        "embed" => Block::Embed { url: extract_json_string(attrs, "url").unwrap_or_default() },
        "table" => parse_table_block(inner),
        "columns" => Block::Columns {
            columns: parse_columns(&strip_wrapper_tag(inner, "div")),
        },
        "buttons" => Block::Buttons {
            buttons: parse_buttons(&strip_wrapper_tag(inner, "div")),
        },
        "gallery" => {
            let (images, size_slug) = parse_gallery(&strip_wrapper_tag(inner, "figure"));
            Block::Gallery {
                images,
                settings: GallerySettings {
                    columns: extract_json_number(attrs, "columns"),
                    cropped: !attr_is_false(attrs, "imageCrop"),
                    link_to: extract_json_string(attrs, "linkTo").unwrap_or_else(|| "none".to_string()),
                    size_slug: size_slug.unwrap_or_else(|| "large".to_string()),
                },
            }
        }
        "pullquote" => parse_pullquote_block(inner),
        "details" => parse_details_block(inner),
        // Our own "html" passthrough, and WordPress's own unusual "more"
        // block (whose inner content already just *is* the bare
        // `<!--more-->` marker - see the matching arm in `lib.rs`'s
        // `render_block`) both keep only their inner content, same as
        // before - `inner` is exactly what's wanted for the Markdown these
        // two produce, and neither is ever self-closing (an empty `raw`
        // fallback risk, see below, doesn't apply to either).
        "html" | "more" => Block::RawHtml { html: inner.trim().to_string() },
        // Any other unrecognized block type (third-party plugin blocks,
        // Synced Patterns, Page Break, Query Loop, ...) keeps its ENTIRE
        // original `<!-- wp:name {...} -->...<!-- /wp:name -->` markup
        // verbatim instead - many of these are dynamic/server-rendered
        // blocks whose raw markup is self-closing with no inner HTML at
        // all, so keeping only `inner` would silently discard them outright
        // (the block simply vanishes on save, with no attrs left behind to
        // even show what was lost). `render_block` in `lib.rs` recognizes
        // this verbatim form and re-emits it unchanged instead of
        // re-wrapping it as a generic (and, for these blocks, likely
        // non-functional) Custom HTML block.
        _ => Block::RawHtml { html: raw.trim().to_string() },
    }
}

fn attr_flag(attrs: Option<&str>, key: &str) -> bool {
    attrs.is_some_and(|a| a.contains(&format!("\"{key}\":true")))
}

/// `attr_flag`'s inverse for a boolean that defaults to `true` - WordPress's
/// `imageCrop` gallery attribute, whose absence means "still true", not
/// "unset".
fn attr_is_false(attrs: Option<&str>, key: &str) -> bool {
    attrs.is_some_and(|a| a.contains(&format!("\"{key}\":false")))
}

/// Reads a bare (unquoted) numeric JSON attr, e.g. `wp:gallery`'s own
/// `columns` - the numeric-value equivalent of `extract_json_string`.
fn extract_json_number(attrs: Option<&str>, key: &str) -> Option<u8> {
    let attrs = attrs?;
    let needle = format!("\"{key}\":");
    let start = attrs.find(&needle)? + needle.len();
    let digits: String = attrs[start..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Reads a `"key":"value"` string out of a block's JSON attrs comment -
/// this crate's block-comment scanner is a hand-rolled scanner rather than
/// a full HTML parser (see the module doc comment above), and this is the
/// JSON-attrs equivalent of `extract_attr`'s HTML-tag-attribute scanning.
fn extract_json_string(attrs: Option<&str>, key: &str) -> Option<String> {
    let attrs = attrs?;
    let needle = format!("\"{key}\":\"");
    let start = attrs.find(&needle)? + needle.len();
    let end = attrs[start..].find('"')? + start;
    Some(attrs[start..end].replace("\\\"", "\"").replace("\\\\", "\\"))
}

fn detect_heading_level(inner: &str) -> Option<u8> {
    let trimmed = inner.trim_start();
    (1..=6u8).find(|level| trimmed.starts_with(&format!("<h{level}")))
}

/// Strips a leading `<tag ...>` and trailing `</tag>` wrapper, returning
/// what's between. Falls back to the input unchanged if `tag` isn't found,
/// so malformed input degrades gracefully instead of panicking or vanishing.
fn strip_wrapper_tag(html: &str, tag: &str) -> String {
    let html = html.trim();
    let open_prefix = format!("<{tag}");
    if !html.starts_with(&open_prefix) {
        return html.to_string();
    }
    let Some(gt) = html.find('>') else {
        return html.to_string();
    };
    let close_tag = format!("</{tag}>");
    match html.rfind(&close_tag) {
        Some(close_pos) if close_pos >= gt => html[gt + 1..close_pos].to_string(),
        _ => html[gt + 1..].to_string(),
    }
}

fn parse_list_items(list_inner: &str) -> Vec<Vec<Block>> {
    let mut items = Vec::new();
    let mut pos = 0;
    while pos < list_inner.len() {
        let Some((inner, _cstart, cend)) = next_comment(list_inner, pos) else {
            break;
        };
        let Some(parsed) = parse_wp_comment(inner) else {
            pos = cend;
            continue;
        };
        if parsed.closing || parsed.name != "list-item" {
            pos = cend;
            continue;
        }
        if parsed.self_closing {
            items.push(vec![Block::Paragraph { html: String::new() }]);
            pos = cend;
            continue;
        }
        match find_block_end(list_inner, cend, "list-item") {
            Some((inner_end, after)) => {
                items.push(parse_list_item_content(&list_inner[cend..inner_end]));
                pos = after;
            }
            None => {
                items.push(parse_list_item_content(&list_inner[cend..]));
                pos = list_inner.len();
            }
        }
    }
    items
}

/// A list item's own text comes first (as a `Paragraph`), followed by any
/// nested list found inside the same `<li>` - mirroring exactly how the
/// forward direction structures a list item's `Vec<Block>`.
fn parse_list_item_content(li_wrapped: &str) -> Vec<Block> {
    let li_inner = strip_wrapper_tag(li_wrapped, "li");
    if let Some((inner, cstart, _cend)) = next_comment(&li_inner, 0) {
        if let Some(parsed) = parse_wp_comment(inner) {
            if !parsed.closing && parsed.name == "list" {
                let text = li_inner[..cstart].trim();
                let mut result = vec![Block::Paragraph {
                    html: inline_html_to_markdown(text),
                }];
                result.extend(parse_gutenberg_blocks(&li_inner[cstart..]));
                return result;
            }
        }
    }
    vec![Block::Paragraph {
        html: inline_html_to_markdown(li_inner.trim()),
    }]
}

/// Scans a `wp:columns` block's stripped `<div>` content for its `wp:column`
/// children - same depth-aware comment-scanning structure as
/// `parse_list_items`, just one nesting level shallower since a column has
/// no sibling-name ambiguity to worry about.
fn parse_columns(columns_inner: &str) -> Vec<Vec<Block>> {
    let mut columns = Vec::new();
    let mut pos = 0;
    while pos < columns_inner.len() {
        let Some((inner, _cstart, cend)) = next_comment(columns_inner, pos) else {
            break;
        };
        let Some(parsed) = parse_wp_comment(inner) else {
            pos = cend;
            continue;
        };
        if parsed.closing || parsed.name != "column" {
            pos = cend;
            continue;
        }
        match find_block_end(columns_inner, cend, "column") {
            Some((inner_end, after)) => {
                columns.push(parse_gutenberg_blocks(&strip_wrapper_tag(&columns_inner[cend..inner_end], "div")));
                pos = after;
            }
            None => {
                columns.push(parse_gutenberg_blocks(&strip_wrapper_tag(&columns_inner[cend..], "div")));
                pos = columns_inner.len();
            }
        }
    }
    columns
}

/// Scans a `wp:buttons` block's stripped `<div>` content for its `wp:button`
/// children, reading each one's `<a href>`/link text directly rather than
/// going through `make_block` - a button's inner HTML is a single `<a>`
/// element, not a nested block tree.
fn parse_buttons(buttons_inner: &str) -> Vec<ButtonItem> {
    let mut buttons = Vec::new();
    let mut pos = 0;
    while pos < buttons_inner.len() {
        let Some((inner, _cstart, cend)) = next_comment(buttons_inner, pos) else {
            break;
        };
        let Some(parsed) = parse_wp_comment(inner) else {
            pos = cend;
            continue;
        };
        if parsed.closing || parsed.name != "button" {
            pos = cend;
            continue;
        }
        match find_block_end(buttons_inner, cend, "button") {
            Some((inner_end, after)) => {
                buttons.push(button_from_html(&buttons_inner[cend..inner_end]));
                pos = after;
            }
            None => {
                buttons.push(button_from_html(&buttons_inner[cend..]));
                pos = buttons_inner.len();
            }
        }
    }
    buttons
}

fn button_from_html(html: &str) -> ButtonItem {
    let url = extract_attr(html, "href").unwrap_or_default();
    let text = html
        .find("<a")
        .and_then(|a_start| html[a_start..].find('>').map(|gt| a_start + gt + 1))
        .and_then(|text_start| html[text_start..].find("</a>").map(|rel_end| &html[text_start..text_start + rel_end]))
        .map(|t| unescape_entities(t.trim()))
        .unwrap_or_default();
    ButtonItem { text, url }
}

/// Scans a `wp:gallery` block's stripped `<figure>` content for its
/// `wp:image` children, reading each one's `src`/`alt` directly - same
/// reasoning as `parse_buttons` above.
/// Also returns the first image's own `sizeSlug` - WordPress sets it
/// per-image, but this crate's simpler data model keeps a single size for
/// the whole gallery (see `GallerySettings::size_slug`), so only the first
/// one actually needs reading.
fn parse_gallery(gallery_inner: &str) -> (Vec<GalleryImage>, Option<String>) {
    let mut images = Vec::new();
    let mut size_slug = None;
    let mut pos = 0;
    while pos < gallery_inner.len() {
        let Some((inner, _cstart, cend)) = next_comment(gallery_inner, pos) else {
            break;
        };
        let Some(parsed) = parse_wp_comment(inner) else {
            pos = cend;
            continue;
        };
        if parsed.closing || parsed.name != "image" {
            pos = cend;
            continue;
        }
        if size_slug.is_none() {
            size_slug = extract_json_string(parsed.attrs, "sizeSlug");
        }
        match find_block_end(gallery_inner, cend, "image") {
            Some((inner_end, after)) => {
                images.push(image_from_html(&gallery_inner[cend..inner_end]));
                pos = after;
            }
            None => {
                images.push(image_from_html(&gallery_inner[cend..]));
                pos = gallery_inner.len();
            }
        }
    }
    (images, size_slug)
}

fn image_from_html(html: &str) -> GalleryImage {
    let caption = html
        .find("<figcaption")
        .and_then(|idx| extract_between(&html[idx..], ">", "</figcaption>"))
        .map(|c| unescape_entities(c.trim()))
        .filter(|c| !c.is_empty());
    GalleryImage {
        url: extract_attr(html, "src").unwrap_or_default(),
        alt: extract_attr(html, "alt").unwrap_or_default(),
        caption,
    }
}

/// Reads a `wp:pullquote` block's `<figure><blockquote>` inner HTML back
/// into paragraphs + an optional citation - the reverse of `render_pullquote`.
/// Handles both this crate's own output (a bare `<p>` per paragraph) and a
/// real WordPress-authored pullquote (same markup), splitting off the
/// trailing `<cite>` before reading the paragraphs.
fn parse_pullquote_block(inner: &str) -> Block {
    let blockquote_inner = strip_wrapper_tag(&strip_wrapper_tag(inner, "figure"), "blockquote");
    let (quote_html, citation) = match blockquote_inner.find("<cite") {
        Some(idx) => {
            let citation = extract_between(&blockquote_inner[idx..], ">", "</cite>").map(|c| unescape_entities(c.trim())).filter(|c| !c.is_empty());
            (blockquote_inner[..idx].to_string(), citation)
        }
        None => (blockquote_inner, None),
    };
    let tagged_paragraphs = extract_all_tags(&quote_html, "p");
    let paragraphs = if tagged_paragraphs.is_empty() {
        vec![inline_html_to_markdown(quote_html.trim())]
    } else {
        tagged_paragraphs.into_iter().map(|(_, content)| inline_html_to_markdown(content)).collect()
    };
    Block::Pullquote { paragraphs, citation }
}

/// Reads a `wp:details` block's `<details><summary>...</summary>...` inner
/// HTML back into a summary + body block tree - the reverse of
/// `render_details`.
fn parse_details_block(inner: &str) -> Block {
    let details_inner = strip_wrapper_tag(inner, "details");
    match details_inner.find("</summary>") {
        Some(close_rel) => {
            let summary = extract_between(&details_inner, "<summary>", "</summary>").map(inline_html_to_markdown).unwrap_or_default();
            let body = &details_inner[close_rel + "</summary>".len()..];
            Block::Details { summary, blocks: parse_gutenberg_blocks(body) }
        }
        None => Block::Details { summary: String::new(), blocks: parse_gutenberg_blocks(&details_inner) },
    }
}

fn parse_table_block(inner: &str) -> Block {
    let table_inner = strip_wrapper_tag(inner, "table");
    let mut alignments = Vec::new();
    let mut header = Vec::new();
    if let Some(thead) = extract_between(&table_inner, "<thead>", "</thead>") {
        if let Some(row) = extract_between(thead, "<tr>", "</tr>") {
            for cell_tag in extract_all_tags(row, "th") {
                alignments.push(alignment_from_style(cell_tag.0));
                header.push(inline_html_to_markdown(cell_tag.1));
            }
        }
    }
    let tbody = extract_between(&table_inner, "<tbody>", "</tbody>").unwrap_or(table_inner.as_str());
    let mut rows = Vec::new();
    for row in extract_all_between(tbody, "<tr>", "</tr>") {
        let cells = extract_all_tags(row, "td").into_iter().map(|(_, content)| inline_html_to_markdown(content)).collect();
        rows.push(cells);
    }
    Block::Table { alignments, header, rows }
}

fn alignment_from_style(open_tag: &str) -> ColumnAlignment {
    if open_tag.contains("text-align:left") {
        ColumnAlignment::Left
    } else if open_tag.contains("text-align:center") {
        ColumnAlignment::Center
    } else if open_tag.contains("text-align:right") {
        ColumnAlignment::Right
    } else {
        ColumnAlignment::None
    }
}

/// Finds the first `<start>...<end>` span and returns what's between.
fn extract_between<'a>(html: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let s = html.find(start)? + start.len();
    let e = html[s..].find(end)? + s;
    Some(&html[s..e])
}

/// Finds every `<start>...<end>` span (non-overlapping, in order).
fn extract_all_between<'a>(html: &'a str, start: &str, end: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(s_rel) = html[pos..].find(start) {
        let s = pos + s_rel + start.len();
        let Some(e_rel) = html[s..].find(end) else { break };
        let e = s + e_rel;
        out.push(&html[s..e]);
        pos = e + end.len();
    }
    out
}

/// Finds every `<tagname ...>content</tagname>` element, returning each
/// element's opening tag (for reading attributes like `style=`) alongside
/// its inner content.
fn extract_all_tags<'a>(html: &'a str, tag: &str) -> Vec<(&'a str, &'a str)> {
    let mut out = Vec::new();
    let open_prefix = format!("<{tag}");
    let close_tag = format!("</{tag}>");
    let mut pos = 0;
    while let Some(s_rel) = html[pos..].find(&open_prefix) {
        let tag_start = pos + s_rel;
        let Some(gt_rel) = html[tag_start..].find('>') else { break };
        let content_start = tag_start + gt_rel + 1;
        let Some(e_rel) = html[content_start..].find(&close_tag) else { break };
        let content_end = content_start + e_rel;
        out.push((&html[tag_start..content_start], &html[content_start..content_end]));
        pos = content_end + close_tag.len();
    }
    out
}

fn extract_attr(html: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = html.find(&needle)? + needle.len();
    let end = html[start..].find('"')? + start;
    Some(unescape_entities(&html[start..end]))
}

// ---------------------------------------------------------------------
// Inline HTML -> Markdown
// ---------------------------------------------------------------------

fn inline_html_to_markdown(html: &str) -> String {
    let mut out = String::new();
    let mut link_hrefs: Vec<String> = Vec::new();
    let mut i = 0;
    while i < html.len() {
        if html.as_bytes()[i] == b'<' {
            let Some(rel_end) = html[i..].find('>') else {
                break;
            };
            let tag_content = &html[i + 1..i + rel_end];
            i += rel_end + 1;
            let is_closing = tag_content.starts_with('/');
            let body = tag_content.trim_start_matches('/').trim_end_matches('/');
            let name = body.split_whitespace().next().unwrap_or("").to_lowercase();
            if is_closing {
                match name.as_str() {
                    "strong" | "b" => out.push_str("**"),
                    "em" | "i" => out.push('*'),
                    "code" => out.push('`'),
                    "s" | "del" => out.push_str("~~"),
                    "a" => out.push_str(&format!("]({})", link_hrefs.pop().unwrap_or_default())),
                    _ => {}
                }
            } else {
                match name.as_str() {
                    "strong" | "b" => out.push_str("**"),
                    "em" | "i" => out.push('*'),
                    "code" => out.push('`'),
                    "s" | "del" => out.push_str("~~"),
                    "br" => out.push('\n'),
                    "a" => {
                        link_hrefs.push(extract_attr(body, "href").unwrap_or_default());
                        out.push('[');
                    }
                    _ => {}
                }
            }
        } else {
            let next = html[i..].find('<').map(|p| i + p).unwrap_or(html.len());
            out.push_str(&unescape_entities(&html[i..next]));
            i = next;
        }
    }
    out.trim().to_string()
}

fn unescape_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&apos;", "'")
}

// ---------------------------------------------------------------------
// Block tree -> Markdown
// ---------------------------------------------------------------------

fn render_markdown(blocks: &[Block]) -> String {
    blocks.iter().map(render_block_markdown).collect::<Vec<_>>().join("\n\n")
}

fn render_block_markdown(block: &Block) -> String {
    match block {
        Block::Paragraph { html } => html.clone(),
        Block::Heading { level, html } => format!("{} {html}", "#".repeat(*level as usize)),
        Block::List { ordered, items } => render_list_markdown(*ordered, items, 0),
        Block::BlockQuote { blocks } => render_markdown(blocks)
            .lines()
            .map(|line| if line.is_empty() { ">".to_string() } else { format!("> {line}") })
            .collect::<Vec<_>>()
            .join("\n"),
        Block::CodeBlock { lang, text } => format!("```{}\n{text}\n```", lang.clone().unwrap_or_default()),
        Block::Image { url, alt, title, .. } => {
            let destination = markdown_destination(url);
            match title {
                Some(title) => format!("![{alt}]({destination} \"{title}\")"),
                None => format!("![{alt}]({destination})"),
            }
        }
        Block::Video { url } => format!("![]({})", markdown_destination(url)),
        Block::Audio { url } => format!("![]({})", markdown_destination(url)),
        Block::Embed { url } => url.clone(),
        Block::ThematicBreak => "---".to_string(),
        Block::Table { alignments, header, rows } => render_table_markdown(alignments, header, rows),
        Block::Columns { columns } => render_columns_markdown(columns),
        Block::Buttons { buttons } => render_buttons_markdown(buttons),
        Block::Gallery { images, settings } => render_gallery_fence(images, settings),
        Block::Pullquote { paragraphs, citation } => render_pullquote_markdown(paragraphs, citation),
        Block::Details { summary, blocks } => render_details_markdown(summary, blocks),
        Block::RawHtml { html } => html.clone(),
    }
}

/// CommonMark's plain `(destination)` link/image syntax breaks on raw
/// whitespace or unbalanced parentheses (the parser stops at the first
/// unescaped one, so `![alt](my photo.png)` isn't recognized as an image at
/// all) - wrapping the destination in `<...>` is also valid CommonMark and
/// is stripped back off by any compliant parser, so it round-trips a
/// space-containing local path or URL without needing to percent-encode or
/// otherwise alter it.
fn markdown_destination(url: &str) -> String {
    if url.chars().any(char::is_whitespace) || url.contains('(') || url.contains(')') {
        format!("<{url}>")
    } else {
        url.to_string()
    }
}

fn render_list_markdown(ordered: bool, items: &[Vec<Block>], indent: usize) -> String {
    let pad = " ".repeat(indent);
    items
        .iter()
        .enumerate()
        .map(|(idx, item_blocks)| {
            let marker = if ordered { format!("{}.", idx + 1) } else { "-".to_string() };
            let mut lines = Vec::new();
            for (i, block) in item_blocks.iter().enumerate() {
                if i == 0 {
                    lines.push(format!("{pad}{marker} {}", render_block_markdown(block)));
                } else if let Block::List { ordered: nested_ordered, items: nested_items } = block {
                    lines.push(render_list_markdown(*nested_ordered, nested_items, indent + 2));
                } else {
                    lines.push(render_block_markdown(block));
                }
            }
            lines.join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The `+++`-separator inverse of `parse_fenced_columns` - each column's own
/// blocks are rendered as ordinary Markdown, then joined back together on
/// that same separator line.
fn render_columns_markdown(columns: &[Vec<Block>]) -> String {
    let body = columns.iter().map(|col| render_markdown(col)).collect::<Vec<_>>().join("\n+++\n");
    format!("```columns\n{body}\n```")
}

fn render_buttons_markdown(buttons: &[ButtonItem]) -> String {
    let body = buttons.iter().map(|b| format!("[{}]({})", b.text, markdown_destination(&b.url))).collect::<Vec<_>>().join("\n");
    format!("```buttons\n{body}\n```")
}

/// Builds a ` ```gallery ``` ` fenced block's text from scratch - public so
/// `gallerydialog.rs` can generate one directly from what the user picked,
/// not just as this module's own reverse-HTML-to-Markdown step.
pub fn render_gallery_fence(images: &[GalleryImage], settings: &GallerySettings) -> String {
    let body = images
        .iter()
        .map(|img| {
            let destination = markdown_destination(&img.url);
            match img.caption.as_ref().filter(|c| !c.is_empty()) {
                Some(caption) => format!("![{}]({destination} \"{caption}\")", img.alt),
                None => format!("![{}]({destination})", img.alt),
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    match format_gallery_settings_line(settings) {
        Some(line) => format!("```gallery\n{line}\n+++\n{body}\n```"),
        None => format!("```gallery\n{body}\n```"),
    }
}

/// The inverse of `parse_gallery_settings_line` - `None` when `settings` is
/// entirely at its default, so a plain gallery still round-trips to the
/// exact same clean, options-line-free Markdown it always has.
fn format_gallery_settings_line(settings: &GallerySettings) -> Option<String> {
    if *settings == GallerySettings::default() {
        return None;
    }
    let mut parts = Vec::new();
    if let Some(columns) = settings.columns {
        parts.push(format!("columns={columns}"));
    }
    if !settings.cropped {
        parts.push("crop=false".to_string());
    }
    if settings.link_to != "none" {
        parts.push(format!("link={}", settings.link_to));
    }
    if settings.size_slug != "large" {
        parts.push(format!("size={}", settings.size_slug));
    }
    Some(parts.join(" "))
}

/// The `+++`-separator inverse of `parse_fenced_pullquote` - see
/// `render_columns_markdown` for the same convention applied to columns.
fn render_pullquote_markdown(paragraphs: &[String], citation: &Option<String>) -> String {
    let mut body = paragraphs.join("\n\n");
    if let Some(citation) = citation.as_ref().filter(|c| !c.is_empty()) {
        body.push_str("\n+++\n");
        body.push_str(citation);
    }
    format!("```pullquote\n{body}\n```")
}

/// The `+++`-separator inverse of `parse_fenced_details`.
fn render_details_markdown(summary: &str, blocks: &[Block]) -> String {
    format!("```details\n{summary}\n+++\n{}\n```", render_markdown(blocks))
}

fn render_table_markdown(alignments: &[ColumnAlignment], header: &[String], rows: &[Vec<String>]) -> String {
    let mut lines = Vec::new();
    if !header.is_empty() {
        lines.push(format!("| {} |", header.join(" | ")));
        let seps: Vec<String> = (0..header.len())
            .map(|i| match alignments.get(i) {
                Some(ColumnAlignment::Left) => ":---".to_string(),
                Some(ColumnAlignment::Center) => ":---:".to_string(),
                Some(ColumnAlignment::Right) => "---:".to_string(),
                _ => "---".to_string(),
            })
            .collect();
        lines.push(format!("| {} |", seps.join(" | ")));
    }
    for row in rows {
        lines.push(format!("| {} |", row.join(" | ")));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown_to_gutenberg;

    fn round_trip(markdown: &str) -> String {
        gutenberg_to_markdown(&markdown_to_gutenberg(markdown))
    }

    #[test]
    fn paragraph_round_trips() {
        assert_eq!(round_trip("Hello **world**, this is *fun*."), "Hello **world**, this is *fun*.");
    }

    #[test]
    fn heading_levels_round_trip() {
        assert_eq!(round_trip("## Title"), "## Title");
        assert_eq!(round_trip("### Sub"), "### Sub");
    }

    #[test]
    fn unordered_list_round_trips() {
        assert_eq!(round_trip("- one\n- two\n"), "- one\n- two");
    }

    #[test]
    fn ordered_list_round_trips() {
        assert_eq!(round_trip("1. first\n2. second\n"), "1. first\n2. second");
    }

    #[test]
    fn nested_list_round_trips() {
        assert_eq!(round_trip("- a\n  - nested\n- b\n"), "- a\n  - nested\n- b");
    }

    #[test]
    fn blockquote_round_trips() {
        assert_eq!(round_trip("> quoted text"), "> quoted text");
    }

    #[test]
    fn code_block_round_trips() {
        assert_eq!(round_trip("```\nlet x = 1;\n```"), "```\nlet x = 1;\n```");
    }

    #[test]
    fn image_round_trips() {
        assert_eq!(
            round_trip("![a cat](https://example.com/cat.png)"),
            "![a cat](https://example.com/cat.png)"
        );
    }

    #[test]
    fn image_with_space_in_url_is_wrapped_in_angle_brackets() {
        let block = Block::Image { url: "my cat.png".to_string(), alt: "a cat".to_string(), title: None, media_id: None, width: 0, height: 0 };
        assert_eq!(render_block_markdown(&block), "![a cat](<my cat.png>)");
    }

    #[test]
    fn image_with_space_in_url_round_trips_via_angle_brackets() {
        assert_eq!(round_trip("![a cat](<my cat.png>)"), "![a cat](<my cat.png>)");
    }

    #[test]
    fn thematic_break_round_trips() {
        assert_eq!(round_trip("---"), "---");
    }

    #[test]
    fn table_round_trips() {
        let out = round_trip("| A | B |\n|---|---|\n| 1 | 2 |\n");
        assert_eq!(out, "| A | B |\n| --- | --- |\n| 1 | 2 |");
    }

    #[test]
    fn raw_html_round_trips() {
        assert_eq!(round_trip("<div class=\"embed\">hi</div>"), "<div class=\"embed\">hi</div>");
    }

    #[test]
    fn more_marker_round_trips() {
        assert_eq!(round_trip("Erster Absatz.\n\n<!--more-->\n\nZweiter Absatz."), "Erster Absatz.\n\n<!--more-->\n\nZweiter Absatz.");
    }

    #[test]
    fn self_closing_unrecognized_block_keeps_its_full_comment_verbatim() {
        // e.g. a Synced Pattern reference (`core/block`) - dynamic, no
        // inner HTML of its own at all in the raw markup, so losing the
        // wrapper here would lose the whole block outright.
        let html = "<!-- wp:paragraph --><p>Vorher.</p><!-- /wp:paragraph -->\
                     <!-- wp:block {\"ref\":123} /-->\
                     <!-- wp:paragraph --><p>Nachher.</p><!-- /wp:paragraph -->";
        assert_eq!(
            gutenberg_to_markdown(html),
            "Vorher.\n\n<!-- wp:block {\"ref\":123} /-->\n\nNachher."
        );
    }

    #[test]
    fn wrapped_unrecognized_block_keeps_its_full_comment_verbatim() {
        let html = "<!-- wp:my-plugin/thing {\"x\":true} --><div class=\"thing\">Inhalt</div><!-- /wp:my-plugin/thing -->";
        assert_eq!(
            gutenberg_to_markdown(html),
            "<!-- wp:my-plugin/thing {\"x\":true} --><div class=\"thing\">Inhalt</div><!-- /wp:my-plugin/thing -->"
        );
    }

    #[test]
    fn self_closing_unrecognized_block_round_trips_through_markdown_unchanged() {
        let original = "<!-- wp:block {\"ref\":123} /-->";
        let markdown = gutenberg_to_markdown(original);
        assert_eq!(markdown_to_gutenberg(&markdown), original);
    }

    #[test]
    fn video_reference_round_trips() {
        assert_eq!(round_trip("![](clip.mp4)"), "![](clip.mp4)");
    }

    #[test]
    fn audio_reference_round_trips() {
        assert_eq!(round_trip("![](song.mp3)"), "![](song.mp3)");
    }

    #[test]
    fn embed_url_round_trips() {
        assert_eq!(round_trip("https://www.youtube.com/watch?v=dQw4w9WgXcQ"), "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
    }

    #[test]
    fn embed_url_from_an_unknown_provider_round_trips() {
        assert_eq!(round_trip("https://example.com/some-article"), "https://example.com/some-article");
    }

    #[test]
    fn link_round_trips() {
        assert_eq!(round_trip("Check [this](https://example.com) out."), "Check [this](https://example.com) out.");
    }

    #[test]
    fn multiple_blocks_round_trip_with_blank_line_separation() {
        assert_eq!(round_trip("# Title\n\nSome text.\n"), "# Title\n\nSome text.");
    }

    #[test]
    fn fenced_columns_round_trip() {
        assert_eq!(
            round_trip("```columns\nColumn A text.\n+++\nColumn B text.\n```"),
            "```columns\nColumn A text.\n+++\nColumn B text.\n```"
        );
    }

    #[test]
    fn fenced_columns_with_multiple_blocks_per_column_round_trip() {
        assert_eq!(
            round_trip("```columns\n## Left\n\nSome text.\n+++\n## Right\n\nMore text.\n```"),
            "```columns\n## Left\n\nSome text.\n+++\n## Right\n\nMore text.\n```"
        );
    }

    #[test]
    fn fenced_buttons_round_trip() {
        assert_eq!(
            round_trip("```buttons\n[Get Started](https://example.com/start)\n[Learn More](https://example.com/more)\n```"),
            "```buttons\n[Get Started](https://example.com/start)\n[Learn More](https://example.com/more)\n```"
        );
    }

    #[test]
    fn fenced_gallery_round_trip() {
        assert_eq!(
            round_trip("```gallery\n![First](one.jpg)\n![Second](two.jpg)\n```"),
            "```gallery\n![First](one.jpg)\n![Second](two.jpg)\n```"
        );
    }

    #[test]
    fn fenced_gallery_with_captions_round_trips() {
        assert_eq!(
            round_trip("```gallery\n![First](one.jpg \"A caption\")\n![Second](two.jpg)\n```"),
            "```gallery\n![First](one.jpg \"A caption\")\n![Second](two.jpg)\n```"
        );
    }

    #[test]
    fn fenced_gallery_with_settings_round_trips() {
        assert_eq!(
            round_trip("```gallery\ncolumns=4 crop=false link=media size=full\n+++\n![First](one.jpg)\n![Second](two.jpg)\n```"),
            "```gallery\ncolumns=4 crop=false link=media size=full\n+++\n![First](one.jpg)\n![Second](two.jpg)\n```"
        );
    }

    #[test]
    fn fenced_pullquote_round_trips() {
        assert_eq!(
            round_trip("```pullquote\nA striking quote.\n+++\nJane Doe\n```"),
            "```pullquote\nA striking quote.\n+++\nJane Doe\n```"
        );
    }

    #[test]
    fn fenced_pullquote_without_citation_round_trips() {
        assert_eq!(round_trip("```pullquote\nNo attribution here.\n```"), "```pullquote\nNo attribution here.\n```");
    }

    #[test]
    fn fenced_details_round_trips() {
        assert_eq!(
            round_trip("```details\nWie funktioniert das?\n+++\nSo funktioniert das.\n```"),
            "```details\nWie funktioniert das?\n+++\nSo funktioniert das.\n```"
        );
    }

    #[test]
    fn fenced_details_with_an_image_in_the_body_round_trips() {
        assert_eq!(
            round_trip("```details\nGalerie?\n+++\n![a cat](cat.png)\n```"),
            "```details\nGalerie?\n+++\n![a cat](cat.png)\n```"
        );
    }

    #[test]
    fn table_alignment_round_trips() {
        let out = round_trip("| A | B | C |\n|:---|:---:|---:|\n| 1 | 2 | 3 |\n");
        assert_eq!(out, "| A | B | C |\n| :--- | :---: | ---: |\n| 1 | 2 | 3 |");
    }
}
