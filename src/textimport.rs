//! Starting a new article from an existing text file (`newarticle.rs`):
//! decoding the file, reading whatever header it brings - YAML (Jekyll,
//! Hugo, Obsidian, Pandoc, Blocksatz itself), TOML (Hugo), MultiMarkdown
//! or a Pandoc title block - into the frontmatter, and finding and
//! rewriting the local images the text points to. Pure functions, no GTK;
//! see `docs/new-article-dialog.md`, section 3.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::document::{self, Document, Frontmatter, PostStatus};

/// The header formats `import` recognizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Yaml,
    Toml,
    MultiMarkdown,
    Pandoc,
    /// A Blocksatz `artikel.md` - read in full, minus its blog link.
    Blocksatz,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Yaml => "YAML",
            Format::Toml => "TOML",
            Format::MultiMarkdown => "MultiMarkdown",
            Format::Pandoc => "Pandoc",
            Format::Blocksatz => "Blocksatz",
        }
    }
}

/// The frontmatter field a header key ended up in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Title,
    Slug,
    Tags,
    Categories,
    Excerpt,
    FeaturedImage,
    FeaturedImageAlt,
    Lang,
    Status,
    ScheduledAt,
    SeoTitle,
    SeoDescription,
    FocusKeyword,
}

/// What became of one header key.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Field(Field),
    /// A Blocksatz key carried over as it is (author, comments, footnotes …).
    Kept,
    /// A Blocksatz key tying the file to an existing blog post - removed,
    /// so the new article doesn't overwrite that post on its first upload.
    Unlinked,
    /// Not taken over.
    Ignored,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HeaderEntry {
    pub key: String,
    pub value: String,
    pub target: Target,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Import {
    pub doc: Document,
    pub format: Option<Format>,
    pub entries: Vec<HeaderEntry>,
    /// The title came from a leading `# Heading`, which was removed from
    /// the body.
    pub title_from_heading: bool,
}

/// Decodes a text file: UTF-8, else Windows-1252 (old `.txt` exports),
/// without a byte order mark and with `\n` line ends.
pub fn decode(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => bytes.iter().map(|&b| windows_1252(b)).collect(),
    };
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn windows_1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž', '\u{8F}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}', 'ž', 'Ÿ',
    ];
    match b {
        0x80..=0x9F => HIGH[usize::from(b - 0x80)],
        _ => char::from(b),
    }
}

/// Keys only Blocksatz writes - one of them in a `---` header means the
/// file is a Blocksatz article and is read as one.
const BLOCKSATZ_KEYS: &[&str] = &[
    "wp_post_id",
    "wp_site",
    "wp_content_hash",
    "wp_synced_hash",
    "wp_synced_at",
    "wp_modified_gmt",
    "wp_pending_create",
    "wp_featured_media_id",
    "media_json",
    "post_type",
    "markdown_hint",
    "vgwort_ignored",
    "translation_of",
    "author_id",
    "comment_status",
    "parent_id",
    "wp_footnotes",
];

/// Blocksatz keys that point at a post on the blog.
const LINK_KEYS: &[&str] = &[
    "wp_post_id",
    "wp_site",
    "wp_content_hash",
    "wp_synced_hash",
    "wp_synced_at",
    "wp_modified_gmt",
    "wp_pending_create",
    "wp_featured_media_id",
    "translation_of",
    "translation_lang",
    "translation_source_hash",
    "translation_sections",
    "translated_at",
    "translation_reviewed",
];

