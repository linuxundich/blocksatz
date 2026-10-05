//! Translating a working copy for a linked blog in another language
//! (`docs/translations.md`). Pure logic - no GTK, no network: the caller
//! hands in a `send` closure that talks to the model (via `aitasks::run`)
//! and does the file work (`translatedialog.rs`).
//!
//! The article is translated section by section (split at `## `
//! headings), so a long article never runs into a model's output limit and
//! an update only re-translates the sections whose original changed.
//! Everything the model must not touch - code, URLs, HTML tags, attribute
//! lists, container markers, footnote markers - is replaced by numbered
//! placeholders (`⟦CODE-3⟧`) before sending and put back afterwards; a
//! placeholder that comes back missing or twice is reported.

use sha2::{Digest, Sha256};

use crate::document::{self, Document, PostStatus, TranslationLink};
use crate::i18n::tr;
use crate::llm::{ChatMessage, Role};
use crate::syncstate;

/// Key of the translation prompt in `ai_prompts.json` (`aiprompts.rs`).
pub const PROMPT_ID: &str = "translate";
/// Key of the category mapping ("Allgemein = General", one per line).
pub const CATEGORY_MAP_ID: &str = "translate-categories";

/// Default system prompt - generic on purpose; a blog's own voice, its
/// conventions and a glossary belong in the user's version of it
/// (Einstellungen → KI-Prompts → Übersetzung).
pub const DEFAULT_PROMPT: &str = "You translate blog articles for a linked edition of the blog in another language. The original is already published. The translation must read as if the author had written it in the target language: same voice, same opinions, same humor, technically precise. It is a translation, not a rewrite.

Rules:
1. Placeholders such as ⟦CODE-3⟧, ⟦URL-1⟧, ⟦TAG-2⟧, ⟦ATTR-4⟧, ⟦MARK-5⟧ or ⟦FN-6⟧ stand for code, links, markup and attributes. Keep every placeholder exactly once, unchanged, in the matching position.
2. Keep the Markdown structure: headings, lists, tables, quotes, blank lines. Translate the human-readable text of image captions, alt texts (the quoted title after an image URL) and link texts.
3. Do not add facts, version numbers, dates, links or opinions. Do not update outdated statements.
4. Carry jokes, irony and the strength of judgements over with the same effect. Address the reader the way the original does.
5. Use the target language's conventions for dates, numbers and quotation marks outside of code.
6. Output only the translation - no preface, no notes, no code fence around it.";

pub fn language_name(code: &str) -> &'static str {
    match code {
        "de" => "German",
        "en" => "English",
        "fr" => "French",
        "es" => "Spanish",
        "it" => "Italian",
        "nl" => "Dutch",
        _ => "the target language",
    }
}

// ---------------------------------------------------------------- sections

fn fence_marker(line: &str) -> Option<String> {
    let t = line.trim_start();
    let ch = t.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let run: String = t.chars().take_while(|c| *c == ch).collect();
    (run.chars().count() >= 3).then_some(run)
}

fn closes_fence(line: &str, marker: &str) -> bool {
    let t = line.trim();
    t.starts_with(marker) && t.chars().all(|c| marker.starts_with(c))
}

/// Splits `body` at every `## ` heading outside fenced code. The pieces
/// concatenate back to `body` exactly; the first one is whatever comes
/// before the first heading (possibly empty).
pub fn split_sections(body: &str) -> Vec<String> {
    let mut sections = vec![String::new()];
    let mut fence: Option<String> = None;
    for line in body.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        match &fence {
            Some(marker) => {
                if closes_fence(bare, marker) {
                    fence = None;
                }
            }
            None => {
                if let Some(marker) = fence_marker(bare) {
                    fence = Some(marker);
                } else if bare.starts_with("## ") {
                    sections.push(String::new());
                }
            }
        }
        sections.last_mut().expect("at least one section").push_str(line);
    }
    sections
}

/// The sections of `original` that changed since a translation was made
/// from it (`source_sections`): their 1-based first and last line in
/// `original.body`, and their heading (empty for the intro).
pub fn changed_sections(original: &Document, source_sections: &[String]) -> Vec<(usize, usize, String)> {
    // Hashed like `translate` does, over the body with uploaded image URLs;
    // that only swaps URLs, so the line numbers are those of `body`.
    let hashed = split_sections(&body_with_uploaded_images(original));
    let mut line = 1;
    let mut out = Vec::new();
    for section in hashed {
        let lines = section.matches('\n').count().max(1);
        if !section.trim().is_empty() && !source_sections.contains(&section_hash(&section)) {
            let heading = section.lines().find(|l| l.starts_with("## ")).map(|l| l.trim_start_matches('#').trim().to_string()).unwrap_or_default();
            out.push((line, line + lines - 1, heading));
        }
        line += lines;
    }
    out
}

