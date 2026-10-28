#!/usr/bin/env python3
"""Recipe hub intake validator (ticket W40-01).

Checks a submitted recipe TOML against the moderation rule in
docs/design/RECIPE_HUB.md and prints a verdict:

  ACCEPT   exit 0  (grades listed above `candidate` are clamped, and noted)
  HOLD     exit 2  file carries [[patch]] rows: they wait for the lockstep referee
  REJECT   exit 1  with every reason

It is a filter, not proof of honesty: a human reads every `source` line.
Facts only: no ROM bytes, patches, disassembly or community-map provenance.
Usage: validate-submission.py [--json] FILE.toml
"""
import json
import re
import sys
import tomllib

MAX_BYTES = 256 * 1024
MAX_ROWS = 5000
MAX_STR = 200

# Exact keys the loader (rf_profiles::recipe, deny_unknown_fields) accepts.
KEYS = {
    "function": {"name", "shape_hash", "exact_hash", "address_hint", "mode", "entry",
                 "insn_count", "hooks", "source", "confidence", "grade"},
    "ram": {"addr", "width", "name", "source", "grade"},
    "patch": {"name", "mod", "function", "offset", "value", "source", "grade"},
    "object_table": {"name", "base", "slots", "stride", "x_lo", "x_hi", "y", "y_hi",
                     "type", "state", "source", "grade"},
    "data_region": {"name", "kind", "rom", "size", "stride", "part", "pointer_bytes",
                    "index_ram", "pointer_ram", "via", "confidence", "source", "grade"},
    "tile": {"class", "first", "last", "layer", "tileset", "source", "grade"},
}
HEAD_KEYS = {"format", "id", "engine"}
GRADES = {"observed", "candidate", "confirmed"}
SOURCE_PREFIXES = ("observed:", "rule:", "finder:", "ai:")
HASH_KEYS = {"shape_hash", "exact_hash"}
NAME_RE = re.compile(r"^[a-z0-9][a-z0-9_.\-]{0,63}$")
HASH_RE = re.compile(r"^0x[0-9a-f]{16}$")
# Provenance the rule forbids (D-021, D-024). Heuristic; matched case-insensitively.
BANNED = ["disassembl", "disasm", "decompil", "decomp ", "decomp.", ".asm", ".sym",
          "smwcentral", "romhacking", "datacrystal", "data crystal", "github.com",
          "gitlab", "pastebin", "wiki", "forum", "community", "ram map", "rom map",
          "memory map by", "documented by", "taken from", "ripped", "ips patch",
          "bps patch", ".ips", ".bps", "rom dump", "hex dump"]
HEX_RUN = re.compile(r"(?:\b(?:0x)?[0-9a-fA-F]{2}\b[ ,:;-]*){8,}")
LONG_HEX = re.compile(r"[0-9a-fA-F]{32,}")
B64 = re.compile(r"[A-Za-z0-9+/=]{48,}")


def walk_strings(node, path):
    if isinstance(node, str):
        yield path, node
    elif isinstance(node, dict):
        for k, v in node.items():
            yield from walk_strings(v, f"{path}.{k}")
    elif isinstance(node, list):
        for i, v in enumerate(node):
            yield from walk_strings(v, f"{path}[{i}]")


