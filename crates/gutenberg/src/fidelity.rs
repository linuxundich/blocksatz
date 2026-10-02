//! The safety net behind `gutenberg_to_markdown`: a block only becomes
//! Markdown if rendering that Markdown again reproduces the block's
//! *structure* - its comment attributes, its block-level elements and their
//! classes/styles. Anything the Markdown form can't carry (a custom border,
//! padding, a table footer, a linked image, ...) makes the comparison fail
//! and the block stays as its original markup instead of silently losing
//! that part of its design on the next upload.
//!
//! Inline content (text, links, emphasis) is deliberately not compared: the
//! inline converter keeps everything it can't express as inline HTML, which
//! Markdown passes through unchanged.

use serde_json::{Map, Value};

use crate::attrs::{parse_tag_attrs, split_comment};

/// Comment attributes that don't need to survive: they're re-derived on
/// export (`id` from `Frontmatter.media`) or are WordPress defaults.
const TOLERATED_JSON: &[&str] = &["id", "sizeSlug"];

/// Classes WordPress adds by itself or that only mirror tolerated JSON.
fn tolerated_class(class: &str) -> bool {
    matches!(class, "wp-block-heading" | "wp-block-list") || class.starts_with("wp-image-") || class.starts_with("size-")
}

/// Inline elements - ignored, see the module docs.
const INLINE_TAGS: &[&str] = &[
    "a", "abbr", "b", "bdi", "bdo", "br", "code", "data", "del", "dfn", "em", "i", "ins", "kbd", "mark", "q", "s", "samp", "small", "span", "strong", "sub", "sup", "time", "u", "var", "wbr",
];

/// Attributes of block-level elements that carry design.
const COMPARED_ATTRS: &[&str] = &["class", "style", "id", "start", "reversed", "type", "colspan", "rowspan", "open", "scope"];

/// Whether `rendered` (our own output for the parsed block) keeps
/// everything structural about `original`.
pub fn same_structure(original: &str, rendered: &str) -> bool {
    skeleton(original) == skeleton(rendered)
}

/// The first structural difference, for diagnostics - `None` if there is
/// none.
pub fn first_difference(original: &str, rendered: &str) -> Option<String> {
    let (a, b) = (skeleton(original), skeleton(rendered));
    let at = a.iter().zip(&b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
    (a != b).then(|| format!("#{at}: {:?} != {:?}", a.get(at), b.get(at)))
}

#[derive(Debug, PartialEq)]
enum Token {
    Comment { name: String, json: Value },
    Tag { name: String, attrs: Vec<(String, String)> },
    /// An `<a>` directly wrapping an `<img>` - a linked image, which plain
    /// Markdown image syntax can't express.
    ImageLink(String),
}

fn skeleton(html: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut i = 0;
    while let Some(rel) = html[i..].find('<') {
        let start = i + rel;
        if html[start..].starts_with("<!--") {
            let Some(end_rel) = html[start..].find("-->") else { break };
            let comment = &html[start..start + end_rel + 3];
            i = start + end_rel + 3;
            if let Some(token) = comment_token(comment) {
                tokens.push(token);
            }
            continue;
        }
        let Some(end_rel) = html[start..].find('>') else { break };
        let tag = &html[start + 1..start + end_rel];
        i = start + end_rel + 1;
        let closing = tag.starts_with('/');
        let body = tag.trim_start_matches('/').trim_end_matches('/');
        let name_end = body.find(char::is_whitespace).unwrap_or(body.len());
        let name = body[..name_end].to_lowercase();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric()) {
            continue;
        }
        if name == "a" && !closing && html[i..].trim_start().starts_with("<img") {
            let href = parse_tag_attrs(&body[name_end..]).into_iter().find(|(k, _)| k == "href").map(|(_, v)| v).unwrap_or_default();
            tokens.push(Token::ImageLink(href));
            continue;
        }
        if INLINE_TAGS.contains(&name.as_str()) {
            continue;
        }
        if closing {
            tokens.push(Token::Tag { name: format!("/{name}"), attrs: Vec::new() });
            continue;
        }
        let mut attrs: Vec<(String, String)> = parse_tag_attrs(&body[name_end..])
            .into_iter()
            .filter(|(k, _)| COMPARED_ATTRS.contains(&k.as_str()))
            .filter_map(|(k, v)| {
                let v = match k.as_str() {
                    "class" => {
                        let mut classes: Vec<&str> = v.split_whitespace().filter(|c| !tolerated_class(c)).collect();
                        classes.sort_unstable();
                        classes.dedup();
                        classes.join(" ")
                    }
                    "style" => normalize_style(&v),
                    _ => v,
                };
                (!(v.is_empty() && matches!(k.as_str(), "class" | "style"))).then_some((k, v))
            })
            .collect();
        attrs.sort();
        tokens.push(Token::Tag { name, attrs });
    }
    tokens
}

fn comment_token(comment: &str) -> Option<Token> {
    // Explicit classic-block delimiters (see `reverse::push_stray`) stand
    // for content that had none.
    if comment.contains("wp:freeform") {
        return None;
    }
    let closing = comment.starts_with("<!-- /wp:");
    if closing {
        let name = comment.trim_start_matches("<!-- /wp:").trim_end_matches("-->").trim();
        return Some(Token::Comment { name: format!("/{name}"), json: Value::Null });
    }
    let normalized = comment.replace("/-->", "-->");
    let (name, mut json, _) = split_comment(&normalized)?;
    strip_tolerated(&mut json);
    Some(Token::Comment { name, json: Value::Object(json) })
}

fn strip_tolerated(json: &mut Map<String, Value>) {
    for key in TOLERATED_JSON {
        json.remove(*key);
    }
    if json.get("linkDestination").and_then(Value::as_str) == Some("none") {
        json.remove("linkDestination");
    }
}

fn normalize_style(style: &str) -> String {
    let mut parts: Vec<String> = style.split(';').map(|p| p.split_whitespace().collect::<String>()).filter(|p| !p.is_empty()).collect();
    parts.sort();
    parts.join(";")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_order_and_tolerated_classes_do_not_matter() {
        assert!(same_structure(
            "<!-- wp:heading {\"level\":3} -->\n<h3 class=\"wp-block-heading has-a has-b\">X</h3>\n<!-- /wp:heading -->",
            "<!-- wp:heading {\"level\":3} --><h3 class=\"has-b has-a\">Y</h3><!-- /wp:heading -->"
        ));
    }

    #[test]
    fn a_dropped_style_is_noticed() {
        assert!(!same_structure("<!-- wp:paragraph --><p style=\"padding:1rem\">X</p><!-- /wp:paragraph -->", "<!-- wp:paragraph --><p>X</p><!-- /wp:paragraph -->"));
    }

    #[test]
    fn a_dropped_attribute_is_noticed() {
        assert!(!same_structure("<!-- wp:quote {\"className\":\"is-style-plain\"} --><blockquote></blockquote><!-- /wp:quote -->", "<!-- wp:quote --><blockquote></blockquote><!-- /wp:quote -->"));
    }

    #[test]
    fn a_linked_image_is_noticed() {
        assert!(!same_structure("<figure><a href=\"x\"><img src=\"a\"/></a></figure>", "<figure><img src=\"a\"/></figure>"));
    }

    #[test]
    fn inline_markup_is_ignored() {
        assert!(same_structure("<p>a <strong>b</strong> <a href=\"x\">c</a></p>", "<p>a b c</p>"));
    }
}
