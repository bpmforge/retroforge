#!/usr/bin/env python3
"""Add cited item addresses to game profiles, checked in play (ticket W26-02).

Input is a TSV of community-cited RAM addresses
(`slug addr len type label notes source`, addr hex without `$`) and the
profile census run with those addresses watched
(crates/rf-harness/tests/profile_census.rs, `RF_PCENSUS_WATCH`).

    scripts/profile-items.py watchlist games.tsv items.tsv...   > list.tsv
    scripts/profile-items.py apply games.tsv census.tsv items.tsv... [--write]

`watchlist` adds each game's addresses as the census list's watch column.
`apply` appends a `[[memory_map]]` row per cited address to the game's
profile (creating the profile from the census's hashes when there is
none), with what the census saw in play written into its notes:

- watched, and plausible for its label: "watched in play on this dump";
- the run never reached play: "cited; not yet watched in play";
- watched and implausible (lives that change every few frames, a stage
  number that counts like a timer): dropped, and listed.

An address a profile already has is never added twice. Nothing is
guessed: every row keeps its source URL.
"""

import importlib.util
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("census", os.path.join(HERE, "profile-census.py"))
census = importlib.util.module_from_spec(spec)
spec.loader.exec_module(census)
ROOT = census.ROOT

# Labels that should hold still in ninety seconds of play.
STEADY = ("lives", "continues", "stage", "level", "world", "area", "max", "tanks", "difficulty")
# Labels that a player walking right should move.
MOVING = ("player_x", "player_y", "camera")


def read_items(paths):
    rows = []
    for p in paths:
        for line in open(p):
            cols = line.rstrip("\n").split("\t")
            if len(cols) < 7 or cols[0] == "slug" or cols[1] == "-":
                continue
            slug, addr, ln, ty, label, notes, source = cols[:7]
            try:
                rows.append((slug, int(addr, 16), int(ln), ty, label, notes, source))
            except ValueError:
                continue
    return rows


def read_games(path):
    games = {}
    for line in open(path):
        cols = line.rstrip("\n").split("\t")
        if len(cols) >= 2:
            games[cols[0]] = cols
    return games


def watchlist(games_path, item_paths):
    items = read_items(item_paths)
    for slug, cols in read_games(games_path).items():
        cols = (cols + ["", ""])[:4]
        watch = ",".join(f"{a:X}:{min(n, 4)}" for s, a, n, *_ in items if s == slug)
        print("\t".join(cols + [watch]))


def verdict(label, seen, lo, hi, distinct, changes, ty="u8"):
    """(keep, note) for one watched address."""
    if seen < 100:
        return True, "cited; not yet watched in play"
    # A `bytes` field (BCD digits, a 3-byte score) has no single number
    # worth printing; say only that it was seen to change.
    what = "" if ty == "bytes" else (f"{lo}, " if lo == hi else f"{lo}-{hi}, ")
    note = f"watched in play on this dump: {what}changed {changes} times over {seen} frames"
    if any(k in label for k in STEADY) and changes > 12:
        return False, f"changed {changes} times in play, too often for {label}"
    if label.startswith(("lives", "continues")) and hi > 0x99:
        return False, f"held {hi} in play, too large for {label}"
    if any(label.startswith(k) for k in MOVING) and changes == 0:
        note += " (did not move while the screen scrolled)"
    return True, note


