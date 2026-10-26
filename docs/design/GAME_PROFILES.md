# Design: Game Profile System (`rf-profiles`)

Profiles are **data, not code**. They carry per-game knowledge: identity,
memory/ROM maps, decode rules, enhancement settings. Complex behavior beyond
declarative rules is a plugin (see PLUGINS.md) referenced *by* a profile.

## 1. Format and identity

- Format: TOML (human-authored, diffable, comments for provenance). Parsed
  into versioned Rust structs; `retroforge-tool profile validate` in CI.
- Identity: normalized-ROM hashes (header-stripped; RA/No-Intro compatible —
  see `docs/research/game-identity-and-re-data.md`). One profile may list
  multiple revisions with per-revision address overrides.
- Versioning: `profile_version` (schema semver) + `revision` (content);
  loader rejects newer majors, warns on unknown keys.

<!-- ANCHOR: schema -->
## 2. Schema (v0)

```toml
[meta]
profile_version = "0.1"
title = "Example Platformer"
console = "nes"                      # nes | snes
region = "ntsc"
authors = ["..."]
sources = ["https://datacrystal.tcrf.net/wiki/..."]   # clean-room provenance REQUIRED

[meta.requires]                       # optional assertions, validated against rf-cart detection at load
mapper = "MMC1"                       # NES mapper name/id
chips = []                            # SNES enhancement chips this profile assumes

[[identity]]
sha256 = "…normalized rom hash…"
md5 = "…"                            # optional, RA cross-ref
crc32 = "…"                          # optional, No-Intro cross-ref
revision = "1.0"

[capabilities]                        # what this profile unlocks (UI gates on these)
full_level = true
hud_separation = true
entity_overlay = true
widescreen = "decoded"               # none | stitched | decoded
fast_load = true
smooth_camera = false

# ---- runtime knowledge (RAM) — DataCrystal-shaped rows ----
[[memory_map]]
addr = 0x0086
len = 1
type = "u8"
label = "player_x_screen"
notes = "X within current screen"
source = "https://datacrystal.tcrf.net/..."
# optional, how Game info shows it (W27-04): show_add = 1 (stored minus
# one), show = "digits" (a digit per byte), names = ["Small", "Super"]
# (the value picks a name), max_label = "health_max" (drawn as a bar)

[camera]
mode = "side_scroller"               # side_scroller | top_down_rooms | static
x = { addr = 0x071C, type = "u8", scale = 1, page = { addr = 0x071A } }
hud = { region = "top", scanlines = [0, 31], detect = "sprite0_split" }

[entities]
table = { addr = 0x0400, stride = 16, count = 8 }
fields = { x = 0, y = 4, kind = 12, active = { offset = 15, mask = 0x80 } }
offscreen_valid = false              # if false, only spawn points are drawn off-screen

# ---- static knowledge (ROM) ----
# NOTE: `source` is MANDATORY on every [[memory_map]] and [[rom_map]] row
# (FR-PROF-003) and validation FAILS without one — enforced by the loader
# since ticket W4-02a. It is the clean-room provenance field: D-005 makes
# community profile intake deny-by-default with provenance required, and
# an unenforced `source` would let a profile assert a RAM map with no
# stated origin. This example omitted it until W4-02a, so the spec's own
# example failed the spec's own requirement — anyone implementing from it
# would have produced an invalid profile.
[[rom_map]]
offset = 0x1D00
len = 0x40
label = "level_pointer_table"
type = "ptr_table"
count = 32
source = "https://datacrystal.tcrf.net/..."   # REQUIRED — FR-PROF-003

[decode]
kind = "metatile_screens"            # selects a built-in decoder family
metatile = { table = 0x2200, size = 4, chr_bank_reg = "mmc1_chr0" }
screens  = { width = 16, height = 15, order = "column_rle" }
palettes = { table = 0x3F00_ref, per_area = true }
collision = { table = 0x2600, bits = "solid,platform,hazard" }

[antiflicker]
mode = "default"                     # default | off | aggressive
exclude_oam = []                     # sprite indices never reconstructed
blink_periods = [2]                  # known intentional blink cadences to respect

[atmosphere]                         # ticket W16-10: pin the atmosphere-layer
plane = 0                            # heuristic's plane directly, skip detection
                                      # (this example's console = "nes", so 0 is
                                      # the only legal plane; SNES allows 0-3)
tint = "#336699"                     # optional, "#rrggbb"
strength = 0.6                       # optional, 0.0..=1.0
ladder = "active"                    # optional: shadow | advisory | active (default active)

[overclock]
superfx_max = 300   # ticket W30-01: cap the Super FX overclock (100..=800, step 100)
cpu_max = 400       # ticket W30-08: cap the S-CPU overclock ceiling (100..=400, step 100)
sa1_max = 400       # ... and the SA-1's
nes_extra_lines_max = 120   # ... and the NES extra vblank lines (0..=240)
tick_counter = 0x7E0012     # a WRAM byte the game increments once per logic tick
target_tick_hz = 60         # the game's normal logic rate (1..=60)

[heights]                   # ticket W30-06: raise the live screen into a diorama
layer = 1                   # SNES BG layer 1..=4 whose tilemap is read (NES: ignored)
tile_px = 16                # grid cell: 8, or 16 (= 2x2 tiles, top-left tile's number)
[heights.tiles]             # tile number (hex string) -> height in eighths, 0..=64
"0x0A4" = 16

[loading]
[[loading.wait_loops]]
pc = 0xC12A
until = { addr = 0x0778, equals = 0 }
label = "level decompression wait"

[plugins]                             # optional code companions, user-approved
required = []
optional = ["example-minimap@0.1"]

[mods]                                # explicit game-logic patches, OFF by default
[[mods.patch]]
id = "no-screen-shake"
description = "..."
addr = 0xC500
replace = [0xEA, 0xEA]

[[text.region]]                       # ticket W8-12: named boxes of on-screen text
id = "banner"
x = 0
y = 0
width = 256
height = 16

[[text.entry]]                        # what may replace one string, OFF by default
region = "banner"
original = "GAME OVER"                # the game's OWN words, verbatim
accessible = "The game has ended."
translations = { fr = "PARTIE TERMINEE", ja = "\u30b2\u30fc\u30e0\u30aa\u30fc\u30d0\u30fc" }
```