/// Reads `text` as a new article. `now` is the current local time as
/// `YYYY-MM-DDTHH:MM:00` (a `date` in the header after it schedules the
/// post); `heading_as_title` takes a leading `# Heading` as the title when
/// the header has none.
pub fn import(text: &str, now: &str, heading_as_title: bool) -> Import {
    let (format, pairs, body) = split_header(text);
    let mut import = if format == Some(Format::Blocksatz) {
        import_blocksatz(text, &pairs)
    } else {
        let mut frontmatter = Frontmatter::default();
        let mut entries: Vec<HeaderEntry> = pairs
            .into_iter()
            .map(|(key, value)| {
                let target = apply(&mut frontmatter, &key, &value, now);
                HeaderEntry { key, value: value.display(), target }
            })
            .collect();
        // `draft: true` wins over a date in the future: a draft isn't
        // scheduled, whichever order the two keys come in.
        if entries.iter().any(|e| e.key == "draft" && e.target == Target::Field(Field::Status)) && frontmatter.status == PostStatus::Future {
            frontmatter.status = PostStatus::Draft;
            frontmatter.scheduled_at = None;
            for entry in entries.iter_mut().filter(|e| e.target == Target::Field(Field::ScheduledAt)) {
                entry.target = Target::Ignored;
            }
        }
        Import { doc: Document { frontmatter, body: body.to_string() }, format, entries, title_from_heading: false }
    };
    if heading_as_title && import.doc.frontmatter.title.trim().is_empty() {
        if let Some((title, rest)) = document::split_title_heading(&import.doc.body) {
            let rest = rest.to_string();
            import.doc.frontmatter.title = title;
            import.doc.body = rest;
            import.title_from_heading = true;
        }
    }
    import
}

/// A Blocksatz article, read by `document::parse` and cut loose from its
/// blog post: a copy must never update the original.
fn import_blocksatz(text: &str, pairs: &[(String, Value)]) -> Import {
    let mut doc = document::parse(text);
    let fm = &mut doc.frontmatter;
    fm.wp_post_id = None;
    fm.wp_site = None;
    fm.wp_content_hash = None;
    fm.wp_synced_hash = None;
    fm.wp_synced_at = None;
    fm.wp_modified_gmt = None;
    fm.wp_pending_create = None;
    fm.featured_media_id = None;
    fm.translation = None;
    fm.status = PostStatus::Draft;
    fm.scheduled_at = None;
    for item in &mut fm.media {
        item.wordpress = None;
    }
    let entries = pairs
        .iter()
        .map(|(key, value)| {
            let target = if LINK_KEYS.contains(&key.as_str()) || key == "status" || key == "scheduled_at" {
                Target::Unlinked
            } else {
                field_for(key).map(Target::Field).unwrap_or(Target::Kept)
            };
            HeaderEntry { key: key.clone(), value: value.display(), target }
        })
        .collect();
    Import { doc, format: Some(Format::Blocksatz), entries, title_from_heading: false }
}

#[derive(Debug, Clone, PartialEq)]
enum Value {
    Text(String),
    List(Vec<String>),
}

impl Value {
    fn display(&self) -> String {
        match self {
            Value::Text(text) => text.clone(),
            Value::List(items) => items.join(", "),
        }
    }

    fn text(&self) -> String {
        match self {
            Value::Text(text) => text.trim().to_string(),
            Value::List(items) => items.first().cloned().unwrap_or_default(),
        }
    }

    /// A list, or a comma-separated string as one.
    fn list(&self) -> Vec<String> {
        match self {
            Value::Text(text) => document::parse_list(text),
            Value::List(items) => items.clone(),
        }
    }
}

/// The header's format, its keys (lowercased, nested ones as
/// `parent.child`) and the body after it.
fn split_header(text: &str) -> (Option<Format>, Vec<(String, Value)>, &str) {
    for (delimiter, format) in [("---", Format::Yaml), ("+++", Format::Toml)] {
        if let Some((header, body)) = fenced(text, delimiter) {
            let pairs = if format == Format::Yaml { read_yaml(header) } else { read_toml(header) };
            let format = if format == Format::Yaml && pairs.iter().any(|(k, _)| BLOCKSATZ_KEYS.contains(&k.as_str())) { Format::Blocksatz } else { format };
            return (Some(format), pairs, body);
        }
    }
    if let Some((pairs, body)) = read_pandoc(text) {
        return (Some(Format::Pandoc), pairs, body);
    }
    if let Some((pairs, body)) = read_multimarkdown(text) {
        return (Some(Format::MultiMarkdown), pairs, body);
    }
    (None, Vec::new(), text)
}

/// `delimiter` on the first line and again further down: the lines in
/// between and the body after the closing one.
fn fenced<'a>(text: &'a str, delimiter: &str) -> Option<(&'a str, &'a str)> {
    let rest = text.strip_prefix(delimiter)?;
    let rest = rest.strip_prefix('\n')?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == delimiter {
            let body = &rest[offset + line.len()..];
            return Some((&rest[..offset], body.trim_start_matches('\n')));
        }
        offset += line.len();
    }
    None
}

