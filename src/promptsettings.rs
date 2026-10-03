//! The "KI-Prompts" page of the Einstellungen dialog: the five built-in
//! context-menu prompts (editable, resettable to their defaults), and the
//! user's own custom prompts (create/edit/delete), kept in a group of
//! their own so built-in and custom prompts are never confused with each
//! other.
//!
//! The editor context menu's "eigene Prompts" section is a live `gio::Menu`
//! built once in `aimenu.rs`; this page is handed a clone of that same menu
//! handle so editing a custom prompt here updates the context menu
//! immediately, without needing the app restarted.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk4::{gio, glib};

use crate::aiprompts::{self, CustomPrompt};
use crate::i18n::tr;

pub fn build_page(custom_prompts_menu: gio::Menu) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder().title(tr("KI-Prompts")).icon_name("insert-text-symbolic").build();

    page.add(&build_builtin_prompts_group());
    page.add(&build_translation_group());
    page.add(&build_custom_prompts_group(&custom_prompts_menu));

    page
}

fn build_builtin_prompts_group() -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(tr("Standard-Prompts")).build();
    group.set_description(Some(&tr("Die vordefinierten KI-Aktionen im Editor-Kontextmenü. Änderungen lassen sich auf die Vorgabe zurücksetzen.")));

    for prompt in aiprompts::BUILTIN_PROMPTS {
        // Titles are plain text (e.g. "Stil & Formatierung prüfen"), not
        // Pango markup - `use_markup(false)` avoids a parse failure on the
        // unescaped "&" (Adw.ExpanderRow's title is markup by default).
        let expander = adw::ExpanderRow::builder().title(aiprompts::builtin_title(prompt.id)).use_markup(false).build();
        let (editor_row, _status) = build_prompt_editor(
            prompt.id,
            {
                let id = prompt.id;
                move || aiprompts::load_prompt_text(id)
            },
            {
                let id = prompt.id;
                move |text: &str| aiprompts::save_prompt_text(id, text)
            },
            Some({
                let id = prompt.id;
                move || aiprompts::reset_prompt_text(id)
            }),
            {
                let id = prompt.id;
                move || aiprompts::is_prompt_customized(id)
            },
        );
        expander.add_row(&editor_row);
        group.add(&expander);
    }

    group
}

/// The translation prompt and the category mapping (`translate.rs`) - not
/// context-menu actions, so a group of their own.
fn build_translation_group() -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(tr("Übersetzung")).build();
    group.set_description(Some(&tr("Für „Übersetzen …“ im Menü der Hauptaktion. Stimme, Konventionen und ein Glossar des eigenen Blogs gehören in den Prompt.")));

    let entries: [(String, &'static str, &'static str); 2] = [
        (tr("Übersetzungs-Prompt"), crate::translate::PROMPT_ID, crate::translate::DEFAULT_PROMPT),
        (tr("Kategorien zuordnen"), crate::translate::CATEGORY_MAP_ID, ""),
    ];
    for (title, id, default) in entries {
        let expander = adw::ExpanderRow::builder().title(title).use_markup(false).build();
        if id == crate::translate::CATEGORY_MAP_ID {
            expander.set_subtitle(&tr("Eine Zeile je Kategorie, z. B. „Allgemein = General“. Nicht aufgeführte Namen bleiben gleich."));
        }
        let (editor_row, _status) = build_prompt_editor(
            id,
            move || aiprompts::load_text_or(id, default),
            move |text: &str| aiprompts::save_prompt_text(id, text),
            Some(move || aiprompts::reset_prompt_text(id)),
            move || aiprompts::is_prompt_customized(id),
        );
        expander.add_row(&editor_row);
        group.add(&expander);
    }
    group
}

