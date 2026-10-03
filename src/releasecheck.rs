//! The release check (`docs/gui-redesign.md`, 5.6): what stands between
//! "Veröffentlichen …" and the post going live. One dialog lists the
//! checks - title, excerpt, category, tags, featured image, image alt
//! texts, links, focus keyword - each with its state and a way to fix it,
//! plus the choice "Sofort / Geplant". Links and images open as sub-pages
//! with the existing link checker and media manager. Replaces the old
//! export wizard (a carousel of the same steps) and the plain "publish?"
//! confirmation; only the final button actually uploads.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::browser;
use crate::document::{self, Document, PostType};
use crate::i18n::tr;
use crate::window::DocContext;
use crate::{library, linkcheck, mediapanel};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Ok,
    /// Missing but optional - worth a look, never blocking.
    Hint,
    Warning,
    /// Blocks publishing.
    Error,
}

/// Where a check's fix lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fix {
    /// The "Beitrag" view of the right-hand pane.
    Properties,
    Media,
    Links,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub title: String,
    pub detail: String,
    pub severity: Severity,
    pub fix: Fix,
}

fn check(title: String, detail: String, severity: Severity, fix: Fix) -> Check {
    Check { title, detail, severity, fix }
}

/// All checks for `doc`. Pure, so it's tested without GTK.
pub fn checks(doc: &Document) -> Vec<Check> {
    let fm = &doc.frontmatter;
    let mut out = Vec::new();

    match library::title_hint(doc) {
        Some(title) => out.push(check(tr("Titel"), title, Severity::Ok, Fix::Properties)),
        None => out.push(check(tr("Titel"), tr("Kein Titel gesetzt."), Severity::Error, Fix::Properties)),
    }

    let excerpt = fm.excerpt.as_deref().map(str::trim).unwrap_or_default();
    out.push(if excerpt.is_empty() {
        check(tr("Auszug"), tr("Ohne Auszug zeigen Feeds und Vorschaukarten einen abgeschnittenen Textanfang."), Severity::Warning, Fix::Properties)
    } else {
        check(tr("Auszug"), tr("{n} Zeichen").replace("{n}", &excerpt.chars().count().to_string()), Severity::Ok, Fix::Properties)
    });

    if fm.post_type == PostType::Post {
        out.push(if fm.categories.is_empty() {
            check(tr("Kategorie"), tr("Keine Kategorie gesetzt; WordPress nimmt dann die Standardkategorie."), Severity::Warning, Fix::Properties)
        } else {
            check(tr("Kategorie"), fm.categories.join(", "), Severity::Ok, Fix::Properties)
        });
        out.push(if fm.tags.is_empty() {
            check(tr("Tags"), tr("Keine Tags gesetzt."), Severity::Hint, Fix::Properties)
        } else {
            check(tr("Tags"), fm.tags.join(", "), Severity::Ok, Fix::Properties)
        });
    }

    let has_featured = fm.featured_image.is_some() || fm.featured_media_id.is_some();
    let featured_alt_missing = fm.featured_image.is_some() && fm.featured_image_alt.as_deref().is_none_or(|alt| alt.trim().is_empty());
    out.push(match (has_featured, featured_alt_missing) {
        (false, _) => check(tr("Beitragsbild"), tr("Kein Beitragsbild gesetzt."), Severity::Warning, Fix::Properties),
        (true, true) => check(tr("Beitragsbild"), tr("Das Beitragsbild hat keinen Alternativtext."), Severity::Warning, Fix::Properties),
        (true, false) => check(tr("Beitragsbild"), tr("Gesetzt"), Severity::Ok, Fix::Properties),
    });

    let images = fm.media.len();
    let without_alt = fm.media.iter().filter(|item| item.alt.is_undefined()).count();
    if images > 0 {
        out.push(if without_alt > 0 {
            let detail = if without_alt == 1 { tr("1 Bild ohne Alternativtext") } else { tr("{n} Bilder ohne Alternativtext").replace("{n}", &without_alt.to_string()) };
            check(tr("Bilder"), detail, Severity::Warning, Fix::Media)
        } else {
            let detail = if images == 1 { tr("1 Bild, Alternativtext gesetzt") } else { tr("{n} Bilder, alle mit Alternativtext").replace("{n}", &images.to_string()) };
            check(tr("Bilder"), detail, Severity::Ok, Fix::Media)
        });
    }

    let links = linkcheck::scan_links(&doc.body).len();
    if links > 0 {
        let detail = if links == 1 { tr("1 Link – vor dem Veröffentlichen prüfen") } else { tr("{n} Links – vor dem Veröffentlichen prüfen").replace("{n}", &links.to_string()) };
        out.push(check(tr("Links"), detail, Severity::Hint, Fix::Links));
    }

    if let Some(link) = &fm.translation {
        out.push(if link.reviewed {
            check(tr("Übersetzung"), tr("Gegengelesen"), Severity::Ok, Fix::Properties)
        } else {
            check(tr("Übersetzung"), tr("Noch nicht gegengelesen – im Menü „Gegenlesen …“ bestätigen."), Severity::Error, Fix::Properties)
        });
    }

    out.push(match fm.rank_math_focus_keyword.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
        Some(keyword) => check(tr("Fokus-Keyword"), keyword.to_string(), Severity::Ok, Fix::Properties),
        None => check(tr("Fokus-Keyword"), tr("Optional, für RankMath."), Severity::Hint, Fix::Properties),
    });

    out
}

