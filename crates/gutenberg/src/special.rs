//! Blocks with a fixed shape and only settings, no Markdown text of their
//! own: a formula (```` ```math ````), a spacer, a file download and an
//! icon. The markup matches WordPress 7.1's own `save()` for each block,
//! so the block editor accepts it unchanged.
//!
//! ```markdown
//! ```math
//! e^{i\pi} + 1 = 0
//! ```
//!
//! ::: spacer {height=40px}
//! :::
//!
//! ::: file {button="Herunterladen" preview}
//! [Handbuch (PDF)](https://example.org/handbuch.pdf)
//! :::
//!
//! ::: icon {icon=core/heart}
//! :::
//! ```

use serde_json::{Map, Value};

use crate::containers::Params;
use crate::{escape_html, Block};

/// The block comment's JSON exactly as WordPress 7.1 serializes it
/// (`serialize_block_attributes`): `\\`, `\"`, `--`, `<`, `>` and `&` as
/// `\u` escapes - so nothing in a value can end the comment, and the REST
/// API, which re-serializes block comments, hands back the same text.
pub fn block_json(json: &Map<String, Value>) -> Option<String> {
    if json.is_empty() {
        return None;
    }
    let text = Value::Object(json.clone()).to_string();
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'\\') => {
                chars.next();
                out.push_str("\\u005c");
            }
            '\\' if chars.peek() == Some(&'"') => {
                chars.next();
                out.push_str("\\u0022");
            }
            '\\' => {
                out.push(c);
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            '-' if chars.peek() == Some(&'-') => {
                chars.next();
                out.push_str("\\u002d\\u002d");
            }
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            _ => out.push(c),
        }
    }
    Some(out)
}

/// The content of `<math>` for `latex` (MathML Core, which browsers render
/// themselves) - `None` when the formula doesn't parse.
pub fn latex_to_mathml(latex: &str) -> Option<String> {
    let converter = math_core::LatexToMathML::new(math_core::MathCoreConfig::default(), Vec::new()).ok()?;
    let mathml = converter.convert_with_local_state(latex, math_core::MathDisplay::Block).ok()?.mathml;
    let start = mathml.find('>')? + 1;
    let end = mathml.rfind("</math>")?;
    (start <= end).then(|| mathml[start..end].to_string())
}

/// `core/math`: the LaTeX source in the comment, its MathML in the block.
/// A formula that doesn't parse still goes up (WordPress shows the error
/// in the editor) with its source as text.
pub fn render_math(latex: &str) -> String {
    let latex = latex.trim();
    let inner = latex_to_mathml(latex).unwrap_or_else(|| format!("<merror><mtext>{}</mtext></merror>", escape_html(latex)));
    let mut json = Map::new();
    json.insert("latex".into(), latex.into());
    crate::wrap("math", block_json(&json), &format!("<div class=\"wp-block-math\"><math display=\"block\">{inner}</math></div>"))
}

/// `core/spacer`: `height` (default 100px, as WordPress's) and `width`.
pub fn render_spacer(params: &Params) -> String {
    let mut json = Map::new();
    let height = params.get("height").unwrap_or("100px");
    if height != "100px" {
        json.insert("height".into(), height.into());
    }
    let mut style = format!("height:{height}");
    if let Some(width) = params.get("width") {
        json.insert("width".into(), width.into());
        style.push_str(&format!(";width:{width}"));
    }
    crate::wrap("spacer", block_json(&json), &format!("<div style=\"{}\" aria-hidden=\"true\" class=\"wp-block-spacer\"></div>", escape_html(&style)))
}

/// The `[Name](url)` of a file container's content, as rendered HTML:
/// `(href, inner HTML)`.
fn content_link(blocks: &[Block]) -> Option<(String, String)> {
    let Some(Block::Paragraph { html }) = blocks.first() else { return None };
    let start = html.find("<a href=\"")? + "<a href=\"".len();
    let href_end = start + html[start..].find('"')?;
    let text_start = href_end + html[href_end..].find('>')? + 1;
    let text_end = html.rfind("</a>")?;
    Some((html[start..href_end].to_string(), html[text_start..text_end].to_string()))
}

