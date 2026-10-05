# Translations for a linked blog

Blocksatz keeps a post and its version for a second blog in another
language together - built for linuxundich.de and its English edition at
linuxundich.de/en/, but nothing in it is specific to that blog.

Translating by hand, or with DeepL, ChatGPT or any other tool, is the
normal case. Blocksatz takes care of the copying back and forth, the
links between the two versions and what changes later. Its own AI
translation is one option among others and can be switched off.

## The idea

- The original stays the original. Original and translation are a
  **language pair**: one library folder holding `artikel.md` (the original)
  and `artikel.en.md` (the translation), each with frontmatter of its own
  (title, slug, tags, blog, post id, sync state, alt texts). The images in
  the folder are shared. The translation is tied to the target blog through
  `wp_site`, to its original through the `translation_*` frontmatter keys,
  and carries `lang: "en"`. The sidebar shows one row per pair with the
  state of each language ("DE Veröffentlicht · EN Entwurf").
  Translations made before (in a folder of their own) are moved next to
  their original on start.
- **DE · EN** in the header bar (Alt+1 / Alt+2) switches between the two
  files of a pair at the same section and paragraph. Spell checking
  follows the file's language.
- **The language decides the blog.** A file without `wp_site` uploads to
  the blog of its language: `artikel.md` to the first blog without a
  language path, `artikel.en.md` to the blog whose address ends in `/en`.
  The active blog only decides what the archive ("Im Blog") lists.
  Autosave, upload, sync state and the release check work on it like on
  any other article.
- The right-hand pane shows **the other language** ("EN" while the
  original is open, "DE" while the translation is), rendered like the
  preview and following the editor's cursor section by section; the
  matching paragraph is highlighted. Next to a translation, sections
  already translated get a green edge, sections of the original changed
  since an orange one.
- Before **every upload** the text's language (German or English, judged
  by common words outside code) is compared with the target blog's; a
  mismatch asks "Falsche Sprache für diesen Blog?" before anything is
  sent.

## Starting a language version

Switching to a language without a file yet shows a start page in place
of the editor. "Fassung anlegen" creates `artikel.en.md`, linked to the
original and its blog, with categories (mapped, see below), tags and the
featured image taken over, and starts it with one of:

- **Original als Vorlage** (default): the original's text, title and
  excerpt. Images, code blocks, containers and links are already in
  place; you overwrite the sentences.
- **Aus der Zwischenablage**: a translation copied from DeepL, a chat or
  elsewhere, read like "Übersetzung einfügen" below.
- **Leer**: no text at all.

The default is set under Einstellungen → Übersetzung. This works before
the original is on its blog too; the translation can only be uploaded once
the original has a post id, which is then filled into the link.

Below the choices sits "Per KI übersetzen …" - see "The AI translation".

## Copying to DeepL, a chat or elsewhere

