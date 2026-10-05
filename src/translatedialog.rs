//! "Übersetzen …" and "Gegenlesen …" (`docs/translations.md`): the dialogs
//! around `translate.rs`.
//!
//! A translation is an ordinary working copy next to its original, as
//! `artikel.<lang>.md` in the same library folder (`library::Pair`), tied
//! to the other blog through `Frontmatter::wp_site` and to its original
//! through `Frontmatter::translation`. Everything else - autosave, upload,
//! sync state - works on it like on any other article.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk4::glib;

use crate::aitasks::{self, AiTask};
use crate::document::{self, Document};
use crate::i18n::tr;
use crate::llm::{self, ChatMessage};
use crate::translate::{self, Issue, Options, Outcome};
use crate::window::{self, DocContext};
use crate::{aiprompts, library, syncstate, worksave, wpclient, wpsite};

/// Target languages offered for a new translation: (code, display name).
fn languages() -> Vec<(&'static str, String)> {
    vec![("en", tr("Englisch")), ("fr", tr("Französisch")), ("es", tr("Spanisch")), ("it", tr("Italienisch")), ("nl", tr("Niederländisch"))]
}

fn source_site_of(doc: &Document) -> String {
    doc.frontmatter.wp_site.clone().unwrap_or_else(|| wpsite::load().site_id())
}

/// The working copy translating `original` (at `path`, if known): the
/// translation file next to it, else one elsewhere in the library.
pub fn find_translation(original: &Document, path: Option<&Path>) -> Option<(PathBuf, Document)> {
    if let Some(path) = path.filter(|p| library::file_lang(p) == Some(None)) {
        let pair = path.parent().and_then(library::read_pair);
        if let Some(entry) = pair.and_then(|pair| pair.files.into_iter().find(|e| e.document.frontmatter.translation.is_some())) {
            return Some((entry.path, entry.document));
        }
    }
    let post_id = original.frontmatter.wp_post_id?;
    let site = source_site_of(original);
    library::scan(&library::root())
        .into_iter()
        .find(|entry| entry.document.frontmatter.translation.as_ref().is_some_and(|t| t.source_id == post_id && t.source_site == site))
        .map(|entry| (entry.path, entry.document))
}

/// The working copy a translation (at `path`, if known) was made from:
/// `artikel.md` next to it, else the library copy of its source post.
pub fn find_original(translation: &Document, path: Option<&Path>) -> Option<(PathBuf, Document)> {
    let link = translation.frontmatter.translation.as_ref()?;
    if let Some(original) = path.filter(|p| library::file_lang(p).is_some_and(|l| l.is_some())).and_then(|p| library::sibling(p, None)) {
        if let Ok(doc) = document::read(&original) {
            return Some((original, doc));
        }
    }
    let path = library::find_by_post_id(&library::root(), &link.source_site, link.source_id)?;
    let doc = document::read(&path).ok()?;
    Some((path, doc))
}

/// Whether the original changed since `translation` was made - `None` when
/// it isn't a translation or the original isn't in the library.
pub fn original_changed(translation: &Document, path: Option<&Path>) -> Option<bool> {
    let link = translation.frontmatter.translation.as_ref()?;
    let (_, original) = find_original(translation, path)?;
    Some(syncstate::fingerprint(&original) != link.source_hash)
}

fn today() -> String {
    glib::DateTime::now_local().ok().and_then(|now| now.format("%Y-%m-%d").ok()).map(|s| s.to_string()).unwrap_or_default()
}

fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

enum Msg {
    Progress(usize, usize),
    Done(Box<Result<aitasks::Routed<Outcome>, String>>),
}

/// What translating the open article needs, looked up once.
struct Prep {
    source: Document,
    source_path: Option<PathBuf>,
    /// An existing translation to update, with its path.
    previous: Option<(PathBuf, Document)>,
    source_site: String,
    /// Blogs a new translation can go to - every one but the original's.
    targets: Vec<wpsite::SiteConfig>,
}

/// Why the open article can't be translated right now.
enum Blocker {
    /// The translation is open, its original isn't in the library.
    OriginalMissing,
    /// The original isn't on its blog yet - the link needs its post id.
    NotUploaded,
    NoSecondBlog,
}