/// What the dialog was opened for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A draft (or a local article) going live, now or scheduled.
    Publish { scheduled: bool },
    /// Changes to an already published post.
    PublishChanges,
}

/// The user's decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Now,
    /// At this normalized `"YYYY-MM-DDTHH:MM:00"` date.
    Scheduled(String),
}

/// Where links found by the link checker open.
pub struct LinkTarget {
    pub view_stack: adw::ViewStack,
    pub browser_view: Rc<browser::BrowserView>,
}

/// Shows the release check; `on_decision` runs when the final button is
/// pressed (not on cancel).
pub fn open(window: &adw::ApplicationWindow, ctx: &DocContext, mode: Mode, links: &LinkTarget, on_decision: impl Fn(Decision) + 'static) {
    let doc = ctx.current_document();
    let nav = adw::NavigationView::new();
    let dialog = adw::Dialog::builder().title(tr("Veröffentlichen")).content_width(560).content_height(820).child(&nav).build();

    let checks_group = adw::PreferencesGroup::builder().title(tr("Prüfung")).build();
    let all_checks = checks(&doc);
    let blocked = all_checks.iter().any(|c| c.severity == Severity::Error);
    for c in &all_checks {
        checks_group.add(&check_row(c, &dialog, &nav, ctx, &doc, links));
    }

    let page_content = adw::PreferencesPage::new();
    page_content.add(&checks_group);

    // "Sofort / Geplant" - not for changes to a post that's already live.
    let schedule_entry = adw::EntryRow::builder().title(tr("Termin (JJJJ-MM-TT HH:MM)")).build();
    if let Some(at) = &doc.frontmatter.scheduled_at {
        schedule_entry.set_text(&document::format_scheduled_at_for_display(at));
    }
    let when = adw::ToggleGroup::new();
    when.add(adw::Toggle::builder().name("now").label(tr("Sofort")).build());
    when.add(adw::Toggle::builder().name("scheduled").label(tr("Geplant")).build());
    let scheduled_initially = matches!(mode, Mode::Publish { scheduled: true });
    when.set_active_name(Some(if scheduled_initially { "scheduled" } else { "now" }));
    schedule_entry.set_visible(scheduled_initially);
    if let Mode::Publish { .. } = mode {
        let when_row = adw::ActionRow::builder().title(tr("Zeitpunkt")).build();
        when.set_valign(gtk4::Align::Center);
        when_row.add_suffix(&when);
        let when_group = adw::PreferencesGroup::new();
        when_group.add(&when_row);
        when_group.add(&schedule_entry);
        page_content.add(&when_group);
    }

    let accept_label = move |scheduled: bool| match mode {
        Mode::PublishChanges => tr("Änderungen veröffentlichen"),
        Mode::Publish { .. } if scheduled => tr("Planen"),
        Mode::Publish { .. } => tr("Jetzt veröffentlichen"),
    };
    let accept = gtk4::Button::builder().label(accept_label(scheduled_initially)).sensitive(!blocked).build();
    accept.add_css_class("suggested-action");
    accept.add_css_class("pill");
    let cancel = gtk4::Button::builder().label(tr("Abbrechen")).build();
    cancel.add_css_class("pill");
    let buttons = gtk4::Box::builder().spacing(12).halign(gtk4::Align::Center).margin_top(12).margin_bottom(12).build();
    buttons.append(&cancel);
    buttons.append(&accept);

    {
        let accept = accept.clone();
        let schedule_entry = schedule_entry.clone();
        when.connect_active_name_notify(move |group| {
            let scheduled = group.active_name().as_deref() == Some("scheduled");
            schedule_entry.set_visible(scheduled);
            accept.set_label(&accept_label(scheduled));
        });
    }
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| {
            dialog.close();
        });
    }
    {
        let dialog = dialog.clone();
        let when = when.clone();
        let schedule_entry = schedule_entry.clone();
        let on_decision = Rc::new(on_decision);
        accept.connect_clicked(move |_| {
            let scheduled = matches!(mode, Mode::Publish { .. }) && when.active_name().as_deref() == Some("scheduled");
            if scheduled {
                let Some(at) = document::parse_scheduled_at(&schedule_entry.text()) else {
                    schedule_entry.add_css_class("error");
                    return;
                };
                dialog.close();
                on_decision(Decision::Scheduled(at));
            } else {
                dialog.close();
                on_decision(Decision::Now);
            }
        });
    }
    {
        schedule_entry.connect_changed(|entry| entry.remove_css_class("error"));
    }

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page_content));
    toolbar.add_bottom_bar(&buttons);
    let title = match mode {
        Mode::PublishChanges => tr("Änderungen veröffentlichen"),
        Mode::Publish { .. } => tr("Veröffentlichen"),
    };
    nav.add(&adw::NavigationPage::builder().title(title).child(&toolbar).build());
    dialog.present(Some(window));
}

