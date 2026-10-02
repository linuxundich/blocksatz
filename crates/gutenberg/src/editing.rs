//! Finding the block under the cursor and rewriting its attributes in the
//! Markdown text - what the block inspector needs. Works on the text, not
//! the parsed tree, so an edit is a single replacement the editor can
//! undo, and everything else in the document stays byte-for-byte as typed.

use std::ops::Range;

use pulldown_cmark::{Event, Parser};

use crate::containers::{self, Header};
use crate::{markdown_options, parse_plain_markdown, split_segments, Block, BlockAttrs, Segment};

/// The top-level (or innermost container-level) block at a text position.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockAtCursor {
    /// The WordPress block name without `core/` (`paragraph`, `heading`,
    /// `table`, `group`, ...). `None` for a block kept verbatim.
    pub name: Option<String>,
    /// For a verbatim block, its comment name (`lui/toc`).
    pub verbatim_name: Option<String>,
    pub attrs: BlockAttrs,
    /// The block's own source.
    pub range: Range<usize>,
    placement: Placement,
}

#[derive(Debug, Clone, PartialEq)]
enum Placement {
    /// An attribute line below the block (existing or to be inserted
    /// after `block_end`).
    Line { block_end: usize, attr_line: Option<Range<usize>> },
    /// Braces at the end of a heading line.
    HeadingInline { line_end: usize, braces: Option<Range<usize>> },
    /// The opening line of a container.
    ContainerHeader { line: Range<usize>, header: Header },
    /// Verbatim markup - nothing to edit here.
    None,
}

/// The block at byte `offset` of `md`, or `None` between blocks.
pub fn block_at(md: &str, offset: usize) -> Option<BlockAtCursor> {
    block_at_in(md, 0, md.len(), offset)
}

fn block_at_in(md: &str, start: usize, end: usize, offset: usize) -> Option<BlockAtCursor> {
    let text = &md[start..end];
    let segments = split_segments(text);
    let mut previous: Option<BlockAtCursor> = None;
    for (index, segment) in segments.iter().enumerate() {
        let next_attrs = match segments.get(index + 1) {
            Some(Segment::Attrs { range, .. }) => Some(range.start + start..range.end + start),
            _ => None,
        };
        match segment {
            Segment::Markdown(range) => {
                let blocks = top_level_blocks(text, range.clone());
                let last = blocks.len().saturating_sub(1);
                for (i, block_range) in blocks.into_iter().enumerate() {
                    let absolute = block_range.start + start..block_range.end + start;
                    let attr_line = if i == last { next_attrs.clone() } else { None };
                    let found = describe(md, absolute.clone(), attr_line.clone());
                    let line_end = attr_line.as_ref().map_or(line_end_of(md, absolute.end), |a| a.end);
                    if offset >= absolute.start && offset <= line_end {
                        return Some(found);
                    }
                    previous = Some(found);
                }
            }
            Segment::Attrs { range, .. } => {
                if offset >= range.start + start && offset <= range.end + start {
                    return previous;
                }
            }
            Segment::Raw(range) => {
                let absolute = range.start + start..range.end + start;
                if offset >= absolute.start && offset <= absolute.end {
                    let name = md[absolute.start + "<!-- wp:".len()..].split_whitespace().next().map(|n| n.trim_end_matches("/-->").to_string());
                    return Some(BlockAtCursor { name: None, verbatim_name: name, attrs: BlockAttrs::default(), range: absolute, placement: Placement::None });
                }
            }
            Segment::Container { header, inner, range } => {
                let absolute = range.start + start..range.end + start;
                let inner_abs = inner.start + start..inner.end + start;
                if offset < absolute.start || offset > absolute.end {
                    continue;
                }
                let header_line = absolute.start..line_end_of(md, absolute.start);
                let container = BlockAtCursor {
                    name: Some(header.kind.clone()),
                    verbatim_name: None,
                    attrs: header.attrs.clone(),
                    range: absolute.clone(),
                    placement: Placement::ContainerHeader { line: header_line.clone(), header: header.clone() },
                };
                if offset <= header_line.end || offset >= inner_abs.end {
                    return Some(container);
                }
                return block_at_in(md, inner_abs.start, inner_abs.end, offset).or(Some(container));
            }
        }
    }
    None
}

