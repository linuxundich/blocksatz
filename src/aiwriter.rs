//! "KI-Artikel schreiben": drafts a whole article from a topic/brief with
//! whichever LLM provider is active in the KI-Chat settings - optionally in
//! the author's own voice, learned from a few of their most recently
//! published WordPress posts, which are sent along as style samples (only
//! for style - the prompt tells the model not to reuse their content).
//!
//! Nothing is written into the editor until the user has seen the result:
//! the generated Markdown lands in an editable text view first, and only
//! "Als neues Dokument" / "An Cursor einfügen" hand it to the caller -
//! the same "review before anything is written" rule `aialt.rs` and
//! `tagsuggest.rs` already follow.

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

/// Fetches the `count` most recently published posts' bodies as Markdown,
/// for use as style samples. Runs on a background thread.
fn fetch_style_samples(count: u32) -> Result<Vec<String>, String> {
    let site = wpsite::load();
    if site.url.is_empty() {
        return Err(tr("Für den eigenen Schreibstil muss eine WordPress-Verbindung eingerichtet sein."));
    }
    let password = futures_lite::future::block_on(secrets::load_app_password(&site.url, &site.username))
        .map_err(|err| err.to_string())?
        .ok_or_else(|| tr("Kein Application Password im Schlüsselbund gefunden."))?;
    let client = wpclient::Client::new(&site.url, &site.username, &password);
    let posts = client.list_posts().map_err(|err| err.to_string())?;
    let mut samples = Vec::new();
    for post in posts.iter().filter(|p| p.status == "publish").take(count as usize) {
        let detail = client.get_item("posts", post.id).map_err(|err| err.to_string())?;
        let markdown = gutenberg::gutenberg_to_markdown(&detail.content);
        if !markdown.trim().is_empty() {
            samples.push(truncate_sample(&format!("# {}\n\n{}", detail.title, markdown), SAMPLE_CHAR_LIMIT));
        }
    }
    Ok(samples)
}

fn llm_client() -> Result<llm::Client, String> {
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
        .subtitle(tr("Schickt die zuletzt veröffentlichten Artikel als Stilvorlage mit"))
        .active(!wpsite::load().url.is_empty())
        .build();
    let samples_row = adw::SpinRow::builder()
        .title(tr("Anzahl Beispielartikel"))
        .adjustment(&gtk4::Adjustment::new(3.0, 1.0, MAX_STYLE_SAMPLES as f64, 1.0, 1.0, 0.0))
        .build();
    voice_row.bind_property("active", &samples_row, "sensitive").sync_create().build();

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
        let generate_button_for_click = generate_button.clone();
        let status_label = status_label.clone();
        let result_buffer = result_buffer.clone();
        let result_scroller = result_scroller.clone();
        let new_doc_button = new_doc_button.clone();
        let insert_button = insert_button.clone();
        let brief_view = brief_view.clone();
        generate_button.connect_clicked(move |_| {
            let brief_buffer = brief_view.buffer();
            let brief = brief_buffer.text(&brief_buffer.start_iter(), &brief_buffer.end_iter(), false).to_string();
            if brief.trim().is_empty() {
                status_label.set_label(&tr("Bitte zuerst ein Thema oder Anweisungen eingeben."));
                status_label.set_visible(true);
                return;
            }
            let length = Length::ALL.get(length_row.selected() as usize).copied().unwrap_or(Length::Medium);
            let sample_count = if voice_row.is_active() { samples_row.value().round() as u32 } else { 0 };

            generate_button_for_click.set_sensitive(false);
            new_doc_button.set_sensitive(false);
            insert_button.set_sensitive(false);
            status_label.set_label(&if sample_count > 0 { tr("Lade Stilvorlagen und generiere …") } else { tr("Wird generiert …") });
            status_label.set_visible(true);

            let (tx, rx) = mpsc::channel::<Result<String, String>>();
            std::thread::spawn(move || {
                let outcome = (|| {
                    let samples = if sample_count > 0 { fetch_style_samples(sample_count)? } else { Vec::new() };
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
