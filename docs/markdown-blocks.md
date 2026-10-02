# Blockgestaltung in Markdown: Bestandsaufnahme und Optionen

Stand: 2026-10-02 · Status: **Schritte 1–7 umgesetzt** (siehe Abschnitt 5)

Anlass: Farben, Farbverläufe, Tabellenvarianten, Akkordeons und weitere
Elemente aus dem Testbeitrag „Lorem Ipsum: Sämtliche Gutenberg-Blöcke“
(Post 45662) fehlen im Editor. Leitfrage: Was davon lässt sich mit
Markdown sinnvoll ausdrücken, was nicht?

## 1. Bestandsaufnahme

### Was die Block-Engine kann (`crates/gutenberg`)

| Richtung | Unterstützt |
|---|---|
| Markdown → Blöcke | paragraph, heading, list, quote, code, image, video, audio, embed, table, separator, more, html, sowie per Code-Fence ```` ```columns ````, ```` ```buttons ````, ```` ```gallery ````, ```` ```details ````, ```` ```pullquote ```` |
| Blöcke → Markdown | dieselben; **jeder andere Block** bleibt als vollständiges `<!-- wp:… -->`-Markup (RawHtml) im Markdown stehen und geht unverändert zurück |

### Messung am Testbeitrag (Hin- und Rückweg)

| | Original | Nach Import + Export |
|---|---|---|
| Blöcke mit Attributen | 109 | 76 |
| Farbangaben (`textColor`, `backgroundColor`, `gradient`) | 28 | 13 |
| CSS-Klassen (`className`, z. B. Blockstile) | 9 | 1 |

**Die bekannten Blöcke verlieren ihre Attribute.** Betroffen u. a.:
Absatz (Farben, Verlauf, Schriftgröße, Innenabstand, Initiale, Rahmen),
Überschrift (Farben, Anker, Abstand), Bild (Ausrichtung, Breite, Rahmen,
Schatten, Medien-ID), Tabelle (`is-style-stripes`, `align: wide`),
Liste (`start`, `reversed`), Spalten (Breiten, Hintergrund), Knopf
(Farben, Verlauf, Stil „Outline“), Pullquote, Trenner (Stil „Punkt“).
Wer einen gestalteten Beitrag in Blocksatz öffnet und wieder hochlädt,
verliert diese Gestaltung **stillschweigend**.

Container (Gruppe, Cover, Akkordeon, Tabs, Media-Text) und dynamische
Blöcke bleiben dagegen erhalten – aber als roher HTML-Block im Markdown,
den man kaum sinnvoll bearbeiten kann.

### Was das Theme vorgibt (`lui-theme/theme.json`)

- Palette (eigene Farben, Standardpalette und freie Farben **aus**):
  `accent` #ff6600, `accent-dark` #cc5200, `contrast` #1a1a1a,
  `contrast-2` #2b2a2a, `base` #ffffff, `base-2` #f5f4f2, `line` #e3e1dd,
  `muted` #63625f, `info` #2563eb, `warning` #dc2626, `tip` #16a34a
- Verläufe (freie Verläufe aus): `accent-fade`, `hero-overlay`
- Eigene Blockstile: Gruppe „Karte“ (`lui-card`), Knopf „Outline“
  (`lui-outline`), Trenner „Punkt“ (`lui-dot`)

Weil das Theme freie Farben abschaltet, reichen **Slugs** (`accent`)
statt Hex-Werten – das hält eine Markdown-Syntax kurz und robust.

### Was der Testbeitrag sonst enthält

Rund 80 Blocktypen. Grob in drei Gruppen:

1. **Text mit Gestaltung** (Absatz, Überschrift, Liste, Zitat, Tabelle,
   Bild, Knopf, Trenner) – Inhalt ist Markdown, nur Attribute fehlen.
2. **Container mit Markdown-Inhalt** (Gruppe, Cover, Spalten, Akkordeon,
   Tabs, Details, Media-Text, Pullquote) – Rahmen mit Einstellungen,
   innen wieder normale Blöcke.
3. **Dynamische Blöcke ohne Textinhalt** (Abfrage-Loop, Neueste Beiträge,
   Kalender, Archive, Suche, Social Links, Seitenliste, Inhaltsverzeichnis
   `lui/toc`, Werbeplatz `lui-ads/slot`, Fußnoten, Formel `math`,
   Playlist, Breadcrumbs …) – werden vom Server gerendert, in Markdown
   gibt es nichts zu schreiben.

## 2. Grenzen von Markdown

Standard-Markdown (CommonMark/GFM) kennt **keine Attribute**: keine
Farben, keine Klassen, keine Container, Tabellen nur mit Spaltenausrichtung
(kein Fuß, keine Beschriftung, keine Zellverbindung). Alles darüber hinaus
ist eine Erweiterung. Verbreitete Konventionen, an die man sich anlehnen
kann, statt etwas Eigenes zu erfinden:

| Konvention | Beispiel | Verbreitung |
|---|---|---|
| Attributliste (Pandoc, kramdown, markdown-it-attrs) | `# Titel {#anker .klasse}` · `{: .klasse}` unter einem Absatz | Pandoc, Jekyll, Hugo, VitePress |
| Fenced Divs / Container | `::: klasse` … `:::` (verschachtelbar mit `::::`) | Pandoc, MyST, Docusaurus, VitePress |
| Tabellenbeschriftung | `: Beschriftung` bzw. `Table: …` unter der Tabelle | Pandoc |
| GitHub-Hinweise | `> [!NOTE]` | GitHub, Obsidian |