/// Short, stable hash of a section, ignoring leading and trailing
/// whitespace.
pub fn section_hash(section: &str) -> String {
    let digest = Sha256::digest(section.trim().as_bytes());
    digest.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

// -------------------------------------------------------------- placeholders

#[derive(Debug, Clone, PartialEq)]
pub struct Masked {
    pub text: String,
    /// `(kind, original)` per placeholder; placeholder `n` is index `n - 1`.
    pub originals: Vec<(&'static str, String)>,
}

impl Masked {
    fn push(&mut self, kind: &'static str, original: &str) {
        self.originals.push((kind, original.to_string()));
        self.text.push_str(&format!("⟦{kind}-{}⟧", self.originals.len()));
    }
}

/// Replaces everything the model must not change with placeholders.
pub fn mask(text: &str) -> Masked {
    let mut out = Masked { text: String::new(), originals: Vec::new() };
    let mut lines = text.split_inclusive('\n').peekable();
    while let Some(line) = lines.next() {
        let bare = line.trim_end_matches(['\n', '\r']);
        let newline = &line[bare.len()..];

        if let Some(marker) = fence_marker(bare) {
            let verse = bare.trim_start().trim_start_matches(marker.as_str()).trim() == "verse";
            if verse {
                // Verse is prose: only the fence lines are protected.
                out.push("CODE", bare);
                out.text.push_str(newline);
                for inner in lines.by_ref() {
                    let inner_bare = inner.trim_end_matches(['\n', '\r']);
                    if closes_fence(inner_bare, &marker) {
                        out.push("CODE", inner_bare);
                        out.text.push_str(&inner[inner_bare.len()..]);
                        break;
                    }
                    mask_inline(inner_bare, &mut out);
                    out.text.push_str(&inner[inner_bare.len()..]);
                }
                continue;
            }
            let mut block = bare.to_string();
            let mut tail = newline.to_string();
            for inner in lines.by_ref() {
                let inner_bare = inner.trim_end_matches(['\n', '\r']);
                block.push_str(&tail);
                block.push_str(inner_bare);
                tail = inner[inner_bare.len()..].to_string();
                if closes_fence(inner_bare, &marker) {
                    break;
                }
            }
            out.push("CODE", &block);
            out.text.push_str(&tail);
            continue;
        }

        if bare.contains("<!--") && !bare.contains("-->") {
            let mut block = bare.to_string();
            let mut tail = newline.to_string();
            for inner in lines.by_ref() {
                let inner_bare = inner.trim_end_matches(['\n', '\r']);
                block.push_str(&tail);
                block.push_str(inner_bare);
                tail = inner[inner_bare.len()..].to_string();
                if inner_bare.contains("-->") {
                    break;
                }
            }
            out.push("TAG", &block);
            out.text.push_str(&tail);
            continue;
        }

        mask_inline(bare, &mut out);
        out.text.push_str(newline);
    }
    out
}

/// Attributes whose values are prose for readers, not markup.
const TEXT_ATTRS: &[&str] = &["alt", "title", "aria-label"];

/// Pushes an HTML tag as placeholders, but leaves the values of
/// `TEXT_ATTRS` translatable in between: `<img src="x" alt="Ein Bild">`
/// becomes `⟦TAG-1⟧Ein Bild⟦TAG-2⟧`. Comments and tags without such an
/// attribute stay one placeholder.
fn push_tag(tag: &str, out: &mut Masked) {
    let mut values: Vec<(usize, usize)> = Vec::new();
    if !tag.starts_with("<!") {
        let lower = tag.to_ascii_lowercase();
        for name in TEXT_ATTRS {
            let pattern = format!("{name}=\"");
            let mut from = 0;
            while let Some(pos) = lower[from..].find(&pattern) {
                let at = from + pos;
                let start = at + pattern.len();
                from = start;
                if !lower[..at].ends_with(char::is_whitespace) {
                    continue;
                }
                if let Some(len) = tag[start..].find('"') {
                    if tag[start..start + len].trim().is_empty() {
                        continue;
                    }
                    values.push((start, start + len));
                }
            }
        }
    }
    values.sort_unstable();
    let mut last = 0;
    for (start, end) in values {
        if start < last {
            continue;
        }
        out.push("TAG", &tag[last..start]);
        out.text.push_str(&tag[start..end]);
        last = end;
    }
    out.push("TAG", &tag[last..]);
}

fn is_attr_list(inner: &str) -> bool {
    let t = inner.trim();
    !t.is_empty() && (t.starts_with('#') || t.starts_with('.') || t.starts_with(':') || t.contains('='))
}

fn mask_inline(line: &str, out: &mut Masked) {
    let mut rest = line;

    // Container markers: `::: details`, `::::`, `::: item` - the quoted
    // title after them stays translatable.
    let trimmed = rest.trim_start();
    if trimmed.starts_with(":::") {
        let indent = &rest[..rest.len() - trimmed.len()];
        out.text.push_str(indent);
        let colons = trimmed.chars().take_while(|c| *c == ':').count();
        let after = &trimmed[colons..];
        let name_len = after.trim_start().chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-').map(char::len_utf8).sum::<usize>();
        let ws = after.len() - after.trim_start().len();
        let end = colons + ws + name_len;
        out.push("MARK", &trimmed[..end]);
        rest = &trimmed[end..];
    }

    let bytes = rest.as_bytes();
    let mut i = 0;
    let mut plain_start = 0;
    let flush = |out: &mut Masked, from: usize, to: usize| out.text.push_str(&rest[from..to]);

    while i < bytes.len() {
        let c = bytes[i];
        // Inline code: a run of N backticks up to the next run of exactly N.
        if c == b'`' {
            let run = rest[i..].bytes().take_while(|b| *b == b'`').count();
            let after = i + run;
            let mut j = after;
            let mut found = None;
            while j < bytes.len() {
                if bytes[j] == b'`' {
                    let r = rest[j..].bytes().take_while(|b| *b == b'`').count();
                    if r == run {
                        found = Some(j + r);
                        break;
                    }
                    j += r;
                } else {
                    j += 1;
                }
            }
            if let Some(end) = found {
                flush(out, plain_start, i);
                out.push("CODE", &rest[i..end]);
                i = end;
                plain_start = i;
                continue;
            }
            i = after;
            continue;
        }
        // HTML tags, comments and autolinks.
        if c == b'<' && i + 1 < bytes.len() && (bytes[i + 1].is_ascii_alphabetic() || bytes[i + 1] == b'/' || bytes[i + 1] == b'!') {
            if let Some(off) = rest[i..].find('>') {
                let end = i + off + 1;
                flush(out, plain_start, i);
                push_tag(&rest[i..end], out);
                i = end;
                plain_start = i;
                continue;
            }
        }
        // Link and image targets: `](target "title")` - the title stays.
        if c == b']' && i + 1 < bytes.len() && bytes[i + 1] == b'(' {
            let start = i + 2;
            let mut j = start;
            let mut depth = 0usize;
            while j < bytes.len() {
                match bytes[j] {
                    b'(' => depth += 1,
                    b')' if depth == 0 => break,
                    b')' => depth -= 1,
                    b' ' | b'\t' if depth == 0 => break,
                    _ => {}
                }
                j += 1;
            }
            if j > start {
                flush(out, plain_start, start);
                out.push("URL", &rest[start..j]);
                i = j;
                plain_start = i;
                continue;
            }
        }
        // Footnote markers `[^1]`.
        if c == b'[' && i + 1 < bytes.len() && bytes[i + 1] == b'^' {
            if let Some(off) = rest[i..].find(']') {
                let end = i + off + 1;
                flush(out, plain_start, i);
                out.push("FN", &rest[i..end]);
                i = end;
                plain_start = i;
                continue;
            }
        }
        // Attribute lists `{#anchor .class key=value}`.
        if c == b'{' {
            if let Some(off) = rest[i..].find('}') {
                let end = i + off + 1;
                if is_attr_list(&rest[i + 1..end - 1]) && !rest[i + 1..end - 1].contains('{') {
                    flush(out, plain_start, i);
                    out.push("ATTR", &rest[i..end]);
                    i = end;
                    plain_start = i;
                    continue;
                }
            }
        }
        // Bare URLs.
        if (c == b'h') && (rest[i..].starts_with("https://") || rest[i..].starts_with("http://")) {
            let mut end = i + rest[i..].find(|ch: char| ch.is_whitespace() || matches!(ch, ')' | '>' | ']' | '"' | '\'')).unwrap_or(rest.len() - i);
            while end > i && matches!(bytes[end - 1], b'.' | b',' | b';' | b':' | b'!' | b'?') {
                end -= 1;
            }
            flush(out, plain_start, i);
            out.push("URL", &rest[i..end]);
            i = end;
            plain_start = i;
            continue;
        }
        i += 1;
    }
    flush(out, plain_start, rest.len());
}

/// Puts the originals back. Returns the text and the problems found
/// (placeholders missing, duplicated or unknown); with problems the text is
/// still as complete as possible.
pub fn unmask(text: &str, originals: &[(&'static str, String)]) -> (String, Vec<String>) {
    let mut out = String::new();
    let mut seen = vec![0usize; originals.len()];
    let mut problems = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('⟦') {
        out.push_str(&rest[..start]);
        let after = &rest[start + '⟦'.len_utf8()..];
        let Some(close) = after.find('⟧') else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let inner = &after[..close];
        let index = inner.rsplit_once('-').and_then(|(_, n)| n.parse::<usize>().ok()).filter(|n| (1..=originals.len()).contains(n));
        match index {
            Some(n) => {
                seen[n - 1] += 1;
                out.push_str(&originals[n - 1].1);
            }
            None => {
                problems.push(tr("Unbekannter Platzhalter „{p}“").replace("{p}", &format!("⟦{inner}⟧")));
                out.push_str(&rest[start..start + '⟦'.len_utf8() + close + '⟧'.len_utf8()]);
            }
        }
        rest = &after[close + '⟧'.len_utf8()..];
    }
    out.push_str(rest);
    for (i, count) in seen.iter().enumerate() {
        let name = format!("⟦{}-{}⟧", originals[i].0, i + 1);
        if *count == 0 {
            problems.push(tr("{p} fehlt ({o})").replace("{p}", &name).replace("{o}", &shorten(&originals[i].1)));
        } else if *count > 1 {
            problems.push(tr("{p} kommt {n}-mal vor").replace("{p}", &name).replace("{n}", &count.to_string()));
        }
    }
    (out, problems)
}

fn shorten(s: &str) -> String {
    let one_line = s.replace('\n', " ⏎ ");
    if one_line.chars().count() > 40 {
        format!("{} …", one_line.chars().take(40).collect::<String>())
    } else {
        one_line
    }
}

// ------------------------------------------------------------------ checks

#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    /// Section index, `None` for the article as a whole.
    pub section: Option<usize>,
    pub message: String,
}

fn protected_parts(text: &str) -> Vec<String> {
    let mut parts: Vec<String> = mask(text).originals.into_iter().map(|(kind, original)| format!("{kind}\u{1f}{original}")).collect();
    parts.sort();
    parts
}

fn heading_count(text: &str) -> usize {
    let mut fence: Option<String> = None;
    let mut n = 0;
    for line in text.lines() {
        match &fence {
            Some(marker) if closes_fence(line, marker) => fence = None,
            Some(_) => {}
            None => {
                if let Some(marker) = fence_marker(line) {
                    fence = Some(marker);
                } else if line.starts_with('#') && line.trim_start_matches('#').starts_with(' ') {
                    n += 1;
                }
            }
        }
    }
    n
}

const SOURCE_MARKERS_DE: &[&str] = &["und", "der", "die", "das", "nicht", "mit", "für", "ist", "ein", "eine", "auch", "oder", "wird", "sind", "auf", "sich", "dem", "den", "ich", "ihr", "euch"];

const MARKERS_EN: &[&str] = &["the", "and", "of", "to", "is", "with", "for", "you", "that", "this", "are", "it", "on", "your", "be", "can", "not", "or", "which", "from"];

/// The language an article body is written in - `"de"` or `"en"`, judged
/// by common function words outside code, links and markup; `None` when
/// the text is too short or too mixed to tell.
pub fn detect_language(body: &str) -> Option<&'static str> {
    let masked = mask(body).text;
    let (mut de, mut en) = (0usize, 0usize);
    for word in masked.split(|c: char| !c.is_alphabetic()).filter(|w| !w.is_empty()) {
        let lower = word.to_lowercase();
        if SOURCE_MARKERS_DE.contains(&lower.as_str()) {
            de += 1;
        } else if MARKERS_EN.contains(&lower.as_str()) {
            en += 1;
        }
    }
    if de >= 8 && de >= en * 3 {
        Some("de")
    } else if en >= 8 && en >= de * 3 {
        Some("en")
    } else {
        None
    }
}

/// Words in `text` (with placeholders, i.e. without code) that look like
/// the source language left untranslated - only German for now.
fn leftover_score(masked: &str, source_lang: &str) -> usize {
    if source_lang != "de" {
        return 0;
    }
    masked
        .split(|c: char| !c.is_alphabetic())
        .filter(|w| !w.is_empty())
        .filter(|w| SOURCE_MARKERS_DE.contains(&w.to_lowercase().as_str()) || w.chars().any(|c| matches!(c, 'ä' | 'ö' | 'ü' | 'ß' | 'Ä' | 'Ö' | 'Ü')))
        .count()
}

/// Compares a translated body with its original: code, links, markup,
/// attributes and footnote markers must be identical, headings as many,
/// and no section may read like the source language.
pub fn check(source_body: &str, translated_body: &str, source_lang: &str) -> Vec<Issue> {
    let mut issues = Vec::new();
    let (src, dst) = (protected_parts(source_body), protected_parts(translated_body));
    if src != dst {
        let missing: Vec<&String> = src.iter().filter(|p| !dst.contains(p)).collect();
        let extra: Vec<&String> = dst.iter().filter(|p| !src.contains(p)).collect();
        let show = |v: &[&String]| v.iter().take(3).map(|p| shorten(p.split_once('\u{1f}').map_or(p.as_str(), |(_, o)| o))).collect::<Vec<_>>().join(", ");
        let mut message = tr("Code, Links oder Auszeichnungen weichen vom Original ab.");
        if !missing.is_empty() {
            message.push(' ');
            message.push_str(&tr("Fehlt: {list}").replace("{list}", &show(&missing)));
        }
        if !extra.is_empty() {
            message.push(' ');
            message.push_str(&tr("Zusätzlich: {list}").replace("{list}", &show(&extra)));
        }
        issues.push(Issue { section: None, message });
    }
    let (h_src, h_dst) = (heading_count(source_body), heading_count(translated_body));
    if h_src != h_dst {
        issues.push(Issue {
            section: None,
            message: tr("Überschriften: {a} im Original, {b} in der Übersetzung.").replace("{a}", &h_src.to_string()).replace("{b}", &h_dst.to_string()),
        });
    }
    for (index, section) in split_sections(translated_body).iter().enumerate() {
        let score = leftover_score(&mask(section).text, source_lang);
        if score >= 4 {
            issues.push(Issue { section: Some(index), message: tr("Abschnitt enthält noch {n} Wörter, die nach Original-Sprache aussehen.").replace("{n}", &score.to_string()) });
        }
    }
    issues
}

// ------------------------------------------------------------- the article

pub struct Options {
    pub source_lang: String,
    pub target_lang: String,
    /// `wpsite::site_id` of the original's blog.
    pub source_site: String,
    /// Today as `YYYY-MM-DD`.
    pub today: String,
    pub translate_tags: bool,
    /// „Allgemein = General“ lines (`CATEGORY_MAP_ID`).
    pub category_map: String,
    /// The target blog's tags and categories: the model is asked to reuse
    /// an existing tag's spelling, and the result is matched against both
    /// ignoring case, so "Tuxedo" isn't created again as "TUXEDO".
    pub known_tags: Vec<String>,
    pub known_categories: Vec<String>,
}

/// `names` with each one replaced by the spelling of a `known` name it
/// equals ignoring case; duplicates dropped.
pub fn match_known(names: &[String], known: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in names {
        let lower = name.trim().to_lowercase();
        let matched = known.iter().find(|k| k.to_lowercase() == lower).cloned().unwrap_or_else(|| name.trim().to_string());
        if !matched.is_empty() && !out.iter().any(|o| o.to_lowercase() == matched.to_lowercase()) {
            out.push(matched);
        }
    }
    out
}

pub struct Outcome {
    pub document: Document,
    pub issues: Vec<Issue>,
    pub translated_sections: usize,
    pub reused_sections: usize,
}

/// `send(system_prompt, history)` → the model's reply.
pub type Send<'a> = &'a dyn Fn(&str, &[ChatMessage]) -> Result<String, String>;

/// Applies the category mapping; unmapped names stay as they are.
pub fn map_categories(categories: &[String], mapping: &str) -> Vec<String> {
    let pairs: Vec<(String, String)> = mapping
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (a, b) = line.split_once('=').or_else(|| line.split_once('→')).or_else(|| line.split_once('\t'))?;
            Some((a.trim().to_lowercase(), b.trim().to_string()))
        })
        .collect();
    categories
        .iter()
        .map(|c| pairs.iter().find(|(from, _)| *from == c.trim().to_lowercase()).map_or_else(|| c.clone(), |(_, to)| to.clone()))
        .collect()
}

