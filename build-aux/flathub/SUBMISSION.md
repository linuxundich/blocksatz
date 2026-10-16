# Flathub submission notes

Text and checklist for the pull request at
[flathub/flathub](https://github.com/flathub/flathub) (new app,
`de.linuxundich.Blocksatz`). The manifest comes from `prepare.sh`; the
domain verification is described in `docs/flathub-verification.md`.

## Checklist

- [x] Manifest builds offline on GNOME 51 and passes
      `flatpak-builder-lint manifest`
- [x] Screenshots in the metainfo (`data/screenshots/`, raw GitHub URLs on
      `main`)
- [ ] Release tag to build from (`prepare.sh <tag>`)
- [ ] `flatpak-builder-lint repo` on the built repo
- [ ] Pull request opened
- [ ] Verification token on linuxundich.de
      (`add-verification-token.sh`)

## Permissions, for the pull request description

> **`--talk-name=org.freedesktop.Flatpak`** - Blocksatz has an optional
> terminal panel (F12) for the article's folder: running `git`, converting
> images, checking links with the user's own tools. Like GNOME Builder's
> terminal it spawns the user's login shell on the host via
> `flatpak-spawn --host`; a shell inside the sandbox would have none of
> those tools. Nothing else is started on the host, and the panel is only
> opened by the user.
>
> **`--filesystem=xdg-documents`** - the article library lives in
> `~/Documents/Blocksatz` (one folder per article, Markdown plus images).
>
> **`--filesystem=xdg-templates`** - installs a "New Blocksatz article"
> template for the file manager once.
>
> **`--share=network`** - talks to the user's WordPress sites (REST API)
> and, if configured, to an AI provider.

If the reviewers don't accept the host shell, the alternative is to build
the Flathub version without the terminal panel (hide the F12 action when
`BLOCKSATZ_FLATHUB` is set) and drop the permission from the generated
manifest.

## Not in the Flathub build

`prepare.sh` sets `BLOCKSATZ_FLATHUB=1`: the "new version available" hint
(`src/updatecheck.rs`) is left out there, since Flathub delivers updates.
