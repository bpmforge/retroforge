# Research: Game Identity, Hashing, and Community RE Data

Date: 2026-07-06 · Researcher: principal (direct)

## 1. Game identification / hashing conventions

Profiles must match a game regardless of how the user's ROM file is packaged
(headered vs headerless dumps). The de-facto community convention is
RetroAchievements' per-console hash method, which normalizes headers before
hashing ([RA game-identification docs](https://docs.retroachievements.org/developer-docs/game-identification.html)):

- **NES**: if the file starts with `NES\x1a`, skip the first 16 bytes (iNES
  header) and hash the rest. Otherwise hash the whole file. FDS also skips its
  header.
- **SNES**: if `filesize % 8192 == 512`, skip the first 512 bytes (copier
  header) and hash the rest; otherwise hash the whole file.
- RA uses MD5 for its database keys; No-Intro DATs catalog SHA-1/CRC32 of the
  full (headerless) dump.

**Decision for RetroForge**: `rf-cart` computes, at load time, over the
*normalized* (header-stripped) ROM image: CRC32, MD5, SHA-1, SHA-256. Profiles
key on SHA-256 (primary) and may list MD5 (RA-compatible) and CRC32
(No-Intro-compatible) aliases so profiles can be cross-referenced against the
RA and No-Intro databases. The raw file hash is stored too, for diagnostics.
Region/revision live in profile metadata, not in the key.

## 2. Community reverse-engineering data (feeds the game-profile system)

- **DataCrystal** lives at [datacrystal.tcrf.net](https://datacrystal.tcrf.net/wiki/Data_Crystal)
  (absorbed by TCRF) and remains the central repository of per-game **RAM
  maps** (WRAM/SRAM address → meaning) and **ROM maps** (offset → data
  structure), e.g. [Metroid/RAM map](https://datacrystal.tcrf.net/wiki/Metroid/RAM_map),
  [Contra (NES)/RAM map](https://datacrystal.tcrf.net/wiki/Contra_(NES)/RAM_map).
- Notably there is a [Micro Mages/RAM map](https://datacrystal.tcrf.net/wiki/Micro_Mages/RAM_map)
  — homebrew titles get community maps too, which supports the plan of
  demoing game-aware features on homebrew first.
- Format is human wiki tables (address, size, description). Not machine
  readable, but trivially transcribable into profile `memory_map` entries.

**Decision for RetroForge**: the profile schema's `memory_map` / `rom_map`
sections mirror the DataCrystal shape (address, length, type, label, notes,
source URL) so community maps can be transcribed 1:1, and each entry carries a
`source` citation for clean-room provenance. The debugger's annotation format
is the same structure, so annotations made while reverse engineering export
directly into a profile.

## 3. Implications

1. Hashing/normalization is a Phase-1 ticket in `rf-cart`, not an afterthought
   — every per-game feature keys off it.
2. Ship a `retroforge-tool hash <rom>` developer command early (RA-compatible
   output) so profile authors can verify identity.
3. Profile authoring pipeline = debugger annotations → export → profile file;
   DataCrystal transcription is the bootstrap path.
