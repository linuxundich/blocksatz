# GUI-Redesign: Bibliothek, Beitrag, Freigabe

Stand: 2026-10-01 · Ziel: GNOME 50 (libadwaita 1.9, GTK 4.22) · Status: **Konzept, noch nicht umgesetzt**

> Hinweis: GNOME 51 (libadwaita 1.10) ist seit 2026-09-16 stabil. Das
> Konzept setzt nur 1.9 voraus (`AdwSidebarItem:suffix` reicht für die
> Statussymbole); `prefix` an Items und `suffix` an Abschnitten (1.10) wären
> nur Kür.

## 1. Ausgangslage

Blocksatz ist heute ein sehr guter *Editor*, aber noch kein *Werkzeug für den
Lebenszyklus eines Beitrags*. Die Bestandsaufnahme (v0.64.0) zeigt:

| Bereich | Heute | Problem |
|---|---|---|
| Linke Seitenleiste | `AdwOverlaySplitView` mit zwei Tabs „Dokument“ (Status-Combo + 6 Aktionsknöpfe) und „Durchsuchen“ (Lokal: 10 zuletzt geöffnete · WordPress: 50 neueste Beiträge) | Mischt Navigation und Werkzeug; die Liste der Beiträge ist zwei Klicks tief versteckt; keine Suche, keine Seitenweise Abfrage |
| Rechter Bereich | 6 Tabs: Vorschau, Gutenberg-Code, Statistik, Chat, Bewertung, Browser | Zu viele gleichrangige Tabs; die Blog-Vorschau eines Entwurfs ist nur über den Export-Assistenten oder den Browser-Tab erreichbar |
| Metadaten | Modaler Dialog „Eigenschaften“ (5 Tabs) | Modal → während des Schreibens nicht sichtbar; Status wird an drei Stellen gepflegt (Seitenleiste, Eigenschaften, Assistent) |
| Veröffentlichen | Kopfleisten-Knopf „Artikel exportieren“ → Assistent (Carousel) **und** Knöpfe in der Seitenleiste | Zwei Wege mit unterschiedlichem Verhalten. **Bug:** `docsidebar.rs:196` berechnet den Zielstatus nur beim Aufbau → „Aktualisieren“ in der Seitenleiste sendet immer `status=publish` und kann einen Entwurf veröffentlichen |
| Speicherort | Eine `.md`-Datei irgendwo auf der Platte; importierte Beiträge haben **keinen** Speicherort (`current_path = None`) | Ein aus WordPress geöffneter Beitrag geht beim Schließen verloren; keine Übersicht „woran arbeite ich gerade?“ |
| Sicherheitsnetz | Keine Nachfrage bei ungespeicherten Änderungen (Schließen, Neu) | Datenverlust möglich |
| Synchronstatus | `wp_content_hash` im Frontmatter, Konfliktprüfung erst beim Hochladen | Der Zustand („online geändert“, „lokal geändert“) ist nirgends sichtbar |

## 2. Wie andere es lösen

[V] = gegen Quelle geprüft, [K] = aus Kenntnis, nicht nachgeprüft.

