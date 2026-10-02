//! Block attributes that Markdown itself has no syntax for - colors,
//! gradients, font size, alignment, block styles, anchors - written as an
//! attribute list in curly braces, the convention Pandoc, kramdown and
//! markdown-it-attrs share:
//!
//! ```markdown
//! Ein Hinweis in Akzentfarbe.
//! {bg=accent color=base}
//!
//! ## Überschrift {#anker color=accent}
//! ```
//!
//! A line consisting of nothing but such a list applies to the block right
//! above it; a heading can also carry it at its end. Colors, gradients and
//! font sizes are the theme's preset *slugs* (`accent`, `base-2`,
//! `accent-fade`, `large`), not CSS values - exactly what WordPress itself
//! stores in the block comment, and short enough to type.
//!
//! [`BlockAttrs::apply`] writes the attributes into a block's rendered
//! Gutenberg markup (comment JSON plus the classes WordPress's own `save()`
//! adds); [`BlockAttrs::take_from_json`] reads them back out of a block
//! comment for `gutenberg_to_markdown`.

use serde_json::{Map, Value};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BlockAttrs {
    /// `#anker` - the block's HTML `id`.
    pub anchor: Option<String>,
    /// `color=slug` - `textColor`.
    pub text_color: Option<String>,
    /// `bg=slug` - `backgroundColor`.
    pub background: Option<String>,
    /// `gradient=slug` - a background gradient preset.
    pub gradient: Option<String>,
    /// `size=slug` - `fontSize` preset.
    pub font_size: Option<String>,
    /// `align=...` - text alignment for paragraphs and headings
    /// (`left`/`center`/`right`), the block's own alignment for everything
    /// else (`wide`/`full`/`left`/`center`/`right`).
    pub align: Option<String>,
    /// `style=slug` - a registered block style, the `is-style-<slug>` class.
    pub style: Option<String>,
    /// `.klasse` - any further CSS class.
    pub classes: Vec<String>,
    /// `dropcap` - a paragraph's initial.
    pub drop_cap: bool,
    /// `width=...` - an image's display width (`240px`, `100%`).
    pub width: Option<String>,
    /// `start=5` - an ordered list's first number.
    pub start: Option<u32>,
    /// `reversed` - an ordered list counting down.
    pub reversed: bool,
    /// `fixed` - a table with fixed column widths.
    pub fixed_layout: bool,
    /// `caption="..."` - a table's caption. Moved into `Block::Table` by
    /// `Block::with_attrs`, never rendered from here.
    pub caption: Option<String>,
    /// `footer` / `footer=2` - how many of a table's last rows form its
    /// footer. Moved into `Block::Table` like `caption`.
    pub footer_rows: u32,
    /// An image's size (`large`) - not Markdown syntax; set on upload from
    /// what the blog had (`Frontmatter.media`), rendered as `sizeSlug`.
    pub size_slug: Option<String>,
    /// `padding=1.5rem` / `padding="0.5rem 1rem"` - inner spacing, CSS
    /// shorthand with one to four values (`var:preset|spacing|50` for a
    /// theme preset).
    pub padding: Option<String>,
    /// `border="2px dashed #1d4ed8"` - width, style and color, each
    /// optional, in any order.
    pub border: Option<String>,
    /// `radius=8px` - rounded corners.
    pub radius: Option<String>,
    /// `shadow=natural` - a theme shadow preset.
    pub shadow: Option<String>,
}

/// Blocks whose `align=` means text alignment rather than block alignment.
fn aligns_text(block: &str) -> bool {
    matches!(block, "paragraph" | "heading")
}

impl BlockAttrs {
    pub fn is_empty(&self) -> bool {
        *self == BlockAttrs::default()
    }

