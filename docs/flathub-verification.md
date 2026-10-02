# Flathub-Verifizierung über linuxundich.de

Stand: 2026-10-01 · betrifft `de.linuxundich.Blocksatz` und `de.linuxundich.Dandelion`

Bei App-IDs mit eigener Domain prüft Flathub die Inhaberschaft über eine Textdatei:

```
https://linuxundich.de/.well-known/org.flathub.VerifiedApps.txt
```

In der Datei steht ein Token pro Zeile, für jede verifizierte App einer. Erst nach der Prüfung zeigt Flathub den Haken „linuxundich.de“ an der App.

## Was schon vorbereitet ist

- **Der Server liefert `.well-known` direkt aus.** Getestet am 2026-10-01: Der Ordner `/hosts/linuxundich.de/.well-known/` existiert und enthält bereits `atproto-did` (die Verifizierung des Bluesky-Handles). Die URL antwortet mit HTTP 200, ohne dass WordPress oder der Seiten-Cache dazwischenkommen. Eine `.txt`-Datei im Webroot (`llms.txt`) kommt als `text/plain` zurück.
- **Die Datei selbst gibt es noch nicht.** Die URL antwortet mit 404 (WordPress-Fehlerseite). Das ist richtig so, denn einen Token vergibt Flathub erst, wenn die App eingereicht ist.
- **Skript zum Eintragen:** `build-aux/flathub/add-verification-token.sh`. Es hängt den Token per SSH an die Datei an, ohne vorhandene Tokens zu überschreiben, und prüft danach mit **einem** Abruf, ob er ausgeliefert wird. Es crawlt nicht und wärmt keinen Cache vor.
- Nebenbei: Im selben Ordner liegt ein leerer Tippfehler-Ordner `.well-know/` vom April 2025. Er stört nicht und kann weg.

## Einreichung vorbereiten (Blocksatz)

- Sandbox: nur `--filesystem=xdg-documents`, Netzwerk, Wayland/X11, GPU. Kein `host`-Zugriff mehr.
- Runtime GNOME 51, Rust-Erweiterung 26.08, libspelling mit Tag und Commit.
- `build-aux/flathub/prepare.sh v<version>` schreibt nach dem Taggen und Pushen das Manifest (Git-Quelle mit Tag und Commit) und `cargo-sources.json` nach `build-aux/flathub/out/`. Diese beiden Dateien kommen in den Pull Request bei flathub/flathub.
- Offen: Screenshots. `flatpak-builder-lint repo` verlangt `<screenshots>` in der Metainfo (Bild-URLs, z. B. raw-GitHub-Dateien unter `data/screenshots/`). Die Meldung `appstream-screenshots-not-mirrored-in-ostree` tritt nur beim lokalen Bau auf.
- `flatpak run --command=flatpak-builder-lint org.flatpak.Builder manifest build-aux/flathub/out/de.linuxundich.Blocksatz.json` muss ohne Fehler durchlaufen. Mit `--local` meldet er erwartbar `source-git-url-not-http`.

## Ablauf, sobald eine App auf Flathub eingereicht ist

1. Die App wird über einen Pull Request bei [flathub/flathub](https://github.com/flathub/flathub) eingereicht (Branch `new-pr`) und nach dem Review aufgenommen.
2. Auf https://flathub.org mit dem GitHub-Account **linuxundich** anmelden, dann *Developer Portal* → App wählen → *Verification* → *Website* `linuxundich.de`. Flathub zeigt einen Token an.
3. Den Token eintragen:
   ```bash
   build-aux/flathub/add-verification-token.sh <TOKEN>
   ```
4. Im Portal auf **Verify** klicken.
5. Für die zweite App dasselbe noch einmal. Die Datei hat danach zwei Zeilen.

Die Datei muss dauerhaft liegen bleiben. Flathub prüft nicht ständig nach, bei einer erneuten Verifizierung braucht es sie aber wieder. Also nicht beim Aufräumen des Webroots löschen.

## Ohne Flathub

Für das eigene Flatpak-Bundle (`build-aux/flatpak/build.sh --bundle`) ist keine Verifizierung nötig. Die App-ID funktioniert sofort.