def check(text, size):
    errs, notes = [], []
    if size > MAX_BYTES:
        errs.append(f"file is {size} bytes; limit is {MAX_BYTES}")
    try:
        doc = tomllib.loads(text)
    except tomllib.TOMLDecodeError as e:
        return ["not valid TOML: %s" % e], notes, 0
    head = doc.get("recipe")
    if not isinstance(head, dict):
        errs.append("missing [recipe] table")
        head = {}
    for k in head:
        if k not in HEAD_KEYS:
            errs.append(f"[recipe]: unknown key `{k}`")
    if head.get("format") != 1:
        errs.append("[recipe] format must be 1")
    if not isinstance(head.get("id"), str) or not NAME_RE.match(head.get("id", "")):
        errs.append("[recipe] id must be a lowercase slug")
    for t in doc:
        if t != "recipe" and t not in KEYS:
            errs.append(f"unknown table `{t}`")
    rows = 0
    clamped = 0
    patches = 0
    for t, allowed in KEYS.items():
        entries = doc.get(t, [])
        if not isinstance(entries, list):
            errs.append(f"`{t}` must be an array of tables ([[{t}]])")
            continue
        for i, row in enumerate(entries):
            rows += 1
            where = f"[[{t}]] #{i + 1}"
            if not isinstance(row, dict):
                errs.append(f"{where}: not a table")
                continue
            nm = row.get("name") or row.get("class") or ""
            if nm:
                where += f" ({nm})"
            for k in row:
                if k not in allowed:
                    errs.append(f"{where}: unknown key `{k}`")
            if t == "patch":
                patches += 1
                v = row.get("value")
                if not isinstance(v, int) or isinstance(v, bool) or not 0 <= v <= 255:
                    errs.append(f"{where}: patch value must be one byte (0..255)")
            if "name" in row and not (isinstance(row["name"], str) and NAME_RE.match(row["name"])):
                errs.append(f"{where}: name must be a lowercase slug (a-z 0-9 _ . -)")
            for hk in HASH_KEYS:
                if hk in row and not (isinstance(row[hk], str) and HASH_RE.match(row[hk])):
                    errs.append(f"{where}: {hk} must be a quoted 0x + 16 lowercase hex digits")
            if t == "function" and "shape_hash" not in row:
                errs.append(f"{where}: shape_hash is required")
            for ik in ("addr", "address_hint", "base", "rom", "index_ram", "pointer_ram"):
                v = row.get(ik)
                if v is not None and (not isinstance(v, int) or not 0 <= v <= 0xFFFFFF):
                    errs.append(f"{where}: {ik} outside 0..0xFFFFFF")
            if t == "ram" and not (isinstance(row.get("width", 1), int) and 1 <= row.get("width", 1) <= 64):
                errs.append(f"{where}: width must be 1..64")
            g = row.get("grade")
            if g is None:
                notes.append(f"{where}: no grade; treated as candidate")
            elif g not in GRADES:
                errs.append(f"{where}: grade `{g}` is not observed|candidate|confirmed")
            elif g != "candidate":
                clamped += 1
            src = row.get("source")
            if not isinstance(src, str) or not src.strip():
                errs.append(f"{where}: `source` note is required")
            elif not src.startswith(SOURCE_PREFIXES):
                errs.append(f"{where}: source must start with one of {', '.join(SOURCE_PREFIXES)}")
            for k, v in row.items():
                if isinstance(v, list) and k != "hooks":
                    errs.append(f"{where}: `{k}` is an array; arrays can carry byte payloads")
    if rows > MAX_ROWS:
        errs.append(f"{rows} rows; limit is {MAX_ROWS}")
    for p, s in walk_strings(doc, "$"):
        last = p.rsplit(".", 1)[-1]
        if last in HASH_KEYS:
            continue
        low = s.lower()
        if len(s) > MAX_STR:
            errs.append(f"{p}: string longer than {MAX_STR} characters")
        for w in BANNED:
            if w in low:
                errs.append(f"{p}: banned provenance/payload word `{w.strip()}`")
        if HEX_RUN.search(s) or LONG_HEX.search(s) or B64.search(s):
            errs.append(f"{p}: looks like a byte payload (hex run or base64)")
    if clamped:
        notes.append(f"{clamped} row(s) claim observed/confirmed; clamped to candidate on intake")
    return errs, notes, patches


def main(argv):
    as_json = "--json" in argv
    args = [a for a in argv if a != "--json"]
    if len(args) != 1:
        print(__doc__)
        return 64
    with open(args[0], "rb") as f:
        raw = f.read()
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        errs, notes, patches = ["not valid UTF-8 (binary data is not allowed)"], [], 0
    else:
        errs, notes, patches = check(text, len(raw))
    verdict = "REJECT" if errs else ("HOLD" if patches else "ACCEPT")
    if as_json:
        print(json.dumps({"verdict": verdict, "reasons": errs, "notes": notes,
                          "patch_rows": patches}, indent=2))
    else:
        print(f"{verdict}: {args[0]}")
        for e in errs:
            print(f"  reject: {e}")
        for n in notes:
            print(f"  note: {n}")
        if verdict == "HOLD":
            print(f"  {patches} [[patch]] row(s): wait for the lockstep referee "
                  "(docs/design/RECIPE_HUB_REVIEW.md); facts must be split into their own file")
    return {"ACCEPT": 0, "REJECT": 1, "HOLD": 2}[verdict]


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
