//! Markdown formatting toolbar: cut/copy/paste plus the most-used Markdown
//! constructs, each just inserting/wrapping plain text in the source
//! buffer - no rich-text state to keep in sync, since the buffer IS the
//! Markdown source.

use gtk4::gdk;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;

use crate::i18n::tr;

/// Keyboard shortcuts for the formatting actions that aren't already
/// covered by GtkSourceView's own bindings (cut/copy/paste are).
pub fn install_shortcuts(view: &sourceview5::View, buffer: &sourceview5::Buffer) {
    let controller = gtk4::EventControllerKey::new();
    let buffer = buffer.clone();
    controller.connect_key_pressed(move |_, key, _, state| {
        if !state.contains(gdk::ModifierType::CONTROL_MASK) || state.contains(gdk::ModifierType::ALT_MASK) {
            return glib::Propagation::Proceed;
        }
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        match (key, shift) {
            (gdk::Key::b, false) => wrap_selection(&buffer, "**", "**"),
            (gdk::Key::i, false) => wrap_selection(&buffer, "*", "*"),
            (gdk::Key::k, false) => insert_link(&buffer),
            (gdk::Key::e, false) => wrap_selection(&buffer, "`", "`"),
            (gdk::Key::_0, false) => set_line_style(&buffer, LineStyle::Paragraph),
            (gdk::Key::_2, false) => set_line_style(&buffer, LineStyle::Heading(2)),
            (gdk::Key::_3, false) => set_line_style(&buffer, LineStyle::Heading(3)),
            (gdk::Key::_4, false) => set_line_style(&buffer, LineStyle::Heading(4)),
            (gdk::Key::X | gdk::Key::x, true) => wrap_selection(&buffer, "~~", "~~"),
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    view.add_controller(controller);
}

/// The toolbar: three groups - inline formatting, the kind of the current
/// line/block, and inserting things. Clipboard actions are left to the
/// keyboard and the context menu, as everywhere in GNOME; everything rarer
/// lives in two menus, so the bar fits a normal editor width.
pub fn build(view: &sourceview5::View, buffer: &sourceview5::Buffer) -> gtk4::Box {
    install_icons();
    let _ = view;
    let toolbar = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(2)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(6)
        .margin_end(6)
        .build();
    let actions = gio::SimpleActionGroup::new();
    install_actions(&actions, buffer);
    toolbar.insert_action_group("fmt", Some(&actions));

    for button in [
        icon_button("format-text-bold-symbolic", &tr("Fett (Strg+B)"), buffer, |b| wrap_selection(b, "**", "**")),
        icon_button("format-text-italic-symbolic", &tr("Kursiv (Strg+I)"), buffer, |b| wrap_selection(b, "*", "*")),
        icon_button("format-text-strikethrough-symbolic", &tr("Durchgestrichen (Strg+Umschalt+X)"), buffer, |b| wrap_selection(b, "~~", "~~")),
        icon_button("bs-code-symbolic", &tr("Code (Strg+E)"), buffer, |b| wrap_selection(b, "`", "`")),
        icon_button("insert-link-symbolic", &tr("Link (Strg+K)"), buffer, insert_link),
    ] {
        toolbar.append(&button);
    }
    toolbar.append(&separator());

    toolbar.append(&menu_button("bs-heading-symbolic", &tr("Überschrift"), &heading_menu()));
    for button in [
        icon_button("view-list-bullet-symbolic", &tr("Liste"), buffer, |b| set_line_style(b, LineStyle::Bullet)),
        icon_button("view-list-ordered-symbolic", &tr("Nummerierte Liste"), buffer, |b| set_line_style(b, LineStyle::Ordered)),
        icon_button("bs-quote-symbolic", &tr("Zitat"), buffer, |b| set_line_style(b, LineStyle::Quote)),
        icon_button("bs-code-block-symbolic", &tr("Codeblock"), buffer, insert_code_block),
        icon_button("bs-table-symbolic", &tr("Tabelle"), buffer, insert_table),
    ] {
        toolbar.append(&button);
    }
    toolbar.append(&separator());

    let image = gtk4::Button::builder().icon_name("insert-image-symbolic").tooltip_text(tr("Bild einfügen …")).action_name("win.insert-image").build();
    image.add_css_class("flat");
    toolbar.append(&image);
    toolbar.append(&menu_button("list-add-symbolic", &tr("Einfügen"), &insert_menu()));
    toolbar
}

fn separator() -> gtk4::Separator {
    let separator = gtk4::Separator::new(gtk4::Orientation::Vertical);
    separator.set_margin_start(4);
    separator.set_margin_end(4);
    separator.set_margin_top(6);
    separator.set_margin_bottom(6);
    separator
}

fn menu_button(icon_name: &str, tooltip: &str, menu: &gio::Menu) -> gtk4::MenuButton {
    let button = gtk4::MenuButton::builder().icon_name(icon_name).tooltip_text(tooltip).menu_model(menu).build();
    button.add_css_class("flat");
    button.update_property(&[gtk4::accessible::Property::Label(tooltip)]);
    button
}

fn heading_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let headings = gio::Menu::new();
    for level in 2..=4 {
        let item = gio::MenuItem::new(Some(&tr("Überschrift {n}").replace("{n}", &level.to_string())), Some(&format!("fmt.line-style::h{level}")));
        item.set_attribute_value("accel", Some(&format!("<Control>{level}").to_variant()));
        headings.append_item(&item);
    }
    menu.append_section(None, &headings);
    let paragraph = gio::MenuItem::new(Some(&tr("Normaler Text")), Some("fmt.line-style::p"));
    paragraph.set_attribute_value("accel", Some(&"<Control>0".to_variant()));
    let rest = gio::Menu::new();
    rest.append_item(&paragraph);
    menu.append_section(None, &rest);
    menu
}

fn insert_menu() -> gio::Menu {
    let media = gio::Menu::new();
    media.append(Some(&tr("Video/Audio …")), Some("win.insert-media"));
    media.append(Some(&tr("Aus WordPress-Mediathek …")), Some("win.insert-media-library"));
    media.append(Some(&tr("Bestehenden Artikel verlinken …")), Some("win.insert-post-link"));
    let elements = gio::Menu::new();
    elements.append(Some(&tr("Trenner")), Some("fmt.insert-separator"));
    elements.append(Some(&tr("„Weiterlesen“-Marker")), Some("fmt.insert-more"));
    elements.append(Some(&tr("Fußnote")), Some("fmt.insert-footnote"));
    let containers = gio::Menu::new();
    let dynamic = gio::Menu::new();
    for (id, label, snippet) in BLOCK_SNIPPETS {
        let target = if snippet.starts_with("<!--") { &dynamic } else { &containers };
        target.append(Some(&tr(label)), Some(&format!("fmt.insert-block::{id}")));
    }
    let menu = gio::Menu::new();
    menu.append_section(None, &media);
    menu.append_section(None, &elements);
    menu.append_section(Some(&tr("Container")), &containers);
    menu.append_section(Some(&tr("Vom Blog erzeugt")), &dynamic);
    menu
}

fn install_actions(actions: &gio::SimpleActionGroup, buffer: &sourceview5::Buffer) {
    let insert = gio::SimpleAction::new("insert-block", Some(glib::VariantTy::STRING));
    {
        let buffer = buffer.clone();
        insert.connect_activate(move |_, parameter| {
            let Some(id) = parameter.and_then(|p| p.get::<String>()) else { return };
            if let Some((_, _, snippet)) = BLOCK_SNIPPETS.iter().find(|(snippet_id, _, _)| *snippet_id == id) {
                insert_block(&buffer, snippet);
            }
        });
    }
    actions.add_action(&insert);
    let line_style = gio::SimpleAction::new("line-style", Some(glib::VariantTy::STRING));
    {
        let buffer = buffer.clone();
        line_style.connect_activate(move |_, parameter| {
            let style = match parameter.and_then(|p| p.get::<String>()).as_deref() {
                Some("h2") => LineStyle::Heading(2),
                Some("h3") => LineStyle::Heading(3),
                Some("h4") => LineStyle::Heading(4),
                _ => LineStyle::Paragraph,
            };
            set_line_style(&buffer, style);
        });
    }
    actions.add_action(&line_style);
    let separator = gio::SimpleAction::new("insert-separator", None);
    {
        let buffer = buffer.clone();
        separator.connect_activate(move |_, _| insert_block(&buffer, "---"));
    }
    actions.add_action(&separator);
    let more = gio::SimpleAction::new("insert-more", None);
    {
        let buffer = buffer.clone();
        more.connect_activate(move |_, _| insert_block(&buffer, "<!--more-->"));
    }
    actions.add_action(&more);
    let footnote = gio::SimpleAction::new("insert-footnote", None);
    {
        let buffer = buffer.clone();
        footnote.connect_activate(move |_, _| insert_footnote(&buffer));
    }
    actions.add_action(&footnote);
}

/// The next free numeric footnote label in `text`.
fn next_footnote_label(text: &str) -> u32 {
    text.match_indices("[^")
        .filter_map(|(i, _)| {
            let rest = &text[i + 2..];
            rest[..rest.find(']')?].parse::<u32>().ok()
        })
        .max()
        .unwrap_or(0)
        + 1
}

/// The definition line appended for a new footnote: after a blank line,
/// or right below the previous definition.
fn footnote_definition(text: &str, label: u32) -> String {
    let last_line = text.trim_end_matches('\n').rsplit('\n').next().unwrap_or("");
    let separator = if text.trim().is_empty() {
        ""
    } else if last_line.starts_with("[^") && last_line.contains("]:") {
        "\n"
    } else {
        "\n\n"
    };
    format!("{separator}[^{label}]: ")
}

/// `[^n]` at the cursor and its definition at the end of the text, the
/// cursor there to type the note.
fn insert_footnote(buffer: &sourceview5::Buffer) {
    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
    let label = next_footnote_label(&text);
    buffer.begin_user_action();
    buffer.delete_selection(true, true);
    let mut cursor = buffer.iter_at_mark(&buffer.get_insert());
    buffer.insert(&mut cursor, &format!("[^{label}]"));
    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
    let trimmed_len = text.trim_end_matches('\n').chars().count() as i32;
    let mut end = buffer.iter_at_offset(trimmed_len);
    let definition = footnote_definition(&text, label);
    buffer.insert(&mut end, &definition);
    buffer.end_user_action();
    buffer.place_cursor(&buffer.iter_at_offset(trimmed_len + definition.chars().count() as i32));
}

/// What a line is: the prefix the line-style buttons set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineStyle {
    Paragraph,
    Heading(u8),
    Bullet,
    Ordered,
    Quote,
}

/// The block prefix a line starts with, and its length.
fn line_style_of(line: &str) -> (LineStyle, usize) {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') {
        return (LineStyle::Heading(hashes as u8), hashes + 1);
    }
    for marker in ["- ", "* ", "+ "] {
        if line.starts_with(marker) {
            return (LineStyle::Bullet, 2);
        }
    }
    if line.starts_with("> ") {
        return (LineStyle::Quote, 2);
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && line[digits..].starts_with(". ") {
        return (LineStyle::Ordered, digits + 2);
    }
    (LineStyle::Paragraph, 0)
}

/// One line with `style` - or back to plain text if it already had it
/// (`toggle`). `number` counts ordered list items.
pub fn restyle_line(line: &str, style: LineStyle, toggle: bool, number: usize) -> String {
    let (current, prefix_len) = line_style_of(line);
    let text = &line[prefix_len..];
    let style = if toggle && current == style { LineStyle::Paragraph } else { style };
    match style {
        LineStyle::Paragraph => text.to_string(),
        LineStyle::Heading(level) => format!("{} {text}", "#".repeat(level as usize)),
        LineStyle::Bullet => format!("- {text}"),
        LineStyle::Ordered => format!("{number}. {text}"),
        LineStyle::Quote => format!("> {text}"),
    }
}

/// Gives every line of the selection (or the cursor's line) `style`; if
/// all of them already have it, they go back to plain text. Blank lines in
/// a selection are left alone. One undo step.
fn set_line_style(buffer: &sourceview5::Buffer, style: LineStyle) {
    let (start, end) = buffer.selection_bounds().unwrap_or_else(|| {
        let cursor = buffer.iter_at_mark(&buffer.get_insert());
        (cursor, cursor)
    });
    let first = start.line();
    let mut last = end.line();
    if end.line_offset() == 0 && last > first {
        last -= 1;
    }
    let line_text = |line: i32| -> String {
        let Some(line_start) = buffer.iter_at_line(line) else { return String::new() };
        let mut line_end = line_start;
        if !line_end.ends_line() {
            line_end.forward_to_line_end();
        }
        buffer.text(&line_start, &line_end, false).to_string()
    };
    let lines: Vec<String> = (first..=last).map(line_text).collect();
    let toggle = lines.iter().filter(|l| !l.trim().is_empty()).all(|l| line_style_of(l).0 == style);
    let single = lines.len() == 1;
    buffer.begin_user_action();
    let mut number = 0;
    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() && !single {
            continue;
        }
        number += 1;
        let new_line = restyle_line(line, style, toggle, number);
        if new_line == *line {
            continue;
        }
        let line_no = first + i as i32;
        let Some(mut line_start) = buffer.iter_at_line(line_no) else { continue };
        let mut line_end = line_start;
        if !line_end.ends_line() {
            line_end.forward_to_line_end();
        }
        buffer.delete(&mut line_start, &mut line_end);
        let mut at = buffer.iter_at_line(line_no).unwrap_or(line_start);
        buffer.insert(&mut at, &new_line);
    }
    buffer.end_user_action();
}

