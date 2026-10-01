//! Converts `CHANGELOG.md`'s [Keep a Changelog](https://keepachangelog.com/)
//! release history into the restricted HTML subset (`<p>`, `<ul>`, `<li>` -
//! the same subset AppStream release descriptions use) that
//! `Adw.AboutDialog`'s `release-notes` property accepts, so the "Über
//! Blocksatz" dialog (`about.rs`) can show the full version history
//! directly, not just point at the repository.
//!
//! Deliberately drops the `### Added`/`### Changed`/`### Fixed` category
//! headers rather than trying to render them too - the target markup has
//! no heading-like element to spend on them, and a flat bullet list per
//! version is still perfectly readable.

use gtk4::glib;

struct VersionEntry {
    heading: String,
    bullets: Vec<String>,
}

pub fn to_release_notes_html(changelog: &str) -> String {
    let mut html = String::new();
    for entry in parse_entries(changelog) {
        html.push_str(&format!("<p>{}</p>", glib::markup_escape_text(&entry.heading)));
        if !entry.bullets.is_empty() {
            html.push_str("<ul>");
            for bullet in &entry.bullets {
                html.push_str(&format!("<li>{}</li>", glib::markup_escape_text(&strip_inline_markdown(bullet))));
            }
            html.push_str("</ul>");
        }
    }
    html
}

/// One version section per `## [x.y.z] - date` heading, up to the next
/// such heading or the end of the file - `## [Unreleased]` is skipped
/// since it's never anything but empty at release time in this project's
/// own practice. A bullet's wrapped continuation lines (indented, no
/// leading `- `) are joined back into one line, matching how a Markdown
/// renderer would treat them.
fn parse_entries(changelog: &str) -> Vec<VersionEntry> {
    let mut entries: Vec<VersionEntry> = Vec::new();
    let mut current: Option<VersionEntry> = None;
    let mut bullet_buf = String::new();

    fn flush_bullet(entry: &mut Option<VersionEntry>, buf: &mut String) {
        let text = buf.trim();
        if !text.is_empty() {
            if let Some(entry) = entry {
                entry.bullets.push(text.to_string());
            }
        }
        buf.clear();
    }

    for line in changelog.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            flush_bullet(&mut current, &mut bullet_buf);
            if let Some(finished) = current.take() {
                entries.push(finished);
            }
            let heading = heading.trim();
            if heading.starts_with("[Unreleased]") {
                continue;
            }
            // "[0.25.0] - 2026-09-04" -> "0.25.0 - 2026-09-04"
            let heading = heading.replacen('[', "", 1).replacen(']', "", 1);
            current = Some(VersionEntry { heading, bullets: Vec::new() });
        } else if line.starts_with("### ") {
            flush_bullet(&mut current, &mut bullet_buf);
        } else if let Some(rest) = line.strip_prefix("- ") {
            flush_bullet(&mut current, &mut bullet_buf);
            bullet_buf.push_str(rest);
        } else if current.is_some() && !bullet_buf.is_empty() && line.starts_with("  ") {
            bullet_buf.push(' ');
            bullet_buf.push_str(line.trim());
        }
    }
    flush_bullet(&mut current, &mut bullet_buf);
    if let Some(finished) = current {
        entries.push(finished);
    }
    entries
}

/// Removes the handful of inline Markdown constructs this changelog
/// actually uses - `**bold**`/`` `code` `` markers dropped, `[text](url)`
/// reduced to just its text - since the target markup has no inline
/// formatting to map them onto either. Not a general Markdown parser,
/// just enough for what `CHANGELOG.md` itself contains.
fn strip_inline_markdown(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' | '`' => {}
            '[' => {
                let mut link_text = String::new();
                for inner in chars.by_ref() {
                    if inner == ']' {
                        break;
                    }
                    link_text.push(inner);
                }
                if chars.peek() == Some(&'(') {
                    for inner in chars.by_ref() {
                        if inner == ')' {
                            break;
                        }
                    }
                }
                result.push_str(&link_text);
            }
            _ => result.push(c),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_version_heading_and_bullets() {
        let changelog = "\
# Changelog

## [Unreleased]

## [1.2.0] - 2026-01-02

### Added

- First bullet.
- Second bullet.

## [1.1.0] - 2026-01-01

### Fixed

- Only bullet here.
";
        let html = to_release_notes_html(changelog);
        assert_eq!(
            html,
            "<p>1.2.0 - 2026-01-02</p><ul><li>First bullet.</li><li>Second bullet.</li></ul>\
             <p>1.1.0 - 2026-01-01</p><ul><li>Only bullet here.</li></ul>"
        );
    }

    #[test]
    fn joins_wrapped_bullet_continuation_lines() {
        let changelog = "\
## [1.0.0] - 2026-01-01

### Added

- A bullet that wraps
  onto a second line
  and a third.
";
        let html = to_release_notes_html(changelog);
        assert_eq!(html, "<p>1.0.0 - 2026-01-01</p><ul><li>A bullet that wraps onto a second line and a third.</li></ul>");
    }

    #[test]
    fn skips_the_unreleased_section() {
        let changelog = "\
## [Unreleased]

### Added

- Not yet released, should not appear.

## [1.0.0] - 2026-01-01

### Added

- Released bullet.
";
        let html = to_release_notes_html(changelog);
        assert!(!html.contains("Not yet released"));
        assert!(html.contains("Released bullet."));
    }

    #[test]
    fn strips_inline_markdown_formatting() {
        assert_eq!(strip_inline_markdown("**bold** and `code` and [a link](https://example.com)"), "bold and code and a link");
    }

    #[test]
    fn escapes_html_special_characters_in_output() {
        let changelog = "\
## [1.0.0] - 2026-01-01

### Added

- Uses `<img>` & other <b>tags</b>.
";
        let html = to_release_notes_html(changelog);
        assert!(html.contains("&lt;img&gt;"));
        assert!(html.contains("&amp;"));
        assert!(!html.contains("<img>"));
    }

    #[test]
    fn a_version_with_no_bullets_gets_no_list() {
        let changelog = "## [1.0.0] - 2026-01-01\n";
        let html = to_release_notes_html(changelog);
        assert_eq!(html, "<p>1.0.0 - 2026-01-01</p>");
    }

    /// Not `#[ignore]`d for a lack-of-network reason like most other ignored
    /// tests in this project - this one just doesn't need real credentials
    /// or a display, so it always runs. Guards against the parser choking
    /// on some real-world line shape the hand-written fixtures above don't
    /// happen to exercise (e.g. this file's own occasional inline code
    /// spans or links inside a bullet).
    #[test]
    fn the_real_changelog_parses_without_a_broken_tag() {
        let changelog = include_str!("../CHANGELOG.md");
        let html = to_release_notes_html(changelog);
        assert!(html.contains("<p>0.1.0"), "expected the first-ever release to still be present: {html}");
        assert!(!html.contains("[Unreleased]"));
        assert_eq!(html.matches("<p>").count(), html.matches("</p>").count());
        assert_eq!(html.matches("<ul>").count(), html.matches("</ul>").count());
        assert_eq!(html.matches("<li>").count(), html.matches("</li>").count());
    }
}