    /// `self` with every attribute set in `other` overriding it.
    pub fn merged(mut self, other: BlockAttrs) -> BlockAttrs {
        fn or<T>(a: Option<T>, b: Option<T>) -> Option<T> {
            b.or(a)
        }
        self.anchor = or(self.anchor, other.anchor);
        self.text_color = or(self.text_color, other.text_color);
        self.background = or(self.background, other.background);
        self.gradient = or(self.gradient, other.gradient);
        self.font_size = or(self.font_size, other.font_size);
        self.align = or(self.align, other.align);
        self.style = or(self.style, other.style);
        self.width = or(self.width, other.width);
        self.start = or(self.start, other.start);
        for class in other.classes {
            if !self.classes.contains(&class) {
                self.classes.push(class);
            }
        }
        self.drop_cap |= other.drop_cap;
        self.reversed |= other.reversed;
        self.fixed_layout |= other.fixed_layout;
        self.caption = or(self.caption, other.caption);
        self.footer_rows = self.footer_rows.max(other.footer_rows);
        self.size_slug = or(self.size_slug, other.size_slug);
        self.padding = or(self.padding, other.padding);
        self.border = or(self.border, other.border);
        self.radius = or(self.radius, other.radius);
        self.shadow = or(self.shadow, other.shadow);
        self
    }

    /// Parses a whole attribute line (`{bg=accent .klasse}`), surrounding
    /// whitespace allowed. `None` unless *every* token is understood - so a
    /// paragraph that merely happens to be wrapped in braces stays text.
    pub fn parse_line(line: &str) -> Option<BlockAttrs> {
        let inner = line.trim().strip_prefix('{')?.strip_suffix('}')?;
        if inner.contains(['{', '}']) {
            return None;
        }
        Self::parse_tokens(inner)
    }

    /// Parses the tokens inside the braces.
    pub fn parse_tokens(inner: &str) -> Option<BlockAttrs> {
        let mut attrs = BlockAttrs::default();
        let tokens = split_tokens(inner)?;
        if tokens.is_empty() {
            return None;
        }
        for token in tokens {
            if let Some(id) = token.strip_prefix('#') {
                attrs.anchor = Some(non_empty(id)?);
            } else if let Some(class) = token.strip_prefix('.') {
                let class = non_empty(class)?;
                match class.strip_prefix("is-style-") {
                    Some(style) if attrs.style.is_none() => attrs.style = Some(style.to_string()),
                    _ => attrs.classes.push(class),
                }
            } else if let Some((key, value)) = token.split_once('=') {
                let value = non_empty(value)?;
                match key {
                    "color" => attrs.text_color = Some(value),
                    "bg" => attrs.background = Some(value),
                    "gradient" => attrs.gradient = Some(value),
                    "size" => attrs.font_size = Some(value),
                    "align" => attrs.align = Some(value),
                    "style" => attrs.style = Some(value),
                    "width" => attrs.width = Some(value),
                    "start" => attrs.start = Some(value.parse().ok()?),
                    "caption" => attrs.caption = Some(value),
                    "footer" => attrs.footer_rows = value.parse().ok().filter(|n| *n > 0)?,
                    "padding" if (1..=4).contains(&value.split_whitespace().count()) => attrs.padding = Some(value),
                    "border" if parse_border(&value).is_some() => attrs.border = Some(value),
                    "radius" => attrs.radius = Some(value),
                    "shadow" => attrs.shadow = Some(value),
                    _ => return None,
                }
            } else {
                match token.as_str() {
                    "dropcap" => attrs.drop_cap = true,
                    "reversed" => attrs.reversed = true,
                    "fixed" => attrs.fixed_layout = true,
                    "footer" => attrs.footer_rows = 1,
                    _ => return None,
                }
            }
        }
        Some(attrs)
    }

    /// The attribute list as Markdown, braces included - the inverse of
    /// [`parse_line`](Self::parse_line).
    pub fn to_markdown(&self) -> String {
        let mut tokens = Vec::new();
        if let Some(anchor) = &self.anchor {
            tokens.push(format!("#{anchor}"));
        }
        let mut push = |key: &str, value: &Option<String>| {
            if let Some(value) = value {
                tokens.push(format!("{key}={}", quote_value(value)));
            }
        };
        push("style", &self.style);
        push("color", &self.text_color);
        push("bg", &self.background);
        push("gradient", &self.gradient);
        push("size", &self.font_size);
        push("align", &self.align);
        push("width", &self.width);
        push("padding", &self.padding);
        push("border", &self.border);
        push("radius", &self.radius);
        push("shadow", &self.shadow);
        if let Some(start) = self.start {
            tokens.push(format!("start={start}"));
        }
        if self.reversed {
            tokens.push("reversed".to_string());
        }
        if self.drop_cap {
            tokens.push("dropcap".to_string());
        }
        if self.fixed_layout {
            tokens.push("fixed".to_string());
        }
        match self.footer_rows {
            0 => {}
            1 => tokens.push("footer".to_string()),
            n => tokens.push(format!("footer={n}")),
        }
        if let Some(caption) = &self.caption {
            tokens.push(format!("caption=\"{caption}\""));
        }
        tokens.extend(self.classes.iter().map(|c| format!(".{c}")));
        format!("{{{}}}", tokens.join(" "))
    }