`[text]` (ticket W8-12) declares translation and accessibility overlays.
Like `[widescreen]` and `[mods]` it has **no `enabled` field**: a profile
describes what a translation *would* be, and the user decides whether to
have one — which is what keeps law 6's "a fresh install boots in Accuracy
Mode" true of a machine that happens to load the profile.

`original` is stored as **text rather than a hash**, and that is a
requirement rather than a convenience. W8-12's third criterion is that
"text an overlay replaces is identifiable in the ledger, so a player can
always tell what was changed" — a ledger that named only the region would
satisfy a careless reading and still leave the player unable to read what
the game actually said. `rf_enhance::overlay` quotes `original` back into
every ledger row.

Regions are **declared, not detected**. `ENHANCEMENT_RUNTIME.md` §6 lists
HUD/text detection as a later AI capability; a human writing the box down
is what makes the feature work today and what makes a wrong overlay a
wrong profile line rather than an opaque misfire. An entry naming an
undeclared region is refused rather than ignored, since a typo'd id would
otherwise silently drop one line of a translation.

Decoder families (`decode.kind`) are implemented once in `rf-enhance` and
parameterized by data: `metatile_screens`, `room_grid`, `tilemap_direct`,
plus `custom` (delegates to a named plugin). New families are added when ≥2
games need the same shape — resist one-off engine code in the runtime.
Families are **versioned** (schema v0.2 adds `decode.family_version`): a
behavioral change to a family bumps its version; the loader refuses a
newer major, mirroring `profile_version` semantics — profiles never
silently re-decode differently under an upgraded emulator.

Community-submitted profiles pass license-gated intake (D-005,
FR-PROF-007): `[meta]` requires `license` (SPDX) alongside `sources`;
deny-by-default at the Phase-9 submission CI.

### `[atmosphere]` (ticket W16-10)

`[atmosphere]` (`rf_profiles::schema::Atmosphere`) pins a background plane
directly as the atmosphere-layer heuristic's plane
(`rf_enhance::atmosphere`, `HEURISTIC_ID = "atmosphere-layer"`), the same
"a profile may pin the plane directly, skip detection" mechanism
`ENHANCEMENT_WAVE_16.md` §4 and `ENHANCEMENT_RUNTIME.md` §2a describe:

- `plane` (required, `u8`) — the background plane index. Range is
  per-console and checked at load time against `[meta].console`: NES has
  one background plane (`0`); SNES has up to four (`0..=3`), per
  `rf_core_api::PixelLayer::Background(n)`'s own doc that BG mode decides
  which of 0-3 exist. Out of range fails to load
  (`ProfileError::AtmospherePlaneOutOfRange`).
- `tint` (optional, `"#rrggbb"`) — a cosmetic hint for the fog pass
  (W16-04). Anything else fails to load
  (`ProfileError::AtmosphereInvalidTint`).
- `strength` (optional, `0.0..=1.0`) — out of range fails to load
  (`ProfileError::AtmosphereStrengthOutOfRange`).
- `ladder` (optional, `"shadow" | "advisory" | "active"`) — which
  `crate::trust::TrustState` rung to pin `HEURISTIC_ID` to. Absent
  defaults to `active`: a profile author who names a plane at all is
  declaring a known fact, not a shadow-mode guess — the opposite default
  from the heuristic's own fresh-install-shadow rule (D-004). An
  unrecognised value fails to load as a schema error.