fn normalize_key(key: &str) -> String {
    key.trim().to_lowercase().replace(['-', ' '], "_")
}

fn unquote(value: &str) -> String {
    document::unquote(value.trim())
}

/// An inline `[a, "b"]` list, or `None` for anything else.
fn inline_list(value: &str) -> Option<Vec<String>> {
    let value = value.trim();
    if !(value.starts_with('[') && value.ends_with(']')) {
        return None;
    }
    Some(document::parse_list(value))
}

fn scalar_or_list(value: &str) -> Value {
    inline_list(value).map(Value::List).unwrap_or_else(|| Value::Text(unquote(value)))
}

/// Flat `key: value`, inline and block lists, and one level of nesting -
/// enough for every front matter seen in the wild.
fn read_yaml(header: &str) -> Vec<(String, Value)> {
    let mut pairs: Vec<(String, Value)> = Vec::new();
    let mut parent: Option<String> = None;
    for line in header.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indented = line.starts_with([' ', '\t']);
        let trimmed = line.trim();
        if let Some(item) = trimmed.strip_prefix("- ").or_else(|| (trimmed == "-").then_some("")) {
            // A block list item belongs to the last key.
            if let Some((_, value)) = pairs.last_mut() {
                let item = unquote(item);
                match value {
                    Value::List(items) => items.push(item),
                    Value::Text(text) if text.is_empty() => *value = Value::List(vec![item]),
                    Value::Text(_) => {}
                }
            }
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else { continue };
        let key = normalize_key(key);
        let key = match (&parent, indented) {
            (Some(parent), true) => format!("{parent}.{key}"),
            _ => key,
        };
        if !indented {
            parent = value.trim().is_empty().then(|| key.clone());
        }
        pairs.push((key, scalar_or_list(value)));
    }
    // A key that only introduced a nested block isn't an entry of its own.
    let parents: Vec<String> = pairs.iter().filter_map(|(k, _)| k.split_once('.').map(|(p, _)| p.to_string())).collect();
    pairs.retain(|(k, v)| !(parents.contains(k) && *v == Value::Text(String::new())));
    pairs
}

/// `key = value` and `[section]` tables.
fn read_toml(header: &str) -> Vec<(String, Value)> {
    let mut pairs = Vec::new();
    let mut section: Option<String> = None;
    for line in header.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(name) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = Some(normalize_key(name.trim_matches(['[', ']'])));
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else { continue };
        let key = normalize_key(key);
        let key = section.as_ref().map(|s| format!("{s}.{key}")).unwrap_or(key);
        pairs.push((key, scalar_or_list(value)));
    }
    pairs
}

/// `% Title`, `% Author`, `% Date` on the first lines.
fn read_pandoc(text: &str) -> Option<(Vec<(String, Value)>, &str)> {
    if !text.starts_with("% ") {
        return None;
    }
    let mut pairs = Vec::new();
    let mut offset = 0;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        let Some(value) = line.trim_end().strip_prefix('%') else { break };
        let key = ["title", "author", "date"].get(index)?;
        pairs.push((key.to_string(), Value::Text(value.trim().to_string())));
        offset += line.len();
    }
    Some((pairs, text[offset..].trim_start_matches('\n')))
}

/// `Key: value` lines from the very first line up to the first blank one.
/// Only when the first key is one this reader knows - so a text starting
/// with "Hinweis: …" stays text.
fn read_multimarkdown(text: &str) -> Option<(Vec<(String, Value)>, &str)> {
    let first = text.lines().next()?;
    let (first_key, _) = first.split_once(':')?;
    let first_key = normalize_key(first_key);
    if first_key.contains('.') || (field_for(&first_key).is_none() && !["author", "date", "base_header_level"].contains(&first_key.as_str())) {
        return None;
    }
    let mut pairs: Vec<(String, Value)> = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.trim().is_empty() {
            offset += line.len();
            break;
        }
        if line.starts_with([' ', '\t']) {
            // Continuation of the previous value.
            if let Some((_, Value::Text(value))) = pairs.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
        } else {
            let (key, value) = line.split_once(':')?;
            if key.trim().is_empty() || key.contains(['#', '[', '!', '<']) {
                return None;
            }
            pairs.push((normalize_key(key), Value::Text(value.trim().to_string())));
        }
        offset += line.len();
    }
    Some((pairs, &text[offset..]))
}