Faustregel für die Abwägung: **Was man beim Schreiben tippt, gehört in
die Syntax; was man nur einstellt, gehört in eine Oberfläche**, die die
Syntax für einen schreibt.

## 3. Optionen

### A – Datenverlust stoppen (Pflicht, unabhängig von allem anderen)

Der Rückweg Blöcke → Markdown wandelt einen bekannten Block nur noch dann
in Markdown, wenn er **keine Attribute trägt, die Markdown nicht abbilden
kann**; sonst bleibt er wie unbekannte Blöcke als Original-Markup stehen.

- \+ kein stiller Verlust mehr, klein, sofort machbar
- − gestaltete Absätze erscheinen als HTML im Editor, bis B/C sie lesbar
  machen

### B – Attribute an Textblöcken (Attributliste)

Kurze, semantische Attribute in geschweiften Klammern, angelehnt an
Pandoc/kramdown, mit **Theme-Slugs** statt CSS:

```markdown
Ein Hinweis in Akzentfarbe.
{bg=accent color=base}

## Überschrift {#anker color=accent}

Ein Absatz mit Verlauf und großer Schrift.
{gradient=accent-fade size=large}

![Screenshot](bild.webp){align=wide width=600 style=rounded}

- erster Punkt
- zweiter Punkt
{start=5 reversed}
```

Abgebildet würden: Text-/Hintergrundfarbe, Verlauf, Schriftgröße
(Theme-Größen), Ausrichtung, Anker, Blockstil (`style=lui-outline`),
Initiale, Listenstart. Selten Genutztes (Innenabstand in Pixeln,
Buchstabenabstand) bleibt Original-Markup (A).

- \+ Text bleibt Markdown, gut lesbar, mit Pandoc verwandt
- \+ Vorschau kann es direkt darstellen (Palette → CSS)
- − eigener Dialekt; andere Markdown-Programme zeigen `{…}` als Text

### C – Container (`:::`)

Für Blöcke, die andere Blöcke enthalten. Der Inhalt bleibt normales,
bearbeitbares Markdown:

```markdown
::: group {bg=base-2 style=lui-card}
**Wichtig:** Dieser Kasten ist eine Gruppe mit Kartenstil.
:::

:::: accordion
::: item "Akkordeon-Eintrag 1" open
Text des ersten Eintrags, mit *Markdown*.
:::
::: item "Akkordeon-Eintrag 2"
Zweiter Eintrag.
:::
::::

:::: tabs
::: tab "Reiter 1"
Inhalt 1
:::
::: tab "Reiter 2"
Inhalt 2
:::
::::

::: cover {image=tfm.webp overlay=contrast dim=60 height=420 align=full}
## Text auf dem Titelbild
:::
```

