//! "KI-Artikel schreiben": drafts a whole article from a topic/brief with
//! whichever LLM provider is active in the KI-Chat settings - optionally in
//! the author's own voice, learned from up to `MAX_STYLE_SAMPLES` of their
//! own published WordPress posts, explicitly picked by title (not just
//! "however many of the most recent ones" - the Quill macOS app this
//! feature is modeled on, `site/images/ai-writing.png`/
//! `SamplePostPickerSheet.swift` in its source, lets the user choose which
//! specific posts represent their voice, since the most recent ones aren't
//! always the most representative), sent along as style samples (only for
//! style - the prompt tells the model not to reuse their content).
//!
//! Nothing is written into the editor until the user has seen the result:
//! the generated Markdown lands in an editable text view first, and only
//! "Als neues Dokument" / "An Cursor einfügen" hand it to the caller -
//! the same "review before anything is written" rule `aialt.rs` and
//! `tagsuggest.rs` already follow.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk4::glib;

use crate::i18n::tr;
use crate::{chatconfig, llm, secrets, wpclient, wpsite};

/// How much of each style-sample post is sent - enough to carry tone,
/// sentence rhythm and typical structure, without blowing past a small
/// local (Ollama) model's context window with five long articles.
const SAMPLE_CHAR_LIMIT: usize = 3000;
pub const MAX_STYLE_SAMPLES: u32 = 5;

const SYSTEM_PROMPT: &str = "Du bist ein erfahrener Blog-Autor und schreibst vollständige, \
     veröffentlichungsreife Blogartikel auf Deutsch in Markdown. Beginne mit genau einer \
     Überschrift erster Ebene (\"# Titel\") als Artikeltitel, danach folgt der Text mit \
     sinnvollen Zwischenüberschriften (\"##\"), Absätzen und - wo passend - Listen oder \
     Codeblöcken. Erfinde keine Fakten, Zahlen, Zitate oder Links; wo dir Informationen \
     fehlen, formuliere allgemein oder markiere die Stelle mit [TODO: ...]. Antworte \
     ausschließlich mit dem Artikel selbst - keine Einleitung wie \"Hier ist der Artikel\", \
     keine Erklärung danach, kein umschließender Codeblock.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Length {
    Short,
    Medium,
    Long,
}

impl Length {
    const ALL: [Length; 3] = [Length::Short, Length::Medium, Length::Long];

    fn label(&self) -> String {
        match self {
            Length::Short => tr("Kurz (ca. 400 Wörter)"),
            Length::Medium => tr("Mittel (ca. 800 Wörter)"),
            Length::Long => tr("Lang (ca. 1500 Wörter)"),
        }
    }

    fn words(&self) -> u32 {
        match self {
            Length::Short => 400,
            Length::Medium => 800,
            Length::Long => 1500,
        }
    }
}

/// What the dialog hands back: the title (from the model's leading `#`
/// heading, empty if it didn't write one) and the body without it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedArticle {
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyMode {
    NewDocument,
    InsertAtCursor,
}

/// Cuts `text` to at most `limit` characters, at a paragraph or at least a
/// word boundary where one is reasonably close, marking the cut with "…".
fn truncate_sample(text: &str, limit: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let cut: String = text.chars().take(limit).collect();
    let boundary = cut.rfind("\n\n").filter(|&i| i > limit / 2).or_else(|| cut.rfind(' ')).unwrap_or(cut.len());
    format!("{} …", cut[..boundary].trim_end())
}

pub(crate) fn build_prompt(brief: &str, length: Length, samples: &[String]) -> String {
    let mut prompt = String::new();
    if !samples.is_empty() {
        prompt.push_str(
            "Die folgenden Texte sind Beispiele aus meinem eigenen Blog. Übernimm ihren \
             Schreibstil - Tonfall, Anrede, Satzlänge, Wortwahl, typischen Aufbau -, aber \
             keine Inhalte daraus.\n\n",
        );
        for (index, sample) in samples.iter().enumerate() {
            prompt.push_str(&format!("--- Beispiel {} ---\n{}\n\n", index + 1, sample));
        }
        prompt.push_str("--- Ende der Beispiele ---\n\n");
    }
    prompt.push_str(&format!("Schreibe einen Blogartikel mit etwa {} Wörtern.\n\nThema und Anweisungen:\n{}\n", length.words(), brief.trim()));
    prompt
}

