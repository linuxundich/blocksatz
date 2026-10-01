# Neuer Name für Blocksmith

Stand: 2026-10-01 · **Entscheidung: Blocksatz** (freigegeben am 2026-10-01)

Gesucht wurde ein neuer Name für den GNOME-Editor (GTK4/libadwaita), mit dem man Artikel in Markdown schreibt und als native Gutenberg-Blöcke in ein selbst gehostetes WordPress hochlädt. Als Ideenfelder waren vorgegeben: WordPress, Worte, Schmied, Linux, FOSS, GNOME, Stift, Buch. Der Name sollte kurz sein und zum GNOME-Stil passen (vgl. Apostrophe, Folio, Letterpress, Fragments, Errands).

## Methode

Geprüft wurde am 2026-10-01 wie bei Dandelion (`gnome-social-poster/docs/names.md`). Neu ist das WordPress-Verzeichnis, weil die App eng an WordPress hängt.

| Quelle | Wie geprüft |
|---|---|
| GitHub | `gh api search/repositories`, `"<name> in:name"` nach Sternen sortiert, zusätzlich mit `markdown` |
| GitLab.com | REST-API `/api/v4/projects?search=<name>` |
| GNOME GitLab | REST-API auf gitlab.gnome.org, mit Gegenprobe „fragments“ (liefert Treffer, die anonyme Suche funktioniert also) |
| Flathub | `POST https://flathub.org/api/v2/search`, gewertet nur Treffer mit dem Kandidaten im App-Namen |
| AUR / Arch | AUR-RPC v5 und `archlinux.org/packages/search/json` |
| WordPress.org | Plugin- und Theme-Verzeichnis über `api.wordpress.org/{plugins,themes}/info/1.2` |
| Domains | RDAP über rdap.org, ersatzweise DNS-over-HTTPS (NS-Abfrage). „frei?“ heißt nur wahrscheinlich frei. |
| Marken/Produkte | Websuche „<Name> app/software/writing“. **Keine** Recherche bei DPMA/EUIPO/USPTO. |

