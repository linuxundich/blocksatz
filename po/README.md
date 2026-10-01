# Translating Blocksatz

Blocksatz's source strings are **German** - that's the app's original
language, not a translation of anything - so a `.po` file here translates
*from* German *to* another language, the reverse of the usual gettext
convention where English is the source.

## Current status

Internationalization is set up and working end-to-end (locale detection,
catalog lookup, a real translation), and applied throughout essentially the
whole UI - every file in `POTFILES.in` (all dialogs, menus, toolbars,
tooltips, toasts, and status/error messages). `po/en.po` is a complete,
real English translation of all 668 extracted strings - no untranslated,
fuzzy or obsolete entries left.

The catalogs drift as soon as source strings are added or reworded, and
nothing in the build fails when they do: `tr()` simply falls back to the
German original, so an out-of-date `en.po` shows up as a German string in
an otherwise English UI rather than as an error. Re-run the extraction and
merge below after any round of UI work, and check the result with
`msgattrib --untranslated po/en.po` and `msgattrib --only-fuzzy po/en.po`
before committing - a `fuzzy` entry is *not* used at runtime, and
`msgmerge`'s guesses are frequently wrong (it once paired "Keine Artikel
gefunden" with "{n} articles found."), so every one of them needs reading
rather than just unmarking.

Deliberately **not** translated, by design:

- **AI prompt content** (`src/aiprompts.rs`'s `default_template`s,
  `src/aialt.rs`'s `DetailLevel::prompt()`, `src/default_prompt.rs`'s
  system prompt) - this is prompt-engineering text sent to an LLM, not UI
  chrome. It stays German so it reads as one coherent, grammatically
  correct instruction regardless of the app's display language (the model
  itself understands German fine) - only the short *labels* for these
  prompts (`aiprompts::builtin_title()`) are translated. Users can already
  edit this text directly in Einstellungen → KI-Prompts if they want it in
  another language.
- **Proper nouns and technical terms**: "Blocksatz", "WordPress",
  "Application Password", "API-Key", provider names (`llm.rs`'s
  `Provider::label()`), keyring item labels (`secrets.rs`) - translating
  these would make them harder to recognize consistently across a locale
  switch, not easier.

**Pluralization / dynamic content**: `tr()` only takes a literal `msgid`
and returns it verbatim or translated - there's no `ngettext`/plural-forms
support wired up. Two conventions handle this without it:

1. **Named placeholders**, filled in with `.replace("{name}", &value)`
   after the `tr()` call (e.g. `tr("Fehler: {err}").replace("{err}",
   &err)`) - the placeholder token itself is part of the translatable
   text, so a translator sees it in context and just needs to preserve it
   verbatim somewhere in their translation.
2. **Never concatenate grammar fragments across a `tr()` boundary.** An
   earlier version of `mediapanel.rs::summary_text` built one German
   template and filled a `{plural}`/`{verb}` placeholder with hardcoded
   German suffixes ("ern"/"haben") - translating the template to English
   would have left literal German words stuck in the English sentence,
   since those fragments never went through `tr()` themselves. The fix:
   every count-dependent case (0/1/n, as needed) is its own complete,
   self-contained `tr()` call - see `mediapanel.rs::summary_text` and
   `searchbar.rs::count_label_text` for the pattern. A little more
   repetition in the Rust source, but each translation is a real,
   grammatically correct sentence in the target language.

**Installed builds:** a `cargo build`/`cargo run` from this source tree
reads `po/locale` via `CARGO_MANIFEST_DIR`. The Flatpak manifest instead
installs the compiled `.mo` files to
`/app/share/locale/<lang>/LC_MESSAGES/blocksatz.mo` and sets
`BLOCKSATZ_LOCALEDIR=/app/share/locale` at compile time, which
`i18n::init()` prefers when set.

Adding a genuinely new string anywhere in the app is the same mechanical
step as always: wrap the literal in `i18n::tr("...")` (see
`src/i18n.rs`'s doc comment), add the file to `POTFILES.in` if it isn't
there yet, and re-extract/re-translate as below.

## Adding a translation for a new language

1. Copy the template and start from it:
   ```sh
   msginit --input=po/blocksatz.pot --locale=<lang> --output-file=po/<lang>.po
   ```
   (`<lang>` is a locale code like `fr`, `es`, `pt_BR`.)
2. Edit `po/<lang>.po`, filling in `msgstr` for each `msgid`.
3. `cargo build` (or `cargo run`) automatically compiles it to
   `po/locale/<lang>/LC_MESSAGES/blocksatz.mo` via `build.rs` - nothing
   else to wire up.
4. To see it without changing your desktop's language, run with the
   locale forced for just that one process:
   ```sh
   LANGUAGE=<lang> cargo run
   ```

## Updating `.pot`/existing `.po` files after changing source strings

Re-extract every marked string from the files listed in `POTFILES.in`:

```sh
xgettext --keyword=tr --language=C --from-code=UTF-8 \
  --package-name=Blocksatz --package-version="$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)" \
  --copyright-holder="Christoph Langner" \
  --msgid-bugs-address="https://github.com/linuxundich/blocksatz/issues" \
  -o po/blocksatz.pot $(cat po/POTFILES.in)
```

`--language=C` is deliberate: `xgettext` has no native Rust mode, but
Rust's `"..."` string literals are close enough to C's that this works for
the plain (non-raw, non-byte) strings `tr()` is ever called with. It will
print a harmless warning like `Zeichenkonstante nicht korrekt terminiert`
("unterminated character constant") for every `'static`/named-lifetime
annotation in a scanned file, since the C lexer misreads a lifetime's
leading `'` as starting a char literal - extraction still completes
correctly past it, so this specific warning can be ignored.

Then bring each existing `.po` up to date with any new/changed/removed
`msgid`s, preserving its existing translations:

```sh
msgmerge --update po/en.po po/blocksatz.pot
```

**Important - only strings that appear as a literal argument at the
`tr(...)` call site itself are found.** `xgettext` cannot see through a
variable: `tr(some_variable)` extracts nothing, even if `some_variable`
only ever holds one of a few known literals (e.g. from a `match`). Always
call `tr("the literal text")` directly at each place it's needed, even if
that means calling it once per `match` arm instead of once on a shared
variable afterward - see `document.rs`'s `PostStatus::label()` and
`shortcuts.rs`'s `group()` for the pattern.

## Validating a translation

```sh
msgfmt --check --statistics -o /dev/null po/<lang>.po
```

Reports syntax errors and how many strings are translated/fuzzy/untranslated.
