//! A link clicked in the preview opens in a popup browser instead of
//! leaving the editor: browse on from there, and "Adresse kopieren" puts
//! whatever page is open right now on the clipboard - to paste it into
//! the article as a link. Uses the app's browser view (`browser.rs`), so
//! it shares the session, cookies and ad blocking of the Browser tab.

use std::rc::Rc;

use adw::prelude::*;

use crate::browser::BrowserView;
use crate::i18n::tr;

pub fn open(parent: &impl IsA<gtk4::Widget>, uri: &str) {
    let browser = Rc::new(BrowserView::new_blank());

    let title = adw::WindowTitle::new(&tr("Link"), uri);
    let copy_button = gtk4::Button::with_label(&tr("Adresse kopieren"));
    copy_button.add_css_class("suggested-action");
    copy_button.set_tooltip_text(Some(&tr("Die Adresse der gerade geöffneten Seite in die Zwischenablage kopieren")));
    let external_button = gtk4::Button::from_icon_name("web-browser-symbolic");
    external_button.set_tooltip_text(Some(&tr("Im Webbrowser öffnen")));

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title));
    header.pack_start(&external_button);
    header.pack_end(&copy_button);

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&browser.widget));
    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&toasts));

    let dialog = adw::Dialog::builder().content_width(1100).content_height(800).child(&toolbar_view).build();
    dialog.set_title(&tr("Link"));

    {
        let title = title.clone();
        let weak = Rc::downgrade(&browser);
        browser.connect_title_changed(move |page_title| {
            let uri = weak.upgrade().and_then(|b| b.current_uri()).unwrap_or_default();
            title.set_title(page_title.as_deref().filter(|t| !t.is_empty()).unwrap_or(&tr("Link")));
            title.set_subtitle(&uri);
        });
    }
    {
        let browser = browser.clone();
        let toasts = toasts.clone();
        copy_button.connect_clicked(move |button| {
            let Some(uri) = browser.current_uri() else { return };
            button.clipboard().set_text(&uri);
            toasts.add_toast(adw::Toast::new(&tr("Adresse kopiert: {uri}").replace("{uri}", &uri)));
        });
    }
    {
        let browser = browser.clone();
        let dialog = dialog.clone();
        external_button.connect_clicked(move |_| {
            let Some(uri) = browser.current_uri() else { return };
            let parent = dialog.root().and_downcast::<gtk4::Window>();
            gtk4::UriLauncher::new(&uri).launch(parent.as_ref(), gtk4::gio::Cancellable::NONE, |_| {});
        });
    }
    // The dialog owns the browser view for as long as it's open.
    {
        let browser = browser.clone();
        dialog.connect_closed(move |_| {
            let _ = &browser;
        });
    }

    browser.load_uri(uri);
    dialog.present(Some(parent));
}