/// The original's body with images that are already uploaded pointing at
/// their WordPress URLs - the translation reuses those uploads instead of
/// uploading the same files to the other blog.
pub fn body_with_uploaded_images(doc: &Document) -> String {
    let mut body = doc.body.clone();
    for item in &doc.frontmatter.media {
        let Some(wp) = &item.wordpress else { continue };
        if item.source.is_empty() || item.source.contains("://") || wp.url.is_empty() {
            continue;
        }
        for close in [")", " "] {
            body = body.replace(&format!("]({}{close}", item.source), &format!("]({}{close}", wp.url));
        }
    }
    body
}

/// Local files the translated body still references (images not uploaded
/// yet) - to be copied into the translation's folder.
pub fn local_media(doc: &Document) -> Vec<String> {
    doc.frontmatter
        .media
        .iter()
        .filter(|item| item.wordpress.is_none() && !item.source.contains("://") && !item.source.is_empty())
        .map(|item| item.source.clone())
        .collect()
}

fn strip_wrapper(reply: &str) -> String {
    let t = reply.trim();
    if t.starts_with("```") && t.ends_with("```") && t.len() > 6 {
        let inner = &t[3..t.len() - 3];
        let inner = inner.split_once('\n').map_or(inner, |(_, rest)| rest);
        return inner.trim().to_string();
    }
    t.to_string()
}

