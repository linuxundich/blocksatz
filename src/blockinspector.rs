//! The "Block" section of the "Beitrag" view: the design of the block the
//! cursor is in - text color, background (color or gradient), font size,
//! alignment, block style - offered exactly from the blog theme's presets
//! (`themestyle`). A change rewrites the block's attribute line
//! (`{bg=accent}`, see `crates/gutenberg/src/attrs.rs`) as one undoable
//! edit, so the syntax never has to be typed by hand.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk4::glib;

use crate::i18n::tr;
use crate::themestyle::{self, ThemeStyle};

/// What a block can be given - mirrors what WordPress supports per block.
#[derive(Default, Clone, Copy)]
struct Supports {
    color: bool,
    background: bool,
    gradient: bool,
    size: bool,
    text_align: bool,
    block_align: bool,
}

fn supports(name: &str) -> Supports {
    let text = Supports { color: true, background: true, gradient: true, size: true, ..Default::default() };
    match name {
        "paragraph" | "heading" => Supports { text_align: true, ..text },
        "list" | "quote" | "code" => text,
        "table" => Supports { color: true, background: true, gradient: true, block_align: true, ..Default::default() },
        "image" | "video" | "audio" | "embed" | "gallery" | "cover" => Supports { block_align: true, ..Default::default() },
        "separator" => Supports { background: true, ..Default::default() },
        "group" | "columns" | "pullquote" => Supports { color: true, background: true, gradient: true, block_align: true, ..Default::default() },
        "column" | "item" | "tab" | "accordion" | "tabs" | "details" => Supports { color: true, background: true, gradient: true, ..Default::default() },
        _ => Supports::default(),
    }
}

/// The block type `themestyle` lists registered styles under.
fn style_block(name: &str) -> String {
    match name {
        "item" => "core/accordion-item".to_string(),
        "tab" => "core/tab-panel".to_string(),
        other => format!("core/{other}"),
    }
}

fn block_label(name: &str) -> String {
    match name {
        "paragraph" => tr("Absatz"),
        "heading" => tr("Überschrift"),
        "list" => tr("Liste"),
        "quote" => tr("Zitat"),
        "code" => tr("Code"),
        "image" => tr("Bild"),
        "video" => tr("Video"),
        "audio" => tr("Audio"),
        "embed" => tr("Einbettung"),
        "separator" => tr("Trenner"),
        "table" => tr("Tabelle"),
        "columns" => tr("Spalten"),
        "column" => tr("Spalte"),
        "buttons" => tr("Buttons"),
        "gallery" => tr("Galerie"),
        "pullquote" => tr("Hervorgehobenes Zitat"),
        "details" => tr("Details"),
        "group" => tr("Gruppe"),
        "accordion" => tr("Akkordeon"),
        "item" => tr("Akkordeon-Eintrag"),
        "tabs" => tr("Reiter"),
        "tab" => tr("Reiter-Inhalt"),
        "cover" => tr("Cover"),
        other => other.to_string(),
    }
}

#[derive(Clone, Copy)]
enum Field {
    Size,
    Align,
    Style,
}

/// A background or text color choice.
#[derive(Clone, PartialEq)]
enum Choice {
    None,
    Color(String),
    Gradient(String),
}

/// A row with a swatch button that opens the palette.
struct SwatchRow {
    row: adw::ActionRow,
    button: gtk4::MenuButton,
    swatch: gtk4::Box,
    colors: gtk4::FlowBox,
    gradients_label: gtk4::Label,
    gradients: gtk4::FlowBox,
}

impl SwatchRow {
    fn new(title: &str) -> SwatchRow {
        let swatch = gtk4::Box::builder().width_request(20).height_request(20).valign(gtk4::Align::Center).build();
        swatch.add_css_class("bs-swatch");
        let colors = flow_box();
        let gradients = flow_box();
        let gradients_label = gtk4::Label::builder().label(tr("Verläufe")).xalign(0.0).build();
        gradients_label.add_css_class("heading");
        let colors_label = gtk4::Label::builder().label(tr("Farben")).xalign(0.0).build();
        colors_label.add_css_class("heading");
        let content = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(6).margin_top(6).margin_bottom(6).margin_start(6).margin_end(6).build();
        content.append(&colors_label);
        content.append(&colors);
        content.append(&gradients_label);
        content.append(&gradients);
        let popover = gtk4::Popover::builder().child(&content).build();
        let button = gtk4::MenuButton::builder().child(&swatch).popover(&popover).valign(gtk4::Align::Center).tooltip_text(title).build();
        let row = adw::ActionRow::builder().title(title).activatable_widget(&button).build();
        row.add_suffix(&button);
        SwatchRow { row, button, swatch, colors, gradients_label, gradients }
    }

