#!/usr/bin/env bash
# Writes the files a Flathub submission (or update) needs into
# build-aux/flathub/out/: the manifest building from the Git tag, and
# cargo-sources.json with every crate from Cargo.lock.
#
#   build-aux/flathub/prepare.sh v0.66.0        # from the pushed tag on GitHub
#   build-aux/flathub/prepare.sh --local        # from this checkout's HEAD, for a test build
#
# The manifest is derived from build-aux/flatpak/de.linuxundich.Blocksatz.json
# (sandbox, runtime, modules, install steps stay in one place); only the
# Blocksatz module's sources and its cargo setup differ.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
out="$here/out"
ref="${1:-}"
[ -n "$ref" ] || { echo "Aufruf: $0 <tag> | --local" >&2; exit 2; }

if [ "$ref" = "--local" ]; then
  url="file://$root"
  tag=""
  commit="$(git -C "$root" rev-parse HEAD)"
else
  url="https://github.com/linuxundich/blocksatz.git"
  tag="$ref"
  commit="$(git -C "$root" rev-parse "$tag^{commit}")"
fi

mkdir -p "$out"
python3 "$here/cargo-sources.py" "$root/Cargo.lock" > "$out/cargo-sources.json"
python3 - "$root/build-aux/flatpak/de.linuxundich.Blocksatz.json" "$out/de.linuxundich.Blocksatz.json" "$url" "$tag" "$commit" <<'PY'
import json, sys
src, dst, url, tag, commit = sys.argv[1:]
manifest = json.load(open(src))
manifest["build-options"]["env"]["CARGO_HOME"] = "/run/build/blocksatz/cargo"
module = next(m for m in manifest["modules"] if m["name"] == "blocksatz")
module["build-commands"] = [c for c in module["build-commands"] if "cargo-vendor-config" not in c]
git = {"type": "git", "url": url, "commit": commit}
if tag:
    git["tag"] = tag
module["sources"] = [git, "cargo-sources.json"]
json.dump(manifest, open(dst, "w"), indent=4, ensure_ascii=False)
open(dst, "a").write("\n")
PY
echo "Geschrieben: $out/de.linuxundich.Blocksatz.json, $out/cargo-sources.json (${tag:-HEAD} = $commit)"
