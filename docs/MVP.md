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
- [x] SingleStepTests nes6502: 100% official opcodes, cycle-by-cycle bus
      — `cargo test -p rf-nes --release vectors` (4 passed: official +
      unofficial opcode vectors, W1-01/W1-03)
- [x] nestest.log byte-exact; blargg instr_test-v5 + ppu_vbl_nmi +
      sprite_hit pass headless via $6000 protocol
      — `cargo test -p rf-nes --release nestest blargg` (nestest 1 passed;
      blargg 5 passed, incl. `ppu_vbl_nmi_known_good_roms_still_pass`
      10/10 and `sprite_hit_tests_all_eleven_pass`)
- [x] 10k-frame double-run state-hash identity + replay-from-log identity
      — `cargo test --release -p retroforge --test determinism -- --ignored`
      (2 passed, W1-08)
- [x] Save state → load → continue is pixel- and hash-identical
      — `cargo test -p retroforge --test save_state` (5 passed, W2-04)

Boundary
- [x] Accuracy vs Enhanced: identical per-frame core state hashes over a
      scripted 5k-frame RF-Scroller run (CI)
      — `cargo test --release -p retroforge --test mode_invariant_5k --
      --ignored` (W5-08): 5000 frames with Right held, both modes, every
      frame's core state hash compared and identical, with 312 distinct
      video frames observed so the run demonstrably drove the game.
      Plus the 10-frame corpora at `mode_invariant_corpus` (6 passed) and
      `sprite_overlay_mode_invariant` (3 passed).
- [x] Enhancement crates absent from rf-nes dependency graph
      (validate-arch.sh) — `scripts/validate-arch.sh` → `arch OK`, run in
      CI on every push (rule 1)

Enhancement demos (recorded + reproducible via replay files in-repo)
Capture: `docs/demo/rf-scroller-full-level.png`, regenerated from
`fixtures/replays/unprofiled-scroller.rfreplay` by
`cargo test -p retroforge --test demo_capture -- --ignored`. See
`docs/demo/README.md`.
- [x] Un-profiled scroller (Alter Ego or homebrew fixture): stitched canvas
      grows during play; ultrawide view shows visited terrain with fog
      beyond; scene changes create new canvases; canvases persist across
      restart via cache
      — `fixtures/replays/unprofiled-scroller.rfreplay` is checked in and
      `cargo test -p retroforge --test unprofiled_scroller_replay` (W5-08)
      replays it, asserting the canvas grows past one console viewport
      and that a single continuous scroll stays ONE scene. Mechanisms
      unit-gated at `rf-enhance` (68 passed; W4-03a/b/d), cross-restart
      persistence at W4-03b.
      **Read "un-profiled" correctly:** it describes how the session is
      DRIVEN, not which ROM. The stitcher, fog and scene identity never
      consult a profile — that is what makes them the generic fallback —
      so RF-Scroller driven without loading its profile is a faithful
      un-profiled session. Alter Ego is fetch-only with no redistribution
      grant (W0-03), so no replay of it could be checked in and the
      "in-repo" half of this line could not be met with it at all.
- [x] RF-Scroller: full level rendered from ROM decode before
      visiting it; player + active sprites drawn over reconstruction at
      correct positions; original-viewport outline toggle; camera modes
      (original / ultrawide / full-level)
      — decode verified against the running game on **all 96 raw tile
      columns** (`rf-harness/tests/level_decode_vram.rs`, W5-02b + W5-02c);
      sprites at world positions, outline, and both cameras in
      `rf-enhance/tests/level_view_offline.rs` +
      `retroforge/tests/level_view_demo.rs` (W5-03). Scene data only —
      GPU compositing of a `SceneGraph` is W4-03c's.
- [x] Sprite-limit bypass removes flicker on a constructed 9-sprites-a-line
      fixture ROM; Accuracy mode still flickers (both under golden frames)
      — `cargo test -p retroforge --test sprite_overlay_mode_invariant`
      (3 passed, W3-05a)
- [x] Lua script draws live player-position overlay using profile-published
      addresses
      — ticket W5-07 connected them. `rf.mem.read_u8/read_u16` read live
      published memory, `rf.profile.addr(label)` resolves a
      `[[memory_map]]` label, and `rf.gui.*` emit real draw commands.
      `cargo test -p retroforge --test lua_overlay_demo` (4 passed) runs
      the **shipped** `plugins/examples/player-overlay/main.lua` against
      the RF-Scroller fixture and asserts the marker MOVES with the
      player and sits exactly at `player_x`. The example no longer
      contains an address literal.

Product floor
- [x] macOS + Linux + Windows builds from CI; ROM library with normalized-
      hash identity; input remap (keyboard + one gamepad); per-game settings
      persist; 60 fps with enhancement on (M-class hardware)
      — 3-OS matrix in `.github/workflows/ci.yml` (`build` job);
      `crate::library` keyed on normalized hashes (W2-07);
      `crate::bindings_store` (W2-06, keyboard + gilrs);
      `crate::game_settings` (W2-07); **60.15 fps measured with FrameBundle
      assembly enabled**, 8.68 ms/frame with layer extraction on
      (`frame_bundle_perf`, release, M-series)

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
