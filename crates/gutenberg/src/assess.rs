//! How close an article is to plain Markdown - for the warning when a
//! heavily designed post is opened from the blog (`docs/markdown-naehe.md`).
//! Blocksatz is meant for articles written in Markdown; every attribute
//! line, container and block kept as WordPress markup makes one harder to
//! write and edit.

use std::collections::BTreeMap;

use crate::{parse_plain_markdown, split_segments, Block, Segment};

/// The three outcomes of `assess`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Closeness {
    /// Plain Markdown, at most a few attribute lines - nothing to say.
    Plain,
    /// Some design, a container, or one or two blocks kept as markup.
    Designed,
    /// Much of it can only be edited as WordPress markup.
    Heavy,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Assessment {
    pub closeness: Closeness,
    /// Attribute lines, heading attributes, inline HTML.
    pub design: usize,
    /// `:::` containers and the fenced blocks (```` ```columns ```` ...).
    pub structure: usize,
    /// Blocks kept as WordPress markup, classic content, shortcodes, raw
    /// HTML - building blocks excluded.
    pub foreign: usize,
    /// Classic (pre-block-editor) content is part of the article.
    pub classic: bool,
    /// Kinds behind `structure` and `foreign` with their counts, most
    /// frequent first: block names (`group`, `media-text`, `shortcode`,
    /// `freeform`, `html`) and container kinds (`:::accordion`).
    pub kinds: Vec<(String, usize)>,
    total_chars: usize,
    foreign_chars: usize,
    structure_chars: usize,
}

impl Assessment {
    /// Share of the text (0..=1) that is kept WordPress markup.
    pub fn foreign_share(&self) -> f64 {
        if self.total_chars == 0 {
            0.0
        } else {
            self.foreign_chars as f64 / self.total_chars as f64
        }
    }

    /// Share of the text inside containers and fenced blocks.
    pub fn structure_share(&self) -> f64 {
        if self.total_chars == 0 {
            0.0
        } else {
            self.structure_chars as f64 / self.total_chars as f64
        }
    }
}

#[derive(Default)]
struct Tally {
    design: usize,
    structure: usize,
    foreign: usize,
    classic: bool,
    kinds: BTreeMap<String, usize>,
    total_chars: usize,
    foreign_chars: usize,
    structure_chars: usize,
}

/// Rates `markdown`. `building_blocks` are block names that belong to the
/// normal workflow (`lui/toc`, `more`, ...) and don't count against it.
pub fn assess(markdown: &str, building_blocks: &[&str]) -> Assessment {
    let mut tally = Tally::default();
    tally_text(markdown, building_blocks, &mut tally, false);
    let heavy = tally.foreign >= 3 || tally.foreign_chars * 10 > tally.total_chars || tally.classic || tally.structure_chars * 2 > tally.total_chars;
    let designed = tally.foreign > 0 || tally.design > 3 || tally.structure > 0;
    let closeness = if heavy {
        Closeness::Heavy
    } else if designed {
        Closeness::Designed
    } else {
        Closeness::Plain
    };
    let mut kinds: Vec<(String, usize)> = tally.kinds.into_iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Assessment {
        closeness,
        design: tally.design,
        structure: tally.structure,
        foreign: tally.foreign,
        classic: tally.classic,
        kinds,
        total_chars: tally.total_chars,
        foreign_chars: tally.foreign_chars,
        structure_chars: tally.structure_chars,
    }
}

/// `nested`: inside a container, whose characters already count as
/// structure.
fn tally_text(text: &str, building_blocks: &[&str], tally: &mut Tally, nested: bool) {
    for segment in split_segments(text) {
        match segment {
            Segment::Markdown(range) => {
                let source = &text[range];
                if !nested {
                    tally.total_chars += source.trim().len();
                }
                for block_range in crate::editing::top_level_blocks(source, 0..source.len()) {
                    let block_source = &source[block_range];
                    for block in parse_plain_markdown(block_source) {
                        tally_block(&block, tally, nested, block_source.len());
                    }
                }
            }
            Segment::Attrs { attrs, range } => {
                if is_visual(&attrs) {
                    tally.design += 1;
                }
                if !nested {
                    tally.total_chars += range.len();
                }
            }
            Segment::Raw(range) => {
                let raw = &text[range];
                if !nested {
                    tally.total_chars += raw.len();
                }
                let name = raw.trim_start().strip_prefix("<!-- wp:").and_then(|rest| rest.split_whitespace().next()).map(|n| n.trim_end_matches("/-->").to_string()).unwrap_or_default();
                if building_blocks.contains(&name.as_str()) {
                    continue;
                }
                if name == "freeform" {
                    tally.classic = true;
                }
                tally.foreign += 1;
                tally.foreign_chars += raw.len();
                *tally.kinds.entry(name).or_default() += 1;
            }
            Segment::Container { header, inner, range } => {
                if !nested {
                    tally.total_chars += range.len();
                    tally.structure_chars += range.len();
                }
                // Columns, accordion items and tabs belong to their parent.
                if !matches!(header.kind.as_str(), "column" | "item" | "tab") {
                    tally.structure += 1;
                    *tally.kinds.entry(format!(":::{}", header.kind)).or_default() += 1;
                }
                if is_visual(&header.attrs) {
                    tally.design += 1;
                }
                tally_text(&text[inner], building_blocks, tally, true);
            }
        }
    }
}