fn is_notes_heading(line: &str) -> bool {
    let t = line.trim().trim_matches('*').trim_matches('#').trim().trim_end_matches(':').trim().to_lowercase();
    matches!(t.as_str(), "notes" | "note" | "hinweise" | "anmerkungen" | "translator notes" | "translation notes")
}

/// Splits a trailing "---" + "Notes" block off a reply. A blog's own
/// prompt may ask for such notes after the article (the request for a
/// section says not to, but models don't always listen); they belong into
/// the review, not into the text.
fn split_notes(reply: &str) -> (String, Vec<String>) {
    let lines: Vec<&str> = reply.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if line.trim() != "---" {
            continue;
        }
        let Some(next) = lines[i + 1..].iter().position(|l| !l.trim().is_empty()).map(|p| i + 1 + p) else { continue };
        if !is_notes_heading(lines[next]) {
            continue;
        }
        let notes = lines[next + 1..]
            .iter()
            .map(|l| l.trim().trim_start_matches(['-', '*', '•']).trim())
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        return (lines[..i].join("\n").trim_end().to_string(), notes);
    }
    (reply.to_string(), Vec::new())
}

fn section_request(masked: &str, opts: &Options) -> String {
    format!(
        "Translate this section of a blog article from {} into {}. Return only the translated Markdown of this section: no title, no excerpt, no notes, no explanations, no code fence around it - this overrides any output format in your instructions. Keep every placeholder of the form ⟦KIND-n⟧ exactly once and unchanged.\n\n{}",
        language_name(&opts.source_lang),
        language_name(&opts.target_lang),
        masked
    )
}