    /// Moves every attribute this type models out of a block comment's JSON
    /// object - whatever is left afterwards is what Markdown can't carry.
    /// `fixed_layout` isn't JSON at all (see `take_from_html`).
    pub fn take_from_json(block: &str, json: &mut Map<String, Value>) -> BlockAttrs {
        let mut attrs = BlockAttrs::default();
        let mut take_str = |key: &str| match json.get(key) {
            Some(Value::String(s)) => {
                let s = s.clone();
                json.remove(key);
                Some(s)
            }
            _ => None,
        };
        attrs.text_color = take_str("textColor");
        attrs.background = take_str("backgroundColor");
        attrs.gradient = take_str("gradient");
        attrs.font_size = take_str("fontSize");
        attrs.anchor = take_str("anchor");
        if !aligns_text(block) {
            attrs.align = take_str("align");
        }
        if block == "image" {
            attrs.width = take_str("width");
        }
        if let Some(class_name) = take_str("className") {
            for class in class_name.split_whitespace() {
                match class.strip_prefix("is-style-") {
                    Some(style) if attrs.style.is_none() => attrs.style = Some(style.to_string()),
                    _ => attrs.classes.push(class.to_string()),
                }
            }
        }
        if aligns_text(block) {
            attrs.align = take_nested_str(json, &["style", "typography", "textAlign"]);
        }
        take_box_styles(json, &mut attrs);
        if block == "paragraph" && json.get("dropCap") == Some(&Value::Bool(true)) {
            json.remove("dropCap");
            attrs.drop_cap = true;
        }
        if block == "list" {
            if let Some(start) = json.get("start").and_then(Value::as_u64) {
                json.remove("start");
                attrs.start = u32::try_from(start).ok();
            }
            if json.get("reversed") == Some(&Value::Bool(true)) {
                json.remove("reversed");
                attrs.reversed = true;
            }
        }
        attrs
    }

