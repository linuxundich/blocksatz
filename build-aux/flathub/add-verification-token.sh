#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Trägt einen Flathub-Verifizierungstoken für eine de.linuxundich.*-App auf
# linuxundich.de ein und prüft danach, ob Flathub ihn abrufen kann.
#
#   build-aux/flathub/add-verification-token.sh <TOKEN>
#   build-aux/flathub/add-verification-token.sh --show
#
# Die Datei enthält einen Token pro Zeile, also einen pro App (Blocksatz,
# Dandelion, ...). Bereits vorhandene Tokens bleiben stehen, ein doppelter
# Token wird nicht ein zweites Mal eingetragen.
set -euo pipefail

HOST=christoph-langner@linuxundich.de
FILE=/hosts/linuxundich.de/.well-known/org.flathub.VerifiedApps.txt
URL=https://linuxundich.de/.well-known/org.flathub.VerifiedApps.txt

if [ "${1:-}" = "--show" ]; then
  curl -fsS "$URL" || echo "(noch keine Datei)"
  exit 0
fi

token=${1:?Aufruf: $0 <TOKEN>  (Token aus dem Flathub-Entwicklerportal)}
if ! [[ $token =~ ^[A-Za-z0-9_-]+$ ]]; then
  echo "Unerwartetes Token-Format: $token" >&2
  exit 1
fi

# printf statt echo, damit der Token nicht von der Remote-Shell ausgewertet wird
ssh "$HOST" "touch '$FILE' && grep -qxF '$token' '$FILE' || printf '%s\n' '$token' >> '$FILE'; chmod 644 '$FILE'"

# Ein einzelner Abruf zur Kontrolle (kein Crawlen, siehe Cache-Hinweise zum Blog)
body=$(curl -fsS "$URL")
if grep -qxF "$token" <<<"$body"; then
  echo "OK: Token ist unter $URL abrufbar."
  echo "Jetzt im Flathub-Portal auf „Verify“ klicken."
else
  echo "Token fehlt in der ausgelieferten Datei. Cache oder Weiterleitung prüfen:" >&2
  curl -sI "$URL" >&2
  exit 1
fi