    fn show(&self, choice: &Choice) {
        for class in self.swatch.css_classes() {
            if class.starts_with("bs-swatch-") {
                self.swatch.remove_css_class(&class);
            }
        }
        let (class, tooltip) = match choice {
            Choice::None => ("bs-swatch-none".to_string(), tr("Keine")),
            Choice::Color(slug) => (format!("bs-swatch-c-{}", css_ident(slug)), slug.clone()),
            Choice::Gradient(slug) => (format!("bs-swatch-g-{}", css_ident(slug)), slug.clone()),
        };
        self.swatch.add_css_class(&class);
        self.button.set_tooltip_text(Some(&tooltip));
    }

    /// Fills the palette; `on_pick` gets the chosen preset.
    fn populate(&self, style: &ThemeStyle, with_gradients: bool, on_pick: Rc<dyn Fn(Choice)>) {
        for flow in [&self.colors, &self.gradients] {
            while let Some(child) = flow.first_child() {
                flow.remove(&child);
            }
        }
        let popover = self.button.popover();
        let add = |flow: &gtk4::FlowBox, choice: Choice, class: String, tooltip: String| {
            let button = gtk4::Button::builder().tooltip_text(&tooltip).width_request(28).height_request(28).build();
            button.add_css_class("bs-swatch");
            button.add_css_class(&class);
            button.update_property(&[gtk4::accessible::Property::Label(&tooltip)]);
            let on_pick = on_pick.clone();
            let popover = popover.clone();
            button.connect_clicked(move |_| {
                if let Some(popover) = &popover {
                    popover.popdown();
                }
                on_pick(choice.clone());
            });
            flow.insert(&button, -1);
        };
        add(&self.colors, Choice::None, "bs-swatch-none".to_string(), tr("Keine"));
        for preset in style.offered_colors() {
            add(&self.colors, Choice::Color(preset.slug.clone()), format!("bs-swatch-c-{}", css_ident(&preset.slug)), preset.name.clone());
        }
        let gradients: Vec<_> = style.offered_gradients().collect();
        for preset in &gradients {
            add(&self.gradients, Choice::Gradient(preset.slug.clone()), format!("bs-swatch-g-{}", css_ident(&preset.slug)), preset.name.clone());
        }
        let show_gradients = with_gradients && !gradients.is_empty();
        self.gradients.set_visible(show_gradients);
        self.gradients_label.set_visible(show_gradients);
    }
}

fn flow_box() -> gtk4::FlowBox {
    gtk4::FlowBox::builder().selection_mode(gtk4::SelectionMode::None).max_children_per_line(8).min_children_per_line(4).column_spacing(6).row_spacing(6).homogeneous(true).build()
}

fn css_ident(slug: &str) -> String {
    slug.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}

fn css_value(value: &str) -> String {
    value.chars().filter(|c| !matches!(c, ';' | '{' | '}')).collect()
}

/// One CSS rule per preset, for the swatches.
fn swatch_css(style: &ThemeStyle) -> String {
    let mut css = String::from(
        ".bs-swatch { min-width: 20px; min-height: 20px; padding: 0; border-radius: 9999px; box-shadow: inset 0 0 0 1px alpha(currentColor, .25); }\n\
         button.bs-swatch { min-width: 28px; min-height: 28px; }\n\
         .bs-swatch-none { background: linear-gradient(135deg, transparent 46%, alpha(currentColor, .6) 46%, alpha(currentColor, .6) 54%, transparent 54%); }\n",
    );
    for preset in &style.colors {
        css.push_str(&format!(".bs-swatch.bs-swatch-c-{} {{ background: {}; }}\n", css_ident(&preset.slug), css_value(&preset.value)));
    }
    for preset in &style.gradients {
        css.push_str(&format!(".bs-swatch.bs-swatch-g-{} {{ background: {}; }}\n", css_ident(&preset.slug), css_value(&preset.value)));
    }
    css
}