    /// Writes the attributes into one rendered block (`<!-- wp:name ... -->`
    /// followed by its markup): merged into the comment JSON, and as the
    /// classes/`id` WordPress's own `save()` puts on the block's outermost
    /// element. Markup that doesn't start with a block comment is returned
    /// unchanged.
    pub fn apply(&self, html: &str) -> String {
        if self.is_empty() {
            return html.to_string();
        }
        let Some((name, mut json, rest)) = split_comment(html) else {
            return html.to_string();
        };
        self.merge_json(&name, &mut json);
        let comment = if json.is_empty() {
            format!("<!-- wp:{name} -->")
        } else {
            format!("<!-- wp:{name} {} -->", Value::Object(json))
        };

        let mut body = rest.to_string();
        let mut classes = Vec::new();
        if let Some(align) = &self.align {
            classes.push(if aligns_text(&name) { format!("has-text-align-{align}") } else { format!("align{align}") });
        }
        if let Some(color) = &self.text_color {
            classes.push(format!("has-{color}-color"));
        }
        if let Some(bg) = &self.background {
            if name == "separator" {
                // A separator's "color" is its background, mirrored into
                // the text color so the dotted style picks it up too.
                classes.push(format!("has-{bg}-color"));
                classes.push("has-text-color".to_string());
            }
            classes.push(format!("has-{bg}-background-color"));
        }
        if let Some(gradient) = &self.gradient {
            classes.push(format!("has-{gradient}-gradient-background"));
        }
        if self.text_color.is_some() {
            classes.push("has-text-color".to_string());
        }
        if self.background.is_some() || self.gradient.is_some() {
            classes.push("has-background".to_string());
        }
        if let Some(size) = &self.font_size {
            classes.push(format!("has-{size}-font-size"));
        }
        if self.drop_cap {
            classes.push("has-drop-cap".to_string());
        }
        if name == "image" && self.width.as_deref().is_some_and(|w| w != "100%") {
            classes.push("is-resized".to_string());
        }
        if let Some(style) = &self.style {
            classes.push(format!("is-style-{style}"));
        }
        if name == "image" {
            if let Some(size) = &self.size_slug {
                classes.push(format!("size-{size}"));
            }
        }
        // Border and shadow sit on an image's `<img>`, the rest on the
        // block's outer element.
        let has_border = self.border.is_some() || self.radius.is_some();
        let border_color = self.border.as_deref().and_then(parse_border).is_some_and(|(_, _, color)| color.is_some());
        if name == "image" {
            if has_border {
                classes.push("has-custom-border".to_string());
            }
        } else if border_color {
            classes.push("has-border-color".to_string());
        }
        classes.extend(self.classes.iter().cloned());

        let box_style = self.box_style(name == "image");
        let mut extra = Vec::new();
        if let Some(anchor) = &self.anchor {
            extra.push(("id", anchor.clone()));
        }
        let outer_style = (name != "image" && !box_style.is_empty()).then_some(box_style.as_str());
        body = edit_first_tag(&body, None, &classes, &extra, outer_style);
        if name == "image" && !box_style.is_empty() {
            let img_classes = if border_color { vec!["has-border-color".to_string()] } else { Vec::new() };
            body = edit_first_tag(&body, Some("img"), &img_classes, &[], Some(&box_style));
        }

        if name == "image" {
            if let Some(width) = &self.width {
                let style = if width == "100%" { format!("width:{width}") } else { format!("width:{width};height:auto") };
                body = edit_first_tag(&body, Some("img"), &[], &[], Some(&style));
            }
        }
        if name == "list" && (self.start.is_some() || self.reversed) {
            let mut list_attrs = Vec::new();
            if self.reversed {
                list_attrs.push(("reversed", String::new()));
            }
            if let Some(start) = self.start {
                list_attrs.push(("start", start.to_string()));
            }
            body = edit_first_tag(&body, Some("ol"), &[], &list_attrs, None);
        }
        if name == "table" && self.fixed_layout {
            body = edit_first_tag(&body, Some("table"), &["has-fixed-layout".to_string()], &[], None);
        }
        format!("{comment}{body}")
    }

    /// The inline CSS WordPress writes for padding, border and shadow (for
    /// an image without the padding, which it doesn't support).
    fn box_style(&self, image: bool) -> String {
        let mut parts = Vec::new();
        if let Some((width, style, color)) = self.border.as_deref().and_then(parse_border) {
            if let Some(color) = color {
                parts.push(format!("border-color:{color}"));
            }
            if let Some(style) = style {
                parts.push(format!("border-style:{style}"));
            }
            if let Some(width) = width {
                parts.push(format!("border-width:{width}"));
            }
        }
        if let Some(radius) = &self.radius {
            parts.push(format!("border-radius:{}", css_value(radius)));
        }
        if !image {
            if let Some([top, right, bottom, left]) = self.padding.as_deref().and_then(expand_box) {
                for (side, value) in [("top", top), ("right", right), ("bottom", bottom), ("left", left)] {
                    parts.push(format!("padding-{side}:{}", css_value(&value)));
                }
            }
        }
        if let Some(shadow) = &self.shadow {
            parts.push(format!("box-shadow:var(--wp--preset--shadow--{shadow})"));
        }
        parts.join(";")
    }

