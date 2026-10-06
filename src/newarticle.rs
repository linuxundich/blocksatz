//! The "Neuer Artikel" dialog (Ctrl+N): title and slug, the text to start
//! from - empty, or a Markdown/text file whose header is taken over into
//! the frontmatter (`textimport.rs`) - and images to put into the article
//! folder right away, by picker or drag and drop. The library folder is
//! created once, with its final name, when the dialog is confirmed
//! (`library::create_prepared`). See `docs/new-article-dialog.md`.
//!
//! Confirming with neither title, text nor images starts an untitled
//! article as before: Ctrl+N, Enter.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk4::{gdk, gio, glib};

use crate::document::{self, Document, PostType};
use crate::i18n::tr;
use crate::library;
use crate::textimport::{self, Field, ImageRef, Import, Target};
use crate::window::{self, DocContext};

const TEXT_SUFFIXES: &[&str] = &["md", "markdown", "mdown", "txt", "text"];
const IMAGE_SUFFIXES: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "svg", "avif"];

fn has_suffix(path: &Path, suffixes: &[&str]) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| suffixes.contains(&e.to_lowercase().as_str()))
}

pub fn is_text_file(path: &Path) -> bool {
    has_suffix(path, TEXT_SUFFIXES)
}

pub fn is_image_file(path: &Path) -> bool {
    has_suffix(path, IMAGE_SUFFIXES)
}

#[derive(Default)]
struct State {
    source: Option<PathBuf>,
    /// The source file, decoded.
    text: String,
    import: Option<Import>,
    /// Local images the text points to.
    text_images: Vec<ImageRef>,
    /// Images added by picker or drop.
    extra_images: Vec<PathBuf>,
    /// The image starred as featured image.
    featured: Option<PathBuf>,
    /// Title and slug as last filled in from the source - replaced on a
    /// re-import only while the user hasn't changed them.
    imported_title: String,
    imported_slug: String,
}

struct NewArticle {
    ctx: DocContext,
    state: RefCell<State>,
    /// The slug was typed by hand and no longer follows the title.
    slug_edited: Cell<bool>,
    /// Set while the dialog itself changes title/slug/checks.
    updating: Cell<bool>,
    dialog: adw::Dialog,
    type_toggles: adw::ToggleGroup,
    title_row: adw::EntryRow,
    slug_row: adw::EntryRow,
    folder_row: adw::ActionRow,
    blank_check: gtk4::CheckButton,
    file_check: gtk4::CheckButton,
    file_row: adw::ActionRow,
    file_button: gtk4::Button,
    header_row: adw::ExpanderRow,
    header_children: RefCell<Vec<adw::ActionRow>>,
    heading_row: adw::SwitchRow,
    unlinked_row: adw::ActionRow,
    images_box: gtk4::FlowBox,
    images_hint: gtk4::Label,
    missing_row: adw::ActionRow,
    append_row: adw::SwitchRow,
}

/// Opens the dialog over `parent`. `files` pre-fills it: the first text
/// file becomes the source, images are added.
pub fn present(parent: &impl IsA<gtk4::Widget>, ctx: &DocContext, post_type: PostType, files: Vec<PathBuf>) {
    let this = build(ctx, post_type);
    // The widgets' handlers only hold weak references; the dialog keeps
    // its state alive until it closes.
    let keep = RefCell::new(Some(this.clone()));
    this.dialog.connect_closed(move |_| drop(keep.borrow_mut().take()));
    this.add_files(files);
    this.dialog.present(Some(parent));
    this.title_row.grab_focus();
}

/// Asks for a text file first, then opens the dialog with it.
pub fn present_from_file(window: &adw::ApplicationWindow, ctx: &DocContext) {
    let ctx = ctx.clone();
    let parent = window.clone();
    text_file_dialog().open(Some(window), gio::Cancellable::NONE, move |result| {
        let Some(path) = result.ok().and_then(|f| f.path()) else { return };
        present(&parent, &ctx, PostType::Post, vec![path]);
    });
}

