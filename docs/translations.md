# Translations for a linked blog

Blocksatz can turn a post into a translation for a second blog in another
language - built for linuxundich.de and its English edition at
linuxundich.de/en/, but nothing in it is specific to that blog.

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
  their original on start. Autosave, upload, sync state and the release check work on it like
  on any other article.
- The model translates **section by section** (split at `## `). Long
  articles never hit a model's output limit, and an update only sends the
  sections whose original changed.
- Whatever the model must not change never reaches it: code blocks, inline
  code, link and image targets, bare URLs, HTML tags and comments, attribute
  lists (`{#anchor .class key=value}`), container markers (`::: details`)
  and footnote markers (`[^1]`) are replaced by numbered placeholders
  (`⟦CODE-3⟧`, `⟦URL-1⟧`, …) and put back afterwards. Image captions, alt
  texts, link texts, container titles and verse fences stay translatable,
  and so do the values of `alt`, `title` and `aria-label` inside HTML tags.
  Attributes in block comments (`<!-- wp:… {…} -->`) stay as they are; the
  rendered HTML of the block is what readers see.
- A person reviews before anything goes live. Only a reviewed translation
  can be published, and only a reviewed one is linked on the blog.

## Using it

1. Configure the second blog under Einstellungen → WordPress.
2. Optional: pick a model for the task "Übersetzung" under Einstellungen →
   KI-Modelle (otherwise the chat's model is used), and replace the generic
   prompt under Einstellungen → KI-Prompts → Übersetzung with your blog's
   voice, conventions and glossary. "Kategorien zuordnen" maps category
   names, one per line: `Allgemein = General`.
3. Open a post that is on the blog and pick **Übersetzen …** in the main
   action's menu. Choose the target blog and language; the dialog shows the
   scope and the model.
4. The translation opens in the editor, and **Gegenlesen …** shows original
   and translation side by side with the checks. Correct in the editor,
   then mark it as reviewed.
5. Upload and publish the translation like any other article.

When the original changes later, the translation shows a banner;
**Übersetzung aktualisieren …** re-translates only the changed sections.
Your corrections in the other sections stay. If anything was re-translated,
the translation counts as unreviewed again.

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

After translating and in "Gegenlesen …":

- every protected part (code, links, markup, attributes, footnote markers)
  appears in the translation exactly as in the original - as a set, so
  reordering within a sentence is fine;
- as many headings as in the original;
- no section with four or more words that look like the source language
  (German only for now - a short word list plus umlauts);
- placeholders the model lost or duplicated, after one automatic retry.

## Code

- `src/translate.rs` - the pure part: sections, placeholders, checks,
  category mapping, building and updating the translated document. Tested
  with a fake model; `cargo test real_library -- --ignored` round-trips the
  placeholders over the real library.
- `src/translatedialog.rs` - "Übersetzen …" (worker thread, progress) and
  "Gegenlesen …". `cargo test live_translation -- --ignored` translates one
  article through the configured model (see the test for the environment
  variables; it writes nothing to the library).
- `src/mainaction.rs` - menu entries and banners; `src/releasecheck.rs` -
  the blocking check; `src/export.rs` - the post meta.