| Werkzeug | Aufbau | Entwurf vs. veröffentlicht | Erneut senden | Konflikte |
|---|---|---|---|---|
| **MarsEdit** [V/K] | Drei Spalten: Blog-Quellen (Published Posts, Pages, Media, *Local Drafts*) · Beitragsliste · Editor in eigenem Fenster | Status-Popup im Optionsbereich, Standard „Draft“ einstellbar | „Send to Blog“ aktualisiert | kaum; überschreibt. Release-Notes 5.4.1/5.4.6: Änderungen als *neuer* Beitrag bzw. doppelt nach Netzabbruch |
| **Ulysses** [V] | Bibliothek · Blattliste · Editor; keine Liste der Blog-Beiträge | Publish-Dialog mit Status/Termin | Knopf wird zu „Update…“; **Papierflieger-Symbol** in der Liste, *gefüllt* bei nicht hochgeladenen Änderungen | keine |
| **iA Writer** [V] | „Publish → New Draft on…“ | immer Entwurf | **nie** – erzeugt jedes Mal einen neuen Entwurf (Negativbeispiel: keine Verknüpfung) | – |
| **Open Live Writer** [K] | Ribbon, Tabs Bearbeiten/Vorschau/Quelltext, Eigenschaften seitlich | getrennte Knöpfe „Entwurf senden“ / „Veröffentlichen“ | ja | keine |
| **Blogilo (KDE)** [K] | Werkzeugkasten mit getrennten Tabs „Einträge“ (Server) und „Lokale Einträge“ | Submit mit Wahl neu/ändern + Entwurf/veröffentlichen | ja | keine |
| **Ghost Admin** [K] | Beitragsliste mit Filter Alle/Entwürfe/Geplant/Veröffentlicht; Einstellungen in **rechter Seitenleiste** | mehrstufiger Publish-Ablauf | Knopf wird „Update“, „Unpublish“ nachrangig | Sperre |
| **Gutenberg** [V/K] | Einstellungs-Seitenleiste rechts, Status-Popover | „Entwurf speichern“ + **Pre-Publish-Prüfung** vor dem zweiten Klick | „Aktualisieren“ | Autosave-Hinweis, Post-Locking |
| **WordPress-App** [V/K] | Tabs Veröffentlicht/Entwürfe/Geplant/Papierkorb | gelbes Label **„Lokale Änderungen“** | ja | vergleicht Änderungsdatum; binär „meine/Web-Fassung“, „Verwerfen“ im Überlaufmenü versteckt (Nutzerkritik) |
| **Obsidian-WP-Plugin** [V] | Publish-Modal | Status wählbar | schreibt `postId` ins Frontmatter, Modal sagt „neu“ vs. „aktualisieren“ | keine |
| **Apostrophe** [K] | Einzeldokument, Vorschau-Modi, Fokusmodus | – | – | – |
| **Iotas** [V] | Kategorien-Seitenleiste · Notizliste nach Zeit gruppiert | – | Offline-Bearbeitung, Sync bei Verbindung | ETag-basiert; mehrere Releases gegen *falsche* Konflikte |

**Was Blocksatz daraus übernimmt**

1. **Feste Verknüpfung** lokal ↔ Blog (`wp_post_id` + `modified_gmt` +
   Hash), und **idempotente Uploads**: ein Wiederholen nach Netzabbruch darf
   keinen zweiten Beitrag erzeugen (MarsEdit-Fehler).
2. **Zwei getrennte Dimensionen anzeigen:** WordPress-Status (Nur lokal /
   Entwurf / Ausstehend / Geplant / Veröffentlicht / Privat) *und*
   Synchronstatus (aktuell / lokale Änderungen / im Blog geändert /
   Konflikt) – nach Vorbild Ulysses-Papierflieger und WP-App-Label.
3. **Die Hauptaktion heißt, was sie tut:** „Veröffentlichen“ nur für
   Entwürfe, sonst „Entwurf aktualisieren“ bzw. „Änderungen
   veröffentlichen“. Veröffentlichen mit Prüfschritt (Gutenberg, Ghost).
4. **Vor dem Überschreiben** Blog-Stand prüfen und die Wahl
   *meine / Blog-Fassung* anbieten – plus einen **Vergleich**, den keines der
   Werkzeuge bietet. „Verwerfen“ nicht verstecken.
5. **Statusfilter** Alle/Entwürfe/Geplant/Veröffentlicht wie überall;
   lokale Arbeitskopien als eigene Gruppe.
6. **Echte WordPress-Vorschau** statt nachgebautem Theme; schnelle lokale
   Vorschau bleibt.
7. **Fallen vermeiden:** stilles Überschreiben, versteckte Aktionen,
   falsche Konflikte durch Caches, nach dem Veröffentlichen liegen
   gebliebene „Entwürfe“, ein mehrdeutiger „Senden“-Knopf.

## 3. Leitidee