fn translate_section(section: &str, system: &str, opts: &Options, send: Send) -> Result<(String, Vec<String>, Vec<String>), String> {
    if section.trim().is_empty() {
        return Ok((section.to_string(), Vec::new(), Vec::new()));
    }
    let masked = mask(section);
    let mut history = vec![ChatMessage { role: Role::User, text: section_request(&masked.text, opts) }];
    let (mut reply, mut notes) = split_notes(&strip_wrapper(&send(system, &history)?));
    let (mut text, mut problems) = unmask(&reply, &masked.originals);
    if !problems.is_empty() {
        // One retry with the problems spelled out.
        history.push(ChatMessage { role: Role::Model, text: reply.clone() });
        history.push(ChatMessage {
            role: Role::User,
            text: format!(
                "Some placeholders were lost or duplicated: {}. Translate the section again and keep every placeholder exactly once, unchanged.",
                problems.join("; ")
            ),
        });
        (reply, notes) = split_notes(&strip_wrapper(&send(system, &history)?));
        (text, problems) = unmask(&reply, &masked.originals);
    }
    // Keep the original's trailing whitespace so sections join as before.
    let trailing = &section[section.trim_end().len()..];
    Ok((format!("{}{trailing}", text.trim_end()), problems, notes))
}

#[derive(Debug, Default, PartialEq)]
struct Meta {
    title: String,
    excerpt: String,
    featured_image_alt: String,
    tags: Vec<String>,
}

fn translate_meta(doc: &Document, system: &str, opts: &Options, send: Send) -> Result<Meta, String> {
    let fm = &doc.frontmatter;
    let input = serde_json::json!({
        "title": fm.title,
        "excerpt": fm.excerpt.clone().unwrap_or_default(),
        "featured_image_alt": fm.featured_image_alt.clone().unwrap_or_default(),
        "tags": if opts.translate_tags { fm.tags.clone() } else { Vec::new() },
    });
    // Capped: a blog's whole tag list can run into the thousands.
    let known: Vec<&str> = opts.known_tags.iter().map(String::as_str).take(500).collect();
    let reuse = if opts.translate_tags && !known.is_empty() {
        format!(" When a translated tag means the same as one of the blog's existing tags, use that tag's exact spelling instead. Existing tags: {}.", serde_json::json!(known))
    } else {
        String::new()
    };
    let request = format!(
        "Translate the values of this JSON object (title, excerpt, featured image alt text and tags of a blog article) from {} into {}. Keep the keys.{reuse} Reply with the JSON object only.\n\n{}",
        language_name(&opts.source_lang),
        language_name(&opts.target_lang),
        input
    );
    let reply = send(system, &[ChatMessage { role: Role::User, text: request }])?;
    parse_meta(&reply).ok_or_else(|| tr("Die Antwort für Titel und Auszug war kein gültiges JSON."))
}

fn parse_meta(reply: &str) -> Option<Meta> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    let value: serde_json::Value = serde_json::from_str(&reply[start..=end]).ok()?;
    let s = |k: &str| value.get(k).and_then(|v| v.as_str()).unwrap_or_default().trim().to_string();
    Some(Meta {
        title: s("title"),
        excerpt: s("excerpt"),
        featured_image_alt: s("featured_image_alt"),
        tags: value.get("tags").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|t| t.as_str().map(|t| t.trim().to_string())).filter(|t| !t.is_empty()).collect()).unwrap_or_default(),
    })
}

