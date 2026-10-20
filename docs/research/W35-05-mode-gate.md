# W35-05: the start gate is the game's mode byte (evidence)

Clean-room: everything below was watched in the emulator (the finder, and the ROM-gated `#[ignore]` probes in
`crates/retroforge/tests/w35_05_mode_gate.rs`, `w33_07_discovery.rs`, `w35_0{2,3,4}_*.rs`). No disassembly, no decomp
rows; every row still carries `source = "observed: auto (W33-07)"`. Release builds, one process at a time under an RSS
watchdog (peak 211 MB).

## 1. The problem

W35-02..04 ranked start gates by what a byte does at scene cuts. That picks bytes that sit still through the level and
differ in menus: per-level data, stack bytes. SMW's real mode byte `$0100` ranked 209th of 603 (it steps outside the
cut zones), and `$13D4` (the pause flag) was thrown out because it is 0 in menus too. A gate that is not about control
lets the scout start in a pause, a door or a speech.

## 2. What the finder scores now

`rf_enhance::scout_finder` labels each frame as one of these states, by observation only:

* **Play**: the level and the rooms the player played in, as one state (a room is played in when the camera moved >= 60
  frames at the pad, or, in a one-screen room, a direction was held >= 100 live-picture frames over a third of it).
* **Each other scene** that held still >= 20 frames (title, map, menus).
* **Cut zones**: the frames in W33-09's zones (doors, fades, loads) plus the short scenes crossed at the pad.
* **Pause**: a Start press in a scene already played in, the picture frozen >= 4 frames **and the game's memory quiet**
  (bytes changing per frame fall to a quarter of their running average), up to the next press in the same scene. The
  window is staged and only kept when the second press closes it: a Start the game ignores (picture still, counters
  ticking) or a Start on a title is not a pause. SMW's pause: 2 bytes change a frame, against 30-60 in play.
* **Speech**: a direction held >= 24 frames on a picture that has not moved for 24 (a text box, a lock; a wall in a
  game whose player freezes against it also looks like this, see 5).

`Frame` gained `start` and `shown` (a hash of the indexed picture, `picture_hash`). Pause and speech frames go to
histograms of their own, so the level's histogram stays the level's play.