    fn merge_json(&self, name: &str, json: &mut Map<String, Value>) {
        let mut set = |key: &str, value: &Option<String>| {
            if let Some(value) = value {
                json.insert(key.to_string(), Value::String(value.clone()));
            }
        };
        set("textColor", &self.text_color);
        set("backgroundColor", &self.background);
        set("gradient", &self.gradient);
        set("fontSize", &self.font_size);
        set("anchor", &self.anchor);
        if name == "image" {
            set("width", &self.width);
            if let Some(size) = &self.size_slug {
                json.insert("sizeSlug".to_string(), Value::String(size.clone()));
                json.entry("linkDestination").or_insert_with(|| Value::String("none".to_string()));
            }
        }
        if let Some(align) = &self.align {
            if aligns_text(name) {
                let style = json.entry("style").or_insert_with(|| Value::Object(Map::new()));
                if let Value::Object(style) = style {
                    let typography = style.entry("typography").or_insert_with(|| Value::Object(Map::new()));
                    if let Value::Object(typography) = typography {
                        typography.insert("textAlign".to_string(), Value::String(align.clone()));
                    }
                }
            } else {
                json.insert("align".to_string(), Value::String(align.clone()));
            }
        }
        let mut class_tokens: Vec<String> = json.get("className").and_then(Value::as_str).map(|s| s.split_whitespace().map(str::to_string).collect()).unwrap_or_default();
        let ours = self.style.iter().map(|s| format!("is-style-{s}")).chain(self.classes.iter().cloned());
        for class in ours {
            if !class_tokens.contains(&class) {
                class_tokens.push(class);
            }
        }
        if !class_tokens.is_empty() {
            json.insert("className".to_string(), Value::String(class_tokens.join(" ")));
        }
        if self.drop_cap {
            json.insert("dropCap".to_string(), Value::Bool(true));
        }
        let mut style = Map::new();
        if let Some([top, right, bottom, left]) = self.padding.as_deref().and_then(expand_box) {
            let padding: Map<String, Value> = [("top", top), ("right", right), ("bottom", bottom), ("left", left)].into_iter().map(|(k, v)| (k.to_string(), Value::String(v))).collect();
            style.insert("spacing".to_string(), Value::Object([("padding".to_string(), Value::Object(padding))].into_iter().collect()));
        }
        let mut border = Map::new();
        if let Some((width, line, color)) = self.border.as_deref().and_then(parse_border) {
            for (key, value) in [("width", width), ("style", line), ("color", color)] {
                if let Some(value) = value {
                    border.insert(key.to_string(), Value::String(value.to_string()));
                }
            }
        }
        if let Some(radius) = &self.radius {
            border.insert("radius".to_string(), Value::String(radius.clone()));
        }
        if !border.is_empty() {
            style.insert("border".to_string(), Value::Object(border));
        }
        if let Some(shadow) = &self.shadow {
            style.insert("shadow".to_string(), Value::String(format!("var:preset|shadow|{shadow}")));
        }
        if !style.is_empty() {
            if let Value::Object(existing) = json.entry("style").or_insert_with(|| Value::Object(Map::new())) {
                existing.extend(style);
            }
        }
        if name == "list" {
            if let Some(start) = self.start {
                json.insert("start".to_string(), Value::from(start));
            }
            if self.reversed {
                json.insert("reversed".to_string(), Value::Bool(true));
            }
        }
    }
}

const BORDER_STYLES: &[&str] = &["solid", "dashed", "dotted", "double", "groove", "ridge", "inset", "outset", "none"];

/// `2px dashed #1d4ed8` -> (width, style, color), each optional. `None`
/// for a token that is none of the three, or a part given twice.
fn parse_border(value: &str) -> Option<(Option<&str>, Option<&str>, Option<&str>)> {
    let (mut width, mut style, mut color) = (None, None, None);
    for token in value.split_whitespace() {
        let slot = if BORDER_STYLES.contains(&token) {
            &mut style
        } else if token.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
            &mut width
        } else if token.starts_with('#') || token.starts_with("rgb") || token.starts_with("hsl") || token.starts_with("var(") || token.chars().all(|c| c.is_ascii_alphabetic()) {
            &mut color
        } else {
            return None;
        };
        if slot.replace(token).is_some() {
            return None;
        }
    }
    (width.is_some() || style.is_some() || color.is_some()).then_some((width, style, color))
}

/// CSS shorthand (`0.5rem 1rem`) -> top, right, bottom, left.
fn expand_box(value: &str) -> Option<[String; 4]> {
    let v: Vec<String> = value.split_whitespace().map(str::to_string).collect();
    Some(match v.len() {
        1 => [v[0].clone(), v[0].clone(), v[0].clone(), v[0].clone()],
        2 => [v[0].clone(), v[1].clone(), v[0].clone(), v[1].clone()],
        3 => [v[0].clone(), v[1].clone(), v[2].clone(), v[1].clone()],
        4 => [v[0].clone(), v[1].clone(), v[2].clone(), v[3].clone()],
        _ => return None,
    })
}

