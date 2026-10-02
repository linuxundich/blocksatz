//! Container blocks - blocks that hold other blocks - written as fenced
//! divs, the convention Pandoc, MyST and markdown-it-container share:
//!
//! ```markdown
//! :::: accordion
//! ::: item "Wie funktioniert das?" {open}
//! Ganz normales *Markdown*.
//! :::
//! ::::
//! ```
//!
//! The opening line names the container, optionally a quoted title (an
//! accordion item's heading, a tab's label, a details summary), and its
//! settings, with or without braces. A line of three or more colons closes
//! the innermost open container; using more colons for outer containers is
//! only for readability. Everything between is an ordinary Markdown
//! document, so containers nest and their content stays editable.

use serde_json::{Map, Value};

use crate::{escape_html, render_blocks, wrap, Block, BlockAttrs};

/// The container kinds this crate knows - anything else after `:::` is
/// just text.
pub const KINDS: &[&str] = &["group", "columns", "column", "accordion", "item", "tabs", "tab", "cover", "details", "media-text"];

/// A container's own settings (`open`, `image=...`, `layout=flex`) -
/// whatever isn't a general block attribute.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Params(pub Vec<(String, Option<String>)>);

impl Params {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_deref())
    }

    pub fn flag(&self, key: &str) -> bool {
        self.0.iter().any(|(k, v)| k == key && v.is_none())
    }

    pub fn set(&mut self, key: &str, value: Option<String>) {
        self.0.retain(|(k, _)| k != key);
        self.0.push((key.to_string(), value));
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A parsed opening line.
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub colons: usize,
    pub kind: String,
    pub title: Option<String>,
    pub params: Params,
    pub attrs: BlockAttrs,
}

/// Parses `::: kind "Titel" {key=value flag}`. `None` if the line isn't a
/// container opening (unknown kind, unbalanced quotes, a closing line).
pub fn parse_header(line: &str) -> Option<Header> {
    let colons = line.chars().take_while(|c| *c == ':').count();
    if colons < 3 {
        return None;
    }
    let rest = line[colons..].trim();
    let kind_end = rest.find(|c: char| c.is_whitespace() || c == '{').unwrap_or(rest.len());
    let kind = &rest[..kind_end];
    if !KINDS.contains(&kind) {
        return None;
    }
    let mut tokens = crate::attrs::split_tokens_keeping_quotes(rest[kind_end..].trim())?;
    let mut title = None;
    if tokens.first().is_some_and(|t| t.starts_with('"')) {
        title = Some(unquote(&tokens.remove(0)));
    }
    let mut params = Params::default();
    let mut attrs = BlockAttrs::default();
    for token in tokens {
        let token = token.trim_start_matches('{').trim_end_matches('}');
        if token.is_empty() {
            continue;
        }
        match BlockAttrs::parse_tokens(token) {
            Some(parsed) => attrs = attrs.merged(parsed),
            None => match token.split_once('=') {
                Some((key, value)) => params.set(key, Some(unquote(value))),
                None => params.set(token, None),
            },
        }
    }
    // Settings that look like general attributes but belong to the
    // container itself.
    if kind == "column" || kind == "media-text" {
        if let Some(width) = attrs.width.take() {
            params.set("width", Some(width));
        }
    }
    if kind == "media-text" {
        if let Some(size) = attrs.font_size.take() {
            params.set("size", Some(size));
        }
    }
    if kind == "cover" {
        if let Some(gradient) = attrs.gradient.take() {
            params.set("gradient", Some(gradient));
        }
        if let Some(overlay) = attrs.background.take() {
            params.set("overlay", Some(overlay));
        }
    }
    Some(Header { colons, kind: kind.to_string(), title, params, attrs })
}

/// The kinds whose `image=` is an image file (and `alt=` its alt text).
pub const IMAGE_KINDS: &[&str] = &["cover", "media-text"];

/// Every container image in `md`, nested ones included: `(image, alt)`.
/// They aren't Markdown images, so the media list and the upload need
/// them pointed out.
pub fn images(md: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    for segment in crate::split_segments(md) {
        if let crate::Segment::Container { header, inner, .. } = segment {
            if IMAGE_KINDS.contains(&header.kind.as_str()) && header.params.get("type") != Some("video") {
                if let Some(image) = header.params.get("image") {
                    out.push((image.to_string(), header.params.get("alt").map(str::to_string)));
                }
            }
            out.extend(images(&md[inner]));
        }
    }
    out
}

