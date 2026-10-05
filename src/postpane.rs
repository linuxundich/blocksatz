//! The "Beitrag" view of the right-hand pane (`docs/gui-redesign.md`,
//! 5.5): everything about the open article that isn't its text, next to
//! the editor instead of in a modal dialog - a status card (state, preview
//! and wp-admin links), the properties (`properties.rs`), the media
//! manager entry and the statistics.
//!
//! The properties are rebuilt only when another article is loaded
//! (`DocContext::doc_generation`), never while one is being edited, so
//! typing into a field is never interrupted.

use std::cell::Cell;
use std::path::Path;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk4::glib;

use crate::document::PostStatus;
use crate::i18n::tr;
use crate::syncstate::{self, Remote};
use crate::window::DocContext;
use crate::{blockinspector, mainaction, properties, termcache, wpsite};

pub struct PostPane {
    pub widget: gtk4::Widget,
    state_label: gtk4::Label,
    detail_label: gtk4::Label,
    /// "Markdown-Nähe: mittel/gering" - only for articles with more than
    /// plain Markdown (`markdowncheck.rs`).
    closeness_row: gtk4::Box,
    closeness_label: gtk4::Label,
    preview_button: gtk4::Button,
    admin_button: gtk4::Button,
    properties_slot: gtk4::Box,
    media_row: adw::ActionRow,
    shown_generation: Cell<Option<u64>>,
    /// The header-relevant fields last handed to the preview, so it only
    /// re-renders when one of them changed.
    shown_header: std::cell::RefCell<String>,
    window: glib::WeakRef<adw::ApplicationWindow>,
    ctx: DocContext,
    term_caches: termcache::TermCacheHandles,
    open_url: Rc<dyn Fn(String)>,
    _inspector: Rc<blockinspector::BlockInspector>,
    weak: Weak<PostPane>,
}

impl PostPane {
    pub fn new(window: &adw::ApplicationWindow, ctx: &DocContext, term_caches: &termcache::TermCacheHandles, stats: &gtk4::Widget, open_url: Rc<dyn Fn(String)>) -> Rc<Self> {
        let state_label = gtk4::Label::builder().xalign(0.0).wrap(true).build();
        state_label.add_css_class("heading");
        let detail_label = gtk4::Label::builder().xalign(0.0).wrap(true).build();
        detail_label.add_css_class("dim-label");
        let preview_button = gtk4::Button::builder().label(tr("Blog-Vorschau öffnen")).action_name("main.open-preview").build();
        let admin_button = gtk4::Button::builder().label(tr("In wp-admin bearbeiten")).build();
        let closeness_label = gtk4::Label::builder().xalign(0.0).wrap(true).hexpand(true).build();
        closeness_label.add_css_class("dim-label");
        let closeness_button = gtk4::Button::builder().label(tr("Details")).valign(gtk4::Align::Center).build();
        closeness_button.add_css_class("flat");
        let closeness_row = gtk4::Box::builder().spacing(6).visible(false).build();
        closeness_row.append(&closeness_label);
        closeness_row.append(&closeness_button);
        let buttons = gtk4::Box::builder().spacing(6).margin_top(6).build();
        buttons.append(&preview_button);
        buttons.append(&admin_button);
        let card_content = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(4).margin_top(12).margin_bottom(12).margin_start(12).margin_end(12).build();
        card_content.append(&state_label);
        card_content.append(&detail_label);
        card_content.append(&closeness_row);
        card_content.append(&buttons);
        let card = gtk4::Frame::builder().child(&card_content).build();
        card.add_css_class("card");

        let properties_slot = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).build();

        let media_row = adw::ActionRow::builder().title(tr("Medienverwaltung")).activatable(true).action_name("win.media-manager").build();
        media_row.add_suffix(&gtk4::Image::from_icon_name("go-next-symbolic"));
        let media_group = adw::PreferencesGroup::builder().title(tr("Medien")).build();
        media_group.add(&media_row);

        let stats_group = adw::PreferencesGroup::builder().title(tr("Statistik")).build();
        stats_group.add(stats);

        let inspector = blockinspector::BlockInspector::new(&ctx.buffer);

        let column = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(24).margin_top(12).margin_bottom(24).margin_start(12).margin_end(12).build();
        column.append(&card);
        column.append(&inspector.widget);
        column.append(&properties_slot);
        column.append(&media_group);
        column.append(&stats_group);
        let scrolled = gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vexpand(true)
            .child(&adw::Clamp::builder().maximum_size(640).child(&column).build())
            .build();