/// The shortest CSS shorthand for four sides.
fn compress_box(top: &str, right: &str, bottom: &str, left: &str) -> String {
    if right != left {
        format!("{top} {right} {bottom} {left}")
    } else if top != bottom {
        format!("{top} {right} {bottom}")
    } else if top != right {
        format!("{top} {right}")
    } else {
        top.to_string()
    }
}

/// A block-comment value as CSS: theme presets (`var:preset|spacing|50`)
/// become their custom property.
fn css_value(value: &str) -> String {
    match value.strip_prefix("var:") {
        Some(preset) => format!("var(--wp--{})", preset.replace('|', "--")),
        None => value.to_string(),
    }
}

/// Moves padding (all four sides), border (width/style/color/radius as
/// plain values) and a preset shadow out of `json.style` - anything more
/// specific (one side's border, a border color preset) stays and keeps
/// the block raw.
fn take_box_styles(json: &mut Map<String, Value>, attrs: &mut BlockAttrs) {
    let Some(Value::Object(style)) = json.get_mut("style") else { return };
    let padding = style.get("spacing").and_then(|s| s.get("padding")).and_then(Value::as_object).and_then(|p| {
        let side = |k: &str| p.get(k).and_then(Value::as_str).filter(|v| !v.contains(char::is_whitespace));
        (p.len() == 4).then(|| Some(compress_box(side("top")?, side("right")?, side("bottom")?, side("left")?))).flatten()
    });
    if let Some(padding) = padding {
        attrs.padding = Some(padding);
        if let Some(Value::Object(spacing)) = style.get_mut("spacing") {
            spacing.remove("padding");
            if spacing.is_empty() {
                style.remove("spacing");
            }
        }
    }
    let border = style.get("border").and_then(Value::as_object).filter(|b| b.iter().all(|(k, v)| matches!(k.as_str(), "width" | "style" | "color" | "radius") && v.as_str().is_some_and(|v| !v.contains(char::is_whitespace))));
    if let Some(border) = border {
        let get = |k: &str| border.get(k).and_then(Value::as_str);
        let line: Vec<&str> = [get("width"), get("style"), get("color")].into_iter().flatten().collect();
        let line = line.join(" ");
        if line.is_empty() || parse_border(&line).is_some() {
            attrs.border = (!line.is_empty()).then_some(line);
            attrs.radius = get("radius").map(str::to_string);
            style.remove("border");
        }
    }
    if let Some(shadow) = style.get("shadow").and_then(Value::as_str).and_then(|s| s.strip_prefix("var:preset|shadow|")).map(str::to_string) {
        attrs.shadow = Some(shadow);
        style.remove("shadow");
    }
    if style.is_empty() {
        json.remove("style");
    }
}

/// Splits `<!-- wp:name {json} -->rest` into its parts.
pub(crate) fn split_comment(html: &str) -> Option<(String, Map<String, Value>, &str)> {
    let body = html.strip_prefix("<!-- wp:")?;
    let end = body.find("-->")?;
    let inner = body[..end].trim();
    let (name, json_text) = match inner.find(char::is_whitespace) {
        Some(ws) => (&inner[..ws], inner[ws..].trim()),
        None => (inner, ""),
    };
    let json = if json_text.is_empty() {
        Map::new()
    } else {
        match serde_json::from_str::<Value>(json_text).ok()? {
            Value::Object(map) => map,
            _ => return None,
        }
    };
    Some((name.to_string(), json, &body[end + 3..]))
}

/// Removes `json.style.typography.textAlign` (or whatever nested path),
/// pruning objects that end up empty.
fn take_nested_str(json: &mut Map<String, Value>, path: &[&str]) -> Option<String> {
    let (first, rest) = path.split_first()?;
    if rest.is_empty() {
        let value = json.get(*first)?.as_str()?.to_string();
        json.remove(*first);
        return Some(value);
    }
    let child = json.get_mut(*first)?.as_object_mut()?;
    let value = take_nested_str(child, rest);
    if child.is_empty() {
        json.remove(*first);
    }
    value
}

