# W33 results: the stitching wave in numbers

Wave W33 (tickets W33-00 .. W33-10, 2026-10-17/18) moved margin stitching from
"visited areas, main screen only" to one per-layer canvas model with sub-screen
pixels, confidence, persistence and an observed-settings scout. Numbers below
come from the merge commits (`git log --grep W33`), the notes
`W33-00-baseline.md`, `W33-08-vertical.md`, `W33-09-scenes.md`, and the W33-10
re-shoot (release build, Apple M2 Max shared with other jobs, load 10-100;
shots in `docs/screens/w30-w31/`). Gate numbers are `workspace: N passing`.

## Per ticket

| Ticket | What | Key numbers |
|---|---|---|
| W33-00 | Re-check the ~125 px backtrack offset; fps baseline | Offset closed: centre 48,540/48,640 px identical (99.8%); VRAM-visible margins 44,816/48,640 (92%), every difference in the sky-stripe band (W33-02's job). |
| W33-01 | Sub-screen pixels + per-line registers in the frame bundle; stitcher composes main/sub/colour math | Cost 0.008 ms/frame (core + builder), UI compose +0.5 ms/frame, widescreen only. Accuracy hash unchanged (tested). SMW far hills (BG2) fill the margins. |
| (fix) | SNES save state did not store `$212D` (TS) | SNES `PPU_` tag v4 -> v5 (appends TS); state hashes re-pinned once. |
| W33-02 | Exact backdrop per row; per-scene palette table | SMW 32:9: 191/191 backdrop rows exact, 0 banding rows. Palette table: u8 ids, 256 cap. Workspace 2950. |
| W33-03 | One canvas model (`rf-enhance::canvas`) for NES and SNES | SMB3/MM2 margins 0/144,000 px differing from the old path. Workspace 2952. |
| W33-04 | Placement cross-check, per-layer confidence 0-3 | SMW YI1 walk: BG1 100.000% (280,306/280,306), BG2 99.71%; SMB3 1-1 99.69%. A +3 px fault is caught on the first bad frame, canvas byte-identical. Cost 4-13 us per layer per frame. Workspace 2964. |
| W33-09 | Scene keys from level ids, real cut detection, continuous edges | SMB3 title/map/1-1/1-2 = 4 ids; MM2 = 2 scenes + continuous edge; `SceneTracker` 116 us/frame (SMB3), 176 us (MM2). Workspace 2998. |
| W33-05 | Persistence v2, one entry per (ROM sha256, scene id), instant second visit | SMW YI1: full margins at live frame 17 (282,809 cells, 66.7 KB on disk), SMB3 1-1 at frame 5 (34.3 KB), scouts started 0. A canvas of 3,722,583 raw bytes stores as 72,000 (zstd). Workspace 3005. |
| W33-06 | Scout self-check and strategy ladder; Live/Scout cell source | Delta thresholds: >= 99% merge, 95-99% slow, < 95% reject. SMB3 forced to 8 px: probe 85.7% rejected, 4 vs 2 px 99.7%, retried at 4 and accepted (494,529 cells). Profiles: SMB3 99.5%, MM2 98.1%, SMW 100%. Workspace 3041. |
| W33-07 | Auto-discovery of camera, player, hold, gate | With `[scout]` removed: SMW 5120/5120 px, SMB3 2816/2816, MM2 2560/2560 (100% of the hand table). Mega Man X (no hand table): 1024 px, 162,519 cells, 100% agreement. Player finder false positives on camera mirrors: 4/12 down to 0/8 across six games. Workspace 3053. |
| W33-08 | Vertical and 8-way scouting | MM2 Air Man shaft: 8 px refused, 4 px accepted; section canvas 256x416 -> 261x656; agreement 94.9% (re-shot 95.5%); 61,440 cells. Horizontal widths unchanged. Workspace 3063. |
| (fix) dda1a4b | Stale placement candidate learned from a fade | NES scouts merge again in the app: SMB3 493,954, MM2 552,960, SMW 624,161, MMX 162,516 cells. Workspace 3062. |

## Widths reached (scout, whole level pass)

SMW Yoshi's Island 1 5120 px; SMB3 1-1 2816 px; MM2 Air Man 2560 px (stops at
the first scroll lock); Mega Man X highway 1024 px (auto-found settings).

## W33-10 re-shoot (cold cache, in-app scout)

| Game | Scout frames | Cells merged | Agreement (shared cells) | Rung |
|---|---|---|---|---|
| SMW YI1 | 1078 (15.1 s worker, 32 live frames) | 624,161 | 100.0% (336,075) | player_move 8 px |
| SMB3 1-1 | 1460 | 493,954 | 99.8% (372,660) | player_move 4 px |
| MM2 Air Man | 2050 | 552,960 | 98.7% (133,440) | walk 1 px |
| MM2 shaft | 1382 | 61,440 | 95.5% (744,188) | player_move 4 px (8 px refused) |
| Mega Man X | 994 | 161,511 | 100.0% (395,317) | player_move 4 px |

SMW scout deltas: 14 merged on the core thread, 42.26 ms in total, longest
29.48 ms (load ~10; the W32-08 figure under lighter load was 7.65 ms over an
8.2 s pass).

## Live cost (W33-00 table, unpaced, median of 600 frames)

| Game | 4:3 | 32:9 scout off | 32:9 scout on |
|---|---|---|---|
| SMW | 88.5 / 99.7 fps | 50.1 / 44.1 | 53.6 (scene drift, not a gain) |
| SMB3 | 413.9 / 443.2 | 427.3 / 430.0 | 442.9 |
| MM2 | 381.9 / 420.1 | 353.0 / 421.3 | 362.9 |

SNES 32:9 costs about half the unpaced fps (widened PPU render, 800 dots per
line); the NES shows no effect beyond the 10-15% noise between repeated 4:3
controls. Paced SMW with the scout (W32-08): 54.0 fps vs 53.9 / 51.7 without.
A scout frame costs about one live frame (10.3 ms vs 10.1 ms), on its own
thread.

## Cache sizes

SMW YI1 scene 66.7 KB on disk (282,809 cells); SMB3 1-1 34.3 KB; one
widescreen-walk SMW scene 72,000 bytes stored from 3,722,583 raw.

## Discovery times

Not timed as wall-clock in any W33 note. What is recorded: the player must
play a level about a minute (walk right and left, >= 6 jumps, stand still 20
frames); the trials then run each recipe for 300 frames from one snapshot and
tune the hold over six heights on 1100-frame trials (`scout_discovery`). The
finder's pass over the whole `[scout]`-less set is in
`w33_07_discovery.rs` (180 s cap per game in its test).

## What is still not done

Continuous live re-check of scout deltas and `Confidence.scout` (W33-06); a
visible vertical gain on a horizontal 32:9 frame (the MM2 shaft pair looks the
same ON and OFF); areas the scout cannot reach (doors, pipes, scroll locks,
forward-only engines) stay "visited areas only"; live enemies and moving
scenery are never taken from the scout.

## Addendum: discovery wall-clock (from the W33-07 hand-back)

Scout-settings discovery with the hand-made `[scout]` tables removed, one
scripted session each (release build, loaded machine): Super Mario World 92 s,
Super Mario Bros. 3 8 s, Mega Man 2 18 s. Widths reached: 5120/5120,
2816/2816, 2560/2560 px of the hand-made tables. Mega Man X (no hand table):
1024 px, 162,519 cells at 100% agreement.