/// The frontmatter field a header key maps to.
fn field_for(key: &str) -> Option<Field> {
    Some(match key {
        "title" => Field::Title,
        "slug" | "permalink" | "url" => Field::Slug,
        "tags" | "tag" | "keywords" | "schlagworte" | "schlagwörter" => Field::Tags,
        "categories" | "category" | "kategorie" | "kategorien" => Field::Categories,
        "excerpt" | "description" | "summary" | "abstract" | "auszug" => Field::Excerpt,
        "featured_image" | "image" | "cover" | "cover.image" | "thumbnail" | "banner" => Field::FeaturedImage,
        "featured_image_alt" | "image_alt" | "cover.alt" => Field::FeaturedImageAlt,
        "lang" | "language" => Field::Lang,
        "status" | "draft" => Field::Status,
        "date" | "publishdate" | "publish_date" => Field::ScheduledAt,
        "seo_title" | "meta_title" | "rank_math_title" => Field::SeoTitle,
        "meta_description" | "seo_description" | "rank_math_description" => Field::SeoDescription,
        "focus_keyword" | "focus_keyphrase" | "rank_math_focus_keyword" => Field::FocusKeyword,
        _ => return None,
    })
}

fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

/// Writes one header value into `fm`. Returns where it went - `Ignored`
/// for unknown keys and for values that don't fit (a date in the past, a
/// `draft: false`, a language that isn't a code).
fn apply(fm: &mut Frontmatter, key: &str, value: &Value, now: &str) -> Target {
    let Some(field) = field_for(key) else { return Target::Ignored };
    let text = value.text();
    let taken = match field {
        Field::Title => {
            fm.title = text;
            true
        }
        Field::Slug => {
            // A permalink like `/2026/10/mein-artikel/` - its last segment.
            let last = text.trim_end_matches('/').rsplit('/').next().unwrap_or_default();
            fm.slug = document::slugify(last);
            !fm.slug.is_empty()
        }
        Field::Tags => {
            fm.tags = value.list();
            !fm.tags.is_empty()
        }
        Field::Categories => {
            fm.categories = value.list();
            !fm.categories.is_empty()
        }
        Field::Excerpt => {
            fm.excerpt = non_empty(text);
            fm.excerpt.is_some()
        }
        Field::FeaturedImage => {
            fm.featured_image = non_empty(text);
            fm.featured_image.is_some()
        }
        Field::FeaturedImageAlt => {
            fm.featured_image_alt = non_empty(text);
            fm.featured_image_alt.is_some()
        }
        Field::Lang => {
            let code = text.to_lowercase().replace('_', "-");
            let code = code.split('-').next().unwrap_or_default().to_string();
            let valid = (2..=3).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_lowercase());
            if valid {
                fm.lang = Some(code);
            }
            valid
        }
        Field::Status => {
            // Never publish straight away from an imported header: only
            // the states that keep the post off the front page are taken.
            match (key, text.to_lowercase().as_str()) {
                ("draft", "true" | "yes") => {
                    fm.status = PostStatus::Draft;
                    true
                }
                ("status", "pending") => {
                    fm.status = PostStatus::Pending;
                    true
                }
                ("status", "private") => {
                    fm.status = PostStatus::Private;
                    true
                }
                ("status", "draft") => true,
                _ => false,
            }
        }
        Field::ScheduledAt => match scheduled_date(&text) {
            Some(date) if date.as_str() > now => {
                fm.scheduled_at = Some(date);
                fm.status = PostStatus::Future;
                true
            }
            _ => false,
        },
        Field::SeoTitle => {
            fm.rank_math_title = non_empty(text);
            fm.rank_math_title.is_some()
        }
        Field::SeoDescription => {
            fm.rank_math_description = non_empty(text);
            fm.rank_math_description.is_some()
        }
        Field::FocusKeyword => {
            fm.rank_math_focus_keyword = non_empty(text);
            fm.rank_math_focus_keyword.is_some()
        }
    };
    if taken { Target::Field(field) } else { Target::Ignored }
}