fn prepare(ctx: &DocContext) -> Result<Prep, Blocker> {
    worksave::flush(ctx, false);
    let current = ctx.current_document();
    let current_path = ctx.current_path.borrow().clone();
    let (source, source_path, previous) = if current.frontmatter.translation.is_some() {
        let (path, original) = find_original(&current, current_path.as_deref()).ok_or(Blocker::OriginalMissing)?;
        (original, Some(path), current_path.map(|p| (p, current.clone())))
    } else {
        let previous = find_translation(&current, current_path.as_deref());
        (current.clone(), current_path, previous)
    };
    if source.frontmatter.wp_post_id.is_none() {
        return Err(Blocker::NotUploaded);
    }
    let source_site = source_site_of(&source);
    let targets: Vec<wpsite::SiteConfig> = wpsite::load_all().sites.into_iter().filter(|s| s.site_id() != source_site).collect();
    if previous.is_none() && targets.is_empty() {
        return Err(Blocker::NoSecondBlog);
    }
    Ok(Prep { source, source_path, previous, source_site, targets })
}

/// "Übersetzen …" for the open article: creates its translation or, if
/// there is one, updates it. Also works with the translation open - then
/// its original is looked up in the library.
pub fn open(window: &adw::ApplicationWindow, ctx: &DocContext) {
    let prep = match prepare(ctx) {
        Ok(prep) => prep,
        Err(Blocker::OriginalMissing) => {
            window::show_toast(&ctx.toast_overlay, &tr("Das Original dieser Übersetzung liegt nicht in der Bibliothek. Öffne es dort zuerst aus dem Blog."));
            return;
        }
        Err(Blocker::NotUploaded) => {
            window::show_toast(&ctx.toast_overlay, &tr("Erst hochladen: Übersetzt werden Beiträge, die im Blog liegen."));
            return;
        }
        Err(Blocker::NoSecondBlog) => {
            let alert = adw::AlertDialog::builder()
                .heading(tr("Kein zweites Blog"))
                .body(tr("Eine Übersetzung landet in einem anderen Blog. Lege es unter Einstellungen → WordPress an."))
                .build();
            alert.add_response("ok", &tr("OK"));
            alert.present(Some(window));
            return;
        }
    };
    let updating = prep.previous.is_some();
    // Fixed height: the progress bar and an error message appear below the
    // rows later and must not end up behind the dialog's lower edge.
    let dialog = adw::Dialog::builder().title(if updating { tr("Übersetzung aktualisieren") } else { tr("Übersetzung erstellen") }).content_width(560).content_height(600).build();
    let on_saved: Rc<dyn Fn()> = {
        let dialog = dialog.clone();
        Rc::new(move || {
            dialog.close();
        })
    };
    let form = build_form(prep, window, ctx, on_saved);
    let header = adw::HeaderBar::new();
    header.pack_end(&form.go);
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    group.add(&form.widget);
    page.add(&group);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(window));
}

/// "Selbst übersetzen": an empty translation next to the open original,
/// already tied to it and to the blog of `lang` - categories, tags and the
/// featured image taken over, the text left for you to write or paste.
/// Counts as reviewed: you wrote it. Works before the original is on its
/// blog too; the link gets its post id on upload (`fill_source_id`).
pub fn create_manual(ctx: &DocContext, lang: &str) -> Result<PathBuf, String> {
    worksave::flush(ctx, false);
    let path = ctx.current_path.borrow().clone().ok_or_else(|| tr("Der Artikel hat noch keine Datei."))?;
    if library::file_lang(&path) != Some(None) || !library::contains(&library::root(), &path) {
        return Err(tr("Selbst übersetzen geht vom Original in der Bibliothek aus."));
    }
    let target = library::sibling(&path, Some(lang)).ok_or_else(|| tr("Der Artikel hat noch keine Datei."))?;
    if target.exists() {
        return Ok(target);
    }
    let source = ctx.current_document();
    let site = wpsite::for_lang(Some(lang)).map(|s| s.site_id());
    let source_body = translate::body_with_uploaded_images(&source);
    let src = &source.frontmatter;
    let link = document::TranslationLink {
        lang: lang.to_string(),
        source_site: source_site_of(&source),
        source_id: src.wp_post_id.unwrap_or_default(),
        source_hash: syncstate::fingerprint(&source),
        source_sections: translate::split_sections(&source_body).iter().map(|s| translate::section_hash(s)).collect(),
        translated_at: today(),
        reviewed: true,
    };
    let frontmatter = document::Frontmatter {
        lang: Some(lang.to_string()),
        post_type: src.post_type,
        status: document::PostStatus::Draft,
        categories: translate::map_categories(&src.categories, &aiprompts::load_text_or(translate::CATEGORY_MAP_ID, "")),
        tags: src.tags.clone(),
        featured_image: src.featured_image.clone(),
        comment_status: src.comment_status,
        wp_site: site,
        translation: Some(link),
        ..document::Frontmatter::default()
    };
    document::write(&target, &Document { frontmatter, body: String::new() }).map_err(|err| err.to_string())?;
    Ok(target)
}

