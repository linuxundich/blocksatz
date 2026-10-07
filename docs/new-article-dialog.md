# New article dialog

*Planned and built 2026-10-06, released in 0.68.0. Mockups: [`mockups/new-article-dialog.html`](mockups/new-article-dialog.html). Code: `src/newarticle.rs` (dialog), `src/textimport.rs` (header and image references), `library::create_prepared`.*

## 1. Today

- **Ctrl+N / "+"** clears the editor. Nothing exists on disk yet.
- With the first keystroke `worksave.rs` creates `~/Dokumente/Blocksatz/<YYYY-MM-DD-HHMM>/artikel.md`.
- Once a title is known (frontmatter or a finished `# ` line), `library::rename_after_title`
  renames the folder to `slugify(title)` – once, and only while it is still auto-named.
- Title and slug are entered later in the properties pane.

Problems:

1. **Folder named after the title, not the slug.** The slug the user types afterwards
   doesn't matter any more; long titles give names like
   `raspberry-pi-imager-2-0-unter-linux-installation-unter-arch-und-ubuntu-root-rechte-cli-und-tipps`.
2. **No way to start from an existing text.** "Datei öffnen…" edits a `.md` in place,
   outside the library; a `.txt` or a file with a Hugo/Jekyll/Obsidian/MultiMarkdown header
   loses its metadata (only Blocksatz' own keys are read, the rest is dropped).
3. **Images arrive one by one.** Drag & drop into the editor copies them into the folder –
   but only after a folder exists, and each drop also inserts an `![]()` at the cursor.
4. Two renames per article (timestamp → title), a moving target for recent files and
   the sidebar.

## 2. Idea

One dialog **"Neuer Artikel"** that creates the folder *once*, with its final name, and
everything in it. Not a multi-step carousel – the GUI redesign removed the export carousel
on purpose (`gui-redesign.md` §5.6). One `AdwDialog` with three groups, top to bottom:

| Group | Contents |
|---|---|
| **Text** | "Leer beginnen" (default) · "Aus Datei…" (`.md`, `.markdown`, `.txt`) · drop a file onto the group |
| **Titel & Adresse** | Title, slug (follows the title until edited), type Beitrag/Seite, folder preview |
| **Bilder** | "Bilder hinzufügen…" · drop zone · thumbnails with remove button; images referenced by the imported text are listed automatically |

Footer: **Abbrechen** · **Anlegen** (suggested action, Enter). The fast path stays:
Ctrl+N, type a title, Enter – or Ctrl+N, Enter for an untitled article as today.

"Assistent" is already the name of the right-hand chat pane, so the dialog is called
"Neuer Artikel" / "Neue Seite", not "Assistent".

## 3. Behavior

### 3.1 Entry points

| Trigger | Opens |
|---|---|
| Ctrl+N, "+" in the sidebar | dialog, type Beitrag |
| Ctrl+Alt+N, menu "Neue Seite" | dialog, type Seite |
| new menu item **"Aus Textdatei…"** | file chooser first, then the dialog pre-filled |
| `.md`/`.txt` dropped onto the library sidebar | dialog pre-filled with that file |
| `.md`/`.txt` dropped onto the dialog | replaces the chosen text |
| images dropped onto the library sidebar | dialog with those images, empty text |

"Datei öffnen…" keeps editing a file in place (unchanged).

### 3.2 Reading a text file

1. Read bytes; UTF-8, else Windows-1252 (old `.txt` exports). Normalize CRLF, strip a BOM.
2. Detect a header, first match wins:
   - YAML block `---` … `---` (Jekyll, Hugo, Obsidian, Pandoc, Blocksatz itself)
   - TOML block `+++` … `+++` (Hugo)
   - MultiMarkdown: `Key: value` lines from the first line up to the first blank line
     (used by `tuxedo-editor`) – only if the first line's key is a known one, so prose
     starting with "Hinweis: …" isn't eaten
   - Pandoc title block `% Title`
3. No title in the header → a leading `# Heading` becomes the title and is removed from the
   body (`document::split_title_heading`, switch "Erste Überschrift als Titel" in the dialog).
4. `.txt` is taken as Markdown as-is (plain paragraphs are valid Markdown).

No YAML crate: a tolerant reader for flat `key: value`, inline lists `[a, b]`, block lists
(`- a`) and one nesting level (`cover:\n  image: x`) covers every header seen in practice.
Pure function, unit-tested: `headerimport::parse(text) -> HeaderImport`.

### 3.3 Header → frontmatter mapping

Keys are compared case-insensitively.

| Header key(s) | Frontmatter | Note |
|---|---|---|
| `title` | `title` | |
| `slug`; last segment of `permalink`/`url` | `slug` | otherwise generated from the title |
| `tags`, `keywords`, `schlagworte` | `tags` | list or comma string |
| `categories`, `category`, `kategorie(n)` | `categories` | names must exist on the blog – unknown ones are marked like typed tags in the properties pane |
| `description`, `excerpt`, `summary`, `abstract` | `excerpt` | |
| `image`, `cover`, `cover.image`, `featured_image`, `thumbnail` | `featured_image` | file copied into the folder |
| `image_alt`, `cover.alt`, `featured_image_alt` | `featured_image_alt` | |
| `lang`, `language` | `lang` | only `de`/`en` style codes |
| `draft: true` / `status` | `status` | anything else → draft |
| `date` in the future | `scheduled_at` + status future | past dates ignored (WordPress sets its own) |
| `seo_title`, `meta_title` · `meta_description` · `focus_keyword` | `rank_math_*` | |
| everything else (`author`, `layout`, `weight`, `aliases`, …) | – | listed as "nicht übernommen" |

**A Blocksatz `artikel.md` as source** is read with `document::parse`, then every blog link is
removed: `wp_post_id`, `wp_site`, `wp_content_hash`, `wp_synced_*`, `wp_modified_gmt`,
`wp_pending_create`, `featured_media_id`, `translation_*`, and the upload ids in `media`.
Otherwise the first upload of the copy would overwrite the original post. The dialog says so
("Als neuer Artikel – ohne Verbindung zum Blogbeitrag").

### 3.4 Images

- **From the text:** every `![…](path)` and `<img src="path">` with a *local* path, resolved
  relative to the source file, is listed with a badge "im Text". On "Anlegen" each is copied
  with `document::adopt_file` and the reference rewritten to the new file name. Missing files
  are listed in a warning row ("2 Bilder im Text nicht gefunden") and stay as they are.
  Remote URLs are left alone.
- **Added by hand** (file chooser, multi-select, or drop): copied into the folder, **not**
  inserted into the text – the media panel already shows unused images in the folder, and
  where they belong is the author's decision. Optional switch "Am Ende einfügen" for the
  screenshot-batch case.
- Accepted types like the editor drop: PNG, JPEG, WebP, GIF, SVG, AVIF. Name clashes get
  `-2`, `-3` (`unique_file_path`). The originals are never moved or changed.
- First image becomes the featured image if the header had none? **No** – a header image or
  an explicit star on a thumbnail ("Als Beitragsbild") only.

### 3.5 Creating

On "Anlegen":

1. `worksave::flush` the current article (as today).
2. Folder name = **slug** (fallback slugify(title), fallback timestamp), via `unique_dir`.
   New `library::create_entry_named(root, name)`; `create_entry` keeps its signature for the
   translation and blog-import callers.
3. Copy images, rewrite references, write `artikel.md` (even with an empty body – the article
   has a title now).
4. `open_document_at_path`; cursor at the start of the body.

Errors (disk full, unreadable source) leave nothing half-made: everything is written into
`.<name>.partial` first (hidden, so the sidebar ignores it) and renamed once complete.

The folder name is final from the start, so `rename_after_title` only remains for articles
started with an empty title (Ctrl+N, Enter) – unchanged behavior for them.

## 4. Implementation

| Step | Files | Test |
|---|---|---|
| 1. Header reader + mapping, pure | new `src/headerimport.rs` | unit tests: YAML, TOML, MMD, Pandoc, Blocksatz-with-wp-ids, prose with a colon in line 1, CRLF/BOM/Latin-1 |
| 2. Image reference scan + rewrite, pure | `media.rs` / `document.rs` | unit tests: relative/absolute/URL/missing, `<img>`, spaces in names |
| 3. `create_entry_named`, partial-folder write | `library.rs` | unit tests in a temp dir |
| 4. Dialog | new `src/newarticle.rs` (`AdwDialog`, `AdwPreferencesGroup`, `GtkFlowBox` thumbnails, `GtkDropTarget` on dialog and image group) | live: all entry points, both themes, narrow window |
| 5. Wiring | `window.rs` (`win.new`, `win.new-page`, new `win.new-from-file`), `librarysidebar.rs` (menu item, sidebar drop target) | live |
| 6. Docs | CHANGELOG `[Unreleased]`, README "Writing", `gui-redesign.md` §5.2, po/ strings | |

Roughly one 0.x minor release.

## 5. Decisions (2026-10-06)

1. Ctrl+N always opens the dialog; Ctrl+N, Enter is the instant untitled start.
2. Folders are named after the slug, else the title - also when an untitled
   article is renamed later (`library::folder_name`).
3. A later slug change does not rename the folder.
4. The source file is copied, never moved.

## 6. Differences from the plan

- Images the text points to have no remove button (the text needs them);
  only added images can be removed.
- The star on a thumbnail replaces a local featured image from the header;
  a remote one (URL) stays as it is.
- `<img src>` and link reference definitions (`[ref]: path`) are rewritten
  too.
- `win.new-blank` empties the editor without the dialog (used when the open
  article is moved to the trash); `win.new-with-files` (`as`) is the entry
  point for drops on the sidebar.
