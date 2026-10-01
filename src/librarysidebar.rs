//! The left navigation sidebar (`docs/gui-redesign.md`, 5.2): an
//! `AdwSidebar` with two sections.
//!
//! - **In Arbeit** - the articles in the library (`library.rs`), most
//!   recently changed first. Each row's subtitle names the WordPress status,
//!   its suffix icon the sync state (a paper plane for changes not uploaded
//!   yet). Published, unchanged working copies drop out after 30 days.
//! - **Im Blog** - the site's status groups with counts; activating one
//!   opens the blog archive page (`blogposts.rs`) for it.
//!
//! The sidebar has its own header bar (new article, search, primary menu)
//! as a navigation sidebar does in the HIG, and a footer naming the
//! connected site.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk4::{gio, glib};

use crate::blogposts::BlogFilter;
use crate::document::{Document, PostStatus};
use crate::i18n::tr;
use crate::syncstate::{self, Remote, SyncState};
use crate::window::{self, DocContext};
use crate::{importer, library, wpclient, wpsite};

pub struct LibrarySidebar {
    pub widget: adw::ToolbarView,
    sidebar: adw::Sidebar,
    work: adw::SidebarSection,
    work_items: RefCell<Vec<(PathBuf, adw::SidebarItem)>>,
    blog_items: Vec<(BlogFilter, adw::SidebarItem)>,
    /// The blog group shown in the content area, if any - selected instead
    /// of the open article while the archive page is up.
    shown_filter: Cell<Option<BlogFilter>>,
    /// The "In Arbeit" row a context menu was opened on.
    menu_target: RefCell<Option<PathBuf>>,
    site_icon: gtk4::Image,
    site_label: gtk4::Label,
    ctx: DocContext,
    weak: Weak<LibrarySidebar>,
}

