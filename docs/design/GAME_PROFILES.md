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

## 4. Loading and precedence

Match at cart load by normalized hash → load base profile → apply user
overrides (`~/.retroforge/profiles.d/`) → apply per-session toggles. All
layers visible in the profile inspector panel. Enhanced save states record
the active profile revision + enabled mods (see SAVE_STATES.md).
