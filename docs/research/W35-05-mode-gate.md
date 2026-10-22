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

## 7. W35-11: start gates for Zelda 1, SMB3, MM2, MMX

Observation only (D-021): the bytes below were found by the finder from the sessions in
`crates/retroforge/tests/w35_11_start_gates.rs` (no RAM map, no disassembly; the Zelda profile's own RAM-map
comment was not used). Release builds, one process at a time under an RSS watchdog (peak 340 MB).

### 7.1 Zelda 1: why no gate was right (ranked causes, each checked)

Before: the discovered rows were `$0211 = 8`, `$0215 = 0x0a` (sprite-table bytes) and the real `ScoutDriver`, ticked
every frame from power-on, **started a scout in the file-select menus** (frame 2451).

1. **The level's scene holds the menus.** Scene identity is the picture's colours (`palette_scene_id`), so the title
   stays its own scene but the story/name entry/file select and the overworld Link then walks are ONE scene
   (`explore_session`: scene 28709 from frame 606 to the end). Verified with `state_probe`: the true state byte
   (`$12`) reads 1/0x0e/3 on 1,100 frames of that scene before play, 5/7/4/6 in play; the level histogram held
   both, so every real separator read its play value on 51 to 74 % of the "level" and failed `MIN_CONSTANCY`
   (0.95); the bytes that ranked were sprite-table bytes that read the same value in the menus (they only differ from the
   title). *This was the cause.*
2. The mode byte takes several play values (5 field, 7 a screen scrolling in, 4/6 short ones), so a single-value
   gate covers 66 % of play (`MIN_PLAY_COVER` 0.6 barely, and a scouting run through screen flips fails the 98 %
   survival bar). True, but secondary: it only matters once cause 1 is out of the way.
3. Too few contrasting states in the session (no pause): the title was present (248 frames); a pause adds a guard
   row but does not touch cause 1.

