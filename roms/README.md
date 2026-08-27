# `roms/` — your ROMs, never ours

**Nothing in this directory is committed.** `.gitignore` excludes it, and
project law 5 is absolute: no ROM bytes in git, ever. Put lawfully
obtained dumps here and they stay on this machine.

## Why this directory exists (ticket W11-06, Brad's ruling 2026-08-26)

`docs/VISION.md` §5 asks for ten curated game profiles. There are five,
and every one is against our own fixtures. Brad's 2026-08-20 ruling
permits profiles for real commercial titles, citing published
documentation — and **permitted is not verifiable**: a profile's value is
its decode offsets and camera addresses, and those cannot be checked
without running the ROM they describe.

W9-08 already showed what happens without that check.
`profiles/snes/example-mode7` declared `kind = "room_grid"` from W4-02
until 2026-08-21 while configuring nothing at all, **and loaded cleanly
the whole time**. A profile nobody can check by looking is not evidence,
it is an artifact that resembles evidence.

So: ROMs live here, offsets get verified against them locally, and only
the *profile* — facts plus citations — is ever committed.

## Layout

```
roms/
  nes/      .nes
  snes/     .sfc .smc .fig
```

The library scanner already reads these extensions and identifies files
by normalized hash (`rf-cart`), so adding a folder here under
Settings ▸ Paths is all that is needed to see them in the app.

## What is still true regardless

- **No ROM bytes in git.** Not as a fixture, not as a test vector, not
  base64'd into a doc.
- **No copyrighted title hardcoded in engine code** — a profile is data
  under `/profiles`, never a name baked into a crate.
- **Clean-room from published documentation** (NFR-011). Verifying an
  address against a running ROM is *checking* a published claim.
  Transcribing from a disassembly is not acceptable provenance no matter
  how convenient.
- **Every claim still needs its FR-PROF-003 source citation**, which the
  loader enforces.