/// A translation made before its original was uploaded knows no post id
/// yet; once the original has one, it's filled in (before the upload that
/// sends it along as `lui_source_id`).
pub fn fill_source_id(ctx: &DocContext) {
    let path = ctx.current_path.borrow().clone();
    let missing = ctx.frontmatter.borrow().translation.as_ref().is_some_and(|t| t.source_id == 0);
    if !missing {
        return;
    }
    let Some((_, original)) = path.as_deref().and_then(|p| library::sibling(p, None)).and_then(|p| document::read(&p).ok().map(|d| (p, d))) else { return };
    if let (Some(id), Some(link)) = (original.frontmatter.wp_post_id, ctx.frontmatter.borrow_mut().translation.as_mut()) {
        link.source_id = id;
        link.source_site = source_site_of(&original);
    }
}

/// The start page shown in place of the editor when the language switch
/// (`langswitch.rs`) goes to a language that has no file yet: what will be
/// translated, and the button that does it - or why it can't happen yet.
pub fn start_page(window: &adw::ApplicationWindow, ctx: &DocContext, lang: &str) -> gtk4::Widget {
    let name = languages().into_iter().find(|(c, _)| *c == lang).map_or(lang.to_uppercase(), |(_, n)| n);
    let page = adw::StatusPage::builder()
        .icon_name("preferences-desktop-locale-symbolic")
        .title(tr("Noch keine Fassung auf {lang}").replace("{lang}", &name))
        .build();
    // Writing the translation yourself: an empty, linked file to paste into.
    let manual = gtk4::Button::builder().label(tr("Selbst übersetzen")).halign(gtk4::Align::Center).margin_top(12).build();
    manual.add_css_class("pill");
    manual.set_tooltip_text(Some(&tr("Legt die Fassung leer an - Text selbst schreiben oder einfügen")));
    {
        let ctx = ctx.clone();
        let lang = lang.to_string();
        manual.connect_clicked(move |_| match create_manual(&ctx, &lang) {
            Ok(path) => {
                window::open_document_at_path(path, &ctx);
                window::show_toast(&ctx.toast_overlay, &tr("Leere Fassung angelegt - Titel unter „Beitrag“, Text hier einfügen."));
            }
            Err(err) => window::show_toast(&ctx.toast_overlay, &err),
        });
    }
    let with_manual = |main: &gtk4::Widget| -> gtk4::Widget {
        let column = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        column.append(main);
        column.append(&manual);
        column.upcast()
    };
    match prepare(ctx) {
        Ok(prep) => {
            page.set_description(Some(&tr("Blocksatz übersetzt den Artikel abschnittweise. Danach liest du gegen und lädst die Fassung als Entwurf hoch.")));
            let form = build_form(prep, window, ctx, Rc::new(|| {}));
            form.go.add_css_class("pill");
            form.go.set_halign(gtk4::Align::Center);
            form.go.set_margin_top(18);
            let list = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
            list.append(&form.widget);
            list.append(&form.go);
            list.append(&manual);
            page.set_child(Some(&adw::Clamp::builder().maximum_size(560).child(&list).build()));
        }
        Err(Blocker::NotUploaded) => {
            page.set_description(Some(&tr("Übersetzt wird, sobald das Original als Entwurf im Blog liegt - die Übersetzung wird mit diesem Beitrag verknüpft.")));
            let upload = gtk4::Button::builder().label(tr("Original als Entwurf hochladen")).halign(gtk4::Align::Center).build();
            upload.add_css_class("pill");
            upload.add_css_class("suggested-action");
            upload.set_action_name(Some("main.upload-draft"));
            page.set_child(Some(&with_manual(upload.upcast_ref())));
        }
        Err(Blocker::NoSecondBlog) => {
            page.set_description(Some(&tr("Eine Übersetzung landet in einem anderen Blog. Lege es unter Einstellungen → WordPress an.")));
        }
        Err(Blocker::OriginalMissing) => {
            page.set_description(Some(&tr("Das Original dieser Übersetzung liegt nicht in der Bibliothek. Öffne es dort zuerst aus dem Blog.")));
        }
    }
    page.upcast()
}

struct Form {
    /// The rows, the progress bar and the status line.
    widget: gtk4::Widget,
    go: gtk4::Button,
}

