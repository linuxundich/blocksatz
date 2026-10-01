//! The blog archive page: the site's posts (or pages) in one status group,
//! newest first, searchable on the server and loaded 50 at a time as the
//! list scrolls. Opened from the library sidebar's "Im Blog" entries;
//! activating a row fetches the post and hands it to the caller, which
//! opens it through its working copy (`docs/gui-redesign.md`, 5.3).
//!
//! A `GtkListBox` rather than the sidebar itself: `AdwSidebar` is meant
//! for a handful of navigation entries, not for an archive of hundreds.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::{Rc, Weak};

use adw::prelude::*;

use crate::document::PostType;
use crate::i18n::tr;
use crate::importer::{self, ImportedPost};
use crate::{library, wpclient, wpsite};

/// The "Im Blog" groups the sidebar offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlogFilter {
    Drafts,
    Pending,
    Scheduled,
    Published,
    Pages,
    Trash,
}

impl BlogFilter {
    pub const ALL: [BlogFilter; 6] = [
        BlogFilter::Drafts,
        BlogFilter::Pending,
        BlogFilter::Scheduled,
        BlogFilter::Published,
        BlogFilter::Pages,
        BlogFilter::Trash,
    ];

    pub fn title(self) -> String {
        match self {
            BlogFilter::Drafts => tr("Entwürfe"),
            BlogFilter::Pending => tr("Ausstehend"),
            BlogFilter::Scheduled => tr("Geplant"),
            BlogFilter::Published => tr("Veröffentlicht"),
            BlogFilter::Pages => tr("Seiten"),
            BlogFilter::Trash => tr("Papierkorb"),
        }
    }

    pub fn icon_name(self) -> &'static str {
        match self {
            BlogFilter::Drafts => "document-edit-symbolic",
            BlogFilter::Pending => "mail-unread-symbolic",
            BlogFilter::Scheduled => "alarm-symbolic",
            BlogFilter::Published => "web-browser-symbolic",
            BlogFilter::Pages => "text-x-generic-symbolic",
            BlogFilter::Trash => "user-trash-symbolic",
        }
    }

    pub fn post_type(self) -> PostType {
        if self == BlogFilter::Pages { PostType::Page } else { PostType::Post }
    }

    /// The WordPress `status` values this group shows.
    pub fn statuses(self) -> &'static str {
        match self {
            BlogFilter::Drafts => "draft",
            BlogFilter::Pending => "pending",
            BlogFilter::Scheduled => "future",
            BlogFilter::Published => "publish,private",
            BlogFilter::Pages => "publish,future,draft,pending,private",
            BlogFilter::Trash => "trash",
        }
    }

    /// Unfinished work is sorted by last edit, everything else by date.
    fn by_modified(self) -> bool {
        matches!(self, BlogFilter::Drafts | BlogFilter::Pending)
    }

    /// Groups mixing several statuses name each row's status (unless it's
    /// the plain "publish" most rows have).
    fn mixed(self) -> bool {
        matches!(self, BlogFilter::Published | BlogFilter::Pages)
    }
}

struct State {
    filter: BlogFilter,
    page: u32,
    total_pages: u32,
    loading: bool,
    /// Bumped on every fresh load, so a slow answer for an old filter or
    /// search can't land in the list for a newer one.
    generation: u64,
    posts: Vec<wpclient::PostSummary>,
    in_library: HashSet<u64>,
}

pub struct BlogPostsPage {
    pub page: adw::NavigationPage,
    title: adw::WindowTitle,
    search: gtk4::SearchEntry,
    list: gtk4::ListBox,
    stack: gtk4::Stack,
    status_page: adw::StatusPage,
    more_spinner: adw::Spinner,
    state: RefCell<State>,
    /// The search the list currently shows. `search-changed` arrives with
    /// a delay - also after `show` cleared the field itself - so a change
    /// only reloads when the text really differs from this.
    shown_search: RefCell<String>,
    on_open: Rc<dyn Fn(ImportedPost)>,
    on_error: Rc<dyn Fn(&str)>,
    on_changed: Rc<dyn Fn()>,
    /// Handed to async callbacks, which must not keep the page alive.
    weak: Weak<BlogPostsPage>,
}

