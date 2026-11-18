//! "Affiliate-Link einfügen …": an Amazon product address (in any of
//! Amazon's shapes) or a bare ASIN becomes a clean partner link,
//! `https://www.amazon.de/dp/ASIN?tag=…`, inserted as a plain Markdown
//! link. Uploading gives it `rel="sponsored nofollow noopener"` by its
//! address (`gutenberg::affiliate`), so the theme marks it. The partner
//! tag is remembered per blog (`affiliate_tag` in `sites.conf`).

use std::rc::Rc;

use adw::prelude::*;
use gtk4::glib;

use crate::i18n::tr;
use crate::{formatting, wpsite};

pub fn open(parent: &adw::ApplicationWindow, buffer: &sourceview5::Buffer) {
    let site = wpsite::load();
    // The selection is gone by the time the dialog closes (focus moves
    // to it) - the range is kept as marks to replace it then.
    let selection = buffer.selection_bounds().map(|(start, end)| (buffer.create_mark(None, &start, true), buffer.create_mark(None, &end, false)));
    let selected = buffer.selection_bounds().map(|(start, end)| buffer.text(&start, &end, false).to_string()).unwrap_or_default();

    let address_row = adw::EntryRow::builder().title(tr("Amazon-Adresse oder ASIN")).build();
    let text_row = adw::EntryRow::builder().title(tr("Linktext")).text(selected.trim()).build();
    let tag_row = adw::EntryRow::builder().title(tr("Partner-Tag")).text(site.affiliate_tag.as_deref().unwrap_or_default()).build();
    let group = adw::PreferencesGroup::new();
    group.add(&address_row);
    group.add(&text_row);
    group.add(&tag_row);

    let result_label = gtk4::Label::builder().xalign(0.0).wrap(true).wrap_mode(gtk4::pango::WrapMode::WordChar).selectable(true).build();
    result_label.add_css_class("caption");
    result_label.add_css_class("dim-label");
    let content = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(12).build();
    content.append(&group);
    content.append(&result_label);

    let dialog = adw::AlertDialog::builder()
        .heading(tr("Affiliate-Link einfügen"))
        .body(tr("Der Link geht als „sponsored“ hoch, das Blog kennzeichnet ihn als Affiliate-Link."))
        .extra_child(&content)
        .build();
    dialog.add_responses(&[("cancel", &tr("Abbrechen")), ("insert", &tr("Einfügen"))]);
    dialog.set_response_appearance("insert", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("insert"));
    dialog.set_close_response("cancel");

    let update: Rc<dyn Fn()> = {
        let (address_row, text_row, tag_row, result_label, dialog) = (address_row.clone(), text_row.clone(), tag_row.clone(), result_label.clone(), dialog.clone());
        Rc::new(move || {
            let address = address_row.text().to_string();
            // A pasted partner link brings its tag along when none is set.
            if tag_row.text().trim().is_empty() {
                if let Some(tag) = gutenberg::affiliate::amazon_tag(&address) {
                    tag_row.set_text(&tag);
                }
            }
            let link = gutenberg::affiliate::amazon_link(&address, &tag_row.text());
            let ready = link.is_some() && !tag_row.text().trim().is_empty() && !text_row.text().trim().is_empty();
            result_label.set_label(&match &link {
                Some(url) if tag_row.text().trim().is_empty() => format!("{url}\n{}", tr("Ohne Partner-Tag ist es kein Affiliate-Link.")),
                Some(url) => url.clone(),
                None if address.trim().is_empty() => tr("Adresse einer Produktseite oder ASIN (z. B. B0D8JCN5P4) eintragen."),
                None => tr("Keine Produktseite erkannt – gesucht wird die ASIN nach /dp/ oder /gp/product/."),
            });
            dialog.set_response_enabled("insert", ready);
        })
    };
    for row in [&address_row, &text_row, &tag_row] {
        let update = update.clone();
        row.connect_changed(move |_| update());
    }
    update();

    // An Amazon address on the clipboard is the usual case: fill it in.
    {
        let address_row = address_row.clone();
        parent.clipboard().read_text_async(None::<&gtk4::gio::Cancellable>, move |text| {
            let Ok(Some(text)) = text else { return };
            let text = text.trim();
            if address_row.text().is_empty() && gutenberg::affiliate::amazon_link(text, "").is_some() && !text.contains(char::is_whitespace) {
                address_row.set_text(text);
            }
        });
    }

    {
        let address_row = address_row.clone();
        let buffer = buffer.clone();
        let site_id = site.site_id();
        let known_tag = site.affiliate_tag.clone().unwrap_or_default();
        dialog.connect_response(None, move |_, response| {
            let marks = selection.clone();
            let release = |buffer: &sourceview5::Buffer| {
                if let Some((start, end)) = &marks {
                    buffer.delete_mark(start);
                    buffer.delete_mark(end);
                }
            };
            if response != "insert" {
                release(&buffer);
                return;
            }
            let tag = tag_row.text().trim().to_string();
            let Some(url) = gutenberg::affiliate::amazon_link(&address_row.text(), &tag) else {
                release(&buffer);
                return;
            };
            if let Some((start, end)) = &marks {
                buffer.select_range(&buffer.iter_at_mark(start), &buffer.iter_at_mark(end));
            }
            formatting::insert_existing_link(&buffer, text_row.text().trim(), &url);
            release(&buffer);
            if tag != known_tag && !site_id.is_empty() {
                if let Err(err) = wpsite::save_affiliate_tag(&site_id, &tag) {
                    eprintln!("affiliate: Partner-Tag nicht gespeichert: {err}");
                }
            }
        });
    }
    dialog.present(Some(parent));
    glib::idle_add_local_once(move || {
        address_row.grab_focus();
    });
}