fn text_file_dialog() -> gtk4::FileDialog {
    let filter = gtk4::FileFilter::new();
    for suffix in TEXT_SUFFIXES {
        filter.add_suffix(suffix);
    }
    filter.set_name(Some(&tr("Markdown und Text")));
    let filters = gio::ListStore::new::<gtk4::FileFilter>();
    filters.append(&filter);
    gtk4::FileDialog::builder().title(tr("Text übernehmen")).filters(&filters).build()
}

fn dialog_title(post_type: PostType) -> String {
    match post_type {
        PostType::Post => tr("Neuer Artikel"),
        PostType::Page => tr("Neue Seite"),
    }
}

fn build(ctx: &DocContext, post_type: PostType) -> Rc<NewArticle> {
    // Titel & Adresse
    let type_toggles = adw::ToggleGroup::builder().valign(gtk4::Align::Center).build();
    type_toggles.add(adw::Toggle::builder().name("post").label(tr("Beitrag")).build());
    type_toggles.add(adw::Toggle::builder().name("page").label(tr("Seite")).build());
    type_toggles.set_active_name(Some(post_type.as_str()));

    let title_row = adw::EntryRow::builder().title(tr("Titel")).build();
    let slug_row = adw::EntryRow::builder().title(tr("Slug")).build();
    let slug_button = gtk4::Button::builder().icon_name("view-refresh-symbolic").tooltip_text(tr("Aus dem Titel erzeugen")).valign(gtk4::Align::Center).css_classes(["flat"]).build();
    slug_row.add_suffix(&slug_button);
    let folder_row = adw::ActionRow::builder().title(tr("Ordner")).css_classes(["property"]).subtitle_selectable(true).build();
    let address_group = adw::PreferencesGroup::builder().title(tr("Titel und Adresse")).header_suffix(&type_toggles).build();
    address_group.add(&title_row);
    address_group.add(&slug_row);
    address_group.add(&folder_row);

    // Text
    let blank_check = gtk4::CheckButton::builder().active(true).valign(gtk4::Align::Center).build();
    let file_check = gtk4::CheckButton::builder().group(&blank_check).valign(gtk4::Align::Center).build();
    let blank_row = adw::ActionRow::builder().title(tr("Leer beginnen")).activatable_widget(&blank_check).build();
    blank_row.add_prefix(&blank_check);
    let file_button = gtk4::Button::builder().label(tr("Auswählen …")).valign(gtk4::Align::Center).css_classes(["flat"]).build();
    let file_row = adw::ActionRow::builder()
        .title(tr("Aus Datei …"))
        .subtitle(tr("Markdown oder Text, auch mit Kopfzeile (YAML, TOML, MultiMarkdown) – oder hierher ziehen"))
        .activatable_widget(&file_check)
        .build();
    file_row.add_prefix(&file_check);
    file_row.add_suffix(&file_button);
    let header_row = adw::ExpanderRow::builder().title(tr("Aus der Kopfzeile")).visible(false).build();
    let heading_row = adw::SwitchRow::builder()
        .title(tr("Erste Überschrift als Titel"))
        .subtitle(tr("Die Überschrift wird zum Titel und aus dem Text entfernt."))
        .active(true)
        .visible(false)
        .build();
    let unlinked_row = adw::ActionRow::builder()
        .title(tr("Als neuer Artikel – ohne Verbindung zum Blogbeitrag"))
        .subtitle(tr("Beitrags-ID und Abgleichsdaten werden nicht übernommen; der erste Upload legt einen neuen Beitrag an."))
        .visible(false)
        .build();
    unlinked_row.add_prefix(&gtk4::Image::from_icon_name("dialog-information-symbolic"));
    let text_group = adw::PreferencesGroup::builder().title(tr("Text")).build();
    text_group.add(&blank_row);
    text_group.add(&file_row);
    text_group.add(&header_row);
    text_group.add(&heading_row);
    text_group.add(&unlinked_row);

    // Bilder
    let add_images_button = gtk4::Button::builder().label(tr("Hinzufügen …")).css_classes(["flat"]).valign(gtk4::Align::Center).build();
    let images_box = gtk4::FlowBox::builder()
        .selection_mode(gtk4::SelectionMode::None)
        .homogeneous(true)
        .min_children_per_line(3)
        .max_children_per_line(4)
        .column_spacing(10)
        .row_spacing(10)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .visible(false)
        .build();
    let images_hint = gtk4::Label::builder().label(tr("Bilder hierher ziehen oder „Hinzufügen …“ wählen")).css_classes(["dim-label"]).margin_top(24).margin_bottom(24).margin_start(12).margin_end(12).wrap(true).build();
    let images_card = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).css_classes(["card"]).build();
    images_card.append(&images_box);
    images_card.append(&images_hint);
    let images_group = adw::PreferencesGroup::builder().title(tr("Bilder")).description(tr("Werden in den Artikelordner kopiert; die Originale bleiben, wo sie sind.")).header_suffix(&add_images_button).build();
    images_group.add(&images_card);
    let missing_row = adw::ActionRow::builder().title(tr("Bilder im Text nicht gefunden")).visible(false).build();
    missing_row.add_prefix(&gtk4::Image::from_icon_name("dialog-warning-symbolic"));
    let append_row = adw::SwitchRow::builder().title(tr("Am Ende des Textes einfügen")).subtitle(tr("Sonst liegen hinzugefügte Bilder nur im Ordner und im Medienbereich bereit.")).build();
    let image_options = adw::PreferencesGroup::new();
    image_options.add(&missing_row);
    image_options.add(&append_row);

    let page = adw::PreferencesPage::new();
    page.add(&address_group);
    page.add(&text_group);
    page.add(&images_group);
    page.add(&image_options);

    let cancel_button = gtk4::Button::with_label(&tr("Abbrechen"));
    let create_button = gtk4::Button::builder().label(tr("Anlegen")).css_classes(["suggested-action"]).build();
    let header = adw::HeaderBar::builder().show_start_title_buttons(false).show_end_title_buttons(false).build();
    header.pack_start(&cancel_button);
    header.pack_end(&create_button);
    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&page));

    let dialog = adw::Dialog::builder().title(dialog_title(post_type)).content_width(560).content_height(720).child(&toolbar_view).build();

    let this = Rc::new(NewArticle {
        ctx: ctx.clone(),
        state: RefCell::new(State::default()),
        slug_edited: Cell::new(false),
        updating: Cell::new(false),
        dialog: dialog.clone(),
        type_toggles: type_toggles.clone(),
        title_row: title_row.clone(),
        slug_row: slug_row.clone(),
        folder_row,
        blank_check: blank_check.clone(),
        file_check: file_check.clone(),
        file_row,
        file_button: file_button.clone(),
        header_row,
        header_children: RefCell::new(Vec::new()),
        heading_row: heading_row.clone(),
        unlinked_row,
        images_box,
        images_hint,
        missing_row,
        append_row,
    });
    this.update_folder();

    let weak = Rc::downgrade(&this);
    let with = move |f: &dyn Fn(&Rc<NewArticle>)| {
        if let Some(this) = weak.upgrade() {
            f(&this);
        }
    };
    let with = Rc::new(with);

    {
        let with = with.clone();
        type_toggles.connect_active_name_notify(move |_| with(&|this| this.dialog.set_title(&dialog_title(this.post_type()))));
    }
    {
        let with = with.clone();
        title_row.connect_changed(move |_| with(&|this| this.title_changed()));
    }
    {
        let with = with.clone();
        slug_row.connect_changed(move |_| with(&|this| this.slug_changed()));
    }
    {
        let with = with.clone();
        slug_button.connect_clicked(move |_| {
            with(&|this| {
                this.slug_edited.set(false);
                this.title_changed();
            })
        });
    }
    for row in [&title_row, &slug_row] {
        let with = with.clone();
        row.connect_entry_activated(move |_| with(&|this| this.create()));
    }
    {
        let with = with.clone();
        blank_check.connect_toggled(move |check| {
            if check.is_active() {
                with(&|this| this.clear_source());
            }
        });
    }
    {
        let with = with.clone();
        file_check.connect_toggled(move |check| {
            if check.is_active() {
                with(&|this| {
                    if !this.updating.get() && this.state.borrow().source.is_none() {
                        this.set_blank_checked();
                        this.choose_text_file();
                    }
                })
            }
        });
    }
    {
        let with = with.clone();
        file_button.connect_clicked(move |_| with(&|this| this.choose_text_file()));
    }
    {
        let with = with.clone();
        heading_row.connect_active_notify(move |_| with(&|this| this.reimport()));
    }
    {
        let with = with.clone();
        add_images_button.connect_clicked(move |_| with(&|this| this.choose_images()));
    }
    {
        let with = with.clone();
        create_button.connect_clicked(move |_| with(&|this| this.create()));
    }
    {
        let dialog = dialog.clone();
        cancel_button.connect_clicked(move |_| {
            dialog.close();
        });
    }

    let drop_target = gtk4::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    {
        let with = with.clone();
        drop_target.connect_drop(move |_, value, _, _| {
            let Ok(list) = value.get::<gdk::FileList>() else { return false };
            let paths: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
            let accepted = paths.iter().any(|p| is_text_file(p) || is_image_file(p));
            with(&|this| this.add_files(paths.clone()));
            accepted
        });
    }
    toolbar_view.add_controller(drop_target);

    this
}