/// Splits the model's reply into title + body: drops a wrapping ```` ``` ````
/// fence some models add despite being told not to, then takes a leading
/// `# ` heading as the title.
pub(crate) fn parse_generated(reply: &str) -> GeneratedArticle {
    let mut text = reply.trim();
    if text.starts_with("```") && text.ends_with("```") && text.len() > 6 {
        text = text[3..text.len() - 3].trim();
        // The fence's own language tag ("markdown", "md") on its first line.
        if let Some((first, rest)) = text.split_once('\n') {
            if !first.contains(' ') && !first.starts_with('#') {
                text = rest.trim();
            }
        }
    }
    let (first_line, rest) = text.split_once('\n').unwrap_or((text, ""));
    match first_line.trim().strip_prefix("# ") {
        Some(title) => GeneratedArticle { title: title.trim().to_string(), body: format!("{}\n", rest.trim()) },
        None => GeneratedArticle { title: String::new(), body: format!("{text}\n") },
    }
}

/// Connects and fetches every published post's id/title, for
/// `open_sample_picker` - not the samples' content itself, which
/// `fetch_style_samples` only fetches for whichever ids end up picked.
fn fetch_publishable_posts() -> Result<Vec<wpclient::PostSummary>, String> {
    let site = wpsite::load();
    if site.url.is_empty() {
        return Err(tr("Für den eigenen Schreibstil muss eine WordPress-Verbindung eingerichtet sein."));
    }
    let password = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
        .map_err(|err| err.to_string())?
        .ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden."))?;
    let client = wpclient::Client::new(&site.url, &site.username, &password);
    Ok(client.list_posts().map_err(|err| err.to_string())?.into_iter().filter(|p| p.status == "publish").collect())
}

/// Fetches `ids`' posts' bodies as Markdown, for use as style samples -
/// explicitly picked ones (`open_sample_picker`), not just "however many
/// of the most recent published posts". Runs on a background thread.
fn fetch_style_samples(ids: &[u64]) -> Result<Vec<String>, String> {
    let site = wpsite::load();
    if site.url.is_empty() {
        return Err(tr("Für den eigenen Schreibstil muss eine WordPress-Verbindung eingerichtet sein."));
    }
    let password = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
        .map_err(|err| err.to_string())?
        .ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden."))?;
    let client = wpclient::Client::new(&site.url, &site.username, &password);
    let mut samples = Vec::new();
    for &id in ids {
        let detail = client.get_item("posts", id).map_err(|err| err.to_string())?;
        let markdown = gutenberg::gutenberg_to_markdown(&detail.content);
        if !markdown.trim().is_empty() {
            samples.push(truncate_sample(&format!("# {}\n\n{}", detail.title, markdown), SAMPLE_CHAR_LIMIT));
        }
    }
    Ok(samples)
}

