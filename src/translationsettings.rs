//! The "Übersetzung" page of the Einstellungen dialog and the settings it
//! keeps (`docs/translations.md`): how a new language version starts,
//! whether "Original kopieren" protects code and markup, and whether the
//! AI translation is offered at all. Translating by hand or with another
//! tool is the normal case; the AI is one option among others.

use std::path::PathBuf;

use adw::prelude::*;
use gtk4::glib;

use crate::i18n::tr;
use crate::{aiprompts, translate};

/// How "Fassung anlegen" fills a new language version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// The original's text, to overwrite sentence by sentence.
    Template,
    /// A translation copied from DeepL, a chat or elsewhere.
    Clipboard,
    /// Nothing but the link, categories, tags and featured image.
    Empty,
}

impl Start {
    pub const ALL: [Start; 3] = [Start::Template, Start::Clipboard, Start::Empty];

    fn key(self) -> &'static str {
        match self {
            Start::Template => "template",
            Start::Clipboard => "clipboard",
            Start::Empty => "empty",
        }
    }

    pub fn label(self) -> String {
        match self {
            Start::Template => tr("Original als Vorlage"),
            Start::Clipboard => tr("Aus der Zwischenablage"),
            Start::Empty => tr("Leer"),
        }
    }

    pub fn description(self) -> String {
        match self {
            Start::Template => tr("Text, Bilder, Code und Container werden übernommen. Du überschreibst die Sätze."),
            Start::Clipboard => tr("Ganzer Artikel aus DeepL, einem Chat oder einer Datei. Code und Links werden geprüft."),
            Start::Empty => tr("Nur Verknüpfung, Kategorien, Schlagwörter und Beitragsbild. Den Text schreibst du neu."),
        }
    }
}

fn path() -> PathBuf {
    let mut path = glib::user_config_dir();
    path.push(crate::APP_DIR);
    path.push("translation.conf");
    path
}

fn get(key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path()).ok()?;
    text.lines().find_map(|l| l.split_once('=').filter(|(k, _)| k.trim() == key).map(|(_, v)| v.trim().to_string()))
}

fn set(key: &str, value: &str) {
    let text = std::fs::read_to_string(path()).unwrap_or_default();
    let mut lines: Vec<String> = text.lines().filter(|l| l.split_once('=').is_none_or(|(k, _)| k.trim() != key)).map(str::to_string).collect();
    lines.push(format!("{key}={value}"));
    let path = path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, lines.join("\n") + "\n");
}

pub fn start() -> Start {
    let key = get("start");
    Start::ALL.into_iter().find(|s| Some(s.key()) == key.as_deref()).unwrap_or(Start::Template)
}

pub fn set_start(start: Start) {
    set("start", start.key());
}

/// Whether "Original kopieren" replaces code, links and markup with
/// `⟦KIND-n⟧` placeholders (on unless switched off).
pub fn protect() -> bool {
    get("protect").is_none_or(|v| v != "0")
}

/// Whether the AI translation is offered - as a secondary way (on unless
/// switched off).
pub fn ai_enabled() -> bool {
    get("ai").is_none_or(|v| v != "0")
}

pub fn build_page() -> adw::PreferencesPage {
    let group = adw::PreferencesGroup::builder().title(tr("Übersetzung")).build();
    group.set_description(Some(&tr("Für die zweite Sprachfassung eines Artikels (DE · EN im Fensterkopf).")));

    let starts = gtk4::StringList::new(&Start::ALL.iter().map(|s| s.label()).collect::<Vec<_>>().iter().map(String::as_str).collect::<Vec<_>>());
    let start_row = adw::ComboRow::builder().title(tr("Neue Fassungen beginnen mit")).model(&starts).build();
    start_row.set_selected(Start::ALL.iter().position(|s| *s == start()).unwrap_or(0) as u32);
    start_row.connect_selected_notify(|row| {
        if let Some(start) = Start::ALL.get(row.selected() as usize) {
            set_start(*start);
        }
    });
    group.add(&start_row);

    let protect_row = adw::SwitchRow::builder()
        .title(tr("Beim Kopieren schützen"))
        .subtitle(tr("„Original kopieren“ ersetzt Code, Link-Ziele und Auszeichnungen durch Marken wie ⟦CODE-3⟧, die DeepL und Chats stehen lassen. Beim Einfügen kommen sie zurück."))
        .active(protect())
        .build();
    protect_row.connect_active_notify(|row| set("protect", if row.is_active() { "1" } else { "0" }));
    group.add(&protect_row);

    let ai_row = adw::SwitchRow::builder()
        .title(tr("KI-Übersetzung anbieten"))
        .subtitle(tr("Als zusätzlicher Weg auf der Startseite und im Menü. Prompt und Modell unter KI-Prompts und KI-Modelle."))
        .active(ai_enabled())
        .build();
    ai_row.connect_active_notify(|row| set("ai", if row.is_active() { "1" } else { "0" }));
    group.add(&ai_row);

    let categories = adw::ExpanderRow::builder().title(tr("Kategorien zuordnen")).subtitle(tr("Eine Zeile je Kategorie, z. B. „Allgemein = General“. Nicht aufgeführte Namen bleiben gleich.")).build();
    let id = translate::CATEGORY_MAP_ID;
    let (editor_row, _status) = crate::promptsettings::build_prompt_editor(
        id,
        move || aiprompts::load_text_or(id, ""),
        move |text: &str| aiprompts::save_prompt_text(id, text),
        Some(move || aiprompts::reset_prompt_text(id)),
        move || aiprompts::is_prompt_customized(id),
    );
    categories.add_row(&editor_row);
    group.add(&categories);

    let page = adw::PreferencesPage::builder().title(tr("Übersetzung")).icon_name("preferences-desktop-locale-symbolic").build();
    page.add(&group);
    page
}
