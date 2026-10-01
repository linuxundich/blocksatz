# Umbenennung Blocksmith → Blocksatz

Stand: 2026-10-01 · **im Code umgesetzt (v0.64.0)**, offen: GitHub-Repo, Projektordner, altes Flatpak

## App-ID

**Entscheidung (2026-10-01): `de.linuxundich.Blocksatz`.**

Die bisherige ID `de.christophlangner.Blocksmith` beruht auf der Domain christophlangner.de, und die existiert nicht (NXDOMAIN). Die neue ID hängt an der Blog-Domain linuxundich.de. Flathub prüft sie über eine Datei unter `/.well-known/` (siehe `docs/flathub-verification.md`). Dandelion wurde gleich mit umgestellt (`de.linuxundich.Dandelion`), damit beide Apps demselben Schema folgen.

Ableitungen:

| Zweck | Wert |
|---|---|
| App-ID, Desktop-Datei, Icon-Name | `de.linuxundich.Blocksatz` |
| Developer-ID in der Metainfo | `<developer id="de.linuxundich"><name>Christoph Langner</name></developer>` |
| Flatpak-Datenordner | `~/.var/app/de.linuxundich.Blocksatz/` |

## Wo der Name vorkommt

Gezählt mit `grep -ri blocksmith` (ohne `target/` und `.git/`): 103-mal „Blocksmith“, 27-mal „blocksmith“, 7-mal die App-ID, verteilt auf 48 Dateien. Nach Art der Änderung:

### 1. App-ID (Dateinamen und Inhalte)

| Datei | Was |
|---|---|
| `src/main.rs:71` | `const APP_ID` |
| `src/about.rs:18`, `src/firstrun.rs:76` | `application_icon` / `icon_name` |
| `data/de.christophlangner.Blocksmith.desktop` | **Datei umbenennen**, `Icon=` |
| `data/de.christophlangner.Blocksmith.metainfo.xml` | **Datei umbenennen**, `<id>`, `<launchable>` |
| `data/icons/de.christophlangner.Blocksmith.svg` | ersetzen durch `data/icons/hicolor/scalable/apps/de.linuxundich.Blocksatz.svg` + `symbolic/apps/de.linuxundich.Blocksatz-symbolic.svg` |
| `data/icons/hicolor/{48,64,128,256}/apps/*.png` | neu aus dem SVG gerendert (nötig ohne SVG-Loader für gdk-pixbuf, siehe `docs/icon.md`) |
| `build-aux/flatpak/de.christophlangner.Blocksmith.json` | **Datei umbenennen**, `id`, Install-Zeilen für Desktop, Metainfo und Icons |
| `build-aux/flatpak/build.sh:17` | `app_id=` |

### 2. Anzeigename „Blocksmith“ (sichtbar für Nutzer)

| Datei | Was |
|---|---|
| `src/window.rs:235, 286, 340, 1432` | Fenstertitel, Menüpunkt „Über Blocksmith“, Wiederherstellungs-Dialog |
| `src/about.rs:17` | `application_name` |
| `src/firstrun.rs:77` | „Willkommen bei Blocksmith“ |
| `src/secrets.rs:21, 52` | Beschriftung der Keyring-Einträge (in Seahorse sichtbar) |
| `data/*.desktop`, `data/*.metainfo.xml` | `Name=`, `<name>`, Beschreibungstext |
| `po/blocksmith.pot`, `po/en.po` | Msgids mit „Blocksmith“ ändern sich mit dem Quelltext, dann `xgettext` neu laufen lassen und `en.po` nachziehen |

### 3. Technischer Kurzname „blocksmith“