/// Opens the "Beispielartikel auswählen" sheet - a checklist of every
/// publishable post, capped at `MAX_STYLE_SAMPLES` selected at once (ticking
/// a further one past the cap is simply refused, same "disable the rest
/// once at the limit" approach `tagsuggest.rs`'s own suggestion checklist
/// already uses, rather than reverting a just-set toggle back off, which
/// would refire its own "toggled" signal for no reason). `on_done` fires
/// once, when "Fertig" is clicked - `selected_ids` itself already holds
/// whatever was picked, there's nothing to hand back separately.
fn open_sample_picker(parent: &adw::Dialog, posts: &[wpclient::PostSummary], selected_ids: Rc<RefCell<Vec<u64>>>, on_done: impl Fn() + 'static) {
    let list_box = gtk4::ListBox::new();
    list_box.set_selection_mode(gtk4::SelectionMode::None);
    list_box.add_css_class("boxed-list");

    let limit_label = gtk4::Label::builder().xalign(0.0).label(tr("Maximal {max} Beispielartikel.").replace("{max}", &MAX_STYLE_SAMPLES.to_string())).build();
    limit_label.add_css_class("caption");
    limit_label.add_css_class("dim-label");

    let checks: Rc<RefCell<Vec<(gtk4::CheckButton, u64)>>> = Rc::new(RefCell::new(Vec::new()));

    let update_sensitivities: Rc<dyn Fn()> = {
        let checks = checks.clone();
        let selected_ids = selected_ids.clone();
        let limit_label = limit_label.clone();
        Rc::new(move || {
            let count = selected_ids.borrow().len();
            let at_limit = count >= MAX_STYLE_SAMPLES as usize;
            limit_label.set_visible(at_limit);
            for (check, _) in checks.borrow().iter() {
                if !check.is_active() {
                    check.set_sensitive(!at_limit);
                }
            }
        })
    };

    for post in posts {
        let title = if post.title.trim().is_empty() { tr("(Ohne Titel)") } else { post.title.clone() };
        let check = gtk4::CheckButton::builder().label(&title).active(selected_ids.borrow().contains(&post.id)).build();
        let post_id = post.id;
        {
            let selected_ids = selected_ids.clone();
            let update_sensitivities = update_sensitivities.clone();
            check.connect_toggled(move |check| {
                let mut ids = selected_ids.borrow_mut();
                if check.is_active() {
                    ids.push(post_id);
                } else {
                    ids.retain(|id| *id != post_id);
                }
                drop(ids);
                update_sensitivities();
            });
        }
        checks.borrow_mut().push((check.clone(), post_id));
        list_box.append(&check);
    }
    update_sensitivities();

    let content = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(12).margin_top(18).margin_bottom(18).margin_start(18).margin_end(18).build();
    if posts.is_empty() {
        let placeholder = adw::StatusPage::builder()
            .icon_name("dialog-information-symbolic")
            .title(tr("Keine Artikel gefunden"))
            .description(tr("Veröffentliche zuerst einen Artikel, um ihn als Stilvorlage zu verwenden."))
            .vexpand(true)
            .build();
        placeholder.add_css_class("compact");
        content.append(&placeholder);
    } else {
        let scroller = gtk4::ScrolledWindow::builder().child(&list_box).vexpand(true).build();
        content.append(&scroller);
        content.append(&limit_label);
    }

    let done_button = gtk4::Button::with_label(&tr("Fertig"));
    done_button.add_css_class("suggested-action");

    let header = adw::HeaderBar::new();
    header.pack_end(&done_button);

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&content));

    let sheet = adw::Dialog::builder().title(tr("Beispielartikel auswählen")).content_width(440).content_height(480).child(&toolbar_view).build();

    {
        let sheet = sheet.clone();
        done_button.connect_clicked(move |_| {
            on_done();
            sheet.close();
        });
    }

    sheet.present(Some(parent));
}

pub(crate) fn llm_client() -> Result<llm::Client, String> {
    let config = chatconfig::load_provider_config();
    let provider = config.active;
    let model = config.model_for(provider).to_string();
    if provider.needs_api_key() {
        let key = futures_lite::future::block_on(secrets::load_llm_api_key(provider.id()))
            .map_err(|err| err.to_string())?
            .ok_or_else(|| tr("Kein {provider}-API-Key in den Einstellungen hinterlegt.").replace("{provider}", provider.label()))?;
        Ok(llm::Client::new(provider, &key, &model, &config.ollama_base_url))
    } else {
        Ok(llm::Client::new(provider, "", &model, &config.ollama_base_url))
    }
}