/// Target, scope and model as rows, plus the button that translates and
/// opens the result. `on_saved` runs once the translation is written.
fn build_form(prep: Prep, window: &adw::ApplicationWindow, ctx: &DocContext, on_saved: Rc<dyn Fn()>) -> Form {
    let Prep { source, source_path, previous, source_site, targets } = prep;
    let source_body = translate::body_with_uploaded_images(&source);
    let sections = translate::split_sections(&source_body);
    let changed = previous.as_ref().and_then(|(_, p)| p.frontmatter.translation.clone()).map(|link| {
        sections.iter().filter(|s| !s.trim().is_empty() && !link.source_sections.contains(&translate::section_hash(s))).count()
    });
    let code_blocks = translate::mask(&source_body).originals.iter().filter(|(k, _)| *k == "CODE").count();

    let list = gtk4::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk4::SelectionMode::None);
    let site_row = adw::ComboRow::builder().title(tr("Ziel-Blog")).build();
    let lang_row = adw::ComboRow::builder().title(tr("Sprache")).build();
    let languages = languages();

    match &previous {
        Some((_, prev)) => {
            let link = prev.frontmatter.translation.clone().unwrap_or_default();
            let site = prev.frontmatter.wp_site.clone().unwrap_or_default();
            site_row.set_model(Some(&gtk4::StringList::new(&[site.as_str()])));
            site_row.set_sensitive(false);
            let name = languages.iter().find(|(c, _)| *c == link.lang).map_or(link.lang.clone(), |(_, n)| n.clone());
            lang_row.set_model(Some(&gtk4::StringList::new(&[name.as_str()])));
            lang_row.set_sensitive(false);
        }
        None => {
            let ids: Vec<String> = targets.iter().map(wpsite::SiteConfig::site_id).collect();
            site_row.set_model(Some(&gtk4::StringList::new(&ids.iter().map(String::as_str).collect::<Vec<_>>())));
            lang_row.set_model(Some(&gtk4::StringList::new(&languages.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>())));
            // The blog whose address names a language, and that language.
            if let Some((index, lang)) = targets.iter().enumerate().find_map(|(i, s)| wpsite::site_lang(s).map(|l| (i, l))) {
                site_row.set_selected(index as u32);
                if let Some(lang_index) = languages.iter().position(|(c, _)| *c == lang) {
                    lang_row.set_selected(lang_index as u32);
                }
            }
            // The target blog's language follows the blog.
            let languages = languages.clone();
            let targets = targets.clone();
            let lang_row = lang_row.clone();
            site_row.connect_selected_notify(move |row| {
                if let Some(lang) = targets.get(row.selected() as usize).and_then(wpsite::site_lang) {
                    if let Some(index) = languages.iter().position(|(c, _)| *c == lang) {
                        lang_row.set_selected(index as u32);
                    }
                }
            });
        }
    }
    list.append(&site_row);
    list.append(&lang_row);

    let scope = match changed {
        Some(0) => tr("Keine Abschnitte geändert – es wird nichts neu übersetzt."),
        Some(n) => tr("{n} von {total} Abschnitten geändert; nur sie werden neu übersetzt, deine Korrekturen in den übrigen bleiben.").replace("{n}", &n.to_string()).replace("{total}", &sections.len().to_string()),
        None => tr("{s} Abschnitte · {w} Wörter · {c} Code-Blöcke bleiben unverändert")
            .replace("{s}", &sections.iter().filter(|s| !s.trim().is_empty()).count().to_string())
            .replace("{w}", &word_count(&source_body).to_string())
            .replace("{c}", &code_blocks.to_string()),
    };
    list.append(&adw::ActionRow::builder().title(tr("Umfang")).subtitle(scope).subtitle_lines(3).build());
    if previous.is_none() {
        let category_map = aiprompts::load_text_or(translate::CATEGORY_MAP_ID, "");
        let categories = translate::map_categories(&source.frontmatter.categories, &category_map);
        list.append(&adw::ActionRow::builder().title(tr("Titel, Slug, Auszug")).subtitle(tr("werden übersetzt")).build());
        if !categories.is_empty() {
            list.append(&adw::ActionRow::builder().title(tr("Kategorien")).subtitle(categories.join(", ")).subtitle_lines(2).build());
        }
        if !source.frontmatter.tags.is_empty() {
            let tags = tr("{n} Schlagwörter, übersetzt und mit denen des Ziel-Blogs abgeglichen").replace("{n}", &source.frontmatter.tags.len().to_string());
            list.append(&adw::ActionRow::builder().title(tr("Schlagwörter")).subtitle(tags).subtitle_lines(2).build());
        }
    }

    let chat = crate::chatconfig::load_provider_config();
    let models: Vec<String> = aitasks::candidates(&aitasks::load_assignment(AiTask::Translation), &chat).iter().map(aitasks::ModelRef::label).collect();
    list.append(&adw::ActionRow::builder().title(tr("Modell")).subtitle(models.join(" → ")).build());

    let progress = gtk4::ProgressBar::builder().show_text(true).visible(false).margin_top(12).build();
    let status = gtk4::Label::builder().wrap(true).xalign(0.0).visible(false).margin_top(6).build();
    status.add_css_class("dim-label");
    let widget = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    widget.append(&list);
    widget.append(&progress);
    widget.append(&status);

    let go = gtk4::Button::with_label(&if previous.is_some() { tr("Aktualisieren") } else { tr("Übersetzen") });
    go.add_css_class("suggested-action");
    go.set_sensitive(changed != Some(0));

    let window_weak = window.downgrade();
    let ctx = ctx.clone();
    let targets = Rc::new(targets);
    let previous = Rc::new(previous);
    let source = Rc::new(source);
    go.connect_clicked(move |go| {
        let (target_site, target_url) = match previous.as_ref() {
            Some((_, prev)) => {
                let id = prev.frontmatter.wp_site.clone().unwrap_or_default();
                (id.clone(), wpsite::for_site_id(Some(&id)))
            }
            None => targets.get(site_row.selected() as usize).map(|s| (s.site_id(), s.clone())).unwrap_or_default(),
        };
        let target_lang = match previous.as_ref() {
            Some((_, prev)) => prev.frontmatter.translation.as_ref().map(|t| t.lang.clone()).unwrap_or_else(|| "en".into()),
            None => languages.get(lang_row.selected() as usize).map(|(c, _)| c.to_string()).unwrap_or_else(|| "en".into()),
        };
        let mut opts = Options {
            source_lang: "de".into(),
            target_lang,
            source_site: source_site.clone(),
            today: today(),
            translate_tags: true,
            category_map: aiprompts::load_text_or(translate::CATEGORY_MAP_ID, ""),
            known_tags: Vec::new(),
            known_categories: Vec::new(),
        };
        let system = aiprompts::load_text_or(translate::PROMPT_ID, translate::DEFAULT_PROMPT);

        go.set_sensitive(false);
        progress.set_visible(true);
        progress.set_fraction(0.0);
        progress.set_text(Some(&tr("Wird übersetzt …")));
        status.set_visible(false);

        let (tx, rx) = mpsc::channel::<Msg>();
        let source_doc = (*source).clone();
        let previous_doc = previous.as_ref().as_ref().map(|(_, d)| d.clone());
        let source_url = wpsite::load_all().sites.into_iter().find(|s| s.site_id() == source_site).map(|s| s.url);
        std::thread::spawn(move || {
            // The target blog's terms, so translated ones match existing
            // spellings. Without them (offline, no password) it still works.
            if previous_doc.is_none() && !target_url.url.is_empty() {
                if let Ok(Some(password)) = futures_lite::future::block_on(crate::secrets::load_app_password(&target_url.url, &target_url.username)) {
                    let client = wpclient::Client::new(&target_url.url, &target_url.username, &password);
                    opts.known_tags = client.list_term_names("tags").unwrap_or_default();
                    opts.known_categories = client.list_term_names("categories").unwrap_or_default();
                }
            }
            let progress_tx = tx.clone();
            let result = aitasks::run(AiTask::Translation, |client| {
                let api_error: RefCell<Option<llm::ApiError>> = RefCell::new(None);
                let send = |system: &str, history: &[ChatMessage]| {
                    client.send_with(system, history, &llm::SendOptions::bulk_text()).map_err(|err| {
                        let message = err.message.clone();
                        *api_error.borrow_mut() = Some(err);
                        message
                    })
                };
                let report = |done: usize, total: usize| {
                    let _ = progress_tx.send(Msg::Progress(done, total));
                };
                translate::translate(&source_doc, previous_doc.as_ref(), &opts, &system, &send, &report)
                    .map_err(|message| api_error.take().unwrap_or(llm::ApiError { message, status: llm::ModelStatus::Other }))
            });
            // An original imported from the blog has its featured image
            // only as a media id of that blog. The translation gets the
            // file URL instead, which the upload then copies into the
            // target blog like any other featured image.
            let result = result.map(|mut outcome| {
                let fm = &mut outcome.value.document.frontmatter;
                if previous_doc.is_none() && fm.featured_image.is_none() {
                    if let (Some(id), Some(url)) = (source_doc.frontmatter.featured_media_id, source_url.as_deref()) {
                        fm.featured_image = wpclient::public_media_url(url, id).ok();
                    }
                }
                outcome
            });
            let _ = tx.send(Msg::Done(Box::new(result)));
        });

        let go = go.clone();
        let progress = progress.clone();
        let status = status.clone();
        let ctx = ctx.clone();
        let window_weak = window_weak.clone();
        let previous = previous.clone();
        let source_path = source_path.clone();
        let on_saved = on_saved.clone();
        glib::timeout_add_local(Duration::from_millis(150), move || loop {
            match rx.try_recv() {
                Ok(Msg::Progress(done, total)) => {
                    if total > 0 {
                        progress.set_fraction(done as f64 / total as f64);
                        progress.set_text(Some(&tr("{done} von {total}").replace("{done}", &done.to_string()).replace("{total}", &total.to_string())));
                    }
                }
                Ok(Msg::Done(result)) => {
                    match aitasks::deliver(*result) {
                        Ok(outcome) => {
                            let target = previous.as_ref().as_ref().map(|(p, _)| p.clone());
                            match save(outcome, target, source_path.as_deref(), &target_site) {
                                Ok((path, issues, summary)) => {
                                    on_saved();
                                    window::open_document_at_path(path, &ctx);
                                    window::show_toast(&ctx.toast_overlay, &summary);
                                    if let Some(window) = window_weak.upgrade() {
                                        open_review_with(&window, &ctx, issues);
                                    }
                                }
                                Err(err) => {
                                    status.set_label(&tr("Speichern fehlgeschlagen: {err}").replace("{err}", &err.to_string()));
                                    status.set_visible(true);
                                    go.set_sensitive(true);
                                }
                            }
                        }
                        Err(err) => {
                            progress.set_visible(false);
                            status.set_label(&err);
                            status.set_visible(true);
                            go.set_sensitive(true);
                        }
                    }
                    return glib::ControlFlow::Break;
                }
                Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    status.set_label(&tr("Interner Fehler: Übersetzung hat kein Ergebnis geliefert."));
                    status.set_visible(true);
                    go.set_sensitive(true);
                    return glib::ControlFlow::Break;
                }
            }
        });
    });
    Form { widget: widget.upcast(), go }
}