Score per byte (`state_information`, `mode_score`): `NMI = I(value; state) / H(state)` with the states weighed equally
(a 100-frame pause counts as much as a 10,000-frame level), times the share of play the value covers (`MIN_PLAY_COVER`
0.6, replaces the per-room 60 % bar: ALttP's `$10` is 7 indoors, 9 outdoors), times `0.7 + 0.3 purity`
(`purity = I / H(value)`: a counter's values belong to no state), times `0.8 + 0.2 calm` (changes away from the cuts).
Hard bars: 95 % constancy on the level (not counting cut zones, pause, speech, and the values the byte reads on a
quarter of the pause/speech frames, at most a fifth of the level: a door that leaves the picture alone), <= 8 values
that matter (>= 1 % of play) while playing, a score >= 0.15. The old `differs >= 0.6` bar is gone (a mode byte holds its
value in a room the player cannot move in). Ranking: score to a fiftieth, then the W35-03 contrast (to a twentieth).

**Guard.** Where a pause was seen and the best byte does not leave its value in it, the best byte that does (value
differs on >= 90 % of >= 12 pause frames, holds through play, calmest) is put second. It needs no `differs`: a pause flag
is 0 in menus too. `scout_discovery` already writes two `start_when` rows and keeps those that survive a scouting run.

Memory: each scene histogram got a lazily allocated zone twin; pause, staging and speech add three. Worst case SNES is
8 x 8 MB + 12 MB; measured peak RSS of the whole session 124 MB.

## 3. Per game: the top-ranked gate

Sessions: SMW's now also walks Yoshi's House a second time (the map's `A`, controllable, out of its right end: a second
room played in; the intro house is not controllable and counted against per-level data) and pauses once; SMB3, MMX, MM2,
SM, CV and ALttP got a Start pause. `rank_<game>` prints the ranking and the values in each state.

| Game | Top gate | Why |
|---|---|---|
| SMW | `$0100 = 0x14` (score 0.52) | the named mode byte; 0x0a/08/06/04 title, 0x0e map, 0x14 in YI1 and the house, 0x11/12/13 in cuts. Second row `$13D4 = 0` (pause flag, 102 pause frames, value 1; the mode stays 0x14 in a pause) |
| SMB3 | `$071E = 0x4F` (0.61) | not named in W35; 0x4f level, 0x32 in all 80 pause frames, 0x23 title/map, 0x7f elsewhere |
| MM2 | `$002A = 0x01` (0.49) | 0 title, 2/5/7 selects, 1 in the level; no pause was recognised (Start opens the weapon menu, memory does not go quiet) |
| MMX | `$0960 = 0xE8` (0.59) | a word `0x1CE8` (and `$0961`): 0xe8 in level, 0x88 in cuts/title; no pause recognised; the scouting run then drops it, `$0400..` is kept |
| Super Metroid | `$0998 = 0x08` (0.58) | the named byte; runner-up `$0001 = 0x9d` at 0.579, a DP pointer: a tie broken by contrast and calm (6 changes off the cuts against 16), see 5 |
| Castlevania | `$0018 = 0x05` (0.66) | the named byte: its 0x0a (a door, 127 level frames) is excused because it is also what 45 speech frames read |
| ALttP | `$0010 = 0x09` (0.63) | the named module: 9 outdoors, 7 in the house, 0x0e in 206 of 226 speech frames |

## 4. Acceptance numbers

* Scout widths and agreement, `discover_*`, all identical to W35-04's table: SMW 5120/5120 px (100.0 %), SMB3 2816
  (99.7 %), MM2 2560 (98.7 %), MMX 2820 (100.0 %), Super Metroid 512 (99.8 %; 99.7 before), Castlevania 768 (100 %),
  ALttP 1024 (99.8 %). Gates kept after the scouting run: SMW `$0100`, `$13D4`; SM `$0998`, `$0001`; CV `$18`.
* Pause (`smw_scout_never_starts_while_paused`, the real `ScoutDriver`, ticked every frame): 150 paused frames, 0 starts;
  one press later it starts once. Rows `(7E0100, 14)` and `(7E13D4, 0)`.
* Door (`sm_gate_is_shut_in_the_door`): the shaft's fade, 24 frames the picture is cut: gate open on 0; open on 1,054 of
  the 1,216 frames of the rooms.
* Speech (`alttp_gate_is_shut_in_a_speech`): 208 frames of a direction held on a frozen picture: open on 8 (4 %); open on
  46 % of the other 15,841 frames (the boot and story take most).
* Unit tests (`rf-enhance`, 24 in `scout_finder`): `state_information` on a separator, a constant, a counter and a
  half-separator; the mode byte beats room data, constants and counters; the guard is second after a pause and absent
  without one; a pause's frames are not held against the byte; a Start in a menu, or one the game ignores, is no pause;
  a speech is a state; `picture_hash`.

## 5. What failed, honestly

* **Thin margins.** SMW wins by 0.517 to 0.503 and only after a second played room was added to the session; SM by
  0.580 to 0.579, settled by calm. Without a second playable room per-level data ($0101, $0065) outranks `$0100`, and the
  intro house, where the byte reads 0x14 with no control, is held against it. Stack and pointer bytes tell states apart
  as well as a mode byte does (SM `$1FF9`, `$0001`); the finder cannot tell them from the value alone.
* **ALttP's `$10` is dropped after the scouting run** (held 41 % of its frames, bar 98 %: the trial machine leaves the
  overworld module), so the saved table carries `$0670`, `$0021`. The finder ranks `$10` first; `scout_discovery`'s
  survival bar is unchanged.
* SM's Ceres corridor ignores Start (no pause there); MM2/MMX's menus were not recognised as pauses. Their pause is
  untested.
* "Speech" is any direction held on a frozen picture; walls in games whose sprite freezes look the same (SMW's level
  end: 500 frames).
* SMB3, MM2, MMX bytes are state-tellers, not verified mode bytes.

## 6. Run

`cargo test --release -p retroforge --test w35_05_mode_gate -- --ignored --nocapture <rank_smw|rank_sm|rank_cv|rank_alttp|rank_smb3|rank_mm2|rank_mmx|smw_scout_never_starts_while_paused|sm_gate_is_shut_in_the_door|alttp_gate_is_shut_in_a_speech>`;
`explore_session` with `RF_GAME`, `RF_SCRIPT`, `RF_PROBES` prints a probe byte per token.
