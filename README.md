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

- **Split-pane editor** — the window remembers its size (and whether it was
  maximized) across restarts, always opening with the editor/preview split
  exactly 50/50 regardless of that size; a header-bar toggle button
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
  leaving is a smooth slide rather than an abrupt layout jump. A "Zuletzt
  geöffnet" button next to "Öffnen" lists the most
  recently opened/saved articles (most-recent-first) for one-click
  reopening, and a plain launch with no file argument reopens the most
  recent one automatically instead of always starting at a blank
  "Unbenannt" document - `Ctrl+N` still gets to a blank one in one step.
  Markdown editing pane (GtkSourceView, syntax
  highlighting, spell-checking via [`libspelling`](https://gitlab.gnome.org/GNOME/libspelling))
  with a grouped formatting toolbar (cut/copy/paste; bold/italic/
  strikethrough with Ctrl+B/I; heading/quote/code/code block; lists; table;
  link with Ctrl+K; "Bild einfügen" opening a native image file picker
  instead of typing a filename by hand; "Bestehenden Artikel verlinken"
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
  current selection, whenever one is active. The right pane's tabs (an
  `Adw.InlineViewSwitcher`, rendering all tabs as one seamless linked
  pill, in a toolbar row matching the editor's) are "Vorschau" (follows the app's light/dark
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
  image), "Gutenberg-Code" (the exact block HTML that would be published),
  "Statistik" (word/character/paragraph counts, estimated reading time, and
  a German-adapted Flesch reading-ease score with a qualitative label -
  expandable into the formula itself, the article's actual average
  words-per-sentence and syllables-per-word, and concrete tips for
  improving the score, derived from whichever of those two numbers is
  actually holding it down),
  and "Chat" - a writing assistant with message bubbles (replies rendered
  as Markdown), backed by Gemini, ChatGPT, Claude, Groq, or Ollama (self-hosted,
  no API key), with a provider/model picker both in the tab itself and in
  Einstellungen. A message typed here gets the editor's current selection -
  or, if nothing's selected, the whole article - appended before it's sent,
  the same rule the context menu's AI actions below already follow, so the
  model always has the article as context without pasting it in by hand.
  A "Browser" tab next to it is a plain `WebKit` view with an address bar
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
  re-parsed as ordinary Markdown, `wp:details` in modern WordPress) - all
  with full round-trip support back to the same Markdown when re-opening
  an existing post.
- **Document model** — per-article frontmatter (title, slug, status -
  Entwurf/Ausstehend/Veröffentlicht/Geplant/Privat, matching every native
  WordPress post status -, scheduled publish date/time, categories, tags,
  excerpt/meta description, RankMath SEO title/description/focus keyword,
  featured image and its own alt text, WordPress post id) stored in the
  `.md` file itself,
  editable via an "Artikel-Eigenschaften" dialog with autocomplete for
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
- **Library and continuous saving** — every article lives in its own
  folder under `~/Dokumente/Blocksatz/` (`<slug>/artikel.md` plus its
  images). A new article gets its folder as soon as something is typed
  (named by date and time, renamed after the title once the first `# `
  heading is finished); a post opened from WordPress gets one right away,
  and opening the same post again reopens that working copy instead of
  overwriting it. The open article is written to disk every two seconds
  and when the window closes, so there is no unsaved state to lose.
  Markdown files from elsewhere still open in place; they are only
  written once actually edited (or with Ctrl+S).
- **Von WordPress öffnen** (Ctrl+Shift+O) — pick an existing post - or,
  via the "Artikel"/"Seiten" toggle in its header, a static page - from the
  configured site, grouped into "Entwürfe" and "Veröffentlicht" (drafts
  first), and edit it as Markdown: `crates/gutenberg`'s reverse converter
  turns its Gutenberg block HTML back into Markdown, categories/tags are
  resolved from ids back to names, and the post's id carries over so
  exporting afterward updates that same post instead of creating a
  duplicate. Every row also has an "In den Papierkorb" button that moves
  the post/page to WordPress's own (recoverable) trash after a
  confirmation.
- **WordPress pages** — besides blog posts, Blocksatz edits static pages
  ("Impressum", "Über mich"): a "Typ" row in Artikel-Eigenschaften
  (locked once the document is linked to WordPress), "Neue Seite"
  (Ctrl+Alt+N) in the primary menu, and export/preview/delete/conflict
  checks all targeting `/wp/v2/pages`. The "Kategorien & Tags" tab is
  hidden for pages, which have neither taxonomy.
- **WordPress-Mediathek** (Ctrl+Shift+L, primary menu) — browse and manage
  the whole media library without opening wp-admin: a thumbnail grid
  (WordPress's own generated thumbnails, type icons for documents/audio/
  video), a type filter (Alle Medien/Bilder/Dokumente/Audio/Video),
  server-side search, 48 items per page behind "Mehr laden", and a details
  pane with file name, MIME type, dimensions, size, upload date, alt text
  and URL - plus "URL kopieren", "Im Browser öffnen", "In Artikel
  einfügen" (images) and "Endgültig löschen".
- **KI-Artikel schreiben** (Ctrl+Shift+G, primary menu) — drafts a whole
  article from a topic/brief with the active KI-Chat provider, at a chosen
  length, optionally imitating your own writing style: your 1-5 most
  recently published posts are sent along as style samples (style only,
  not content). The result lands in an editable preview first and is only
  then used "Als neues Dokument" (title taken from its `#` heading) or
  inserted "An Cursor".
- **Medienverwaltung** (Ctrl+Shift+M, also embedded as a "Medien" tab in
  the "Artikel exportieren" dialog next to "Vorschau" so it can be checked
  right before publishing, and reachable per-image via "Alternativtext
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
- **Einstellungen dialog** (`Adw.PreferencesDialog`, Ctrl+,) — an
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
  GNOME Text Editor themselves offer. Whichever scheme is picked also
  colors the live Markdown preview's code blocks to match, not just the
  editor. Independent font pickers for the editor and the preview
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
- **Publishing** — an "Artikel exportieren" dialog shows the generated
  Gutenberg HTML, then creates/updates the WordPress post via its REST API
  on a background thread. "Veröffentlichen" and "Als Entwurf hochladen"
  each send their own status explicitly, independent of whatever the
  "Artikel-Eigenschaften" dialog's status field currently holds - so
  publishing directly vs. uploading a draft first is an unambiguous choice
  made right in this dialog, and either one updates the same tracked post
  rather than creating a new one. A successful send writes the article back
  to disk right away: the post id and each image's upload reference live in
  the frontmatter, not in the editor buffer, so nothing else would ever
  mark the document as changed - closing the app would silently drop the
  link to the existing post and make every image look local again. A third "Terminieren" button appears once
  "Geplant" is picked in Artikel-Eigenschaften with a valid publish
  date/time set there - exporting without one is refused with a clear
  error rather than silently publishing immediately, which is what
  WordPress itself does with a scheduled status and no real future date.
  Likewise, a "Privat veröffentlichen" button appears once "Privat" is
  picked instead. On success, the post's real permalink appears as a
  clickable link right in the dialog. Once an article already exists on
  WordPress but isn't published yet, a "Vorschau öffnen" button opens
  WordPress's own unpublished-post preview link in the app's Browser tab
  (switching to it automatically). WordPress shows a draft only to a
  session allowed to edit it, so this needs a wp-admin login - without
  one, the site answers with a login page or its 404 page, which the
  button's tooltip notes up front. That login is only needed once: the
  Browser tab and the dialog's own Live-Vorschau share a single network
  session whose cookies are kept on disk (`blocksatz/webkit/cookies.sqlite`
  under the user's data directory), so signing in there once covers both
  views and outlives restarts of the app. Re-exporting an already-published post first
  re-fetches its current server content and compares it against a
  locally-remembered baseline (set on import and after every successful
  publish/update) - if the post changed on WordPress since (edited
  directly in wp-admin, most likely), a confirmation dialog asks whether
  to overwrite that change before sending anything, instead of silently
  clobbering it. Skipped for a new post or an article never yet synced
  with a server copy, since there's nothing to compare against, and
  skipped (fails open) if the check itself can't complete, so a network
  hiccup never blocks publishing outright.
- **Broken-link checker** — a "Links" tab in the same "Artikel
  exportieren" dialog, next to "Vorschau" and "Medien". It scans the
  article for every unique `http(s)://` URL (Markdown link/image
  destinations, plus a bare URL alone on its own line that exports as a
  `wp:embed` block) and, on "Links prüfen", HEADs each one on a
  background thread (falling back to GET if a server rejects HEAD),
  flagging anything outside the 2xx/3xx range or timing out - catching a
  typo'd URL or a since-deleted page before it ships as part of the
  published post.
  Category/tag names are resolved to
  WordPress term ids (creating them if they don't exist yet). Locally-referenced images
  are uploaded to the media library "bei Bedarf" (as needed), sharing the
  same tracked media list Medienverwaltung uses: an image whose content
  hash still matches what's already on the server is reused rather than
  re-uploaded, and a changed one is uploaded as a new attachment with the
  superseded one cleaned up automatically, since WordPress can't replace
  an existing attachment's file in place. An oversized PNG/JPEG (above
  ~300 KB) is downscaled to at most 2000px on its longer edge and
  re-encoded before upload - an opaque PNG (no transparency) converts to
  JPEG, since that's usually far smaller for a photo/screenshot saved as
  PNG - via `gdk-pixbuf`, already part of the GTK stack this app links
  against. An already-small image, or a result that somehow isn't smaller
  than the original, is uploaded completely unchanged; only what's *sent*
  is ever affected, never the local file. Once published, the same dialog
  offers a confirmed "Von WordPress löschen" to remove the post again.
- **Primary menu** (the header bar's hamburger button) — "Neue Seite",
  "WordPress-Mediathek", "KI-Artikel schreiben…", "Einstellungen",
  "Tastenkürzel" (a native `Gtk.ShortcutsWindow`, also reachable via
  Ctrl+?), and "Über Blocksatz", the latter a native `Adw.AboutDialog`
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
  shared recent-files list, alongside Blocksatz's own "Zuletzt geöffnet"
  popover. Publishing, an image upload, or a link check finishing while
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
`org.gnome.Platform` 50, which already bundles GTK4, libadwaita,
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