/// Adds classes, attributes and an inline style to the first tag of `html`
/// (or the first `<tag` of the given name). Existing classes are kept and
/// not duplicated; `style` is appended to an existing `style` attribute.
pub(crate) fn edit_first_tag(html: &str, tag: Option<&str>, classes: &[String], extra: &[(&str, String)], style: Option<&str>) -> String {
    let start = match tag {
        Some(tag) => find_tag(html, tag),
        None => html.find('<').filter(|&i| !html[i..].starts_with("<!--")),
    };
    let Some(start) = start else { return html.to_string() };
    let Some(end_rel) = html[start..].find('>') else { return html.to_string() };
    let end = start + end_rel;
    let self_closing = html[..end].ends_with('/');
    let tag_body = &html[start + 1..if self_closing { end - 1 } else { end }];
    let name_end = tag_body.find(char::is_whitespace).unwrap_or(tag_body.len());
    let name = &tag_body[..name_end];
    let mut attrs = parse_tag_attrs(&tag_body[name_end..]);

    if !classes.is_empty() {
        let existing = attrs.iter().position(|(k, _)| k == "class");
        let mut tokens: Vec<String> = existing.map(|i| attrs[i].1.split_whitespace().map(str::to_string).collect()).unwrap_or_default();
        for class in classes {
            if !tokens.contains(class) {
                tokens.push(class.clone());
            }
        }
        match existing {
            Some(i) => attrs[i].1 = tokens.join(" "),
            None => attrs.push(("class".to_string(), tokens.join(" "))),
        }
    }
    for (key, value) in extra {
        match attrs.iter().position(|(k, _)| k == key) {
            Some(i) => attrs[i].1 = value.clone(),
            None => attrs.push((key.to_string(), value.clone())),
        }
    }
    if let Some(style) = style {
        match attrs.iter().position(|(k, _)| k == "style") {
            Some(i) => {
                let existing = attrs[i].1.trim_end_matches(';').to_string();
                attrs[i].1 = if existing.is_empty() { style.to_string() } else { format!("{existing};{style}") };
            }
            None => attrs.push(("style".to_string(), style.to_string())),
        }
    }

    let mut tag_text = format!("<{name}");
    for (key, value) in &attrs {
        if value.is_empty() && matches!(key.as_str(), "reversed" | "open" | "controls") {
            tag_text.push_str(&format!(" {key}"));
        } else {
            tag_text.push_str(&format!(" {key}=\"{value}\""));
        }
    }
    tag_text.push_str(if self_closing { "/>" } else { ">" });
    format!("{}{}{}", &html[..start], tag_text, &html[end + 1..])
}

fn find_tag(html: &str, tag: &str) -> Option<usize> {
    let needle = format!("<{tag}");
    let mut from = 0;
    while let Some(rel) = html[from..].find(&needle) {
        let pos = from + rel;
        let after = html[pos + needle.len()..].chars().next();
        if matches!(after, Some(c) if c.is_whitespace() || c == '>' || c == '/') {
            return Some(pos);
        }
        from = pos + needle.len();
    }
    None
}

/// `key="value"` pairs of a tag (bare boolean attributes get an empty value).
pub(crate) fn parse_tag_attrs(s: &str) -> Vec<(String, String)> {
    let mut attrs = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        let key_start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'=' && bytes[i] != b'/' {
            i += 1;
        }
        if key_start == i {
            break;
        }
        let key = s[key_start..i].to_lowercase();
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            let value = if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                let value_start = i + 1;
                let value_end = s[value_start..].find(quote as char).map(|e| value_start + e).unwrap_or(s.len());
                i = (value_end + 1).min(s.len());
                &s[value_start..value_end]
            } else {
                let value_start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                &s[value_start..i]
            };
            attrs.push((key, value.to_string()));
        } else {
            attrs.push((key, String::new()));
        }
    }
    attrs
}

