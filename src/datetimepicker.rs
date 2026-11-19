//! A calendar-and-clock popover for the `JJJJ-MM-TT HH:MM` entry rows
//! (schedule in the release check, "Veröffentlichungstermin" and
//! "Veröffentlicht am" in the post pane). The entry stays editable; the
//! button next to it opens a `Gtk.Calendar` with hour and minute spinners
//! and writes the picked moment back into the entry, which then runs its
//! usual parsing and validation.

use adw::prelude::*;
use gtk4::glib;

use crate::document;
use crate::i18n::tr;

/// Adds the calendar button as a suffix to `row`.
pub fn attach(row: &adw::EntryRow) {
    let calendar = gtk4::Calendar::new();
    let hour = time_spin(23, 1.0, 6.0);
    let minute = time_spin(59, 5.0, 15.0);
    let colon = gtk4::Label::new(Some(":"));
    colon.add_css_class("title-3");
    let clock_icon = gtk4::Image::from_icon_name("preferences-system-time-symbolic");
    let clock = gtk4::Box::builder().spacing(6).halign(gtk4::Align::Center).build();
    clock.append(&clock_icon);
    clock.append(&hour);
    clock.append(&colon);
    clock.append(&minute);

    let now_button = gtk4::Button::builder().label(tr("Jetzt")).build();
    let apply = gtk4::Button::builder().label(tr("Übernehmen")).build();
    apply.add_css_class("suggested-action");
    let actions = gtk4::Box::builder().spacing(6).homogeneous(true).build();
    actions.append(&now_button);
    actions.append(&apply);

    let content = gtk4::Box::builder().orientation(gtk4::Orientation::Vertical).spacing(12).margin_top(6).margin_bottom(6).margin_start(6).margin_end(6).build();
    content.append(&calendar);
    content.append(&clock);
    content.append(&actions);

    let popover = gtk4::Popover::builder().child(&content).build();
    let button = gtk4::MenuButton::builder()
        .icon_name("x-office-calendar-symbolic")
        .tooltip_text(tr("Datum und Uhrzeit wählen"))
        .valign(gtk4::Align::Center)
        .popover(&popover)
        .build();
    button.add_css_class("flat");
    row.add_suffix(&button);

    // Opening: start from what the entry holds, else the next full hour.
    {
        let row = row.clone();
        let (calendar, hour, minute) = (calendar.clone(), hour.clone(), minute.clone());
        popover.connect_show(move |_| {
            let start = document::parse_scheduled_at(&row.text()).and_then(|iso| from_iso(&iso)).unwrap_or_else(next_full_hour);
            show(&start, &calendar, &hour, &minute);
        });
    }
    {
        let (calendar, hour, minute) = (calendar.clone(), hour.clone(), minute.clone());
        now_button.connect_clicked(move |_| {
            if let Ok(now) = glib::DateTime::now_local() {
                show(&now, &calendar, &hour, &minute);
            }
        });
    }
    let commit = {
        let row = row.clone();
        let popover = popover.clone();
        let (calendar, hour, minute) = (calendar.clone(), hour.clone(), minute.clone());
        move || {
            let date = calendar.date();
            row.set_text(&format!("{:04}-{:02}-{:02} {:02}:{:02}", date.year(), date.month(), date.day_of_month(), hour.value_as_int(), minute.value_as_int()));
            popover.popdown();
        }
    };
    {
        let commit = commit.clone();
        apply.connect_clicked(move |_| commit());
    }
    // A double click on a day picks it right away.
    {
        let gesture = gtk4::GestureClick::new();
        gesture.connect_released(move |_, n, _, _| {
            if n == 2 {
                commit();
            }
        });
        calendar.add_controller(gesture);
    }
}

/// An hour or minute spinner that always shows two digits.
fn time_spin(max: u32, step: f64, page: f64) -> gtk4::SpinButton {
    let spin = gtk4::SpinButton::with_range(0.0, max as f64, step);
    spin.set_increments(step, page);
    spin.set_wrap(true);
    spin.set_numeric(true);
    spin.set_width_chars(2);
    spin.set_orientation(gtk4::Orientation::Vertical);
    spin.connect_output(|spin| {
        spin.set_text(&format!("{:02}", spin.value_as_int()));
        glib::Propagation::Stop
    });
    spin
}

fn show(at: &glib::DateTime, calendar: &gtk4::Calendar, hour: &gtk4::SpinButton, minute: &gtk4::SpinButton) {
    calendar.set_date(at);
    hour.set_value(at.hour() as f64);
    minute.set_value(at.minute() as f64);
}

/// `YYYY-MM-DDTHH:MM:00` (from `parse_scheduled_at`) as local time.
fn from_iso(iso: &str) -> Option<glib::DateTime> {
    let (date, time) = iso.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i32>().ok());
    let mut t = time.split(':').map(|p| p.parse::<i32>().ok());
    let (y, mo, da) = (d.next()??, d.next()??, d.next()??);
    let (h, mi) = (t.next()??, t.next()??);
    glib::DateTime::new(&glib::TimeZone::local(), y, mo, da, h, mi, 0.0).ok()
}

fn next_full_hour() -> glib::DateTime {
    let now = glib::DateTime::now_local().expect("local time");
    now.add_hours(1).and_then(|t| t.add_minutes(-t.minute())).and_then(|t| t.add_seconds(-t.seconds())).unwrap_or(now)
}
