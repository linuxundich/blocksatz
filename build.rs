//! Compiles every `po/<lang>.po` translation into the `.mo` catalog
//! `gettextrs` reads at runtime (see `src/i18n.rs`), so a plain `cargo run`
//! picks up a freshly edited translation without any separate manual step
//! or Meson/build-system machinery (this project deliberately has none -
//! see the Flatpak manifest for the equivalent step in an installed
//! build). Best-effort: if `msgfmt` (part of GNU gettext) isn't installed,
//! this only prints a build warning rather than failing the build - the
//! app still runs fine without compiled translations, just always showing
//! the original German source strings (see `i18n::tr`'s fallback).

use std::fs;
use std::path::Path;
use std::process::Command;

fn main() {
    let po_dir = Path::new("po");
    println!("cargo:rerun-if-changed={}", po_dir.display());

    let Ok(entries) = fs::read_dir(po_dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("po") {
            continue;
        }
        let Some(lang) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        println!("cargo:rerun-if-changed={}", path.display());

        let out_dir = po_dir.join("locale").join(lang).join("LC_MESSAGES");
        if let Err(err) = fs::create_dir_all(&out_dir) {
            println!("cargo:warning=Konnte {} nicht anlegen: {err}", out_dir.display());
            continue;
        }
        let mo_path = out_dir.join("blocksatz.mo");

        match Command::new("msgfmt").arg("-o").arg(&mo_path).arg(&path).status() {
            Ok(status) if status.success() => {}
            Ok(status) => println!("cargo:warning=msgfmt ist mit Status {status} fehlgeschlagen für {} - Übersetzung '{lang}' bleibt ungültig.", path.display()),
            Err(err) => {
                println!("cargo:warning=msgfmt nicht gefunden ({err}) - Übersetzungen werden nicht kompiliert, die App zeigt weiterhin die deutschen Originaltexte.");
                return;
            }
        }
    }
}