impl LibrarySidebar {
    /// `primary_menu` goes into the sidebar's header bar; `on_document`
    /// runs after an article was opened from the list (to bring the editor
    /// back to front), `on_filter` when a blog group was activated.
    pub fn new(window: &adw::ApplicationWindow, ctx: &DocContext, primary_menu: &gio::MenuModel, on_document: Rc<dyn Fn()>, on_filter: Rc<dyn Fn(BlogFilter)>) -> Rc<Self> {
        let sidebar = adw::Sidebar::new();

        let work = adw::SidebarSection::new();
        work.set_title(Some(&tr("In Arbeit")));
        let work_menu = gio::Menu::new();
        work_menu.append(Some(&tr("Im Dateimanager zeigen")), Some("library.show-folder"));
        work_menu.append(Some(&tr("Aus Bibliothek entfernen")), Some("library.remove"));
        work.set_menu_model(Some(&work_menu));
        sidebar.append(work.clone());

        let blog = adw::SidebarSection::new();
        blog.set_title(Some(&tr("Im Blog")));
        let blog_items: Vec<(BlogFilter, adw::SidebarItem)> = BlogFilter::ALL
            .iter()
            .map(|filter| {
                let item = adw::SidebarItem::new(&filter.title());
                item.set_icon_name(Some(filter.icon_name()));
                // Shown once the counters say there's something in it.
                item.set_visible(*filter != BlogFilter::Pending);
                blog.append(item.clone());
                (*filter, item)
            })
            .collect();
        sidebar.append(blog);

        // Header: new article (main click) with the other ways to start one
        // in its menu, search, primary menu.
        let new_menu = gio::Menu::new();
        new_menu.append(Some(&tr("Neuer Artikel")), Some("win.new"));
        new_menu.append(Some(&tr("Neue Seite")), Some("win.new-page"));
        new_menu.append(Some(&tr("KI-Artikel schreiben…")), Some("win.ai-write"));
        let open_section = gio::Menu::new();
        open_section.append(Some(&tr("Datei öffnen…")), Some("win.open"));
        new_menu.append_section(None, &open_section);
        let new_button = adw::SplitButton::builder().icon_name("list-add-symbolic").action_name("win.new").menu_model(&new_menu).tooltip_text(tr("Neuer Artikel (Strg+N)")).dropdown_tooltip(tr("Weitere Möglichkeiten")).build();

        let search_button = gtk4::ToggleButton::builder().icon_name("edit-find-symbolic").tooltip_text(tr("Bibliothek durchsuchen")).build();
        let menu_button = gtk4::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(primary_menu).tooltip_text(tr("Hauptmenü")).primary(true).build();
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&adw::WindowTitle::new("Blocksatz", "")));
        header.pack_start(&new_button);
        header.pack_end(&menu_button);
        header.pack_end(&search_button);

        let search_entry = gtk4::SearchEntry::builder().placeholder_text(tr("Artikel suchen")).build();
        let search_bar = gtk4::SearchBar::builder().child(&search_entry).show_close_button(false).build();
        search_bar.connect_entry(&search_entry);
        search_bar.set_key_capture_widget(Some(&sidebar));
        search_button.bind_property("active", &search_bar, "search-mode-enabled").bidirectional().build();
        let filter = gtk4::CustomFilter::new({
            let search_entry = search_entry.clone();
            move |object| {
                let query = search_entry.text().to_lowercase();
                query.is_empty() || object.downcast_ref::<adw::SidebarItem>().and_then(|item| item.title()).is_some_and(|title| title.to_lowercase().contains(&query))
            }
        });
        sidebar.set_filter(Some(&filter));
        search_entry.connect_search_changed({
            let filter = filter.clone();
            move |_| filter.changed(gtk4::FilterChange::Different)
        });
        sidebar.set_placeholder(Some(&adw::StatusPage::builder().icon_name("edit-find-symbolic").title(tr("Keine Treffer")).css_classes(["compact"]).build()));

        let site_icon = gtk4::Image::from_icon_name("network-server-symbolic");
        let site_label = gtk4::Label::builder().xalign(0.0).hexpand(true).ellipsize(gtk4::pango::EllipsizeMode::End).build();
        site_label.add_css_class("dim-label");
        let refresh_button = gtk4::Button::builder().icon_name("view-refresh-symbolic").tooltip_text(tr("Bibliothek und Blog aktualisieren")).build();
        refresh_button.add_css_class("flat");
        let footer = gtk4::Box::builder().spacing(8).margin_start(12).margin_end(6).margin_top(6).margin_bottom(6).build();
        footer.append(&site_icon);
        footer.append(&site_label);
        footer.append(&refresh_button);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.add_top_bar(&search_bar);
        toolbar.set_content(Some(&sidebar));
        toolbar.add_bottom_bar(&footer);

        let this = Rc::new_cyclic(|weak| LibrarySidebar {
            widget: toolbar,
            sidebar: sidebar.clone(),
            work,
            work_items: RefCell::new(Vec::new()),
            blog_items,
            shown_filter: Cell::new(None),
            menu_target: RefCell::new(None),
            site_icon,
            site_label,
            ctx: ctx.clone(),
            weak: weak.clone(),
        });

        {
            let weak = this.weak.clone();
            sidebar.connect_activated(move |sidebar, index| {
                let Some(this) = weak.upgrade() else { return };
                let Some(item) = sidebar.item(index) else { return };
                let path = this.work_items.borrow().iter().find(|(_, i)| *i == item).map(|(p, _)| p.clone());
                if let Some(path) = path {
                    this.shown_filter.set(None);
                    if this.ctx.current_path.borrow().as_deref() != Some(path.as_path()) {
                        window::open_document_at_path(path, &this.ctx);
                    }
                    on_document();
                } else if let Some((filter, _)) = this.blog_items.iter().find(|(_, i)| *i == item) {
                    this.shown_filter.set(Some(*filter));
                    on_filter(*filter);
                }
            });
        }
        {
            let weak = this.weak.clone();
            sidebar.connect_setup_menu(move |_, item| {
                let Some(this) = weak.upgrade() else { return };
                let path = item.and_then(|item| this.work_items.borrow().iter().find(|(_, i)| i == item).map(|(p, _)| p.clone()));
                *this.menu_target.borrow_mut() = path;
            });
        }
        this.install_actions(window);
        {
            let weak = this.weak.clone();
            refresh_button.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.reload();
                    this.refresh_counts();
                    crate::blogsync::refresh(&this.ctx);
                }
            });
        }
        {
            let weak = this.weak.clone();
            // Deferred: a rescan replaces every row, and this can be called
            // from inside the sidebar's own `activated` handler.
            ctx.add_library_listener(Rc::new(move |structural| {
                let weak = weak.clone();
                glib::idle_add_local_once(move || {
                    if let Some(this) = weak.upgrade() {
                        if structural { this.reload() } else { this.update_current() }
                    }
                });
            }));
        }

        {
            let weak = this.weak.clone();
            ctx.blog_listeners.borrow_mut().push(Rc::new(move || {
                if let Some(this) = weak.upgrade() {
                    this.refresh_counts();
                }
            }));
        }

        this.reload();
        this.refresh_counts();
        this
    }

    fn install_actions(&self, window: &adw::ApplicationWindow) {
        let group = gio::SimpleActionGroup::new();

        let show_folder = gio::SimpleAction::new("show-folder", None);
        {
            let weak = self.weak.clone();
            let window = window.downgrade();
            show_folder.connect_activate(move |_, _| {
                let (Some(this), Some(window)) = (weak.upgrade(), window.upgrade()) else { return };
                let Some(path) = this.menu_target.borrow().clone() else { return };
                gtk4::FileLauncher::new(Some(&gio::File::for_path(&path))).open_containing_folder(Some(&window), gio::Cancellable::NONE, |_| {});
            });
        }
        group.add_action(&show_folder);

        let remove = gio::SimpleAction::new("remove", None);
        {
            let weak = self.weak.clone();
            let window = window.downgrade();
            remove.connect_activate(move |_, _| {
                let (Some(this), Some(window)) = (weak.upgrade(), window.upgrade()) else { return };
                let Some(path) = this.menu_target.borrow().clone() else { return };
                this.remove_entry(&window, &path);
            });
        }
        group.add_action(&remove);

        self.sidebar.insert_action_group("library", Some(&group));
    }

    /// Moves an article's library folder to the desktop trash (the blog is
    /// not touched). Closes it first if it's the open one.
    fn remove_entry(&self, window: &adw::ApplicationWindow, path: &Path) {
        let Some(dir) = path.parent() else { return };
        if self.ctx.current_path.borrow().as_deref() == Some(path) {
            let _ = WidgetExt::activate_action(window, "win.new", None);
        }
        let name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        match gio::File::for_path(dir).trash(gio::Cancellable::NONE) {
            Ok(()) => window::show_toast(&self.ctx.toast_overlay, &tr("„{name}“ in den Papierkorb verschoben.").replace("{name}", &name)),
            Err(err) => window::show_toast(&self.ctx.toast_overlay, &tr("Entfernen fehlgeschlagen: {err}").replace("{err}", &err.to_string())),
        }
        self.reload();
    }

    /// Rebuilds "In Arbeit" from disk.
    pub fn reload(&self) {
        let root = library::root();
        let current = self.ctx.current_path.borrow().clone();
        let now = glib::DateTime::now_utc().ok();
        let mut entries: Vec<(library::Entry, std::time::SystemTime)> = library::scan(&root)
            .into_iter()
            .filter(|entry| Some(&entry.path) == current.as_ref() || !now.as_ref().is_some_and(|now| library::is_retired(&entry.document, now)))
            .map(|entry| {
                let modified = std::fs::metadata(&entry.path).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                (entry, modified)
            })
            .collect();
        entries.sort_by_key(|(_, modified)| std::cmp::Reverse(*modified));

        self.work.remove_all();
        let mut items = Vec::new();
        for (entry, _) in entries {
            // The open article's row reflects the editor, not the file,
            // which can be up to one save interval behind.
            let document = if Some(&entry.path) == current.as_ref() { self.ctx.current_document() } else { entry.document };
            let item = adw::SidebarItem::new("");
            apply(&item, &entry.path, &document, &self.ctx.remote_for(&document.frontmatter));
            self.work.append(item.clone());
            items.push((entry.path, item));
        }
        if items.is_empty() {
            let placeholder = adw::SidebarItem::new(&tr("Noch keine Artikel"));
            placeholder.set_enabled(false);
            self.work.append(placeholder);
        }
        *self.work_items.borrow_mut() = items;
        self.sync_selection();
    }

    /// Updates the open article's row after an edit or upload.
    fn update_current(&self) {
        let Some(path) = self.ctx.current_path.borrow().clone() else { return };
        let item = self.work_items.borrow().iter().find(|(p, _)| *p == path).map(|(_, i)| i.clone());
        match item {
            Some(item) => {
                let document = self.ctx.current_document();
                apply(&item, &path, &document, &self.ctx.remote_for(&document.frontmatter));
            }
            None if library::contains(&library::root(), &path) => self.reload(),
            None => {}
        }
    }

    /// Back to the editor: the open article is the selected row again.
    pub fn show_document(&self) {
        self.shown_filter.set(None);
        self.sync_selection();
    }

    /// Selects `filter`'s row (archive page opened another way, Ctrl+Shift+O).
    pub fn show_filter(&self, filter: BlogFilter) {
        self.shown_filter.set(Some(filter));
        self.sync_selection();
    }

    fn sync_selection(&self) {
        let selected = match self.shown_filter.get() {
            Some(filter) => self.blog_items.iter().find(|(f, _)| *f == filter).map(|(_, item)| item.clone()),
            None => {
                let current = self.ctx.current_path.borrow().clone();
                self.work_items.borrow().iter().find(|(p, _)| Some(p) == current.as_ref()).map(|(_, item)| item.clone())
            }
        };
        let index = selected.map(|item| item.index()).unwrap_or(gtk4::INVALID_LIST_POSITION);
        self.sidebar.set_selected(index);
    }

    /// Fetches the "Im Blog" counters and updates the footer.
    pub fn refresh_counts(&self) {
        let site = wpsite::load();
        if site.url.is_empty() {
            self.set_site_state("network-offline-symbolic", &tr("Kein Blog verbunden"));
            return;
        }
        self.set_site_state("network-server-symbolic", &site.site_id());
        let weak = self.weak.clone();
        importer::run_with_password(
            &site,
            |site, password| {
                let client = wpclient::Client::new(&site.url, &site.username, password);
                BlogFilter::ALL
                    .iter()
                    .map(|filter| client.count_items(filter.post_type().rest_base(), filter.statuses()).map(|n| (*filter, n)))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|err| err.to_string())
            },
            move |outcome| {
                let Some(this) = weak.upgrade() else { return };
                match outcome {
                    Ok(counts) => {
                        for (filter, count) in counts {
                            let Some((_, item)) = this.blog_items.iter().find(|(f, _)| *f == filter) else { continue };
                            let label = gtk4::Label::new(Some(&count.to_string()));
                            label.add_css_class("dim-label");
                            label.add_css_class("numeric");
                            item.set_suffix(Some(&label));
                            // "Ausstehend" is only worth a row while there's something in it.
                            item.set_visible(filter != BlogFilter::Pending || count > 0);
                        }
                    }
                    Err(err) => {
                        this.set_site_state("network-offline-symbolic", &tr("Blog nicht erreichbar"));
                        this.site_label.set_tooltip_text(Some(&err));
                    }
                }
            },
        );
    }

    fn set_site_state(&self, icon: &str, label: &str) {
        self.site_icon.set_icon_name(Some(icon));
        self.site_label.set_label(label);
        self.site_label.set_tooltip_text(None);
    }
}

