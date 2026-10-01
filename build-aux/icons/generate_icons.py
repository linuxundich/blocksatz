#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Erzeugt das App-Icon und das symbolische Icon von Blocksatz nach GNOME-HIG.

Eine Seite mit Gutenberg-Blöcken (Überschrift, Absatz, Bild, Zitat), alle
Zeilen im Blocksatz; um den Absatz der blaue Auswahlrahmen des Block-Editors.
Raster 2 px, Farben aus der GNOME-Palette, Profil vorn ≤ 4 px. Die Entwürfe,
aus denen dieses Icon gewählt wurde, stehen in docs/icon.md.

Zusätzlich rendert es PNGs in 48/64/128/256 px (per rsvg-convert): Manche
Systeme haben keinen gdk-pixbuf-Loader für SVG mehr, dann zeigt GNOME Shell
nur eine leere Kachel, und `flatpak build-export` verweigert den Export
(siehe CHANGELOG 0.54.1).

    python3 build-aux/icons/generate_icons.py
"""

import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
APP_ID = "de.linuxundich.Blocksatz"

# GNOME-Palette (https://developer.gnome.org/hig/reference/palette.html)
BLUE2, BLUE3 = "#62a0ea", "#3584e4"
GREEN4 = "#2ec27e"
YELLOW2 = "#f8e45c"
ORANGE3 = "#ff7800"
LIGHT1, LIGHT4 = "#ffffff", "#c0bfbc"
DARK1, DARK3 = "#77767b", "#3d3846"
SYM = "#2e3436"  # Standardfarbe für symbolische Icons


def svg(body: str, size: int = 128) -> str:
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" '
            f'viewBox="0 0 {size} {size}">\n{body}</svg>\n')


def rect(x, y, w, h, fill, rx=0):
    r = f' rx="{rx}"' if rx else ""
    return f'  <rect x="{x}" y="{y}" width="{w}" height="{h}"{r} fill="{fill}"/>\n'


# --------------------------------------------------------------------------
# Vollfarbiges Icon, 128 × 128: Seite mit Gutenberg-Blöcken im Blocksatz
# --------------------------------------------------------------------------

def app_icon() -> str:
    b = ""
    px, py, pw, ph = 22, 10, 84, 104
    b += rect(px, py + 4, pw, ph, LIGHT4, 6)               # Profil (Blattstapel)
    b += rect(px, py, pw, ph, LIGHT1, 6)                   # Seite
    lx, lw = px + 10, pw - 20                              # Satzspiegel 64 px
    # Überschrift-Block
    b += rect(lx, 22, 44, 8, DARK3, 2)
    # Absatz-Block: Zeilen alle gleich lang = Blocksatz, Wortfugen 2 px
    y = 38
    for words in ([14, 20, 12, 12], [22, 10, 16, 10], [10, 18, 14, 16]):
        assert sum(words) + 2 * (len(words) - 1) == lw
        x = lx
        for w in words:
            b += rect(x, y, w, 4, DARK1, 1)
            x += w + 2
        y += 8
    # Gutenberg-Auswahlrahmen um den Absatz-Block
    b += f'  <rect x="{lx-4}" y="34" width="{lw+8}" height="28" rx="3" fill="none" stroke="{BLUE3}" stroke-width="2"/>\n'
    # Bild-Block
    b += rect(lx, 66, lw, 22, BLUE2, 2)
    b += f'  <path d="M{lx} 88 L{lx+20} 74 L{lx+34} 82 L{lx+44} 76 L{lx+lw} 88 Z" fill="{GREEN4}"/>\n'
    b += f'  <circle cx="{lx+50}" cy="73" r="4" fill="{YELLOW2}"/>\n'
    # Zitat-Block mit Akzentbalken
    b += rect(lx, 94, 4, 12, ORANGE3, 1)
    for i, words in enumerate(([18, 14, 24], [12, 22, 22])):
        x = lx + 8
        for w in words:
            b += rect(x, 94 + i * 8, w, 4, DARK1, 1)
            x += w + 2
    return svg(b)


def symbolic_icon() -> str:
    b = ""
    b += (f'  <path d="M4 1h8a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V3a2 2 0 0 1 2-2z'
          f'M4 3v10h8V3z" fill="{SYM}" fill-rule="evenodd"/>\n')
    b += rect(5, 4, 4, 2, SYM)
    b += rect(5, 7, 6, 1, SYM)
    b += rect(5, 9, 6, 1, SYM)
    b += rect(5, 11, 6, 1, SYM)
    return svg(b, 16)


if __name__ == "__main__":
    icons = ROOT / "data" / "icons" / "hicolor"
    for path, data in {
        icons / "scalable" / "apps" / f"{APP_ID}.svg": app_icon(),
        icons / "symbolic" / "apps" / f"{APP_ID}-symbolic.svg": symbolic_icon(),
    }.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(data)
        print(path.relative_to(ROOT))
    svg_path = icons / "scalable" / "apps" / f"{APP_ID}.svg"
    for size in (48, 64, 128, 256):
        png = icons / f"{size}x{size}" / "apps" / f"{APP_ID}.png"
        png.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(size), str(svg_path), "-o", str(png)], check=True)
        print(png.relative_to(ROOT))
