//! "Markdown-Nähe" (`docs/markdown-naehe.md`): how close an article is to
//! plain Markdown, and the warning when a heavily designed post is opened
//! from the blog. Blocksatz is meant for articles written in Markdown; a
//! post full of Gutenberg features opens without loss, but much of it can
//! then only be edited as WordPress markup - wp-admin is the better tool.
//!
//! The rating itself is `gutenberg::assess`; this module adds the settings,
//! German labels and the dialogs.

use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk4::glib;

use crate::i18n::tr;
use gutenberg::{Assessment, Closeness};

/// Blocks that belong to the normal workflow and don't count as foreign.
pub const DEFAULT_BUILDING_BLOCKS: &str = "lui/toc, lui-ads/slot, more, nextpage, footnotes";

fn config_path(name: &str) -> PathBuf {
    let mut dir = glib::user_config_dir();
    dir.push(crate::APP_DIR);
    dir.push(name);
    dir
}

fn write_config(name: &str, value: &str) {
    let path = config_path(name);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, value);
}

/// "Vor stark gestalteten Beiträgen warnen" - on unless switched off.
pub fn warn_enabled() -> bool {
    std::fs::read_to_string(config_path("markdown-warning")).map(|s| s.trim() != "0").unwrap_or(true)
}

pub fn set_warn_enabled(enabled: bool) {
    write_config("markdown-warning", if enabled { "1" } else { "0" });
}

/// The building blocks as typed in the settings (comma separated).
pub fn building_blocks_text() -> String {
    std::fs::read_to_string(config_path("building-blocks")).map(|s| s.trim().to_string()).unwrap_or_else(|_| DEFAULT_BUILDING_BLOCKS.to_string())
}

pub fn set_building_blocks_text(text: &str) {
    write_config("building-blocks", text.trim());
}

fn building_blocks() -> Vec<String> {
    building_blocks_text().split(',').map(|name| name.trim().trim_start_matches("core/").to_string()).filter(|name| !name.is_empty()).collect()
}

pub fn assess(body: &str) -> Assessment {
    let blocks = building_blocks();
    let names: Vec<&str> = blocks.iter().map(String::as_str).collect();
    gutenberg::assess(body, &names)
}

/// "Markdown-Nähe: …" - high for plain Markdown.
pub fn closeness_label(closeness: Closeness) -> String {
    match closeness {
        Closeness::Plain => tr("hoch"),
        Closeness::Designed => tr("mittel"),
        Closeness::Heavy => tr("gering"),
    }
}

/// A readable name for a block or container kind from `Assessment::kinds`.
fn kind_label(kind: &str) -> String {
    let name = kind.trim_start_matches(":::").trim_start_matches("```");
    match name {
        "group" => tr("Gruppe"),
        "columns" => tr("Spalten"),
        "media-text" => tr("Medien & Text"),
        "cover" => tr("Cover"),
        "accordion" => tr("Akkordeon"),
        "tabs" => tr("Reiter"),
        "details" => tr("Details"),
        "gallery" => tr("Galerie"),
        "buttons" | "button" => tr("Buttons"),
        "pullquote" => tr("Hervorgehobenes Zitat"),
        "quote" => tr("Zitat"),
        "paragraph" => tr("Absatz"),
        "heading" => tr("Überschrift"),
        "list" => tr("Liste"),
        "image" => tr("Bild"),
        "table" => tr("Tabelle"),
        "audio" => tr("Audio"),
        "video" => tr("Video"),
        "embed" => tr("Einbettung"),
        "file" => tr("Datei"),
        "html" => tr("HTML"),
        "freeform" => tr("Klassischer Inhalt"),
        "shortcode" => tr("Shortcode"),
        "query" => tr("Abfrage-Loop"),
        "separator" => tr("Trenner"),
        "spacer" => tr("Abstandhalter"),
        other => other.to_string(),
    }
}

/// "Gruppe (5), Medien & Text (2), …" - the most frequent kinds.
fn kinds_text(assessment: &Assessment, limit: usize) -> String {
    let mut parts: Vec<String> = assessment.kinds.iter().take(limit).map(|(kind, count)| if *count > 1 { format!("{} ({count})", kind_label(kind)) } else { kind_label(kind) }).collect();
    if assessment.kinds.len() > limit {
        parts.push(tr("…"));
    }
    parts.join(", ")
}

