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