pub struct BlockInspector {
    pub widget: adw::PreferencesGroup,
    buffer: sourceview5::Buffer,
    hint_row: adw::ActionRow,
    color: SwatchRow,
    background: SwatchRow,
    size_row: adw::ComboRow,
    align_row: adw::ComboRow,
    style_row: adw::ComboRow,
    size_values: RefCell<Vec<Option<String>>>,
    align_values: RefCell<Vec<Option<String>>>,
    style_values: RefCell<Vec<Option<String>>>,
    current: RefCell<Option<gutenberg::BlockAtCursor>>,
    /// The block kind the rows were last set up for.
    shown_kind: RefCell<Option<String>>,
    updating: Cell<bool>,
    pending: RefCell<Option<glib::SourceId>>,
    provider: gtk4::CssProvider,
    weak: Weak<BlockInspector>,
}

impl BlockInspector {
    pub fn new(buffer: &sourceview5::Buffer) -> Rc<BlockInspector> {
        let widget = adw::PreferencesGroup::builder().title(tr("Block")).build();
        let hint_row = adw::ActionRow::builder().title(tr("Kein Block ausgewählt")).subtitle(tr("Den Cursor in einen Absatz, eine Überschrift, Tabelle oder einen Container setzen.")).build();
        hint_row.add_css_class("dim-label");
        let color = SwatchRow::new(&tr("Textfarbe"));
        let background = SwatchRow::new(&tr("Hintergrund"));
        let size_row = adw::ComboRow::builder().title(tr("Schriftgröße")).build();
        let align_row = adw::ComboRow::builder().title(tr("Ausrichtung")).build();
        let style_row = adw::ComboRow::builder().title(tr("Stil")).build();
        widget.add(&hint_row);
        widget.add(&style_row);
        widget.add(&color.row);
        widget.add(&background.row);
        widget.add(&size_row);
        widget.add(&align_row);

        let provider = gtk4::CssProvider::new();
        if let Some(display) = gtk4::gdk::Display::default() {
            gtk4::style_context_add_provider_for_display(&display, &provider, gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }

        let this = Rc::new_cyclic(|weak| BlockInspector {
            widget,
            buffer: buffer.clone(),
            hint_row,
            color,
            background,
            size_row,
            align_row,
            style_row,
            size_values: RefCell::new(Vec::new()),
            align_values: RefCell::new(Vec::new()),
            style_values: RefCell::new(Vec::new()),
            current: RefCell::new(None),
            shown_kind: RefCell::new(None),
            updating: Cell::new(false),
            pending: RefCell::new(None),
            provider,
            weak: weak.clone(),
        });
        this.load_presets();
        {
            let weak = this.weak.clone();
            themestyle::connect_changed(move || {
                if let Some(this) = weak.upgrade() {
                    this.load_presets();
                    this.shown_kind.replace(None);
                    this.update();
                }
            });
        }
        // Each row changes only its own attribute - a value the row can't
        // show (typed by hand) is left alone by the others.
        for field in [Field::Size, Field::Align, Field::Style] {
            let weak = this.weak.clone();
            this.row_for(field).connect_selected_notify(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.apply_row(field);
                }
            });
        }
        {
            let weak = this.weak.clone();
            buffer.connect_cursor_position_notify(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.schedule_update();
                }
            });
        }
        {
            let weak = this.weak.clone();
            buffer.connect_changed(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.schedule_update();
                }
            });
        }
        this.update();
        this
    }

    fn load_presets(&self) {
        let style = themestyle::current();
        self.provider.load_from_string(&swatch_css(&style));
        let weak = self.weak.clone();
        self.color.populate(
            &style,
            false,
            Rc::new(move |choice| {
                if let Some(this) = weak.upgrade() {
                    this.pick_color(choice);
                }
            }),
        );
        let weak = self.weak.clone();
        self.background.populate(
            &style,
            true,
            Rc::new(move |choice| {
                if let Some(this) = weak.upgrade() {
                    this.pick_background(choice);
                }
            }),
        );
    }

    fn schedule_update(&self) {
        if self.updating.get() {
            return;
        }
        if let Some(id) = self.pending.take() {
            id.remove();
        }
        let weak = self.weak.clone();
        let id = glib::timeout_add_local_once(Duration::from_millis(150), move || {
            if let Some(this) = weak.upgrade() {
                this.pending.replace(None);
                this.update();
            }
        });
        self.pending.replace(Some(id));
    }

    fn text_and_offset(&self) -> (String, usize) {
        let text = self.buffer.text(&self.buffer.start_iter(), &self.buffer.end_iter(), true).to_string();
        let char_offset = self.buffer.iter_at_offset(self.buffer.cursor_position()).offset() as usize;
        let byte_offset = text.char_indices().nth(char_offset).map_or(text.len(), |(i, _)| i);
        (text, byte_offset)
    }

    /// Re-reads the block at the cursor and shows its attributes.
    fn update(&self) {
        let (text, offset) = self.text_and_offset();
        let block = gutenberg::block_at(&text, offset);
        self.updating.set(true);
        let style = themestyle::current();
        match &block {
            None => self.show_hint(&tr("Kein Block ausgewählt"), &tr("Den Cursor in einen Absatz, eine Überschrift, Tabelle oder einen Container setzen.")),
            Some(found) if found.name.is_none() => {
                let name = found.verbatim_name.clone().unwrap_or_default();
                self.show_hint(&tr("Unverändert übernommener Block"), &tr("„{name}“ ist im Text als WordPress-Markup enthalten und lässt sich hier nicht gestalten.").replace("{name}", &name));
            }
            Some(found) => {
                let name = found.name.clone().unwrap_or_default();
                let sup = supports(&name);
                let styles = style.styles_for(&style_block(&name));
                self.widget.set_description(Some(&block_label(&name)));
                if self.shown_kind.borrow().as_deref() != Some(name.as_str()) {
                    self.setup_rows(&name, sup, &style);
                    self.shown_kind.replace(Some(name.clone()));
                }
                let any = sup.color || sup.background || sup.size || sup.text_align || sup.block_align || !styles.is_empty();
                self.hint_row.set_visible(!any);
                if !any {
                    self.hint_row.set_title(&tr("Keine Gestaltung"));
                    self.hint_row.set_subtitle(&tr("Für diesen Block bietet das Theme keine Einstellungen an."));
                }
                let attrs = &found.attrs;
                self.color.show(&attrs.text_color.clone().map_or(Choice::None, Choice::Color));
                self.background.show(&match (&attrs.gradient, &attrs.background) {
                    (Some(g), _) => Choice::Gradient(g.clone()),
                    (None, Some(c)) => Choice::Color(c.clone()),
                    _ => Choice::None,
                });
                select(&self.size_row, &self.size_values.borrow(), attrs.font_size.as_deref());
                select(&self.align_row, &self.align_values.borrow(), attrs.align.as_deref());
                select(&self.style_row, &self.style_values.borrow(), attrs.style.as_deref());
            }
        }
        self.current.replace(block);
        self.updating.set(false);
    }

    fn show_hint(&self, title: &str, subtitle: &str) {
        self.widget.set_description(None);
        self.hint_row.set_title(title);
        self.hint_row.set_subtitle(subtitle);
        self.hint_row.set_visible(true);
        for row in [self.color.row.upcast_ref::<gtk4::Widget>(), self.background.row.upcast_ref(), self.size_row.upcast_ref(), self.align_row.upcast_ref(), self.style_row.upcast_ref()] {
            row.set_visible(false);
        }
        self.shown_kind.replace(None);
    }

    /// Which rows a block kind gets, and the choices in them.
    fn setup_rows(&self, name: &str, sup: Supports, style: &ThemeStyle) {
        self.color.row.set_visible(sup.color);
        self.background.row.set_visible(sup.background);
        self.background.gradients.set_visible(sup.gradient && style.offered_gradients().next().is_some());
        self.background.gradients_label.set_visible(self.background.gradients.get_visible());

        let mut sizes = vec![(tr("Standard"), None)];
        sizes.extend(style.font_sizes.iter().filter(|p| p.from_theme).map(|p| (p.name.clone(), Some(p.slug.clone()))));
        fill(&self.size_row, &self.size_values, sizes);
        self.size_row.set_visible(sup.size && style.font_sizes.iter().any(|p| p.from_theme));

        let aligns: Vec<(String, Option<String>)> = if sup.text_align {
            vec![(tr("Standard"), None), (tr("Links"), Some("left".into())), (tr("Zentriert"), Some("center".into())), (tr("Rechts"), Some("right".into()))]
        } else {
            vec![(tr("Standard"), None), (tr("Weite Breite"), Some("wide".into())), (tr("Volle Breite"), Some("full".into())), (tr("Links"), Some("left".into())), (tr("Zentriert"), Some("center".into())), (tr("Rechts"), Some("right".into()))]
        };
        fill(&self.align_row, &self.align_values, aligns);
        self.align_row.set_visible(sup.text_align || sup.block_align);

        let styles = style.styles_for(&style_block(name));
        let mut style_choices = vec![(tr("Standard"), None)];
        style_choices.extend(styles.iter().map(|s| (if s.label.is_empty() { s.name.clone() } else { s.label.clone() }, Some(s.name.clone()))));
        fill(&self.style_row, &self.style_values, style_choices);
        self.style_row.set_visible(!styles.is_empty());
    }

    fn pick_color(&self, choice: Choice) {
        self.edit(|attrs| {
            attrs.text_color = match choice {
                Choice::Color(slug) => Some(slug),
                _ => None,
            };
        });
    }

    fn pick_background(&self, choice: Choice) {
        self.edit(|attrs| {
            attrs.background = None;
            attrs.gradient = None;
            match choice {
                Choice::Color(slug) => attrs.background = Some(slug),
                Choice::Gradient(slug) => attrs.gradient = Some(slug),
                Choice::None => {}
            }
        });
    }

    fn row_for(&self, field: Field) -> &adw::ComboRow {
        match field {
            Field::Size => &self.size_row,
            Field::Align => &self.align_row,
            Field::Style => &self.style_row,
        }
    }

    fn apply_row(&self, field: Field) {
        if self.updating.get() {
            return;
        }
        let values = match field {
            Field::Size => &self.size_values,
            Field::Align => &self.align_values,
            Field::Style => &self.style_values,
        };
        let value = selected(self.row_for(field), &values.borrow());
        self.edit(|attrs| match field {
            Field::Size => attrs.font_size = value,
            Field::Align => attrs.align = value,
            Field::Style => attrs.style = value,
        });
    }

    /// Changes the current block's attributes as one undoable edit.
    fn edit(&self, change: impl FnOnce(&mut gutenberg::BlockAttrs)) {
        // The block may have moved since the last update (typing).
        let (text, offset) = self.text_and_offset();
        let Some(block) = gutenberg::block_at(&text, offset) else { return };
        let mut attrs = block.attrs.clone();
        change(&mut attrs);
        if attrs == block.attrs {
            return;
        }
        let Some((range, replacement)) = gutenberg::attrs_edit(&text, &block, &attrs) else { return };
        let char_at = |byte: usize| text[..byte].chars().count() as i32;
        let cursor = self.buffer.cursor_position();
        self.updating.set(true);
        self.buffer.begin_user_action();
        let mut start = self.buffer.iter_at_offset(char_at(range.start));
        let mut end = self.buffer.iter_at_offset(char_at(range.end));
        self.buffer.delete(&mut start, &mut end);
        let mut at = self.buffer.iter_at_offset(char_at(range.start));
        self.buffer.insert(&mut at, &replacement);
        self.buffer.end_user_action();
        // Keep the cursor where it was when the edit lies behind it.
        if char_at(range.start) >= cursor {
            self.buffer.place_cursor(&self.buffer.iter_at_offset(cursor));
        }
        self.updating.set(false);
        self.update();
    }
}

fn fill(row: &adw::ComboRow, values: &RefCell<Vec<Option<String>>>, choices: Vec<(String, Option<String>)>) {
    let labels: Vec<&str> = choices.iter().map(|(label, _)| label.as_str()).collect();
    row.set_model(Some(&gtk4::StringList::new(&labels)));
    values.replace(choices.into_iter().map(|(_, value)| value).collect());
}

fn select(row: &adw::ComboRow, values: &[Option<String>], value: Option<&str>) {
    let index = values.iter().position(|v| v.as_deref() == value).unwrap_or(0);
    if row.selected() != index as u32 {
        row.set_selected(index as u32);
    }
}

fn selected(row: &adw::ComboRow, values: &[Option<String>]) -> Option<String> {
    values.get(row.selected() as usize).cloned().flatten()
}