/// Icons Adwaita doesn't have (heading, quote, code, table), shipped
/// inside the binary and handed to the icon theme through a small
/// directory in the user cache - works the same from `cargo run` and
/// from the Flatpak.
fn install_icons() {
    const ICONS: &[(&str, &str)] = &[
        ("bs-heading-symbolic", r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><path fill="#2e3436" d="M2 2h2v5h5V2h2v12H9V9H4v5H2zm11 6h1.5v6H13z"/></svg>"##),
        ("bs-quote-symbolic", r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><path fill="#2e3436" d="M2 9a3 3 0 0 1 3-3V4a5 5 0 0 0-5 5v3h5V9zm7 0a3 3 0 0 1 3-3V4a5 5 0 0 0-5 5v3h5V9z" transform="translate(1 0)"/></svg>"##),
        ("bs-code-symbolic", r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><path fill="#2e3436" d="M5.3 3.3 6.7 4.7 3.4 8l3.3 3.3-1.4 1.4L.6 8zm5.4 0L15.4 8l-4.7 4.7-1.4-1.4L12.6 8 9.3 4.7z"/></svg>"##),
        ("bs-code-block-symbolic", r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><path fill="#2e3436" d="M2 1h12a1 1 0 0 1 1 1v12a1 1 0 0 1-1 1H2a1 1 0 0 1-1-1V2a1 1 0 0 1 1-1zm1 2v10h10V3zm2.3 2.3 1.4 1.4L5.4 8l1.3 1.3-1.4 1.4L2.6 8zm5.4 0L13.4 8l-2.7 2.7-1.4-1.4L10.6 8 9.3 6.7z"/></svg>"##),
        ("bs-table-symbolic", r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><path fill="#2e3436" fill-rule="evenodd" d="M1 2h14v12H1zm1.5 1.5v2.5h5V3.5zm6.5 0v2.5h5V3.5zM2.5 7.5v2h5v-2zm6.5 0v2h5v-2zM2.5 11v1.5h5V11zm6.5 0v1.5h5V11z"/></svg>"##),
    ];
    let mut dir = glib::user_cache_dir();
    dir.push(crate::APP_DIR);
    dir.push("icons");
    let actions_dir = dir.join("hicolor").join("scalable").join("actions");
    if std::fs::create_dir_all(&actions_dir).is_err() {
        return;
    }
    for (name, svg) in ICONS {
        let path = actions_dir.join(format!("{name}.svg"));
        if std::fs::read_to_string(&path).ok().as_deref() != Some(*svg) {
            let _ = std::fs::write(&path, svg);
        }
    }
    if let Some(display) = gdk::Display::default() {
        let theme = gtk4::IconTheme::for_display(&display);
        if !theme.search_path().iter().any(|p| p == &dir) {
            theme.add_search_path(&dir);
        }
    }
}

/// The blocks "Block einfügen" offers: id, label, Markdown. The text to
/// select afterwards (so typing replaces it) is the first line that isn't
/// a fence or block comment.
const BLOCK_SNIPPETS: &[(&str, &str, &str)] = &[
    ("group", "Gruppe", "::: group\nText\n:::"),
    ("columns", "Spalten", ":::: columns\n::: column\nLinke Spalte\n:::\n\n::: column\nRechte Spalte\n:::\n::::"),
    ("accordion", "Akkordeon", ":::: accordion\n::: item \"Frage\"\nAntwort\n:::\n\n::: item \"Zweite Frage\"\nAntwort\n:::\n::::"),
    ("tabs", "Reiter", ":::: tabs\n::: tab \"Reiter 1\"\nInhalt\n:::\n\n::: tab \"Reiter 2\"\nInhalt\n:::\n::::"),
    ("cover", "Cover", "::: cover {overlay=contrast dim=60 height=400px}\n## Titel\n:::"),
    ("media-text", "Medien & Text", "::: media-text {image=bild.png}\nText neben dem Bild\n:::"),
    ("details", "Details", "::: details \"Zusammenfassung\"\nInhalt\n:::"),
    ("latest-posts", "Neueste Beiträge", "<!-- wp:latest-posts {\"postsToShow\":5} /-->"),
    ("archives", "Archive", "<!-- wp:archives /-->"),
    ("categories", "Kategorien", "<!-- wp:categories /-->"),
    ("tag-cloud", "Schlagwörter-Wolke", "<!-- wp:tag-cloud /-->"),
    ("search", "Suche", "<!-- wp:search {\"label\":\"Suchen\",\"buttonText\":\"Suchen\"} /-->"),
];

/// Inserts `snippet` as a block of its own at the cursor - blank lines
/// around it as needed - and selects its first editable text.
fn insert_block(buffer: &sourceview5::Buffer, snippet: &str) {
    let mut iter = buffer.iter_at_mark(&buffer.get_insert());
    if !iter.ends_line() {
        iter.forward_to_line_end();
    }
    let line_start = buffer.iter_at_line(iter.line()).unwrap_or(iter);
    let current_line_empty = buffer.text(&line_start, &iter, false).trim().is_empty();
    let before = if current_line_empty { if iter.line() == 0 { "" } else { "\n" } } else { "\n\n" };
    // Exactly one blank line after the block: the line break the cursor's
    // line already has, plus one more unless a blank line follows anyway.
    let mut next_line = iter;
    let next_blank = if next_line.forward_line() {
        let mut next_end = next_line;
        if !next_end.ends_line() {
            next_end.forward_to_line_end();
        }
        buffer.text(&next_line, &next_end, false).trim().is_empty()
    } else {
        false
    };
    let at_end = iter.is_end();
    let after = if at_end { "\n" } else if next_blank { "" } else { "\n" };
    let text = format!("{before}{snippet}{after}");
    buffer.begin_user_action();
    let start = iter.offset();
    buffer.insert(&mut iter, &text);
    buffer.end_user_action();
    let mut offset = start + before.chars().count() as i32;
    for line in snippet.lines() {
        let editable = !line.starts_with(":::") && !line.starts_with("<!--") && !line.is_empty();
        if editable {
            let content = line.trim_start_matches('#').trim_start();
            let skip = (line.chars().count() - content.chars().count()) as i32;
            select(buffer, offset + skip, offset + line.chars().count() as i32);
            return;
        }
        offset += line.chars().count() as i32 + 1;
    }
    buffer.place_cursor(&buffer.iter_at_offset(start + text.chars().count() as i32));
}



fn icon_button<T: Clone + 'static>(icon_name: &str, tooltip: &str, target: &T, action: impl Fn(&T) + 'static) -> gtk4::Button {
    let button = gtk4::Button::from_icon_name(icon_name);
    button.set_tooltip_text(Some(tooltip));
    button.add_css_class("flat");
    button.update_property(&[gtk4::accessible::Property::Label(tooltip)]);
    let target = target.clone();
    button.connect_clicked(move |_| action(&target));
    button
}


/// Wraps the current selection in `prefix`...`suffix`; with no selection,
/// inserts an empty `prefix``suffix` pair with the cursor placed between them.
fn wrap_selection(buffer: &sourceview5::Buffer, prefix: &str, suffix: &str) {
    if let Some((mut start, mut end)) = buffer.selection_bounds() {
        let selected = buffer.text(&start, &end, false).to_string();
        buffer.delete(&mut start, &mut end);
        let pos = start.offset();
        buffer.insert(&mut start, &format!("{prefix}{selected}{suffix}"));
        let inner_start = buffer.iter_at_offset(pos + prefix.chars().count() as i32);
        let inner_end = buffer.iter_at_offset(pos + prefix.chars().count() as i32 + selected.chars().count() as i32);
        buffer.select_range(&inner_end, &inner_start);
    } else {
        let mut iter = buffer.iter_at_mark(&buffer.get_insert());
        let pos = iter.offset();
        buffer.insert(&mut iter, &format!("{prefix}{suffix}"));
        let cursor = buffer.iter_at_offset(pos + prefix.chars().count() as i32);
        buffer.place_cursor(&cursor);
    }
}


fn insert_code_block(buffer: &sourceview5::Buffer) {
    if let Some((mut start, mut end)) = buffer.selection_bounds() {
        let selected = buffer.text(&start, &end, false).to_string();
        buffer.delete(&mut start, &mut end);
        buffer.insert(&mut start, &format!("```\n{selected}\n```"));
    } else {
        let mut iter = buffer.iter_at_mark(&buffer.get_insert());
        let pos = iter.offset();
        buffer.insert(&mut iter, "```\n\n```");
        let cursor = buffer.iter_at_offset(pos + 4); // right after "```\n"
        buffer.place_cursor(&cursor);
    }
}

/// Inserts a minimal 2x2 Markdown table template at the cursor, with the
/// first header cell pre-selected so the user can start typing over it
/// immediately - more rows/columns are just more `| ... |` text, no special
/// UI needed for that.
fn insert_table(buffer: &sourceview5::Buffer) {
    let mut iter = buffer.iter_at_mark(&buffer.get_insert());
    let pos = iter.offset();
    let col1 = tr("Spalte 1");
    let col2 = tr("Spalte 2");
    let cell1 = tr("Zelle 1");
    let cell2 = tr("Zelle 2");
    buffer.insert(&mut iter, &format!("| {col1} | {col2} |\n| --- | --- |\n| {cell1} | {cell2} |\n"));
    let col1_len = col1.chars().count() as i32;
    select(buffer, pos + 2, pos + 2 + col1_len);
}


/// Inserts a Markdown link, selecting the placeholder text (existing
/// selection becomes the link text, or "text"/"url" placeholders otherwise)
/// so the user can immediately type to replace it.
fn insert_link(buffer: &sourceview5::Buffer) {
    if let Some((mut start, mut end)) = buffer.selection_bounds() {
        let selected = buffer.text(&start, &end, false).to_string();
        buffer.delete(&mut start, &mut end);
        let pos = start.offset();
        buffer.insert(&mut start, &format!("[{selected}](url)"));
        let url_start = pos + 1 + selected.chars().count() as i32 + 2;
        select(buffer, url_start, url_start + 3);
    } else {
        let mut iter = buffer.iter_at_mark(&buffer.get_insert());
        let pos = iter.offset();
        buffer.insert(&mut iter, "[text](url)");
        select(buffer, pos + 1, pos + 5);
    }
}

/// Inserts a Markdown link to an already-known `(text, url)` pair - e.g. an
/// existing WordPress post picked via `linkpicker.rs`. Unlike `insert_link`,
/// no placeholder/selection dance is needed since both pieces are already
/// resolved; an existing selection is simply replaced.
pub fn insert_existing_link(buffer: &sourceview5::Buffer, text: &str, url: &str) {
    let destination = markdown_destination(url);
    if let Some((mut start, mut end)) = buffer.selection_bounds() {
        buffer.delete(&mut start, &mut end);
        buffer.insert(&mut start, &format!("[{text}]({destination})"));
    } else {
        let mut iter = buffer.iter_at_mark(&buffer.get_insert());
        buffer.insert(&mut iter, &format!("[{text}]({destination})"));
    }
}

/// CommonMark's plain `(destination)` link/image syntax breaks on raw
/// whitespace or unbalanced parentheses - the parser stops at the first
/// unescaped one, so e.g. `![](my photo.png)` from a file picked via "Bild
/// einfügen" wouldn't be recognized as an image at all, just literal text.
/// Wrapping the destination in `<...>` is also valid CommonMark and is
/// stripped back off by any compliant parser (including `crates/gutenberg`'s
/// own forward converter), so it round-trips such a path without needing to
/// percent-encode or otherwise alter it.
fn markdown_destination(path: &str) -> String {
    if path.chars().any(char::is_whitespace) || path.contains('(') || path.contains(')') {
        format!("<{path}>")
    } else {
        path.to_string()
    }
}

/// Inserts a Markdown image reference for an already-picked file `path`
/// (relative to the document if possible - see `window.rs`'s
/// `wire_insert_image_action`). An existing selection becomes the alt text,
/// mirroring `insert_link`; otherwise the cursor lands right between `![`
/// and `]` so the user can type the alt text immediately (left empty is
/// also valid - Markdown alone can't yet express "deliberately no alt text",
/// see `media.rs`, but leaving it blank here is a fine starting point).
pub fn insert_image(buffer: &sourceview5::Buffer, path: &str) {
    let destination = markdown_destination(path);
    if let Some((mut start, mut end)) = buffer.selection_bounds() {
        let selected = buffer.text(&start, &end, false).to_string();
        buffer.delete(&mut start, &mut end);
        let pos = start.offset();
        buffer.insert(&mut start, &format!("![{selected}]({destination})"));
        let cursor = pos + 2 + selected.chars().count() as i32 + 2 + destination.chars().count() as i32;
        buffer.place_cursor(&buffer.iter_at_offset(cursor));
    } else {
        let mut iter = buffer.iter_at_mark(&buffer.get_insert());
        let pos = iter.offset();
        buffer.insert(&mut iter, &format!("![]({destination})"));
        buffer.place_cursor(&buffer.iter_at_offset(pos + 2));
    }
}

/// Replaces the current selection (if any) with `text` and leaves the
/// cursor right after it - the same behavior GtkSourceView's own plain-text
/// paste already has, used here for the converted-Markdown path of
/// `window.rs`'s `wire_paste_shortcut` since that path bypasses normal
/// paste handling entirely (it hands the buffer already-final text, not a
/// clipboard value GTK pastes on its own).
pub fn insert_pasted_text(buffer: &sourceview5::Buffer, text: &str) {
    if let Some((mut start, mut end)) = buffer.selection_bounds() {
        buffer.delete(&mut start, &mut end);
    }
    let mut iter = buffer.iter_at_mark(&buffer.get_insert());
    buffer.insert(&mut iter, text);
}

fn select(buffer: &sourceview5::Buffer, start_offset: i32, end_offset: i32) {
    let start = buffer.iter_at_offset(start_offset);
    let end = buffer.iter_at_offset(end_offset);
    buffer.select_range(&end, &start);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footnotes_get_the_next_number_and_a_definition_line() {
        assert_eq!(next_footnote_label("Kein Verweis"), 1);
        assert_eq!(next_footnote_label("A[^1] B[^quelle] C[^3]\n\n[^1]: x"), 4);
        assert_eq!(footnote_definition("Text[^1]\n", 1), "\n\n[^1]: ");
        assert_eq!(footnote_definition("Text[^2]\n\n[^1]: Eins\n", 2), "\n[^2]: ");
    }

    #[test]
    fn markdown_destination_wraps_a_path_containing_a_space_in_angle_brackets() {
        assert_eq!(markdown_destination("my photo.png"), "<my photo.png>");
    }

    #[test]
    fn line_styles_replace_each_other_and_toggle_off() {
        assert_eq!(restyle_line("Text", LineStyle::Heading(2), true, 1), "## Text");
        assert_eq!(restyle_line("## Text", LineStyle::Heading(3), true, 1), "### Text");
        assert_eq!(restyle_line("## Text", LineStyle::Heading(2), true, 1), "Text");
        assert_eq!(restyle_line("- Punkt", LineStyle::Ordered, true, 3), "3. Punkt");
        assert_eq!(restyle_line("12. Punkt", LineStyle::Quote, true, 1), "> Punkt");
        assert_eq!(restyle_line("> Zitat", LineStyle::Quote, true, 1), "Zitat");
        assert_eq!(restyle_line("> Zitat", LineStyle::Quote, false, 1), "> Zitat");
        assert_eq!(restyle_line("#hashtag", LineStyle::Bullet, true, 1), "- #hashtag");
    }

    #[test]
    fn markdown_destination_leaves_a_plain_path_unchanged() {
        assert_eq!(markdown_destination("photo.png"), "photo.png");
    }

    #[test]
    fn markdown_destination_wraps_a_path_containing_parentheses() {
        assert_eq!(markdown_destination("photo(1).png"), "<photo(1).png>");
    }
}