impl BlogPostsPage {
    /// `on_open` receives a fetched post; `on_error` shows a message (a
    /// toast) for failures that don't belong in the list itself;
    /// `on_changed` runs after a post was trashed or restored.
    pub fn new(on_open: Rc<dyn Fn(ImportedPost)>, on_error: Rc<dyn Fn(&str)>, on_changed: Rc<dyn Fn()>) -> Rc<Self> {
        let title = adw::WindowTitle::new(&tr("Beiträge"), "");
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title));

        let search = gtk4::SearchEntry::builder().placeholder_text(tr("Im Blog suchen")).hexpand(true).search_delay(300).build();
        let search_clamp = adw::Clamp::builder().maximum_size(640).margin_start(12).margin_end(12).margin_top(6).margin_bottom(6).child(&search).build();

        let list = gtk4::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk4::SelectionMode::None);
        let more_spinner = adw::Spinner::builder().height_request(32).visible(false).build();
        let column = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(12).margin_top(12).margin_bottom(24).margin_start(12).margin_end(12).build();
        column.append(&list);
        column.append(&more_spinner);
        let scrolled = gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vexpand(true)
            .child(&adw::Clamp::builder().maximum_size(800).child(&column).build())
            .build();

        let loading = adw::Spinner::builder().width_request(32).height_request(32).halign(gtk4::Align::Center).valign(gtk4::Align::Center).build();
        let status_page = adw::StatusPage::new();
        let stack = gtk4::Stack::new();
        stack.add_named(&scrolled, Some("list"));
        stack.add_named(&loading, Some("loading"));
        stack.add_named(&status_page, Some("status"));

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.add_top_bar(&search_clamp);
        toolbar.set_content(Some(&stack));
        let page = adw::NavigationPage::builder().title(tr("Beiträge")).tag("posts").child(&toolbar).build();

        let this = Rc::new_cyclic(|weak| BlogPostsPage {
            page,
            title,
            search: search.clone(),
            list: list.clone(),
            stack,
            status_page,
            more_spinner,
            state: RefCell::new(State {
                filter: BlogFilter::Drafts,
                page: 1,
                total_pages: 1,
                loading: false,
                generation: 0,
                posts: Vec::new(),
                in_library: HashSet::new(),
            }),
            shown_search: RefCell::new(String::new()),
            on_open,
            on_error,
            on_changed,
            weak: weak.clone(),
        });

        let weak = Rc::downgrade(&this);
        search.connect_search_changed(move |entry| {
            if let Some(this) = weak.upgrade().filter(|this| *this.shown_search.borrow() != entry.text().as_str()) {
                this.load(true);
            }
        });
        let weak = Rc::downgrade(&this);
        scrolled.connect_edge_reached(move |_, edge| {
            if edge == gtk4::PositionType::Bottom {
                if let Some(this) = weak.upgrade() {
                    this.load_more();
                }
            }
        });
        let weak = Rc::downgrade(&this);
        list.connect_row_activated(move |_, row| {
            if let Some(this) = weak.upgrade() {
                this.open_row(row.index());
            }
        });
        this
    }

    /// Shows `filter`'s posts from the top, with an empty search.
    pub fn show(&self, filter: BlogFilter) {
        self.state.borrow_mut().filter = filter;
        self.title.set_title(&filter.title());
        self.page.set_title(&filter.title());
        self.search.set_text("");
        self.load(true);
    }

    fn load_more(&self) {
        let more = {
            let state = self.state.borrow();
            !state.loading && state.page < state.total_pages
        };
        if more {
            self.state.borrow_mut().page += 1;
            self.load(false);
        }
    }

    fn load(&self, reset: bool) {
        let site = wpsite::load();
        if site.url.is_empty() {
            self.show_status("network-offline-symbolic", &tr("Kein Blog verbunden"), &tr("Die WordPress-Verbindung wird in den Einstellungen eingerichtet."));
            return;
        }
        let (filter, page, generation) = {
            let mut state = self.state.borrow_mut();
            if reset {
                state.generation += 1;
                state.page = 1;
                state.total_pages = 1;
                state.posts.clear();
                state.in_library = library::scan(&library::root()).iter().filter_map(|entry| entry.document.frontmatter.wp_post_id).collect();
            }
            state.loading = true;
            (state.filter, state.page, state.generation)
        };
        if reset {
            while let Some(child) = self.list.first_child() {
                self.list.remove(&child);
            }
            self.title.set_subtitle("");
            self.stack.set_visible_child_name("loading");
        } else {
            self.more_spinner.set_visible(true);
        }

        let search = self.search.text().to_string();
        *self.shown_search.borrow_mut() = search.clone();
        let weak = self.weak();
        importer::run_with_password(
            &site,
            move |site, password| {
                wpclient::Client::new(&site.url, &site.username, password)
                    .query_items(filter.post_type().rest_base(), filter.statuses(), &search, filter.by_modified(), page)
                    .map_err(|err| err.to_string())
            },
            move |outcome| {
                let Some(this) = weak.upgrade() else { return };
                if this.state.borrow().generation != generation {
                    return;
                }
                this.state.borrow_mut().loading = false;
                this.more_spinner.set_visible(false);
                match outcome {
                    Ok(result) => this.append(result, reset),
                    Err(err) if reset => this.show_status("dialog-error-symbolic", &tr("Beiträge konnten nicht geladen werden"), &err),
                    Err(err) => (this.on_error)(&tr("Fehler beim Laden: {err}").replace("{err}", &err)),
                }
            },
        );
    }

    fn append(&self, result: wpclient::PostPage, reset: bool) {
        let filter = self.state.borrow().filter;
        if reset && result.items.is_empty() {
            let searching = !self.search.text().trim().is_empty();
            let (title, description) = if searching {
                (tr("Keine Treffer"), tr("Ein anderer Suchbegriff findet vielleicht mehr."))
            } else {
                (tr("Keine Beiträge"), String::new())
            };
            self.show_status(if searching { "edit-find-symbolic" } else { filter.icon_name() }, &title, &description);
            self.title.set_subtitle("");
            return;
        }
        self.title.set_subtitle(&tr("{n} insgesamt").replace("{n}", &result.total.to_string()));
        let in_library = self.state.borrow().in_library.clone();
        for post in &result.items {
            self.list.append(&self.row(post, filter, in_library.contains(&post.id)));
        }
        {
            let mut state = self.state.borrow_mut();
            state.total_pages = result.total_pages.max(1);
            state.posts.extend(result.items);
        }
        self.stack.set_visible_child_name("list");
    }

    fn row(&self, post: &wpclient::PostSummary, filter: BlogFilter, in_library: bool) -> adw::ActionRow {
        let date_source = if filter.by_modified() && !post.modified_gmt.is_empty() { &post.modified_gmt } else { &post.date };
        let date = format_date(date_source);
        let subtitle = if filter.mixed() && post.status != "publish" { format!("{date} · {}", importer::status_display(&post.status)) } else { date };
        let title = if post.title.trim().is_empty() { tr("(ohne Titel)") } else { post.title.clone() };
        let row = adw::ActionRow::builder().title(title).subtitle(subtitle).use_markup(false).activatable(filter != BlogFilter::Trash).build();

        if in_library {
            let icon = gtk4::Image::from_icon_name("folder-documents-symbolic");
            icon.set_tooltip_text(Some(&tr("Liegt in der Bibliothek")));
            row.add_suffix(&icon);
        }
        let button = if filter == BlogFilter::Trash {
            let button = gtk4::Button::with_label(&tr("Wiederherstellen"));
            button.set_tooltip_text(Some(&tr("Als Entwurf wiederherstellen")));
            button
        } else {
            let button = gtk4::Button::from_icon_name("user-trash-symbolic");
            button.set_tooltip_text(Some(&tr("In den Papierkorb")));
            button
        };
        button.set_valign(gtk4::Align::Center);
        button.add_css_class("flat");
        let weak = self.weak();
        let post = post.clone();
        button.connect_clicked(move |button| {
            if let Some(this) = weak.upgrade() {
                if filter == BlogFilter::Trash {
                    this.restore(&post);
                } else {
                    this.confirm_trash(button, &post);
                }
            }
        });
        row.add_suffix(&button);
        row
    }

    fn open_row(&self, index: i32) {
        let (post, post_type) = {
            let state = self.state.borrow();
            let Some(post) = usize::try_from(index).ok().and_then(|i| state.posts.get(i)).cloned() else { return };
            (post, state.filter.post_type())
        };
        self.list.set_sensitive(false);
        // Feedback on the row itself while the post is fetched.
        let spinner = adw::Spinner::new();
        let row = self.list.row_at_index(index).and_then(|row| row.downcast::<adw::ActionRow>().ok());
        if let Some(row) = &row {
            row.add_suffix(&spinner);
        }
        let weak = self.weak();
        importer::run_with_password(
            &wpsite::load(),
            move |site, password| importer::fetch_and_convert(site, password, post_type, post.id),
            move |outcome| {
                if let Some(row) = &row {
                    row.remove(&spinner);
                }
                let Some(this) = weak.upgrade() else { return };
                this.list.set_sensitive(true);
                match outcome {
                    Ok(imported) => (this.on_open)(imported),
                    Err(err) => (this.on_error)(&tr("Öffnen fehlgeschlagen: {err}").replace("{err}", &err)),
                }
            },
        );
    }

    /// Asks before moving `post` to WordPress's trash - recoverable from
    /// the "Papierkorb" entry, but not something a stray click should do.
    fn confirm_trash(&self, anchor: &gtk4::Button, post: &wpclient::PostSummary) {
        let confirm = adw::AlertDialog::new(
            Some(&tr("In den Papierkorb verschieben?")),
            Some(&tr("„{title}“ wird in den WordPress-Papierkorb verschoben und lässt sich von dort wiederherstellen.").replace("{title}", &post.title)),
        );
        confirm.add_response("cancel", &tr("Abbrechen"));
        confirm.add_response("trash", &tr("In den Papierkorb"));
        confirm.set_response_appearance("trash", adw::ResponseAppearance::Destructive);
        confirm.set_default_response(Some("cancel"));
        confirm.set_close_response("cancel");
        let weak = self.weak();
        let post = post.clone();
        confirm.connect_response(None, move |_, response| {
            if response != "trash" {
                return;
            }
            let Some(this) = weak.upgrade() else { return };
            let rest_base = this.state.borrow().filter.post_type().rest_base();
            let post_id = post.id;
            this.run_and_reload(move |client| client.trash_item(rest_base, post_id));
        });
        confirm.present(Some(anchor));
    }

    fn restore(&self, post: &wpclient::PostSummary) {
        let post_id = post.id;
        self.run_and_reload(move |client| client.update_item("posts", post_id, &serde_json::json!({ "status": "draft" })).map(|_| ()));
    }

    /// Runs a change against the site, then reloads the list.
    fn run_and_reload(&self, job: impl FnOnce(&wpclient::Client) -> wpclient::Result<()> + Send + 'static) {
        self.list.set_sensitive(false);
        let weak = self.weak();
        importer::run_with_password(
            &wpsite::load(),
            move |site, password| job(&wpclient::Client::new(&site.url, &site.username, password)).map_err(|err| err.to_string()),
            move |outcome| {
                let Some(this) = weak.upgrade() else { return };
                this.list.set_sensitive(true);
                match outcome {
                    Ok(()) => {
                        this.load(true);
                        (this.on_changed)();
                    }
                    Err(err) => (this.on_error)(&tr("Fehler: {err}").replace("{err}", &err)),
                }
            },
        );
    }

    fn show_status(&self, icon: &str, title: &str, description: &str) {
        self.status_page.set_icon_name(Some(icon));
        self.status_page.set_title(title);
        self.status_page.set_description((!description.is_empty()).then_some(description));
        self.stack.set_visible_child_name("status");
    }

    fn weak(&self) -> Weak<Self> {
        self.weak.clone()
    }
}

/// `"2026-09-28T10:00:00"` → `"28.09.2026"`.
fn format_date(iso: &str) -> String {
    let date = iso.split('T').next().unwrap_or(iso);
    match date.split('-').collect::<Vec<_>>().as_slice() {
        [y, m, d] => format!("{d}.{m}.{y}"),
        _ => date.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_shown_day_first() {
        assert_eq!(format_date("2026-09-28T10:00:00"), "28.09.2026");
        assert_eq!(format_date("kaputt"), "kaputt");
    }
}
