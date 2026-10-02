# Markdown-Nähe: Warnung bei stark gestalteten Beiträgen

Stand: 2026-10-02 · Status: **Konzept, nicht umgesetzt**

## Ziel

Blocksatz ist dafür gedacht, in Markdown geschriebene Artikel ins Blog zu
bringen. Ein Beitrag, der viele Gutenberg-Funktionen nutzt, lässt sich zwar
verlustfrei öffnen (`docs/markdown-blocks.md`), ist aber mühsam zu
bearbeiten: Ein Teil des Textes besteht dann aus WordPress-Markup.

Beim Laden aus dem Blog schätzt Blocksatz deshalb ein, wie nah ein Beitrag
an normalem Markdown ist, und weist bei stark gestalteten Beiträgen darauf
hin. Blocksatz blockiert dabei nichts, es gibt nur einen Hinweis und einen
besseren Weg (wp-admin).

## 1. Messung

Eine reine Funktion in der Block-Engine, `gutenberg::assess(markdown)`,
ordnet jeden Block des umgewandelten Textes einer Stufe zu:

| Stufe | Was dazu zählt | Schreibgefühl |
|---|---|---|
| **Markdown** | Absatz, Überschrift, Liste, Zitat, Code, Bild, einfache Tabelle, Trenner, Einbettungs-URL | normales Schreiben |
| **Gestaltung** | Attributzeilen (`{bg=…}`), Anker, Bildbreite, Tabellen-Fußzeile, Inline-HTML wie `<mark>` | lesbar, aber Zusatzsyntax |
| **Struktur** | `:::`-Container, ```` ```columns ````/```` ```gallery ```` usw. | verschachtelt, umständlicher |
| **Fremdkörper** | unverändert übernommene `<!-- wp:… -->`-Blöcke, klassischer Inhalt (`wp:freeform`), Shortcodes | nur als HTML editierbar |

Ergebnis (`Assessment`):

- Anteile je Stufe nach Textmenge (Zeichen der Blockquelle), nicht nur nach
  Blockzahl,
- Anzahl der Fremdkörper und Gestaltungsstellen,
- die auffälligen Blocktypen mit Anzahl („5 × Gruppe, 2 × Medien & Text,
  1 × Shortcode“) für die Detailansicht,
- die daraus abgeleitete Einstufung (Abschnitt 2).

**Blog-Bausteine** zählen nicht als Fremdkörper: Blöcke, die zum normalen
Workflow gehören. Standard: `lui/toc`, `lui-ads/slot`, `more`, `nextpage`,
`footnotes`. Die Liste ist in den Einstellungen änderbar.

## 2. Einstufung

Grundlage sind die Messungen vom 2026-10-02:

| Beitrag | Fremdkörper | Einstufung |
|---|---|---|
| 12 neueste Beiträge auf linuxundich.de | 0 (nur `lui/toc`, ein Bild mit `<br>`) | unauffällig |
| Demo 45437 „Alle Gestaltungselemente im Test“ | 1 (Zitat mit Quelle), viele `{size=…}` | gestaltet |
| Demo 45662 „Sämtliche Gutenberg-Blöcke“ | 84 von ~235 Blöcken | stark gestaltet |

Vorgeschlagene Schwellen:

- **Unauffällig:** keine Fremdkörper, höchstens drei Gestaltungsstellen.
- **Gestaltet:** bis zu zwei Fremdkörper oder mehr Gestaltungs- und
  Strukturstellen.
- **Stark gestaltet:** drei oder mehr Fremdkörper, oder mehr als 10 % des
  Textes als Fremdkörper, oder klassischer Inhalt, oder überwiegend
  Container.

## 3. Oberfläche (GNOME HIG)

**Unauffällig:** Blocksatz zeigt nichts an.

**Stark gestaltet:** ein `AdwAlertDialog`, bevor die Arbeitskopie angelegt
wird:

> **Dieser Beitrag nutzt viele Gutenberg-Funktionen**
> 84 Blöcke lassen sich hier nur als WordPress-Markup bearbeiten, darunter
> Gruppen mit Rahmen, Medien & Text und Widgets. Blocksatz ist für Artikel
> gedacht, die in Markdown geschrieben sind.
>
> [Abbrechen] [In wp-admin bearbeiten] [Trotzdem öffnen]

- „In wp-admin bearbeiten“ ist die hervorgehobene Antwort
  (`ResponseAppearance::Suggested`). Sie öffnet den Gutenberg-Editor im
  Browser-Tab der App.
- „Trotzdem öffnen“ hat die normale Optik. Blocksatz kann den Beitrag ja
  korrekt öffnen, das ist also keine Gefahr.
- Kein Kästchen „Nicht mehr fragen“, das HIG rät davon ab. Abschalten geht
  über die Einstellungen.

**Gestaltet:** ein `AdwBanner` über dem Editor, dasselbe Muster wie die
Sync-Banner (`mainaction.rs`): „Teile dieses Beitrags sind WordPress-Markup
und nur als Text bearbeitbar.“ Der Knopf „Details“ öffnet die Aufstellung
nach Stufen und Blocktypen. Ein Sync-Banner hat Vorrang, weil er dringender
ist.

**Immer:** Die Statuskarte der Ansicht „Beitrag“ zeigt „Markdown-Nähe:
hoch / mittel / gering“ mit denselben Details, auch später noch.

## 4. Wann geprüft wird

- **Öffnen aus „Im Blog“ (Archiv), aus der Seitenleiste und über den
  Abgleich:** Alle diese Wege laufen über `importer` und
  `window::open_imported_post`. Dort setzt die Prüfung einmal an, bevor die
  Arbeitskopie entsteht.
- **„Blog-Fassung laden“ für einen bereits geöffneten Artikel**
  (`mainaction::apply_blog_version`): kein Dialog, denn die Entscheidung ist
  schon gefallen. Nur Banner und Statuszeile werden aktualisiert.
- **Selbst geschriebene Artikel:** keine Warnung, wer selbst Container
  einsetzt, will sie. Die Statuszeile zeigt die Einschätzung trotzdem.

## 5. Einstellungen

Unter Einstellungen → Editor:

- „Vor stark gestalteten Beiträgen warnen“ (an/aus, Standard an),
- „Blog-Bausteine“: Blöcke, die nicht als Fremdkörper zählen.

## 6. Bewusst nicht im ersten Schritt

- **Markierung schon in der Archivliste:** Dafür müsste Blocksatz den Inhalt
  jedes gelisteten Beitrags laden. Das sind viele Anfragen, und der Hoster
  sperrt bei zu vielen. Möglich wäre es nur für Beiträge, deren Inhalt schon
  im Cache liegt.
- **Zuklappen der Markup-Blöcke im Editor:** würde das Bearbeiten
  erleichtern, ist aber ein eigenes Thema.

## 7. Offene Entscheidungen

1. **Schwellen:** Passen „mindestens 3 Fremdkörper oder mehr als 10 % des
   Textes“ für die Rückfrage, oder lieber strenger (ab dem ersten
   Fremdkörper, der kein Blog-Baustein ist)?
2. **Hervorgehobene Antwort im Dialog:** „In wp-admin bearbeiten“ oder
   neutral ohne Empfehlung?
3. **Statuszeile in „Beitrag“:** immer anzeigen oder nur bei gestalteten
   Beiträgen?