/// A line of nothing but three or more colons.
pub fn is_closing(line: &str) -> bool {
    let trimmed = line.trim_end();
    trimmed.len() >= 3 && trimmed.chars().all(|c| c == ':')
}

fn unquote(s: &str) -> String {
    let s = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s);
    s.replace("\\\"", "\"")
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\\\""))
}

/// The opening line for Markdown output.
pub fn header_markdown(colons: usize, kind: &str, title: Option<&str>, params: &Params, attrs: &BlockAttrs) -> String {
    let mut line = format!("{} {kind}", ":".repeat(colons));
    if let Some(title) = title {
        line.push(' ');
        line.push_str(&quote(title));
    }
    let mut tokens: Vec<String> = params
        .0
        .iter()
        .map(|(k, v)| match v {
            Some(v) if v.contains(char::is_whitespace) || v.is_empty() => format!("{k}={}", quote(v)),
            Some(v) => format!("{k}={v}"),
            None => k.clone(),
        })
        .collect();
    if !attrs.is_empty() {
        let attr_text = attrs.to_markdown();
        tokens.push(attr_text[1..attr_text.len() - 1].to_string());
    }
    if !tokens.is_empty() {
        line.push_str(&format!(" {{{}}}", tokens.join(" ")));
    }
    line
}

/// Inline Markdown (a title) as HTML.
fn inline(markdown: &str) -> String {
    match crate::parse_plain_markdown(markdown).into_iter().next() {
        Some(Block::Paragraph { html }) => html,
        _ => escape_html(markdown),
    }
}

/// Plain text of a title, for attributes like a tab's `label`.
fn plain(markdown: &str) -> String {
    let html = inline(markdown);
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"")
}

fn json_comment(json: Map<String, Value>) -> Option<String> {
    (!json.is_empty()).then(|| Value::Object(json).to_string())
}