/// Byte ranges (relative to `text`) of the top-level blocks of one plain
/// Markdown stretch.
fn top_level_blocks(text: &str, range: Range<usize>) -> Vec<Range<usize>> {
    let offset = range.start;
    let mut blocks = Vec::new();
    let mut depth = 0usize;
    let mut block_start = 0;
    for (event, r) in Parser::new_ext(&text[range], markdown_options()).into_offset_iter() {
        match event {
            Event::Start(_) => {
                if depth == 0 {
                    block_start = r.start;
                }
                depth += 1;
            }
            Event::End(_) => {
                depth -= 1;
                if depth == 0 {
                    blocks.push(block_start + offset..r.end + offset);
                }
            }
            Event::Rule if depth == 0 => blocks.push(r.start + offset..r.end + offset),
            _ => {}
        }
    }
    blocks
}

fn line_end_of(md: &str, pos: usize) -> usize {
    let pos = pos.min(md.len());
    // A block range often ends just after its newline - the line it ends on
    // is the one before.
    let search_from = if pos > 0 && md.as_bytes()[pos - 1] == b'\n' { pos - 1 } else { pos };
    md[search_from..].find('\n').map_or(md.len(), |nl| search_from + nl)
}

fn describe(md: &str, range: Range<usize>, attr_line: Option<Range<usize>>) -> BlockAtCursor {
    let source = &md[range.clone()];
    let parsed = parse_plain_markdown(source);
    let name = parsed.first().map(|b| block_name(b.unstyled()).to_string());
    let block_end = line_end_of(md, range.end);
    let mut attrs = attr_line.as_ref().and_then(|r| BlockAttrs::parse_line(&md[r.clone()])).unwrap_or_default();
    let placement = if name.as_deref() == Some("heading") && attr_line.is_none() {
        let line = &md[range.start..block_end];
        let braces = heading_braces(line).map(|r| r.start + range.start..r.end + range.start);
        if let Some(b) = &braces {
            attrs = BlockAttrs::parse_tokens(md[b.clone()].trim().trim_start_matches('{').trim_end_matches('}')).unwrap_or_default();
        }
        Placement::HeadingInline { line_end: block_end, braces }
    } else {
        Placement::Line { block_end, attr_line }
    };
    // A list starting at 5. is part of the text, not an attribute.
    if let Some(Block::Styled { attrs: parsed_attrs, .. }) = parsed.first() {
        if attrs.start.is_none() {
            attrs.start = parsed_attrs.start;
        }
    }
    BlockAtCursor { name, verbatim_name: None, attrs, range, placement }
}

/// ` {...}` at the end of a heading line (leading space included).
fn heading_braces(line: &str) -> Option<Range<usize>> {
    let trimmed = line.trim_end();
    if !trimmed.ends_with('}') {
        return None;
    }
    let open = trimmed.rfind(" {")?;
    Some(open..trimmed.len())
}

fn block_name(block: &Block) -> &'static str {
    match block {
        Block::Paragraph { .. } => "paragraph",
        Block::Heading { .. } => "heading",
        Block::List { .. } => "list",
        Block::BlockQuote { .. } => "quote",
        Block::CodeBlock { .. } => "code",
        Block::Image { .. } => "image",
        Block::Video { .. } => "video",
        Block::Audio { .. } => "audio",
        Block::Embed { .. } => "embed",
        Block::ThematicBreak => "separator",
        Block::Table { .. } => "table",
        Block::Columns { .. } => "columns",
        Block::Buttons { .. } => "buttons",
        Block::Gallery { .. } => "gallery",
        Block::Pullquote { .. } => "pullquote",
        Block::Details { .. } => "details",
        Block::RawHtml { .. } => "html",
        Block::Container { .. } => "group",
        Block::Styled { block, .. } => block_name(block),
    }
}