/// Translates `source` (an article from the original blog). With
/// `previous` (an earlier translation of it) only the sections whose
/// original changed are sent again, and the previous working copy's
/// metadata - title, categories, its own post id - is kept; the result is
/// then unreviewed again if anything was re-translated.
pub fn translate(source: &Document, previous: Option<&Document>, opts: &Options, system: &str, send: Send, progress: &dyn Fn(usize, usize)) -> Result<Outcome, String> {
    let source_body = body_with_uploaded_images(source);
    let sections = split_sections(&source_body);
    let hashes: Vec<String> = sections.iter().map(|s| section_hash(s)).collect();

    // Reusable translated sections from `previous`, by the hash of the
    // original section they came from.
    let mut reuse: Vec<(String, String)> = Vec::new();
    if let Some(prev) = previous {
        if let Some(link) = &prev.frontmatter.translation {
            let prev_sections = split_sections(&prev.body);
            if prev_sections.len() == link.source_sections.len() {
                reuse = link.source_sections.iter().cloned().zip(prev_sections).collect();
            }
        }
    }

    let needs_meta = previous.is_none();
    let to_send = sections.iter().zip(&hashes).filter(|(s, h)| !s.trim().is_empty() && !reuse.iter().any(|(rh, _)| rh == *h)).count() + usize::from(needs_meta);
    let mut done = 0;
    progress(done, to_send);

    let meta = if needs_meta {
        let meta = translate_meta(source, system, opts, send)?;
        done += 1;
        progress(done, to_send);
        Some(meta)
    } else {
        None
    };

    let mut body = String::new();
    let mut issues = Vec::new();
    let (mut translated, mut reused) = (0, 0);
    for (index, (section, hash)) in sections.iter().zip(&hashes).enumerate() {
        if let Some((_, text)) = reuse.iter().find(|(rh, _)| rh == hash) {
            body.push_str(text);
            reused += 1;
            continue;
        }
        let (text, problems, notes) = translate_section(section, system, opts, send)?;
        if !section.trim().is_empty() {
            translated += 1;
            done += 1;
            progress(done, to_send);
        }
        for problem in problems {
            issues.push(Issue { section: Some(index), message: problem });
        }
        for note in notes {
            issues.push(Issue { section: Some(index), message: tr("Hinweis des Modells: {note}").replace("{note}", &note) });
        }
        body.push_str(&text);
    }
    issues.extend(check(&source_body, &body, &opts.source_lang));

    let link = TranslationLink {
        lang: opts.target_lang.clone(),
        source_site: opts.source_site.clone(),
        source_id: source.frontmatter.wp_post_id.unwrap_or_default(),
        source_hash: syncstate::fingerprint(source),
        source_sections: hashes,
        translated_at: opts.today.clone(),
        reviewed: previous.and_then(|p| p.frontmatter.translation.as_ref()).is_some_and(|t| t.reviewed) && translated == 0,
    };

    let document = match (previous, meta) {
        (Some(prev), _) => {
            let mut doc = prev.clone();
            doc.body = body;
            doc.frontmatter.translation = Some(link);
            doc
        }
        (None, Some(meta)) => {
            let src = &source.frontmatter;
            let mut fm = document::Frontmatter {
                title: meta.title.clone(),
                slug: document::slugify(&meta.title),
                status: PostStatus::Draft,
                post_type: src.post_type,
                categories: match_known(&map_categories(&src.categories, &opts.category_map), &opts.known_categories),
                tags: match_known(&meta.tags, &opts.known_tags),
                excerpt: (!meta.excerpt.is_empty()).then_some(meta.excerpt),
                featured_image: src.featured_image.clone(),
                featured_image_alt: (!meta.featured_image_alt.is_empty()).then_some(meta.featured_image_alt),
                comment_status: src.comment_status,
                translation: Some(link),
                ..document::Frontmatter::default()
            };
            if fm.title.is_empty() {
                fm.title = src.title.clone();
            }
            Document { frontmatter: fm, body }
        }
        (None, None) => unreachable!("meta is translated whenever there is no previous translation"),
    };

    Ok(Outcome { document, issues, translated_sections: translated, reused_sections: reused })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn sections_split_at_h2_outside_code_and_join_back() {
        let body = "Intro\n\n## Eins\nText\n```bash\n## kein Kopf\n```\n## Zwei\nMehr\n";
        let sections = split_sections(body);
        assert_eq!(sections.len(), 3);
        assert_eq!(sections.concat(), body);
        assert!(sections[1].contains("## kein Kopf"));
    }

    #[test]
    fn body_starting_with_a_heading_has_an_empty_intro() {
        let sections = split_sections("## A\nx\n");
        assert_eq!(sections, vec![String::new(), "## A\nx\n".to_string()]);
    }

    #[test]
    fn section_hash_ignores_surrounding_whitespace() {
        assert_eq!(section_hash("## A\nx\n\n"), section_hash("\n## A\nx"));
        assert_ne!(section_hash("## A\nx"), section_hash("## A\ny"));
    }

    #[test]
    fn mask_protects_code_links_tags_attrs_and_footnotes() {
        let text = "Öffnet `ls -la` und [die Seite](https://example.org/a_(b) \"Titel\") mit <a href=\"x\">Link</a>{.klasse} und Fußnote[^1].\n";
        let m = mask(text);
        let kinds: Vec<&str> = m.originals.iter().map(|(k, _)| *k).collect();
        assert_eq!(kinds, vec!["CODE", "URL", "TAG", "TAG", "ATTR", "FN"]);
        assert!(m.text.contains("[die Seite](⟦URL-2⟧ \"Titel\")"));
        assert_eq!(m.originals[1].1, "https://example.org/a_(b)");
        let (back, problems) = unmask(&m.text, &m.originals);
        assert_eq!(back, text);
        assert!(problems.is_empty());
    }

    #[test]
    fn mask_leaves_alt_and_title_values_translatable() {
        let text = "<figure><img src=\"https://example.org/a.webp\" alt=\"Die Karte\" class=\"x\" title=\"Titel\"/></figure>\n<img src=\"b\" alt=\"\">\n<!-- wp:image {\"alt\":\"bleibt\"} -->\n";
        let m = mask(text);
        assert!(m.text.contains("Die Karte"));
        assert!(m.text.contains("Titel"));
        assert!(!m.text.contains("example.org"));
        assert!(!m.text.contains("bleibt"));
        assert_eq!(unmask(&m.text, &m.originals).0, text);
        let translated = text.replace("Die Karte", "The map").replace("\"Titel\"", "\"Title\"");
        assert!(check(text, &translated, "de").is_empty());
    }

    #[test]
    fn mask_keeps_fenced_blocks_whole_but_translates_verse() {
        let text = "Vorher\n```bash\necho \"Hallo\"\n```\n```verse\nRosen sind rot\n```\n";
        let m = mask(text);
        assert_eq!(m.originals[0].1, "```bash\necho \"Hallo\"\n```");
        assert!(m.text.contains("Rosen sind rot"));
        assert_eq!(unmask(&m.text, &m.originals).0, text);
    }

    #[test]
    fn mask_container_markers_and_bare_urls() {
        let text = "::: item \"Erster Eintrag\" open\nSiehe https://example.org/x.\n:::\n";
        let m = mask(text);
        assert_eq!(m.originals[0].1, "::: item");
        assert!(m.text.contains("\"Erster Eintrag\" open"));
        assert_eq!(m.originals[1].1, "https://example.org/x");
        assert_eq!(unmask(&m.text, &m.originals).0, text);
    }

    #[test]
    fn unmask_reports_missing_and_duplicate_placeholders() {
        let originals = vec![("CODE", "`a`".to_string()), ("URL", "https://x".to_string())];
        let (text, problems) = unmask("⟦CODE-1⟧ ⟦CODE-1⟧", &originals);
        assert_eq!(text, "`a` `a`");
        assert_eq!(problems.len(), 2);
    }

    #[test]
    fn check_finds_changed_code_and_leftover_german() {
        let src = "## A\nNutzt `ls`.\n";
        assert!(check(src, "## A\nUse `ls`.\n", "de").is_empty());
        assert!(!check(src, "## A\nUse `ls -l`.\n", "de").is_empty());
        let german = "## A\nDas ist nicht der Fall und auch für die Sache gilt das.\n";
        assert!(check(src.replace("`ls`", "x").as_str(), german, "de").iter().any(|i| i.section == Some(1)));
    }

    #[test]
    fn trailing_notes_are_split_off() {
        let (text, notes) = split_notes("## Part\nText.\n\n---\n**Notes**\n- [CHECK: menu label]\n- Pun not carried over\n");
        assert_eq!(text, "## Part\nText.");
        assert_eq!(notes, vec!["[CHECK: menu label]", "Pun not carried over"]);
        let (text, notes) = split_notes("A\n\n---\n\nB\n");
        assert_eq!((text.as_str(), notes.len()), ("A\n\n---\n\nB\n", 0));
    }

    #[test]
    fn changed_sections_of_the_original_are_found_with_their_lines() {
        let before = document::parse("---\ntitle: \"x\"\n---\nIntro.\n\n## Eins\nText.\n\n## Zwei\nMehr.\n");
        let hashes: Vec<String> = split_sections(&body_with_uploaded_images(&before)).iter().map(|s| section_hash(s)).collect();
        assert!(changed_sections(&before, &hashes).is_empty());
        let after = document::parse("---\ntitle: \"x\"\n---\nIntro.\n\n## Eins\nText.\n\n## Zwei\nMehr, und neu.\n");
        let changed = changed_sections(&after, &hashes);
        assert_eq!(changed.len(), 1);
        let (first, last, heading) = &changed[0];
        assert_eq!(heading, "Zwei");
        assert_eq!(after.body.lines().nth(first - 1), Some("## Zwei"));
        assert_eq!(after.body.lines().nth(last - 1), Some("Mehr, und neu."));
    }

    #[test]
    fn the_language_of_a_body_is_recognized() {
        let de = "Wer ein Notebook von TUXEDO besitzt, der kennt das Control Center. Über die App legt ihr fest, wie schnell der Prozessor taktet und welches Profil gilt. Das ist auch unter GNOME nicht anders, und die Erweiterung ist für euch gedacht.";
        let en = "If you own a TUXEDO notebook, you know the Control Center. With the app you decide how fast the processor runs and which profile is used. This is not different on GNOME, and the extension is for you and your desktop.";
        assert_eq!(detect_language(de), Some("de"));
        assert_eq!(detect_language(en), Some("en"));
        assert_eq!(detect_language("Kurz."), None);
        // Code doesn't count.
        assert_eq!(detect_language(&format!("{de}\n\n```sh\nthe and of to is with for you that this are it on\n```\n")), Some("de"));
    }

    #[test]
    fn translated_terms_take_the_target_blogs_spelling() {
        let known = vec!["Tuxedo".to_string(), "GNOME Extensions".to_string()];
        let out = match_known(&["TUXEDO".into(), "gnome extensions".into(), "Pulsgeber".into(), "tuxedo".into()], &known);
        assert_eq!(out, vec!["Tuxedo", "GNOME Extensions", "Pulsgeber"]);
    }

    #[test]
    fn categories_are_mapped_case_insensitively() {
        let map = "Allgemein = General\n# Kommentar\nWebdesign/-hosting → Web hosting\n";
        assert_eq!(map_categories(&["allgemein".into(), "GNU/Linux".into(), "Webdesign/-hosting".into()], map), vec!["General", "GNU/Linux", "Web hosting"]);
    }

    fn source_doc() -> Document {
        let mut doc = document::parse("---\ntitle: \"Titel\"\ncategories: [\"Allgemein\"]\nwp_post_id: 42\n---\n\nIntro mit `code`.\n\n## Teil\nText.\n");
        doc.frontmatter.wp_site = Some("example.org".into());
        doc
    }

    fn opts() -> Options {
        Options {
            source_lang: "de".into(),
            target_lang: "en".into(),
            source_site: "example.org".into(),
            today: "2026-10-03".into(),
            translate_tags: false,
            category_map: "Allgemein = General".into(),
            known_tags: Vec::new(),
            known_categories: Vec::new(),
        }
    }

    /// A fake model: JSON for the meta request, otherwise the masked text
    /// with German words swapped - placeholders untouched.
    fn fake(calls: &RefCell<usize>) -> impl Fn(&str, &[ChatMessage]) -> Result<String, String> + '_ {
        move |_, history| {
            *calls.borrow_mut() += 1;
            let last = &history.last().unwrap().text;
            if last.contains("JSON") {
                return Ok("{\"title\": \"Title\", \"excerpt\": \"\", \"featured_image_alt\": \"\", \"tags\": []}".into());
            }
            let masked = last.split_once("\n\n").unwrap().1;
            Ok(masked.replace("Intro mit", "Intro with").replace("Teil", "Part"))
        }
    }

    #[test]
    fn translate_builds_a_linked_unreviewed_draft() {
        let calls = RefCell::new(0);
        let send = fake(&calls);
        let out = translate(&source_doc(), None, &opts(), "sys", &send, &|_, _| {}).unwrap();
        let fm = &out.document.frontmatter;
        assert_eq!(fm.title, "Title");
        assert_eq!(fm.slug, "title");
        assert_eq!(fm.categories, vec!["General"]);
        assert_eq!(fm.wp_post_id, None);
        let link = fm.translation.as_ref().unwrap();
        assert_eq!((link.source_site.as_str(), link.source_id, link.reviewed), ("example.org", 42, false));
        assert_eq!(link.source_sections.len(), 2);
        assert!(out.document.body.contains("Intro with `code`."));
        assert!(out.document.body.contains("## Part"));
        assert!(out.issues.is_empty(), "{:?}", out.issues);
        assert_eq!(*calls.borrow(), 3);
    }

    #[test]
    fn update_reuses_unchanged_sections_and_keeps_metadata() {
        let calls = RefCell::new(0);
        let send = fake(&calls);
        let first = translate(&source_doc(), None, &opts(), "sys", &send, &|_, _| {}).unwrap();
        let mut previous = first.document.clone();
        previous.frontmatter.wp_post_id = Some(7);
        previous.frontmatter.title = "Edited title".into();
        previous.frontmatter.translation.as_mut().unwrap().reviewed = true;

        let mut changed = source_doc();
        changed.body = changed.body.replace("Text.", "Neuer Text.");
        *calls.borrow_mut() = 0;
        let out = translate(&changed, Some(&previous), &opts(), "sys", &send, &|_, _| {}).unwrap();
        assert_eq!((out.reused_sections, out.translated_sections), (1, 1));
        assert_eq!(*calls.borrow(), 1);
        let fm = &out.document.frontmatter;
        assert_eq!((fm.wp_post_id, fm.title.as_str()), (Some(7), "Edited title"));
        assert!(!fm.translation.as_ref().unwrap().reviewed);

        *calls.borrow_mut() = 0;
        let same = translate(&source_doc(), Some(&previous), &opts(), "sys", &send, &|_, _| {}).unwrap();
        assert_eq!(*calls.borrow(), 0);
        assert!(same.document.frontmatter.translation.as_ref().unwrap().reviewed);
    }

    /// Round trip over the real library (read only):
    /// `cargo test real_library -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_library_masks_and_unmasks_losslessly() {
        let root = crate::library::root();
        for entry in crate::library::scan(&root) {
            let body = &entry.document.body;
            let m = mask(body);
            let (back, problems) = unmask(&m.text, &m.originals);
            assert_eq!(&back, body, "{}", entry.path.display());
            assert!(problems.is_empty());
            assert_eq!(split_sections(body).concat(), *body);
            println!("{}: {} Abschnitte, {} Platzhalter", entry.path.display(), split_sections(body).len(), m.originals.len());
            if std::env::var("BLOCKSATZ_SHOW_MASK").is_ok_and(|v| entry.path.to_string_lossy().contains(&v)) {
                println!("{}", split_sections(&m.text).get(1).cloned().unwrap_or_default());
            }
        }
    }

    #[test]
    fn uploaded_images_point_at_their_wordpress_urls() {
        let mut doc = document::parse("![Bild](foto.webp \"Alt\")\n");
        doc.frontmatter.media = vec![crate::media::MediaItem {
            id: "m1".into(),
            filename: "foto.webp".into(),
            source: "foto.webp".into(),
            alt: crate::media::AltText::Text("Alt".into()),
            caption: None,
            wordpress: Some(crate::media::WordPressMediaRef { media_id: 5, url: "https://example.org/foto.webp".into(), content_hash: String::new(), width: 0, height: 0, size_slug: None }),
            last_markdown_caption: None,
        }];
        assert_eq!(body_with_uploaded_images(&doc), "![Bild](https://example.org/foto.webp \"Alt\")\n");
        assert!(local_media(&doc).is_empty());
    }
}