/// A new library folder for a translation whose original lives outside
/// the library, with copies of the images that aren't uploaded yet.
fn new_folder_with_media(document: &Document, source_path: Option<&Path>) -> std::io::Result<PathBuf> {
    let path = library::create_entry(&library::root(), Some(&document.frontmatter.title), &library::untitled_name())?;
    if let (Some(from_dir), Some(to_dir)) = (source_path.and_then(Path::parent), path.parent()) {
        let mut files: Vec<String> = Vec::new();
        if let Some(source) = source_path.and_then(|p| document::read(p).ok()) {
            files.extend(translate::local_media(&source));
        }
        files.extend(document.frontmatter.featured_image.iter().filter(|f| !f.contains("://")).cloned());
        for file in files {
            let from = from_dir.join(&file);
            if from.is_file() {
                if let Some(parent) = to_dir.join(&file).parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(&from, to_dir.join(&file))?;
            }
        }
    }
    Ok(path)
}

/// Writes the result: into the existing translation, else next to the
/// original as `artikel.<lang>.md` - or, for an original outside the
/// library, into a new library folder with copies of the images that
/// aren't uploaded yet.
fn save(outcome: Outcome, existing: Option<PathBuf>, source_path: Option<&Path>, target_site: &str) -> std::io::Result<(PathBuf, Vec<Issue>, String)> {
    let Outcome { mut document, issues, translated_sections, reused_sections } = outcome;
    let lang = document.frontmatter.translation.as_ref().map(|t| t.lang.clone()).filter(|l| !l.is_empty());
    if document.frontmatter.lang.is_none() {
        document.frontmatter.lang = lang.clone();
    }
    let root = library::root();
    let path = match existing {
        Some(path) => path,
        None => {
            document.frontmatter.wp_site = Some(target_site.to_string());
            match source_path.filter(|p| library::contains(&root, p) && library::file_lang(p) == Some(None)) {
                Some(original) => library::sibling(original, Some(lang.as_deref().unwrap_or("en"))).expect("library file has a folder"),
                None => new_folder_with_media(&document, source_path)?,
            }
        }
    };
    document::write(&path, &document)?;
    let summary = if reused_sections > 0 {
        tr("{n} Abschnitte übersetzt, {k} übernommen. Jetzt gegenlesen.").replace("{n}", &translated_sections.to_string()).replace("{k}", &reused_sections.to_string())
    } else {
        tr("{n} Abschnitte übersetzt. Jetzt gegenlesen.").replace("{n}", &translated_sections.to_string())
    };
    Ok((path, issues, summary))
}