/// The single text replacement that gives `block` the attributes `attrs`
/// - `None` for a verbatim block.
pub fn attrs_edit(md: &str, block: &BlockAtCursor, attrs: &BlockAttrs) -> Option<(Range<usize>, String)> {
    // A list's start number lives in its first marker.
    let mut attrs = attrs.clone();
    if block.name.as_deref() == Some("list") {
        attrs.start = None;
    }
    match &block.placement {
        Placement::None => None,
        Placement::Line { block_end, attr_line } => Some(match (attr_line, attrs.is_empty()) {
            (Some(line), true) => (*block_end..line.end, String::new()),
            (Some(line), false) => (line.clone(), attrs.to_markdown()),
            (None, true) => (*block_end..*block_end, String::new()),
            (None, false) => (*block_end..*block_end, format!("\n{}", attrs.to_markdown())),
        }),
        Placement::HeadingInline { line_end, braces } => {
            let text = if attrs.is_empty() { String::new() } else { format!(" {}", attrs.to_markdown()) };
            Some(match braces {
                Some(b) => (b.start..b.end.max(*line_end).min(md.len()), text),
                None => (*line_end..*line_end, text),
            })
        }
        Placement::ContainerHeader { line, header } => Some((line.clone(), containers::header_markdown(header.colons, &header.kind, header.title.as_deref(), &header.params, &attrs))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(md: &str, offset: usize, attrs: BlockAttrs) -> String {
        let block = block_at(md, offset).expect("a block");
        let (range, text) = attrs_edit(md, &block, &attrs).expect("editable");
        format!("{}{}{}", &md[..range.start], text, &md[range.end..])
    }

    fn bg(slug: &str) -> BlockAttrs {
        BlockAttrs { background: Some(slug.into()), ..Default::default() }
    }

    #[test]
    fn finds_paragraph_and_its_attribute_line() {
        let md = "Erster.\n\nZweiter.\n{bg=line}\n\nDritter.\n";
        let block = block_at(md, 11).unwrap();
        assert_eq!(block.name.as_deref(), Some("paragraph"));
        assert_eq!(block.attrs.background.as_deref(), Some("line"));
        assert_eq!(block_at(md, 20).unwrap().attrs, block.attrs);
        assert_eq!(block_at(md, 8), None);
    }

    #[test]
    fn inserts_changes_and_removes_an_attribute_line() {
        let md = "Erster.\n\nZweiter.\n\nDritter.";
        let added = apply(md, 10, bg("accent"));
        assert_eq!(added, "Erster.\n\nZweiter.\n{bg=accent}\n\nDritter.");
        assert_eq!(apply(&added, 10, bg("line")), "Erster.\n\nZweiter.\n{bg=line}\n\nDritter.");
        assert_eq!(apply(&added, 10, BlockAttrs::default()), md);
    }

    #[test]
    fn attribute_line_on_the_last_block_without_trailing_newline() {
        assert_eq!(apply("Nur.", 0, bg("accent")), "Nur.\n{bg=accent}");
    }

    #[test]
    fn heading_attributes_stay_on_the_heading_line() {
        let md = "## Titel\n\nText";
        let styled = apply(md, 3, BlockAttrs { text_color: Some("accent".into()), ..Default::default() });
        assert_eq!(styled, "## Titel {color=accent}\n\nText");
        let block = block_at(&styled, 3).unwrap();
        assert_eq!(block.attrs.text_color.as_deref(), Some("accent"));
        assert_eq!(apply(&styled, 3, BlockAttrs::default()), md);
    }

    #[test]
    fn table_caption_and_footer_survive_a_color_change() {
        let md = "| A |\n| --- |\n| 1 |\n{footer caption=\"Summe\"}\n";
        let block = block_at(md, 2).unwrap();
        assert_eq!(block.name.as_deref(), Some("table"));
        let attrs = BlockAttrs { style: Some("stripes".into()), ..block.attrs.clone() };
        assert_eq!(apply(md, 2, attrs), "| A |\n| --- |\n| 1 |\n{style=stripes footer caption=\"Summe\"}\n");
    }

    #[test]
    fn container_header_and_inner_blocks() {
        let md = "::: group {layout=flex}\nInnen.\n:::\n";
        let header = block_at(md, 3).unwrap();
        assert_eq!(header.name.as_deref(), Some("group"));
        assert_eq!(apply(md, 3, bg("base-2")), "::: group {layout=flex bg=base-2}\nInnen.\n:::\n");
        let inner = block_at(md, 26).unwrap();
        assert_eq!(inner.name.as_deref(), Some("paragraph"));
        assert_eq!(apply(md, 26, bg("accent")), "::: group {layout=flex}\nInnen.\n{bg=accent}\n:::\n");
    }

    #[test]
    fn verbatim_blocks_are_found_but_not_editable() {
        let md = "<!-- wp:lui/toc /-->\n\nText";
        let block = block_at(md, 3).unwrap();
        assert_eq!(block.verbatim_name.as_deref(), Some("lui/toc"));
        assert_eq!(attrs_edit(md, &block, &bg("x")), None);
    }
}