fn check_row(c: &Check, dialog: &adw::Dialog, nav: &adw::NavigationView, ctx: &DocContext, doc: &Document, links: &LinkTarget) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(c.title.as_str()).subtitle(c.detail.as_str()).use_markup(false).build();
    let (icon, class) = match c.severity {
        Severity::Ok => ("object-select-symbolic", "success"),
        Severity::Hint => ("dialog-information-symbolic", "dim-label"),
        Severity::Warning => ("dialog-warning-symbolic", "warning"),
        Severity::Error => ("dialog-error-symbolic", "error"),
    };
    let image = gtk4::Image::from_icon_name(icon);
    image.add_css_class(class);
    row.add_prefix(&image);

    match c.fix {
        Fix::Properties if c.severity != Severity::Ok => {
            let button = flat_button(&tr("Beheben"));
            let dialog = dialog.clone();
            let window = ctx.toast_overlay.root().and_downcast::<adw::ApplicationWindow>();
            button.connect_clicked(move |_| {
                dialog.close();
                if let Some(window) = &window {
                    let _ = WidgetExt::activate_action(window, "win.properties", None);
                }
            });
            row.add_suffix(&button);
        }
        Fix::Properties => {}
        Fix::Media => {
            let doc_dir = ctx.current_path.borrow().as_deref().and_then(std::path::Path::parent).map(std::path::Path::to_path_buf);
            let content = mediapanel::build_content(ctx.frontmatter.clone(), &doc.body, doc_dir, ctx.preview_pane.clone());
            add_subpage(&row, nav, &tr("Bilder"), content);
        }
        Fix::Links => {
            let content = linkcheck::build_content(&doc.body, &links.view_stack, &links.browser_view);
            add_subpage(&row, nav, &tr("Links"), content);
        }
    }
    row
}

/// Makes `row` open `content` as a sub-page of the dialog. Built lazily
/// would be nicer, but the link checker starts its requests on build, and
/// opening the dialog is exactly when they should start.
fn add_subpage(row: &adw::ActionRow, nav: &adw::NavigationView, title: &str, content: gtk4::Widget) {
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&gtk4::ScrolledWindow::builder().child(&content).vexpand(true).build()));
    let page = adw::NavigationPage::builder().title(title).child(&toolbar).build();
    let page = Rc::new(RefCell::new(Some(page)));
    row.set_activatable(true);
    row.add_suffix(&gtk4::Image::from_icon_name("go-next-symbolic"));
    let nav = nav.clone();
    row.connect_activated(move |_| {
        if let Some(page) = page.borrow().as_ref() {
            nav.push(page);
        }
    });
}