        let this = Rc::new_cyclic(|weak| PostPane {
            widget: scrolled.upcast(),
            state_label,
            detail_label,
            closeness_row,
            closeness_label,
            preview_button,
            admin_button: admin_button.clone(),
            properties_slot,
            media_row,
            shown_generation: Cell::new(None),
            shown_header: std::cell::RefCell::new(String::new()),
            window: window.downgrade(),
            ctx: ctx.clone(),
            term_caches: term_caches.clone(),
            open_url,
            _inspector: inspector,
            weak: weak.clone(),
        });
        {
            let weak = this.weak.clone();
            closeness_button.connect_clicked(move |button| {
                let Some(this) = weak.upgrade() else { return };
                let assessment = crate::markdowncheck::assess(&this.ctx.current_document().body);
                crate::markdowncheck::show_details(Some(button.upcast_ref()), &assessment, None);
            });
        }
        {
            let weak = this.weak.clone();
            admin_button.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.open_admin();
                }
            });
        }
        {
            let weak = this.weak.clone();
            ctx.add_library_listener(Rc::new(move |_| {
                let weak = weak.clone();
                glib::idle_add_local_once(move || {
                    if let Some(this) = weak.upgrade() {
                        this.refresh();
                    }
                });
            }));
        }
        this.refresh();
        this
    }

    /// Updates the status card; rebuilds the properties if another
    /// article was loaded since.
    pub fn refresh(&self) {
        let doc = self.ctx.current_document();
        let remote = self.ctx.remote_for(&doc.frontmatter);
        let state = syncstate::state(&doc, &remote);
        self.state_label.set_label(&mainaction::state_text(&doc, state));
        let detail = match (&doc.frontmatter.wp_synced_at, doc.frontmatter.wp_post_id) {
            (Some(at), Some(_)) => tr("Zuletzt abgeglichen: {at}").replace("{at}", &format_timestamp(at)),
            _ => tr("Noch nicht im Blog."),
        };
        self.detail_label.set_label(&detail);
        let assessment = crate::markdowncheck::assess(&doc.body);
        let designed = assessment.closeness != gutenberg::Closeness::Plain;
        self.closeness_row.set_visible(designed);
        if designed {
            self.closeness_label.set_label(&tr("Markdown-Nähe: {level}").replace("{level}", &crate::markdowncheck::closeness_label(assessment.closeness)));
        }
        let linked = doc.frontmatter.wp_post_id.is_some() && !matches!(remote, Remote::Gone);
        let published = matches!(state.status, Some(PostStatus::Publish | PostStatus::Private));
        self.preview_button.set_label(&if published { tr("Im Blog ansehen") } else { tr("Blog-Vorschau öffnen") });
        self.preview_button.set_visible(linked);
        self.admin_button.set_visible(linked);
        let images = doc.frontmatter.media.len();
        self.media_row.set_subtitle(&match images {
            0 => tr("Keine Bilder"),
            1 => tr("1 Bild"),
            n => tr("{n} Bilder").replace("{n}", &n.to_string()),
        });
        // The preview's article header shows these; re-render it only
        // when one of them changed (a full preview render each time).
        let fm = &doc.frontmatter;
        let header = format!("{:?}", (&fm.title, &fm.excerpt, &fm.featured_image, &fm.categories, &fm.author_name, fm.post_type, fm.status, &fm.scheduled_at));
        if *self.shown_header.borrow() != header {
            *self.shown_header.borrow_mut() = header;
            self.ctx.preview_pane.set_article_header(fm);
        }

        let generation = self.ctx.doc_generation.get();
        if self.shown_generation.get() != Some(generation) {
            self.shown_generation.set(Some(generation));
            self.rebuild_properties(doc.body);
        }
    }

    fn rebuild_properties(&self, body: String) {
        let Some(window) = self.window.upgrade() else { return };
        while let Some(child) = self.properties_slot.first_child() {
            self.properties_slot.remove(&child);
        }
        let doc_dir = self.ctx.current_path.borrow().as_deref().and_then(Path::parent).map(Path::to_path_buf);
        let widget = properties::build(&window, body, self.ctx.frontmatter.clone(), self.term_caches.clone(), doc_dir);
        self.properties_slot.append(&widget);
    }

    fn open_admin(&self) {
        let Some(id) = self.ctx.frontmatter.borrow().wp_post_id else { return };
        let path = self.ctx.current_path.borrow().clone();
        let site = wpsite::for_document(path.as_deref(), self.ctx.frontmatter.borrow().wp_site.as_deref());
        (self.open_url)(format!("{}/wp-admin/post.php?post={id}&action=edit", site.url.trim_end_matches('/')));
    }
}

/// `2026-10-01T20:40:19.51Z` → `01.10.2026, 22:40` (local time).
fn format_timestamp(rfc3339: &str) -> String {
    glib::DateTime::from_iso8601(rfc3339, None)
        .and_then(|dt| dt.to_local())
        .and_then(|dt| dt.format("%d.%m.%Y, %H:%M"))
        .map(|s| s.to_string())
        .unwrap_or_else(|_| rfc3339.to_string())
}
