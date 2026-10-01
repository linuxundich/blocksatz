//! "Links" tab of the export dialog: nothing in the app ever validated a
//! URL before this - a typo'd or since-deleted link would ship silently.
//! `scan_links` finds every `http(s)://` URL referenced anywhere in the
//! article (Markdown link/image destinations, plus bare-URL embed lines
//! that `crates/gutenberg`'s `as_lone_embed` turns into a real `wp:embed`
//! block), and a background thread HEADs each one, falling back to GET for
//! servers that reject HEAD, reporting anything outside the 2xx/3xx range
//! - or a network error/timeout - as broken.

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk4::glib;
use pulldown_cmark::{Event, Options, Parser, Tag};

use crate::i18n::tr;
use crate::{browser, notify};

/// Finds every unique `http(s)://` URL referenced in `markdown` - Markdown
/// link and image/media destinations (`[text](url)`, `![alt](url)`, and the
/// CommonMark `<url>` autolink form all parse as `Tag::Link`/`Tag::Image`
/// events), plus a bare URL alone on its own line, which pulldown-cmark
/// leaves as plain `Event::Text` rather than turning into a link event -
/// exactly the case `crates/gutenberg`'s `as_lone_embed` recognizes and
/// exports as a real `wp:embed` block, so it needs checking here too.
/// Order is first-seen; a local/relative path (an uploaded image, a video
/// file) is never a candidate, only ever a real `http(s)://` URL.
pub fn scan_links(markdown: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut seen = HashSet::new();

    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(markdown, options) {
        if let Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) = event {
            add_link(&mut links, &mut seen, &dest_url);
        }
    }
    for word in markdown.split_whitespace() {
        add_link(&mut links, &mut seen, word);
    }

    links
}

fn add_link(links: &mut Vec<String>, seen: &mut HashSet<String>, raw: &str) {
    let url = raw.trim_end_matches(['.', ',', ')', ']', '!', '?', '"', '\'', ';', ':']);
    if (url.starts_with("http://") || url.starts_with("https://")) && seen.insert(url.to_string()) {
        links.push(url.to_string());
    }
}

#[derive(Debug, Clone)]
enum LinkOutcome {
    Ok(u16),
    Broken(String),
}

/// Checks one URL: a HEAD request, falling back to GET if the server
/// rejects HEAD outright (405/501 - a real, if uncommon, thing some
/// servers/CDNs do even though the same resource GETs fine). Any response
/// outside the 2xx/3xx range, or a network error/timeout, counts as
/// broken - this is a pre-publish sanity check, not a strict spec-
/// compliance test, so a redirect is treated the same as a direct 200.
fn check_link(url: &str) -> LinkOutcome {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);

    let status = agent.head(url).call().map(|r| r.status().as_u16());
    let status = match status {
        Ok(405) | Ok(501) => agent.get(url).call().map(|r| r.status().as_u16()),
        other => other,
    };

    match status {
        Ok(code) if (200..400).contains(&code) => LinkOutcome::Ok(code),
        Ok(code) => LinkOutcome::Broken(code.to_string()),
        Err(err) => LinkOutcome::Broken(err.to_string()),
    }
}

enum CheckMessage {
    Result(String, LinkOutcome),
    Done,
}

fn outcome_text(outcome: &LinkOutcome) -> String {
    match outcome {
        LinkOutcome::Ok(code) => tr("OK ({code})").replace("{code}", &code.to_string()),
        LinkOutcome::Broken(reason) => tr("Fehler: {reason}").replace("{reason}", reason),
    }
}

fn summary_text(total: usize, broken: usize) -> String {
    if total == 0 {
        return tr("Dieser Artikel enthält keine Links.");
    }
    if broken == 0 {
        return match total {
            1 => tr("1 Link gefunden - noch nicht geprüft."),
            n => tr("{n} Links gefunden - noch nicht geprüft.").replace("{n}", &n.to_string()),
        };
    }
    tr("{broken} von {total} Links fehlerhaft.").replace("{broken}", &broken.to_string()).replace("{total}", &total.to_string())
}