> **Jeder Beitrag hat genau eine lokale Arbeitskopie und höchstens ein
> Gegenstück im Blog. Blocksatz zeigt jederzeit, in welchem Verhältnis beide
> stehen, und bietet genau eine nächste Aktion an.**

Daraus folgen fünf Entscheidungen:

1. **Bibliothek statt Dateien.** Blocksatz verwaltet einen Bibliotheksordner
   (Standard `~/Dokumente/Blocksatz/`), ein Unterordner pro Beitrag
   (`<slug>/artikel.md` + Bilder). Externe `.md`-Dateien lassen sich weiter
   öffnen; sie erscheinen in der Bibliothek, bis man sie entfernt.
   Wer einen Blog-Beitrag öffnet, bekommt automatisch eine Arbeitskopie.
2. **Automatisch speichern.** Die Arbeitskopie wird laufend gesichert (wie
   GNOME Notizen/Iotas). Der Speichern-Knopf entfällt; Strg+S bleibt als
   Sofort-Sichern erhalten. Damit erübrigt sich die Nachfrage beim Schließen.
3. **Der erste Upload ist immer ein Entwurf.** Direkt veröffentlichen geht
   nur über die Freigabe-Prüfung (Abschnitt 6).
4. **Eine kontextabhängige Hauptaktion** in der Kopfleiste
   (`AdwSplitButton`, `suggested-action`), Alternativen im Menü des Knopfs.
   Die Aktionsknopf-Sammlung der heutigen Seitenleiste entfällt.
5. **Metadaten in einen Seitenbereich statt in einen Dialog.** Status,
   Kategorien, Beitragsbild, Auszug, SEO liegen in einem einklappbaren
   Bereich rechts („Beitrag“) und sind beim Schreiben sichtbar.

## 4. Zustandsmodell eines Beitrags

Der Zustand ergibt sich aus Frontmatter (`wp_post_id`, `wp_content_hash`,
neu: `wp_modified_gmt`, `wp_status`) und einem leichten Abgleich mit dem Blog.

| # | Zustand | Bedingung | Anzeige (Seitenleiste) | Hauptaktion |
|---|---|---|---|---|
| A | **Nur lokal** | keine `wp_post_id` | Symbol `computer-symbolic`, Untertitel „Nur auf diesem Rechner“ | **Als Entwurf hochladen** |
| B | **Entwurf, aktuell** | Remote `draft`/`pending`, Hash gleich, lokal unverändert | „Entwurf · online“ | **Veröffentlichen …** |
| C | **Entwurf, lokal geändert** | wie B, lokale Änderungen seit letztem Upload | „Entwurf · Änderungen nicht hochgeladen“ + Punkt | **Entwurf aktualisieren** |
| D | **Geplant** | Remote `future` | „Geplant · Fr., 3. Okt., 08:00“ | **Änderungen hochladen** / Termin im Menü |
| E | **Veröffentlicht, aktuell** | Remote `publish`, Hash gleich | „Veröffentlicht · 28. Sep.“ | (keine; Knopf „Im Blog ansehen“) |
| F | **Veröffentlicht, lokal geändert** | wie E, lokale Änderungen | „Veröffentlicht · Änderungen nicht online“ + Punkt | **Änderungen veröffentlichen …** |
| G | **Im Blog geändert** | Remote-`modified_gmt` neuer als gespeichert **und** Hash weicht ab | Warnsymbol | Banner: „Im Blog geändert – Übernehmen / Vergleichen / Meine Fassung behalten“ |
| H | **Im Blog gelöscht / Papierkorb** | 404 bzw. `trash` | durchgestrichen, „Im Papierkorb“ | Banner: „Als neuen Entwurf hochladen / Verknüpfung lösen“ |

**Darstellung in zwei Dimensionen:** Der *Untertitel* eines Eintrags nennt
den WordPress-Status („Entwurf“, „Geplant · Fr., 8:00“, „Veröffentlicht ·
28. Sep.“, „Nur lokal“), das *Suffix-Symbol* den Synchronstatus:

| Symbol | Bedeutung |
|---|---|
| *(keins)* | aktuell |
| `document-send-symbolic` *(Papierflieger, hervorgehoben)* | lokale Änderungen, noch nicht hochgeladen |
| `emblem-synchronizing-symbolic` | wird hochgeladen |
| `dialog-warning-symbolic` | im Blog geändert / Konflikt |
| `network-offline-symbolic` | Blog nicht erreichbar (nur in der Site-Zeile) |

„Lokal geändert“ heißt: SHA-256 des aktuell erzeugten Gutenberg-HTML ≠
`wp_content_hash` **oder** geänderte Metadaten (eigener Hash über die
gesendeten Felder). Das ist schon heute fast vorhanden.

### Ablauf (Hauptworkflow)

```
 Schreiben ──► [Als Entwurf hochladen] ──► Entwurf online (B)
                                              │
            ┌──── Korrigieren (C) ◄───────────┤  Blog-Vorschau prüfen
            │                                 │
            └──► [Entwurf aktualisieren] ─────┘
                                              │
                                   [Veröffentlichen …]
                                              │
                                     Freigabe-Prüfung
                                      │            │
                               [Jetzt veröffentlichen]  [Planen]
                                      │            │
                                      ▼            ▼
                              Veröffentlicht (E)  Geplant (D)
```

### Bestehende Beiträge bearbeiten

1. In der Seitenleiste „Im Blog“ wählen → Beitrag suchen/filtern.
2. Öffnen → Blocksatz legt die Arbeitskopie an (Bilder bleiben Remote-URLs,
   werden nicht heruntergeladen) und zeigt den Beitrag sofort im Editor.
3. **Entwürfe** verhalten sich danach wie neue Artikel (B/C).
4. **Veröffentlichte Beiträge** zeigen ein `AdwBanner`:
   „Veröffentlichter Beitrag – Änderungen gehen erst mit *Änderungen
   veröffentlichen* online.“ Hauptaktion F; im Menü des Knopfs:
   - **Vorschau im Blog** – lädt die Änderungen als WordPress-Autosave
     (`POST /wp/v2/posts/<id>/autosaves`) hoch und öffnet die Vorschau, ohne
     den Live-Beitrag anzufassen. So macht es auch Gutenberg selbst.
     *(Technisch zu verifizieren: Vorschau-Link mit `preview_id`/`preview_nonce`
     über die geteilte WebKit-Sitzung.)*
   - **Auf Entwurf zurücksetzen** (Beitrag offline nehmen, mit Rückfrage).
   - **Mit Blog-Fassung vergleichen**.
5. Ist man fertig, entfernt „Aus Bibliothek entfernen“ die Arbeitskopie
   (Blog bleibt unberührt). Veröffentlichte, unveränderte Beiträge räumt
   Blocksatz optional nach 30 Tagen selbst auf.

## 5. Fensteraufbau

### 5.1 Breit (≥ 1200 sp)

```
┌────────────────────┬──────────────────────────────────────────────────────────────┐
│ [+]  Blocksatz  [≡]│ [◧]        Raspberry Pi 5 als NAS        [⤢] [◨] [Entwurf aktualisieren|▾]│
│ [🔍 Suchen       ] │            Entwurf · nicht hochgeladen                        │
│                    ├────────────────────────────────────────┬─────────────────────┤
│ IN ARBEIT          │ B I S │ H ❝ <> │ • 1. │ 🔗 🖼 …          │ Vorschau·Beitrag·KI │
│ Raspberry Pi 5 … ✈ │                                        │ ┌─────────────────┐ │
│   Entwurf          │ # Raspberry Pi 5 als NAS               │ │ Entwurf online  │ │
│ Fedora 45 Beta     │                                        │ │ hochgeladen vor │ │
│   Nur lokal        │ Der neue Pi bringt endlich PCIe …      │ │ 12 Min. ✈       │ │
│ GNOME 50 im Test   │                                        │ │ [Blog-Vorschau] │ │
│   Geplant · Fr 8:00│                                        │ └─────────────────┘ │
│ IM BLOG            │                                        │ Veröffentlichung    │
│ Entwürfe         7 │                                        │ Kategorien & Tags   │
│ Geplant          2 │                                        │ Beitragsbild        │
│ Veröffentlicht     │                                        │ Auszug · SEO        │
│ Seiten             │                                        │ Medien (4)          │
│ Papierkorb         │                                        │ Statistik           │
├────────────────────┼────────────────────────────────────────┴─────────────────────┤
│ linuxundich.de  ✓  │ 1.240 Wörter · 6 Min.                                         │
└────────────────────┴──────────────────────────────────────────────────────────────┘
```