impl NewArticle {
    fn post_type(&self) -> PostType {
        match self.type_toggles.active_name().as_deref() {
            Some("page") => PostType::Page,
            _ => PostType::Post,
        }
    }

    fn parent_window(&self) -> Option<gtk4::Window> {
        self.dialog.root().and_downcast::<gtk4::Window>()
    }

    /// Sets a row's text without it counting as the user's edit.
    fn set_quietly(&self, row: &adw::EntryRow, text: &str) {
        if row.text() != text {
            self.updating.set(true);
            row.set_text(text);
            self.updating.set(false);
        }
    }

    fn title_changed(&self) {
        if !self.slug_edited.get() {
            let slug = document::slugify(&self.title_row.text());
            self.set_quietly(&self.slug_row, &slug);
        }
        self.update_folder();
    }

    fn slug_changed(&self) {
        if !self.updating.get() {
            // An emptied slug follows the title again.
            self.slug_edited.set(!self.slug_row.text().trim().is_empty());
        }
        self.update_folder();
    }

    /// The folder name the article will get, as shown.
    fn folder_name(&self) -> String {
        let slug = document::slugify(self.slug_row.text().trim());
        if !slug.is_empty() {
            return slug;
        }
        let title = document::slugify(&self.title_row.text());
        if !title.is_empty() {
            return title;
        }
        library::untitled_name()
    }