/// A stable element id for the file link, as WordPress's own (a random
/// UUID there): derived from the file's address.
fn file_link_id(href: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in href.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("wp-block-file--media-{hash:016x}")
}

/// `core/file`: the content's link is the file name and its address; `href=`
/// when the file itself is elsewhere (a link to the attachment page),
/// `button="…"` / `nobutton`, `preview` (PDF embed) with `height=`,
/// `newtab`, `id=` (the attachment).
pub fn render_file(params: &Params, blocks: &[Block]) -> String {
    let (link, name) = content_link(blocks).unwrap_or_default();
    let href = params.get("href").map(str::to_string).unwrap_or_else(|| link.clone());
    let mut json = Map::new();
    if let Some(id) = params.get("id").and_then(|id| id.parse::<u64>().ok()) {
        json.insert("id".into(), id.into());
    }
    json.insert("href".into(), href.as_str().into());
    let preview = params.flag("preview");
    let height = params.get("height").and_then(|h| h.trim_end_matches("px").parse::<u64>().ok()).unwrap_or(600);
    if preview {
        json.insert("displayPreview".into(), true.into());
        if height != 600 {
            json.insert("previewHeight".into(), height.into());
        }
    }
    let button = !params.flag("nobutton");
    if !button {
        json.insert("showDownloadButton".into(), false.into());
    }
    let id = file_link_id(&href);
    let mut out = String::from("<div class=\"wp-block-file\">");
    if preview {
        let label = if name.is_empty() { "PDF embed".to_string() } else { crate::reverse_plain_text(&name) };
        out.push_str(&format!("<object class=\"wp-block-file__embed\" data=\"{}\" type=\"application/pdf\" style=\"width:100%;height:{height}px\" aria-label=\"{}\"></object>", escape_html(&href), escape_html(&label)));
    }
    if !name.is_empty() {
        let target = if params.flag("newtab") { " target=\"_blank\" rel=\"noreferrer noopener\"" } else { "" };
        out.push_str(&format!("<a id=\"{id}\" href=\"{}\"{target}>{name}</a>", escape_html(&link)));
    }
    if button {
        let text = params.get("button").unwrap_or("Herunterladen");
        let described = if name.is_empty() { String::new() } else { format!(" aria-describedby=\"{id}\"") };
        out.push_str(&format!("<a href=\"{}\" class=\"wp-block-file__button wp-element-button\" download{described}>{}</a>", escape_html(&href), escape_html(text)));
    }
    out.push_str("</div>");
    crate::wrap("file", block_json(&json), &out)
}

/// `core/icon` (rendered by the server): `icon=`, `flip-h`, `flip-v`,
/// `rotate=`.
pub fn render_icon(params: &Params) -> String {
    let mut json = Map::new();
    if let Some(icon) = params.get("icon") {
        json.insert("icon".into(), icon.into());
    }
    if params.flag("flip-h") {
        json.insert("flipHorizontal".into(), true.into());
    }
    if params.flag("flip-v") {
        json.insert("flipVertical".into(), true.into());
    }
    if let Some(rotation) = params.get("rotate").and_then(|r| r.parse::<i64>().ok()).filter(|r| *r != 0) {
        json.insert("rotation".into(), rotation.into());
    }
    match block_json(&json) {
        Some(json) => format!("<!-- wp:icon {json} /-->"),
        None => "<!-- wp:icon /-->".to_string(),
    }
}

/// Settings of a spacer block from its comment JSON.
pub fn spacer_params(json: &mut Map<String, Value>) -> Params {
    let mut params = Params::default();
    if let Some(height) = json.remove("height").and_then(|v| v.as_str().map(str::to_string)).filter(|h| h != "100px") {
        params.set("height", Some(height));
    }
    if let Some(width) = json.remove("width").and_then(|v| v.as_str().map(str::to_string)) {
        params.set("width", Some(width));
    }
    params
}