`rf_enhance::atmosphere::apply_profile_pin(&Profile, &mut TrustLadder) ->
Option<AtmospherePin>` reads the table and calls `TrustLadder::pin`
(returning `None`/leaving the ladder untouched when the table is absent).
A caller (the app shell) still calls `rf_enhance::atmosphere::pinned_layer`
itself with the pin's `plane` and the frame's live sub-screen buffer to
build the actual `SceneLayer` — `apply_profile_pin` only decides trust
state, since it never sees a live frame.

Two profiles carry this table today, and only two: `profiles/snes/
rf-scroller-s` uses `ladder = "shadow"` purely to exercise the schema
against a real, loadable profile — its own fixture (`fixtures/snes/
rf-scroller-s/FORMAT.md`) has no colour-math/translucent plane at all, so
`shadow` is what keeps the entry from asserting an effect that was not
built. No commercial profile carries it: `ENHANCEMENT_WAVE_16.md` §4 is
explicit that Super Metroid's Norfair heat is palette cycling, not a
translucent colour-math plane, so `profiles/snes/super-metroid` is not a
candidate without a separate, later palette-cycle detector or profile
flag; there is no ALttP profile in this repository to add one to either.
A future commercial profile earns this table the same way `[antiflicker]`
or a `[decode]` row does — a cited, checkable source naming the plane, not
an assumption.

### `[overclock]` (tickets W30-01, W30-08)

`[overclock]` (`rf_profiles::schema::Overclock`) is optional and every key in
it is optional. The overclock rows themselves are per-game, off-by-default,
Enhanced-only player settings; the profile only limits them and tells the
speed governor what it cannot work out alone. The loader rejects a bad value
naming its key.

| Key | Range | Meaning |
|---|---|---|
| `superfx_max` | 100..=800, step 100 | Highest Super FX overclock (W30-01). |
| `cpu_max` | 100..=400, step 100 | Highest S-CPU ceiling. `100` = this game may not be overclocked (the row becomes unavailable). |
| `sa1_max` | 100..=400, step 100 | Highest SA-1 ceiling, likewise. |
| `nes_extra_lines_max` | 0..=240 | Most NES extra vblank lines. `0` = none (row unavailable). |
| `tick_counter` | `0x7E0000..=0x7FFFFF` | The WRAM address of a byte the game increments by one each time its logic tick finishes. The governor's tick signal; without it the S-CPU's idle observer is used. |
| `target_tick_hz` | 1..=60 | The game's normal logic rate. Without it the governor learns it. |

