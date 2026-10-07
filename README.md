# Blocksatz

[![CI](https://github.com/linuxundich/blocksatz/actions/workflows/ci.yml/badge.svg)](https://github.com/linuxundich/blocksatz/actions/workflows/ci.yml)

A GNOME (GTK4 + libadwaita) editor for writing blog articles in Markdown
and exporting them as native WordPress **Gutenberg blocks** — not a single
classic/freeform HTML block, but real, individually editable blocks
(`core/paragraph`, `core/heading`, `core/list`, ...) — to a self-hosted
WordPress site via its REST API.

Split-screen editing: Markdown on the left (GtkSourceView, syntax
highlighting), a live HTML preview on the right (WebKit), with Gutenberg
export running in the background against your own WordPress install using
an [Application Password](https://make.wordpress.org/core/2020/11/05/application-passwords-integration-guide/).

## Status

Functionally complete for its core purpose - write Markdown, review a live
preview, and publish/update a real WordPress post as native Gutenberg
blocks. Implemented so far:

- **Workflow from first draft to published post** (see
  [`docs/gui-redesign.md`](docs/gui-redesign.md)) — write locally, upload
  as a draft, correct, upload again, publish:
  - The **library sidebar** on the left (`AdwSidebar`) lists "In Arbeit" -
    every article in the library, most recently changed first, with its
    WordPress status as subtitle and a paper-plane icon for changes not
    uploaded yet - and "Im Blog" with the site's drafts, scheduled,
    published posts, pages and trash and their counts. Picking a group
    opens the **blog archive**: server-side search, 50 posts at a time
    while scrolling, trash/restore; opening a post creates its working
    copy in the library (or reopens the existing one).
  - One **main action** at the end of the header bar says what it does
    for the open article: "Als Entwurf hochladen" (the first upload is
    always a draft), "Entwurf aktualisieren", "Veröffentlichen …",
    "Änderungen veröffentlichen …" - with submit for review, update and
    preview, compare, schedule, revert to draft and discard in its menu. The window
    title shows the article and its state ("Entwurf · nicht hochgeladen").
  - **Publishing goes through a release check**: title, excerpt, category,
    tags, featured image, alt texts, links, focus keyword, each with a way
    to fix it, plus "Sofort / Geplant".
  - A **sync check** (at start, when the window becomes active again, and
    on demand) notices posts changed or deleted on the blog; a banner then
    offers to load the blog's version, resolve a conflict or unlink.
    **"Mit Blog-Fassung vergleichen"** shows a line diff of the working
    copy against the blog's current version before deciding.
  - A first upload interrupted by a dropped connection doesn't create a
    second post on the next try: Blocksatz first looks for the post it
    may already have created.
  - **"Vorschau im Blog"** shows changes to a published post in the blog's
    theme without touching the live post (as a WordPress autosave).
  - **Several blogs**: Einstellungen → WordPress lists them (add, edit,
    remove, pick the active one); the sidebar's footer switches between
    them. "In Arbeit" shows the active blog's working copies and the
    local-only ones; uploads, previews and the sync check of a working
    copy always go to the blog it belongs to.
  - **Translations for a second blog** (see
    [`docs/translations.md`](docs/translations.md)): the English version
    of a post lives next to its original as `artikel.en.md` in the same
    library folder - one sidebar row per article, showing the state of
    each language. The **DE · EN** switch in the header bar (Alt+1 /
    Alt+2) flips between the two at the same paragraph, the right-hand
    pane shows the other language following along, and the language
    decides the blog an upload goes to. Translate by hand from the
    original as a template, or copy the original into DeepL or a chat
    with code and links protected and paste the result back - the
    placeholders come back and the checks say what doesn't match. When
    the original changes, its view marks the changed sections with a word
    diff and an "Erledigt" button each. The AI translation is there as an
    option: section by section, code and markup untouched, and only
    published once you've reviewed it.
  - The **right-hand pane** (F9) has four views: Vorschau (rendered /
    Gutenberg code / "Im Blog" - the open article as the blog itself shows
    it, draft preview or live post), Beitrag (state, all post properties,
    media, statistics), Assistent (chat / evaluation) and a Browser of its
    own (switchable in Einstellungen → Browser).
- **Fold-out terminal** (F12, header-bar toggle) — a terminal below
  editor and pane, starting your own shell in the article's folder (on the
  host when running as a Flatpak, via `flatpak-spawn --host`, with the
  host's `script` providing a proper pty there). Hiding it
  keeps the shell running; `exit` closes the panel. Ctrl+Shift+C/V copy
  and paste, and while it has the focus the app's own shortcuts step
  aside, so Ctrl+N and friends reach the shell. Its height is remembered.
- **Split-pane editor** — the window remembers its size (and whether it was
  maximized) across restarts, together with the layout of its panes: the
  editor/pane split (a ratio, 50/50 to start), whether the sidebar and the
  pane are shown, and the pane's last view; a header-bar toggle button
  collapses the whole right-hand pane for a full-width editor and restores
  it again. Below roughly 700sp of window width (a tiled quarter of a
  typical monitor, or a Linux tablet in portrait) - via `libadwaita`
  breakpoints - the side-by-side split gives way to a single pane at a
  time, switched by an `Adw.InlineViewSwitcher` styled like the sidebar's
  own tab switcher, and reverts automatically once the window is wide
  enough again; the formatting toolbar scrolls horizontally rather than
  clipping if it doesn't fit at that width. A second toggle button next to it (Ctrl+Shift+F) is a
  Fokus-Schreibmodus, additionally hiding the header bar and the editor's
  own formatting toolbar down to just the editor text - `Adw.ToolbarView`'s
  own animated reveal handles the header/status bar, so entering and
  leaving is a smooth slide rather than an abrupt layout jump. A plain launch with no file argument reopens the most
  recent one automatically instead of always starting at a blank
  "Unbenannt" document - `Ctrl+N`, `Enter` still gets to a blank one.
  Markdown editing pane (GtkSourceView, syntax
  highlighting, spell-checking via [`libspelling`](https://gitlab.gnome.org/GNOME/libspelling))
  with a compact formatting toolbar in three groups - inline (bold Ctrl+B,
  italic Ctrl+I, strikethrough Ctrl+Shift+X, code Ctrl+E, link Ctrl+K),
  line/block (heading menu with Ctrl+2/3/4/0, list, numbered list, quote -
  applied to every selected line and toggled back off - code block,
  table) and insert ("Bild einfügen" opening a native image file picker
  instead of typing a filename by hand, plus an "Einfügen" menu with
  video/audio, media library, separator, "Weiterlesen" marker, containers
  and dynamic blocks); "Bestehenden Artikel verlinken"
  opening a searchable picker over the site's existing posts and inserting
  a real Markdown link to the one picked; pasting an image straight from the
  clipboard with Ctrl+V - a screenshot, or "Copy Image" from a browser -
  saves it into the article's own folder and inserts it; pasting rich text
  copied from a browser, word processor, or anywhere else that puts a
  `text/html` entry on the clipboard alongside its plain-text one converts
  that formatting to Markdown on the way in (headings, bold/italic, links,
  lists, tables - via a real HTML5 parser rather than a hand-rolled one, to
  hold up against how varied real-world HTML actually is) instead of
  dropping it, falling through to a normal plain-text paste only when
  there's genuinely no image or HTML on the clipboard; dragging one or
  more local files from a file manager onto the editor inserts them the same
  way, even in an unsaved article; a "Weiterlesen"
  button inserting WordPress's `<!--more-->` marker, exported as a real
  `wp:more` block rather than generic HTML), a Ctrl+F search-and-replace bar
  sliding up from the bottom of the editor (live match highlighting and
  count, next/previous navigation, replace one or all), a debounced live HTML preview kept in scroll-sync with the
  editor (matched by source line, not scroll percentage, so a tall image
  doesn't throw off the sync), and a footer status bar with word count and
  reading time for the whole article - plus the same two numbers for the
  current selection, whenever one is active. The right pane's views are
  "Vorschau" - rendered (follows the app's light/dark
  mode, with a choice of Modern/Klassisch/Sepia typographic styles picked
  in Einstellungen; a caption renders as a small line under its image,
  matching the published post; every image gets small badges in its
  bottom-right corner - an upload arrow once it's on WordPress, "Alt" once
  its alt text is defined (hovering it shows the actual alt text as a
  tooltip, not a generic sentence), and its file format - in that fixed
  order whenever more than one applies, updated live from Medienverwaltung/
  the alt-text dialogs, not just on the next edit; right-clicking an image
  offers "Alternativtext bearbeiten…"/"KI-Alternativtext generieren…"/"Bild
  bearbeiten…" - WebKit's own default image actions (open/save/copy the
  rendered file, copy its address) are trimmed from that menu, alongside
  the navigation items, since none of them apply to an embedded article
  image; hovering a link shows where it goes), Gutenberg code (the exact block HTML that would be published)
  and "Im Blog" -, "Beitrag", whose statistics section has word/character/paragraph counts, estimated reading time, and
  a German-adapted Flesch reading-ease score with a qualitative label -
  expandable into the formula itself, the article's actual average
  words-per-sentence and syllables-per-word, and concrete tips for
  improving the score, derived from whichever of those two numbers is
  actually holding it down),
  and "Assistent" with "Chat" - a writing assistant with message bubbles (replies rendered
  as Markdown), backed by Gemini, ChatGPT, Claude, Groq, or Ollama (self-hosted,
  no API key), with a provider/model picker both in the tab itself and in
  Einstellungen. A message typed here gets the editor's current selection -
  or, if nothing's selected, the whole article - appended before it's sent,
  the same rule the context menu's AI actions below already follow, so the
  model always has the article as context without pasting it in by hand.
  The "Browser" view is a plain `WebKit` view with an address bar
  and back/forward/reload controls, for consulting documentation or the
  live target site without alt-tabbing away - typing a bare domain adds
  `https://` automatically, anything else is sent to Google as a search
  query. Its start page (Startpage by default) is configurable in a
  "Browser" page in Einstellungen, which also holds a "Werbung
  blockieren" toggle (on by default) - basic ad/tracker blocking built on
  the same WebKit content-blocker mechanism GNOME Web itself uses, driven
  by a real EasyList-syntax rule file rather than a hand-coded domain
  list, so it can be extended without a code change.
- **AI actions in the editor's context menu** — right-click the editor for
  "Inhalt prüfen", "Stil & Formatierung prüfen", "Rechtschreibung prüfen",
  "Zeichensetzung prüfen", and "Länge anpassen…"; each sends the selection
  (or the whole article, if nothing's selected) to the Chat tab with a
  matching prompt. All five built-in prompts are editable/resettable in a
  "KI-Prompts" settings page, which also holds your own custom prompts
  (kept separate from the built-ins), reflected in the context menu
  immediately as you edit them.
- **Gutenberg block engine** (`crates/gutenberg`) — a standalone, unit-tested
  library that parses Markdown into a block tree and renders it as
  block-comment-annotated HTML (`<!-- wp:paragraph -->...`), independent of
  the GUI. A local image/video/audio file referenced with `![]()` becomes
  the matching `wp:image`/`wp:video`/`wp:audio` block by its file
  extension, and a bare URL alone on its own line becomes a real
  `wp:embed` block (YouTube, X/Twitter, Vimeo, Instagram, SoundCloud,
  Spotify recognized by name for a nicer immediate block-editor preview;
  any other URL still embeds generically, the same way WordPress's own
  editor falls back to oEmbed discovery for it). Since Markdown has no
  native syntax for side-by-side columns, buttons, a photo gallery, a
  highlighted pullquote, or a collapsible disclosure widget, those are
  written as fenced code blocks with a special "language" tag -
  ` ```columns ` (split into columns on a `+++` line, each side re-parsed
  as ordinary Markdown), ` ```buttons ` (one Markdown link per line),
  ` ```gallery ` (one Markdown image per line), ` ```pullquote ` (quote
  text and an optional citation, split on a `+++` line), and
  ` ```details ` (a summary and its body, also `+++`-split, the body
  re-parsed as ordinary Markdown, `wp:details` in modern WordPress),
  ` ```preformatted ` and ` ```verse ` (lines and spacing kept) - all
  with full round-trip support back to the same Markdown when re-opening
  an existing post. Design Markdown has no syntax for goes into an
  **attribute line** in curly braces below the block (Pandoc/kramdown
  style), using the blog theme's preset slugs: `{bg=accent color=base}`,
  `{gradient=accent-fade}`, `{size=large align=center}`,
  `{style=stripes}`, `{width=100%}`, `{dropcap}`, `{reversed}`, boxes
  with `{padding=1.5rem border="1px solid #ddd" radius=10px shadow=natural}`,
  custom colors (`{color=#1d4ed8}`), typography (`{line-height=2 weight=300
  transform=uppercase}`), `{link-color=warning}`, `{marker=upper-roman}`
  for list numbering and `{aspect=1 scale=cover}` for images; buttons take
  `{justify=center}` below the fence and `[Text](url){style=outline
  radius=0px width=50% newtab}` per button;
  a heading carries it at its end (`## Titel {#anker color=accent}`).
  Tables, galleries and embeds take `{caption="..."}`, tables also
  `{footer}` (last row is the footer); an ordered list starting at `5.`
  keeps its start number. Images follow this app's convention
  `![Bildunterschrift](bild.png "Alternativtext")` - the same brackets
  caption audio and video, and may hold links and emphasis
  (`![Foto: [Name](https://…)](bild.png)`) - and a linked image is plain Markdown,
  `[![BU](bild.png)](ziel)`. A quote's last paragraph starting with an em
  dash becomes its citation: `> — Cicero, *De finibus*`. **Footnotes**
  are written as on GitHub, `Satz.[^1]` with `[^1]: Quelle` anywhere
  (Einfügen → Fußnote numbers and places them), and go out as WordPress
  footnotes - the references, the footnote list and the `footnotes`
  meta; a post opened from the blog gets its footnotes back as `[^1]`.
  Blocks that hold other blocks are **fenced containers** (Pandoc/MyST
  style), their content ordinary Markdown, nestable:
  `::: group {bg=base-2 style=lui-card layout=grid columns=3}`,
  `:::: columns` with `::: column {width=25%}`, `:::: accordion` with
  `::: item "Frage" {open}`, `:::: tabs` with `::: tab "Reiter 1"`,
  `::: cover {image=titel.png overlay=contrast dim=60 height=420px}`,
  `::: media-text {image=bild.png position=right valign=center fill}`,
  `::: details "Zusammenfassung" {open}`. A cover's or media-text's local
  image is uploaded with the other images. A line of colons closes the
  innermost container. The older ` ```columns `/` ```details ` fences are
  still read.
- **Lossless import** — opening a post from the blog turns a block into
  Markdown only if that Markdown renders back to the same block structure
  (attributes, classes, styles, captions, table footers); anything
  Markdown can't carry (a border on one side only, a link with
  attributes beyond its address, dynamic blocks) stays as its original block markup and
  goes back unchanged. Inline markup without Markdown syntax (`<mark>`,
  `<sub>`, a link with `target`) is kept as inline HTML.
- **Block inspector** — the "Beitrag" view has a "Block" section for the
  block the cursor is in: text color and background (colors and
  gradients as swatches from the blog theme's palette), font size,
  alignment and the block styles the theme registers - only what the
  block supports. Every change rewrites the attribute line (or a
  heading's braces, or a container's opening line) as one undoable edit.
- **Markdown closeness check** — opening a post from the blog rates how
  much of it is beyond plain Markdown; a heavily designed post suggests
  editing it in wp-admin instead, a moderately designed one gets a banner
  with details (`docs/markdown-naehe.md`).
- **Theme presets in the preview** — the active blog's color palette,
  gradients, font sizes and block styles are fetched over the REST API
  (`src/themestyle.rs`, cached per blog) and turned into the same preset
  classes WordPress generates, so attribute lines, striped tables,
  accordions and tabs look in the preview as they will on the blog.
- **Document model** — per-article frontmatter (title, slug, status -
  Entwurf/Ausstehend/Veröffentlicht/Geplant/Privat, matching every native
  WordPress post status -, scheduled publish date/time, categories, tags,
  excerpt/meta description, RankMath SEO title/description/focus keyword,
  featured image and its own alt text, WordPress post id) stored in the
  `.md` file itself,
  editable in the right-hand pane's "Beitrag" view with autocomplete for
  every existing WordPress
  category/tag (backed by an on-disk cache, `src/termcache.rs`, fully
  paginated so it never silently caps out on a site with hundreds of
  tags, refreshed at startup and on demand) and a native file picker for
  the featured image, not just a path field - its alt text is sent as the
  resulting WordPress attachment's `alt_text` on upload, the same as any
  body image's. A "Slug aus Titel
  generieren" button fills the slug from the title using the same
  transliteration WordPress's own `sanitize_title()` uses, and a
  "URL-Länge (SEO)" row shows the full URL the post would actually
  publish at - domain, real category slug, and post slug together, not
  just the slug in isolation - with a green checkmark once it's within
  Google's search-result truncation length, or a warning past it. A
  "Kategorien & Tags verwalten" dialog next to the autocomplete's refresh
  button lists every existing category/tag and lets you rename or
  permanently delete one straight from the app, instead of only ever
  being able to read or auto-create a term.
- **New article dialog** (`Ctrl+N`) — title, slug and post/page in one
  dialog; the library folder is created right away and named after the
  slug. Start empty or from a Markdown/text file: its header (YAML front
  matter from Jekyll/Hugo/Obsidian, Hugo's TOML, MultiMarkdown, a Pandoc
  title block) goes into the frontmatter - title, slug, tags, categories,
  excerpt, featured image, a future date as scheduled - and the dialog
  shows which keys were taken over and which weren't. Local images the
  text points to are copied into the folder and the references rewritten;
  more images can be added by picker or drag and drop, a star picks the
  featured image. A Blocksatz `artikel.md` used as source loses its link
  to the blog post, so the copy never overwrites the original. Files
  dropped onto the library sidebar open the dialog pre-filled; "Aus
  Textdatei …" in the new-article menu asks for the file first.
  `Ctrl+N`, `Enter` without a title still starts an untitled article.
- **Library and continuous saving** — every article lives in its own
  folder under `~/Dokumente/Blocksatz/` (`<slug>/artikel.md` plus its
  images). An untitled article gets its folder as soon as something is typed
  (named by date and time, renamed after the slug or title once known);
  a post opened from WordPress gets one right away,
  and opening the same post again reopens that working copy instead of
  overwriting it. The open article is written to disk every two seconds
  and when the window closes, so there is no unsaved state to lose.
  Markdown files from elsewhere still open in place; they are only
  written once actually edited (or with Ctrl+S).
- **Editing existing posts** — the blog archive (Ctrl+Shift+O opens the
  drafts) opens any post or page as Markdown: `crates/gutenberg`'s reverse
  converter turns its Gutenberg block HTML back into Markdown,
  categories/tags are resolved from ids back to names, and the post's id
  carries over so uploading afterward updates that same post instead of
  creating a duplicate.
- **WordPress pages** — besides blog posts, Blocksatz edits static pages
  ("Impressum", "Über mich"): a "Typ" row in the "Beitrag" view (locked
  once the document is linked to WordPress), "Neue Seite" (Ctrl+Alt+N, in
  the menu of the sidebar's new-article button), and upload/preview/
  conflict checks all targeting `/wp/v2/pages`. The "Kategorien & Tags" group is
  hidden for pages, which have neither taxonomy.
- **WordPress-Mediathek** (Ctrl+Shift+L, primary menu) — browse and manage
  the whole media library without opening wp-admin: a thumbnail grid
  (WordPress's own generated thumbnails, type icons for documents/audio/
  video), a type filter (Alle Medien/Bilder/Dokumente/Audio/Video),
  server-side search, 48 items per page behind "Mehr laden", and a details
  pane with file name, MIME type, dimensions, size, upload date, alt text
  and URL - plus "URL kopieren", "Im Browser öffnen", "In Artikel
  einfügen" (images, videos and audio) and "Endgültig löschen". The
  insert menu's "Aus WordPress-Mediathek …" opens the same browser.
- **KI-Artikel schreiben** (Ctrl+Shift+G, menu of the sidebar's new-article button) — drafts a whole
  article from a topic/brief with the active KI-Chat provider, at a chosen
  length, optionally imitating your own writing style: your 1-5 most
  recently published posts are sent along as style samples (style only,
  not content). The result lands in an editable preview first and is only
  then used "Als neues Dokument" (title taken from its `#` heading) or
  inserted "An Cursor".
- **Medienverwaltung** (Ctrl+Shift+M, also reachable from the "Beitrag"
  view and as the "Bilder" page of the release check right before
  publishing, and per-image via "Alternativtext
  festlegen…" in the editor's right-click context menu - which, like its
  "KI-Alternativtext generieren…" neighbor, only appears when the click
  actually landed on a line with a media reference, rebuilt live from the
  cursor position on every right-click rather than always shown) — every
  unique image referenced in the article gets its own alt text, caption,
  and WordPress upload state, independent of the Markdown source
  (persisted alongside the rest of the document in the frontmatter) - the
  same image referenced more than once (a logo, a divider) shares that one
  entry across every occurrence rather than getting a separate one each
  time. Alt text is a three-state value rather
  than a plain on/off: not yet defined (flagged by a non-blocking "N von M
  Bildern haben noch keinen Alternativtext" hint), deliberately left empty
  for decorative images (not treated as an error), or defined text - an
  unusually long one (WCAG guidance: well under 150 characters, since a
  screen reader reads the whole thing aloud) gets its own non-blocking
  warning icon and tooltip, here and everywhere else alt text can be
  entered (the featured image's field, the quick-edit dialog); the
  caption is a separate field, never derived from the alt text, though it
  is seeded the first time an image is seen from the Markdown image's
  optional `"title"` (`![alt](src "title")`) if present, else from its
  bracket text (`![Bildunterschrift](src)`) - most images never get the
  quoted-title form, so the bracket text is usually the only description
  there is - and both alt text and caption actually reach the published
  post's HTML on export (a real `<figcaption>`, not just an invisible
  `<img title="">`), not only the WordPress media library's own
  attachment metadata. A "Zu WordPress hochladen" button
  per image uploads it via the real REST API and stores the resulting
  media id/URL so re-opening the article recognizes it as already
  uploaded rather than re-uploading it; the list's own "Aufmacherbild" row
  does the same for the featured image, showing whether one is set,
  pending upload, or already live. An "Alle hochladen" button above the
  list uploads every not-yet-uploaded image in one go - the featured image
  included, if one's pending - instead of one at a time, tracked with a
  progress bar and a final summary of how many succeeded, and a failure
  partway through doesn't stop the rest. A "Bild einfügen…" button in the
  editor toolbar opens a native image file picker and inserts a real
  Markdown image reference at the cursor (relative to the document's own
  folder when possible) - previously the only way to add an image
  reference was to type its filename by hand. A "Video/Audio einfügen…"
  button next to it does the same for local video/audio files - the same
  `![]()` reference, just picked from a video/audio file filter instead. A right-click on an image -
  its Markdown line in the editor, or the rendered image itself in the
  Vorschau pane - also offers "KI-Alternativtext generieren…": the active
  KI-Chat provider looks at the real image and proposes an accessible alt
  text at a choice of three detail levels (Standard/Ausführlich/Hohe
  Genauigkeit, remembered across uses), only starting once "Text
  generieren" is actually clicked, shown for review and correction before
  it's applied directly into the same alt-text field, ready for the next
  upload - applying it keeps the preview scrolled to wherever it already
  was rather than jumping back to the top. The rendered image in the
  Vorschau pane also offers a plain "Alternativtext bearbeiten…" (the same
  dialog as the editor's own line-based shortcut, without the AI step) and
  "Bild bearbeiten…": convert it to PNG/JPEG/WebP and/or resize it by
  width or height (with an optional "Seitenverhältnis beibehalten" toggle
  for a free, non-proportional resize) - writes a new sibling file and
  updates the article's own image reference to it, leaving the original
  untouched.
- **Einstellungen dialog** (Ctrl+,; its pages as vertical tabs in a
  sidebar, an `Adw.NavigationSplitView` that turns into two steps when
  narrow) — an
  "Erscheinungsbild" page adopted directly from GNOME Builder's own
  implementation (light/dark/follow-system cards using Builder's bundled
  preview illustrations, and an editor color-scheme grid using
  GtkSourceView's `StyleSchemePreview` widget filtered to schemes matching
  the current light/dark mode, the same widget and filtering Builder uses,
  plus the article preview's own typographic style picker). The grid
  shows GtkSourceView's own real, canonical bundled schemes (six per
  mode: `Adwaita`/`-dark`, `classic`/`-dark`, `cobalt`/`-light`,
  `kate`/`-dark`, `oblivion`, `solarized-light`/`-dark`, `tango`) rather
  than the app shipping its own copies - the same set GNOME Builder and
  GNOME Text Editor themselves offer. The picked scheme colors
  everything code-like: the editor, the Gutenberg code tab (HTML
  highlighted), the comparison dialog, the fold-out terminal (background,
  text, cursor and selection, with a GNOME ANSI palette tuned for light or
  dark backgrounds) and the preview's code blocks. Switching between light
  and dark swaps in the scheme's own counterpart (`cobalt-light` ↔
  `cobalt` ...), so a light pick never leaves a white editor in a dark
  window. Independent font pickers for the editor and the preview
  (family/size/weight/style,
  each with a live sample and a reset-to-default button), a WordPress-connection page (site
  URL/username in a small config file, the Application Password in the
  Secret Service via [`oo7`](https://crates.io/crates/oo7), never written
  to disk in plain text), a "KI-Chat" page (a provider picker for
  Gemini/ChatGPT/Claude/Groq/Ollama, each with its own API key - verified live
  against the provider's API as soon as it's entered, and saved
  automatically once that check succeeds, with no separate save button -
  and a model picker populated from that account's actual available
  models, Ollama additionally getting a configurable base URL; plus a
  fully editable, resettable system prompt shared across all providers),
  a "KI-Modelle" page (see "Per-task AI models" below), and a "KI-Prompts"
  page (the context menu's five built-in prompts and custom prompts - see
  above).
- **Per-task AI models with a capability check** — Einstellungen →
  "KI-Modelle" checks, per provider, which of the models the key *lists*
  it can actually *use*: each model gets a tiny dry-run request, and the
  outcome is classified (usable, quota temporarily exhausted, paid
  plan/credit required - including Gemini's free tier `limit: 0` models -,
  no access, blocked in the region, retired, invalid key) and cached per
  key fingerprint with a status-dependent lifetime (`src/modelcheck.rs`,
  the error matrix is `llm::classify`). Image descriptions (alt text and
  captions), editing (in-place rewrites, article evaluation, tag
  suggestions) and text generation (the AI article draft) can each get
  their own primary and fallback model; unassigned tasks keep following
  the KI-Chat model. A task skips a model known to be blocked, and falls
  back automatically when a real call fails for a model/account reason,
  with a toast saying so (`src/aitasks.rs`). The chat pane itself keeps
  using the KI-Chat model.
- **Publishing** — uploads create/update the WordPress post via its REST
  API on a background thread, always sending the status the chosen action
  stands for, and always updating the same tracked post. A successful
  upload writes the post id, each image's upload reference and the sync
  baseline back into the working copy right away. Scheduling needs a
  valid date; WordPress itself would otherwise publish immediately. Draft
  previews open in the app's web view, which needs a wp-admin login once:
  its cookies are kept on disk (`blocksatz/webkit/cookies.sqlite` under
  the user's data directory). Re-uploading a post first re-fetches its
  server content and compares it against the locally remembered baseline;
  if it changed on WordPress since, a confirmation asks before
  overwriting. Skipped (fails open) if the check itself can't complete, so
  a network hiccup never blocks publishing outright.
- **Broken-link checker** — the "Links" page of the release check scans
  the article for every unique `http(s)://` URL (Markdown link/image
  destinations, plus a bare URL alone on its own line that exports as a
  `wp:embed` block) and, on "Links prüfen", HEADs each one on a
  background thread (falling back to GET if a server rejects HEAD),
  flagging anything outside the 2xx/3xx range or timing out.
  Category/tag names are resolved to
  WordPress term ids (creating them if they don't exist yet). Locally-referenced images
  are uploaded to the media library "bei Bedarf" (as needed), sharing the
  same tracked media list Medienverwaltung uses: an image whose content
  hash still matches what's already on the server is reused rather than
  re-uploaded, and a changed one is uploaded as a new attachment with the
  superseded one cleaned up automatically, since WordPress can't replace
  an existing attachment's file in place. Every PNG/JPEG is uploaded as
  WebP (transparency kept), downscaled to at most 2000px on its longer
  edge; a 3.9 MB PNG screenshot ends up around 110 KB. Media uploads get
  a timeout that grows with the file size, so a slow uplink doesn't cut
  them off. Only what's *sent*
  is ever affected, never the local file. Posts are only ever moved to
  WordPress's (recoverable) trash, never deleted permanently.
- **Primary menu** (in the sidebar's header bar) — "WordPress-Mediathek",
  "Galerie einfügen…", "Einstellungen", "Tastenkürzel" (an
  `AdwShortcutsDialog`, also reachable via Ctrl+?), and "Über Blocksatz", the latter a native `Adw.AboutDialog`
  with the version (always in sync with `Cargo.toml`), GPL-3.0-or-later
  license text, issue tracker/repository links, and the full
  `CHANGELOG.md` history as its browsable "Neuigkeiten" release notes.
- **Internationalization** — translatable via GNU gettext (`gettext-rs`).
  Source strings are German (the app's original language); essentially the
  whole UI is wrapped for translation, and `po/en.po` is a complete,
  real English translation (~270 strings) proving the pipeline works end
  to end (`build.rs` compiles every `po/*.po` into a `.mo` catalog on
  every build, picked up automatically by a `cargo run` from this source
  tree). AI prompt content and proper nouns (WordPress, provider names)
  deliberately stay untranslated by design - see `po/README.md` for the
  full translator/contributor workflow.
- **Flatpak packaging** — manifest and build script under
  `build-aux/flatpak/`, desktop entry, AppStream metainfo and icons under
  `data/`.
- **GNOME desktop integration** — the `.desktop` file declares
  `MimeType=text/markdown;` and the app handles being launched with a file
  argument, so double-clicking a `.md` file (or "Open With" → Blocksatz)
  in GNOME Files opens it directly, loading into the already-running
  window rather than a second one if Blocksatz is already open. Opening
  or saving a file also registers it with `Gtk.RecentManager`, GNOME's
  shared recent-files list. Publishing, an image upload, or a link check finishing while
  the window isn't focused raises a desktop notification.

## Building & running

Requires a Rust toolchain (stable) and the GTK4/libadwaita/GtkSourceView5/
WebKitGTK 6.0/libspelling development packages (available on any recent
GNOME-based Linux distribution). Spell-checking needs at least one hunspell
dictionary installed for it to have anything to check against. GNU
gettext's `msgfmt` (for compiling `po/*.po` translations - see
`po/README.md`) is optional: `build.rs` only prints a build warning and
skips it if not found, and the app runs fine without it, just always
showing its original German source strings.

```sh
cargo build
cargo run
```

## Testing

```sh
cargo test --workspace
```

A few tests exercise real system services (e.g. the Secret Service via
`oo7`) rather than mocks, and are marked `#[ignore]` so a normal test run
doesn't depend on your desktop's state. Run those explicitly with:

```sh
cargo test --workspace -- --ignored
```

## Packaging (Flatpak)

Blocksatz is packaged and installed as a Flatpak. The manifest at
`build-aux/flatpak/de.linuxundich.Blocksatz.json` targets
`org.gnome.Platform` 51, which already bundles GTK4, libadwaita,
GtkSourceView5 and WebKitGTK 6.0 - only libspelling is built as an extra
module, plus the `org.freedesktop.Sdk.Extension.rust-stable` SDK extension
for the Rust toolchain itself.

One script builds the current checkout and installs it for the current
user (it also installs the runtime/SDK/extension if they're missing):

```sh
build-aux/flatpak/build.sh            # build + install
build-aux/flatpak/build.sh --run      # ... and launch it afterwards
build-aux/flatpak/build.sh --bundle   # ... and also write blocksatz.flatpak
```

The sandboxed build runs fully offline: the script first vendors every
crate from `Cargo.lock` into `build-aux/flatpak/.cache/vendor` with
`cargo vendor`, so a dependency change needs no separate manual step.
Compiled translations are installed to `/app/share/locale` (see
`po/README.md`). The app icon (`data/icons/hicolor/`) is generated by
`build-aux/icons/generate_icons.py`: a scalable SVG and a symbolic SVG, plus
PNGs at 48/64/128/256 px rendered from the SVG. The PNGs matter on systems
whose gdk-pixbuf has no SVG loader any more (librsvg 2.62 dropped it) -
GNOME Shell would otherwise show a blank tile. `build-aux/icons/make_preview.sh`
renders `docs/icon-preview.png`; `docs/icon.md` explains the design.

The sandbox sees the documents folder (`--filesystem=xdg-documents`,
where the library `~/Dokumente/Blocksatz` lives) and the templates folder
(`xdg-templates`: on the first launch Blocksatz puts a
"Blocksatz-Artikel.md" there, so Nautilus offers it under "Neues
Dokument") and nothing else of the home directory: other files arrive through the file chooser and drag and
drop portals, and an image or video picked from elsewhere is copied into
the article's folder, so an article stays self-contained.

`--talk-name=org.freedesktop.Flatpak` is for the fold-out terminal only:
it runs the user's shell on the host through `flatpak-spawn --host`, the
sandbox having no shell tools of its own. VTE isn't part of the GNOME
runtime, so the manifest builds it as a module.

For Flathub, `build-aux/flathub/prepare.sh <tag>` writes the submission
manifest (building from the Git tag) and `cargo-sources.json` (every
crate from `Cargo.lock`, generated by `build-aux/flathub/cargo-sources.py`)
into `build-aux/flathub/out/`; `--local` builds from this checkout's HEAD
for a test. `docs/flathub-verification.md` covers the submission and the
domain verification.

Secrets (WordPress application password, AI API keys) go through `oo7`,
which inside the sandbox uses the Secret portal's own per-app keyring
rather than the host's GNOME Keyring - so they have to be entered once
again after switching from a non-Flatpak build.

## Versioning

Blocksatz follows [Semantic Versioning](https://semver.org/). The version
in `Cargo.toml` is the source of truth; see [CHANGELOG.md](CHANGELOG.md) for
what changed in each release. Before `1.0.0`, minor version bumps (`0.x.0`)
may still change the on-disk frontmatter format or other user-facing
behavior — check the changelog when upgrading.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