fn flat_button(label: &str) -> gtk4::Button {
    let button = gtk4::Button::builder().label(label).valign(gtk4::Align::Center).build();
    button.add_css_class("flat");
    button
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Frontmatter;
    use crate::media::{AltText, MediaItem};

    fn severity_of(checks: &[Check], title: &str) -> Option<Severity> {
        checks.iter().find(|c| c.title == tr(title)).map(|c| c.severity)
    }

    #[test]
    fn a_missing_title_blocks_publishing() {
        let doc = Document { frontmatter: Frontmatter::default(), body: "Nur Text\n".into() };
        assert_eq!(severity_of(&checks(&doc), "Titel"), Some(Severity::Error));

        let doc = Document { frontmatter: Frontmatter::default(), body: "# Überschrift\nText\n".into() };
        assert_eq!(severity_of(&checks(&doc), "Titel"), Some(Severity::Ok));
    }

    #[test]
    fn a_complete_post_has_no_warnings() {
        let fm = Frontmatter {
            title: "Titel".into(),
            excerpt: Some("Kurz".into()),
            categories: vec!["Linux".into()],
            tags: vec!["GNOME".into()],
            featured_image: Some("bild.png".into()),
            featured_image_alt: Some("Ein Bild".into()),
            rank_math_focus_keyword: Some("gnome".into()),
            wp_footnotes: None,
            markdown_hint: false,
            ..Frontmatter::default()
        };
        let doc = Document { frontmatter: fm, body: "Text\n".into() };
        assert!(checks(&doc).iter().all(|c| c.severity == Severity::Ok), "{:?}", checks(&doc));
    }

    #[test]
    fn missing_metadata_and_alt_texts_are_flagged() {
        let mut fm = Frontmatter { title: "Titel".into(), ..Frontmatter::default() };
        fm.media = vec![MediaItem {
            id: "1".into(),
            filename: "a.png".into(),
            source: "a.png".into(),
            alt: AltText::Undefined,
            caption: None,
            wordpress: None,
            last_markdown_caption: None,
        }];
        let doc = Document { frontmatter: fm, body: "![](a.png)\n\n[Link](https://example.org)\n".into() };
        let all = checks(&doc);
        assert_eq!(severity_of(&all, "Auszug"), Some(Severity::Warning));
        assert_eq!(severity_of(&all, "Kategorie"), Some(Severity::Warning));
        assert_eq!(severity_of(&all, "Tags"), Some(Severity::Hint));
        assert_eq!(severity_of(&all, "Beitragsbild"), Some(Severity::Warning));
        assert_eq!(severity_of(&all, "Bilder"), Some(Severity::Warning));
        assert_eq!(severity_of(&all, "Links"), Some(Severity::Hint));
    }

    #[test]
    fn pages_skip_categories_and_tags() {
        let fm = Frontmatter { title: "Impressum".into(), post_type: PostType::Page, ..Frontmatter::default() };
        let all = checks(&Document { frontmatter: fm, body: String::new() });
        assert_eq!(severity_of(&all, "Kategorie"), None);
        assert_eq!(severity_of(&all, "Tags"), None);
    }
    #[test]
    fn unreviewed_translation_blocks_publishing() {
        let mut fm = Frontmatter { title: "Title".into(), translation: Some(crate::document::TranslationLink { source_site: "example.org".into(), source_id: 1, ..Default::default() }), ..Frontmatter::default() };
        let doc = |fm: &Frontmatter| Document { frontmatter: fm.clone(), body: "Body.\n".into() };
        let severity = |fm: &Frontmatter| checks(&doc(fm)).into_iter().find(|c| c.title == tr("Übersetzung")).map(|c| c.severity);
        assert_eq!(severity(&fm), Some(Severity::Error));
        fm.translation.as_mut().unwrap().reviewed = true;
        assert_eq!(severity(&fm), Some(Severity::Ok));
        fm.translation = None;
        assert_eq!(severity(&fm), None);
    }

}