fn all_ok_text(total: usize) -> String {
    match total {
        1 => tr("Der eine Link funktioniert."),
        n => tr("Alle {n} Links funktionieren.").replace("{n}", &n.to_string()),
    }
}

/// Builds the "Links" tab's content - a summary line, a "Links prüfen"
/// button, and one row per unique URL found in `body`, updated live as
/// results come in. Scans `body` once at open time, same as
/// `mediapanel::build_content`'s one-shot `media::reconcile` - if links are
/// added/removed after this dialog is already open, re-opening the export
/// dialog picks them up, matching how the rest of this dialog behaves.
///
/// Each row has two suffix buttons: "URL kopieren" copies the plain URL to
/// the clipboard, and "im Browser-Tab öffnen" opens it in the app's own
/// Browser tab (`app_view_stack`/`browser_view`) rather than an external
/// browser - the same "stay inside the app" convention `export.rs`'s own
/// "Vorschau öffnen" button already uses, so checking a link that looks
/// broken doesn't mean leaving Blocksatz to look at it.
pub fn build_content(body: &str, app_view_stack: &adw::ViewStack, browser_view: &Rc<browser::BrowserView>) -> gtk4::Widget {
    let links = scan_links(body);

    let content_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(12)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();

    let status_label = gtk4::Label::new(Some(&summary_text(links.len(), 0)));
    status_label.set_wrap(true);
    status_label.set_xalign(0.0);

    let check_button = gtk4::Button::with_label(&tr("Links prüfen"));
    check_button.add_css_class("suggested-action");
    check_button.set_halign(gtk4::Align::Start);
    check_button.set_sensitive(!links.is_empty());

    let list_box = gtk4::ListBox::new();
    list_box.add_css_class("boxed-list");
    let rows: Vec<adw::ActionRow> = links
        .iter()
        .map(|url| {
            let row = adw::ActionRow::builder().title(url.as_str()).subtitle(tr("Noch nicht geprüft")).use_markup(false).build();

            let copy_button = gtk4::Button::from_icon_name("edit-copy-symbolic");
            copy_button.set_tooltip_text(Some(&tr("URL kopieren")));
            copy_button.set_valign(gtk4::Align::Center);
            copy_button.add_css_class("flat");
            {
                let url = url.clone();
                copy_button.connect_clicked(move |button| {
                    button.clipboard().set_text(&url);
                });
            }
            row.add_suffix(&copy_button);

            let open_button = gtk4::Button::from_icon_name("web-browser-symbolic");
            open_button.set_tooltip_text(Some(&tr("Im Browser-Tab öffnen")));
            open_button.set_valign(gtk4::Align::Center);
            open_button.add_css_class("flat");
            {
                let url = url.clone();
                let app_view_stack = app_view_stack.clone();
                let browser_view = browser_view.clone();
                open_button.connect_clicked(move |_| {
                    browser_view.load_uri(&url);
                    app_view_stack.set_visible_child_name("browser");
                });
            }
            row.add_suffix(&open_button);

            list_box.append(&row);
            row
        })
        .collect();

    let scroller = gtk4::ScrolledWindow::builder().child(&list_box).vexpand(true).min_content_height(300).build();

    content_box.append(&status_label);
    content_box.append(&check_button);
    content_box.append(&scroller);

    {
        let links = links.clone();
        let status_label = status_label.clone();
        let check_button_for_click = check_button.clone();
        check_button.connect_clicked(move |_| {
            check_button_for_click.set_sensitive(false);
            for row in &rows {
                row.set_subtitle(&tr("Wird geprüft …"));
                row.remove_css_class("error");
            }
            status_label.set_label(&tr("Wird geprüft …"));

            let total = links.len();
            let (tx, rx) = mpsc::channel::<CheckMessage>();
            {
                let links = links.clone();
                std::thread::spawn(move || {
                    for url in links {
                        let outcome = check_link(&url);
                        let _ = tx.send(CheckMessage::Result(url, outcome));
                    }
                    let _ = tx.send(CheckMessage::Done);
                });
            }

            let rows_by_url: std::collections::HashMap<String, adw::ActionRow> =
                links.iter().cloned().zip(rows.iter().cloned()).collect();
            let status_label = status_label.clone();
            let check_button = check_button_for_click.clone();
            let broken_count = std::rc::Rc::new(std::cell::Cell::new(0usize));
            glib::timeout_add_local(Duration::from_millis(150), move || {
                loop {
                    match rx.try_recv() {
                        Ok(CheckMessage::Result(url, outcome)) => {
                            if let Some(row) = rows_by_url.get(&url) {
                                row.set_subtitle(&outcome_text(&outcome));
                                if matches!(outcome, LinkOutcome::Broken(_)) {
                                    row.add_css_class("error");
                                    broken_count.set(broken_count.get() + 1);
                                }
                            }
                        }
                        Ok(CheckMessage::Done) => {
                            let broken = broken_count.get();
                            let summary = if broken == 0 { all_ok_text(total) } else { summary_text(total, broken) };
                            status_label.set_label(&summary);
                            notify::send("link-check", &tr("Links geprüft"), &summary);
                            check_button.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                        Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            status_label.set_label(&tr("Interner Fehler: Prüf-Thread hat kein Ergebnis geliefert."));
                            check_button.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                    }
                }
            });
        });
    }

    content_box.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_link_destination() {
        assert_eq!(scan_links("See [my site](https://example.com/page) for more."), vec!["https://example.com/page"]);
    }

    #[test]
    fn finds_a_remote_image_url_but_not_a_local_path() {
        let markdown = "![remote](https://example.com/pic.png)\n\n![local](photo.png)\n";
        assert_eq!(scan_links(markdown), vec!["https://example.com/pic.png"]);
    }

    #[test]
    fn finds_a_bare_url_alone_on_its_own_line() {
        assert_eq!(scan_links("Intro.\n\nhttps://example.com/video\n\nOutro.\n"), vec!["https://example.com/video"]);
    }

    #[test]
    fn finds_a_commonmark_autolink() {
        assert_eq!(scan_links("Check <https://example.com/autolink> please."), vec!["https://example.com/autolink"]);
    }

    #[test]
    fn trims_trailing_sentence_punctuation_from_a_bare_url() {
        assert_eq!(scan_links("Siehe https://example.com/seite. Mehr dazu."), vec!["https://example.com/seite"]);
    }

    #[test]
    fn deduplicates_the_same_url_seen_twice() {
        let markdown = "[one](https://example.com/x) and [two](https://example.com/x)";
        assert_eq!(scan_links(markdown), vec!["https://example.com/x"]);
    }

    #[test]
    fn ignores_a_relative_link_to_another_local_file() {
        assert_eq!(scan_links("[see also](../notes.md)"), Vec::<String>::new());
    }

    #[test]
    fn no_links_in_plain_prose() {
        assert_eq!(scan_links("Just a normal paragraph with no links at all."), Vec::<String>::new());
    }

    #[test]
    fn summary_text_reports_the_no_links_case() {
        assert_eq!(summary_text(0, 0), "Dieser Artikel enthält keine Links.");
    }

    #[test]
    fn summary_text_uses_singular_for_one_unchecked_link() {
        assert_eq!(summary_text(1, 0), "1 Link gefunden - noch nicht geprüft.");
    }

    #[test]
    fn summary_text_reports_a_broken_count() {
        assert_eq!(summary_text(5, 2), "2 von 5 Links fehlerhaft.");
    }

    #[test]
    fn all_ok_text_uses_singular_for_one_link() {
        assert_eq!(all_ok_text(1), "Der eine Link funktioniert.");
    }

    #[test]
    fn all_ok_text_uses_plural_for_several_links() {
        assert_eq!(all_ok_text(3), "Alle 3 Links funktionieren.");
    }

    #[test]
    fn outcome_text_formats_success_and_failure() {
        assert_eq!(outcome_text(&LinkOutcome::Ok(200)), "OK (200)");
        assert_eq!(outcome_text(&LinkOutcome::Broken("404".to_string())), "Fehler: 404");
    }
}
