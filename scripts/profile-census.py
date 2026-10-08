#!/usr/bin/env python3
"""Turn a profile-census run into game profiles (ticket W25-02).

Reads the TSV that `crates/rf-harness/tests/profile_census.rs` writes and
the `slug<TAB>archive` list it was run with, and writes
`profiles/<console>/<slug>/profile.toml` for every game whose camera
evidence clears the bar below. A game that already has a profile is
reported, never overwritten. Prints a table of every game and why it was
or was not written, so the evidence is read by a person, not trusted.

    scripts/profile-census.py games.tsv census.tsv [--write]

Without --write it only reports. No ROM bytes and no paths are written:
titles come from the archive's file name, hashes from the census.
"""

import os
import re
import sys

# The bar a camera has to clear. A camera byte matches the frame's scroll
# on nearly every moving frame; 70% leaves room for status-bar splits and
# the frame a room transition writes something else.
MIN_MOVING = 150
MIN_RATIO = 0.70
MIN_DISTINCT = 64
MIN_MOVING_Y = 100
MIN_DISTINCT_Y = 32

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "profiles")


def parse_axis(cell):
    """`x=moving/ADDR:hits:distinct:wraps:carried,...` -> (moving, [cands])."""
    _, rest = cell.split("=", 1)
    moving, _, cands = rest.partition("/")
    out = []
    for c in filter(None, cands.split(",")):
        addr, hits, distinct, wraps, carried = c.split(":")
        out.append((int(addr, 16), int(hits), int(distinct), int(wraps), int(carried)))
    return int(moving), out


def pick(moving, cands, min_moving, min_distinct):
    if moving < min_moving:
        return None, f"only {moving} scrolling frames"
    if not cands:
        return None, f"no byte tracked the scroll over {moving} frames"
    addr, hits, distinct, wraps, carried = cands[0]
    ratio = hits / moving
    if ratio < MIN_RATIO:
        return None, f"best match {ratio:.0%} of {moving} frames"
    if distinct < min_distinct:
        return None, f"only {distinct} distinct values"
    # 16-bit when the next byte carried on every wrap seen; a camera whose
    # page lives elsewhere is read as its low byte, which is what the
    # scroll register holds anyway.
    wide = wraps > 0 and carried == wraps
    return (addr, "u16" if wide else "u8", ratio, moving, wraps), None


def title_of(archive):
    stem = os.path.splitext(os.path.basename(archive))[0]
    name = re.sub(r"\s*\(.*?\)", "", stem).strip()
    m = re.match(r"^(.*), The$", name)
    return f"The {m.group(1)}" if m else name


def toml_str(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def render(console, title, revision, hashes, x, y):
    sha256, sha1, md5, crc32 = hashes
    width = 6 if console == "snes" else 4

    def axis(v):
        return f"{{ addr = 0x{v[0]:0{width}X}, type = \"{v[1]}\" }}"

    lines = [
        f"# {title} — made by RetroForge's profile census (W25-02).",
        "#",
        "# The camera was found by playing this exact dump headlessly",
        "# (crates/rf-harness/tests/profile_census.rs): every work-RAM byte was",
        "# scored by how often it held the low byte of the frame's scroll",
        f"# register while the screen moved. x matched {x[2]:.0%} of {x[3]} scrolling frames"
        + (f"; y {y[2]:.0%} of {y[3]}." if y else "."),
        "# Only the camera is claimed; nothing else here was guessed.",
        "",
        "[meta]",
        'profile_version = "0.1"',
        f"title = {toml_str(title)}",
        f'console = "{console}"',
        'region = "ntsc"',
        'authors = ["RetroForge profile census"]',
        "sources = []",
        "",
        "[[identity]]",
        f'sha256 = "{sha256}"',
        f'sha1 = "{sha1}"',
        f'md5 = "{md5}"',
        f'crc32 = "{crc32}"',
        f"revision = {toml_str(revision + ', No-Intro')}",
        "",
        "[capabilities]",
        "full_level = false",
        "hud_separation = false",
        "entity_overlay = false",
    ]
    if console == "snes":
        lines.append('widescreen = "decoded"')
    lines += ["", "[camera]", 'mode = "side_scroller"', f"x = {axis(x)}"]
    if y:
        lines.append(f"y = {axis(y)}")
    return "\n".join(lines) + "\n"


def shipped_hashes():
    """sha256 -> profile path, for every profile already in the tree."""
    out = {}
    for d, _, files in os.walk(ROOT):
        if "profile.toml" in files:
            path = os.path.join(d, "profile.toml")
            for m in re.finditer(r'^sha256 = "([0-9a-f]{64})"', open(path).read(), re.M):
                out[m.group(1)] = os.path.relpath(path, ROOT)
    return out


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    write = "--write" in sys.argv
    games = dict(l.rstrip("\n").split("\t", 1) for l in open(args[0]) if "\t" in l)
    written, skipped = [], []
    shipped = shipped_hashes()
    for line in open(args[1]):
        cols = line.rstrip("\n").split("\t")
        slug = cols[0]
        console, name = slug.split("/", 1)
        archive = games.get(slug, "")
        title = title_of(archive)
        if len(cols) < 7 or cols[1] != "ok":
            skipped.append((slug, cols[1] if len(cols) > 1 else "no result"))
            continue
        hashes = cols[2:6]
        # Layer 1 first (x/y); an SNES game may keep its playfield on
        # layer 2 (x2/y2), tried only when layer 1 finds nothing.
        cells = {c.split("=", 1)[0]: c for c in cols[6:] if "=" in c}
        x, why = pick(*parse_axis(cells["x"]), MIN_MOVING, MIN_DISTINCT)
        ycell = cells.get("y")
        if not x and "x2" in cells:
            x2, _ = pick(*parse_axis(cells["x2"]), MIN_MOVING, MIN_DISTINCT)
            if x2:
                x, ycell = x2, cells.get("y2")
        if not x:
            skipped.append((slug, why))
            continue
        y, _ = pick(*parse_axis(ycell), MIN_MOVING_Y, MIN_DISTINCT_Y) if ycell else (None, "")
        if y and (
            y[0] == x[0]
            or (x[1] == "u16" and y[0] == x[0] + 1)
            or (y[1] == "u16" and x[0] == y[0] + 1)
        ):
            y = None  # one byte cannot be both axes
        path = os.path.join(ROOT, console, name, "profile.toml")
        width = 6 if console == "snes" else 4
        found = f"x ${x[0]:0{width}X} {x[1]} ({x[2]:.0%} of {x[3]})"
        if hashes[0] in shipped or os.path.exists(path):
            where = shipped.get(hashes[0], os.path.relpath(path, ROOT))
            skipped.append((slug, f"already shipped as {where}; census found {found}"))
            continue
        revision = os.path.splitext(os.path.basename(archive))[0]
        if write:
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as f:
                f.write(render(console, title, revision, hashes, x, y))
        written.append((slug, found + (f", y ${y[0]:0{width}X}" if y else "")))
    print(f"{len(written)} profiles" + (" written" if write else " would be written"))
    for slug, what in written:
        print(f"  + {slug}: {what}")
    print(f"{len(skipped)} not written")
    for slug, why in skipped:
        print(f"  - {slug}: {why}")


if __name__ == "__main__":
    main()