Die linke Seitenleiste reicht als Navigations-Seitenleiste bis in die
Kopfleiste (eigene Kopfleiste). Der rechte Seitenbereich ist ein
*Utility Pane* und liegt laut HIG **unter** der Kopfleiste des Inhalts; er
wird mit **F9** ein- und ausgeblendet.

**Widget-Baum**

```
AdwApplicationWindow
└ AdwToastOverlay
  └ AdwNavigationSplitView                                     ← „Bibliothek“
    ├ sidebar: AdwNavigationPage „Blocksatz“ → AdwToolbarView
    │   ├ top: AdwHeaderBar  [Neuer Artikel (+)] Titel [Hauptmenü]
    │   │      + GtkSearchBar (Strg+F bei Fokus in der Leiste, Tippen-zum-Suchen)
    │   ├ content: AdwSidebar   (libadwaita 1.9)
    │   │    ├ AdwSidebarSection „In Arbeit“   → bind_model(Bibliothek)
    │   │    └ AdwSidebarSection „Im Blog“     → feste Einträge mit Zählern
    │   └ bottom: Site-Zeile (Verbindungsstatus, später Site-Umschalter)
    └ content: AdwNavigationPage → AdwNavigationView
         ├ Seite „Editor“: AdwToolbarView
         │    ├ top: AdwHeaderBar (Hauptaktion etc.), AdwBanner
         │    ├ content: AdwOverlaySplitView (sidebar-position=end) ← Utility Pane
         │    │    ├ content: Formatleiste + Editor (+ Suchleiste, KI-Leiste)
         │    │    └ sidebar: AdwToggleGroup „Vorschau | Beitrag | Assistent“ + AdwViewStack
         │    └ bottom: Statusleiste
         └ Seite „Beiträge“ (Blog-Archiv, siehe 5.3)
```

Begründung: Die HIG unterscheidet *Navigations-Seitenleiste* (links,
wählt Inhalt, für viele/dynamische Orte mit häufigem Wechsel, sortiert
nach Nützlichkeit, oft „zuletzt geändert zuerst“) und *Utility Pane*
(Zusatzinformationen zum aktuellen Inhalt, rechts, wenn untergeordnet).
Genau diese Trennung fehlt heute: Die linke Leiste enthält Aktionsknöpfe,
der rechte Bereich Navigation (Browser).

`AdwSidebar` passt für die *Navigation* (überschaubare Zahl Einträge),
**nicht** für hunderte Beiträge (intern eine `GtkListBox` ohne
Zeilen-Recycling, feste Zeilenform). Deshalb lebt das Blog-Archiv in einer
`GtkListView` auf eigener Seite.

### 5.2 Linke Seitenleiste „Bibliothek“ (`AdwSidebar`)

- **In Arbeit** – alle Arbeitskopien, sortiert nach letzter Änderung.
  `AdwSidebarItem` mit Titel, Untertitel = Zustand (Tabelle oben), Präfix-Icon
  = Zustandssymbol, Suffix = Punkt bei „nicht hochgeladen“. Kontextmenü
  (`setup-menu`): Öffnen · Im Blog ansehen · Im Dateimanager zeigen ·
  Duplizieren · Aus Bibliothek entfernen · In den Papierkorb (Blog).
- **Im Blog** – Filter-Einträge mit Zähler als Suffix: Entwürfe ·
  Ausstehend (nur wenn > 0) · Geplant · Veröffentlicht · Seiten · Papierkorb.
  Ein Klick öffnet im Inhaltsbereich die Seite „Beiträge“ mit diesem Filter.
