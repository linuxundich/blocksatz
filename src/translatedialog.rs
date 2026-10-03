//! "Übersetzen …" and "Gegenlesen …" (`docs/translations.md`): the dialogs
//! around `translate.rs`.
//!
//! A translation is an ordinary working copy in a library folder of its
//! own, tied to the other blog through `Frontmatter::wp_site` and to its
//! original through `Frontmatter::translation`. Everything else - autosave,
//! upload, sync state - works on it like on any other article.

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
use crate::{aiprompts, library, syncstate, worksave, wpsite};

/// Target languages offered for a new translation: (code, display name).
fn languages() -> Vec<(&'static str, String)> {
    vec![("en", tr("Englisch")), ("fr", tr("Französisch")), ("es", tr("Spanisch")), ("it", tr("Italienisch")), ("nl", tr("Niederländisch"))]
}

fn source_site_of(doc: &Document) -> String {
    doc.frontmatter.wp_site.clone().unwrap_or_else(|| wpsite::load().site_id())
}

/// The library working copy translating `original`, if there is one.
pub fn find_translation(original: &Document) -> Option<(PathBuf, Document)> {
    let post_id = original.frontmatter.wp_post_id?;
    let site = source_site_of(original);
    library::scan(&library::root())
        .into_iter()
        .find(|entry| entry.document.frontmatter.translation.as_ref().is_some_and(|t| t.source_id == post_id && t.source_site == site))
        .map(|entry| (entry.path, entry.document))
}

/// The library working copy a translation was made from, if it's there.
pub fn find_original(translation: &Document) -> Option<(PathBuf, Document)> {
    let link = translation.frontmatter.translation.as_ref()?;
    let path = library::find_by_post_id(&library::root(), &link.source_site, link.source_id)?;
    let doc = document::read(&path).ok()?;
    Some((path, doc))
}