    fn update_folder(&self) {
        let root = library::root();
        let dir = library::unique_dir(&root, &self.folder_name());
        let shown = dir.strip_prefix(glib::home_dir()).map(|p| format!("~/{}/", p.display())).unwrap_or_else(|_| format!("{}/", dir.display()));
        self.folder_row.set_subtitle(&glib::markup_escape_text(&shown));
    }

    fn set_blank_checked(&self) {
        self.updating.set(true);
        self.blank_check.set_active(true);
        self.updating.set(false);
    }

    fn choose_text_file(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        text_file_dialog().open(self.parent_window().as_ref(), gio::Cancellable::NONE, move |result| {
            let (Some(this), Some(path)) = (weak.upgrade(), result.ok().and_then(|f| f.path())) else { return };
            this.load_source(&path);
        });
    }

    fn choose_images(self: &Rc<Self>) {
        let filter = gtk4::FileFilter::new();
        filter.add_mime_type("image/*");
        filter.set_name(Some(&tr("Bilder")));
        let filters = gio::ListStore::new::<gtk4::FileFilter>();
        filters.append(&filter);
        let file_dialog = gtk4::FileDialog::builder().title(tr("Bilder hinzufügen")).filters(&filters).build();
        let weak = Rc::downgrade(self);
        file_dialog.open_multiple(self.parent_window().as_ref(), gio::Cancellable::NONE, move |result| {
            let (Some(this), Ok(files)) = (weak.upgrade(), result) else { return };
            let paths = files.iter::<gio::File>().filter_map(Result::ok).filter_map(|f| f.path()).collect();
            this.add_files(paths);
        });
    }