- **Suche** filtert „In Arbeit“ sofort lokal (`AdwSidebar:filter`); Enter
  springt in die Seite „Beiträge“ und sucht serverseitig (`search=`).
- **Drop-Ziel**: Eine `.md`-Datei oder ein Ordner auf die Leiste ziehen →
  in die Bibliothek aufnehmen (`AdwSidebar::drop`).
- Leere Bibliothek → `AdwStatusPage` „Noch keine Artikel“ mit Knöpfen
  „Neuer Artikel“ und „Beitrag aus dem Blog öffnen“.

### 5.3 Seite „Beiträge“ (Blog-Archiv)

Eine eigene Inhaltsseite statt eines Tabs in der Seitenleiste, weil das
Archiv hunderte Einträge hat und Platz braucht.

- Kopfleiste: Zurück zum Editor · Titel „Veröffentlicht“ (je nach Filter) ·
  `AdwToggleGroup` Beiträge/Seiten.
- Suchfeld (serverseitig, 300 ms entprellt), Kategorie-Filter als Dropdown.
- `GtkListView` mit Seitenweisem Nachladen (`per_page=50`, `page=n`,
  `X-WP-TotalPages`), Zeilen im Stil von `AdwActionRow`: Titel,
  Datum · Kategorie · Autor, Status-Pille, Suffix-Icon „bereits in Arbeit“.
- Aktivieren = Arbeitskopie anlegen und öffnen (oder vorhandene öffnen).
- Kontextmenü: Im Blog ansehen · In wp-admin öffnen · Link einfügen (ersetzt
  den heutigen Link-Picker-Dialog) · In den Papierkorb.

### 5.4 Kopfleiste des Editors

Links: Seitenleisten-Umschalter.
Mitte: `AdwWindowTitle` – Titel = Artikeltitel, Untertitel = Zustand in Worten.
Rechts (von außen nach innen):
1. **Hauptaktion** als `AdwSplitButton` mit `suggested-action` (die einzige
   hervorgehobene Schaltfläche der Ansicht, HIG „Buttons“)
2. Umschalter Seitenbereich (`sidebar-show-right-symbolic`, F9)
3. Fokusmodus

Das Hauptmenü (`open-menu-symbolic`) sitzt am Ende der Kopfleiste der
Seitenleiste und enthält nur Einstellungen, Tastenkürzel, Info (HIG
„Menus“). „Neue Seite“, „Galerie einfügen“, „KI-Artikel schreiben“ wandern
ins Menü des Plus-Knopfs bzw. in die Formatleiste.

| Zustand | Hauptknopf | Menü des Knopfs |
|---|---|---|
| A | Als Entwurf hochladen | Zur Prüfung einreichen (pending) · Veröffentlichen … |
| B | Veröffentlichen … | Blog-Vorschau öffnen · Planen … · Privat veröffentlichen |
| C | Entwurf aktualisieren | Aktualisieren und Vorschau öffnen · Veröffentlichen … |
| D | Änderungen hochladen | Termin ändern … · Jetzt veröffentlichen · Auf Entwurf zurücksetzen |
| E | *(flacher Knopf)* Im Blog ansehen | Auf Entwurf zurücksetzen |
| F | Änderungen veröffentlichen … | Vorschau im Blog (Autosave) · Mit Blog-Fassung vergleichen · Änderungen verwerfen |

„Neuer Artikel“, „Medien“, „Eigenschaften“ wandern aus der Editor-Kopfleiste:
Neu → Seitenleiste, Eigenschaften → Seitenbereich „Beitrag“, Medien →
Abschnitt im Seitenbereich „Beitrag“ (Medienverwaltung als Liste, Mediathek
weiter als Dialog).

### 5.5 Rechter Seitenbereich (Utility Pane)

Drei Ansichten über eine `AdwToggleGroup` statt sechs Tabs:

| Ansicht | Inhalt | Herkunft heute |
|---|---|---|
| **Vorschau** | Lokale Vorschau; Umschalter oben rechts: *Gerendert · Gutenberg-Code · Im Blog* (Blog = WebKit-Ansicht der Entwurfs-Vorschau bzw. des Live-Beitrags, gleiche Sitzung) | Vorschau, Gutenberg-Code, Browser |
| **Beitrag** | Statuskarte (Zustand, zuletzt hochgeladen, Links), dann `AdwPreferencesGroup`s: Veröffentlichung (Typ, Termin, Autor, Kommentare, Slug) · Kategorien & Tags · Beitragsbild · Auszug · SEO · Medien · Statistik (Wörter, Lesezeit, Lesbarkeit) | Eigenschaften-Dialog, Dokument-Tab, Medienverwaltung, Statistik |
| **Assistent** | Chat, darunter einklappbar „Bewertung“ | Chat, Bewertung |

Der allgemeine Browser-Tab entfällt als eigener Tab; Links aus der Vorschau
öffnen im Standardbrowser oder in *Vorschau → Im Blog*. (Falls der freie
Browser vermisst wird: als vierte Ansicht über die Einstellungen zuschaltbar.)

### 5.6 Freigabe-Prüfung (ersetzt den Export-Assistenten)

`AdwDialog` (Breite ~560 sp, auf schmalen Fenstern Bottom Sheet), geöffnet
von „Veröffentlichen …“ / „Änderungen veröffentlichen …“. Eine
`AdwPreferencesPage` mit Prüfpunkten, jeweils mit Status-Icon und
Sprung-Knopf („Beheben“) in den passenden Abschnitt des Seitenbereichs:

- Titel, Slug, Auszug vorhanden
- Kategorie gesetzt, Tags (Hinweis, kein Muss)
- Beitragsbild + Alt-Text
- Alle Bilder mit Alt-Text, alle hochgeladen
- Links geprüft (bestehende Link-Prüfung)
- SEO-Fokus-Keyword (Rank Math)
- Zeitpunkt: `AdwToggleGroup` *Sofort · Geplant* (+ Datum/Uhrzeit)

Unten: „Abbrechen“ und **„Jetzt veröffentlichen“** bzw. **„Planen“**
(`suggested-action`). Ist die Blog-Fassung zwischenzeitlich geändert worden
(Hash-Abgleich wie heute), erscheint statt der Knöpfe die Konfliktauswahl.
Nach Erfolg: Toast „Veröffentlicht“ mit Aktion „Ansehen“.

Der Carousel-Assistent entfällt; seine Schritte (Links, Medien,
Gutenberg-HTML) sind in Prüfpunkte bzw. die Vorschau aufgegangen.

### 5.7 Schmal (< 700 sp) und mittel (700–1200 sp)

- **< 1200 sp:** rechter Seitenbereich wird Overlay (`collapsed=true`),
  standardmäßig zu (F9).
- **< 860 sp:** `AdwNavigationSplitView` klappt zusammen: Seite 1 =
  Bibliothek (`AdwSidebar` mit `mode = PAGE`, also als Boxed List, gleicher
  Breakpoint), Seite 2 = Editor mit Zurück-Pfeil.
- **< 700 sp:** wie bisher Editor und Vorschau nicht mehr nebeneinander.
  Am unteren Rand des Editors `AdwViewSwitcherBar`: Editor · Vorschau ·
  Beitrag. Die Freigabe-Prüfung erscheint als `AdwBottomSheet`.

## 6. Abgleich mit dem Blog

- **Wann:** beim Start, beim Fokussieren des Fensters (max. alle 2 Min.),
  manuell über „Aktualisieren“ (F5) im Kopf der Seitenleiste.
- **Was:** eine schlanke Abfrage pro Statusgruppe
  (`_fields=id,status,modified_gmt,title,date`, `per_page=100`) + die
  `wp_post_id`s der Arbeitskopien (`include=`). Nur bei abweichendem
  `modified_gmt` wird der Inhalt geholt und gehasht.
