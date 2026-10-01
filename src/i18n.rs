//! Internationalization setup (GNU gettext). Source strings throughout
//! this codebase are German (the app's original/default language, not a
//! translation of anything) - `tr()` marks a string as translatable and
//! looks it up in the current locale's compiled catalog, falling back to
//! returning the German text unchanged if no catalog is bound or no
//! matching locale is found. That fallback is what makes this safe to
//! introduce incrementally: a string nobody has wrapped in `tr()` yet
//! simply stays exactly as it always was.
//!
//! Currently applied to a representative slice of the UI (the main
//! window's header bar/menu/tabs, the "Artikel-Eigenschaften" dialog, and
//! the "Tastenkürzel" window) as a working, end-to-end proof that the
//! pipeline is wired correctly - not yet to every string in the app.
//! Converting the rest is the same mechanical step repeated: wrap a
//! literal in `i18n::tr("...")`, then add its translation to `po/en.po`
//! (or a new `po/<lang>.po`).
//!
//! Deliberately calls `bindtextdomain`/`textdomain` directly rather than
//! using this crate's `TextDomain` convenience builder: that builder ties
//! catalog selection to whatever `setlocale()` actually resolves to
//! (checking a matching directory exists for that *exact* locale before
//! binding anything at all), which fails outright on a system that has
//! the desktop's own locale installed (say, `de_DE.UTF-8`) but not the
//! *target* one (`en_US.UTF-8` - exactly this dev machine's situation, and
//! plausibly a minimal Flatpak runtime's too). Calling `setlocale(LC_ALL,
//! "")` once to adopt the real environment, then binding the domain
//! unconditionally, lets glibc's own runtime fallback chain (`LANGUAGE`,
//! then `LC_MESSAGES`/`LANG`) resolve which catalog to use on each lookup
//! instead - the standard GNOME convention of overriding just one app's
//! language via `LANGUAGE=xx` independent of the desktop's own locale
//! keeps working, and a locale/catalog that doesn't exist yet is simply
//! never found, with no error to handle at all.
//!
//! See `po/README.md` for the translator-facing workflow and `build.rs`
//! for how `.po` files are compiled to the `.mo` catalogs gettext reads.

use gettextrs::LocaleCategory;

const DOMAIN: &str = "blocksatz";

/// Adopts the process's real locale from the environment and binds this
/// app's translation catalogs - must run before any other thread starts
/// (`glibc`'s `setlocale` isn't thread-safe against locale-dependent calls
/// running concurrently on other threads), so this is called first thing
/// in `main()`, before the GTK application or any background thread
/// exists.
///
/// Best-effort throughout: `bindtextdomain`/`bind_textdomain_codeset`/
/// `textdomain` only fail for reasons like an invalid domain name, never
/// because a translation doesn't exist yet for the current language (that
/// case is handled per-lookup by `tr()`'s own fallback) - so a failure
/// here isn't fatal and is only logged, not surfaced to the user; not
/// being able to find optional translation data is not an error a
/// Markdown editor's user needs to hear about.
pub fn init() {
    gettextrs::setlocale(LocaleCategory::LcAll, "");

    // A plain `cargo run` dev build reads freshly `msgfmt`-compiled
    // catalogs straight from the source tree (`build.rs` writes them to
    // `po/locale/<lang>/LC_MESSAGES/blocksatz.mo` on every build).
    // `bindtextdomain` wants the directory containing those `<lang>/...`
    // subdirectories directly, i.e. `po/locale`. The Flatpak build sets
    // `BLOCKSATZ_LOCALEDIR` at compile time (`/app/share/locale`, where
    // its manifest installs those same catalogs), since the source tree
    // it was built from no longer exists at runtime.
    let locale_dir = option_env!("BLOCKSATZ_LOCALEDIR").unwrap_or(concat!(env!("CARGO_MANIFEST_DIR"), "/po/locale"));
    if let Err(err) = gettextrs::bindtextdomain(DOMAIN, locale_dir) {
        eprintln!("i18n: bindtextdomain fehlgeschlagen ({err}) - falle auf Deutsch zurück.");
        return;
    }
    if let Err(err) = gettextrs::bind_textdomain_codeset(DOMAIN, "UTF-8") {
        eprintln!("i18n: bind_textdomain_codeset fehlgeschlagen ({err}) - falle auf Deutsch zurück.");
    }
    if let Err(err) = gettextrs::textdomain(DOMAIN) {
        eprintln!("i18n: textdomain fehlgeschlagen ({err}) - falle auf Deutsch zurück.");
    }
}

/// Translates `msgid` (German source text) via the bound catalog for the
/// current locale, or returns it unchanged if there's no catalog/no match.
pub fn tr(msgid: &str) -> String {
    gettextrs::gettext(msgid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tr_falls_back_to_the_original_german_text_with_no_catalog_bound() {
        // No `init()` call in this test, so no domain is bound - this is
        // exactly the fallback the whole incremental-conversion approach
        // (only some strings wrapped in `tr()` so far) depends on.
        assert_eq!(tr("Ganz und gar kein echter Schlüssel"), "Ganz und gar kein echter Schlüssel");
    }

    /// The one piece of this feature that's actually verifiable without a
    /// running GUI: that `po/en.po`, compiled by `build.rs` into a real
    /// `.mo` catalog, is found and genuinely translates a real string from
    /// this app - not just that the gettext plumbing compiles. Mirrors
    /// `init()`'s own sequence (`setlocale` then bind), but forces
    /// `LANGUAGE=en` afterward rather than depending on whichever locale
    /// happens to be installed on the machine running the test.
    ///
    /// `LANGUAGE` and the bound gettext domain are both process-global C
    /// library state, not thread-local - cargo's default test harness runs
    /// every test in one process, so leaving `LANGUAGE=en` set after this
    /// test would silently translate every other test's `tr()` calls too
    /// (this was a real, reproducible bug: it broke `searchbar`'s
    /// `count_label_text` tests, which assert the untranslated German
    /// fallback). Restoring it - and rebinding to a domain with no catalog,
    /// so gettext has nothing to translate through even if some other test
    /// still has `LANGUAGE=en` from its own environment - undoes both
    /// pieces of global state this test changes.
    #[test]
    fn the_compiled_english_catalog_actually_translates_a_real_string() {
        gettextrs::setlocale(LocaleCategory::LcAll, "");
        let original_language = std::env::var("LANGUAGE").ok();
        std::env::set_var("LANGUAGE", "en");

        let dev_locale_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/po/locale");
        gettextrs::bindtextdomain(DOMAIN, dev_locale_dir).expect("bindtextdomain failed");
        gettextrs::bind_textdomain_codeset(DOMAIN, "UTF-8").expect("bind_textdomain_codeset failed");
        gettextrs::textdomain(DOMAIN).expect("textdomain failed");

        let result = tr("Vorschau");

        match original_language {
            Some(value) => std::env::set_var("LANGUAGE", value),
            None => std::env::remove_var("LANGUAGE"),
        }
        gettextrs::textdomain("blocksatz-test-no-such-domain").ok();

        assert_eq!(result, "Preview");
    }
}