**Why the governor needs `tick_counter` and `target_tick_hz`.** An overclock
must only remove slowdown ("we don't want to distort the game, we are
enhancing it"). Governed mode, the default, raises a chip's clock only while
the game ticks below its normal rate, and lowers it the moment it does not;
the player's slider is the ceiling. To do that it counts logic ticks per
second. By default a tick is a frame in which the S-CPU reached an idle wait
(`frame_idle`), and the normal rate is learned: the most common ticks-per-
second seen at 100% while the picture is changing, needing 10 such seconds.
That fails for some games (Gradius III almost never idles visibly, so the
governor reports "No speed signal for this game; add a tick counter to its
profile" and never raises the clock). A `tick_counter` fixes the signal, and
`target_tick_hz` fixes a game that is slow from its very first frame, which
the learned mode would take for its normal speed. Find the counter in the
profile builder's RAM search: it is the byte that changes by exactly +1 each
logic tick (not each frame, for a 30 Hz game).

The governor also has a hang guard: a game that stops changing the picture
for 180 frames while overclocked is returned to 100% for the rest of the
session ("Turned off: the game stopped responding at higher speed"). That
protects games that stall at a raised clock without any profile entry.

### `[scout]` (ticket W32-07)

`[scout]` (`rf_profiles::schema::Scout`) is optional. It is the recipe for the
level scout (`crates/retroforge/src/scout.rs`): a second machine restored from
a snapshot of the live one, driven ahead of (and behind) the camera so the
widescreen margins show the real level, not only visited areas. It runs only
in Game-Aware / Enhanced with widescreen on at an aspect wider than the game's
own picture, once per scene, and its cells fill only UNKNOWN cells of the live
canvases (live wins). Every address row needs a `source`, like
`[[memory_map]]`, and the addresses are found by observation; the loader
rejects a bad key or row naming it.

| Key | Meaning |
|---|---|
| `strategy` | `"player_move"` (move the player's X words; the game's own follow logic moves the camera and streams the level) `"walk"` (hold the pad direction on the scout's own controller; Mega Man 2, whose camera follows only a walking player) or `"camera_write"` (write the camera X words; most games snap them back). |
| `step_px` | Pixels per scout frame, 1..=64. |
| `frame_budget` | Most frames one pass may run, 1..=20000. |
| `stall_frames` | A pass ends after this many frames with no new level, 1..=3000 (default 120). |
| `settle_frames` | Live frames to wait after a scene is entered (default 90). |
| `dirs` | Passes, each restarting from the snapshot: `"right"` / `"left"` on a horizontal scene, `"down"` / `"up"` on a vertical one (the scout follows the axis the scene tracker names for the scene, W33-08). Default: right + left, or down + up when the profile names none on that axis. |
| `pin` | `true` = try only `strategy`. Without it the scout starts there and falls back down the ladder (player_move, walk, camera_write) when its own self-check refuses a rung; a rung with no addresses to act on is skipped (W33-06). |
| `[[scout.addr]]` | Rows with `role` = `camera_x` / `player_x` (16-bit little-endian words), `camera_y` / `player_y` (the same for a vertical scene, W33-08; `player_y_hi` names the high byte, a `player_y` with none is one byte as in Mega Man 2), `hold` (a byte `value` written every scout frame: invincibility, Mario kept out of reach; a hold on a `player_y` byte is released on a vertical pass) or `start_when` (a byte `value` the LIVE game must hold, for 30 frames running, before a scout starts: "in a level, player alive"; W35-11: rows on one `addr` are alternatives, a mode byte that reads 5 on a field and 7 while the screen scrolls is two rows, and rows on different addresses must all hold). |

**Auto-discovered rows (W33-07).** A profile does not have to carry a
hand-made `[scout]`. The Enhancements action "Find scout settings for this
game" watches a play session (walk right and left, jump >= 6 times, stand
still), names the camera X pair, the player X word (high half by wraps), a Y
byte and a start gate by observation, tries the candidates on a second
machine to choose the player word, hold, hold height and strategy, and offers
to save the result as a `[scout]` table whose rows read `source = "observed:
auto (W33-07)"`. It is written to the PLAYER's own profile, never into the
repo's `profiles/` (shipped profiles stay byte-identical; tested). A shipped
or saved table is always tried with the W33-06 self-check, so a wrong row costs
margins, never corruption. With the hand tables removed the finder reached SMW
5120 px, SMB3 2816, MM2 2560 (100% of each), and a scout for Mega Man X with
no hand table: 1024 px.

A scout stops at its frame budget, after `stall_frames` without new level, or
when the camera jumps (a scene change). Sprites are never taken from it. See
`docs/research/W32-07-scout-smw.md` for the evidence behind the Super Mario
World table, and for which recipes failed there.

### `[scene]` (ticket W33-09)

`[scene]` (`rf_profiles::schema::Scene`) is optional. It says which RAM bytes
name the level (or the room behind a pipe) the game is in, so the canvas model
(`rf_enhance::canvas`) keys its scenes by what the game says and not by what
the picture looks like. Without it the picture and the scroll decide
(`rf_enhance::scene_tracker`), which already tells a title, a map and a level
apart; with it a pipe room and the level it came from are two scenes and the
level's canvas is intact when the player comes back out. The rows are found by
observation (`docs/research/W33-09-scenes.md`); every row needs a `source`.

| Key | Meaning |
|---|---|
| `[[scene.level_id]]` | One row per field of the key: `addr` (NES `0..=0xFFFF`, SNES `0x7E0000..=0x7FFFFF`), `len` (bytes, little-endian, `1..=4`, default 1), `idle` (what the row reads when no level is running, default 0), `decides` (default true), `notes`, `source`. At most 8 rows and 16 bytes. |
| `sections` | `"axis"`: a switch of the scroll axis (a horizontal section into a climb) starts a new section scene, joined to the old one by a continuous edge. Off by default when `level_id` rows exist, on for a game without any. |

The scene key is the bytes of every row, concatenated. While every `decides`
row reads its `idle` value the game is "not in a level" (a title, a map) and
the picture names the scene instead. A row with `decides = false` joins the
key without deciding on its own: a word that holds stale values on the map but
is level-specific inside a level. A level that is still writing its pointers
(Super Mario Bros. 3's first word settles over five frames, Super Mario
World's pointer ahead of its flag by eight) is not named until the key has
held for `SETTLE_FRAMES` frames, and the frames in between are not stitched
into any scene.

```toml
[scene]
sections = "axis"                    # Mega Man 2: the stage has no section byte

[[scene.level_id]]
addr = 0x002A
notes = "Stage index (1 = Air Man for the whole stage)."
source = "observed: W33-09 note"
```

A scene change is recorded as an edge (`SceneEdge`: from, to, the scroll
position at each end, `continuous`). Only a `continuous` edge, one the picture
flowed across as the scroll said, may show the neighbouring scene in a margin;
a door, a pipe or a fade never does.

### `[superfx_polygons]` (ticket W31-02)

`[superfx_polygons]` (`rf_profiles::schema::SuperfxPolygons`) is optional and
**every key is required**. It tells the Super FX polygon observer where a game's
3D rasteriser can be seen handing the GSU a polygon. The addresses are found by
observation only (watch GSU RAM reads and `PLOT`s in our own emulator; no
disassembly, no leaked source; method in `docs/research/W31-01-starfox-polygon-map.md`).
Nothing reads it in Accuracy Mode; the observer is read-only, so the machine is
bit-identical with it on or off.

| Key | Range | Meaning |
|---|---|---|
| `pc_bank` | `0x00..=0xFF` | GSU program bank of the two code addresses below. |
| `count_read_pc` | `0x0000..=0xFFFF` | Address of the instruction that reads a polygon's vertex count; seeing it start a polygon. |
| `count_addr` | `0x0000..=0xFFFF` | GSU RAM address of that count. |
| `buffer_addr` | `0x0000..=0xFF00` | GSU RAM address of the clipped-corner work buffer: 4-byte entries (x word, y word); only the low byte of each is meaningful. |
| `walker_pcs` | `[start, end)`, `start < end <= 0x10000` | The edge walker's code range; its reads of the buffer say how many corners the polygon has. |
| `viewport` | `[w, h]`, each `1..=256` | The 3D view's size in pixels. |

The first `PLOT` after the walker has read the buffer closes the polygon's
corner list; its fill ends at the next count read or at `STOP`.

### `[heights]` (ticket W30-06)

`[heights]` (`rf_profiles::schema::Heights`) is optional. It is the
"height painter": a table the profile author paints by hand (the Profile
Builder painter, W30-06 part B), mapping a tile number to a wall height. It
exists for a game with **no level decoder** — one that lays its world out
tile by tile as it scrolls, so there is no level to decode and nothing in
the ROM that says which tile is a wall. Nothing is inferred from game code.

| Key | Range | Meaning |
|---|---|---|
| `layer` | 1..=4 (default 1) | The SNES BG layer whose tilemap is read. Ignored for NES. |
| `tile_px` | 8 or 16 (default 8) | The grid cell a height applies to. `16` groups 2x2 tiles and uses the top-left tile's number. |
| `[heights.tiles]` | key `"0x000"`..`"0x3FF"`, value 0..=64 | Tile number (the tilemap entry's 10-bit character number; hex, `0x` optional) to height in eighths of a tile. Tiles not listed are flat ground. |

The loader rejects a bad value naming its key (`[heights].tiles."0x400"`),
and a tile listed twice under two spellings.

**How it is used.** With a matched profile in Game-Aware mode, the Diorama
row is available for a profile that has `[heights]` even though it has no
decoder (a profile that does have a decoded level keeps using that). Every
frame the app reads the chosen layer's tilemap for the screen currently
showing, offset by the layer's live scroll so the walls stay locked to the
picture, looks each cell up in the table (a cell > 0 is a wall of that
height, anything else is ground), and renders the diorama with the current
frame as the ground and wall-top texture and the sprites as upright
billboards. Presentation only, opt-in: Accuracy output is untouched.

**Limits, stated.** SNES only for now (the live NES nametable path has no
scroll snapshot); the scroll used is the one the layer holds at the end of
the frame, so a game that changes scroll mid-frame with HDMA (parallax,
wavy water) is walled as if the last value held for the whole screen;
Mode 7 has no tilemap and gets no grid; the tilemap entry's flip, palette
and priority bits are ignored.

**The height painter (W30-06 part B).** SNES games only. Press **Esc,
Enhancements, Paint tile heights...** (or **Paint tile heights...** in the
Profile Builder panel). The game pauses on its current picture with a grid
at the profile's `tile_px` over it, aligned to the chosen BG layer's scroll
by the same grid math the walls use. Click a tile to give it the brush
height (0..=64 eighths; 0/8/16/32 quick buttons), right-click or Alt-click to
clear it, **Pick** (or Ctrl/Cmd-click) to copy a tile's height into the
brush, **Undo** for the last 20 paint actions. A height belongs to the tile
*number*, so painting one cell paints every cell that uses it. **Save
heights** writes `[heights]` into the player's own copy of the profile,
`<config>/retroforge/profiles/snes/<title-slug>/profile.toml` (created from
the matched profile if that was a shipped one, which is never modified; the
user folder is searched first). Closing the painter drops unsaved painting.
Nothing is read from game code: heights are only what a person clicked.

**HD-2D preset.** In Game-Aware mode, Esc > Enhancements has an **HD-2D
look** switch that sets, together: lens bloom 35%, tilt-shift 50%, soft
shadows 50%, grading off, weather clear, Diorama walls on, camera tilt 40
degrees, zoom 160%, follow the player. It is available exactly when the
Diorama row is (a decoded level, or a height table); otherwise it says so.
It only changes settings, and turning it off restores the previous values.

### `room_grid`'s collision field (ticket W16-05)

`room_grid` (`rf_enhance::decode::room_grid`, W9-08) is the second decoder
family — a 2-D grid of fixed-size rooms, with an optional room-number
indirection table so one room can be reused in many cells. It reads the
same `[decode].collision = { table, bits }` row `metatile_screens` does
(`CollisionSpec` hangs off `[decode]` itself, not off either family's own
sub-table, so no schema change was needed to add this):

```toml
[decode]
kind = "room_grid"
collision = { table = 0x2600, bits = "solid,platform,hazard" }

[decode.room_grid]
rooms_across = 4
rooms_down = 3
room_width = 4
room_height = 4
indexed = true
```

The two families read that same table differently, because they store
tiles differently:

- `metatile_screens` indexes it by **metatile id**, through its own
  metatile-definition table — `DecodedLevel.collision` has one byte per
  *defined metatile*.
- `room_grid` has no separate tile-definition table: the bytes stored in
  ROM ARE the tiles. So `collision.table` is indexed directly by **raw
  tile byte value**, and `DecodedRooms.collision` is sized to
  `max(tile value used) + 1` — the same "derive the table's real length
  from what the level actually uses, don't assume 256" discipline
  `metatile_screens::metatile_table_len` already applies, applied here
  because `room_grid` has nothing else to size it against.

A profile that omits `[decode].collision` decodes exactly as before —
`DecodedRooms.collision` is `None`. Two profiles cite it as a sourced
fact for `room_grid` specifically: `profiles/nes/rf-scroller-demo` (the
`metatile_screens` fixture used as the fallback demonstration per this
ticket's own instructions, since neither in-repo `room_grid` fixture
document — `fixtures/nes/rf-rooms/FORMAT.md`,
`fixtures/snes/rf-rooms-flat/FORMAT.md` — describes collision bytes, and
those fixture docs sit outside `rf-enhance`/`rf-profiles`/`profiles`'s
write_scope) and `profiles/nes/legend-of-zelda` (a documented-commercial
layout, profile-only, no ROM bytes — see that file's header comment for
exactly which Data Crystal RAM-map facts it can and cannot responsibly
turn into `[decode]` rows).

### The solidity mask (ticket W16-05)

`rf_enhance::scene_graph::solidity_mask(tiles, collision_table, bits,
solid_bit_name)` turns a decoded screen's tiles plus its collision table
into a per-tile `0`/`1` mask, aligned 1:1 with `tiles` — the "which tiles
are solid" fact `ENHANCEMENT_WAVE_16.md` §5's Geometry layer (W16-06)
extrudes into blocks. It works for either family's collision shape (both
are "one attribute byte per id"; only the id space — metatile id vs. raw
tile byte — differs, and the caller already resolved that when it built
`tiles`).

It is a free function in `rf-enhance` rather than a new field on
`SceneLayer::DecodedLevel` in this ticket: that enum variant already has
two production call sites (`crates/retroforge/src/enhanced_view.rs`)
outside W16-05's write_scope, spelling out every field explicitly, so
adding a field there without editing those call sites does not compile.
W16-06 depends_on W16-05 and its own write_scope includes
`crates/retroforge/**`/`crates/rf-renderer/**` — wiring the mask into that
field, and NON_GOALS #11's "visited/decoded screens only" restriction on
*which* screens ever get one, belongs there.

<!-- ANCHOR_END: schema -->

### Rebuild recipes (ticket W36-05)

A **recipe** is a second data file beside a profile,
`profiles/<console>/<game>/recipe.toml`, for the Rebuild lab
(`docs/design/REBUILD_LAB.md` §3): facts about a game's *functions* and the
meaning of RAM, used to attach hooks by name instead of by address. Like a
profile it holds facts only: no ROM bytes, no disassembly (law 5, D-021).
Every row carries a `source` note saying how it was observed. The data types
and the matcher are in `rf-rebuild` (`recipe`), the TOML reader/writer is
`rf_profiles::recipe` (`load_recipe_str`, `load_recipe_file`,
`recipe_to_toml`).

```toml
[recipe]
format = 1                    # this build reads 1 (see "Format v1" below for older files)
id = "super-mario-bros"
engine = "smb1"               # a label for the engine family, not logic

[[function]]
name = "nmi"                  # hooks register by this name
shape_hash = "0x3f3d315a776a77dd"   # required, quoted hex (u64 does not fit a TOML integer)
exact_hash = "0xf25cac5c834aa9ee"   # optional; raises match confidence
address_hint = 0x8082         # optional CPU entry on the observed dump; breaks ties
entry = "nmi"                 # optional: nmi | irq | reset | brk | call | indirect
insn_count = 41               # optional: a count (not bytes); bounds the unmatched-reason scan
mode = 3                      # optional, 65816 only: decode mode the function is entered in (0..=4)
hooks = ["entry", "exit"]     # sides a hook may attach to (default: both)
grade = "observed"            # observed | candidate | confirmed
source = "observed: the atlas function the NMI vector targets in a recording"

[[ram]]
addr = 0x075a
width = 1
name = "lives"
grade = "confirmed"
source = "profiles/nes/super-mario-bros/profile.toml [[memory_map]] lives"

[[patch]]                     # W36-06: new byte for one place in a listed function
name = "spawn-lead"
mod = "live-enemies"
function = "tick"             # a [[function]] row with an exact_hash
offset = 41
value = 0xf0
grade = "confirmed"           # a candidate patch is never applied
source = "observed: ..."

[[object_table]]              # field f of slot i is at base + f + i * stride
name = "enemies"
base = 0x000f
slots = 5
stride = 1                    # a record per slot: the record size; a column per field: 1
x_lo = 0x3f2                  # optional field offsets: x_lo x_hi y y_hi type state
state = 0
grade = "observed"
source = "observed: ..."

[[tile]]                      # tile ids first..=last behave as `class`
class = "solid"               # solid | one_way | water | hazard
first = 0x10
last = 0x1f
layer = 1                     # optional; so is tileset = "name"
grade = "candidate"
source = "ai:..."
```

**Matching.** A row matches an atlas function by `shape_hash`: the FNV-1a hash
of the function's instruction bytes with every *absolute-address* operand
zeroed (`rf_rebuild::Isa::masked_operand`; for the 6502 that is every 3-byte
instruction, i.e. absolute, absolute indexed, indirect, `JSR`, `JMP`). Zero
page, immediate and relative operands and all opcode bytes are kept, so a
function that was moved or re-linked still matches and one whose code changed
does not. If the row has an `exact_hash` and some candidates equal it, those
win and the match is `Exact`; otherwise it is `Shape`. Among several
candidates the entry nearest `address_hint` wins (no hint: the lowest entry).
A row with no candidate is **unmatched**: its hooks are reported inactive and
the game runs on the generic rebuild and the interpreter. A hook never
guesses.

**Entry kinds.** The atlas records *how* each function was entered
(`rf_rebuild::entry`): reset, NMI, IRQ or `BRK` vector target, call target,
indirect target. A row with `entry` only matches functions entered that way.
That is what tells twin stubs apart (an `nmi` and an `irq` handler with
identical bytes): two rows with one shape, a compatible `mode` and the same
(or no) `entry` are *twins* and can only be trusted at their own
`address_hint`; rows whose `entry` kinds differ are not twins, so a hack that
moved the stubs still binds `nmi` to whatever its NMI vector reaches. If the
vector reaches nothing of that shape the row is unmatched. The understand
pass writes `entry` for a function reached by exactly one interrupt kind.

**Grades.** Every row says how far to trust it:

| grade | meaning |
|---|---|
| `observed` | measured by RetroForge tooling on a real run (rule-derived names, RAM seen changing) |
| `candidate` | proposed by a finder or an AI (`ai:` sources) and not verified |
| `confirmed` | verified: a lockstep test passed, or a person checked it (patch rows verified by lockstep, RAM profile.toml marks verified) |

The candidate rule: **a candidate row never drives a patch and never drives a
hook on a host that can write memory.** `resolve_patches` refuses a mod whose
patch row, or whose target function row, is a candidate
(`PatchRefusal::Candidate`); `EmuStepper::resolve_hooks` resolves a candidate
row to nothing when the hook host holds `write_memory`
(`MatchReport::function_of_writable`). Candidates still match, label and drive
read-only hooks. Promote a row by editing its grade once it is verified.

**Format v1 (ticket W38-00).** `format = 1` now means the schema above:
`grade` on every row, `entry` and `insn_count` on function rows, and the
`[[object_table]]` and `[[tile]]` fact tables. Files written before it use the
same `format = 1` but have no `grade` keys; the loader accepts them and gives
each such row `observed` (`candidate` for an `ai:` source), and
`rf_profiles::recipe_to_toml` writes the grade out. `load_recipe_noted`
returns how many rows were defaulted (0 = already v1). Unknown keys stay load
errors. No bytes of the game appear anywhere (law 5, D-021).

**Data regions (ticket W38-01).** `[[data_region]]` rows say where a game keeps
ROM tables and level data: `name`, `kind` (`pointer_table` | `element_table` |
`stream` | `graphics`), `rom` and `size` (an offset into the image the atlas
indexes: the PRG image on the NES, the header-stripped ROM on the SNES, never
bytes), `stride`, and optionally `part` (`lo` | `hi` | `bank` for a pointer
table split into byte tables), `pointer_bytes`, `index_ram`, `pointer_ram`,
`via` (the pointer table a stream is reached through) and `confidence`.
`rf_rebuild::dataregion` writes them from how the code reads ROM (no hints);
the source starts with `finder:` and the grade defaults to `candidate`. A row
is `confirmed` only when a level-switch experiment proved it (the entry the
pointer is loaded from changes with the level and each level's stream starts
at the entry's target). Extents are what the game was seen reading: a lower
bound. `rf_profiles::recipe::region_rows_block` / `splice_region_block` write
and replace the marked block of finder rows in a hand-commented recipe.

**Unmatched reasons.** For a row with `insn_count`, `rf_rebuild::classify_unmatched`
scans a ROM image (every offset, the row's decode mode, shape hash only) and
says why the row found nothing: `removed` (no code of that shape anywhere),
`not_reached` (it exists in bytes the drive never executed) or `changed` (it
ran, but not as the row's `entry`/`mode` demand). A linear sweep: a function
laid out in pieces, or edited in place, reads as `removed`.

Unknown keys, a bad hash, a hook point other than `entry`/`exit`, a grade or
entry kind outside the lists above, a duplicate function name and a row without
`source` are load errors. Hashes depend on which blocks the recording reached
(a function's extent is what was observed plus what static decode adds), so a
recipe is made from, and matches best against, atlases built from a similar
amount of play.

## 3. Authoring pipeline

1. Play in Research/Debug mode; label addresses in the debugger (watchpoints
   + annotation store), or take facts from DataCrystal wiki tables under
   the **facts-only transcription policy** (CONSTRAINTS §2: individual
   addresses/sizes/values only, fresh prose, never verbatim whole tables —
   DataCrystal is GFDL 1.2).
2. Debugger exports annotations → profile skeleton (`memory_map`/`rom_map`
   pre-filled with sources).
3. Iterate decode rules with the tilemap/level preview panel (live re-decode
   on profile file save).
4. `retroforge-tool profile validate` + golden screenshot test → commit under
   `/profiles/<console>/<title>/`.

Repo policy: profiles for commercial games contain only addresses/rules
(facts), never ROM-derived assets. Homebrew demo profiles may bundle assets
when the game's license permits.

### Local-only verification recipe (D-009, ticket W11-06)

Law 5 means no ROM is ever committed, so a commercial profile's addresses
cannot be checked by CI. D-009 makes local-only verification against a
developer-held dump count as evidence anyway, **provided the profile
records the normalized hash it was verified against** (so the claim is
checkable by anyone holding the same dump). The recipe W11-06 used, in
case a future author needs to repeat it:

1. Identify the ROM's normalized identity the same way `rf-cart` does:
   `rf_cart::hash::identity_nes`/`identity_snes` over the unzipped image
   (NES strips a 16-byte iNES header if present; SNES strips a 512-byte
   copier header if `len % 8192 == 512`) — never the raw/zipped file's
   hash, and never a hash typed in from memory.
2. Build a small out-of-tree harness against this workspace's `rf-cart`/
   `rf-nes`/`rf-snes` crates (path dependencies; this is *not* a change to
   `crates/rf-harness`, which is out of this ticket's `write_scope`) that
   loads the ROM, drives `EmulatorCore::run_frame` with an `InputFrame`
   (NES) or writes `SnesSystem::bus.joypads.ports` directly before
   stepping (SNES — `run_frame`'s own `InputFrame` parameter is unwired
   for this core as of W11-06; see `crates/rf-snes/src/core.rs`'s
   `run_frame`), and peeks the documented addresses through
   `NesBus::peek`/`rf_snes::cpu::CpuBus::peek` — the same side-effect-free
   accessors `crates/rf-harness/tests/title_probe.rs` uses.
3. Compare what comes back against the *documented meaning* of the
   address, not an assumed value: Super Mario Bros.'s timer read "400" at
   the start of World 1-1 because Data Crystal documents that as the
   level's starting time; Super Metroid's region-index address read 6
   (Ceres Station) during the game's own opening sequence, matching
   Kejardon's RAM map's area numbering, before the player ever reaches
   Crateria (area 0) — a match to the *specific* documented fact is what
   makes a reading evidence rather than a coincidence.
4. A row that never reaches a checkable state (stays at an ambiguous
   default, or the run never gets far enough into the game to exercise
   it) is dropped from the profile rather than shipped as an assumption —
   `profiles/nes/metroid`'s header comment records three addresses cut
   this way (health, current-area, room-index) alongside the three that
   verified cleanly.
5. Record the normalized hash bundle in `[[identity]]` and write exactly
   what was read, at what point in booting the game, and how it matches
   the cited documentation, in the profile's header comment — the
   citation is what turns "I ran a debugger once" into a claim a second
   person holding the same dump can check.

## 4. Loading and precedence

Match at cart load by normalized hash → load base profile → apply user
overrides (`~/.retroforge/profiles.d/`) → apply per-session toggles. All
layers visible in the profile inspector panel. Enhanced save states record
the active profile revision + enabled mods (see SAVE_STATES.md).