/// Opens the dialog; `on_apply` is called once with the reviewed article
/// and how the user chose to use it.
pub fn open(window: &gtk4::Window, on_apply: impl Fn(GeneratedArticle, ApplyMode) + 'static) {
    let brief_view = gtk4::TextView::builder().wrap_mode(gtk4::WrapMode::WordChar).top_margin(8).bottom_margin(8).left_margin(8).right_margin(8).accepts_tab(false).build();
    let brief_scroller = gtk4::ScrolledWindow::builder().child(&brief_view).min_content_height(90).build();
    brief_scroller.add_css_class("card");
    let brief_label = gtk4::Label::builder().label(tr("Thema und Anweisungen")).xalign(0.0).build();
    brief_label.add_css_class("heading");

    let length_labels: Vec<String> = Length::ALL.iter().map(Length::label).collect();
    let length_label_refs: Vec<&str> = length_labels.iter().map(String::as_str).collect();
    let length_row = adw::ComboRow::builder().title(tr("Länge")).model(&gtk4::StringList::new(&length_label_refs)).selected(1).build();

    let voice_row = adw::SwitchRow::builder()
        .title(tr("Meinen Schreibstil nachahmen"))
        .subtitle(tr("Schickt ausgewählte eigene Artikel als Stilvorlage mit"))
        .active(!wpsite::load().url.is_empty())
        .build();

    // Explicitly picked posts, not just "however many of the most recent
    // ones" - see this module's own doc comment for why (Quill's
    // `SamplePostPickerSheet`). `selected_sample_ids` is the chosen ids;
    // `posts_cache` is every publishable post's id/title, fetched once
    // the first time the picker actually opens (not eagerly at dialog-open
    // time - a real network round trip nobody may end up needing, if
    // "Meinen Schreibstil nachahmen" stays off).
    let selected_sample_ids: Rc<RefCell<Vec<u64>>> = Rc::new(RefCell::new(Vec::new()));
    let posts_cache: Rc<RefCell<Option<Vec<wpclient::PostSummary>>>> = Rc::new(RefCell::new(None));

    // Activatable row (not a separate button) with just a plain chevron
    // suffix - the whole row is the click target, matching the
    // "row that opens something else" convention (e.g. a sub-page link)
    // rather than needing a precisely-aimed icon click.
    let samples_row = adw::ActionRow::builder().title(tr("Beispielartikel")).activatable(true).build();
    let samples_row_chevron = gtk4::Image::from_icon_name("go-next-symbolic");
    samples_row_chevron.add_css_class("dim-label");
    samples_row.add_suffix(&samples_row_chevron);
    voice_row.bind_property("active", &samples_row, "sensitive").sync_create().build();

    let refresh_samples_row = {
        let samples_row = samples_row.clone();
        let selected_sample_ids = selected_sample_ids.clone();
        move || {
            let count = selected_sample_ids.borrow().len();
            samples_row.set_subtitle(&if count == 0 {
                tr("Keine ausgewählt - tippen zum Auswählen")
            } else if count == 1 {
                tr("1 Artikel ausgewählt")
            } else {
                tr("{n} Artikel ausgewählt").replace("{n}", &count.to_string())
            });
        }
    };
    refresh_samples_row();

    let options_group = adw::PreferencesGroup::new();
    options_group.add(&length_row);
    options_group.add(&voice_row);
    options_group.add(&samples_row);

    let generate_button = gtk4::Button::with_label(&tr("Artikel generieren"));
    generate_button.add_css_class("suggested-action");
    generate_button.add_css_class("pill");
    generate_button.set_halign(gtk4::Align::Center);

    let status_label = gtk4::Label::builder().wrap(true).xalign(0.0).build();
    status_label.add_css_class("dim-label");
    status_label.set_visible(false);

    let result_buffer = gtk4::TextBuffer::new(None);
    let result_view = gtk4::TextView::builder().buffer(&result_buffer).wrap_mode(gtk4::WrapMode::WordChar).monospace(true).top_margin(8).bottom_margin(8).left_margin(8).right_margin(8).build();
    let result_scroller = gtk4::ScrolledWindow::builder().child(&result_view).min_content_height(220).vexpand(true).build();
    result_scroller.add_css_class("card");
    result_scroller.set_visible(false);

    let content = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(12).margin_top(18).margin_bottom(18).margin_start(18).margin_end(18).build();
    content.append(&brief_label);
    content.append(&brief_scroller);
    content.append(&options_group);
    content.append(&generate_button);
    content.append(&status_label);
    content.append(&result_scroller);
    let content_scroller = gtk4::ScrolledWindow::builder().child(&content).hscrollbar_policy(gtk4::PolicyType::Never).vexpand(true).build();

    let new_doc_button = gtk4::Button::with_label(&tr("Als neues Dokument"));
    new_doc_button.add_css_class("suggested-action");
    new_doc_button.set_sensitive(false);
    let insert_button = gtk4::Button::with_label(&tr("An Cursor einfügen"));
    insert_button.set_sensitive(false);

    // Bottom bar, not the header: two text buttons there squeezed the
    // dialog's own title off-center.
    let action_bar = gtk4::Box::builder().orientation(gtk4::Orientation::Horizontal).spacing(6).halign(gtk4::Align::End).margin_top(6).margin_bottom(6).margin_start(12).margin_end(12).build();
    action_bar.append(&insert_button);
    action_bar.append(&new_doc_button);

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&adw::HeaderBar::new());
    toolbar_view.add_bottom_bar(&action_bar);
    toolbar_view.set_content(Some(&content_scroller));

    let dialog = adw::Dialog::builder().title(tr("KI-Artikel schreiben")).content_width(640).content_height(700).child(&toolbar_view).build();

    {
        let dialog = dialog.clone();
        let posts_cache = posts_cache.clone();
        let selected_sample_ids = selected_sample_ids.clone();
        let refresh_samples_row = refresh_samples_row.clone();
        let status_label = status_label.clone();
        samples_row.connect_activated(move |_| {
            if let Some(posts) = posts_cache.borrow().as_ref() {
                open_sample_picker(&dialog, posts, selected_sample_ids.clone(), refresh_samples_row.clone());
                return;
            }
            let dialog = dialog.clone();
            let posts_cache = posts_cache.clone();
            let selected_sample_ids = selected_sample_ids.clone();
            let refresh_samples_row = refresh_samples_row.clone();
            let status_label = status_label.clone();
            status_label.set_label(&tr("Lade eigene Artikel …"));
            status_label.set_visible(true);
            let (tx, rx) = mpsc::channel::<Result<Vec<wpclient::PostSummary>, String>>();
            std::thread::spawn(move || {
                let _ = tx.send(fetch_publishable_posts());
            });
            glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
                Ok(Ok(posts)) => {
                    status_label.set_visible(false);
                    *posts_cache.borrow_mut() = Some(posts.clone());
                    open_sample_picker(&dialog, &posts, selected_sample_ids.clone(), refresh_samples_row.clone());
                    glib::ControlFlow::Break
                }
                Ok(Err(err)) => {
                    status_label.set_label(&tr("Fehler: {err}").replace("{err}", &err));
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    status_label.set_label(&tr("Interner Fehler: Laden hat kein Ergebnis geliefert."));
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            });
        });
    }

    {
        let generate_button_for_click = generate_button.clone();
        let status_label = status_label.clone();
        let result_buffer = result_buffer.clone();
        let result_scroller = result_scroller.clone();
        let new_doc_button = new_doc_button.clone();
        let insert_button = insert_button.clone();
        let brief_view = brief_view.clone();
        let selected_sample_ids = selected_sample_ids.clone();
        generate_button.connect_clicked(move |_| {
            let brief_buffer = brief_view.buffer();
            let brief = brief_buffer.text(&brief_buffer.start_iter(), &brief_buffer.end_iter(), false).to_string();
            if brief.trim().is_empty() {
                status_label.set_label(&tr("Bitte zuerst ein Thema oder Anweisungen eingeben."));
                status_label.set_visible(true);
                return;
            }
            let length = Length::ALL.get(length_row.selected() as usize).copied().unwrap_or(Length::Medium);
            let sample_ids: Vec<u64> = if voice_row.is_active() { selected_sample_ids.borrow().clone() } else { Vec::new() };

            generate_button_for_click.set_sensitive(false);
            new_doc_button.set_sensitive(false);
            insert_button.set_sensitive(false);
            status_label.set_label(&if sample_ids.is_empty() { tr("Wird generiert …") } else { tr("Lade Stilvorlagen und generiere …") });
            status_label.set_visible(true);

            let (tx, rx) = mpsc::channel::<Result<String, String>>();
            std::thread::spawn(move || {
                let outcome = (|| {
                    let samples = if sample_ids.is_empty() { Vec::new() } else { fetch_style_samples(&sample_ids)? };
                    let prompt = build_prompt(&brief, length, &samples);
                    llm_client()?.send(SYSTEM_PROMPT, &[llm::ChatMessage { role: llm::Role::User, text: prompt }]).map_err(|err| err.to_string())
                })();
                let _ = tx.send(outcome);
            });

            let generate_button = generate_button_for_click.clone();
            let status_label = status_label.clone();
            let result_buffer = result_buffer.clone();
            let result_scroller = result_scroller.clone();
            let new_doc_button = new_doc_button.clone();
            let insert_button = insert_button.clone();
            glib::timeout_add_local(Duration::from_millis(150), move || {
                let outcome = match rx.try_recv() {
                    Ok(outcome) => outcome,
                    Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                    Err(mpsc::TryRecvError::Disconnected) => Err(tr("Interner Fehler: Generierung hat kein Ergebnis geliefert.")),
                };
                generate_button.set_sensitive(true);
                match outcome {
                    Ok(text) => {
                        result_buffer.set_text(text.trim());
                        result_scroller.set_visible(true);
                        new_doc_button.set_sensitive(true);
                        insert_button.set_sensitive(true);
                        status_label.set_label(&tr("Fertig - vor dem Übernehmen prüfen und bei Bedarf direkt hier bearbeiten."));
                    }
                    Err(err) => status_label.set_label(&tr("Fehler: {err}").replace("{err}", &err)),
                }
                glib::ControlFlow::Break
            });
        });
    }

    let on_apply: Rc<dyn Fn(GeneratedArticle, ApplyMode)> = Rc::new(on_apply);
    for (button, mode) in [(new_doc_button, ApplyMode::NewDocument), (insert_button, ApplyMode::InsertAtCursor)] {
        let result_buffer = result_buffer.clone();
        let dialog = dialog.clone();
        let on_apply = on_apply.clone();
        button.connect_clicked(move |_| {
            let text = result_buffer.text(&result_buffer.start_iter(), &result_buffer.end_iter(), false).to_string();
            on_apply(parse_generated(&text), mode);
            dialog.close();
        });
    }

    dialog.present(Some(window));
    brief_view.grab_focus();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_generated_takes_the_leading_heading_as_title() {
        let article = parse_generated("# Mein Titel\n\nErster Absatz.\n\n## Teil\n\nMehr.");
        assert_eq!(article.title, "Mein Titel");
        assert_eq!(article.body, "Erster Absatz.\n\n## Teil\n\nMehr.\n");
    }

    #[test]
    fn parse_generated_strips_a_wrapping_markdown_fence() {
        let article = parse_generated("```markdown\n# Titel\n\nText.\n```");
        assert_eq!(article.title, "Titel");
        assert_eq!(article.body, "Text.\n");
    }

    #[test]
    fn parse_generated_without_heading_keeps_everything_as_body() {
        let article = parse_generated("Nur Text.\n\nZweiter Absatz.");
        assert_eq!(article.title, "");
        assert_eq!(article.body, "Nur Text.\n\nZweiter Absatz.\n");
    }

    #[test]
    fn build_prompt_includes_samples_only_when_given() {
        let without = build_prompt("Über GNOME 50", Length::Short, &[]);
        assert!(!without.contains("Beispiel 1"));
        assert!(without.contains("etwa 400 Wörtern"));
        assert!(without.contains("Über GNOME 50"));

        let with = build_prompt("Über GNOME 50", Length::Long, &["# Alt\n\nText".to_string(), "# Alt 2".to_string()]);
        assert!(with.contains("--- Beispiel 1 ---\n# Alt\n\nText"));
        assert!(with.contains("--- Beispiel 2 ---"));
        assert!(with.contains("etwa 1500 Wörtern"));
    }

    #[test]
    fn truncate_sample_prefers_a_paragraph_boundary() {
        let text = format!("{}\n\n{}", "a".repeat(80), "b".repeat(80));
        let cut = truncate_sample(&text, 100);
        assert_eq!(cut, format!("{} …", "a".repeat(80)));
        assert_eq!(truncate_sample("kurz", 100), "kurz");
    }

    #[test]
    fn truncate_sample_is_safe_on_multibyte_text() {
        let cut = truncate_sample(&"ä".repeat(50), 10);
        assert!(cut.starts_with("ää"));
    }
}