/// Whether the original changed since `translation` was made - `None` when
/// it isn't a translation or the original isn't in the library.
pub fn original_changed(translation: &Document) -> Option<bool> {
    let link = translation.frontmatter.translation.as_ref()?;
    let (_, original) = find_original(translation)?;
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

/// "Übersetzen …" for the open article: creates its translation or, if
/// there is one, updates it. Also works with the translation open - then
/// its original is looked up in the library.
pub fn open(window: &adw::ApplicationWindow, ctx: &DocContext) {
    worksave::flush(ctx, false);
    let current = ctx.current_document();
    let current_path = ctx.current_path.borrow().clone();

    // (original, its path, previous translation and its path)
    let (source, source_path, previous) = if current.frontmatter.translation.is_some() {
        match find_original(&current) {
            Some((path, original)) => (original, Some(path), current_path.map(|p| (p, current.clone()))),
            None => {
                window::show_toast(&ctx.toast_overlay, &tr("Das Original dieser Übersetzung liegt nicht in der Bibliothek. Öffne es dort zuerst aus dem Blog."));
                return;
            }
        }
    } else {
        let previous = find_translation(&current);
        (current.clone(), current_path, previous)
    };

    if source.frontmatter.wp_post_id.is_none() {
        window::show_toast(&ctx.toast_overlay, &tr("Erst hochladen: Übersetzt werden Beiträge, die im Blog liegen."));
        return;
    }
    let source_site = source_site_of(&source);
    let targets: Vec<wpsite::SiteConfig> = wpsite::load_all().sites.into_iter().filter(|s| s.site_id() != source_site).collect();
    if previous.is_none() && targets.is_empty() {
        let alert = adw::AlertDialog::builder()
            .heading(tr("Kein zweites Blog"))
            .body(tr("Eine Übersetzung landet in einem anderen Blog. Lege es unter Einstellungen → WordPress an."))
            .build();
        alert.add_response("ok", &tr("OK"));
        alert.present(Some(window));
        return;
    }

    let source_body = translate::body_with_uploaded_images(&source);
    let sections = translate::split_sections(&source_body);
    let changed = previous.as_ref().and_then(|(_, p)| p.frontmatter.translation.clone()).map(|link| {
        sections.iter().filter(|s| !s.trim().is_empty() && !link.source_sections.contains(&translate::section_hash(s))).count()
    });
    let code_blocks = translate::mask(&source_body).originals.iter().filter(|(k, _)| *k == "CODE").count();

    let group = adw::PreferencesGroup::new();
    let site_row = adw::ComboRow::builder().title(tr("Ziel-Blog")).build();
    let lang_row = adw::ComboRow::builder().title(tr("Sprache")).build();
    let tags_row = adw::SwitchRow::builder().title(tr("Schlagwörter übersetzen")).subtitle(tr("Sonst bleiben sie leer und lassen sich im Ziel-Blog setzen.")).build();
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
        }
    }
    group.add(&site_row);
    group.add(&lang_row);
    if previous.is_none() {
        group.add(&tags_row);
    }

    let scope = match changed {
        Some(0) => tr("Keine Abschnitte geändert – es wird nichts neu übersetzt."),
        Some(n) => tr("{n} von {total} Abschnitten geändert; nur sie werden neu übersetzt, deine Korrekturen in den übrigen bleiben.").replace("{n}", &n.to_string()).replace("{total}", &sections.len().to_string()),
        None => tr("{s} Abschnitte · {w} Wörter · {c} Code-Blöcke bleiben unverändert")
            .replace("{s}", &sections.iter().filter(|s| !s.trim().is_empty()).count().to_string())
            .replace("{w}", &word_count(&source_body).to_string())
            .replace("{c}", &code_blocks.to_string()),
    };
    let scope_row = adw::ActionRow::builder().title(tr("Umfang")).subtitle(scope).subtitle_lines(3).build();
    group.add(&scope_row);

    let chat = crate::chatconfig::load_provider_config();
    let models: Vec<String> = aitasks::candidates(&aitasks::load_assignment(AiTask::Translation), &chat).iter().map(aitasks::ModelRef::label).collect();
    let model_row = adw::ActionRow::builder().title(tr("Modell")).subtitle(models.join(" → ")).build();
    group.add(&model_row);

    let progress = gtk4::ProgressBar::builder().show_text(true).visible(false).margin_top(12).build();
    let status = gtk4::Label::builder().wrap(true).xalign(0.0).visible(false).margin_top(6).build();
    status.add_css_class("dim-label");

    let page = adw::PreferencesPage::new();
    page.add(&group);
    let extra = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    extra.set_margin_start(12);
    extra.set_margin_end(12);
    extra.append(&progress);
    extra.append(&status);
    let progress_group = adw::PreferencesGroup::new();
    progress_group.add(&extra);
    page.add(&progress_group);

    let title = if previous.is_some() { tr("Übersetzung aktualisieren") } else { tr("Übersetzung erstellen") };
    let go = gtk4::Button::with_label(&if previous.is_some() { tr("Aktualisieren") } else { tr("Übersetzen") });
    go.add_css_class("suggested-action");
    go.set_sensitive(changed != Some(0));
    let header = adw::HeaderBar::new();
    header.pack_end(&go);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    // Fixed height: the progress bar and an error message appear below the
    // rows later and must not end up behind the dialog's lower edge.
    let dialog = adw::Dialog::builder().title(title).content_width(560).content_height(560).child(&toolbar).build();

    let window_weak = window.downgrade();
    let ctx = ctx.clone();
    let targets = Rc::new(targets);
    let previous = Rc::new(previous);
    let source = Rc::new(source);
    {
        let dialog = dialog.clone();
        go.connect_clicked(move |go| {
            let target_site = match previous.as_ref() {
                Some((_, prev)) => prev.frontmatter.wp_site.clone().unwrap_or_default(),
                None => targets.get(site_row.selected() as usize).map(wpsite::SiteConfig::site_id).unwrap_or_default(),
            };
            let target_lang = match previous.as_ref() {
                Some((_, prev)) => prev.frontmatter.translation.as_ref().map(|t| t.lang.clone()).unwrap_or_else(|| "en".into()),
                None => languages.get(lang_row.selected() as usize).map(|(c, _)| c.to_string()).unwrap_or_else(|| "en".into()),
            };
            let opts = Options {
                source_lang: "de".into(),
                target_lang,
                source_site: source_site.clone(),
                today: today(),
                translate_tags: tags_row.is_active(),
                category_map: aiprompts::load_text_or(translate::CATEGORY_MAP_ID, ""),
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
            std::thread::spawn(move || {
                let progress_tx = tx.clone();
                let result = aitasks::run(AiTask::Translation, |client| {
                    let api_error: RefCell<Option<llm::ApiError>> = RefCell::new(None);
                    let send = |system: &str, history: &[ChatMessage]| {
                        client.send(system, history).map_err(|err| {
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
                let _ = tx.send(Msg::Done(Box::new(result)));
            });

            let dialog = dialog.clone();
            let go = go.clone();
            let progress = progress.clone();
            let status = status.clone();
            let ctx = ctx.clone();
            let window_weak = window_weak.clone();
            let previous = previous.clone();
            let source_path = source_path.clone();
            let target_site = target_site.clone();
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
                                        dialog.close();
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
    }
    dialog.present(Some(window));
}

/// Writes the result: into the existing translation, or into a new
/// library folder (with copies of the images that aren't uploaded yet).
fn save(outcome: Outcome, existing: Option<PathBuf>, source_path: Option<&Path>, target_site: &str) -> std::io::Result<(PathBuf, Vec<Issue>, String)> {
    let Outcome { mut document, issues, translated_sections, reused_sections } = outcome;
    let path = match existing {
        Some(path) => path,
        None => {
            document.frontmatter.wp_site = Some(target_site.to_string());
            let path = library::create_entry(&library::root(), Some(&document.frontmatter.title), &library::untitled_name())?;
            if let (Some(from_dir), Some(to_dir)) = (source_path.and_then(Path::parent), path.parent()) {
                let mut files: Vec<String> = Vec::new();
                if let Ok(source) = document::read(source_path.expect("checked above")) {
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
            path
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
    let Some((_, original)) = find_original(&translation) else {
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
        let opts = Options { source_lang: "de".into(), target_lang: "en".into(), source_site: source_site_of(&source), today: "2026-10-03".into(), translate_tags: true, category_map: "Allgemein = General".into() };
        let routed = aitasks::run(AiTask::Translation, |client| {
            let send = |system: &str, history: &[ChatMessage]| client.send(system, history).map_err(|e| e.message);
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