/// Like `split_tokens`, but a quoted token keeps its quotes (so a caller
/// can tell a quoted title from a bare word), and `\"` is a literal quote.
pub(crate) fn split_tokens_keeping_quotes(inner: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if in_quotes && chars.peek() == Some(&'"') => {
                current.push('\\');
                current.push(chars.next().unwrap_or('"'));
            }
            '"' => {
                in_quotes = !in_quotes;
                current.push('"');
            }
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if in_quotes {
        return None;
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Some(tokens)
}

fn split_tokens(inner: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for c in inner.chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if in_quotes {
        return None;
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Some(tokens)
}

fn quote_value(value: &str) -> String {
    if value.is_empty() || value.contains(char::is_whitespace) {
        format!("\"{value}\"")
    } else {
        value.to_string()
    }
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_writes_an_attribute_line() {
        let attrs = BlockAttrs::parse_line("{bg=accent color=base #hinweis .extra}").unwrap();
        assert_eq!(attrs.background.as_deref(), Some("accent"));
        assert_eq!(attrs.text_color.as_deref(), Some("base"));
        assert_eq!(attrs.anchor.as_deref(), Some("hinweis"));
        assert_eq!(attrs.classes, vec!["extra"]);
        assert_eq!(BlockAttrs::parse_line(&attrs.to_markdown()), Some(attrs));
    }

    #[test]
    fn rejects_lines_with_unknown_tokens() {
        assert_eq!(BlockAttrs::parse_line("{Hallo Welt}"), None);
        assert_eq!(BlockAttrs::parse_line("{}"), None);
        assert_eq!(BlockAttrs::parse_line("{foo=bar}"), None);
        assert_eq!(BlockAttrs::parse_line("{start=x}"), None);
    }

    #[test]
    fn style_class_becomes_style() {
        let attrs = BlockAttrs::parse_line("{.is-style-stripes}").unwrap();
        assert_eq!(attrs.style.as_deref(), Some("stripes"));
    }

    #[test]
    fn applies_colors_to_a_paragraph() {
        let attrs = BlockAttrs { text_color: Some("base".into()), gradient: Some("accent-fade".into()), ..Default::default() };
        let out = attrs.apply("<!-- wp:paragraph -->\n<p>Text</p>\n<!-- /wp:paragraph -->");
        assert_eq!(out, "<!-- wp:paragraph {\"gradient\":\"accent-fade\",\"textColor\":\"base\"} -->\n<p class=\"has-base-color has-accent-fade-gradient-background has-text-color has-background\">Text</p>\n<!-- /wp:paragraph -->");
    }

    #[test]
    fn text_alignment_goes_into_typography_for_headings() {
        let attrs = BlockAttrs { align: Some("center".into()), anchor: Some("a".into()), ..Default::default() };
        let out = attrs.apply("<!-- wp:heading {\"level\":3} -->\n<h3 class=\"wp-block-heading\">T</h3>\n<!-- /wp:heading -->");
        assert!(out.starts_with("<!-- wp:heading {\"anchor\":\"a\",\"level\":3,\"style\":{\"typography\":{\"textAlign\":\"center\"}}} -->"), "{out}");
        assert!(out.contains("<h3 class=\"wp-block-heading has-text-align-center\" id=\"a\">"), "{out}");
    }

    #[test]
    fn json_round_trips_through_take() {
        let mut json: Map<String, Value> = serde_json::from_str(r#"{"level":3,"textColor":"accent","anchor":"x","style":{"typography":{"textAlign":"center"}}}"#).unwrap();
        let attrs = BlockAttrs::take_from_json("heading", &mut json);
        assert_eq!(attrs.align.as_deref(), Some("center"));
        assert_eq!(attrs.anchor.as_deref(), Some("x"));
        assert_eq!(Value::Object(json), serde_json::json!({"level":3}));
    }

    #[test]
    fn image_width_lands_on_the_img_tag() {
        let attrs = BlockAttrs { width: Some("100%".into()), ..Default::default() };
        let out = attrs.apply("<!-- wp:image -->\n<figure class=\"wp-block-image\"><img src=\"a.png\" alt=\"\"/></figure>\n<!-- /wp:image -->");
        assert!(out.contains("<img src=\"a.png\" alt=\"\" style=\"width:100%\"/>"), "{out}");
        assert!(!out.contains("is-resized"), "{out}");
    }
}
