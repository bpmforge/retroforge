# RetroForge — MVP Definition

Status: target = end of Phase 5 · 2026-07-06

## 1. What the MVP proves

Not feature-completeness — **architecture truth**. Four claims, each
falsifiable by a demo + CI gate:

1. **The core is honest**: an NES machine accurate enough for the blargg
   CPU/PPU gates, deterministic enough to replay bit-exact.
2. **The boundary holds**: enhancement observes and composes but provably
   cannot perturb simulation (mode-invariant hash test).
3. **Generic enhancement is real**: ultrawide stitched terrain on a game we
   never profiled.
4. **Game-aware enhancement is real**: full-level view with live sprites on
   RF-Scroller (our in-repo fixture platformer, W2-10), driven by profile
   data, not hardcoded logic — plus the pipeline re-proven on Alter Ego
   (PD, a game we didn't design) as the independent check.

## 2. MVP scope

| Included | Explicitly not in MVP |
|---|---|
| NES core: NROM + MMC1 (UxROM/CNROM/MMC3 arrive in Phase 2 but only NROM+MMC1 are MVP-gating), APU, controllers | SNES anything |
| Save states + replay + determinism CI | Rewind |
| wgpu original pipeline, integer scaling, one CRT shader | Shader library, HDR |
| Debug viewers: pattern, nametable, OAM, palette + frame stepping + trace log | Full event viewer, memory search |
| Enhancement runtime + event bus + SceneGraph | Temporal de-flicker (bypass-only) |
| Generic stitcher + ultrawide camera | Room stitching, widescreen policies |
| Profile loader + RF-Scroller full-level profile | Profile editor UI, second profile |
| Overlay API + one Lua overlay script | Plugin manager UI, wasmtime |
| Side-by-side compare view | AI anything, HD packs |

## 3. Acceptance checklist

Core/accuracy
- [ ] SingleStepTests nes6502: 100% official opcodes, cycle-by-cycle bus
- [ ] nestest.log byte-exact; blargg instr_test-v5 + ppu_vbl_nmi +
      sprite_hit pass headless via $6000 protocol
- [ ] 10k-frame double-run state-hash identity + replay-from-log identity
- [ ] Save state → load → continue is pixel- and hash-identical

Boundary
- [ ] Accuracy vs Enhanced: identical per-frame core state hashes over a
      scripted 5k-frame RF-Scroller run (CI)
- [ ] Enhancement crates absent from rf-nes dependency graph
      (validate-arch.sh)

Enhancement demos (recorded + reproducible via replay files in-repo)
- [ ] Un-profiled scroller (Alter Ego or homebrew fixture): stitched canvas
      grows during play; ultrawide view shows visited terrain with fog
      beyond; scene changes create new canvases; canvases persist across
      restart via cache
- [ ] RF-Scroller: full level rendered from ROM decode before
      visiting it; player + active sprites drawn over reconstruction at
      correct positions; original-viewport outline toggle; camera modes
      (original / ultrawide / full-level)
- [ ] Sprite-limit bypass removes flicker on a constructed 9-sprites-a-line
      fixture ROM; Accuracy mode still flickers (both under golden frames)
- [ ] Lua script draws live player-position overlay using profile-published
      addresses

Product floor
- [ ] macOS + Linux + Windows builds from CI; ROM library with normalized-
      hash identity; input remap (keyboard + one gamepad); per-game settings
      persist; 60 fps with enhancement on (M-class hardware)

## 4. Stretch (only if MVP gates green early)

MMC3 + Micro Mages manual smoke test (user-supplied ROM); temporal
de-flicker prototype behind a debug flag; Mesen HD-pack *reader* spike;
second profile (Alter Ego flip-screen "stitch" = trivial room_grid case);
screenshot-to-clipboard + PNG export of full stitched canvas.

## 5. Anti-goals for the MVP period

No SNES work before Phase 4 gates (R-01 discipline). No public plugin ABI.
No AI pipeline code. No promising MVP demos on commercial ROMs — everything
demonstrable runs on in-repo-built fixtures or PD content (fixture doctrine
D-001, CONSTRAINTS §2).