/// Builds one prompt-template editor: a debounced auto-saving multi-line
/// text view plus a status label, and (for built-ins) a "Zurücksetzen"
/// button next to it. Shared between the built-in prompts group (with a
/// reset function) and the custom prompts group (`reset_fn: None`, since a
/// user-authored prompt has no "default" to revert to).
fn build_prompt_editor(
    id: &str,
    load: impl Fn() -> String + 'static,
    save: impl Fn(&str) -> std::io::Result<()> + 'static,
    reset_fn: Option<impl Fn() -> std::io::Result<()> + 'static>,
    is_customized: impl Fn() -> bool + 'static,
) -> (gtk4::ListBoxRow, gtk4::Label) {
    let buffer = gtk4::TextBuffer::new(None::<&gtk4::TextTagTable>);
    buffer.set_text(&load());
    let text_view = gtk4::TextView::builder()
        .buffer(&buffer)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(8)
        .right_margin(8)
        .build();
    let scroller = gtk4::ScrolledWindow::builder().child(&text_view).vexpand(false).min_content_height(140).build();
    scroller.add_css_class("card");

    let status = gtk4::Label::new(None);
    status.add_css_class("dim-label");
    status.set_xalign(0.0);
    status.set_visible(false);

    let reset_button = gtk4::Button::with_label(&tr("Auf Standard zurücksetzen"));
    reset_button.set_halign(gtk4::Align::Start);
    let has_reset = reset_fn.is_some();
    reset_button.set_visible(has_reset);
    if has_reset {
        reset_button.set_sensitive(is_customized());
    }

    let save = Rc::new(save);
    let is_customized = Rc::new(is_customized);
    let suppress_autosave = Rc::new(RefCell::new(false));
    let debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    {
        let status = status.clone();
        let reset_button = reset_button.clone();
        let suppress_autosave = suppress_autosave.clone();
        let debounce = debounce.clone();
        let save = save.clone();
        let is_customized = is_customized.clone();
        buffer.connect_changed(move |buf| {
            if suppress_autosave.replace(false) {
                return;
            }
            if let Some(id) = debounce.borrow_mut().take() {
                id.remove();
            }
            let text = buf.text(&buf.start_iter(), &buf.end_iter(), false).to_string();
            let status = status.clone();
            let reset_button = reset_button.clone();
            let debounce_inner = debounce.clone();
            let save = save.clone();
            let is_customized = is_customized.clone();
            let id = glib::timeout_add_local(Duration::from_millis(500), move || {
                match save(&text) {
                    Ok(()) => status.set_label(&tr("Gespeichert.")),
                    Err(err) => status.set_label(&tr("Fehler beim Speichern: {err}").replace("{err}", &err.to_string())),
                }
                status.set_visible(true);
                if has_reset {
                    reset_button.set_sensitive(is_customized());
                }
                *debounce_inner.borrow_mut() = None;
                glib::ControlFlow::Break
            });
            *debounce.borrow_mut() = Some(id);
        });
    }

    if let Some(reset_fn) = reset_fn {
        let buffer = buffer.clone();
        let status = status.clone();
        let reset_button_for_click = reset_button.clone();
        let load = Rc::new(load);
        let load_for_click = load.clone();
        reset_button.connect_clicked(move |_| {
            if let Err(err) = reset_fn() {
                status.set_label(&tr("Fehler beim Zurücksetzen: {err}").replace("{err}", &err.to_string()));
                status.set_visible(true);
                return;
            }
            *suppress_autosave.borrow_mut() = true;
            buffer.set_text(&load_for_click());
            status.set_label(&tr("Auf Standard zurückgesetzt."));
            status.set_visible(true);
            reset_button_for_click.set_sensitive(false);
        });
    }

    let box_ = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(8).margin_top(8).margin_bottom(8).margin_start(8).margin_end(8).build();
    box_.append(&reset_button);
    box_.append(&scroller);
    box_.append(&status);

    let row = gtk4::ListBoxRow::builder().child(&box_).activatable(false).selectable(false).build();
    row.add_css_class("prompt-editor-row");
    let _ = id;
    (row, status)
}