/// WordPress's own slug for a tab label (`cleanForSlug`): accents
/// removed, lowercase, everything else a dash.
pub fn slug(label: &str) -> String {
    let mut out = String::new();
    for c in label.to_lowercase().chars() {
        let mapped: &str = match c {
            'ä' | 'à' | 'á' | 'â' | 'ã' | 'å' => "a",
            'ö' | 'ò' | 'ó' | 'ô' | 'õ' => "o",
            'ü' | 'ù' | 'ú' | 'û' => "u",
            'é' | 'è' | 'ê' | 'ë' => "e",
            'í' | 'ì' | 'î' | 'ï' => "i",
            'ç' => "c",
            'ñ' => "n",
            'ß' => "ss",
            _ => "",
        };
        if !mapped.is_empty() {
            out.push_str(mapped);
        } else if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// Gutenberg markup for one container.
pub fn render(kind: &str, title: Option<&str>, params: &Params, blocks: &[Block]) -> String {
    match kind {
        "group" => render_group(params, blocks),
        "columns" => {
            let mut json = Map::new();
            let mut classes = String::from("wp-block-columns");
            if let Some(valign) = params.get("valign") {
                json.insert("verticalAlignment".into(), valign.into());
                classes.push_str(&format!(" are-vertically-aligned-{valign}"));
            }
            if params.flag("nostack") {
                json.insert("isStackedOnMobile".into(), false.into());
                classes.push_str(" is-not-stacked-on-mobile");
            }
            wrap("columns", json_comment(json), &format!("<div class=\"{classes}\">{}</div>", render_children(blocks)))
        }
        "column" => {
            let mut json = Map::new();
            let mut classes = String::from("wp-block-column");
            let mut style = String::new();
            if let Some(valign) = params.get("valign") {
                json.insert("verticalAlignment".into(), valign.into());
                classes.push_str(&format!(" is-vertically-aligned-{valign}"));
            }
            if let Some(width) = params.get("width") {
                json.insert("width".into(), width.into());
                style = format!(" style=\"flex-basis:{width}\"");
            }
            wrap("column", json_comment(json), &format!("<div class=\"{classes}\"{style}>{}</div>", render_children(blocks)))
        }
        "accordion" => wrap("accordion", None, &format!("<div role=\"group\" class=\"wp-block-accordion\">{}</div>", render_children(blocks))),
        "item" => render_accordion_item(title.unwrap_or(""), params, blocks),
        "tabs" => render_tabs(blocks),
        "tab" => render_tab_panel(title.unwrap_or(""), params, blocks),
        "cover" => render_cover(params, blocks),
        "media-text" => render_media_text(params, blocks),
        "details" => {
            let mut json = Map::new();
            let open = if params.flag("open") {
                json.insert("showContent".into(), true.into());
                " open"
            } else {
                ""
            };
            wrap("details", json_comment(json), &format!("<details class=\"wp-block-details\"{open}><summary>{}</summary>{}</details>", inline(title.unwrap_or("")), render_children(blocks)))
        }
        _ => render_blocks(blocks),
    }
}

fn render_children(blocks: &[Block]) -> String {
    if blocks.is_empty() {
        String::new()
    } else {
        render_blocks(blocks)
    }
}

/// `layout=` values -> the group's `layout` object.
fn render_group(params: &Params, blocks: &[Block]) -> String {
    let mut json = Map::new();
    if let Some(layout) = params.get("layout") {
        let mut object = Map::new();
        match layout {
            "flex" | "row" => {
                object.insert("type".into(), "flex".into());
            }
            "stack" => {
                object.insert("type".into(), "flex".into());
                object.insert("orientation".into(), "vertical".into());
            }
            other => {
                object.insert("type".into(), other.into());
            }
        }
        if params.flag("nowrap") {
            object.insert("flexWrap".into(), "nowrap".into());
        }
        if let Some(justify) = params.get("justify") {
            object.insert("justifyContent".into(), justify.into());
        }
        if let Some(columns) = params.get("columns").and_then(|c| c.parse::<u64>().ok()) {
            object.insert("columnCount".into(), columns.into());
        }
        json.insert("layout".into(), Value::Object(object));
    }
    let tag = params.get("tag").unwrap_or("div");
    if tag != "div" {
        json.insert("tagName".into(), tag.into());
    }
    wrap("group", json_comment(json), &format!("<{tag} class=\"wp-block-group\">{}</{tag}>", render_children(blocks)))
}

fn render_accordion_item(title: &str, params: &Params, blocks: &[Block]) -> String {
    let open = params.flag("open");
    let level = params.get("level").and_then(|l| l.parse::<u8>().ok()).filter(|l| (1..=6).contains(l)).unwrap_or(3);
    let mut json = Map::new();
    if open {
        json.insert("openByDefault".into(), true.into());
    }
    let mut heading_json = json.clone();
    if level != 3 {
        heading_json.insert("level".into(), level.into());
    }
    let heading = wrap(
        "accordion-heading",
        json_comment(heading_json),
        &format!(
            "<h{level} class=\"wp-block-accordion-heading has-icon has-icon-right\"><button type=\"button\" class=\"wp-block-accordion-heading__toggle\"><span class=\"wp-block-accordion-heading__toggle-title\">{}</span><span class=\"wp-block-accordion-heading__toggle-icon\" aria-hidden=\"true\">+</span></button></h{level}>",
            inline(title)
        ),
    );
    let panel = wrap("accordion-panel", None, &format!("<div role=\"region\" class=\"wp-block-accordion-panel\">{}</div>", render_children(blocks)));
    let class = if open { "wp-block-accordion-item is-open" } else { "wp-block-accordion-item" };
    wrap("accordion-item", json_comment(json), &format!("<div class=\"{class}\">{heading}\n\n{panel}</div>"))
}

fn tab_anchor(title: &str, params: &Params) -> String {
    params.get("anchor").map(str::to_string).unwrap_or_else(|| slug(&plain(title)))
}

fn render_tabs(blocks: &[Block]) -> String {
    let buttons: String = blocks
        .iter()
        .filter_map(|block| match block.unstyled() {
            Block::Container { kind, title, .. } if kind == "tab" => Some(format!("<button type=\"button\" role=\"tab\">{}</button>", escape_html(&plain(title.as_deref().unwrap_or(""))))),
            _ => None,
        })
        .collect();
    let list = wrap("tab-list", None, &format!("<div role=\"tablist\" class=\"wp-block-tab-list\">{buttons}</div>"));
    let panels = wrap("tab-panels", None, &format!("<div class=\"wp-block-tab-panels\">{}</div>", render_children(blocks)));
    wrap("tabs", None, &format!("<div class=\"wp-block-tabs\">{list}\n\n{panels}</div>"))
}

fn render_tab_panel(title: &str, params: &Params, blocks: &[Block]) -> String {
    let label = plain(title);
    let anchor = tab_anchor(title, params);
    let mut json = Map::new();
    json.insert("label".into(), label.into());
    json.insert("anchor".into(), anchor.clone().into());
    wrap("tab-panel", json_comment(json), &format!("<section role=\"tabpanel\" tabindex=\"0\" id=\"{}\" class=\"wp-block-tab-panel\">{}</section>", escape_html(&anchor), render_children(blocks)))
}

fn render_cover(params: &Params, blocks: &[Block]) -> String {
    let mut json = Map::new();
    let image = params.get("image");
    let parallax = params.flag("parallax");
    let image_id = params.get("id").and_then(|id| id.parse::<u64>().ok());
    if let Some(image) = image {
        json.insert("url".into(), image.into());
        if let Some(id) = image_id {
            json.insert("id".into(), id.into());
        }
        if parallax {
            json.insert("hasParallax".into(), true.into());
        }
    }
    let dim: u64 = match params.get("dim").and_then(|d| d.parse::<u64>().ok()) {
        Some(dim) => {
            json.insert("dimRatio".into(), dim.into());
            dim
        }
        None => 100,
    };
    let overlay = params.get("overlay");
    if let Some(overlay) = overlay {
        json.insert("overlayColor".into(), overlay.into());
    }
    let gradient = params.get("gradient");
    if let Some(gradient) = gradient {
        json.insert("gradient".into(), gradient.into());
    }
    let mut style = String::new();
    if let Some(height) = params.get("height") {
        let split = height.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(height.len());
        let (number, unit) = height.split_at(split);
        if let Ok(number) = number.parse::<f64>() {
            json.insert("minHeight".into(), serde_json::Number::from_f64(number).map(|n| if number.fract() == 0.0 { Value::from(number as u64) } else { Value::Number(n) }).unwrap_or(Value::Null));
            let unit = if unit.is_empty() { "px" } else { unit };
            json.insert("minHeightUnit".into(), unit.into());
            style = format!(" style=\"min-height:{number}{unit}\"");
        }
    }
    let mut classes = String::from("wp-block-cover");
    if let Some(position) = params.get("position") {
        json.insert("contentPosition".into(), position.into());
        if position != "center center" {
            classes.push_str(&format!(" has-custom-content-position is-position-{}", position.replace(' ', "-")));
        }
    }
    if parallax && image.is_some() {
        classes.push_str(" has-parallax");
    }

    let id_class = image_id.map(|id| format!(" wp-image-{id}")).unwrap_or_default();
    let background = match image {
        Some(url) if parallax => format!("<div class=\"wp-block-cover__image-background{id_class} has-parallax\" style=\"background-position:50% 50%;background-image:url({})\"></div>", escape_html(url)),
        Some(url) => format!("<img class=\"wp-block-cover__image-background{id_class}\" alt=\"\" src=\"{}\" data-object-fit=\"cover\"/>", escape_html(url)),
        None => String::new(),
    };
    let mut span_classes = String::from("wp-block-cover__background");
    if let Some(overlay) = overlay {
        span_classes.push_str(&format!(" has-{overlay}-background-color"));
    }
    if dim > 0 {
        span_classes.push_str(&format!(" has-background-dim-{} has-background-dim", (dim + 5) / 10 * 10));
    }
    if let Some(gradient) = gradient {
        span_classes.push_str(&format!(" has-background-gradient has-{gradient}-gradient-background"));
    }
    wrap(
        "cover",
        json_comment(json),
        &format!("<div class=\"{classes}\"{style}>{background}<span aria-hidden=\"true\" class=\"{span_classes}\"></span><div class=\"wp-block-cover__inner-container\">{}</div></div>", render_children(blocks)),
    )
}

/// `wp:media-text`: an image (or video) beside the content.
/// `image=` and `id=` like a cover, `alt=`, `position=right`, `valign=`,
/// `fill` (crop the image to fill its half), `width=` (the media column in
/// percent), `nostack`, `size=` (the image size, default full),
/// `type=video`.
fn render_media_text(params: &Params, blocks: &[Block]) -> String {
    let mut json = Map::new();
    let right = params.get("position") == Some("right");
    if right {
        json.insert("mediaPosition".into(), "right".into());
    }
    let id = params.get("id").and_then(|id| id.parse::<u64>().ok());
    if let Some(id) = id {
        json.insert("mediaId".into(), id.into());
    }
    let video = params.get("type") == Some("video");
    if params.get("image").is_some() {
        json.insert("mediaType".into(), if video { "video" } else { "image" }.into());
    }
    let size = params.get("size").unwrap_or("full");
    if size != "full" {
        json.insert("mediaSizeSlug".into(), size.into());
    }
    let width = params.get("width").and_then(|w| w.trim_end_matches('%').parse::<u64>().ok()).filter(|w| *w != 50);
    if let Some(width) = width {
        json.insert("mediaWidth".into(), width.into());
    }
    let nostack = params.flag("nostack");
    if nostack {
        json.insert("isStackedOnMobile".into(), false.into());
    }
    let valign = params.get("valign");
    if let Some(valign) = valign {
        json.insert("verticalAlignment".into(), valign.into());
    }
    let fill = params.flag("fill");
    if fill {
        json.insert("imageFill".into(), true.into());
    }

    let mut classes = String::from("wp-block-media-text");
    if right {
        classes.push_str(" has-media-on-the-right");
    }
    if !nostack {
        classes.push_str(" is-stacked-on-mobile");
    }
    if let Some(valign) = valign {
        classes.push_str(&format!(" is-vertically-aligned-{valign}"));
    }
    if fill {
        classes.push_str(" is-image-fill-element");
    }
    let style = width.map(|w| if right { format!(" style=\"grid-template-columns:auto {w}%\"") } else { format!(" style=\"grid-template-columns:{w}% auto\"") }).unwrap_or_default();
    let media = match params.get("image") {
        Some(url) if video => format!("<video controls src=\"{}\"></video>", escape_html(url)),
        Some(url) => {
            let id_class = id.map(|id| format!("wp-image-{id} ")).unwrap_or_default();
            let position = if fill { " style=\"object-position:50% 50%\"" } else { "" };
            format!("<img src=\"{}\" alt=\"{}\" class=\"{id_class}size-{size}\"{position}/>", escape_html(url), escape_html(params.get("alt").unwrap_or("")))
        }
        None => String::new(),
    };
    let figure = format!("<figure class=\"wp-block-media-text__media\">{media}</figure>");
    let content = format!("<div class=\"wp-block-media-text__content\">{}</div>", render_children(blocks));
    let inner = if right { format!("{content}{figure}") } else { format!("{figure}{content}") };
    wrap("media-text", json_comment(json), &format!("<div class=\"{classes}\"{style}>{inner}</div>"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_header_with_title_flags_and_attributes() {
        let header = parse_header(":::: item \"Wie \\\"genau\\\"?\" {open bg=base-2}").unwrap();
        assert_eq!(header.colons, 4);
        assert_eq!(header.kind, "item");
        assert_eq!(header.title.as_deref(), Some("Wie \"genau\"?"));
        assert!(header.params.flag("open"));
        assert_eq!(header.attrs.background.as_deref(), Some("base-2"));
    }

    #[test]
    fn unknown_kinds_are_not_containers() {
        assert_eq!(parse_header("::: warnung"), None);
        assert_eq!(parse_header(":: group"), None);
        assert!(is_closing(":::"));
        assert!(!is_closing("::: group"));
    }

    #[test]
    fn cover_takes_its_own_gradient_and_overlay() {
        let header = parse_header("::: cover {gradient=hero-overlay bg=contrast height=260px align=full}").unwrap();
        assert_eq!(header.params.get("gradient"), Some("hero-overlay"));
        assert_eq!(header.params.get("overlay"), Some("contrast"));
        assert_eq!(header.attrs.align.as_deref(), Some("full"));
        assert!(header.attrs.gradient.is_none());
    }

    #[test]
    fn header_round_trips() {
        let header = parse_header("::: tab \"Reiter 1\" {anchor=eins .extra}").unwrap();
        let line = header_markdown(3, &header.kind, header.title.as_deref(), &header.params, &header.attrs);
        assert_eq!(parse_header(&line), Some(header));
    }

    #[test]
    fn finds_container_images_also_nested() {
        let md = "::: cover {image=titel.png}\n# Titel\n:::\n\n:::: group\n::: media-text {image=\"mein bild.png\" alt=\"Ein Bild\"}\nText\n:::\n::::\n\n::: media-text {image=film.mp4 type=video}\n:::";
        assert_eq!(images(md), vec![("titel.png".to_string(), None), ("mein bild.png".to_string(), Some("Ein Bild".to_string()))]);
    }

    #[test]
    fn slugs_like_wordpress() {
        assert_eq!(slug("Reiter 1"), "reiter-1");
        assert_eq!(slug("Größe & Maße"), "grosse-masse");
    }
}
