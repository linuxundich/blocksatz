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

use crate::{fidelity, Block, BlockAttrs, ButtonItem, ColumnAlignment, GalleryImage, GallerySettings};

/// Every `<img>` in a post's block markup that carries its attachment id
/// (`class="wp-image-123"`): `(src, id)`. Markdown has no place for the
/// id, so an opened post keeps it in `Frontmatter.media` instead - without
/// it, the next upload would lose the class WordPress needs for `srcset`.
/// The image size (`large`) comes from the surrounding figure's
/// `size-large` class, when there is one.
pub fn image_media_ids(html: &str) -> Vec<(String, u64, Option<String>)> {
    let mut out: Vec<(String, u64, Option<String>)> = Vec::new();
    let mut offset = 0;
    while let Some(rel) = html[offset..].find("<img ") {
        let start = offset + rel;
        let Some(end) = html[start..].find('>') else { break };
        let tag = &html[start..start + end];
        offset = start + end;
        let (Some(src), Some(class)) = (extract_attr(tag, "src"), extract_attr(tag, "class")) else { continue };
        let id = class.split_whitespace().find_map(|c| c.strip_prefix("wp-image-")).and_then(|n| n.parse().ok());
        let Some(id) = id else { continue };
        let size = html[..start]
            .rfind("<figure")
            .and_then(|figure| extract_attr(&html[figure..start], "class"))
            .and_then(|classes| classes.split_whitespace().find_map(|c| c.strip_prefix("size-").map(str::to_string)));
        let src = unescape_entities(&src);
        if !out.iter().any(|(s, _, _)| *s == src) {
            out.push((src, id, size));
        }
    }
    out
}