    /// Files from a drop or the entry point: the first text file becomes
    /// the source, images are added (each once).
    fn add_files(self: &Rc<Self>, paths: Vec<PathBuf>) {
        if let Some(text) = paths.iter().find(|p| is_text_file(p)) {
            self.load_source(text);
        }
        let mut added = false;
        {
            let mut state = self.state.borrow_mut();
            for path in paths.into_iter().filter(|p| is_image_file(p)) {
                if !state.extra_images.contains(&path) && !state.text_images.iter().any(|i| i.path == path) {
                    state.extra_images.push(path);
                    added = true;
                }
            }
        }
        if added {
            self.refresh_images();
        }
    }

    fn load_source(self: &Rc<Self>, path: &Path) {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) => {
                window::show_toast(&self.ctx.toast_overlay, &tr("Datei konnte nicht gelesen werden: {err}").replace("{err}", &err.to_string()));
                return;
            }
        };
        {
            let mut state = self.state.borrow_mut();
            state.source = Some(path.to_path_buf());
            state.text = textimport::decode(&bytes);
            state.featured = None;
        }
        self.updating.set(true);
        self.file_check.set_active(true);
        self.updating.set(false);
        self.file_button.set_label(&tr("Ändern …"));
        self.reimport();
    }

    fn clear_source(self: &Rc<Self>) {
        if self.updating.get() || self.state.borrow().source.is_none() {
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            let (imported_title, imported_slug) = (state.imported_title.clone(), state.imported_slug.clone());
            let extra_images = std::mem::take(&mut state.extra_images);
            *state = State { extra_images, ..State::default() };
            drop(state);
            // Title and slug that came from the file go with it.
            if self.title_row.text() == imported_title {
                self.set_quietly(&self.title_row, "");
            }
            if self.slug_row.text() == imported_slug && !imported_slug.is_empty() {
                self.slug_edited.set(false);
                self.title_changed();
            }
        }
        self.file_button.set_label(&tr("Auswählen …"));
        self.file_row.set_title(&tr("Aus Datei …"));
        self.file_row.set_subtitle(&tr("Markdown oder Text, auch mit Kopfzeile (YAML, TOML, MultiMarkdown) – oder hierher ziehen"));
        self.header_row.set_visible(false);
        self.heading_row.set_visible(false);
        self.unlinked_row.set_visible(false);
        self.refresh_images();
    }

    /// Reads the source again (after loading it, or when the heading
    /// switch changes) and refreshes everything that depends on it.
    fn reimport(self: &Rc<Self>) {
        let Some(source) = self.state.borrow().source.clone() else { return };
        let now = glib::DateTime::now_local().ok().and_then(|n| n.format("%Y-%m-%dT%H:%M:00").ok()).map(|s| s.to_string()).unwrap_or_default();
        let text = self.state.borrow().text.clone();
        let import = textimport::import(&text, &now, self.heading_row.is_active());
        let base = source.parent().map(Path::to_path_buf).unwrap_or_default();
        let text_images = textimport::local_images(&import.doc.body, import.doc.frontmatter.featured_image.as_deref(), &base);
        let fm = &import.doc.frontmatter;

        // Title and slug: replaced unless the user changed them since the
        // last import.
        let (old_title, old_slug) = {
            let state = self.state.borrow();
            (state.imported_title.clone(), state.imported_slug.clone())
        };
        if self.title_row.text().trim().is_empty() || self.title_row.text() == old_title {
            self.set_quietly(&self.title_row, &fm.title);
        }
        if !fm.slug.is_empty() && (!self.slug_edited.get() || self.slug_row.text() == old_slug) {
            self.set_quietly(&self.slug_row, &fm.slug);
            self.slug_edited.set(true);
        } else if fm.slug.is_empty() && self.slug_edited.get() && self.slug_row.text() == old_slug {
            self.slug_edited.set(false);
        }
        self.title_changed();

        // The heading switch only matters when the header has no title and
        // the text starts with a heading.
        let header_title = import.entries.iter().any(|e| e.target == Target::Field(Field::Title));
        let starts_with_heading = import.title_from_heading || document::split_title_heading(&import.doc.body).is_some();
        self.heading_row.set_visible(!header_title && starts_with_heading);
        self.unlinked_row.set_visible(import.format == Some(textimport::Format::Blocksatz));

        let words = import.doc.body.split_whitespace().count();
        let name = source.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let folder = source.parent().map(|p| p.strip_prefix(glib::home_dir()).map(|r| format!("~/{}", r.display())).unwrap_or_else(|_| p.display().to_string())).unwrap_or_default();
        let mut subtitle = format!("{folder} · {}", tr("{n} Wörter").replace("{n}", &words.to_string()));
        if let Some(format) = import.format {
            subtitle.push_str(&format!(" · {}", tr("Kopfzeile {format}").replace("{format}", format.label())));
        }
        self.file_row.set_title(&glib::markup_escape_text(&name));
        self.file_row.set_subtitle(&glib::markup_escape_text(&subtitle));
        self.refresh_header(&import);

        {
            let mut state = self.state.borrow_mut();
            state.imported_title = fm.title.clone();
            state.imported_slug = fm.slug.clone();
            let featured_source = fm.featured_image.clone();
            state.featured = featured_source.and_then(|f| text_images.iter().find(|i| i.source == f && i.found).map(|i| i.path.clone()));
            state.extra_images.retain(|p| !text_images.iter().any(|i| &i.path == p));
            state.text_images = text_images;
            state.import = Some(import);
        }
        self.refresh_images();
    }

    fn refresh_header(&self, import: &Import) {
        for row in self.header_children.borrow_mut().drain(..) {
            self.header_row.remove(&row);
        }
        let entries: Vec<_> = import.entries.iter().filter(|e| !(e.value.is_empty() && e.target == Target::Kept)).collect();
        if entries.is_empty() {
            self.header_row.set_visible(false);
            return;
        }
        let taken = entries.iter().filter(|e| matches!(e.target, Target::Field(_) | Target::Kept)).count();
        let format = import.format.map(textimport::Format::label).unwrap_or_default();
        self.header_row.set_title(&tr("Aus der Kopfzeile ({format})").replace("{format}", format));
        self.header_row.set_subtitle(&tr("{taken} übernommen, {rest} nicht").replace("{taken}", &taken.to_string()).replace("{rest}", &(entries.len() - taken).to_string()));
        for entry in entries {
            let row = adw::ActionRow::builder().title(glib::markup_escape_text(&entry.key)).subtitle(glib::markup_escape_text(&entry.value)).subtitle_lines(2).build();
            let (label, class) = match &entry.target {
                Target::Field(field) => (field_label(*field), "success"),
                Target::Kept => (tr("übernommen"), "success"),
                Target::Unlinked => (tr("entfernt"), "warning"),
                Target::Ignored => (tr("nicht übernommen"), "dim-label"),
            };
            row.add_suffix(&gtk4::Label::builder().label(label).css_classes(["caption", class]).build());
            self.header_row.add_row(&row);
            self.header_children.borrow_mut().push(row);
        }
        self.header_row.set_visible(true);
    }

    fn refresh_images(self: &Rc<Self>) {
        self.images_box.remove_all();
        let state = self.state.borrow();
        let missing: Vec<&ImageRef> = state.text_images.iter().filter(|i| !i.found).collect();
        let found = state.text_images.iter().filter(|i| i.found).map(|i| (i.path.clone(), true));
        let extra = state.extra_images.iter().map(|p| (p.clone(), false));
        let mut count = 0;
        for (path, in_text) in found.chain(extra) {
            let starred = state.featured.as_ref() == Some(&path);
            self.images_box.append(&self.thumbnail(&path, in_text, starred));
            count += 1;
        }
        self.images_box.set_visible(count > 0);
        self.images_hint.set_visible(count == 0);
        self.missing_row.set_visible(!missing.is_empty());
        if !missing.is_empty() {
            let names: Vec<&str> = missing.iter().map(|i| i.source.as_str()).collect();
            let title = if missing.len() == 1 { tr("Ein Bild im Text nicht gefunden") } else { tr("{n} Bilder im Text nicht gefunden").replace("{n}", &missing.len().to_string()) };
            self.missing_row.set_title(&title);
            self.missing_row.set_subtitle(&glib::markup_escape_text(&format!("{} – {}", names.join(", "), tr("Der Verweis bleibt stehen und lässt sich später ersetzen."))));
        }
        self.append_row.set_sensitive(!state.extra_images.is_empty());
    }

    fn thumbnail(self: &Rc<Self>, path: &Path, in_text: bool, starred: bool) -> gtk4::Widget {
        let picture = gtk4::Picture::builder().content_fit(gtk4::ContentFit::Cover).can_shrink(true).width_request(112).height_request(84).build();
        picture.set_filename(Some(path));
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let caption = gtk4::Label::builder().label(&name).ellipsize(gtk4::pango::EllipsizeMode::Middle).css_classes(["caption"]).build();
        let overlay = gtk4::Overlay::builder().child(&picture).overflow(gtk4::Overflow::Hidden).css_classes(["card"]).build();
        overlay.set_tooltip_text(Some(&if in_text { tr("{name} – im Text verwendet") } else { tr("{name} – hinzugefügt") }.replace("{name}", &name)));

        let star = gtk4::ToggleButton::builder()
            .icon_name(if starred { "starred-symbolic" } else { "non-starred-symbolic" })
            .active(starred)
            .tooltip_text(tr("Als Beitragsbild"))
            .halign(gtk4::Align::Start)
            .valign(gtk4::Align::Start)
            .margin_start(4)
            .margin_top(4)
            .css_classes(if starred { vec!["suggested-action", "circular"] } else { vec!["osd", "circular"] })
            .build();
        {
            let weak = Rc::downgrade(self);
            let path = path.to_path_buf();
            star.connect_toggled(move |star| {
                let Some(this) = weak.upgrade() else { return };
                this.state.borrow_mut().featured = star.is_active().then(|| path.clone());
                // Only one star: redraw.
                glib::idle_add_local_once(move || this.refresh_images());
            });
        }
        overlay.add_overlay(&star);

        if !in_text {
            let remove = gtk4::Button::builder()
                .icon_name("window-close-symbolic")
                .tooltip_text(tr("Entfernen"))
                .halign(gtk4::Align::End)
                .valign(gtk4::Align::Start)
                .margin_end(4)
                .margin_top(4)
                .css_classes(["osd", "circular"])
                .build();
            let weak = Rc::downgrade(self);
            let path = path.to_path_buf();
            remove.connect_clicked(move |_| {
                let Some(this) = weak.upgrade() else { return };
                {
                    let mut state = this.state.borrow_mut();
                    state.extra_images.retain(|p| p != &path);
                    if state.featured.as_ref() == Some(&path) {
                        state.featured = None;
                    }
                }
                glib::idle_add_local_once(move || this.refresh_images());
            });
            overlay.add_overlay(&remove);
        }

        let tile = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(4).build();
        tile.append(&overlay);
        tile.append(&caption);
        tile.upcast()
    }

    /// "Anlegen": writes the library folder and opens the article.
    fn create(self: &Rc<Self>) {
        let post_type = self.post_type();
        let title = self.title_row.text().trim().to_string();
        let state = self.state.borrow();
        if title.is_empty() && state.source.is_none() && state.extra_images.is_empty() {
            drop(state);
            self.dialog.close();
            window::start_blank(&self.ctx, post_type);
            return;
        }

        let mut doc = state.import.as_ref().map(|i| i.doc.clone()).unwrap_or_default();
        doc.frontmatter.title = title;
        doc.frontmatter.slug = document::slugify(self.slug_row.text().trim());
        doc.frontmatter.post_type = post_type;
        let name = library::folder_name(&doc).unwrap_or_else(library::untitled_name);
        let images = Images { in_text: state.text_images.clone(), extra: state.extra_images.clone(), featured: state.featured.clone(), append: self.append_row.is_active() };
        drop(state);

        match library::create_prepared(&library::root(), &name, |dir| write_article(dir, doc, &images)) {
            Ok(path) => {
                self.dialog.close();
                window::open_document_at_path(path, &self.ctx);
            }
            Err(err) => window::show_toast(&self.ctx.toast_overlay, &tr("Artikel konnte nicht angelegt werden: {err}").replace("{err}", &err.to_string())),
        }
    }
}

