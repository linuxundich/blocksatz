//! "Über Blocksatz" - a native `Adw.AboutDialog`. Version comes straight
//! from `Cargo.toml` via `CARGO_PKG_VERSION` (nothing to remember to bump
//! here on a release), and the release notes are the actual
//! `CHANGELOG.md` history, converted by `changelog.rs` into the small HTML
//! subset the dialog accepts - so the full version history is browsable
//! right here, not only in the repository.

use adw::prelude::*;

use crate::changelog;
use crate::i18n::tr;

const CHANGELOG_MARKDOWN: &str = include_str!("../CHANGELOG.md");

pub fn open(parent: &impl IsA<gtk4::Widget>) {
    let dialog = adw::AboutDialog::builder()
        .application_name("Blocksatz")
        .application_icon("de.linuxundich.Blocksatz")
        .developer_name("Christoph Langner")
        .version(env!("CARGO_PKG_VERSION"))
        .comments(tr("Markdown-Artikel als WordPress-Gutenberg-Blöcke veröffentlichen"))
        .website("https://github.com/linuxundich/blocksatz")
        .issue_url("https://github.com/linuxundich/blocksatz/issues")
        .copyright("© 2026 Christoph Langner")
        .license_type(gtk4::License::Gpl30)
        .developers(["Christoph Langner"])
        .release_notes(changelog::to_release_notes_html(CHANGELOG_MARKDOWN))
        .build();
    dialog.present(Some(parent));
}