/// `2026-10-20`, `2026-10-20 14:30`, `2026-10-20T14:30:00+02:00` as
/// `YYYY-MM-DDTHH:MM:00`. A date without a time is taken as 9:00.
fn scheduled_date(text: &str) -> Option<String> {
    let text = text.trim();
    let date = text.get(..10)?;
    let time = text.get(11..16).unwrap_or("09:00");
    document::parse_scheduled_at(&format!("{date} {time}"))
}

/// A local image the article text points to.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageRef {
    /// The reference exactly as written.
    pub source: String,
    /// Where it points, resolved against the source file's folder.
    pub path: PathBuf,
    pub found: bool,
}

/// Whether `source` is a path rather than a URL or anchor.
fn is_local(source: &str) -> bool {
    !(source.is_empty() || source.contains("://") || source.starts_with("data:") || source.starts_with('#') || source.starts_with("mailto:"))
}

fn percent_decode(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| source.to_string())
}

/// `src` attributes of `<img>` tags in raw HTML.
fn html_image_sources(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("<img") {
        rest = &rest[start + 4..];
        let tag = &rest[..rest.find('>').unwrap_or(rest.len())];
        if let Some(pos) = tag.find("src=") {
            let value = &tag[pos + 4..];
            let quote = value.chars().next().filter(|c| *c == '"' || *c == '\'');
            let source = match quote {
                Some(q) => value[1..].split(q).next().unwrap_or_default(),
                None => value.split([' ', '/']).next().unwrap_or_default(),
            };
            out.push(source.to_string());
        }
    }
    out
}

/// Every distinct local image in `body` (Markdown and `<img>`), plus the
/// `featured_image`, resolved against `base` (the source file's folder).
pub fn local_images(body: &str, featured_image: Option<&str>, base: &Path) -> Vec<ImageRef> {
    let mut sources: Vec<String> = Vec::new();
    let markdown = crate::media::scan_images(body).into_iter().map(|(source, _, _)| source);
    for source in featured_image.map(str::to_string).into_iter().chain(markdown).chain(html_image_sources(body)) {
        if is_local(&source) && !sources.contains(&source) {
            sources.push(source);
        }
    }
    sources
        .into_iter()
        .map(|source| {
            let decoded = percent_decode(&source);
            let decoded = decoded.strip_prefix("file://").unwrap_or(&decoded);
            let path = Path::new(decoded);
            let path = if path.is_absolute() { path.to_path_buf() } else { base.join(path) };
            let found = path.is_file();
            ImageRef { source, path, found }
        })
        .collect()
}