- **Idempotenz:** Vor einem *Anlegen* schreibt Blocksatz eine Upload-Marke
  (`wp_pending_create: <uuid>`, als Meta-Feld bzw. im Slug-Abgleich) in die
  Arbeitskopie. Bricht die Verbindung ab, sucht der nächste Versuch zuerst
  nach einem Entwurf mit dieser Marke, statt einen zweiten anzulegen.
  `wp_post_id` wird sofort nach der Antwort gespeichert, nicht erst nach dem
  Medien-Upload.
- **Offline:** Zustände bleiben auf dem letzten bekannten Stand; die
  Site-Zeile unten links zeigt „Offline“, Upload-Aktionen sind gesperrt
  (mit Tooltip), Schreiben geht weiter.
- **Konflikt (G):** `AdwBanner` mit drei Wegen; „Vergleichen“ zeigt eine
  zweispaltige Diff-Ansicht (Markdown beider Fassungen).

## 7. Was entfällt / wandert

| Heute | Neu |
|---|---|
| Speichern-Knopf | Autosave in die Bibliothek |
| „Artikel exportieren“ + Carousel-Assistent | Kontextabhängige Hauptaktion + Freigabe-Prüfung |
| Seitenleisten-Tab „Dokument“ mit 6 Knöpfen | Hauptaktion + Statuskarte im Seitenbereich „Beitrag“ |
| Seitenleisten-Tab „Durchsuchen“ | `AdwSidebar` „In Arbeit“ / „Im Blog“ + Seite „Beiträge“ |
| Eigenschaften-Dialog | Seitenbereich „Beitrag“ |
| Medien-Knopf in der Kopfleiste | Abschnitt „Medien“ im Seitenbereich |
| 6 Tabs rechts | 3 Ansichten (Vorschau · Beitrag · Assistent) |
| Link-Picker-Dialog | Suche der Seite „Beiträge“ wiederverwenden |
| „Von WordPress löschen“ (endgültig) | nur noch „In den Papierkorb“ |
| autosave.md (ein Slot) | entfällt, Bibliothek ist der Speicher |

## 8. Umsetzung in Phasen (jeweils mit Freigabe)

0. **Sofort-Fixes** (unabhängig vom Redesign): Zielstatus in
   `docsidebar.rs:196` bei jedem `refresh()` neu berechnen; tote
   Tastenkombination Strg+Umschalt+O entfernen oder neu belegen.
1. **Fundament:** `adw` auf Feature `v1_9`, `gtk4` auf `v4_22`. Bibliotheksordner
   + Zustandsmodell (`library.rs`, `syncstate.rs`) mit Unit-Tests, Autosave in
   die Arbeitskopie, Import legt Arbeitskopie an.
2. **Linke Seitenleiste** mit `AdwSidebar` (In Arbeit / Im Blog), Seite
   „Beiträge“ mit Suche und Seitenweiser Abfrage (`wpclient::list_items` um
   `page`, `search`, `status`, `include` erweitern).
3. **Hauptaktion** (`AdwSplitButton`) + Zustandsanzeige im `AdwWindowTitle`,
   Abgleich mit dem Blog, Banner für G/H.
4. **Seitenbereich rechts** (Vorschau · Beitrag · Assistent); Eigenschaften-
   Dialog und Medien-Knopf auflösen.
5. **Freigabe-Prüfung** ersetzt den Export-Assistenten; Autosave-Vorschau
   für veröffentlichte Beiträge.
6. **Schmale Layouts**, Tastenkürzel-Fenster, README/CHANGELOG, Übersetzung.

## 9. Offene Fragen

1. Bibliotheksordner `~/Dokumente/Blocksatz/` – passt das, oder gibt es
   schon einen Ort für Artikel (z. B. `00_artikel-scratchpad`)?
2. Soll der freie Browser-Tab ganz entfallen oder zuschaltbar bleiben?
3. „Zur Prüfung einreichen“ (pending) – wird das auf linuxundich.de genutzt?
4. Veröffentlichte, unveränderte Arbeitskopien automatisch aufräumen – ja/nein?
5. Mehrere Sites: jetzt schon im Datenmodell vorsehen (Site-ID im
   Frontmatter), UI später?