/// Settings of an icon block from its comment JSON.
pub fn icon_params(json: &mut Map<String, Value>) -> Params {
    let mut params = Params::default();
    if let Some(icon) = json.remove("icon").and_then(|v| v.as_str().map(str::to_string)) {
        params.set("icon", Some(icon));
    }
    if json.remove("flipHorizontal").and_then(|v| v.as_bool()) == Some(true) {
        params.set("flip-h", None);
    }
    if json.remove("flipVertical").and_then(|v| v.as_bool()) == Some(true) {
        params.set("flip-v", None);
    }
    if let Some(rotation) = json.remove("rotation").and_then(|v| v.as_i64()).filter(|r| *r != 0) {
        params.set("rotate", Some(rotation.to_string()));
    }
    params
}

/// The attribute value `name="…"` in a tag.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(" {name}=\"");
    let start = tag.find(&key)? + key.len();
    Some(&tag[start..start + tag[start..].find('"')?])
}

/// A file block read back: settings and the link paragraph (Markdown).
/// `None` when its markup isn't the one `render_file` writes.
pub fn file_parts(json: &mut Map<String, Value>, content: &str, inline_to_markdown: impl Fn(&str) -> String) -> Option<(Params, Vec<Block>)> {
    let mut params = Params::default();
    if let Some(id) = json.get("id").and_then(Value::as_u64) {
        params.set("id", Some(id.to_string()));
    }
    let href = json.remove("href")?.as_str()?.to_string();
    if json.remove("displayPreview").and_then(|v| v.as_bool()) == Some(true) {
        params.set("preview", None);
    }
    if let Some(height) = json.remove("previewHeight").and_then(|v| v.as_u64()).filter(|h| *h != 600) {
        params.set("height", Some(height.to_string()));
    }
    let button = json.remove("showDownloadButton").and_then(|v| v.as_bool()) != Some(false);
    let mut blocks = Vec::new();
    // The name link: the `<a>` without `download`.
    let mut rest = content;
    let mut link = None;
    let mut button_text = None;
    while let Some(start) = rest.find("<a ") {
        let tag_end = start + rest[start..].find('>')?;
        let tag = &rest[start..tag_end];
        let close = tag_end + rest[tag_end..].find("</a>")?;
        let text = &rest[tag_end + 1..close];
        if tag.contains(" download") {
            button_text = Some(text.to_string());
        } else {
            link = Some((attr(tag, "href")?.to_string(), attr(tag, "target").is_some(), text.to_string()));
        }
        rest = &rest[close + 4..];
    }
    if let Some((link_href, newtab, text)) = link {
        if newtab {
            params.set("newtab", None);
        }
        if link_href != href {
            params.set("href", Some(href.clone()));
        }
        blocks.push(Block::Paragraph { html: format!("[{}]({})", inline_to_markdown(&text), link_href) });
    } else {
        params.set("href", Some(href));
    }
    match (button, button_text) {
        (false, _) => params.set("nobutton", None),
        (true, Some(text)) if text != "Herunterladen" => params.set("button", Some(crate::reverse_plain_text(&text))),
        _ => {}
    }
    Some((params, blocks))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_json_cannot_end_the_comment() {
        let mut json = Map::new();
        json.insert("latex".into(), "a --> b < c & d".into());
        assert_eq!(block_json(&json).unwrap(), r#"{"latex":"a \u002d\u002d\u003e b \u003c c \u0026 d"}"#);
    }

    #[test]
    fn backslashes_and_quotes_are_escaped_like_wordpress() {
        let mut json = Map::new();
        json.insert("latex".into(), "\\frac{\"a\"}{b}".into());
        assert_eq!(block_json(&json).unwrap(), r#"{"latex":"\u005cfrac{\u0022a\u0022}{b}"}"#);
    }

    #[test]
    fn a_formula_becomes_mathml() {
        let html = render_math("e^{i\\pi} + 1 = 0");
        assert!(html.starts_with(r#"<!-- wp:math {"latex":"e^{i\u005cpi} + 1 = 0"} -->"#), "{html}");
        assert!(html.contains("<div class=\"wp-block-math\"><math display=\"block\"><m"), "{html}");
        assert!(html.contains("</math></div>"), "{html}");
    }

    #[test]
    fn a_spacer_is_written_like_wordpress() {
        assert_eq!(render_spacer(&Params::default()), "<!-- wp:spacer -->\n<div style=\"height:100px\" aria-hidden=\"true\" class=\"wp-block-spacer\"></div>\n<!-- /wp:spacer -->");
        let mut params = Params::default();
        params.set("height", Some("40px".into()));
        assert_eq!(render_spacer(&params), "<!-- wp:spacer {\"height\":\"40px\"} -->\n<div style=\"height:40px\" aria-hidden=\"true\" class=\"wp-block-spacer\"></div>\n<!-- /wp:spacer -->");
    }

    #[test]
    fn a_file_has_its_link_and_button() {
        let blocks = vec![Block::Paragraph { html: "<a href=\"https://e.org/h.pdf\">Handbuch</a>".into() }];
        let html = render_file(&Params::default(), &blocks);
        let id = file_link_id("https://e.org/h.pdf");
        assert_eq!(html, format!("<!-- wp:file {{\"href\":\"https://e.org/h.pdf\"}} -->\n<div class=\"wp-block-file\"><a id=\"{id}\" href=\"https://e.org/h.pdf\">Handbuch</a><a href=\"https://e.org/h.pdf\" class=\"wp-block-file__button wp-element-button\" download aria-describedby=\"{id}\">Herunterladen</a></div>\n<!-- /wp:file -->"));
    }

    #[test]
    fn an_icon_is_a_self_closing_comment() {
        let mut params = Params::default();
        params.set("icon", Some("core/heart".into()));
        params.set("rotate", Some("90".into()));
        assert_eq!(render_icon(&params), "<!-- wp:icon {\"icon\":\"core/heart\",\"rotation\":90} /-->");
    }

    fn round_trip(markdown: &str) -> String {
        crate::gutenberg_to_markdown(&crate::markdown_to_gutenberg(markdown))
    }

    #[test]
    fn the_new_blocks_round_trip_through_markdown() {
        for md in [
            "```math\ne^{i\\pi} + 1 = 0\n```",
            "::: spacer\n:::",
            "::: spacer {height=40px}\n:::",
            "::: file\n[Handbuch (PDF)](https://e.org/h.pdf)\n:::",
            "::: file {preview newtab button=Laden}\n[Handbuch](https://e.org/h.pdf)\n:::",
            "::: icon {icon=core/heart rotate=90}\n:::",
        ] {
            assert_eq!(round_trip(md).trim(), md, "{}", crate::markdown_to_gutenberg(md));
        }
    }

    #[test]
    fn wordpress_markup_of_the_new_blocks_becomes_markdown() {
        // As WordPress 7.1 saves them (MathML from its own converter).
        let math = "<!-- wp:math {\"latex\":\"x^2\"} -->\n<div class=\"wp-block-math\"><math display=\"block\"><semantics><msup><mi>x</mi><mn>2</mn></msup><annotation encoding=\"application/x-tex\">x^2</annotation></semantics></math></div>\n<!-- /wp:math -->";
        assert_eq!(crate::gutenberg_to_markdown(math).trim(), "```math\nx^2\n```");
        let spacer = "<!-- wp:spacer {\"height\":\"24px\"} -->\n<div style=\"height:24px\" aria-hidden=\"true\" class=\"wp-block-spacer\"></div>\n<!-- /wp:spacer -->";
        assert_eq!(crate::gutenberg_to_markdown(spacer).trim(), "::: spacer {height=24px}\n:::");
        let file = "<!-- wp:file {\"id\":77,\"href\":\"https://e.org/a.pdf\"} -->\n<div class=\"wp-block-file\"><a id=\"wp-block-file--media-1b2c3d4e-0000-4000-8000-000000000000\" href=\"https://e.org/a.pdf\">a.pdf</a><a href=\"https://e.org/a.pdf\" class=\"wp-block-file__button wp-element-button\" download aria-describedby=\"wp-block-file--media-1b2c3d4e-0000-4000-8000-000000000000\">Herunterladen</a></div>\n<!-- /wp:file -->";
        assert_eq!(crate::gutenberg_to_markdown(file).trim(), "::: file {id=77}\n[a.pdf](https://e.org/a.pdf)\n:::");
        let icon = "<!-- wp:icon {\"icon\":\"core/star\"} /-->";
        assert_eq!(crate::gutenberg_to_markdown(icon).trim(), "::: icon {icon=core/star}\n:::");
    }
}
