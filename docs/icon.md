# Icon für Blocksatz

Stand: 2026-10-01 · **Entwurf B freigegeben und umgesetzt** (v0.64.0)

Das endgültige Icon erzeugt `build-aux/icons/generate_icons.py` (SVG, symbolisches SVG und PNGs in 48/64/128/256 px), die Vorschau `build-aux/icons/make_preview.sh` → `docs/icon-preview.png`. Die beiden Entwürfe liegen zum Vergleich in `docs/icon-drafts/`.

## Warum ein neues Icon

Das bisherige Icon (Amboss und Feder auf einem blauen Squircle mit Verlauf) verstößt an mehreren Stellen gegen die [HIG für App-Icons](https://developer.gnome.org/hig/guidelines/app-icons.html):

- Es hat einen Hintergrund in Kachelform. GNOME-Icons sind dagegen freistehende Objekte mit Draufsicht und Front-Profil.
- Die Farben `#25A6DC` bzw. `#0A4D6E` stammen aus dem WordPress-Umfeld, nicht aus der GNOME-Palette, und ein Verlauf liegt auf einer flachen Fläche.
- Der Kreis spielt auf das WordPress-Logo an, und mit dem Namen Blocksmith geht auch die Schmied-Metapher.
- Ein symbolisches Icon fehlt.

## Regeln (HIG), nach denen die Entwürfe gebaut sind

- Leinwand 128 × 128, 2-px-Raster, gemeinsame Grundlinie, keine extremen Seitenverhältnisse.
- Draufsicht und Front-Profil, das Profil höchstens 4 px hoch.
- Farben aus der GNOME-Palette; flache Flächen ohne Verlauf; keine Schatten.
- Wenig Details, damit es auch bei 64 und 32 px funktioniert.
- Symbolisches Icon 16 × 16 in `#2e3436` mit derselben Metapher.
- Keine Logos, also auch kein WordPress-W.

## Entwurf B: Seite mit Blöcken (Empfehlung)

Ein Blatt mit Überschrift, einem Absatz im Blocksatz (alle Zeilen gleich lang, die Wortfugen sind sichtbar), einem Bild-Block und einem Zitat-Block mit orangem Akzent. Um den Absatz liegt der blaue Auswahlrahmen, den jeder aus dem Gutenberg-Editor kennt. Das Icon zeigt so zugleich den Namen (Blocksatz), die Blöcke (Gutenberg) und den Zweck (Artikel schreiben).

Es bleibt bis 32 px gut lesbar. Das symbolische Icon ist eine Seite mit Überschrift und gleich langen Zeilen.

## Entwurf A: Winkelhaken

Ein Winkelhaken (das Werkzeug des Handsetzers) mit drei Zeilen Bleilettern, jede Zeile exakt auf Breite gesetzt, also Blocksatz im wörtlichen Sinn. Das bringt das Handwerk des alten „Smith“ und den Bezug zu Gutenberg mit. Die Schwächen: Bei 32 px wirkt es wie eine Ziegelmauer, und ohne Vorwissen zum Bleisatz erschließt es sich kaum. Deshalb nur als Alternative.

## Umsetzung

- `data/icons/hicolor/scalable/apps/de.linuxundich.Blocksatz.svg` und `data/icons/hicolor/symbolic/apps/de.linuxundich.Blocksatz-symbolic.svg`.
- Zusätzlich PNGs in 48/64/128/256 px, aus dem SVG gerendert. Die HIG kommt mit dem SVG aus, aber auf Systemen ohne gdk-pixbuf-Loader für SVG (librsvg 2.62) zeigt GNOME Shell sonst eine leere Kachel (siehe CHANGELOG 0.54.1).
- Offen und optional: Feinschliff mit App Icon Preview im Vergleich zu anderen Icons, eine Nightly-Variante für Entwicklungsbuilds.
