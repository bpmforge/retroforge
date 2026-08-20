#!/usr/bin/env bash
# Fetch the PeterLemon SNES PPU golden-frame subset (ticket W6-03b;
# FR-CORE-033).
#
# WHY NOT THE MANIFEST ARCHIVE
#
# tests/rom-manifest.toml carried `sha256 = "TODO-network-not-attempted"`
# for a whole-repo snapshot, with a comment saying the size was never
# checked. It is 240 MB (GitHub API, verified) — and, exactly as with the
# 65816 vectors in W6-01a, the archive is unnecessary: individual .sfc
# files are fetchable from raw.githubusercontent.com at the pinned commit.
#
# THE SUBSET, AND WHY THESE FIVE
#
# The acceptance is "the PeterLemon PPU tests SUBSET". W6-03a implements
# BG modes 0 and 1 only, so the subset is the BGMAP tests those modes can
# actually render:
#   * four 2BPP tests, one per background -> mode 0 (four 2bpp layers)
#
# That is a genuinely useful subset rather than a token one: it exercises
# each of the four background layers independently, through real ROM code,
# with a photographic test image where any tile-fetch, bitplane or palette
# error is immediately visible.
#
# NOT the 4BPP BGMAP test, despite its name. Running it shows it sets
# BGMODE to **3** (8bpp BG1 + 4bpp BG2), not mode 1 — verified, and its
# rendered output on a modes-0/1 PPU is garbage. The 8BPP tests need mode
# 3/4 and Mode7/Window/Mosaic/HDMA/Interlace need features later tickets
# add. Fetching ROMs this PPU cannot render would only produce goldens
# that pin a wrong picture, which is worse than no golden at all.
#
# Local only; CI has no ROMs (NFR-006).
set -euo pipefail

COMMIT=350b394e86ec5d62f600b5cbf64cdce3721bb6ef
ROOT="$(git rev-parse --show-toplevel)"
DEST="${RF_PETERLEMON_PPU:-$ROOT/roms/snes/peterlemon-ppu}"
BASE="https://raw.githubusercontent.com/PeterLemon/SNES/${COMMIT}"

ROMS=(
  "PPU/BGMAP/8x8/2BPP/8x8BG1Map2BPP32x328PAL/8x8BG1Map2BPP32x328PAL.sfc"
  "PPU/BGMAP/8x8/2BPP/8x8BG2Map2BPP32x328PAL/8x8BG2Map2BPP32x328PAL.sfc"
  "PPU/BGMAP/8x8/2BPP/8x8BG3Map2BPP32x328PAL/8x8BG3Map2BPP32x328PAL.sfc"
  "PPU/BGMAP/8x8/2BPP/8x8BG4Map2BPP32x328PAL/8x8BG4Map2BPP32x328PAL.sfc"
  # Added at W7-03, once BG modes 2-6 existed to render them. The "4BPP"
  # ROM runs in mode 3 and the 8BPP set in modes 3/4 — all of which the
  # modes-0/1 PPU of W6-03a could only have drawn as garbage.
  "PPU/BGMAP/8x8/4BPP/8x8BGMap4BPP32x328PAL/8x8BGMap4BPP32x328PAL.sfc"
  "PPU/BGMAP/8x8/8BPP/32x32/8x8BGMap8BPP32x32.sfc"
  "PPU/BGMAP/8x8/8BPP/32x64/8x8BGMap8BPP32x64.sfc"
  "PPU/BGMAP/8x8/8BPP/64x32/8x8BGMap8BPP64x32.sfc"
  "PPU/BGMAP/8x8/8BPP/64x64/8x8BGMap8BPP64x64.sfc"
  "PPU/BGMAP/8x8/8BPP/TileFlip/8x8BGMapTileFlip.sfc"
  # Mode 7 (W7-04).
  "PPU/Mode7/RotZoom/RotZoom.sfc"
  "PPU/Mode7/Perspective/Perspective.sfc"
  "PPU/Mode7/StarWars/StarWars.sfc"
)

mkdir -p "$DEST"
count=0
for path in "${ROMS[@]}"; do
  name="$(basename "$path")"
  if [ -s "$DEST/$name" ]; then continue; fi
  # %20 for the spaces some PeterLemon paths carry.
  url="${BASE}/$(printf '%s' "$path" | sed 's/ /%20/g')"
  if curl -fsSL "$url" -o "$DEST/$name.part"; then
    mv "$DEST/$name.part" "$DEST/$name"
    count=$((count+1))
  else
    rm -f "$DEST/$name.part"
    echo "WARN: could not fetch $path" >&2
  fi
done
echo "peterlemon PPU subset: fetched $count into $DEST"
ls -1 "$DEST"