**Original kopieren** (Ctrl+Shift+C, also in the main action's menu)
puts the original's text on the clipboard; **Abschnitt des Originals
kopieren** only the section at the cursor. Whatever a translator must not
change is replaced by numbered placeholders (`⟦CODE-3⟧`, `⟦URL-1⟧`, …):
code blocks, inline code, link and image targets, bare URLs, HTML tags and
comments, attribute lists (`{#anchor .class key=value}`), container
markers (`::: details`) and footnote markers (`[^1]`). Image captions, alt
texts, link texts and container titles stay translatable. DeepL and chats
leave the placeholders alone (tested with DeepL's web translator).
Placeholders are numbered over the whole article, so a section pasted on
its own still finds its originals. Switch the protection off under
Einstellungen → Übersetzung to copy plain Markdown.

**Übersetzung einfügen** (Ctrl+Shift+V) in the translation reads the
clipboard and

- puts the placeholders back;
- straightens the curly quotes DeepL puts around image and link titles
  (`(bild.webp “Title”)` → `(bild.webp "Title")`), which Markdown would
  otherwise not take as a title;
- drops a code fence a chat wraps around its whole answer;
- takes title and excerpt from a header, if the text starts with one:

  ```markdown
  # Title

  Excerpt

  ---

  The text …
  ```

- replaces the whole text - or, if the pasted text is a single section
  (at most one `## ` heading) and the original has several, only the
  section at the cursor;
- checks the result against the original (see "Checks") and says what
  doesn't match, with "Zeigen" opening the list.

Undo restores the text from before the paste.

## When the original changes

The translation's banner counts the sections of the original changed
since the translation was last brought up to date ("Original geändert: 2
Abschnitte offen"). **Zeigen** opens the original next to the editor at
the first one. Above each changed section a note shows what changed, word
by word, and two buttons: **Kopieren** puts the section on the clipboard
(protected, as above), **Erledigt** marks it as carried over. When the
last one is done, the translation counts as current again; with only
title or excerpt changed, "Zeigen" marks it current right away. "Als
aktuell markieren" in the main action's menu does it for all at once.

The word diff needs the original as it was: Blocksatz keeps its sections
next to the translation in `.artikel.en.basis.json` (written when the
version is created, translated or brought up to date). For older
translations without it, the note only says "Neu oder geändert" until the
first "Erledigt".

## The AI translation

"Per KI übersetzen …" on the start page or in the main action's menu
("Per KI aktualisieren …" for an existing translation) is there as long as
"KI-Übersetzung anbieten" is on in Einstellungen → Übersetzung, and works
once the original is on its blog.

- Pick a model for the task "Übersetzung" under Einstellungen →
  KI-Modelle (otherwise the chat's model is used), and replace the generic
  prompt under Einstellungen → KI-Prompts → Übersetzung with your blog's
  voice, conventions and glossary.
- The model translates **section by section** (split at `## `), so long
  articles never hit a model's output limit, and an update only sends the
  sections whose original changed; your corrections in the others stay.
- The same placeholders as for copying keep code, links and markup away
  from the model. Attributes in block comments (`<!-- wp:… {…} -->`) stay
  as they are; the rendered HTML of the block is what readers see.
- Tags are always translated. Before translating, the target blog's tags
  and categories are fetched; the model gets the existing tags to reuse,
  and every tag or category that equals an existing one ignoring case
  takes that spelling.
- **Gegenlesen …** shows original and translation side by side with the
  checks. An AI translation can only be published, and is only linked on
  the blog, once it's marked as reviewed. Translations you made yourself
  need no such mark; for them the dialog is called "Prüfen".

"Kategorien zuordnen" under Einstellungen → Übersetzung maps category
names for every way of starting, one per line: `Allgemein = General`.

## Files and metadata

A translation's frontmatter:

```yaml
lang: "en"                              # language of this file
wp_site: "linuxundich.de/en"            # target blog
translation_of: "linuxundich.de#45505"  # original blog and post id
translation_lang: "en"
translation_source_hash: "9c1f…"        # fingerprint of the original when translated
translation_sections: ["a1b2c3d4", …]   # hash of each original section
translated_at: "2026-10-03"
translation_reviewed: true
```

Images already uploaded with the original point at their WordPress URLs,
so the second blog doesn't get the same files again. Images not uploaded
yet and the featured image are shared from the pair's folder and uploaded
to the target blog like any local image. (Only an original outside the
library gets a translation in a new folder of its own, with copies of its
local images.) If the original only
knows its featured image as a media id of its blog (because it was opened
from the blog), the translation gets that image's URL from the blog's public
REST API, and the upload copies the file into the target blog.

On upload the link goes along as post meta:

| Meta key | Value |
|---|---|
| `lui_source_id` | the original's post id |
| `lui_source_hash` | `translation_source_hash` |
| `lui_source_translated` | `translated_at` |
| `lui_source_reviewed` | `translation_reviewed` |

These are the keys the WordPress plugin **lui-translations** registers; it
uses them for hreflang links, a language switcher and a note under the
byline. A blog without that plugin drops the keys silently, the same way it
drops RankMath's.

## Checks

After pasting, after an AI translation, in "Prüfen …"/"Gegenlesen …" and
in the release check before publishing:

- every protected part (code, links, markup, attributes, footnote markers)
  appears in the translation exactly as in the original - as a set, so
  reordering within a sentence is fine;
- as many headings as in the original;
- no section with four or more words that look like the source language
  (German only for now - a short word list plus umlauts);
- placeholders lost or duplicated.

The release check also warns about changes of the original not carried
over yet, and about a title or excerpt still the original's.

## Code

- `src/translate.rs` - the pure part: sections, placeholders, copy and
  paste, word diff, checks, category mapping, building and updating the
  translated document. Tested with a fake model; `cargo test real_library -- --ignored` round-trips the
  placeholders over the real library.
- `src/translatedialog.rs` - the start page, creating a version, copy and
  paste, "Erledigt" per section, "Per KI übersetzen …" (worker thread,
  progress) and "Prüfen …"/"Gegenlesen …". `cargo test live_translation
  -- --ignored` translates one article through the configured model (see
  the test for the environment variables; it writes nothing to the
  library).
- `src/translationsettings.rs` - Einstellungen → Übersetzung.
- `src/counterpart.rs` - the other language in the right-hand pane, with
  the marks and notes.
- `src/mainaction.rs` - menu entries and banners; `src/releasecheck.rs` -
  the blocking check; `src/export.rs` - the post meta.