/// The body of the warning before opening.
fn warning_text(assessment: &Assessment) -> String {
    let mut text = if assessment.foreign > 0 {
        tr("{n} Blöcke lassen sich hier nur als WordPress-Markup bearbeiten.").replace("{n}", &assessment.foreign.to_string())
    } else {
        tr("Der Beitrag besteht zum großen Teil aus verschachtelten Containern.")
    };
    let kinds = kinds_text(assessment, 4);
    if !kinds.is_empty() {
        text.push(' ');
        text.push_str(&tr("Darunter: {kinds}.").replace("{kinds}", &kinds));
    }
    text.push_str("\n\n");
    text.push_str(&tr("Blocksatz ist für Artikel gedacht, die in Markdown geschrieben sind. Solche Beiträge bearbeitest du besser im Block-Editor von WordPress."));
    text
}

/// The extras as (label, value) rows, for the details dialog.
fn detail_rows(assessment: &Assessment) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    if assessment.foreign > 0 {
        rows.push((tr("Als WordPress-Markup im Text"), tr("{n} Blöcke ({share} %)").replace("{n}", &assessment.foreign.to_string()).replace("{share}", &format!("{:.0}", assessment.foreign_share() * 100.0))));
    }
    if assessment.classic {
        rows.push((tr("Klassischer Inhalt"), tr("aus der Zeit vor dem Block-Editor")));
    }
    if assessment.structure > 0 {
        rows.push((tr("Container und Sonderblöcke"), assessment.structure.to_string()));
    }
    if assessment.design > 0 {
        rows.push((tr("Gestaltungsangaben"), assessment.design.to_string()));
    }
    let kinds = kinds_text(assessment, 8);
    if !kinds.is_empty() {
        rows.push((tr("Betroffene Blöcke"), kinds));
    }
    rows
}

/// Before a heavily designed post becomes a working copy: open it anyway,
/// or edit it in wp-admin instead (the suggested way).
pub fn confirm_heavy_open(parent: Option<&gtk4::Widget>, assessment: &Assessment, on_open: Rc<dyn Fn()>, on_admin: Rc<dyn Fn()>) {
    let dialog = adw::AlertDialog::new(Some(&tr("Dieser Beitrag nutzt viele Gutenberg-Funktionen")), Some(&warning_text(assessment)));
    dialog.add_responses(&[("cancel", &tr("Abbrechen")), ("admin", &tr("In wp-admin bearbeiten")), ("open", &tr("Trotzdem öffnen"))]);
    dialog.set_response_appearance("admin", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("admin"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |_, response| match response {
        "admin" => on_admin(),
        "open" => on_open(),
        _ => {}
    });
    dialog.present(parent);
}

/// The details behind the banner and the status line. `on_hide` (if
/// given) offers to stop pointing it out for this article.
pub fn show_details(parent: Option<&gtk4::Widget>, assessment: &Assessment, on_hide: Option<Rc<dyn Fn()>>) {
    let dialog = adw::AlertDialog::new(Some(&tr("Gutenberg-Funktionen in diesem Beitrag")), Some(&tr("Markdown-Nähe: {level}").replace("{level}", &closeness_label(assessment.closeness))));
    let list = gtk4::ListBox::builder().selection_mode(gtk4::SelectionMode::None).build();
    list.add_css_class("boxed-list");
    for (title, value) in detail_rows(assessment) {
        let row = adw::ActionRow::builder().title(title.as_str()).subtitle(value.as_str()).subtitle_selectable(true).build();
        row.add_css_class("property");
        list.append(&row);
    }
    dialog.set_extra_child(Some(&list));
    dialog.add_response("close", &tr("Schließen"));
    if on_hide.is_some() {
        dialog.add_response("hide", &tr("Hinweis ausblenden"));
    }
    dialog.set_default_response(Some("close"));
    dialog.set_close_response("close");
    dialog.connect_response(None, move |_, response| {
        if response == "hide" {
            if let Some(on_hide) = &on_hide {
                on_hide();
            }
        }
    });
    dialog.present(parent);
}
