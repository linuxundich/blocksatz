//! Footnotes, written the way GitHub and Pandoc do:
//!
//! ```markdown
//! Ein Satz mit Beleg.[^1]
//!
//! [^1]: Die Quelle, gern mit *Auszeichnung* oder [Link](https://…).
//! ```
//!
//! WordPress keeps footnotes apart from the text: each reference is a
//! `<sup data-fn="id">` in the content, the notes themselves are the
//! post's `footnotes` meta (JSON), and the dynamic `wp:footnotes` block
//! shows the list. [`extract`] turns the Markdown form into the first two
//! before the body is parsed; [`to_markdown`] goes the other way for a
//! post opened from the blog.
//!
//! A top-level verbatim block is left alone in both directions; code
//! spans and fences are skipped on the way out.

use crate::Segment;

#[derive(Debug, Clone, PartialEq)]
pub struct Footnote {
    /// What WordPress identifies the note by (`data-fn`, the list item's
    /// `id`) - derived from the label, so it stays the same on every
    /// upload.
    pub id: String,
    /// The label as written (`1`, `quelle`).
    pub label: String,
    /// The note as inline HTML.
    pub content: String,
}

/// The block that lists the notes.
pub const LIST_BLOCK: &str = "<!-- wp:footnotes /-->";

/// A stable, UUID-shaped id for `label` (two FNV-1a hashes) - WordPress
/// itself uses random UUIDs, any unique string works.
pub fn footnote_id(label: &str) -> String {
    fn fnv(seed: u64, text: &str) -> u64 {
        text.bytes().fold(seed, |hash, byte| (hash ^ byte as u64).wrapping_mul(0x100000001b3))
    }
    let a = fnv(0xcbf29ce484222325, label);
    let b = fnv(0x84222325cbf29ce4, &format!("blocksatz-footnote:{label}"));
    let hex = format!("{a:016x}{b:016x}");
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// The reference as WordPress's editor writes it.
pub fn reference_html(id: &str, number: usize) -> String {
    format!("<sup data-fn=\"{id}\" class=\"fn\"><a href=\"#{id}\" id=\"{id}-link\">{number}</a></sup>")
}

/// The `footnotes` meta value.
pub fn meta_json(notes: &[Footnote]) -> String {
    let items: Vec<serde_json::Value> = notes.iter().map(|note| serde_json::json!({"content": note.content, "id": note.id})).collect();
    serde_json::Value::Array(items).to_string()
}

/// Ranges of `md` that are verbatim blocks at the top level.
fn raw_ranges(md: &str) -> Vec<std::ops::Range<usize>> {
    crate::split_segments(md)
        .into_iter()
        .filter_map(|segment| match segment {
            Segment::Raw(range) => Some(range),
            _ => None,
        })
        .collect()
}

/// `[^label]: text` at the start of a line - the label and the text.
fn definition_start(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("[^")?;
    let close = rest.find("]:")?;
    let label = &rest[..close];
    if label.is_empty() || label.contains(char::is_whitespace) || label.contains(['[', ']']) {
        return None;
    }
    Some((label, rest[close + 2..].trim()))
}

fn is_fence(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    let c = trimmed.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let n = trimmed.chars().take_while(|x| *x == c).count();
    (n >= 3).then_some((c, n))
}

/// Takes the footnote definitions out of `md` and replaces every reference
/// to one of them with WordPress's `<sup>` markup, numbered in order of
/// first reference. Notes without a reference come last; a reference
/// without a definition stays text. Definition lines become empty lines. With no definitions at all `md` comes
/// back unchanged.
pub fn extract(md: &str) -> (String, Vec<Footnote>) {
    let raw = raw_ranges(md);
    let in_raw = |pos: usize| raw.iter().any(|r| r.contains(&pos));

    // Pass 1: definitions out, the rest kept line by line.
    let mut body = String::with_capacity(md.len());
    let mut definitions: Vec<(String, String)> = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    let mut current: Option<usize> = None;
    let mut pos = 0;
    for line in md.split_inclusive('\n') {
        let start = pos;
        pos += line.len();
        let text = line.trim_end_matches(['\n', '\r']);
        if in_raw(start) {
            current = None;
            body.push_str(line);
            continue;
        }
        if let Some((c, n)) = fence {
            if is_fence(text).is_some_and(|(c2, n2)| c2 == c && n2 >= n) && text.trim().chars().all(|x| x == c) {
                fence = None;
            }
            body.push_str(line);
            continue;
        }
        if let Some(index) = current {
            // An indented line continues the definition.
            if !text.trim().is_empty() && (text.starts_with("  ") || text.starts_with('\t')) {
                let note = &mut definitions[index].1;
                note.push('\n');
                note.push_str(text.trim());
                keep_line_count(line, &mut body);
                continue;
            }
            current = None;
        }
        if let Some((label, note)) = definition_start(text) {
            if !definitions.iter().any(|(l, _)| l == label) {
                definitions.push((label.to_string(), note.to_string()));
                current = Some(definitions.len() - 1);
            }
            keep_line_count(line, &mut body);
            continue;
        }
        if let Some(f) = is_fence(text) {
            fence = Some(f);
        }
        body.push_str(line);
    }
    if definitions.is_empty() {
        return (md.to_string(), Vec::new());
    }

    // Pass 2: references, outside fences, code spans and verbatim blocks.
    let raw = raw_ranges(&body);
    let mut out = String::with_capacity(body.len());
    let mut order: Vec<String> = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    let mut pos = 0;
    for line in body.split_inclusive('\n') {
        let start = pos;
        pos += line.len();
        let text = line.trim_end_matches(['\n', '\r']);
        if raw.iter().any(|r| r.contains(&start)) {
            out.push_str(line);
            continue;
        }
        if let Some((c, n)) = fence {
            if is_fence(text).is_some_and(|(c2, n2)| c2 == c && n2 >= n) && text.trim().chars().all(|x| x == c) {
                fence = None;
            }
            out.push_str(line);
            continue;
        }
        if let Some(f) = is_fence(text) {
            fence = Some(f);
            out.push_str(line);
            continue;
        }
        replace_references(line, &definitions, &mut order, &mut out);
    }

    for (label, _) in &definitions {
        if !order.contains(label) {
            order.push(label.clone());
        }
    }
    let notes = order
        .iter()
        .map(|label| {
            let text = &definitions.iter().find(|(l, _)| l == label).expect("order holds defined labels only").1;
            Footnote { id: footnote_id(label), label: label.clone(), content: inline_html(text) }
        })
        .collect();
    (out, notes)
}

/// A removed line leaves an empty one, so source lines still match the
/// preview's (scroll sync).
fn keep_line_count(line: &str, body: &mut String) {
    if line.ends_with('\n') {
        body.push('\n');
    }
}

/// One line's references (outside code spans).
fn replace_references(line: &str, definitions: &[(String, String)], order: &mut Vec<String>, out: &mut String) {
    let mut rest = line;
    let mut code: Option<usize> = None;
    while !rest.is_empty() {
        if let Some(ticks) = code {
            let run = "`".repeat(ticks);
            match rest.find(&run) {
                Some(end) => {
                    out.push_str(&rest[..end + ticks]);
                    rest = &rest[end + ticks..];
                    code = None;
                }
                None => {
                    out.push_str(rest);
                    return;
                }
            }
            continue;
        }
        let next_tick = rest.find('`');
        let next_ref = rest.find("[^");
        match (next_tick, next_ref) {
            (Some(t), r) if r.is_none_or(|r| t < r) => {
                let ticks = rest[t..].chars().take_while(|c| *c == '`').count();
                out.push_str(&rest[..t + ticks]);
                rest = &rest[t + ticks..];
                code = Some(ticks);
            }
            (_, Some(r)) => {
                out.push_str(&rest[..r]);
                let after = &rest[r + 2..];
                let escaped = out.ends_with('\\');
                match after.find(']').map(|end| (&after[..end], end)) {
                    Some((label, end)) if !escaped && definitions.iter().any(|(l, _)| l == label) => {
                        if !order.iter().any(|l| l == label) {
                            order.push(label.to_string());
                        }
                        let number = order.iter().position(|l| l == label).expect("just pushed") + 1;
                        out.push_str(&reference_html(&footnote_id(label), number));
                        rest = &after[end + 1..];
                    }
                    _ => {
                        out.push_str("[^");
                        rest = after;
                    }
                }
            }
            _ => {
                out.push_str(rest);
                return;
            }
        }
    }
}

/// A note's Markdown as inline HTML.
fn inline_html(markdown: &str) -> String {
    match crate::parse_plain_markdown(markdown).into_iter().next() {
        Some(crate::Block::Paragraph { html }) => html,
        _ => crate::escape_html(markdown),
    }
}

/// For a post opened from the blog: every `<sup data-fn>` reference in
/// the imported Markdown becomes `[^n]` and the notes from the
/// `footnotes` meta are appended as definitions. `None` - nothing
/// changed - when a note has no reference or a reference no note, so the
/// meta then travels on unchanged.
pub fn to_markdown(markdown: &str, meta: &str) -> Option<String> {
    let notes: Vec<serde_json::Value> = serde_json::from_str(meta).ok()?;
    let notes: Vec<(String, String)> = notes.iter().map(|n| Some((n.get("id")?.as_str()?.to_string(), n.get("content")?.as_str()?.to_string()))).collect::<Option<_>>()?;
    if notes.is_empty() {
        return None;
    }
    let raw = raw_ranges(markdown);
    let mut out = String::with_capacity(markdown.len());
    let mut numbers: Vec<(String, String)> = Vec::new();
    let mut pos = 0;
    while let Some(rel) = markdown[pos..].find("<sup data-fn=\"") {
        let start = pos + rel;
        out.push_str(&markdown[pos..start]);
        let end = markdown[start..].find("</sup>").map(|e| start + e + "</sup>".len())?;
        if raw.iter().any(|r| r.contains(&start)) {
            out.push_str(&markdown[start..end]);
            pos = end;
            continue;
        }
        let tag = &markdown[start..end];
        let id = tag["<sup data-fn=\"".len()..].split('"').next()?.to_string();
        let number: String = strip_tags(tag).trim().to_string();
        if number.is_empty() || number.contains(char::is_whitespace) || !notes.iter().any(|(note_id, _)| *note_id == id) {
            return None;
        }
        if let Some((_, existing)) = numbers.iter().find(|(i, _)| *i == id) {
            out.push_str(&format!("[^{existing}]"));
        } else {
            if numbers.iter().any(|(_, n)| *n == number) {
                return None;
            }
            out.push_str(&format!("[^{number}]"));
            numbers.push((id, number));
        }
        pos = end;
    }
    out.push_str(&markdown[pos..]);
    if numbers.len() != notes.len() {
        return None;
    }
    let mut definitions: Vec<String> = Vec::new();
    for (id, number) in &numbers {
        let content = &notes.iter().find(|(note_id, _)| note_id == id).expect("checked above").1;
        definitions.push(format!("[^{number}]: {}", crate::reverse::inline_html_to_markdown(content).replace('\n', "\n    ")));
    }
    Some(format!("{}\n\n{}\n", out.trim_end(), definitions.join("\n")))
}

fn strip_tags(html: &str) -> String {
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
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_become_numbered_sups_and_notes_the_meta() {
        let md = "Erst das.[^b] Dann das.[^a] Wieder.[^b]\n\n[^a]: Note *A*.\n[^b]: Note B,\n    weiter.\n\nSchluss.\n";
        let (body, notes) = extract(md);
        let b = footnote_id("b");
        assert!(body.starts_with(&format!("Erst das.{} Dann das.", reference_html(&b, 1))), "{body}");
        assert!(body.contains(&reference_html(&footnote_id("a"), 2)), "{body}");
        assert!(body.contains(&format!("Wieder.{}", reference_html(&b, 1))), "{body}");
        assert!(!body.contains("[^"), "{body}");
        assert_eq!(body.lines().count(), md.lines().count());
        assert!(body.contains("Schluss."));
        assert_eq!(notes.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(), ["b", "a"]);
        assert_eq!(notes[1].content, "Note <em>A</em>.");
        assert_eq!(notes[0].content, "Note B,\nweiter.");
        assert!(meta_json(&notes).starts_with("[{\"content\":\"Note B,\\nweiter.\",\"id\":\""));
    }

    #[test]
    fn code_and_undefined_references_stay_text() {
        let md = "`[^1]` und [^x] und \\[^1] und [^1]\n\n```\n[^1]\n```\n\n[^1]: Eins\n";
        let (body, notes) = extract(md);
        assert_eq!(notes.len(), 1);
        assert!(body.starts_with("`[^1]` und [^x] und \\[^1] und <sup"), "{body}");
        assert!(body.contains("```\n[^1]\n```"), "{body}");
    }

    #[test]
    fn without_definitions_nothing_changes() {
        let md = "Text [^1] ohne Definition.\n";
        assert_eq!(extract(md), (md.to_string(), Vec::new()));
    }

    #[test]
    fn ids_are_stable_and_uuid_shaped() {
        let id = footnote_id("1");
        assert_eq!(id, footnote_id("1"));
        assert_ne!(id, footnote_id("2"));
        assert_eq!(id.len(), 36);
        assert_eq!(id.matches('-').count(), 4);
    }

    #[test]
    fn imported_footnotes_become_markdown_and_back() {
        let a = "9ce9c2bd-4637-4266-a7f6-5638396d3437";
        let b = "1021d0b4-d305-46e3-960b-63442d88d2d9";
        let md = format!("Satz eins.{}\n\nSatz zwei.{}\n\n<!-- wp:footnotes /-->", reference_html(a, 1), reference_html(b, 2));
        let meta = format!("[{{\"id\":\"{a}\",\"content\":\"Lorem.\"}},{{\"id\":\"{b}\",\"content\":\"Sed <em>labore</em>.\"}}]");
        let converted = to_markdown(&md, &meta).unwrap();
        assert_eq!(converted, "Satz eins.[^1]\n\nSatz zwei.[^2]\n\n<!-- wp:footnotes /-->\n\n[^1]: Lorem.\n[^2]: Sed *labore*.\n");
        let (body, notes) = extract(&converted);
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[1].content, "Sed <em>labore</em>.");
        assert!(body.contains(&reference_html(&footnote_id("2"), 2)), "{body}");
        assert!(body.trim_end().ends_with(LIST_BLOCK), "{body}");
    }

    #[test]
    fn a_note_without_reference_keeps_the_meta() {
        let md = format!("Satz.{}", reference_html("x", 1));
        assert_eq!(to_markdown(&md, "[{\"id\":\"x\",\"content\":\"A\"},{\"id\":\"y\",\"content\":\"B\"}]"), None);
    }
}