Zwei Felder fallen aus rechtlichen Gründen weg: Die [WordPress-Markenrichtlinie](https://wordpressfoundation.org/trademark-policy/) verbietet „WordPress“ im Produktnamen (erlaubt ist „WP“), und die GNOME Foundation erlaubt „GNOME“ in fremden App-Namen nur mit Zustimmung. Linux/FOSS ergaben nur sperrige Kombinationen („Tuxpress“, „Librepen“) und wurden nicht weiter verfolgt.

---

## Steckbriefe der engeren Wahl

### 1. Blocksatz

- **Bedeutung:** typografischer Fachbegriff für Text, bei dem alle Zeilen gleich lang sind. Darin stecken drei Dinge, die die App ausmachen: die **Blöcke** (Gutenberg), der **Satz** (Schriftsatz, also Handwerk wie beim Schmied, und zugleich der geschriebene Satz) und die Buchtradition. Außerdem führt der Name das „Block“ aus „Blocksmith“ weiter.
- **Aussprache:** für Deutschsprachige selbsterklärend. Englischsprachige sprechen es etwa „BLOCK-zats“, das ist machbar. GNOME hat einige Apps mit nicht-englischen Namen (Komikku, Kooha, Amberol). Die Oberfläche der App ist ohnehin deutsch (die Quell-Strings sind Deutsch).
- **GitHub:** genau 1 Repo (`ssims437/blocksatz`, 0 Sterne). **GitLab.com:** nichts. **GNOME GitLab:** nichts.
- **Flathub / AUR / Arch:** nichts. **WordPress.org:** kein Plugin, kein Theme.
- **Marken/Produkte:** keine App und keine Software dieses Namens gefunden. Die Websuche liefert nur Erklärungen des Begriffs.
- **Domains:** blocksatz.app und blocksatz.org sind **wahrscheinlich frei** (RDAP 404).
- **Schwächen:** Als Gattungsbegriff ist der Name als Marke schwer zu schützen. Das ist zugleich der Grund, warum ihn sonst niemand belegt. Wer auf Deutsch nach „Blocksatz“ sucht, findet zunächst Typografie-Ratgeber, daher sollte man in Texten „Blocksatz für WordPress“ oder „Blocksatz-Editor“ schreiben.
- **Fazit:** mit Abstand das sauberste Ergebnis, inhaltlich der treffendste Name. ★★★

### 2. Brayer

- **Bedeutung:** engl. „Farbwalze“, die Handwalze, mit der der Drucker Farbe auf die Druckform rollt. Sie passt zu Druck und Handwerk, ist aber kaum bekannt, auch nicht im Englischen.
- **GitHub:** 40 Repos, alle mit 0 Sternen. **GitLab, GNOME GitLab, Flathub, AUR, Arch, WordPress.org:** nichts.
- **Marken/Produkte:** „brayer software gmbh“ (Wien, nach dem Namen des Inhabers), sonst nur das Werkzeug. Kein Schreibprogramm.
- **Domains:** brayer.app ist vergeben (seit 2026-09-09), brayer.org **wahrscheinlich frei**.
- **Fazit:** frei, aber erklärungsbedürftig. ★★

### 3. Fleuron

- **Bedeutung:** typografisches Blattornament (❧), in EN und FR geläufig. Gibt ein hübsches Icon her.
- **GitHub:** 41 Repos, darunter `octalwise/fleuron`. **Flathub, AUR, Arch, WordPress.org, GNOME GitLab:** nichts.
- **Marken/Produkte:** „Fleuron“ ist ein Android-Client für den Feedreader Miniflux (Octalwise), außerdem eine Brüsseler Firma für KI-Dokumentenverarbeitung. Beides liegt im weiteren Inhalts-Umfeld.
- **Domains:** .app vergeben (2023), .org vergeben (2020).
- **Fazit:** schön, aber mit Kollision im Lese- und Feed-Bereich. ★½

### 4. Setzling

- **Bedeutung:** Wortspiel aus „Setzer“ (Schriftsetzer) und „Setzling“ (Jungpflanze). Funktioniert nur auf Deutsch.
- **GitHub:** 3 Repos ohne Bedeutung. **Flathub, AUR, Arch, WordPress.org, GNOME GitLab:** nichts. Keine Produkte.
- **Domains:** setzling.app wahrscheinlich frei, setzling.org vergeben (2017).
- **Fazit:** frei und charmant, aber der Bezug zu WordPress und zu Blöcken fehlt. ★½

### 5. Leadtype

- **Bedeutung:** engl. „Bleisatz, Bleilettern“. Für Deutsche liest es sich leicht als „Lead“ im Sinne von Vertrieb.
- **GitHub:** `inthhq/leadtype`, eine Doku-Pipeline für MDX (Markdown!), und `rowland/leadtype` (PDF in Go). Dazu „leadtype“ als Fachwort in Lead-Management-Software.
- **Domains:** leadtype.app wahrscheinlich frei, .org vergeben (2026-05).
- **Fazit:** Kollision mit einem Markdown-Werkzeug, außerdem mehrdeutig. ★

---

## Übersicht

| Name | Bedeutung | GitHub/GitLab | Flathub | AUR | WP.org | Marken | .app | .org | Bewertung |
|---|---|---|---|---|---|---|---|---|---|
| **Blocksatz** | Blocksatz (Typografie) | 1 Repo, 0 Sterne | frei | frei | frei | keine | frei? | frei? | ★★★ |
| **Brayer** | Farbwalze | nur Kleinstprojekte | frei | frei | frei | Firmenname in Wien | vergeben (2026) | frei? | ★★ |
| **Fleuron** | Blattornament | octalwise/fleuron | frei | frei | frei | Miniflux-Reader für Android | vergeben | vergeben | ★½ |
| **Setzling** | Setzer + Jungpflanze | nur Kleinstprojekte | frei | frei | frei | keine | frei? | vergeben | ★½ |
| **Leadtype** | Bleisatz | MDX-Doku-Pipeline | frei | frei | frei | Lead-Management | frei? | vergeben | ★ |

## Vorab aussortiert

Rund 50 weitere Namen liefen durch dieselben Abfragen. Sortiert nach Ideenfeld:

| Feld | Name | Grund |
|---|---|---|
| Schmied | Wordsmith, Smithy, Inksmith, Wordforge, Inkforge, Ingot, Anvil, Burin | Wordsmith: völlig verbraucht (Docker-Beispiel, KI-Firmen). Smithy: AWS-IDL (2,4k Sterne). Inksmith: kanadische EdTech-Firma, Spiel. Wordforge/Inkforge: viele kleine KI-Schreibtools, Domains 2026 frisch registriert. Burin: Coding-Agent auf GitHub. |
| Schmied/Block | Blockwright | **WordPress.org:** Plugins `blockwright-blocks`, `blockwright-workbench` und ein Theme `blockwright`, also eine direkte Kollision |
| Buch | Folio, Quire, Vellum, Octavo, Colophon, Incipit, Marginalia, Codex, Quarto | Folio: **Flathub-App** (Markdown-Notizen). Vellum: bekannte E-Book-Satz-App für macOS, AUR `vellum`. Colophon: Obsidian-Schreib-Plugin, Publishing-Plattform, SSG. Incipit: zwei Schreib-Apps (eine davon für Linux). Quarto: Markdown-Publishing von Posit. |
| Stift | Quill, Nib, Inkwell, Penwright, Penmark, Calamus, Stylo, Plume, Feder, Scribe, Scriptor | Quill: Quill.js (47k Sterne), Arch-Paket `quill`. Penwright: **Typst-Schreib-App für den Desktop**. Penmark: Markdown-Editor-Projekt. Stylo: akademischer Markdown-Editor (Huma-Num). Plume: föderierte Blog-Software. Scriptor: Flathub hat „Scriptorium“. |
| Worte / Satz | Pilcrow, Galley, Typeset, Typewright, Ligature, Byline, Wortwerk, Writ, Verba, Stanza, Lettern | Pilcrow: **mehrere Markdown-Editoren**, auch für Linux. Galley: mehrere Markdown-Tools, AUR `galley-pad-bin`. Typeset: Flathub „Typesetter“. Typewright: Typst-Editor für iOS. Byline: WP-Theme `byline`. Lettern: als Wort zu blass, sonst frei. |
| Druck | Platen, Woodblock, Hotmetal, Stempel, Quoin, Typecase, Tessera | Platen: **Flathub-App** `page.wisha.platen`. HoTMetaL war ein bekannter HTML-Editor der 90er. Stempel: AUR `stempel-bin`, Elasticsearch-Plugin. Tessera: geht in „Tesseract“ unter. |
| Sonstiges | Lectern, Tinker, Rubric, Tusche, Kolophon | Lectern: WP-Plugin und -Theme. Tinker: Laravel/Tencent. Rubric: Bewertungsraster. |

## Empfehlung und Entscheidung

**Blocksatz.** Der Name verbindet Gutenberg-Blöcke, Schriftsatz und Schreiben in einem bekannten deutschen Wort, und er ist überall frei, die Domains eingeschlossen. Bestätigt am 2026-10-01.

Offen bleibt:

1. Markenrecherche bei DPMA und EUIPO (Klasse 9, Software).
2. Domains sichern, falls gewünscht: blocksatz.app, blocksatz.org (laut RDAP frei).

## Quellen (Auswahl)

- Pilcrow-Editoren: https://pilcroweditor.com/ · https://github.com/pilcrowmd/pilcrow
- Galley-Markdown-Tools: https://github.com/InkyQuill/galley-pad
- Penwright: https://github.com/renejes/penwright
- Colophon: https://community.obsidian.md/plugins/colophon-writer
- Incipit: https://github.com/server9-dev/incipit
- Fleuron (Miniflux-Client): https://octalwise.com/fleuron
- Leadtype: https://github.com/inthhq/leadtype
- Typewright: https://typsteditor.app/