fn tally_block(block: &Block, tally: &mut Tally, nested: bool, source_len: usize) {
    match block {
        Block::Styled { attrs, block } => {
            if is_visual(attrs) {
                tally.design += 1;
            }
            tally_block(block, tally, nested, source_len);
        }
        Block::Columns { .. } | Block::Buttons { .. } | Block::Gallery { .. } | Block::Pullquote { .. } | Block::Details { .. } => {
            tally.structure += 1;
            let kind = match block {
                Block::Columns { .. } => "```columns",
                Block::Buttons { .. } => "```buttons",
                Block::Gallery { .. } => "```gallery",
                Block::Pullquote { .. } => "```pullquote",
                _ => "```details",
            };
            *tally.kinds.entry(kind.to_string()).or_default() += 1;
            if !nested {
                tally.structure_chars += source_len;
            }
        }
        // Raw HTML written into the Markdown (not a kept block comment -
        // those are `Segment::Raw`).
        Block::RawHtml { html } if !html.trim_start().starts_with("<!--more") => {
            tally.foreign += 1;
            if !nested {
                tally.foreign_chars += html.len();
            }
            *tally.kinds.entry("html".to_string()).or_default() += 1;
        }
        Block::Paragraph { html } | Block::Heading { html, .. } if has_inline_html(html) => tally.design += 1,
        _ => {}
    }
}

/// Attributes that change how the block looks. An anchor, an image width,
/// list numbering or a table's caption and footer are part of ordinary
/// writing and don't count.
fn is_visual(attrs: &crate::BlockAttrs) -> bool {
    attrs.text_color.is_some() || attrs.background.is_some() || attrs.gradient.is_some() || attrs.font_size.is_some() || attrs.align.is_some() || attrs.style.is_some() || !attrs.classes.is_empty() || attrs.drop_cap || attrs.padding.is_some() || attrs.border.is_some() || attrs.radius.is_some() || attrs.shadow.is_some()
}

/// Inline HTML beyond what Markdown renders itself (`<mark>`, `<sub>`, a
/// link with `target`, ...). The paragraph HTML here is pulldown-cmark's
/// output, so only tags it never produces on its own count.
fn has_inline_html(html: &str) -> bool {
    const MARKDOWN_TAGS: &[&str] = &["strong", "em", "code", "a", "del", "br", "img", "input"];
    html.match_indices('<').any(|(i, _)| {
        let name: String = html[i + 1..].trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        !name.is_empty() && !MARKDOWN_TAGS.contains(&name.as_str())
    }) || html.contains(" target=") || html.contains(" rel=")
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILDING: &[&str] = &["lui/toc", "more"];

    #[test]
    fn plain_markdown_is_plain() {
        let md = "<!-- wp:lui/toc /-->\n\n## Titel\n\nText mit **fett** und [Link](https://example.org).\n\n- eins\n- zwei\n\n![Bild](a.png \"Alt\")\n{width=100%}\n";
        let a = assess(md, BUILDING);
        assert_eq!(a.closeness, Closeness::Plain, "{a:?}");
        assert_eq!((a.design, a.foreign), (0, 0));
    }

    #[test]
    fn a_few_attribute_lines_and_one_kept_block_are_designed() {
        let md = "Text\n{bg=accent}\n\nMehr Text, ganz normal und lang genug, damit der Anteil klein bleibt. ".repeat(3) + "\n\n<!-- wp:quote -->\n<blockquote class=\"wp-block-quote\"><p>Z</p><cite>Q</cite></blockquote>\n<!-- /wp:quote -->\n" + &"\n\nNoch ein langer Absatz mit viel Text, der den Anteil des Markups klein hält.".repeat(16);
        let a = assess(&md, BUILDING);
        assert_eq!(a.closeness, Closeness::Designed, "{a:?}");
        assert_eq!(a.foreign, 1);
        assert_eq!(a.kinds, vec![("quote".to_string(), 1)]);
    }

    #[test]
    fn three_kept_blocks_or_classic_content_are_heavy() {
        let block = "<!-- wp:media-text -->\n<div class=\"wp-block-media-text\"><p>x</p></div>\n<!-- /wp:media-text -->\n\n";
        let a = assess(&format!("Text\n\n{block}{block}{block}"), BUILDING);
        assert_eq!(a.closeness, Closeness::Heavy);
        assert_eq!(a.kinds, vec![("media-text".to_string(), 3)]);

        let classic = assess("<!-- wp:freeform -->\n<p>Alt</p>\n<!-- /wp:freeform -->\n\n".to_string().as_str(), BUILDING);
        assert!(classic.classic);
        assert_eq!(classic.closeness, Closeness::Heavy);
    }

    #[test]
    fn an_article_mostly_in_containers_is_heavy() {
        let md = "Kurz.\n\n:::: tabs\n::: tab \"Eins\"\nEin längerer Text im ersten Reiter, der viel Platz braucht.\n:::\n\n::: tab \"Zwei\"\nUnd noch mehr Text im zweiten Reiter, der ebenfalls Platz braucht.\n:::\n::::\n";
        let a = assess(md, BUILDING);
        assert_eq!(a.closeness, Closeness::Heavy, "{a:?}");
        assert_eq!(a.structure, 1);
    }

    #[test]
    fn building_blocks_do_not_count() {
        let md = "<!-- wp:lui/toc /-->\n\n<!-- wp:more -->\n<!--more-->\n<!-- /wp:more -->\n\nText.";
        assert_eq!(assess(md, BUILDING).closeness, Closeness::Plain);
        assert_eq!(assess(md, &[]).foreign, 2);
    }

    #[test]
    fn inline_html_counts_as_design() {
        let a = assess("H<sub>2</sub>O und <mark>markiert</mark>.\n\nNormal mit `code`.", BUILDING);
        assert_eq!(a.design, 1);
    }
}