/// `body` with every image reference in `map` (old source → new) replaced -
/// as a Markdown link target (`](src)`, `](src "t")`, `](<src>)`), a link
/// reference definition (`]: src`) or an `<img src="…">`. Text that merely
/// mentions the same file name stays as it is.
pub fn rewrite_sources(body: &str, map: &HashMap<String, String>) -> String {
    let mut out = body.to_string();
    for (old, new) in map {
        if old == new {
            continue;
        }
        let forms = [
            (format!("]({old})"), format!("]({new})")),
            (format!("]({old} "), format!("]({new} ")),
            (format!("](<{old}>"), format!("](<{new}>")),
            (format!("src=\"{old}\""), format!("src=\"{new}\"")),
            (format!("src='{old}'"), format!("src='{new}'")),
        ];
        for (from, to) in &forms {
            out = out.replace(from.as_str(), to);
        }
        // Reference definitions, line by line so only whole targets match.
        let definition = format!("]: {old}");
        if out.contains(&definition) {
            out = out
                .split_inclusive('\n')
                .map(|line| match line.trim_start().strip_prefix('[').and_then(|l| l.split_once(&definition)) {
                    Some((_, after)) if after.trim().is_empty() || after.starts_with(' ') => line.replacen(&definition, &format!("]: {new}"), 1),
                    _ => line.to_string(),
                })
                .collect();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-10-06T11:00:00";

    fn target(import: &Import, key: &str) -> Target {
        import.entries.iter().find(|e| e.key == key).map(|e| e.target.clone()).expect("key present")
    }

    #[test]
    fn decodes_utf8_with_bom_and_crlf() {
        assert_eq!(decode(b"\xEF\xBB\xBFa\r\nb\rc"), "a\nb\nc");
    }

    #[test]
    fn decodes_windows_1252() {
        assert_eq!(decode(b"Gr\xfc\xdfe \x84quoted\x93 \x80"), "Grüße „quoted“ €");
    }

    #[test]
    fn reads_jekyll_yaml() {
        let text = "---\ntitle: \"Raspberry Pi Imager 2.0\"\nslug: rpi-imager\ntags: [Raspberry Pi, Imager]\ncategories:\n  - Anleitungen\ndescription: Kurz gesagt\nauthor: Christoph\nlayout: post\n---\n\nText hier.\n";
        let import = import(text, NOW, true);
        let fm = &import.doc.frontmatter;
        assert_eq!(import.format, Some(Format::Yaml));
        assert_eq!(fm.title, "Raspberry Pi Imager 2.0");
        assert_eq!(fm.slug, "rpi-imager");
        assert_eq!(fm.tags, vec!["Raspberry Pi", "Imager"]);
        assert_eq!(fm.categories, vec!["Anleitungen"]);
        assert_eq!(fm.excerpt.as_deref(), Some("Kurz gesagt"));
        assert_eq!(import.doc.body, "Text hier.\n");
        assert_eq!(target(&import, "author"), Target::Ignored);
        assert_eq!(target(&import, "categories"), Target::Field(Field::Categories));
    }

    #[test]
    fn reads_nested_cover_image() {
        let text = "---\ntitle: X\ncover:\n  image: img/a.png\n  alt: Ein Bild\n---\nBody";
        let import = import(text, NOW, true);
        assert_eq!(import.doc.frontmatter.featured_image.as_deref(), Some("img/a.png"));
        assert_eq!(import.doc.frontmatter.featured_image_alt.as_deref(), Some("Ein Bild"));
        assert!(import.entries.iter().all(|e| e.key != "cover"));
    }

    #[test]
    fn reads_hugo_toml() {
        let text = "+++\ntitle = \"Hugo-Artikel\"\ntags = [\"a\", \"b\"]\ndraft = true\ndate = 2027-01-02T08:00:00+01:00\n[params]\nimage = \"x.png\"\n+++\nBody\n";
        let import = import(text, NOW, true);
        let fm = &import.doc.frontmatter;
        assert_eq!(import.format, Some(Format::Toml));
        assert_eq!(fm.title, "Hugo-Artikel");
        assert_eq!(fm.tags, vec!["a", "b"]);
        assert_eq!(fm.status, PostStatus::Draft);
        assert_eq!(fm.scheduled_at, None);
        assert_eq!(target(&import, "date"), Target::Ignored);
        assert_eq!(target(&import, "params.image"), Target::Ignored);
    }

    #[test]
    fn future_date_schedules() {
        let import = import("+++
date = 2027-01-02T08:00:00+01:00
+++
", NOW, true);
        assert_eq!(import.doc.frontmatter.scheduled_at.as_deref(), Some("2027-01-02T08:00:00"));
        assert_eq!(import.doc.frontmatter.status, PostStatus::Future);
    }

    #[test]
    fn past_dates_and_publish_status_are_ignored() {
        let text = "---\ntitle: T\ndate: 2026-09-30\nstatus: publish\n---\nB";
        let import = import(text, NOW, true);
        assert_eq!(import.doc.frontmatter.status, PostStatus::Draft);
        assert_eq!(import.doc.frontmatter.scheduled_at, None);
        assert_eq!(target(&import, "date"), Target::Ignored);
        assert_eq!(target(&import, "status"), Target::Ignored);
    }

    #[test]
    fn date_without_time_schedules_at_nine() {
        let import = import("---\ndate: 2026-12-24\n---\n", NOW, true);
        assert_eq!(import.doc.frontmatter.scheduled_at.as_deref(), Some("2026-12-24T09:00:00"));
    }

    #[test]
    fn reads_multimarkdown() {
        let text = "Title: TUXEDO-Artikel\nAuthor: Christoph\nKeywords: Linux, Notebook\n  Kernel\n\n# Einleitung\n\nText";
        let import = import(text, NOW, true);
        let fm = &import.doc.frontmatter;
        assert_eq!(import.format, Some(Format::MultiMarkdown));
        assert_eq!(fm.title, "TUXEDO-Artikel");
        assert_eq!(fm.tags, vec!["Linux", "Notebook Kernel"]);
        assert_eq!(import.doc.body, "# Einleitung\n\nText");
        assert!(!import.title_from_heading);
    }

    #[test]
    fn prose_with_a_colon_is_not_a_header() {
        let text = "Hinweis: Das ist Text.\nNoch mehr.\n";
        let import = import(text, NOW, true);
        assert_eq!(import.format, None);
        assert_eq!(import.doc.body, text);
    }

    #[test]
    fn reads_pandoc_title_block() {
        let import = import("% Mein Titel\n% Autor\n\nText\n", NOW, true);
        assert_eq!(import.format, Some(Format::Pandoc));
        assert_eq!(import.doc.frontmatter.title, "Mein Titel");
        assert_eq!(import.doc.body, "Text\n");
    }

    #[test]
    fn heading_becomes_title_only_when_wanted() {
        let with = import("# Überschrift\n\nText\n", NOW, true);
        assert_eq!(with.doc.frontmatter.title, "Überschrift");
        assert_eq!(with.doc.body, "Text\n");
        assert!(with.title_from_heading);
        let without = import("# Überschrift\n\nText\n", NOW, false);
        assert_eq!(without.doc.frontmatter.title, "");
        assert_eq!(without.doc.body, "# Überschrift\n\nText\n");
    }

    #[test]
    fn header_title_wins_over_heading() {
        let import = import("---\ntitle: Aus Kopf\n---\n# Überschrift\n", NOW, true);
        assert_eq!(import.doc.frontmatter.title, "Aus Kopf");
        assert!(import.doc.body.starts_with("# Überschrift"));
    }

    #[test]
    fn permalink_gives_slug() {
        let import = import("---\npermalink: /2026/10/mein-artikel/\n---\n", NOW, true);
        assert_eq!(import.doc.frontmatter.slug, "mein-artikel");
    }

    #[test]
    fn blocksatz_article_loses_its_blog_link() {
        let original = Document {
            frontmatter: Frontmatter {
                title: "Alt".into(),
                slug: "alt".into(),
                status: PostStatus::Publish,
                wp_post_id: Some(42),
                wp_site: Some("linuxundich.de".into()),
                wp_synced_hash: Some("abc".into()),
                author_id: Some(3),
                ..Frontmatter::default()
            },
            body: "Text\n".into(),
        };
        let import = import(&document::serialize(&original), NOW, true);
        let fm = &import.doc.frontmatter;
        assert_eq!(import.format, Some(Format::Blocksatz));
        assert_eq!(fm.title, "Alt");
        assert_eq!(fm.wp_post_id, None);
        assert_eq!(fm.wp_site, None);
        assert_eq!(fm.wp_synced_hash, None);
        assert_eq!(fm.status, PostStatus::Draft);
        assert_eq!(fm.author_id, Some(3));
        assert_eq!(target(&import, "wp_post_id"), Target::Unlinked);
        assert_eq!(target(&import, "author_id"), Target::Kept);
        assert_eq!(import.doc.body, "Text\n");
    }

    #[test]
    fn finds_local_images() {
        let dir = std::env::temp_dir().join(format!("blocksatz-textimport-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("img")).unwrap();
        std::fs::write(dir.join("img/a b.png"), b"x").unwrap();
        let body = "![A](img/a%20b.png)\n![Web](https://example.org/x.png)\n<img src=\"fehlt.png\" alt=\"\">\n![A again](img/a%20b.png)\n";
        let images = local_images(body, Some("cover.jpg"), &dir);
        let sources: Vec<(&str, bool)> = images.iter().map(|i| (i.source.as_str(), i.found)).collect();
        assert_eq!(sources, vec![("cover.jpg", false), ("img/a%20b.png", true), ("fehlt.png", false)]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rewrites_only_reference_targets() {
        let body = "![A](img/a.png \"Alt\")\n![B](img/a.png)\nSiehe img/a.png.\n<img src=\"img/a.png\">\n![C][ref]\n\n[ref]: img/a.png\n";
        let map = HashMap::from([("img/a.png".to_string(), "a.png".to_string())]);
        let out = rewrite_sources(body, &map);
        assert_eq!(out, "![A](a.png \"Alt\")\n![B](a.png)\nSiehe img/a.png.\n<img src=\"a.png\">\n![C][ref]\n\n[ref]: a.png\n");
    }
}
