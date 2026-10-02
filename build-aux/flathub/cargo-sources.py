#!/usr/bin/env python3
"""Writes the cargo-sources.json a Flathub build needs: every crates.io
crate from Cargo.lock as an archive source plus its checksum file, and the
cargo config that points cargo at them - the same output
flatpak-cargo-generator produces for a lock file with crates.io
dependencies only (Blocksatz has no git dependencies).

    build-aux/flathub/cargo-sources.py Cargo.lock > cargo-sources.json
"""
import json
import sys
import tomllib

CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
VENDOR = "cargo/vendor"


def main() -> int:
    with open(sys.argv[1] if len(sys.argv) > 1 else "Cargo.lock", "rb") as f:
        lock = tomllib.load(f)
    sources = []
    for package in lock["package"]:
        source = package.get("source")
        if source is None:
            continue  # the workspace's own crates
        if source != CRATES_IO:
            print(f"unsupported source for {package['name']}: {source}", file=sys.stderr)
            return 1
        name, version, checksum = package["name"], package["version"], package["checksum"]
        dest = f"{VENDOR}/{name}-{version}"
        sources.append({
            "type": "archive",
            "archive-type": "tar-gzip",
            "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
            "sha256": checksum,
            "dest": dest,
        })
        sources.append({
            "type": "inline",
            "contents": json.dumps({"package": checksum, "files": {}}),
            "dest": dest,
            "dest-filename": ".cargo-checksum.json",
        })
    sources.append({
        "type": "inline",
        "contents": f'[source.vendored-sources]\ndirectory = "{VENDOR}"\n\n[source.crates-io]\nreplace-with = "vendored-sources"\n',
        "dest": "cargo",
        "dest-filename": "config",
    })
    json.dump(sources, sys.stdout, indent=4)
    print()
    return 0


if __name__ == "__main__":
    sys.exit(main())