pub fn gutenberg_to_markdown(html: &str) -> String {
    render_markdown(&parse_blocks_in(html, true))
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

/// Content between blocks without a block comment of its own is what
/// WordPress shows as a "Klassisch" block (and all of a post written before
/// the block editor). Kept with explicit `wp:freeform` delimiters - which
/// WordPress parses as exactly that block - so it goes back as a classic
/// block instead of becoming a Custom HTML block.
fn push_stray(blocks: &mut Vec<Block>, html: &str) {
    let trimmed = html.trim();
    if !trimmed.is_empty() {
        blocks.push(Block::RawHtml { html: format!("<!-- wp:freeform -->\n{trimmed}\n<!-- /wp:freeform -->") });
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

/// Blocks nested somewhere the Markdown form has no room for an attribute
/// line (a quote, a list item) - an attributed block there stays raw.
fn parse_gutenberg_blocks(html: &str) -> Vec<Block> {
    parse_blocks_in(html, false)
}

/// `styled_ok`: whether a block here may carry attributes as a `{...}`
/// line - true at the top level and inside fenced containers, whose content
/// is parsed as a document of its own.
fn parse_blocks_in(html: &str, styled_ok: bool) -> Vec<Block> {
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
            blocks.push(make_block(parsed.name, parsed.attrs, "", &html[cstart..cend], styled_ok));
            pos = cend;
            continue;
        }
        match find_block_end(html, cend, parsed.name) {
            Some((inner_end, after)) => {
                blocks.push(make_block(parsed.name, parsed.attrs, &html[cend..inner_end], &html[cstart..after], styled_ok));
                pos = after;
            }
            None => {
                blocks.push(make_block(parsed.name, parsed.attrs, &html[cend..], &html[cstart..], styled_ok));
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
///
/// A recognized block only becomes Markdown when that Markdown renders back
/// to the same structure (see `fidelity`) - otherwise it, too, is kept
/// verbatim, so attributes Markdown can't carry are never lost.
fn make_block(name: &str, attrs: Option<&str>, inner: &str, raw: &str, styled_ok: bool) -> Block {
    let verbatim = || Block::RawHtml { html: raw.trim().to_string() };
    let mut json = match attrs.map(serde_json::from_str::<serde_json::Value>) {
        None => serde_json::Map::new(),
        Some(Ok(serde_json::Value::Object(map))) => map,
        Some(_) => return verbatim(),
    };
    // Containers are written as `:::` fences, which only exist where a
    // block may carry its own lines (not inside a quote or list item).
    if CONTAINER_BLOCKS.contains(&name) {
        if !styled_ok {
            return verbatim();
        }
        let Some(container) = parse_container(name, &mut json, inner) else { return verbatim() };
        return finish(name, container, json, inner, raw, styled_ok);
    }
    let candidate = match name {
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
        "quote" => {
            let quote = strip_wrapper_tag(inner, "blockquote");
            let trimmed = quote.trim_end();
            // WordPress puts the citation after the inner blocks.
            let (body, citation) = match trimmed.rfind("<cite") {
                Some(idx) if trimmed.ends_with("</cite>") => {
                    let citation = extract_between(&trimmed[idx..], ">", "</cite>").map(|c| inline_html_to_markdown(c.trim())).filter(|c| !c.is_empty());
                    (trimmed[..idx].to_string(), citation)
                }
                _ => (quote.clone(), None),
            };
            Block::BlockQuote { blocks: parse_gutenberg_blocks(&body), citation }
        }
        "code" => {
            let code = strip_wrapper_tag(&strip_wrapper_tag(inner, "pre"), "code");
            Block::CodeBlock {
                lang: None,
                text: unescape_entities(code.trim()),
            }
        }
        "image" => {
            let caption = inner.find("<figcaption").and_then(|idx| extract_between(&inner[idx..], ">", "</figcaption>")).map(str::trim).unwrap_or("");
            // Markdown's image title is plain text - a caption with its own
            // markup (a link, emphasis) can't go there.
            if caption.contains('<') {
                return verbatim();
            }
            // A link around the image keeps only its address in Markdown
            // (`[![…](bild)](ziel)`) - a new-tab target or rel stays raw.
            if let Some(a) = inner.find("<a ") {
                let tag = &inner[a + 3..a + inner[a..].find('>').unwrap_or(inner.len() - a)];
                if crate::attrs::parse_tag_attrs(tag).iter().any(|(key, _)| key != "href") {
                    return verbatim();
                }
            }
            Block::Image {
                url: extract_attr(inner, "src").unwrap_or_default(),
                alt: unescape_entities(&extract_attr(inner, "alt").unwrap_or_default()),
                title: Some(unescape_entities(caption)).filter(|t| !t.is_empty()),
                // Plain Markdown has no slot to carry these through a
                // round-trip (see the field's own doc comment in `lib.rs`) -
                // re-acquired fresh from `Frontmatter.media` on the next
                // export instead, the same as importing never recovers a
                // body image's own WordPress upload state in general.
                media_id: None,
                width: 0,
                height: 0,
                // `<a href>` right around the `<img>`.
                link: inner.find("<a ").filter(|a| inner[*a..].split('>').nth(1).is_some_and(|rest| rest.trim_start().starts_with("<img"))).and_then(|a| extract_attr(&inner[a..], "href")).map(|href| unescape_entities(&href)),
            }
        }
        "separator" => Block::ThematicBreak,
        "video" | "audio" | "embed" => {
            // Captions go into the Markdown image's brackets or a quoted
            // attribute value - plain text only.
            let caption = inner.find("<figcaption").and_then(|idx| extract_between(&inner[idx..], ">", "</figcaption>")).map(str::trim).unwrap_or("");
            if caption.contains(['<', '"', '[', ']']) {
                return verbatim();
            }
            let caption = Some(unescape_entities(caption)).filter(|c| !c.is_empty());
            match name {
                "video" => Block::Video { url: extract_attr(inner, "src").unwrap_or_default(), caption },
                "audio" => Block::Audio { url: extract_attr(inner, "src").unwrap_or_default(), caption },
                _ => Block::Embed { url: extract_json_string(attrs, "url").unwrap_or_default(), caption },
            }
        }
        "table" => match parse_table_block(inner) {
            // The caption goes into a quoted attribute value - plain text only.
            Block::Table { caption: Some(caption), .. } if caption.contains(['<', '"']) => return verbatim(),
            table => table,
        },
        "buttons" => {
            let buttons = parse_buttons(&strip_wrapper_tag(inner, "div"));
            // The structure check skips inline tags like the button's `<a>`,
            // so link attributes Markdown can't carry are checked here.
            if !button_links_supported(inner) {
                return verbatim();
            }
            Block::Buttons { buttons }
        }
        "gallery" => {
            let gallery_inner = strip_wrapper_tag(inner, "figure");
            let (images, size_slug) = parse_gallery(&gallery_inner);
            // The gallery's own caption follows its last image block.
            let after_images = gallery_inner.rfind("<!-- /wp:image -->").map(|i| &gallery_inner[i..]).unwrap_or("");
            let caption = after_images.find("<figcaption").and_then(|idx| extract_between(&after_images[idx..], ">", "</figcaption>")).map(str::trim).unwrap_or("");
            if caption.contains(['<', '"']) {
                return verbatim();
            }
            Block::Gallery {
                images,
                settings: GallerySettings {
                    columns: extract_json_number(attrs, "columns"),
                    cropped: !attr_is_false(attrs, "imageCrop"),
                    link_to: extract_json_string(attrs, "linkTo").unwrap_or_else(|| "none".to_string()),
                    size_slug: size_slug.unwrap_or_else(|| "large".to_string()),
                    caption: Some(unescape_entities(caption)).filter(|c| !c.is_empty()),
                },
            }
        }
        "pullquote" => parse_pullquote_block(inner),
        // Our own "html" passthrough, and WordPress's own unusual "more"
        // block (whose inner content already just *is* the bare
        // `<!--more-->` marker - see the matching arm in `lib.rs`'s
        // `render_block`) both keep only their inner content, same as
        // before - `inner` is exactly what's wanted for the Markdown these
        // two produce, and neither is ever self-closing (an empty `raw`
        // fallback risk, see below, doesn't apply to either).
        "html" | "more" if attrs.is_none() && !inner.trim().is_empty() => return Block::RawHtml { html: inner.trim().to_string() },
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
        _ => return verbatim(),
    };

    finish(name, candidate, json, inner, raw, styled_ok)
}

/// Moves the remaining attributes onto `candidate` and keeps it only if
/// its Markdown round trip has the original structure.
fn finish(name: &str, candidate: Block, mut json: serde_json::Map<String, serde_json::Value>, inner: &str, raw: &str, styled_ok: bool) -> Block {
    let verbatim = || Block::RawHtml { html: raw.trim().to_string() };
    let mut block_attrs = BlockAttrs::take_from_json(name, &mut json);
    if name == "table" && inner.find("<table").is_some_and(|i| inner[i..].split('>').next().unwrap_or("").contains("has-fixed-layout")) {
        block_attrs.fixed_layout = true;
    }
    if !styled_ok && !block_attrs.is_empty() {
        return verbatim();
    }
    let candidate = candidate.with_attrs(block_attrs);
    // Checked on the real round trip - Markdown text, parsed and rendered
    // again - since that is what the next upload sends.
    let back = crate::render_blocks(&crate::parse_markdown(&render_block_markdown(&candidate)));
    if fidelity::same_structure(raw, &back) {
        candidate
    } else {
        if std::env::var_os("GUTENBERG_DEBUG").is_some() {
            eprintln!("keeping {name} verbatim: {:?}", fidelity::first_difference(raw, &back));
        }
        verbatim()
    }
}

const CONTAINER_BLOCKS: &[&str] = &["group", "columns", "accordion", "tabs", "cover", "details", "media-text"];

/// Direct child blocks of a container's inner HTML: name, attrs JSON,
/// inner HTML.
fn child_blocks(html: &str) -> Vec<(String, Option<String>, String)> {
    let mut children = Vec::new();
    let mut pos = 0;
    while let Some((comment, _cstart, cend)) = next_comment(html, pos) {
        let Some(parsed) = parse_wp_comment(comment) else {
            pos = cend;
            continue;
        };
        if parsed.closing {
            pos = cend;
            continue;
        }
        let (name, attrs) = (parsed.name.to_string(), parsed.attrs.map(str::to_string));
        if parsed.self_closing {
            children.push((name, attrs, String::new()));
            pos = cend;
            continue;
        }
        match find_block_end(html, cend, &name) {
            Some((inner_end, after)) => {
                children.push((name, attrs, html[cend..inner_end].to_string()));
                pos = after;
            }
            None => break,
        }
    }
    children
}

fn json_map(attrs: Option<&str>) -> Option<serde_json::Map<String, serde_json::Value>> {
    match attrs.map(serde_json::from_str::<serde_json::Value>) {
        None => Some(serde_json::Map::new()),
        Some(Ok(serde_json::Value::Object(map))) => Some(map),
        Some(_) => None,
    }
}

fn take_json_string(json: &mut serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    let value = json.get(key)?.as_str()?.to_string();
    json.remove(key);
    Some(value)
}

fn take_json_true(json: &mut serde_json::Map<String, serde_json::Value>, key: &str) -> bool {
    let is_true = json.get(key) == Some(&serde_json::Value::Bool(true));
    if is_true {
        json.remove(key);
    }
    is_true
}

/// Content of the element the container's markup starts with, e.g. the
/// `<div class="wp-block-group">` - everything up to its last closing tag.
fn element_content(html: &str) -> &str {
    let html = html.trim();
    let Some(open_end) = html.find('>') else { return "" };
    let Some(close) = html.rfind("</") else { return "" };
    if close <= open_end {
        return "";
    }
    &html[open_end + 1..close]
}

fn container(kind: &str, title: Option<String>, params: crate::ContainerParams, blocks: Vec<Block>) -> Block {
    Block::Container { kind: kind.to_string(), title, params, blocks }
}

/// Reads a container block into `Block::Container`, taking the settings
/// it understands out of `json` (the rest becomes block attributes or, if
/// Markdown can't carry it, fails the structure check).
fn parse_container(name: &str, json: &mut serde_json::Map<String, serde_json::Value>, inner: &str) -> Option<Block> {
    let mut params = crate::ContainerParams::default();
    let content = element_content(inner);
    let block = match name {
        "group" => {
            if let Some(serde_json::Value::Object(layout)) = json.get("layout").cloned() {
                let kind = layout.get("type").and_then(|t| t.as_str()).unwrap_or("default");
                let vertical = layout.get("orientation").and_then(|o| o.as_str()) == Some("vertical");
                params.set("layout", Some(if kind == "flex" && vertical { "stack".to_string() } else { kind.to_string() }));
                if layout.get("flexWrap").and_then(|w| w.as_str()) == Some("nowrap") {
                    params.set("nowrap", None);
                }
                if let Some(justify) = layout.get("justifyContent").and_then(|j| j.as_str()) {
                    params.set("justify", Some(justify.to_string()));
                }
                if let Some(columns) = layout.get("columnCount").and_then(|c| c.as_u64()) {
                    params.set("columns", Some(columns.to_string()));
                }
                json.remove("layout");
            }
            if let Some(tag) = take_json_string(json, "tagName") {
                params.set("tag", Some(tag));
            }
            container("group", None, params, parse_blocks_in(content, true))
        }
        "columns" => {
            if let Some(valign) = take_json_string(json, "verticalAlignment") {
                params.set("valign", Some(valign));
            }
            if json.get("isStackedOnMobile") == Some(&serde_json::Value::Bool(false)) {
                json.remove("isStackedOnMobile");
                params.set("nostack", None);
            }
            let mut columns = Vec::new();
            for (child, attrs, child_inner) in child_blocks(content) {
                if child != "column" {
                    return None;
                }
                let mut column_json = json_map(attrs.as_deref())?;
                let mut column_params = crate::ContainerParams::default();
                if let Some(width) = take_json_string(&mut column_json, "width") {
                    column_params.set("width", Some(width));
                }
                if let Some(valign) = take_json_string(&mut column_json, "verticalAlignment") {
                    column_params.set("valign", Some(valign));
                }
                let column_attrs = BlockAttrs::take_from_json("column", &mut column_json);
                columns.push(container("column", None, column_params, parse_blocks_in(element_content(&child_inner), true)).with_attrs(column_attrs));
            }
            container("columns", None, params, columns)
        }
        "accordion" => {
            let mut items = Vec::new();
            for (child, attrs, child_inner) in child_blocks(content) {
                if child != "accordion-item" {
                    return None;
                }
                let mut item_json = json_map(attrs.as_deref())?;
                let mut item_params = crate::ContainerParams::default();
                if take_json_true(&mut item_json, "openByDefault") {
                    item_params.set("open", None);
                }
                let mut title = String::new();
                let mut blocks = Vec::new();
                for (part, _, part_inner) in child_blocks(element_content(&child_inner)) {
                    match part.as_str() {
                        "accordion-heading" => {
                            if let Some(level) = part_inner.trim().strip_prefix("<h").and_then(|r| r.chars().next()).and_then(|c| c.to_digit(10)).filter(|l| *l != 3) {
                                item_params.set("level", Some(level.to_string()));
                            }
                            title = extract_between(&part_inner, "wp-block-accordion-heading__toggle-title\">", "</span>").map(inline_html_to_markdown).unwrap_or_default();
                        }
                        "accordion-panel" => blocks = parse_blocks_in(element_content(&part_inner), true),
                        _ => return None,
                    }
                }
                let item_attrs = BlockAttrs::take_from_json("accordion-item", &mut item_json);
                items.push(container("item", Some(title), item_params, blocks).with_attrs(item_attrs));
            }
            container("accordion", None, params, items)
        }
        "tabs" => {
            let mut tabs = Vec::new();
            for (child, _, child_inner) in child_blocks(content) {
                match child.as_str() {
                    "tab-list" => {}
                    "tab-panels" => {
                        for (panel, attrs, panel_inner) in child_blocks(element_content(&child_inner)) {
                            if panel != "tab-panel" {
                                return None;
                            }
                            let mut panel_json = json_map(attrs.as_deref())?;
                            let label = take_json_string(&mut panel_json, "label").unwrap_or_default();
                            let mut tab_params = crate::ContainerParams::default();
                            if let Some(anchor) = take_json_string(&mut panel_json, "anchor") {
                                if anchor != crate::containers::slug(&label) {
                                    tab_params.set("anchor", Some(anchor));
                                }
                            }
                            tabs.push(container("tab", Some(label), tab_params, parse_blocks_in(element_content(&panel_inner), true)));
                        }
                    }
                    _ => return None,
                }
            }
            container("tabs", None, params, tabs)
        }
        "cover" => {
            if let Some(url) = take_json_string(json, "url") {
                params.set("image", Some(url));
            }
            if let Some(id) = json.get("id").and_then(|i| i.as_u64()) {
                json.remove("id");
                params.set("id", Some(id.to_string()));
            }
            if take_json_true(json, "hasParallax") {
                params.set("parallax", None);
            }
            if let Some(dim) = json.get("dimRatio").and_then(|d| d.as_u64()) {
                json.remove("dimRatio");
                params.set("dim", Some(dim.to_string()));
            }
            if let Some(overlay) = take_json_string(json, "overlayColor") {
                params.set("overlay", Some(overlay));
            }
            if let Some(gradient) = take_json_string(json, "gradient") {
                params.set("gradient", Some(gradient));
            }
            if let Some(height) = json.get("minHeight").and_then(|h| h.as_f64()) {
                json.remove("minHeight");
                let unit = take_json_string(json, "minHeightUnit").unwrap_or_else(|| "px".to_string());
                params.set("height", Some(format!("{height}{unit}")));
            }
            if let Some(position) = take_json_string(json, "contentPosition") {
                params.set("position", Some(position));
            }
            let marker = "wp-block-cover__inner-container\">";
            let start = content.find(marker)? + marker.len();
            let body = &content[start..];
            let body = &body[..body.rfind("</div>")?];
            container("cover", None, params, parse_blocks_in(body, true))
        }
        "details" => {
            if take_json_true(json, "showContent") {
                params.set("open", None);
            }
            let (summary, body) = match content.find("</summary>") {
                Some(end) => (extract_between(content, "<summary>", "</summary>").map(inline_html_to_markdown).unwrap_or_default(), &content[end + "</summary>".len()..]),
                None => (String::new(), content),
            };
            container("details", Some(summary), params, parse_blocks_in(body, true))
        }
        "media-text" => {
            let right = take_json_string(json, "mediaPosition").as_deref() == Some("right");
            let media_type = take_json_string(json, "mediaType");
            let marker = "wp-block-media-text__content\">";
            let start = content.find(marker)? + marker.len();
            let figure_start = content.find("<figure class=\"wp-block-media-text__media\"")?;
            let figure_end = content[figure_start..].find("</figure>")? + figure_start;
            let figure = &content[figure_start..figure_end];
            // The content div runs to the figure (media on the right) or
            // to the end.
            let body_end = if figure_start > start { figure_start } else { content.len() };
            let body = content[start..body_end].trim_end().strip_suffix("</div>")?;
            match media_type.as_deref() {
                Some("video") => {
                    params.set("image", Some(unescape_entities(&extract_attr(figure, "src")?)));
                    params.set("type", Some("video".to_string()));
                }
                Some(_) => {
                    let img = &figure[figure.find("<img")?..];
                    params.set("image", Some(unescape_entities(&extract_attr(img, "src")?)));
                    let alt = unescape_entities(&extract_attr(img, "alt").unwrap_or_default());
                    if !alt.is_empty() {
                        params.set("alt", Some(alt));
                    }
                }
                None => {}
            }
            if right {
                params.set("position", Some("right".to_string()));
            }
            if let Some(id) = json.get("mediaId").and_then(|i| i.as_u64()) {
                json.remove("mediaId");
                params.set("id", Some(id.to_string()));
            }
            if let Some(size) = take_json_string(json, "mediaSizeSlug") {
                params.set("size", Some(size));
            }
            if let Some(width) = json.get("mediaWidth").and_then(|w| w.as_u64()) {
                json.remove("mediaWidth");
                params.set("width", Some(width.to_string()));
            }
            if json.get("isStackedOnMobile") == Some(&serde_json::Value::Bool(false)) {
                json.remove("isStackedOnMobile");
                params.set("nostack", None);
            }
            if let Some(valign) = take_json_string(json, "verticalAlignment") {
                params.set("valign", Some(valign));
            }
            if take_json_true(json, "imageFill") {
                params.set("fill", None);
            }
            container("media-text", None, params, parse_blocks_in(body, true))
        }
        _ => return None,
    };
    Some(block)
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
        let attrs = button_attrs(parsed.attrs);
        match find_block_end(buttons_inner, cend, "button") {
            Some((inner_end, after)) => {
                buttons.push(with_button_attrs(button_from_html(&buttons_inner[cend..inner_end]), &attrs));
                pos = after;
            }
            None => {
                buttons.push(with_button_attrs(button_from_html(&buttons_inner[cend..]), &attrs));
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
    let new_tab = html.find("<a").and_then(|a| extract_attr(&html[a..], "target")).as_deref() == Some("_blank");
    ButtonItem { text, url, attrs: BlockAttrs { new_tab, ..BlockAttrs::default() } }
}

fn with_button_attrs(button: ButtonItem, attrs: &BlockAttrs) -> ButtonItem {
    let attrs = button.attrs.clone().merged(attrs.clone());
    ButtonItem { attrs, ..button }
}

/// Whether every button link's attributes are ones the Markdown form
/// keeps: class, href, style, and the new-tab pair.
fn button_links_supported(html: &str) -> bool {
    html.match_indices("<a ").all(|(start, _)| {
        let tag = &html[start + 3..start + html[start..].find('>').unwrap_or(html.len() - start)];
        crate::attrs::parse_tag_attrs(tag).iter().all(|(key, value)| match key.as_str() {
            "class" | "href" | "style" => true,
            "target" => value == "_blank",
            "rel" => value == "noreferrer noopener",
            _ => false,
        })
    })
}

/// A button's settings from its block comment - whatever is left over
/// shows up in the structure check and keeps the buttons raw.
fn button_attrs(json: Option<&str>) -> BlockAttrs {
    let Some(mut json) = json_map(json) else { return BlockAttrs::default() };
    let mut attrs = BlockAttrs::take_from_json("button", &mut json);
    if let Some(serde_json::Value::Object(style)) = json.get_mut("style") {
        if let Some(width) = style.get("dimensions").and_then(|d| d.get("width")).and_then(|w| w.as_str()).map(str::to_string) {
            attrs.width = Some(width);
            style.remove("dimensions");
        }
    }
    attrs
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
        media_id: None,
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


fn parse_table_block(inner: &str) -> Block {
    let figure_inner = strip_wrapper_tag(inner, "figure");
    let table_inner = extract_between(&figure_inner, ">", "</table>").map(str::to_string).unwrap_or_else(|| strip_wrapper_tag(inner, "table"));
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
    let read_rows = |section: &str| -> Vec<Vec<String>> {
        extract_all_between(section, "<tr>", "</tr>").into_iter().map(|row| extract_all_tags(row, "td").into_iter().map(|(_, content)| inline_html_to_markdown(content)).collect()).collect()
    };
    let tbody = extract_between(&table_inner, "<tbody>", "</tbody>").unwrap_or(table_inner.as_str());
    let rows = read_rows(tbody);
    if alignments.is_empty() {
        if let Some(first_row) = extract_all_between(tbody, "<tr>", "</tr>").first() {
            alignments = extract_all_tags(first_row, "td").into_iter().map(|(tag, _)| alignment_from_style(tag)).collect();
        }
    }
    let footer = extract_between(&table_inner, "<tfoot>", "</tfoot>").map(read_rows).unwrap_or_default();
    let caption = figure_inner.find("<figcaption").and_then(|idx| extract_between(&figure_inner[idx..], ">", "</figcaption>")).map(|c| unescape_entities(c.trim())).filter(|c| !c.is_empty());
    Block::Table { alignments, header, rows, footer, caption }
}

/// A cell's alignment - current WordPress writes a class plus
/// `data-align`, older versions an inline style.
fn alignment_from_style(open_tag: &str) -> ColumnAlignment {
    if open_tag.contains("text-align:left") || open_tag.contains("has-text-align-left") {
        ColumnAlignment::Left
    } else if open_tag.contains("text-align:center") || open_tag.contains("has-text-align-center") {
        ColumnAlignment::Center
    } else if open_tag.contains("text-align:right") || open_tag.contains("has-text-align-right") {
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

/// Inline HTML -> Markdown. What Markdown has syntax for (strong, em,
/// code, strikethrough, plain links) becomes that syntax; every other inline
/// element - `<mark>` with a color, `<sub>`, `<kbd>`, a link with `target`
/// or `rel`, a line break - is kept as inline HTML, which Markdown passes
/// through unchanged, instead of being dropped.
pub(crate) fn inline_html_to_markdown(html: &str) -> String {
    let mut out = String::new();
    // Per open `<a>`: its href when written as Markdown, `None` when kept
    // as HTML (so the matching `</a>` stays HTML too).
    let mut links: Vec<Option<String>> = Vec::new();
    let mut code_depth = 0usize;
    let mut i = 0;
    while i < html.len() {
        if html.as_bytes()[i] == b'<' {
            let Some(rel_end) = html[i..].find('>') else {
                out.push_str(&escape_markdown_text(&html[i..], code_depth > 0));
                break;
            };
            let tag_text = &html[i..i + rel_end + 1];
            let tag_content = &html[i + 1..i + rel_end];
            i += rel_end + 1;
            let is_closing = tag_content.starts_with('/');
            let body = tag_content.trim_start_matches('/').trim_end_matches('/');
            let name = body.split_whitespace().next().unwrap_or("").to_lowercase();
            let plain = !body.contains(char::is_whitespace);
            match (name.as_str(), is_closing) {
                ("strong" | "b", _) if plain => out.push_str("**"),
                ("em" | "i", _) if plain => out.push('*'),
                ("s" | "del", _) if plain => out.push_str("~~"),
                ("code", false) if plain => {
                    code_depth += 1;
                    out.push('`');
                }
                ("code", true) if code_depth > 0 => {
                    code_depth -= 1;
                    out.push('`');
                }
                ("a", false) => {
                    let attrs = crate::attrs::parse_tag_attrs(&body[1..]);
                    if attrs.len() == 1 && attrs[0].0 == "href" {
                        links.push(Some(attrs[0].1.clone()));
                        out.push('[');
                    } else {
                        links.push(None);
                        out.push_str(tag_text);
                    }
                }
                ("a", true) => match links.pop() {
                    Some(Some(href)) => out.push_str(&format!("]({})", markdown_destination(&href))),
                    _ => out.push_str(tag_text),
                },
                _ => out.push_str(tag_text),
            }
        } else {
            let next = html[i..].find('<').map(|p| i + p).unwrap_or(html.len());
            out.push_str(&escape_markdown_text(&html[i..next], code_depth > 0));
            i = next;
        }
    }
    out.trim().to_string()
}

/// Text between tags. Inside a code span entities are decoded (backticks
/// show text literally); outside, `&lt;`/`&amp;` stay encoded wherever
/// decoding them would turn into markup (`&lt;b&gt;` must not become a
/// real `<b>`), and are decoded everywhere else for readability.
fn escape_markdown_text(text: &str, in_code: bool) -> String {
    if in_code {
        return unescape_entities(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let entity_len = rest.find(';').filter(|&e| e <= 10).map(|e| e + 1);
        let Some(len) = entity_len else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[..len];
        let after = &rest[len..];
        let decoded = match entity {
            "&lt;" if !after.starts_with(|c: char| c.is_ascii_alphabetic() || c == '/' || c == '!' || c == '?') => Some("<"),
            "&gt;" => Some(">"),
            "&quot;" => Some("\""),
            "&#039;" | "&apos;" => Some("'"),
            "&amp;" if !looks_like_entity(after) => Some("&"),
            _ => None,
        };
        out.push_str(decoded.unwrap_or(entity));
        rest = after;
    }
    out.push_str(rest);
    out
}

fn looks_like_entity(s: &str) -> bool {
    match s.find(';') {
        Some(end) if end > 0 && end <= 10 => s[..end].chars().all(|c| c.is_ascii_alphanumeric() || c == '#'),
        _ => false,
    }
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
        Block::List { ordered, items } => render_list_markdown(*ordered, items, 0, 1),
        Block::BlockQuote { blocks, citation } => {
            let mut text = render_markdown(blocks);
            if let Some(citation) = citation {
                text.push_str(&format!("\n\n{}{citation}", crate::CITATION_DASH));
            }
            text.lines().map(|line| if line.is_empty() { ">".to_string() } else { format!("> {line}") }).collect::<Vec<_>>().join("\n")
        }
        Block::CodeBlock { lang, text } => format!("```{}\n{text}\n```", lang.clone().unwrap_or_default()),
        Block::Image { url, alt, title, link, .. } => {
            // Bracket = caption, title = alt text - see `as_lone_media`.
            let destination = markdown_destination(url);
            let caption = title.as_deref().unwrap_or("").replace('[', "\\[").replace(']', "\\]");
            let image = if alt.is_empty() {
                format!("![{caption}]({destination})")
            } else {
                format!("![{caption}]({destination} \"{}\")", alt.replace('"', "\\\""))
            };
            match link {
                Some(link) => format!("[{image}]({})", markdown_destination(link)),
                None => image,
            }
        }
        Block::Video { url, caption } | Block::Audio { url, caption } => format!("![{}]({})", caption.as_deref().unwrap_or(""), markdown_destination(url)),
        Block::Embed { url, caption: None } => url.clone(),
        Block::Embed { url, caption: Some(caption) } => format!("{url}\n{}", BlockAttrs { caption: Some(caption.clone()), ..Default::default() }.to_markdown()),
        Block::ThematicBreak => "---".to_string(),
        Block::Table { alignments, header, rows, footer, caption } => {
            let table = render_table_markdown(alignments, header, rows, footer);
            let attrs = BlockAttrs { caption: caption.clone(), footer_rows: footer.len() as u32, ..Default::default() };
            if attrs.is_empty() {
                table
            } else {
                format!("{table}\n{}", attrs.to_markdown())
            }
        }
        Block::Columns { columns } => render_columns_markdown(columns),
        Block::Buttons { buttons } => render_buttons_markdown(buttons),
        Block::Gallery { images, settings } => render_gallery_fence(images, settings),
        Block::Pullquote { paragraphs, citation } => render_pullquote_markdown(paragraphs, citation),
        Block::Details { summary, blocks } => render_details_markdown(summary, blocks),
        Block::RawHtml { html } => html.clone(),
        Block::Container { kind, title, params, blocks } => render_container_markdown(kind, title.as_deref(), params, &BlockAttrs::default(), blocks),
        Block::Styled { attrs, block } => match block.as_ref() {
            Block::Container { kind, title, params, blocks } => render_container_markdown(kind, title.as_deref(), params, attrs, blocks),
            // pulldown-cmark's heading attributes split at every space, so
            // a quoted value (`padding="0.5rem 1rem"`) needs its own line.
            Block::Heading { .. } if attrs.to_markdown().contains('"') => format!("{}\n{}", render_block_markdown(block), attrs.to_markdown()),
            Block::Heading { .. } => format!("{} {}", render_block_markdown(block), attrs.to_markdown()),
            // An ordered list's first number is plain Markdown.
            Block::List { ordered: true, items } if attrs.start.is_some() => {
                let list = render_list_markdown(true, items, 0, attrs.start.unwrap_or(1));
                let rest = BlockAttrs { start: None, ..(**attrs).clone() };
                if rest.is_empty() {
                    list
                } else {
                    format!("{list}\n{}", rest.to_markdown())
                }
            }
            Block::Table { alignments, header, rows, footer, caption } => {
                let table_attrs = BlockAttrs { caption: caption.clone(), footer_rows: footer.len() as u32, ..Default::default() };
                format!("{}\n{}", render_table_markdown(alignments, header, rows, footer), table_attrs.merged((**attrs).clone()).to_markdown())
            }
            Block::Gallery { images, settings } if settings.caption.is_some() => {
                let plain = GallerySettings { caption: None, ..settings.clone() };
                format!("{}\n{}", render_gallery_fence(images, &plain), BlockAttrs { caption: settings.caption.clone(), ..Default::default() }.merged((**attrs).clone()).to_markdown())
            }
            Block::Embed { url, caption } => format!("{url}\n{}", BlockAttrs { caption: caption.clone(), ..Default::default() }.merged((**attrs).clone()).to_markdown()),
            _ => format!("{}\n{}", render_block_markdown(block), attrs.to_markdown()),
        },
    }
}

fn render_container_markdown(kind: &str, title: Option<&str>, params: &crate::ContainerParams, attrs: &BlockAttrs, blocks: &[Block]) -> String {
    let colons = 3 + nested_container_depth(blocks);
    let header = crate::containers::header_markdown(colons, kind, title, params, attrs);
    let body = render_markdown(blocks);
    if body.is_empty() {
        format!("{header}\n{}", ":".repeat(colons))
    } else {
        format!("{header}\n{body}\n{}", ":".repeat(colons))
    }
}

/// How many container levels `blocks` hold - so an outer fence gets more
/// colons than the ones inside it.
fn nested_container_depth(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .map(|block| match block.unstyled() {
            Block::Container { blocks, .. } => 1 + nested_container_depth(blocks),
            _ => 0,
        })
        .max()
        .unwrap_or(0)
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

fn render_list_markdown(ordered: bool, items: &[Vec<Block>], indent: usize, start: u32) -> String {
    let pad = " ".repeat(indent);
    items
        .iter()
        .enumerate()
        .map(|(idx, item_blocks)| {
            let marker = if ordered { format!("{}.", start as usize + idx) } else { "-".to_string() };
            let mut lines = Vec::new();
            for (i, block) in item_blocks.iter().enumerate() {
                if i == 0 {
                    lines.push(format!("{pad}{marker} {}", render_block_markdown(block)));
                } else if let Block::List { ordered: nested_ordered, items: nested_items } = block {
                    lines.push(render_list_markdown(*nested_ordered, nested_items, indent + 2, 1));
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
    let body = buttons
        .iter()
        .map(|b| {
            let link = format!("[{}]({})", b.text, markdown_destination(&b.url));
            if b.attrs.is_empty() {
                link
            } else {
                format!("{link}{}", b.attrs.to_markdown())
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
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
            // Bracket = caption, title = alt text, like a body image.
            let caption = img.caption.as_deref().unwrap_or("").replace('[', "\\[").replace(']', "\\]");
            if img.alt.is_empty() {
                format!("![{caption}]({destination})")
            } else {
                format!("![{caption}]({destination} \"{}\")", img.alt.replace('"', "\\\""))
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let fence = match format_gallery_settings_line(settings) {
        Some(line) => format!("```gallery\n{line}\n+++\n{body}\n```"),
        None => format!("```gallery\n{body}\n```"),
    };
    match &settings.caption {
        Some(caption) => format!("{fence}\n{}", BlockAttrs { caption: Some(caption.clone()), ..Default::default() }.to_markdown()),
        None => fence,
    }
}

/// The inverse of `parse_gallery_settings_line` - `None` when `settings` is
/// entirely at its default, so a plain gallery still round-trips to the
/// exact same clean, options-line-free Markdown it always has.
fn format_gallery_settings_line(settings: &GallerySettings) -> Option<String> {
    if *settings == (GallerySettings { caption: settings.caption.clone(), ..GallerySettings::default() }) {
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

fn render_table_markdown(alignments: &[ColumnAlignment], header: &[String], rows: &[Vec<String>], footer: &[Vec<String>]) -> String {
    let columns = header.len().max(rows.iter().chain(footer).map(Vec::len).max().unwrap_or(0));
    let cell = |s: &str| s.replace('|', "\\|").replace('\n', "<br>");
    let line = |cells: &[String]| {
        let padded: Vec<String> = (0..columns).map(|i| cells.get(i).map(|c| cell(c)).unwrap_or_default()).collect();
        format!("| {} |", padded.join(" | "))
    };
    let mut lines = vec![line(header)];
    let seps: Vec<String> = (0..columns)
        .map(|i| match alignments.get(i) {
            Some(ColumnAlignment::Left) => ":---".to_string(),
            Some(ColumnAlignment::Center) => ":---:".to_string(),
            Some(ColumnAlignment::Right) => "---:".to_string(),
            _ => "---".to_string(),
        })
        .collect();
    lines.push(format!("| {} |", seps.join(" | ")));
    lines.extend(rows.iter().chain(footer).map(|row| line(row)));
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
        let block = Block::Image { url: "my cat.png".to_string(), alt: "a cat".to_string(), title: None, media_id: None, width: 0, height: 0, link: None };
        assert_eq!(render_block_markdown(&block), "![](<my cat.png> \"a cat\")");
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
    fn classic_content_stays_a_classic_block() {
        let wp = "<!-- wp:paragraph -->\n<p>Block</p>\n<!-- /wp:paragraph -->\n\n<p>Klassisch <strong>fett</strong></p>\n\n<!-- wp:paragraph -->\n<p>Block</p>\n<!-- /wp:paragraph -->";
        let md = gutenberg_to_markdown(wp);
        assert!(md.contains("<!-- wp:freeform -->\n<p>Klassisch <strong>fett</strong></p>\n<!-- /wp:freeform -->"), "{md}");
        let back = markdown_to_gutenberg(&md);
        assert!(crate::same_structure(wp, &back), "{:?}", crate::first_difference(wp, &back));
        assert!(!back.contains("wp:html"), "{back}");
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
            ":::: columns\n::: column\nColumn A text.\n:::\n\n::: column\nColumn B text.\n:::\n::::"
        );
    }

    #[test]
    fn fenced_columns_with_multiple_blocks_per_column_round_trip() {
        assert_eq!(
            round_trip("```columns\n## Left\n\nSome text.\n+++\n## Right\n\nMore text.\n```"),
            ":::: columns\n::: column\n## Left\n\nSome text.\n:::\n\n::: column\n## Right\n\nMore text.\n:::\n::::"
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
            "::: details \"Wie funktioniert das?\"\nSo funktioniert das.\n:::"
        );
    }

    #[test]
    fn fenced_details_with_an_image_in_the_body_round_trips() {
        assert_eq!(
            round_trip("```details\nGalerie?\n+++\n![a cat](cat.png)\n```"),
            "::: details \"Galerie?\"\n![a cat](cat.png)\n:::"
        );
    }

    #[test]
    fn table_alignment_round_trips() {
        let out = round_trip("| A | B | C |\n|:---|:---:|---:|\n| 1 | 2 | 3 |\n");
        assert_eq!(out, "| A | B | C |\n| :--- | :---: | ---: |\n| 1 | 2 | 3 |");
    }

    /// WordPress markup -> Markdown -> WordPress markup keeps the structure.
    fn assert_lossless(wp: &str) -> String {
        let md = gutenberg_to_markdown(wp);
        let back = markdown_to_gutenberg(&md);
        assert!(crate::same_structure(wp, &back), "{:?}\nMarkdown:\n{md}\nBack:\n{back}", crate::first_difference(wp, &back));
        md
    }

    #[test]
    fn colored_paragraph_gets_an_attribute_line() {
        let md = assert_lossless("<!-- wp:paragraph {\"backgroundColor\":\"line\"} -->\n<p class=\"has-line-background-color has-background\"><strong>Update:</strong> Text</p>\n<!-- /wp:paragraph -->");
        assert_eq!(md, "**Update:** Text\n{bg=line}");
    }

    #[test]
    fn gradient_and_text_color_round_trip() {
        let md = assert_lossless("<!-- wp:paragraph {\"textColor\":\"base\",\"gradient\":\"accent-fade\",\"fontSize\":\"large\"} -->\n<p class=\"has-base-color has-accent-fade-gradient-background has-text-color has-background has-large-font-size\">Text</p>\n<!-- /wp:paragraph -->");
        assert_eq!(md, "Text\n{color=base gradient=accent-fade size=large}");
    }

    #[test]
    fn heading_anchor_stays_on_the_heading_line() {
        let md = assert_lossless("<!-- wp:heading {\"anchor\":\"mein-eindruck\"} -->\n<h2 id=\"mein-eindruck\" class=\"wp-block-heading\">Mein Eindruck</h2>\n<!-- /wp:heading -->");
        assert_eq!(md, "## Mein Eindruck {#mein-eindruck}");
    }

    #[test]
    fn centered_heading_with_color_round_trips() {
        assert_lossless("<!-- wp:heading {\"level\":3,\"textColor\":\"accent\",\"anchor\":\"anker\",\"style\":{\"typography\":{\"textAlign\":\"center\"}}} -->\n<h3 class=\"wp-block-heading has-text-align-center has-accent-color has-text-color\" id=\"anker\">Zentriert</h3>\n<!-- /wp:heading -->");
    }

    #[test]
    fn image_caption_and_width_round_trip() {
        let md = assert_lossless("<!-- wp:image {\"id\":45504,\"sizeSlug\":\"large\",\"linkDestination\":\"none\",\"width\":\"100%\"} -->\n<figure class=\"wp-block-image size-large\"><img src=\"https://example.org/a.webp\" alt=\"Ein &quot;Bild&quot;\" class=\"wp-image-45504\" style=\"width:100%\"/><figcaption class=\"wp-element-caption\">Bildunterschrift</figcaption></figure>\n<!-- /wp:image -->");
        assert_eq!(md, "![Bildunterschrift](https://example.org/a.webp \"Ein \\\"Bild\\\"\")\n{width=100%}");
    }

    #[test]
    fn image_with_a_linked_caption_stays_verbatim() {
        let wp = "<!-- wp:image -->\n<figure class=\"wp-block-image\"><img src=\"a.webp\" alt=\"\"/><figcaption class=\"wp-element-caption\">Mit <a href=\"x\">Link</a></figcaption></figure>\n<!-- /wp:image -->";
        assert_eq!(assert_lossless(wp), wp);
    }

    #[test]
    fn padding_markdown_cannot_express_keeps_the_block_verbatim() {
        let wp = "<!-- wp:paragraph {\"backgroundColor\":\"base-2\",\"style\":{\"spacing\":{\"padding\":{\"top\":\"1rem\"}}}} -->\n<p class=\"has-base-2-background-color has-background\" style=\"padding-top:1rem\">Text</p>\n<!-- /wp:paragraph -->";
        assert_eq!(assert_lossless(wp), wp);
    }

    #[test]
    fn verbatim_block_with_blank_lines_survives_as_one_block() {
        let wp = "<!-- wp:group {\"layout\":{\"type\":\"constrained\"}} -->\n<div class=\"wp-block-group\"><!-- wp:paragraph -->\n<p>Eins</p>\n<!-- /wp:paragraph -->\n\n<!-- wp:paragraph -->\n<p>Zwei</p>\n<!-- /wp:paragraph --></div>\n<!-- /wp:group -->";
        let md = assert_lossless(wp);
        assert_eq!(markdown_to_gutenberg(&md), wp);
    }

    #[test]
    fn striped_table_with_footer_and_caption_round_trips() {
        let md = assert_lossless("<!-- wp:table {\"align\":\"wide\",\"className\":\"is-style-stripes\"} -->\n<figure class=\"wp-block-table alignwide is-style-stripes\"><table class=\"has-fixed-layout\"><thead><tr><th>A</th><th>B</th></tr></thead><tbody><tr><td>1</td><td>2</td></tr></tbody><tfoot><tr><td>Summe</td><td>3</td></tr></tfoot></table><figcaption class=\"wp-element-caption\">Eine Tabelle</figcaption></figure>\n<!-- /wp:table -->");
        assert_eq!(md, "| A | B |\n| --- | --- |\n| 1 | 2 |\n| Summe | 3 |\n{style=stripes align=wide fixed footer caption=\"Eine Tabelle\"}");
    }

    #[test]
    fn table_without_header_round_trips() {
        let md = assert_lossless("<!-- wp:table -->\n<figure class=\"wp-block-table\"><table><tbody><tr><td>1</td><td>2</td></tr></tbody></table></figure>\n<!-- /wp:table -->");
        assert_eq!(md, "|  |  |\n| --- | --- |\n| 1 | 2 |");
    }

    #[test]
    fn inline_markup_without_markdown_syntax_is_kept_as_html() {
        let md = assert_lossless("<!-- wp:paragraph -->\n<p>H<sub>2</sub>O, <mark style=\"background-color:#ff0\" class=\"has-inline-color\">markiert</mark>, <a href=\"https://example.org\" target=\"_blank\" rel=\"noreferrer noopener\">extern</a>, <a href=\"https://example.org\">intern</a>, &lt;b&gt; und Tom &amp; Jerry</p>\n<!-- /wp:paragraph -->");
        assert_eq!(md, "H<sub>2</sub>O, <mark style=\"background-color:#ff0\" class=\"has-inline-color\">markiert</mark>, <a href=\"https://example.org\" target=\"_blank\" rel=\"noreferrer noopener\">extern</a>, [intern](https://example.org), &lt;b> und Tom & Jerry");
        assert!(markdown_to_gutenberg(&md).contains("&lt;b&gt; und Tom &amp; Jerry"));
    }

    #[test]
    fn attributes_inside_a_quote_keep_the_inner_block_verbatim() {
        assert_lossless("<!-- wp:quote -->\n<blockquote class=\"wp-block-quote\"><!-- wp:paragraph {\"textColor\":\"accent\"} -->\n<p class=\"has-accent-color has-text-color\">Zitat</p>\n<!-- /wp:paragraph --></blockquote>\n<!-- /wp:quote -->");
    }

    #[test]
    fn list_start_and_reversed_round_trip() {
        let md = assert_lossless("<!-- wp:list {\"ordered\":true,\"start\":5,\"reversed\":true} -->\n<ol reversed start=\"5\" class=\"wp-block-list\"><!-- wp:list-item -->\n<li>Eins</li>\n<!-- /wp:list-item --></ol>\n<!-- /wp:list -->");
        assert_eq!(md, "5. Eins\n{reversed}");
    }

    #[test]
    fn accordion_becomes_a_container_and_round_trips() {
        let md = assert_lossless("<!-- wp:accordion -->\n<div role=\"group\" class=\"wp-block-accordion\"><!-- wp:accordion-item {\"openByDefault\":true} -->\n<div class=\"wp-block-accordion-item is-open\"><!-- wp:accordion-heading {\"openByDefault\":true} -->\n<h3 class=\"wp-block-accordion-heading has-icon has-icon-right\"><button type=\"button\" class=\"wp-block-accordion-heading__toggle\"><span class=\"wp-block-accordion-heading__toggle-title\">Akkordeon-Eintrag 1</span><span class=\"wp-block-accordion-heading__toggle-icon\" aria-hidden=\"true\">+</span></button></h3>\n<!-- /wp:accordion-heading -->\n\n<!-- wp:accordion-panel -->\n<div role=\"region\" class=\"wp-block-accordion-panel\"><!-- wp:paragraph -->\n<p>Laboris consectetur quis cillum excepteu</p>\n<!-- /wp:paragraph --></div>\n<!-- /wp:accordion-panel --></div>\n<!-- /wp:accordion-item -->\n\n<!-- wp:accordion-item -->\n<div class=\"wp-block-accordion-item\"><!-- wp:accordion-heading -->\n<h3 class=\"wp-block-accordion-heading has-icon has-icon-right\"><button type=\"button\" class=\"wp-block-accordion-heading__toggle\"><span class=\"wp-block-accordion-heading__toggle-title\">Akkordeon-Eintrag 2</span><span class=\"wp-block-accordion-heading__toggle-icon\" aria-hidden=\"true\">+</span></button></h3>\n<!-- /wp:accordion-heading -->\n\n<!-- wp:accordion-panel -->\n<div role=\"region\" class=\"wp-block-accordion-panel\"><!-- wp:paragraph -->\n<p>Magna nec in sit sunt erat luctus risus </p>\n<!-- /wp:paragraph --></div>\n<!-- /wp:accordion-panel --></div>\n<!-- /wp:accordion-item -->\n\n<!-- wp:accordion-item -->\n<div class=\"wp-block-accordion-item\"><!-- wp:accordion-heading -->\n<h3 class=\"wp-block-accordion-heading has-icon has-icon-right\"><button type=\"button\" class=\"wp-block-accordion-heading__toggle\"><span class=\"wp-block-accordion-heading__toggle-title\">Akkordeon-Eintrag 3</span><span class=\"wp-block-accordion-heading__toggle-icon\" aria-hidden=\"true\">+</span></button></h3>\n<!-- /wp:accordion-heading -->\n\n<!-- wp:accordion-panel -->\n<div role=\"region\" class=\"wp-block-accordion-panel\"><!-- wp:paragraph -->\n<p>Integer dapibus labore minim placerat li</p>\n<!-- /wp:paragraph --></div>\n<!-- /wp:accordion-panel --></div>\n<!-- /wp:accordion-item --></div>\n<!-- /wp:accordion -->");
        assert!(md.starts_with(":::"), "{md}");
    }

    #[test]
    fn tabs_becomes_a_container_and_round_trips() {
        let md = assert_lossless("<!-- wp:tabs -->\n<div class=\"wp-block-tabs\"><!-- wp:tab-list -->\n<div role=\"tablist\" class=\"wp-block-tab-list\"><button type=\"button\" role=\"tab\">Reiter 1</button><button type=\"button\" role=\"tab\">Reiter 2</button><button type=\"button\" role=\"tab\">Reiter 3</button></div>\n<!-- /wp:tab-list -->\n\n<!-- wp:tab-panels -->\n<div class=\"wp-block-tab-panels\"><!-- wp:tab-panel {\"label\":\"Reiter 1\",\"anchor\":\"reiter-1\"} -->\n<section role=\"tabpanel\" tabindex=\"0\" id=\"reiter-1\" class=\"wp-block-tab-panel\"><!-- wp:paragraph -->\n<p>Elit commodo erat; labore in fugiat grav</p>\n<!-- /wp:paragraph --></section>\n<!-- /wp:tab-panel -->\n\n<!-- wp:tab-panel {\"label\":\"Reiter 2\",\"anchor\":\"reiter-2\"} -->\n<section role=\"tabpanel\" tabindex=\"0\" id=\"reiter-2\" class=\"wp-block-tab-panel\"><!-- wp:paragraph -->\n<p>Culpa aliquam ultrices. Fermentum posuer</p>\n<!-- /wp:paragraph --></section>\n<!-- /wp:tab-panel -->\n\n<!-- wp:tab-panel {\"label\":\"Reiter 3\",\"anchor\":\"reiter-3\"} -->\n<section role=\"tabpanel\" tabindex=\"0\" id=\"reiter-3\" class=\"wp-block-tab-panel\"><!-- wp:paragraph -->\n<p>Occaecat. Aliquam aliquip. Luctus magna.</p>\n<!-- /wp:paragraph --></section>\n<!-- /wp:tab-panel --></div>\n<!-- /wp:tab-panels --></div>\n<!-- /wp:tabs -->");
        assert!(md.starts_with(":::"), "{md}");
    }

    #[test]
    fn cover_with_gradient_becomes_a_container_and_round_trips() {
        let md = assert_lossless("<!-- wp:cover {\"minHeight\":260,\"minHeightUnit\":\"px\",\"gradient\":\"hero-overlay\",\"contentPosition\":\"bottom left\"} -->\n<div class=\"wp-block-cover has-custom-content-position is-position-bottom-left\" style=\"min-height:260px\"><span aria-hidden=\"true\" class=\"wp-block-cover__background has-background-dim-100 has-background-dim has-background-gradient has-hero-overlay-gradient-background\"></span><div class=\"wp-block-cover__inner-container\"><!-- wp:paragraph {\"textColor\":\"base\"} -->\n<p class=\"has-base-color has-text-color\">Mollit dolor; et consectetur elit nisi. </p>\n<!-- /wp:paragraph --></div></div>\n<!-- /wp:cover -->");
        assert!(md.starts_with(":::"), "{md}");
    }

    #[test]
    fn columns_with_widths_becomes_a_container_and_round_trips() {
        let md = assert_lossless("<!-- wp:columns {\"verticalAlignment\":\"center\",\"align\":\"wide\"} -->\n<div class=\"wp-block-columns alignwide are-vertically-aligned-center\"><!-- wp:column {\"width\":\"25%\"} -->\n<div class=\"wp-block-column\" style=\"flex-basis:25%\"><!-- wp:image {\"id\":45654,\"sizeSlug\":\"full\",\"linkDestination\":\"none\"} -->\n<figure class=\"wp-block-image size-full\"><img src=\"https://linuxundich.de/wp-content/uploads/2026/09/tfm-archlinux-01.webp\" alt=\"\" class=\"wp-image-45654\"/></figure>\n<!-- /wp:image --></div>\n<!-- /wp:column -->\n\n<!-- wp:column {\"width\":\"50%\"} -->\n<div class=\"wp-block-column\" style=\"flex-basis:50%\"><!-- wp:paragraph -->\n<p>Risus aliquip esse magna pretium varius </p>\n<!-- /wp:paragraph --></div>\n<!-- /wp:column -->\n\n<!-- wp:column {\"width\":\"25%\",\"backgroundColor\":\"base-2\"} -->\n<div class=\"wp-block-column has-base-2-background-color has-background\" style=\"flex-basis:25%\"><!-- wp:heading {\"level\":4} -->\n<h4 class=\"wp-block-heading\">Spalte</h4>\n<!-- /wp:heading -->\n\n<!-- wp:paragraph -->\n<p>Pariatur do laboris nisi duis ipsum sed </p>\n<!-- /wp:paragraph --></div>\n<!-- /wp:column --></div>\n<!-- /wp:columns -->");
        assert!(md.starts_with(":::"), "{md}");
    }

    #[test]
    fn grid_group_becomes_a_container_and_round_trips() {
        let md = assert_lossless("<!-- wp:group {\"layout\":{\"type\":\"grid\",\"columnCount\":3}} -->\n<div class=\"wp-block-group\"><!-- wp:paragraph {\"backgroundColor\":\"base-2\"} -->\n<p class=\"has-base-2-background-color has-background\">Raster 1</p>\n<!-- /wp:paragraph -->\n\n<!-- wp:paragraph {\"backgroundColor\":\"base-2\"} -->\n<p class=\"has-base-2-background-color has-background\">Raster 2</p>\n<!-- /wp:paragraph -->\n\n<!-- wp:paragraph {\"backgroundColor\":\"base-2\"} -->\n<p class=\"has-base-2-background-color has-background\">Raster 3</p>\n<!-- /wp:paragraph -->\n\n<!-- wp:paragraph {\"backgroundColor\":\"base-2\"} -->\n<p class=\"has-base-2-background-color has-background\">Raster 4</p>\n<!-- /wp:paragraph -->\n\n<!-- wp:paragraph {\"backgroundColor\":\"base-2\"} -->\n<p class=\"has-base-2-background-color has-background\">Raster 5</p>\n<!-- /wp:paragraph -->\n\n<!-- wp:paragraph {\"backgroundColor\":\"base-2\"} -->\n<p class=\"has-base-2-background-color has-background\">Raster 6</p>\n<!-- /wp:paragraph --></div>\n<!-- /wp:group -->");
        assert!(md.starts_with(":::"), "{md}");
    }

    #[test]
    fn open_details_becomes_a_container_and_round_trips() {
        let md = assert_lossless("<!-- wp:details {\"showContent\":true} -->\n<details class=\"wp-block-details\" open><summary>Details: bereits geöffnet</summary><!-- wp:paragraph -->\n<p>Cubilia felis nulla aliquam integer pret</p>\n<!-- /wp:paragraph --></details>\n<!-- /wp:details -->");
        assert!(md.starts_with(":::"), "{md}");
    }

    #[test]
    fn container_markdown_reads_back() {
        let md = ":::: accordion\n::: item \"Frage \\\"eins\\\"\" {open}\nAntwort mit **Fett**.\n\n```\n:::\n```\n:::\n\n::: item \"Frage zwei\"\nZweite Antwort.\n:::\n::::";
        assert_eq!(round_trip(md), md);
    }

    #[test]
    fn quote_with_citation_round_trips() {
        let md = assert_lossless("<!-- wp:quote -->\n<blockquote class=\"wp-block-quote\"><!-- wp:paragraph -->\n<p>Ut sollicitudin enim.</p>\n<!-- /wp:paragraph --><cite>Marcus Tullius Cicero, <em>De finibus</em></cite></blockquote>\n<!-- /wp:quote -->");
        assert_eq!(md, "> Ut sollicitudin enim.\n>\n> — Marcus Tullius Cicero, *De finibus*");
    }

    #[test]
    fn plain_style_quote_with_citation_round_trips() {
        let md = assert_lossless("<!-- wp:quote {\"className\":\"is-style-plain\"} -->\n<blockquote class=\"wp-block-quote is-style-plain\"><!-- wp:paragraph -->\n<p>Eins.</p>\n<!-- /wp:paragraph -->\n\n<!-- wp:paragraph -->\n<p>Zwei.</p>\n<!-- /wp:paragraph --><cite>Lorem Ipsum</cite></blockquote>\n<!-- /wp:quote -->");
        assert!(md.contains("> — Lorem Ipsum"), "{md}");
    }

    #[test]
    fn media_text_becomes_a_container_and_round_trips() {
        let md = assert_lossless("<!-- wp:media-text {\"mediaId\":45656,\"mediaType\":\"image\"} -->\n<div class=\"wp-block-media-text is-stacked-on-mobile\"><figure class=\"wp-block-media-text__media\"><img src=\"https://example.org/a.webp\" alt=\"\" class=\"wp-image-45656 size-full\"/></figure><div class=\"wp-block-media-text__content\"><!-- wp:heading {\"level\":3} -->\n<h3 class=\"wp-block-heading\">Medien links</h3>\n<!-- /wp:heading -->\n\n<!-- wp:paragraph -->\n<p>Text.</p>\n<!-- /wp:paragraph --></div></div>\n<!-- /wp:media-text -->");
        assert!(md.starts_with("::: media-text {image=https://example.org/a.webp id=45656}"), "{md}");
    }

    #[test]
    fn media_text_on_the_right_with_fill_round_trips() {
        let md = assert_lossless("<!-- wp:media-text {\"align\":\"wide\",\"mediaPosition\":\"right\",\"mediaId\":45657,\"mediaType\":\"image\",\"mediaWidth\":40,\"verticalAlignment\":\"center\",\"imageFill\":true,\"backgroundColor\":\"base-2\"} -->\n<div class=\"wp-block-media-text alignwide has-media-on-the-right is-stacked-on-mobile is-vertically-aligned-center is-image-fill-element has-base-2-background-color has-background\" style=\"grid-template-columns:auto 40%\"><div class=\"wp-block-media-text__content\"><!-- wp:paragraph -->\n<p>Rechts.</p>\n<!-- /wp:paragraph --></div><figure class=\"wp-block-media-text__media\"><img src=\"https://example.org/b.webp\" alt=\"Ein Bild\" class=\"wp-image-45657 size-full\" style=\"object-position:50% 50%\"/></figure></div>\n<!-- /wp:media-text -->");
        assert!(md.contains("position=right") && md.contains("fill") && md.contains("width=40") && md.contains("alt=\"Ein Bild\""), "{md}");
    }

    #[test]
    fn padding_border_and_shadow_round_trip() {
        let md = assert_lossless("<!-- wp:paragraph {\"backgroundColor\":\"base-2\",\"style\":{\"spacing\":{\"padding\":{\"top\":\"0.5rem\",\"right\":\"1rem\",\"bottom\":\"0.5rem\",\"left\":\"1rem\"}},\"border\":{\"width\":\"2px\",\"style\":\"dashed\",\"color\":\"#1d4ed8\",\"radius\":\"8px\"}}} -->\n<p class=\"has-border-color has-base-2-background-color has-background\" style=\"border-color:#1d4ed8;border-style:dashed;border-width:2px;border-radius:8px;padding-top:0.5rem;padding-right:1rem;padding-bottom:0.5rem;padding-left:1rem\">Kasten</p>\n<!-- /wp:paragraph -->");
        assert_eq!(md, "Kasten\n{bg=base-2 padding=\"0.5rem 1rem\" border=\"2px dashed #1d4ed8\" radius=8px}");
        let md = assert_lossless("<!-- wp:image {\"id\":45657,\"sizeSlug\":\"full\",\"linkDestination\":\"none\",\"style\":{\"border\":{\"width\":\"4px\",\"color\":\"#cccccc\",\"radius\":\"6px\"},\"shadow\":\"var:preset|shadow|natural\"}} -->\n<figure class=\"wp-block-image size-full has-custom-border\"><img src=\"https://example.org/a.webp\" alt=\"Alt\" class=\"has-border-color wp-image-45657\" style=\"border-color:#cccccc;border-width:4px;border-radius:6px;box-shadow:var(--wp--preset--shadow--natural)\"/></figure>\n<!-- /wp:image -->");
        assert!(md.ends_with("{border=\"4px #cccccc\" radius=6px shadow=natural}"), "{md}");
    }

    #[test]
    fn a_padding_preset_round_trips() {
        assert_lossless("<!-- wp:group {\"style\":{\"spacing\":{\"padding\":{\"top\":\"var:preset|spacing|50\",\"right\":\"var:preset|spacing|50\",\"bottom\":\"var:preset|spacing|50\",\"left\":\"var:preset|spacing|50\"}}},\"layout\":{\"type\":\"constrained\"}} -->\n<div class=\"wp-block-group\" style=\"padding-top:var(--wp--preset--spacing--50);padding-right:var(--wp--preset--spacing--50);padding-bottom:var(--wp--preset--spacing--50);padding-left:var(--wp--preset--spacing--50)\"><!-- wp:paragraph -->\n<p>Text</p>\n<!-- /wp:paragraph --></div>\n<!-- /wp:group -->");
    }

    #[test]
    fn media_captions_round_trip() {
        let md = assert_lossless("<!-- wp:video {\"id\":44586} -->\n<figure class=\"wp-block-video\"><video controls src=\"https://example.org/film.mp4\"></video><figcaption class=\"wp-element-caption\">Ein Film</figcaption></figure>\n<!-- /wp:video -->");
        assert_eq!(md, "![Ein Film](https://example.org/film.mp4)");
        let md = assert_lossless("<!-- wp:embed {\"url\":\"https://www.youtube.com/watch?v=aqz-KE-bpKQ\",\"type\":\"video\",\"providerNameSlug\":\"youtube\",\"responsive\":true} -->\n<figure class=\"wp-block-embed is-type-video is-provider-youtube wp-block-embed-youtube\"><div class=\"wp-block-embed__wrapper\">\nhttps://www.youtube.com/watch?v=aqz-KE-bpKQ\n</div><figcaption class=\"wp-element-caption\">YouTube-Einbettung</figcaption></figure>\n<!-- /wp:embed -->");
        assert_eq!(md, "https://www.youtube.com/watch?v=aqz-KE-bpKQ\n{caption=\"YouTube-Einbettung\"}");
    }

    #[test]
    fn linked_image_round_trips() {
        let md = assert_lossless("<!-- wp:image {\"id\":45655,\"width\":\"240px\",\"sizeSlug\":\"full\",\"linkDestination\":\"media\",\"align\":\"left\"} -->\n<figure class=\"wp-block-image alignleft size-full is-resized\"><a href=\"https://example.org/a.webp\"><img src=\"https://example.org/a.webp\" alt=\"Alt\" class=\"wp-image-45655\" style=\"width:240px;height:auto\"/></a></figure>\n<!-- /wp:image -->");
        assert!(md.starts_with("[![](https://example.org/a.webp \"Alt\")](https://example.org/a.webp)"), "{md}");
        let md = assert_lossless("<!-- wp:image {\"linkDestination\":\"custom\"} -->\n<figure class=\"wp-block-image\"><a href=\"https://linuxundich.de/\"><img src=\"https://example.org/a.webp\" alt=\"\"/></a><figcaption class=\"wp-element-caption\">BU</figcaption></figure>\n<!-- /wp:image -->");
        assert_eq!(md, "[![BU](https://example.org/a.webp)](https://linuxundich.de/)");
    }

    #[test]
    fn gallery_caption_round_trips() {
        let md = assert_lossless("<!-- wp:gallery {\"linkTo\":\"none\"} -->\n<figure class=\"wp-block-gallery has-nested-images columns-default is-cropped\"><!-- wp:image {\"sizeSlug\":\"large\",\"linkDestination\":\"none\"} -->\n<figure class=\"wp-block-image size-large\"><img src=\"https://example.org/a.webp\" alt=\"\"/></figure>\n<!-- /wp:image --><figcaption class=\"blocks-gallery-caption wp-element-caption\">Galerie</figcaption></figure>\n<!-- /wp:gallery -->");
        assert!(md.ends_with("```\n{caption=\"Galerie\"}"), "{md}");
    }

    #[test]
    fn custom_colors_typography_and_link_color_round_trip() {
        let md = assert_lossless("<!-- wp:paragraph {\"style\":{\"color\":{\"text\":\"#1d4ed8\",\"background\":\"#eef4ff\"},\"typography\":{\"lineHeight\":\"2\",\"letterSpacing\":\"0.05em\",\"textTransform\":\"uppercase\",\"fontStyle\":\"italic\",\"fontWeight\":\"300\"}}} -->\n<p class=\"has-text-color has-background\" style=\"color:#1d4ed8;background-color:#eef4ff;letter-spacing:0.05em;line-height:2;font-style:italic;font-weight:300;text-transform:uppercase\">Text</p>\n<!-- /wp:paragraph -->");
        assert_eq!(md, "Text\n{color=#1d4ed8 bg=#eef4ff line-height=2 letter-spacing=0.05em weight=300 font-style=italic transform=uppercase}");
        let md = assert_lossless("<!-- wp:paragraph {\"fontSize\":\"medium\",\"style\":{\"elements\":{\"link\":{\"color\":{\"text\":\"var:preset|color|warning\"}}}}} -->\n<p class=\"has-link-color has-medium-font-size\">Mit <a href=\"https://linuxundich.de/\">Link</a>.</p>\n<!-- /wp:paragraph -->");
        assert!(md.ends_with("{size=medium link-color=warning}"), "{md}");
    }

    #[test]
    fn list_marker_and_image_aspect_round_trip() {
        let md = assert_lossless("<!-- wp:list {\"ordered\":true,\"type\":\"upper-roman\",\"start\":5} -->\n<ol start=\"5\" style=\"list-style-type:upper-roman\" class=\"wp-block-list\"><!-- wp:list-item -->\n<li>Eins</li>\n<!-- /wp:list-item --></ol>\n<!-- /wp:list -->");
        assert!(md.ends_with("{marker=upper-roman}"), "{md}");
        let md = assert_lossless("<!-- wp:image {\"width\":\"200px\",\"aspectRatio\":\"1\",\"scale\":\"cover\",\"sizeSlug\":\"full\",\"linkDestination\":\"none\"} -->\n<figure class=\"wp-block-image size-full is-resized\"><img src=\"https://example.org/a.webp\" alt=\"Alt\" style=\"aspect-ratio:1;object-fit:cover;width:200px;height:auto\"/></figure>\n<!-- /wp:image -->");
        assert!(md.ends_with("{width=200px aspect=1 scale=cover}"), "{md}");
    }

    #[test]
    fn button_settings_and_alignment_round_trip() {
        let md = assert_lossless("<!-- wp:buttons {\"layout\":{\"type\":\"flex\",\"justifyContent\":\"center\"}} -->\n<div class=\"wp-block-buttons\"><!-- wp:button {\"className\":\"is-style-outline\"} -->\n<div class=\"wp-block-button is-style-outline\"><a class=\"wp-block-button__link wp-element-button\" href=\"https://linuxundich.de/\">Umriss</a></div>\n<!-- /wp:button -->\n\n<!-- wp:button {\"gradient\":\"accent-fade\",\"style\":{\"border\":{\"radius\":\"0px\"},\"dimensions\":{\"width\":\"50%\"}}} -->\n<div class=\"wp-block-button\"><a class=\"wp-block-button__link has-accent-fade-gradient-background has-background wp-element-button\" href=\"https://linuxundich.de/\" style=\"border-radius:0px\" target=\"_blank\" rel=\"noreferrer noopener\">Neuer Tab</a></div>\n<!-- /wp:button --></div>\n<!-- /wp:buttons -->");
        assert_eq!(md, "```buttons\n[Umriss](https://linuxundich.de/){style=outline}\n[Neuer Tab](https://linuxundich.de/){gradient=accent-fade width=50% radius=0px newtab}\n```\n{justify=center}");
    }

    #[test]
    fn link_attributes_markdown_cannot_carry_stay_raw() {
        let button = "<!-- wp:buttons -->\n<div class=\"wp-block-buttons\"><!-- wp:button -->\n<div class=\"wp-block-button\"><a class=\"wp-block-button__link wp-element-button\" href=\"https://linuxundich.de/\" download>Datei</a></div>\n<!-- /wp:button --></div>\n<!-- /wp:buttons -->";
        assert!(gutenberg_to_markdown(button).starts_with("<!-- wp:buttons"));
        let image = "<!-- wp:image {\"linkDestination\":\"custom\"} -->\n<figure class=\"wp-block-image\"><a href=\"https://linuxundich.de/\" target=\"_blank\" rel=\"noreferrer noopener\"><img src=\"https://example.org/a.webp\" alt=\"\"/></a></figure>\n<!-- /wp:image -->";
        assert!(gutenberg_to_markdown(image).starts_with("<!-- wp:image"));
    }
}
