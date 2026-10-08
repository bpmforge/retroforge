#!/usr/bin/env python3
"""Fill Game info display fields from what each row's notes state (W27-05).

Conservative on purpose: only phrases that say exactly how the value
reads become fields; anything else keeps showing the raw number.

- "minus one" (not about nibbles or containers) -> show_add = 1
- "0 small, 1 super, 2 fire ..." (consecutive from 0) -> names = [...]
- "Hundreds, tens, ones digits" / "BCD digits of the game timer"
  (one digit per byte, most significant first) -> show = "digits"

    scripts/profile-show-fields.py [--write]
"""
import glob
import os
import re
import sys

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "profiles")


def fields_for(label, ty, notes):
    out = []
    low = notes.lower()
    if "minus one" in low and "nibble" not in low and "container" not in low:
        out.append("show_add = 1")
    m = re.match(r"^(?:[A-Za-z ]+:\s*)?((?:\d+\s+[A-Za-z][A-Za-z ]*?\s*,\s*)+\d+\+?\s+[A-Za-z][A-Za-z ]*?)\.", notes)
    if m:
        pairs = re.findall(r"(\d+)\+?\s+([A-Za-z][A-Za-z ]*)", m.group(1))
        if [int(n) for n, _ in pairs] == list(range(len(pairs))) and len(pairs) >= 3:
            names = ", ".join('"' + w.strip().title() + '"' for _, w in pairs)
            out.append(f"names = [{names}]")
    if ty == "bytes" and re.search(r"^(hundreds, tens, ones digits|bcd digits of the game timer)", low):
        out.append('show = "digits"')
    return out


def main():
    write = "--write" in sys.argv
    changed = 0
    for path in sorted(glob.glob(os.path.join(ROOT, "*", "*", "profile.toml"))):
        text = open(path).read()
        blocks = re.split(r"(?=^\[\[memory_map\]\])", text, flags=re.M)
        new = []
        for b in blocks:
            if not b.startswith("[[memory_map]]") or re.search(r"^(show_add|show|names) =", b, re.M):
                new.append(b)
                continue
            label = re.search(r'^label = "([^"]*)"', b, re.M)
            ty = re.search(r'^type = "([^"]*)"', b, re.M)
            notes = re.search(r'^notes = "((?:[^"\\]|\\.)*)"', b, re.M)
            if not (label and ty and notes):
                new.append(b)
                continue
            add = fields_for(label.group(1), ty.group(1), notes.group(1))
            if add:
                changed += 1
                print(f"{os.path.relpath(path, ROOT)} {label.group(1)}: {'; '.join(add)}")
                b = re.sub(r'(^label = "[^"]*"\n)', lambda m: m.group(1) + "\n".join(add) + "\n", b, count=1, flags=re.M)
            new.append(b)
        if write:
            open(path, "w").write("".join(new))
    print(f"{changed} rows {'updated' if write else 'to update'}")


if __name__ == "__main__":
    main()