/// Fills a row from an article: title, status subtitle, sync suffix.
fn apply(item: &adw::SidebarItem, path: &Path, doc: &Document, remote: &Remote) {
    let title = library::title_hint(doc)
        .or_else(|| path.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| tr("Unbenannt"));
    item.set_title(Some(&title));
    let state = syncstate::state(doc, remote);
    item.set_subtitle(Some(&status_text(doc, state.status)));
    item.set_icon_name(Some("text-x-generic-symbolic"));
    let suffix = match state.sync {
        SyncState::LocalChanges => Some(("document-send-symbolic", tr("Änderungen noch nicht hochgeladen"))),
        SyncState::Conflict | SyncState::RemoteChanged => Some(("dialog-warning-symbolic", tr("Im Blog geändert"))),
        SyncState::RemoteGone => Some(("dialog-warning-symbolic", tr("Im Blog gelöscht"))),
        SyncState::LocalOnly | SyncState::InSync => None,
    };
    match suffix {
        Some((icon, tooltip)) => {
            let image = gtk4::Image::from_icon_name(icon);
            image.set_tooltip_text(Some(&tooltip));
            image.add_css_class("accent");
            item.set_suffix(Some(&image));
        }
        None => item.set_suffix(gtk4::Widget::NONE),
    }
    item.set_tooltip(Some(&glib::markup_escape_text(&path.display().to_string())));
}

/// "Nur lokal", "Entwurf", "Geplant · 2026-10-03 08:00", ...
fn status_text(doc: &Document, status: Option<PostStatus>) -> String {
    match status {
        None => tr("Nur lokal"),
        Some(PostStatus::Future) => match &doc.frontmatter.scheduled_at {
            Some(at) => format!("{} · {}", PostStatus::Future.label(), crate::document::format_scheduled_at_for_display(at)),
            None => PostStatus::Future.label(),
        },
        Some(status) => status.label(),
    }
}