Gleiche Form für Spalten, Details, Media-Text, Pullquote. Die heutigen
Code-Fences (```` ```columns ```` usw.) werden weiter gelesen, aber neu
als `:::` geschrieben – Code-Fences haben zwei Nachteile: Ihr Inhalt wird
im Editor nicht als Markdown hervorgehoben, und sie lassen sich nicht
verschachteln (Spalten in einer Gruppe, Akkordeon in Spalten).

- \+ verschachtelbar, Inhalt bleibt Markdown, Pandoc/MyST-üblich
- − Parser-Aufwand (eigene Block-Ebene vor pulldown-cmark)

### D – Tabellen

Markdown-Tabelle bleibt der Inhalt; Beschriftung, Fußzeile und Stil
kommen dazu:

```markdown
| Distribution | Paketformat | Veröffentlichung |
|---|---|--:|
| Arch Linux | `.pkg.tar.zst` | rollend |
| Debian | `.deb` | stabil |
| **Summe** | 3 Formate | – |
{style=stripes align=wide footer fixed}
: Tabelle mit Kopf-, Fuß- und Beschriftungszeile
```

`footer` macht die letzte Zeile zur Fußzeile, `: …` ist die
Beschriftung (Pandoc-Syntax). Verbundene Zellen gibt es in Markdown
nicht – solche Tabellen bleiben Original-Markup (A).

### E – Dynamische Blöcke

Kein Markdown möglich und nötig. Verbesserungen:

- **Kompakte Schreibweise**: einzeilig als selbstschließender Block
  (`<!-- wp:latest-posts {"postsToShow":5} /-->`) – ist heute schon so,
  sofern WordPress sie selbstschließend speichert.
- **Vorschau**: statt rohem HTML eine Platzhalterkarte („Neueste Beiträge
  – wird vom Blog erzeugt“), optional die echte Ausgabe über WordPress'
  Block-Renderer (`POST /wp/v2/block-renderer/<block>`).
- **Einfügen** über ein Menü „Block einfügen“ (statt Abtippen).

### F – Einstellungen per Oberfläche (Block-Inspektor)

Steht der Cursor in einem Absatz/Container, zeigt der Seitenbereich
„Beitrag“ einen Abschnitt **Block** wie Gutenbergs Seitenleiste:
Farbfelder aus der **Theme-Palette**, Verläufe, Schriftgröße, Stil,
Ausrichtung. Ein Klick schreibt/ändert die Attributzeile aus B bzw. den
Container-Kopf aus C. Die Palette holt Blocksatz pro Blog vom Server
(Global Styles des aktiven Themes) und cacht sie wie Kategorien.

- \+ niemand muss die Syntax auswendig kennen; Farben stimmen immer
  mit dem Theme überein
- − baut auf B/C auf

### G – Echter Block-Editor (verworfen)

Gutenberg selbst im WebView einbetten oder einen eigenen Block-Editor
bauen. Würde jede Gestaltung abdecken, gibt aber das Markdown-Konzept
auf – Blocksatz wäre dann ein schlechterer wp-admin. Nicht empfohlen.

## 4. Empfehlung

| Schritt | Inhalt | Aufwand | Nutzen |
|---|---|---|---|
| 1 | **A** – kein Attributverlust beim Import | klein | verhindert Schaden an bestehenden Beiträgen |
| 2 | Palette/Verläufe/Blockstile pro Blog holen; Vorschau-CSS dafür; Tabellen-Streifen, Gruppe, Akkordeon, Tabs, Cover in der Vorschau darstellen | mittel | Vorschau zeigt Gestaltung wie im Blog |
| 3 | **B** – Attributliste für Absatz, Überschrift, Liste, Bild, Knopf, Trenner | mittel | Farben/Verläufe in Markdown |
| 4 | **D** – Tabellen mit Stil, Fuß, Beschriftung | klein | Tabellenvarianten |
| 5 | **C** – `:::`-Container: Gruppe, Akkordeon, Tabs, Cover, Spalten, Details, Media-Text; alte Fences weiter lesen | groß | Container editierbar statt HTML |
| 6 | **F** – Block-Inspektor im Seitenbereich | mittel | Bedienung ohne Syntax |
| 7 | **E** – Platzhalter/Renderer für dynamische Blöcke, „Block einfügen“-Menü | mittel | Rest des Testbeitrags |

Schritt 1 sollte unabhängig vom Rest sofort kommen.

## 5. Umsetzung

- **Schritt 1 (A)**: `crates/gutenberg/src/fidelity.rs` vergleicht beim
  Import die Struktur des Originalblocks (Kommentar-JSON, blockweite
  Elemente mit Klassen/Styles, verlinkte Bilder) mit dem Markdown-Rundweg.
  Nur bei Gleichheit wird ein Block zu Markdown. Mit `GUTENBERG_DEBUG=1`
  meldet der Import, warum ein Block unverändert bleibt. Inline-HTML ohne
  Markdown-Entsprechung bleibt als HTML stehen. Unveränderte Blöcke mit
  Leerzeilen werden vor pulldown-cmark als Ganzes ausgeschnitten
  (`Segment::Raw`). Ergebnis an 12 echten Beiträgen: Struktur überall
  gleich, nur `lui/toc` und ein Bild mit `<br>` in der Unterschrift
  bleiben HTML.
- **Schritt 2**: `src/themestyle.rs` holt `themes?status=active`,
  `global-styles/themes/<stylesheet>` und `block-types?namespace=core`,
  cacht pro Blog und liefert Vorschau-CSS. Die Vorschau rendert Blöcke mit
  Attributen über die Block-Engine (echtes Gutenberg-Markup) und macht
  Akkordeons und Tabs klickbar.
- **Schritt 3 (B)**: `crates/gutenberg/src/attrs.rs`, Schlüssel `color`,
  `bg`, `gradient`, `size`, `align` (Text bei Absatz/Überschrift, sonst
  Block), `style`, `width`, `start`, `#anker`, `.klasse`, `dropcap`,
  `reversed`, `fixed`.
- **Schritt 4 (D)**: `footer`/`footer=N`, `caption="…"`; Tabellen ohne
  Kopfzeile als leere Kopfzeile; Spaltenausrichtung im aktuellen
  WordPress-Format (`has-text-align-*`, `data-align`).
- Nebenbei: Bildunterschrift/Alternativtext beim Import vertauscht (jetzt
  `![Bildunterschrift](url "Alternativtext")` wie in der App).
- **Schritt 5 (C)**: `crates/gutenberg/src/containers.rs` – `group`,
  `columns`/`column`, `accordion`/`item`, `tabs`/`tab`, `cover`,
  `details`. Kopfzeile `::: art "Titel" {einstellungen attribute}`, eine
  Zeile aus Doppelpunkten schließt den innersten Container, Code-Fences
  innen werden übersprungen. Beim Import entstehen Container nur, wo der
  Rundweg die Struktur erhält (Testbeitrag: 112 → 83 rohe Blöcke; offen
  sind u. a. Gruppen mit Rahmen/Innenabstand, Media-Text, Zitat mit
  Quelle, Bild-/Medien-Unterschriften bei Audio/Video/Embed).
  Einschränkung: Ein Cover-Bild muss eine URL sein, lokale Dateien lädt
  der Export dort (noch) nicht hoch.
- **Schritt 6 (F)**: `crates/gutenberg/src/editing.rs` (`block_at`,
  `attrs_edit`: Block am Cursor finden, Attribute als eine Ersetzung
  schreiben – Attributzeile, Klammern einer Überschrift oder Kopfzeile
  eines Containers) und `src/blockinspector.rs` (Abschnitt „Block“ in der
  Ansicht „Beitrag“: Farbfelder für Text und Hintergrund inkl. Verläufe,
  Schriftgröße, Ausrichtung, Stil; Zeilen je nach Block-Unterstützung).
- **Schritt 7 (E)**: Platzhalterkarten für dynamische Blöcke in der
  Vorschau (`dynamic_block_placeholder`), Menü „Einfügen“ in der
  Werkzeugleiste mit Containern und dynamischen Core-Blöcken. Die echte
  Ausgabe über `block-renderer` ist bewusst nicht eingebaut (eine Anfrage
  pro Tastendruck, Hoster-Sperren).
- **Schritt 8 (nach 0.65.0)**: Zitat mit Quelle (letzter Absatz mit
  Gedankenstrich, `> — Quelle`), `::: media-text` (`image`, `id`, `alt`,
  `position=right`, `valign`, `fill`, `width`, `nostack`, `size`,
  `type=video`), Box-Attribute `padding` (1–4 Werte, auch
  `var:preset|spacing|50`), `border` (Breite/Stil/Farbe), `radius`,
  `shadow`; Unterschriften bei Audio/Video (Klammertext), Embed und
  Galerie (`{caption="…"}`); verlinkte Bilder `[![…](bild)](ziel)`
  (`linkDestination` `media`/`custom`). Lokale Bilder in `cover` und
  `media-text` landen in der Medienliste und werden hochgeladen.
  Testbeitrag: 84 → 64 rohe Blöcke; erkannt, aber roh bleiben noch
  eigene Farbwerte, Typografie, `type=upper-roman`, Zellausrichtung,
  Seitenverhältnis von Bildern, Button-Breite/-Ausrichtung.
- **Fußnoten**: `crates/gutenberg/src/footnotes.rs` – `extract` nimmt
  `[^label]: …`-Definitionen heraus (Leerzeilen bleiben, damit die
  Vorschau-Zeilen stimmen) und ersetzt Verweise durch WordPress'
  `<sup data-fn>`; IDs aus dem Label (stabil bei jedem Upload), Meta
  `footnotes` als JSON. `to_markdown` macht beim Import daraus wieder
  `[^n]` plus Definitionen – nur wenn jede Fußnote einen Verweis hat,
  sonst bleibt `wp_footnotes` wie bisher.
- **Schritt 9**: Restliche Attribute – eigene Farbwerte in `color`/`bg`
  (`style.color`), Typografie `line-height`, `letter-spacing`, `weight`,
  `font-style`, `transform`, `decoration` (`style.typography`),
  `link-color` (`style.elements.link`), `marker` (Liste `type`), `aspect`/
  `scale` (Bild), bei Buttons `justify` (Attributzeile unter dem Fence) und
  pro Button `{style bg color gradient radius width newtab}` hinter dem
  Link. Da der Strukturvergleich Inline-Tags überspringt, prüft der Import
  Link-Attribute von Buttons und Bildern selbst. Testbeitrag: 64 → 52 rohe
  Blöcke; roh bleibt eine Tabelle, deren Kopfzeile anders ausgerichtet ist
  als die Zellen, und eine Bildunterschrift mit Link.
- **Schritt 10**: ` ```preformatted ` / ` ```verse ` (`Block::Pre`, Zeilen
  per `<br>`, Einrückung bleibt, Inline-Markdown je Zeile) und
  Bildunterschriften als Inline-Markdown in der Klammer (`Image.title` ist
  in der Engine jetzt Inline-HTML; die Medienverwaltung hält nur den Text
  und ersetzt die Unterschrift nur, wenn der Text abweicht). Testbeitrag:
  52 → 49 rohe Blöcke. Bewusst roh: die Tabelle mit abweichend
  ausgerichteter Kopfzeile (GFM richtet nur ganze Spalten aus; der
  Block-Editor selbst richtet Spalten ebenfalls komplett aus), Formel,
  Datei, Abstandhalter, Symbol und alle dynamischen Blöcke.