Ranking in the scoring itself was tried and dropped: a prelude as one more NMI state, and a leak factor on the
score, both reorder SM/SMW (margins 0.580/0.579 and 0.517/0.503) because their stretch before play is the game
running (Ceres' story pages read mode 8) and indistinguishable from Zelda's menus by evidence alone.

### 7.2 The fix (generic, additive; the old ranking is untouched)

* `scout_finder`: a scene's stretch before the player first played in it (up to the last button press, so the
  standing frames after Start are play) is kept as a copy of the histogram (`preludes`, at most `MAX_SCENES`, only
  when >= 200 frames). `clean_gates()`: bytes whose play values (at most 4, each >= 1 % of play, covering 98 %) are
  open on <= 10 % of the stretch before play and of every other unplayed scene; a byte that dwells (128 frames
  running) on more of its play values than there are played scenes is room data and is out; counters (> 24 values) are out; ranked state variable
  before latch, calmest, fewest values. Save RAM (NES `$6000..`) is excluded (a save is the player's own).
* `Observed::gates` uses it only when the old ranking is **blind**: every one of its first four gates is open on
  >= 95 % of that stretch (`BLIND_OPEN`). SMW (`$0100` 78 %), SM, CV, ALttP, SMB3, MM2, MMX are not blind or have no
  stretch, so their lists are byte-identical to before (rank outputs and discovery tables compared below).
* A gate may be several values: rows on one `start_when` address are alternatives, different addresses all hold
  (`start_gate_open`, `GateCandidate::holds`, `gate_survival`, `docs/design/GAME_PROFILES.md`).
* `ScoutDriver` waits while the live picture is one flat colour (`LiveView::blank`, `picture_is_flat`): the
  black screen between MMX's title and its stage read as "in play" to every byte the finder had.

### 7.3 Per game (`w35_11_start_gates`, real `ScoutDriver` ticked every frame from power-on over a 2,400-frame title
(attract/story), the menus, then play; the finder watched the game's own session)

| Game | Rows found (no hand `[scout]`) | Scouts started: menus / play | Gate open on play |
|---|---|---|---|
| Zelda 1 | `$12` in {5,7,3,4}, `$6F` = 4 | 0 / 2 | 2849 of 2886 frames |
| SMB3 | `$7F50` = 0xF2 (cartridge RAM), `$0302` = 0x28 | 0 / 1 | 1115 of 1238 |
| MM2 | `$2A` = 1, `$36D` = 0x38 | 0 / 1 | 2398 of 2445 |
| MMX | `$7E0352` = 0x11, `$7E0350` = 0x78 | 0 / 2 (1 / 3 without the blank rule) | 1259 of 1345 |

Zelda second visit unchanged (see 7.4). Widths and agreement as before, one process at a time: SMW 5120 px
(100.0 %), SM 512 (99.8 %), CV 768 (100 %), ALttP 1024 (99.8 %), SMB3 2816 (99.7 %), MM2 2560 (99.0 %), MMX 2820
(100 %), Zelda 768 (99.8 %). Gates kept: SMW `$0100`,`$13D4`; SM `$0998`,`$0001`; CV `$18`; ALttP `$10`.

### 7.4 Honest gaps

* SMB3, MM2, MMX rows are not the mode bytes W35-05 named. The ranked mode bytes (`$071E`, `$002A`, `$0960`) do not
  all survive here: with the long title in the session SMB3's pause is not recognised and `$071E` fails the 95 %
  bar; MMX's `$0960` holds 52.7 % of a scouting run. What ships are bytes that tell play from the title/menus/black
  screen on this evidence (sprite and data bytes), and SMB3's first row is cartridge RAM. The tests show 0 starts in
  menus and the gate open on 90 to 98 % of play, not that these are the games' mode bytes.
* Second level: not verified for SMB3/MM2/MMX (one level per session; a second needs stage select scripting). Zelda:
  two overworld screens (flip to and fro), no dungeon; `$12`'s dungeon values are unseen and the gate would be shut
  there. The in-app finder starts at "Find", so it never sees the menus: there the old ranking stands (the gap
  above applies to what the app saves).
* The stretch before play is judged by Start/A/B presses; a game with menus answered by a direction only is not seen.
* `clean_gates` takes whatever separates this session; per-session data that happens to hold (e.g. a flag set at
  game start) can pass; `GATE_SURVIVAL` and the second visit are the checks.

### 7.5 Run

```
cargo test --release -p retroforge --test w35_11_start_gates -- --ignored --nocapture <z1|smb3|mm2|mmx>_start_gate
cargo test --release -p retroforge --test w35_05_mode_gate -- --ignored --nocapture rank_z1   # RF_WATCH, RF_KEEP, RF_CLEAN
```

### 7.7 W35-11b: in the app, and a second area

**In the app.** The core thread now starts the finder watching (`ScoutFind::passive`) on the first widescreen
frame, so the title and menus before "Find scout settings" are in its histograms; asking for it arms the one
that has seen them (`arm`/`disarm`; after an answer a fresh one starts). Memory is the finder's own bound
(`MAX_SCENES` histograms + at most as many press copies, 100 to 110 MB peak in the runs). It watches only while
widescreen is on (the scout is a widescreen feature). Real core thread, real time (`z1_in_app_start_gate`, ~3 min):
Zelda played from power-on through the menus, "Find" pressed in the field: it saved
`start_when` `$6F`=4 and `$12` in {5,7,3,4} (the same rows as the harness); a fresh core with that table over the
2,400-frame title, attract and menus started **0** scouts, and **2** in play.

**Second area (generic rule).** A gate is now validated against every scene the player played in (the level, the
rooms beside it, another stage), not one (`open_in_every_played_scene`, `MIN_SCENE_OPEN` 0.9 of each scene's
frames outside the cuts' zones, the values the byte reads in a pause or speech left out): a gate shut on more than a
tenth of any of them is widened to the values the byte reads in play in all of them (at most `MAX_SET_VALUES` = 8,
covering 98 % of each) or dropped. Clean gates take their values from the same union. The second area enters the
finder's session as a snapshot made by another run of the game (`discover_game_with`); no ROM bytes in git.
Rows: the first two gates the finder is sure of (`MIN_GATE_CONFIDENCE`), not the first two with the sure ones
kept (MM2 had shipped one row because the second was under the floor).

| Game | Second area | Outcome |
|---|---|---|
| Zelda 1 | the old man's cave on the first screen (scene 56468, 488 frames; not a dungeon) | rows `$6F`=4, `$0530` in {0xAA,0xFF}; menus 0, play 3, cave gate open 500 of 500 frames |
| MM2 | another stage from the select screen | `$2A` was shut there (reads another value): widened to {1,2}; rows `$36D`=0x38, `$2A` in {1,2}; menus 0, play 2, stage 2 open 1047 of 1047 |
| SMB3 | not reached | 1-1 not cleared by the scripts (Mario dies near its end, 1-2 stays locked): gap |
| MMX | not reached | stage select comes after the intro stage, X dies in a loop under the scripts: gap |
| Zelda dungeon 1 | not reached | the entrance was not found by walking; the cave stands in for it: gap |

Without the second area MM2's `$2A`=1 alone was a shipped row that is shut in other stages; the rule found it. The
second-area runs also made the finder rank differently (`$36D` first), and `$36D` is open in MM2's title: with only
one row the scout started in the menus (frame 430), which the row-picking fix above repairs (two rows). ALttP: its
mode byte reads 0x0e in text boxes the finder did not recognise as speech (1,442 frames, 16 % of the field); counting
them made the gate "shut" and dropped it, so values read in a pause or speech are excluded from the check (the same
exclusion `frozen_values` already makes in the ranking). Re-run, all as in 7.3: SMW `$0100`+`$13D4`, SM `$0998`+`$0001`,
CV `$18`, ALttP `$10` kept; widths and agreement unchanged; the speech, door and pause tests pass.

Not verified for SMB3/MMX/Zelda-dungeon: nothing in the finder is shipped for them from a second area, so a gate
there that is shut elsewhere is not excluded; see the gaps above.