fn build_custom_prompts_group(custom_prompts_menu: &gio::Menu) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(tr("Eigene Prompts")).build();
    group.set_description(Some(&tr("Eigene KI-Aktionen, die zusätzlich im Editor-Kontextmenü erscheinen.")));

    let add_button = gtk4::Button::from_icon_name("list-add-symbolic");
    add_button.set_tooltip_text(Some(&tr("Neuen Prompt hinzufügen")));
    add_button.add_css_class("flat");
    group.set_header_suffix(Some(&add_button));

    let prompts: Rc<RefCell<Vec<CustomPrompt>>> = Rc::new(RefCell::new(aiprompts::load_custom_prompts()));
    let rows: Rc<RefCell<Vec<adw::ExpanderRow>>> = Rc::new(RefCell::new(Vec::new()));

    fn persist_and_refresh_menu(prompts: &[CustomPrompt], custom_prompts_menu: &gio::Menu) {
        let _ = aiprompts::save_custom_prompts(prompts);
        crate::aimenu::rebuild_custom_prompts_menu(custom_prompts_menu, prompts);
    }

    fn rebuild_rows(
        group: &adw::PreferencesGroup,
        rows: &Rc<RefCell<Vec<adw::ExpanderRow>>>,
        prompts: &Rc<RefCell<Vec<CustomPrompt>>>,
        custom_prompts_menu: &gio::Menu,
    ) {
        for row in rows.borrow_mut().drain(..) {
            group.remove(&row);
        }
        for index in 0..prompts.borrow().len() {
            let expander = build_custom_prompt_row(index, prompts.clone(), rows.clone(), group.clone(), custom_prompts_menu.clone());
            group.add(&expander);
            rows.borrow_mut().push(expander);
        }
    }

    fn build_custom_prompt_row(
        index: usize,
        prompts: Rc<RefCell<Vec<CustomPrompt>>>,
        rows: Rc<RefCell<Vec<adw::ExpanderRow>>>,
        group: adw::PreferencesGroup,
        custom_prompts_menu: gio::Menu,
    ) -> adw::ExpanderRow {
        let prompt = prompts.borrow()[index].clone();
        let expander = adw::ExpanderRow::builder()
            .title(if prompt.title.is_empty() { tr("Neuer Prompt") } else { prompt.title.clone() })
            .use_markup(false)
            .build();

        let title_row = adw::EntryRow::builder().title(tr("Titel")).text(prompt.title.as_str()).build();
        {
            let prompts = prompts.clone();
            let custom_prompts_menu = custom_prompts_menu.clone();
            let expander_for_title = expander.clone();
            let debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
            title_row.connect_changed(move |row| {
                if let Some(id) = debounce.borrow_mut().take() {
                    id.remove();
                }
                let text = row.text().to_string();
                let prompts = prompts.clone();
                let custom_prompts_menu = custom_prompts_menu.clone();
                let expander_for_title = expander_for_title.clone();
                let debounce_inner = debounce.clone();
                let id = glib::timeout_add_local(Duration::from_millis(500), move || {
                    if let Some(p) = prompts.borrow_mut().get_mut(index) {
                        p.title = text.clone();
                    }
                    expander_for_title.set_title(&if text.is_empty() { tr("Neuer Prompt") } else { text.clone() });
                    persist_and_refresh_menu(&prompts.borrow(), &custom_prompts_menu);
                    *debounce_inner.borrow_mut() = None;
                    glib::ControlFlow::Break
                });
                *debounce.borrow_mut() = Some(id);
            });
        }

        let template_buffer = gtk4::TextBuffer::new(None::<&gtk4::TextTagTable>);
        template_buffer.set_text(&prompt.template);
        let template_view = gtk4::TextView::builder()
            .buffer(&template_buffer)
            .wrap_mode(gtk4::WrapMode::WordChar)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(8)
            .right_margin(8)
            .build();
        let template_scroller = gtk4::ScrolledWindow::builder().child(&template_view).vexpand(false).min_content_height(120).build();
        template_scroller.add_css_class("card");
        {
            let prompts = prompts.clone();
            let custom_prompts_menu = custom_prompts_menu.clone();
            let debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
            template_buffer.connect_changed(move |buf| {
                if let Some(id) = debounce.borrow_mut().take() {
                    id.remove();
                }
                let text = buf.text(&buf.start_iter(), &buf.end_iter(), false).to_string();
                let prompts = prompts.clone();
                let custom_prompts_menu = custom_prompts_menu.clone();
                let debounce_inner = debounce.clone();
                let id = glib::timeout_add_local(Duration::from_millis(500), move || {
                    if let Some(p) = prompts.borrow_mut().get_mut(index) {
                        p.template = text.clone();
                    }
                    persist_and_refresh_menu(&prompts.borrow(), &custom_prompts_menu);
                    *debounce_inner.borrow_mut() = None;
                    glib::ControlFlow::Break
                });
                *debounce.borrow_mut() = Some(id);
            });
        }

        let delete_button = gtk4::Button::from_icon_name("user-trash-symbolic");
        delete_button.set_tooltip_text(Some(&tr("Diesen Prompt löschen")));
        delete_button.add_css_class("flat");
        {
            let prompts = prompts.clone();
            let rows = rows.clone();
            let group = group.clone();
            let custom_prompts_menu = custom_prompts_menu.clone();
            delete_button.connect_clicked(move |_| {
                if index < prompts.borrow().len() {
                    prompts.borrow_mut().remove(index);
                }
                persist_and_refresh_menu(&prompts.borrow(), &custom_prompts_menu);
                rebuild_rows(&group, &rows, &prompts, &custom_prompts_menu);
            });
        }

        let template_box = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(4).margin_top(4).margin_bottom(8).margin_start(8).margin_end(8).build();
        let template_label = gtk4::Label::builder().label(tr("Prompt-Text")).xalign(0.0).build();
        template_label.add_css_class("dim-label");
        template_box.append(&template_label);
        template_box.append(&template_scroller);
        let template_row = gtk4::ListBoxRow::builder().child(&template_box).activatable(false).selectable(false).build();

        expander.add_suffix(&delete_button);
        expander.add_row(&title_row);
        expander.add_row(&template_row);
        expander
    }

    rebuild_rows(&group, &rows, &prompts, custom_prompts_menu);

    {
        let prompts = prompts.clone();
        let rows = rows.clone();
        let group_for_add = group.clone();
        let custom_prompts_menu = custom_prompts_menu.clone();
        add_button.connect_clicked(move |_| {
            prompts.borrow_mut().push(CustomPrompt {
                id: aiprompts::new_custom_prompt_id(),
                title: String::new(),
                template: String::new(),
            });
            persist_and_refresh_menu(&prompts.borrow(), &custom_prompts_menu);
            rebuild_rows(&group_for_add, &rows, &prompts, &custom_prompts_menu);
        });
    }

    group
}