def toml_str(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def apply(games_path, census_path, item_paths, write):
    games = read_games(games_path)
    items = read_items(item_paths)
    shipped = census.shipped_hashes()
    added, dropped, made = [], [], []
    for line in open(census_path):
        cols = line.rstrip("\n").split("\t")
        slug = cols[0]
        rows = [r for r in items if r[0] == slug]
        if not rows:
            continue
        console, name = slug.split("/", 1)
        stats = {}
        for cell in cols:
            if cell.startswith("w="):
                for w in filter(None, cell[2:].split(",")):
                    a, seen, lo, hi, distinct, changes = w.split(":")
                    stats[int(a, 16)] = tuple(map(int, (seen, lo, hi, distinct, changes)))
        ok = len(cols) > 6 and cols[1] == "ok"
        fresh = False
        # "Watched in play" only where the census actually reached play:
        # its camera cleared the bar. A run stuck in menus (or a scrolling
        # title) watched nothing worth claiming.
        played = ok and any(
            census.pick(*census.parse_axis(c), census.MIN_MOVING, census.MIN_DISTINCT)[0] is not None
            for c in cols[6:]
            if c.startswith(("x=", "x2="))
        )
        if not played:
            stats = {}
        path = None
        if ok and cols[2] in shipped:
            path = os.path.join(ROOT, shipped[cols[2]])
        elif os.path.exists(os.path.join(ROOT, console, name, "profile.toml")):
            path = os.path.join(ROOT, console, name, "profile.toml")
        text = open(path).read() if path else None
        if text is None:
            if not ok:
                dropped.append((slug, "the dump did not load, so there are no hashes to match"))
                continue
            archive = games.get(slug, ["", ""])[1]
            title = census.title_of(archive)
            revision = os.path.splitext(os.path.basename(archive))[0]
            text = render_items_only(console, title, revision, cols[2:6])
            path = os.path.join(ROOT, console, name, "profile.toml")
            fresh = True
        if ok and not re.search(r"^\[\[identity\]\]", text, re.M):
            # A shipped profile that never said which dump it is (it could
            # not match anything): this dump's hashes make it match.
            archive = games.get(slug, ["", ""])[1]
            revision = os.path.splitext(os.path.basename(archive))[0]
            sha256, sha1, md5, crc32 = cols[2:6]
            text = text.rstrip("\n") + "\n\n" + "\n".join(
                [
                    "# W26-02: identity added from the census's hash of this dump.",
                    "[[identity]]",
                    f'sha256 = "{sha256}"',
                    f'sha1 = "{sha1}"',
                    f'md5 = "{md5}"',
                    f'crc32 = "{crc32}"',
                    f"revision = {toml_str(revision + ', No-Intro')}",
                ]
            ) + "\n"
            made.append(slug + " (identity added)")
            if write:
                with open(path, "w") as f:
                    f.write(text)
        have = {int(m, 16) for m in re.findall(r"^addr = 0x([0-9A-Fa-f]+)", text, re.M)}
        labels = set(re.findall(r'^label = "([^"]+)"', text, re.M))
        blocks = []
        width = 6 if console == "snes" else 4
        for _, addr, ln, ty, label, notes, source in rows:
            if addr in have or label in labels:
                continue
            keep, seen_note = verdict(label, *stats.get(addr, (0, 0, 0, 0, 0)), ty)
            if not keep:
                dropped.append((slug, f"{label} ${addr:0{width}X}: {seen_note}"))
                continue
            full = f"{notes.rstrip('.')}. {seen_note[0].upper()}{seen_note[1:]}." if notes else seen_note
            blocks.append(
                "\n".join(
                    [
                        "",
                        "[[memory_map]]",
                        f"addr = 0x{addr:0{width}X}",
                        f"len = {ln}",
                        f'type = "{ty}"',
                        f"label = {toml_str(label)}",
                        f"notes = {toml_str(full)}",
                        f"source = {toml_str(source)}",
                    ]
                )
            )
            have.add(addr)
            labels.add(label)
            added.append((slug, label, seen_note.startswith("watched")))
        if blocks and fresh:
            made.append(slug)
        if blocks and write:
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as f:
                f.write(text.rstrip("\n") + "\n" + "\n".join(blocks) + "\n")
    watched = sum(1 for *_, w in added if w)
    print(f"{len(added)} rows {'added' if write else 'to add'} ({watched} watched in play), "
          f"{len(made)} new profiles, {len(dropped)} dropped")
    for slug in made:
        print(f"  new profile: {slug}")
    for slug, why in dropped:
        print(f"  - {slug}: {why}")


def render_items_only(console, title, revision, hashes):
    """A profile for a dump whose camera was not found: identity and items."""
    sha256, sha1, md5, crc32 = hashes
    return "\n".join(
        [
            f"# {title} — made by RetroForge's profile census (W26-02).",
            "#",
            "# This dump's identity comes from hashing it; its RAM addresses are",
            "# cited from community maps (each row's `source`) and, where the",
            "# census reached play, watched there (each row's notes say which).",
            "# No camera: the census did not find one for this game yet.",
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
    ) + "\n"


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    if args[0] == "watchlist":
        watchlist(args[1], args[2:])
    elif args[0] == "apply":
        apply(args[1], args[2], args[3:], "--write" in sys.argv)


if __name__ == "__main__":
    main()
