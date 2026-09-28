#!/usr/bin/env bash
# Builds Blocksmith as a Flatpak from the current checkout and installs it
# for the current user (`--user`).
#
#   build-aux/flatpak/build.sh            # build + install
#   build-aux/flatpak/build.sh --run      # ... and launch it afterwards
#   build-aux/flatpak/build.sh --bundle   # ... and also write blocksmith.flatpak
#
# The sandboxed build runs offline, so every crate from `Cargo.lock` is
# first vendored into `.cache/vendor` with plain `cargo vendor` (the same
# crates.io download a normal `cargo build` does) - re-run on every build,
# which is a quick no-op when nothing changed.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
app_id="de.christophlangner.Blocksmith"
manifest="$here/$app_id.json"
cache="$here/.cache"

run=false
bundle=false
for arg in "$@"; do
  case "$arg" in
    --run) run=true ;;
    --bundle) bundle=true ;;
    *) echo "Unbekannte Option: $arg" >&2; exit 2 ;;
  esac
done

for tool in flatpak flatpak-builder cargo; do
  command -v "$tool" >/dev/null || { echo "$tool fehlt." >&2; exit 1; }
done

# Runtime/SDK/Rust extension - no-ops once installed. The rust-stable
# branch must match the freedesktop base of the GNOME runtime (GNOME 50 → 25.08).
flatpak install --user --noninteractive --or-update flathub \
  org.gnome.Platform//50 org.gnome.Sdk//50 org.freedesktop.Sdk.Extension.rust-stable//25.08

mkdir -p "$cache"
echo "Vendore Rust-Abhängigkeiten …"
cargo vendor --locked --quiet --manifest-path "$root/Cargo.toml" "$cache/vendor" >/dev/null
# Only crates.io sources in Cargo.lock (no git dependencies), so this fixed
# source replacement is all the offline build needs.
cat > "$cache/cargo-vendor-config.toml" <<'TOML'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
TOML

cd "$here"
flatpak-builder --user --install --force-clean --ccache \
  --state-dir="$here/.flatpak-builder" --repo="$here/repo" \
  "$here/build-dir" "$manifest"

if $bundle; then
  flatpak build-bundle "$here/repo" "$root/blocksmith.flatpak" "$app_id"
  echo "Bundle: $root/blocksmith.flatpak"
fi

if $run; then
  flatpak run "$app_id"
fi