fn field_label(field: Field) -> String {
    match field {
        Field::Title => tr("Titel"),
        Field::Slug => tr("Slug"),
        Field::Tags => tr("Schlagwörter"),
        Field::Categories => tr("Kategorien"),
        Field::Excerpt => tr("Auszug"),
        Field::FeaturedImage => tr("Beitragsbild"),
        Field::FeaturedImageAlt => tr("Alternativtext Beitragsbild"),
        Field::Lang => tr("Sprache"),
        Field::Status => tr("Status"),
        Field::ScheduledAt => tr("Geplant"),
        Field::SeoTitle => tr("SEO-Titel"),
        Field::SeoDescription => tr("SEO-Beschreibung"),
        Field::FocusKeyword => tr("Fokus-Schlüsselwort"),
    }
}

struct Images {
    in_text: Vec<ImageRef>,
    extra: Vec<PathBuf>,
    featured: Option<PathBuf>,
    append: bool,
}

/// A file name that works in a Markdown link target: no spaces.
fn clean_file_name(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "bild".to_string());
    name.split_whitespace().collect::<Vec<_>>().join("-")
}

/// Fills the new article folder `dir`: copies the images, points the text
/// and the frontmatter at the copies, writes `artikel.md`.
fn write_article(dir: &Path, mut doc: Document, images: &Images) -> std::io::Result<()> {
    let mut copied: HashMap<PathBuf, String> = HashMap::new();
    let mut copy = |path: &Path| -> std::io::Result<String> {
        if let Some(name) = copied.get(path) {
            return Ok(name.clone());
        }
        let target = document::unique_file_path(dir, Path::new(&clean_file_name(path)), Path::exists);
        std::fs::copy(path, &target)?;
        let name = target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        copied.insert(path.to_path_buf(), name.clone());
        Ok(name)
    };

    let mut map = HashMap::new();
    for image in images.in_text.iter().filter(|i| i.found) {
        map.insert(image.source.clone(), copy(&image.path)?);
    }
    let mut appended = Vec::new();
    for path in &images.extra {
        let name = copy(path)?;
        if images.append {
            appended.push(name);
        }
    }

    let fm = &mut doc.frontmatter;
    // A local featured image from the header follows the star: kept when
    // starred, dropped when the star was taken off.
    let header_image_local = fm.featured_image.as_ref().is_some_and(|f| images.in_text.iter().any(|i| i.found && &i.source == f));
    if header_image_local {
        fm.featured_image = None;
    }
    if let Some(featured) = &images.featured {
        fm.featured_image = Some(copy(featured)?);
    }
    for item in &mut fm.media {
        if let Some(name) = map.get(&item.source) {
            item.source = name.clone();
            item.filename = name.clone();
        }
    }

    doc.body = textimport::rewrite_sources(&doc.body, &map);
    for name in appended {
        if !doc.body.is_empty() && !doc.body.ends_with("\n\n") {
            doc.body.push_str(if doc.body.ends_with('\n') { "\n" } else { "\n\n" });
        }
        doc.body.push_str(&format!("![]({name})\n"));
    }
    document::write(&dir.join(library::ARTICLE_FILE), &doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_article_with_copied_images() {
        let base = std::env::temp_dir().join(format!("blocksatz-newarticle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let source_dir = base.join("quelle");
        let target = base.join("ziel");
        std::fs::create_dir_all(source_dir.join("img")).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(source_dir.join("img/start bild.png"), b"a").unwrap();
        std::fs::write(source_dir.join("cover.png"), b"c").unwrap();
        std::fs::write(source_dir.join("extra.png"), b"e").unwrap();

        let text = "---\ntitle: T\nimage: cover.png\n---\n![Start](img/start%20bild.png)\n";
        let import = textimport::import(text, "2026-10-06T11:00:00", true);
        let in_text = textimport::local_images(&import.doc.body, import.doc.frontmatter.featured_image.as_deref(), &source_dir);
        let images = Images { featured: Some(source_dir.join("cover.png")), in_text, extra: vec![source_dir.join("extra.png")], append: true };
        write_article(&target, import.doc, &images).unwrap();

        let doc = document::read(&target.join(library::ARTICLE_FILE)).unwrap();
        assert_eq!(doc.frontmatter.featured_image.as_deref(), Some("cover.png"));
        assert_eq!(doc.body, "![Start](start-bild.png)\n\n![](extra.png)\n");
        assert!(target.join("start-bild.png").exists());
        assert!(target.join("extra.png").exists());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