| Datei | Was |
|---|---|
| `Cargo.toml` | `[package] name`, `[[bin]] name` → `blocksatz`; `Cargo.lock` wird neu erzeugt |
| `src/i18n.rs:39, 66`, `build.rs:38` | gettext-Domain `blocksmith`, Umgebungsvariable `BLOCKSMITH_LOCALEDIR` |
| `po/blocksmith.pot` | umbenennen in `po/blocksatz.pot`; `po/README.md` (11 Stellen) |
| Flatpak-Manifest | `command`, Modulname, `CARGO_HOME`-Pfad, `.mo`-Dateiname, `BLOCKSMITH_LOCALEDIR` |
| `build-aux/flatpak/build.sh`, `.gitignore` | `blocksmith.flatpak` |
| `src/adblock.rs:32`, `src/appearance.rs:142` | interne Bezeichner `blocksmith-basic-adblock`, CSS-Klasse `blocksmith-editor-font` (unkritisch, nur der Einheitlichkeit halber) |
| Konfig-Unterordner `dir.push("blocksmith")` | in 15 Modulen: `recentfiles`, `wpsite`, `adblock` (2×), `chatconfig`, `windowstate`, `autosave` (2×), `browser`, `appearance`, `aiprompts`, `firstrun`, `termcache`, `websession` (2×), `aialt`, `aitasks`, `modelcheck`, `preview`. **Besser einmal zentral** als `const APP_DIR` in einem Modul und überall verwenden. |
| `src/secrets.rs:10` | Keyring-Attribut `service=blocksmith` → `blocksatz` (alte Einträge werden nicht übernommen, siehe Nutzerdaten) |
| Tests | Titel und Dateinamen in `export.rs`, `wpclient.rs`, `importer.rs`, `document.rs`, `preview.rs`, `secrets.rs`, `i18n.rs`, `mdpango.rs` (kosmetisch). Achtung: Die Integrationstests legen im Test-Blog Kategorien wie „Blocksmith Test“ an. |

### 4. Doku und Außenauftritt

| Ort | Was |
|---|---|
| `README.md` (14), `ROADMAP.md` (10) | Name, CI-Badge-URL, Repo-Links |
| `CHANGELOG.md` | **Geschichte stehen lassen**, nur einen neuen Eintrag „Umbenannt in Blocksatz“ ergänzen. Vergleichslinks auf das Repo funktionieren nach der Umbenennung weiter, weil GitHub umleitet. |
| `src/about.rs:22–23`, Metainfo-`<url>` | `https://github.com/linuxundich/blocksatz` |
| GitHub-Repo | in den Settings umbenennen; GitHub leitet alte URLs und `git remote` weiter. Danach `git remote set-url origin git@github.com:linuxundich/blocksatz.git` |
| Projektordner | `05_Projekte/blocksmith` → `05_Projekte/blocksatz`, dazu `todo-blocksmith.md` und `blocksmith-quill-features.mbox` (letztere steht auch in den Ausschlüssen des Manifests) |
| Kommentare im Code | `adblock.rs`, `changelog.rs`, `linkcheck.rs`, `imageedit.rs`, `aiinplace.rs`, `gutenberg/src/reverse.rs` (rein kosmetisch) |

Nicht anfassen: `data/adblock/easylist-basic.txt` (ein Fremdtreffer in der Filterliste) und `data/icons/appearance-preview/ATTRIBUTION.md` (beschreibt die Herkunft).

## Nutzerdaten

Außer dem Entwickler hat die App keine Nutzer (Stand 2026-10-01). Deshalb gibt es **keinen Migrationscode**. Die alten Daten werden gelöscht, und Site, Passwort und KI-Keys gibt man einmal neu ein.

```
flatpak uninstall --delete-data de.christophlangner.Blocksmith
rm -rf ~/.config/blocksmith ~/.cache/blocksmith ~/.local/share/blocksmith   # Reste nativer Testläufe
```

Keyring-Einträge mit `service=blocksmith` aus nativen Läufen lassen sich in Seahorse entfernen („Blocksmith WordPress Application Password“, „Blocksmith … API Key“). Unter Flatpak liegen sie im Sandbox-Keyring und verschwinden mit `--delete-data`.

Vor dem Löschen lohnt ein Blick in `~/.var/app/de.christophlangner.Blocksmith/config/blocksmith/` auf eigene KI-Prompts (`ai_prompts.json`, `chat_system_prompt.txt`), falls man die behalten will.

## Reihenfolge der Umsetzung

1. Icon freigeben (siehe `docs/icon.md`). Die App-ID steht fest.
2. Code: zentrale Konstanten für App-ID, Anzeigenamen und Konfig-Ordner.
3. Dateien umbenennen (Desktop, Metainfo, Manifest, Icons, pot), Texte ersetzen, `xgettext` laufen lassen, `en.po` nachziehen.
4. `cargo test`, Flatpak bauen, altes Flatpak samt Daten entfernen, neu einrichten.
5. Version anheben (0.64.0), CHANGELOG und Metainfo-Release eintragen.
6. GitHub-Repo umbenennen, Remote anpassen, Projektordner umbenennen.
