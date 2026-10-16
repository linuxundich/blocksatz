# Flathub submission notes

Status (2026-10-16): **not submitted, on hold.** Flathub's
[requirements](https://docs.flathub.org/docs/for-app-authors/requirements)
rule out the way this project is built and the way a submission would be
prepared here:

- **Manifests must not contain AI-generated or AI-assisted content** - not
  even with a disclosure. The manifest under `build-aux/flatpak/` and the
  changes `prepare.sh` makes were written with AI assistance, so a
  submission would need a manifest written by hand.
- **AI tools or agents must not open or automate the submission pull
  request**, nor write its description, commit messages or review
  replies. The pull request has to be made and answered by a person.
- **AI-generated code has to be disclosed**, with the affected parts and
  roughly how much; reviewers may reject a submission because of its
  extent. Large parts of Blocksatz are AI-assisted.
- **A meaningful development history** is required; apps that have
  existed only a short time are generally not accepted (the repository
  starts in September 2026).

What exists and can be reused once a submission makes sense:

- Screenshots in the metainfo (`data/screenshots/`, raw GitHub URLs on
  `main`).
- `prepare.sh <tag>` writes the manifest building from a Git tag and
  `cargo-sources.json`; it sets `BLOCKSATZ_FLATHUB=1`, which leaves out the
  "new version available" hint (`src/updatecheck.rs`).
- `flatpak-builder-lint manifest` reports only
  `finish-args-flatpak-spawn-access`: the terminal panel (F12) runs the
  user's shell on the host through `flatpak-spawn --host`, as GNOME Builder
  does. That needs a linter exception, or a Flathub build without the
  terminal (hide the F12 action when `BLOCKSATZ_FLATHUB` is set and drop
  `--talk-name=org.freedesktop.Flatpak` from the generated manifest).
- The domain verification for linuxundich.de is prepared
  (`docs/flathub-verification.md`); the token only exists after the app is
  accepted.
