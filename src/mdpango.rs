//! Renders a subset of Markdown (as returned by chat LLMs) into Pango
//! markup for display in a `GtkLabel` bubble - bold/italic/strikethrough,
//! inline code, fenced code blocks, links (rendered as clickable `<a href>`
//! spans, which `GtkLabel` supports natively), headings, and lists. Not a
//! general Markdown renderer, just enough to make chat replies readable
//! without embedding a second WebView per bubble.

use gtk4::glib;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

pub fn markdown_to_pango(markdown: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let mut out = String::new();
    let mut list_stack: Vec<Option<u64>> = Vec::new();
    let mut first_block = true;

    for event in Parser::new_ext(markdown, options) {
        match event {
            Event::Start(tag) => {
                let is_heading = matches!(tag, Tag::Heading { .. });
                let is_code_block = matches!(tag, Tag::CodeBlock(_));
                match tag {
                    Tag::Paragraph | Tag::Heading { .. } | Tag::BlockQuote(_) | Tag::CodeBlock(_) => {
                        if !first_block {
                            out.push_str("\n\n");
                        }
                        first_block = false;
                        if is_heading {
                            out.push_str("<b>");
                        }
                        if is_code_block {
                            out.push_str("<tt>");
                        }
                    }
                    Tag::List(start) => {
                        if !first_block {
                            out.push_str("\n\n");
                        }
                        first_block = false;
                        list_stack.push(start);
                    }
                    Tag::Item => {
                        out.push('\n');
                        match list_stack.last_mut() {
                            Some(Some(n)) => {
                                out.push_str(&format!("{n}. "));
                                *n += 1;
                            }
                            _ => out.push_str("• "),
                        }
                    }
                    Tag::Strong => out.push_str("<b>"),
                    Tag::Emphasis => out.push_str("<i>"),
                    Tag::Strikethrough => out.push_str("<s>"),
                    Tag::Link { dest_url, .. } => {
                        out.push_str(&format!("<a href=\"{}\">", glib::markup_escape_text(&dest_url)));
                    }
                    _ => {}
                }
            }
            Event::End(tag_end) => match tag_end {
                TagEnd::Heading(_) => out.push_str("</b>"),
                TagEnd::CodeBlock => out.push_str("</tt>"),
                TagEnd::List(_) => {
                    list_stack.pop();
                }
                TagEnd::Strong => out.push_str("</b>"),
                TagEnd::Emphasis => out.push_str("</i>"),
                TagEnd::Strikethrough => out.push_str("</s>"),
                TagEnd::Link => out.push_str("</a>"),
                _ => {}
            },
            Event::Text(text) => out.push_str(&glib::markup_escape_text(&text)),
            Event::Code(text) => {
                out.push_str("<tt>");
                out.push_str(&glib::markup_escape_text(&text));
                out.push_str("</tt>");
            }
            Event::SoftBreak => out.push(' '),
            Event::HardBreak => out.push('\n'),
            Event::Rule => out.push_str("\n───\n"),
            _ => {}
        }
    }

    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bold_and_italic_become_pango_tags() {
        assert_eq!(markdown_to_pango("**bold** and *italic*"), "<b>bold</b> and <i>italic</i>");
    }

    #[test]
    fn inline_code_becomes_monospace() {
        assert_eq!(markdown_to_pango("run `cargo test`"), "run <tt>cargo test</tt>");
    }

    #[test]
    fn special_characters_in_text_are_escaped() {
        assert_eq!(markdown_to_pango("a < b & c > d"), "a &lt; b &amp; c &gt; d");
    }

    #[test]
    fn links_become_clickable_pango_anchors() {
        assert_eq!(markdown_to_pango("[Blocksatz](https://example.com)"), "<a href=\"https://example.com\">Blocksatz</a>");
    }

    #[test]
    fn unordered_list_items_get_bullets() {
        let out = markdown_to_pango("- one\n- two");
        assert!(out.contains("• one"));
        assert!(out.contains("• two"));
    }

    #[test]
    fn ordered_list_items_are_numbered() {
        let out = markdown_to_pango("1. one\n2. two");
        assert!(out.contains("1. one"));
        assert!(out.contains("2. two"));
    }

    #[test]
    fn paragraphs_are_separated_by_a_blank_line() {
        assert_eq!(markdown_to_pango("first\n\nsecond"), "first\n\nsecond");
    }

    #[test]
    fn code_block_is_wrapped_in_monospace_tag() {
        let out = markdown_to_pango("```\nlet x = 1;\n```");
        assert!(out.starts_with("<tt>"));
        assert!(out.contains("let x = 1;"));
        assert!(out.ends_with("</tt>"));
    }
}