/// "Gegenlesen …" for the open translation: original and translation side
/// by side, section by section, the checks on top, and the switch that
/// marks it as reviewed.
pub fn open_review(window: &adw::ApplicationWindow, ctx: &DocContext) {
    open_review_with(window, ctx, Vec::new());
}

fn open_review_with(window: &adw::ApplicationWindow, ctx: &DocContext, extra_issues: Vec<Issue>) {
    worksave::flush(ctx, false);
    let translation = ctx.current_document();
    let Some(link) = translation.frontmatter.translation.clone() else {
        window::show_toast(&ctx.toast_overlay, &tr("Das ist keine Übersetzung."));
        return;
    };
    let path = ctx.current_path.borrow().clone();
    let Some((_, original)) = find_original(&translation, path.as_deref()) else {
        window::show_toast(&ctx.toast_overlay, &tr("Das Original dieser Übersetzung liegt nicht in der Bibliothek. Öffne es dort zuerst aus dem Blog."));
        return;
    };

    let source_body = translate::body_with_uploaded_images(&original);
    let mut issues = extra_issues;
    for issue in translate::check(&source_body, &translation.body, "de") {
        if !issues.contains(&issue) {
            issues.push(issue);
        }
    }
    let stale = syncstate::fingerprint(&original) != link.source_hash;

    let checks = adw::PreferencesGroup::builder().title(tr("Prüfungen")).build();
    if stale {
        let row = adw::ActionRow::builder().title(tr("Original geändert")).subtitle(tr("Das Original wurde seit der Übersetzung bearbeitet – „Übersetzen …“ übernimmt die Änderungen.")).build();
        row.add_prefix(&gtk4::Image::from_icon_name("dialog-warning-symbolic"));
        checks.add(&row);
    }
    if issues.is_empty() {
        let row = adw::ActionRow::builder().title(tr("Keine Auffälligkeiten")).subtitle(tr("Code, Links, Auszeichnungen und Überschriften stimmen mit dem Original überein.")).build();
        row.add_prefix(&gtk4::Image::from_icon_name("emblem-ok-symbolic"));
        checks.add(&row);
    }
    for issue in &issues {
        let title = match issue.section {
            Some(i) => tr("Abschnitt {n}").replace("{n}", &(i + 1).to_string()),
            None => tr("Ganzer Artikel"),
        };
        let row = adw::ActionRow::builder().title(title).subtitle(&issue.message).subtitle_lines(4).build();
        row.add_prefix(&gtk4::Image::from_icon_name("dialog-warning-symbolic"));
        checks.add(&row);
    }

    let grid = gtk4::Grid::builder().column_spacing(18).row_spacing(6).column_homogeneous(true).margin_start(12).margin_end(12).margin_bottom(18).build();
    let heading = |text: String| {
        let label = gtk4::Label::builder().label(text).xalign(0.0).build();
        label.add_css_class("heading");
        label
    };
    grid.attach(&heading(tr("Original")), 0, 0, 1, 1);
    grid.attach(&heading(tr("Übersetzung")), 1, 0, 1, 1);
    let left = translate::split_sections(&source_body);
    let right = translate::split_sections(&translation.body);
    let cell = |text: &str| {
        let label = gtk4::Label::builder()
            .label(text.trim())
            .xalign(0.0)
            .yalign(0.0)
            .wrap(true)
            .wrap_mode(gtk4::pango::WrapMode::WordChar)
            .selectable(true)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        label.add_css_class("monospace");
        let card = gtk4::Box::builder().margin_top(4).build();
        card.add_css_class("card");
        card.append(&label);
        card
    };
    for i in 0..left.len().max(right.len()) {
        let changed = left.get(i).is_some_and(|s| !link.source_sections.contains(&translate::section_hash(s)));
        let caption = if changed {
            tr("Abschnitt {n} · Original geändert").replace("{n}", &(i + 1).to_string())
        } else {
            tr("Abschnitt {n}").replace("{n}", &(i + 1).to_string())
        };
        let section_label = gtk4::Label::builder().label(caption).xalign(0.0).margin_top(12).build();
        section_label.add_css_class("dim-label");
        section_label.add_css_class("caption");
        let row = (i as i32) * 2 + 1;
        grid.attach(&section_label, 0, row, 2, 1);
        grid.attach(&cell(left.get(i).map(String::as_str).unwrap_or("")), 0, row + 1, 1, 1);
        grid.attach(&cell(right.get(i).map(String::as_str).unwrap_or("")), 1, row + 1, 1, 1);
    }

    // Not a PreferencesPage: its clamp would squeeze the two columns into
    // half of a 600 px strip. The checks stay readable-width, the sections
    // use the whole dialog.
    let content = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(12).margin_top(18).margin_bottom(18).build();
    content.append(&adw::Clamp::builder().maximum_size(900).child(&checks).build());
    let sections_title = gtk4::Label::builder().label(tr("Abschnitte")).xalign(0.0).margin_start(12).margin_top(12).build();
    sections_title.add_css_class("heading");
    let sections_hint = gtk4::Label::builder().label(tr("Korrekturen machst du im Editor; dieser Dialog zeigt den gespeicherten Stand.")).xalign(0.0).wrap(true).margin_start(12).build();
    sections_hint.add_css_class("dim-label");
    content.append(&sections_title);
    content.append(&sections_hint);
    content.append(&grid);
    let page = gtk4::ScrolledWindow::builder().child(&content).hscrollbar_policy(gtk4::PolicyType::Never).vexpand(true).build();

    let reviewed = link.reviewed;
    let toggle = gtk4::Button::with_label(&if reviewed { tr("Markierung entfernen") } else { tr("Als gegengelesen markieren") });
    if !reviewed {
        toggle.add_css_class("suggested-action");
    }
    let header = adw::HeaderBar::new();
    header.pack_end(&toggle);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    let dialog = adw::Dialog::builder().title(tr("Gegenlesen")).content_width(1100).content_height(820).child(&toolbar).build();
    {
        let dialog = dialog.clone();
        let ctx = ctx.clone();
        toggle.connect_clicked(move |_| {
            if let Some(link) = ctx.frontmatter.borrow_mut().translation.as_mut() {
                link.reviewed = !reviewed;
            }
            worksave::flush(&ctx, true);
            ctx.notify_library(false);
            let message = if reviewed { tr("Markierung „gegengelesen“ entfernt.") } else { tr("Als gegengelesen markiert. Beim nächsten Hochladen wird die Übersetzung im Blog verknüpft.") };
            window::show_toast(&ctx.toast_overlay, &message);
            dialog.close();
        });
    }
    dialog.present(Some(window));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One real translation through the configured model (costs a few
    /// cents, needs an API key in the keyring), written nowhere but to
    /// `BLOCKSATZ_LIVE_OUT`:
    /// `BLOCKSATZ_LIVE_ARTICLE=…/artikel.md BLOCKSATZ_PROMPT_FILE=… BLOCKSATZ_LIVE_OUT=… cargo test live_translation -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_translation() {
        let article = std::env::var("BLOCKSATZ_LIVE_ARTICLE").expect("BLOCKSATZ_LIVE_ARTICLE");
        let source = document::read(Path::new(&article)).expect("article");
        let system = std::env::var("BLOCKSATZ_PROMPT_FILE").ok().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_else(|| translate::DEFAULT_PROMPT.to_string());
        let opts = Options { source_lang: "de".into(), target_lang: "en".into(), source_site: source_site_of(&source), today: "2026-10-03".into(), translate_tags: true, category_map: "Allgemein = General".into(), known_tags: Vec::new(), known_categories: Vec::new() };
        let routed = aitasks::run(AiTask::Translation, |client| {
            let send = |system: &str, history: &[ChatMessage]| client.send_with(system, history, &llm::SendOptions::bulk_text()).map_err(|e| e.message);
            translate::translate(&source, None, &opts, &system, &send, &|d, t| eprintln!("{d}/{t}")).map_err(|message| llm::ApiError { message, status: llm::ModelStatus::Other })
        })
        .expect("translation");
        if let Some(notice) = &routed.notice {
            eprintln!("Hinweis: {notice}");
        }
        let out = routed.value;
        for issue in &out.issues {
            eprintln!("Befund {:?}: {}", issue.section, issue.message);
        }
        std::fs::write(std::env::var("BLOCKSATZ_LIVE_OUT").expect("BLOCKSATZ_LIVE_OUT"), document::serialize(&out.document)).unwrap();
    }
}
