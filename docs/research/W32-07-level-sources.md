# W32-07 prep: where can full levels come from? (research, no repo edits)

> Research notes (2026-10-16). Game RAM/ROM facts below are leads from public docs or marked background knowledge; none is used by code until verified by observation (camera census / RAM diff). wide-snes was read under an owner-granted exception (no license: ideas only, no code copied).


Scope: ultrawide margins that show the REAL level. Inputs: wide-snes (owner-granted
exception, read incl. .asm, paraphrased only), format wikis, level-editor docs.

## 0. Method and confidence

- wide-snes: read `super-mario-world/asm/*.asm` via `gh api` (no license; everything below is
  my own wording, no code copied). It is a patch against SMW, not documentation, so it is
  used only for "what assumption broke and how was it fixed".
- Wikis actually fetched this session (plain documentation):
  - Data Crystal SMB RAM map, Metroid RAM map, Mega Man 2 RAM map, SMB3 RAM map, Super Metroid RAM map (thin).
  - TASVideos Mega Man X RAM map.
  - nesdev wiki "Level compression" (http://www.nesdev.org/wiki/Level_compression).
- Fetch failures: SMW Central (copyright-flag page), Metroid Construction (403), Data Crystal
  Castlevania/ALttP (404). Where a row says "(bg)" the claim is from my background knowledge of
  public docs and is UNVERIFIED this session. RetroForge's rule applies: every address below is a
  documentation claim to verify with `camera_finder` / a census, never trust it blind.
- Data Crystal is often a stub for the SNES titles; the SNES rows lean on (bg) more.
- Existing repo state noted: profiles for SMB, SMB3, Metroid, Zelda, SMW, Super Metroid, MMX
  already declare `[camera]` (found by observation), none declares `[decode]` for the flagships
  except where noted; DKC has no camera yet.

## 1. wide-snes lessons (SMW widescreen/extrawide, 352/384 px, up to 512 universal)

Core design: the patch keeps the game's 256-wide logical world and its 512-px tilemap ring, and
adds `extra_columns` (48 or 64 px) per side to EVERY range test. The renderer (bsnes-hd) shows
the wider picture; the game code only needed its "is this on screen" tests to know the screen is
wider. (Hard ceiling: the SNES BG tilemap ring is 512 px, so 512 is the universal limit.)

| Subsystem (file) | Original assumption | Fix class |
|---|---|---|
| Camera init (`level_render`: init_scroll, x_scroll_fix, horz_customizer, l2_vertx_h) | Camera starts at 0 and may sit at edges; a level of 1 screen is one 256 window; some levels pin the camera at fixed values | Bias initial/cached camera by +margin so the player stays centred; per-level special cases for pinned-camera rooms (boss rooms, Yoshi's House, bonus). Camera clamp ("scroll lock" flags) disabled where the room is one screen wide |
| Force whole-screen render (`level_render`) | Level loader draws only the 256 visible + a bit; tiny 1-screen levels never fill the ring | Force a full 512 draw on loads where level width = 1 screen (odd camera position trick) |
| Kill-if-too-far-from-camera, side exits (`level_render`, `level_side_exits`) | Player off-screen test is hard-coded to 0..0xF0 | Widen thresholds by margin both sides |
| Entrances (`level_entrances`) | Start position expressed relative to a 1-screen origin | Start on screen 1 not 0 so there is room to the left |
| Sprite spawn window (`level_sprites`: horz_range) | Sprites spawn/despawn from a range around camera (about -0x30 .. +0x120) | Table values extended by margin; separate table kept for vertical levels so vertical spawn range is unchanged |
| Sprite offscreen tables (SUB_OFF_SCREEN, 3 copies in different banks) | Per-sprite-type "erase when this far past screen" distances (8 entries x lo/hi) | Add margin to positive entries, subtract from negative (assert no overflow) |
| Sprite "is it drawn" flags (get_draw_info x3, test_offscreen) | One 1-bit flag meaning "x relative to camera fits 9 bits", sprites outside never drawn | Replace by a wider range test plus a second "wide" flag (OAM x has only 9 bits; sprites beyond -256..255 relative cannot be drawn at all) |
| Sprite wings, Yoshi tongue/throat, hair, etc. (`level_sprites`, `level_yoshi`, `iggy_hair`) | Child tiles computed in 8-bit relative X | Recompute 16-bit relative X, set OAM bit 9 correctly |
| Proximity checks (`proximity_wrap_fix`) | "Is player near" uses 8-bit signed difference: far sprites WRAP into range (thwomp, falling spike, chucks, rip van fish, piranha, swooper, moles, yoshi egg...) | Replace with 16-bit signed compare. Invisible at 256 px because off-screen sprites were frozen anyway; with a wider view you see them wake up |
| Shooters/generators (`level_shooters`, `level_generators`) | Spawn (bullet bill, torpedo ted, eeries, dolphins, bowser fire, multi-bullet) positioned at screen edge +/- constants, with high byte derived from camera hi | Extend edge constants and high-byte table by margin; widen "is shooter on screen" test |
| Extended/minor-extended/cluster/bounce/score/quake/spinning-coin sprites | Own small 8-bit-position tables, drawn if `x < 0x100` | Add 9th position bit/high byte, widen draw test; some (smoke) gained a hi byte via third-party patch |
| Cluster sprites (ghost boos, sumo flame, boo ring/ceiling) | Spawn at a hard X inside the 256 window | Spawn across the wider window; some flagged "ultrawide: later" |
| Bosses (`level_bosses`) | Arena = one screen; lava stream, pipes, Bowser platform and balls positioned/clamped in 0..255; level interaction beyond 256 filled with solid | Add extra OAM for arena width, extend position lists, make the margin solid or non-interactive; special-case camera |
| Windowing/HDMA (`windowing_effects`) | Circle/keyhole/message-box windows use 8-bit window edge registers 0..255, centred at 128 | Recompute centre/width in a 512-wide space, then scale to the 8-bit regs; clamp 0..0x1FF; bsnes-hd consumes the wide form |
| Overworld (`overworld`) | Scroll range 0..0x101; clouds spawn near the visible area; player sprite drawn only if on screen | New scroll bounds, spawn earlier, set OAM bit 9 for map sprites |
| Title screen (`title_screen`) | Demo input recording ties timings to the walk distance | Recorded input retimed per aspect; level used by demo edited |
| Credits, Luigi, goal/score fix | Cutscene sprites and score slots placed in 256 | Offset/shift, add range |
| `windex` | Overworld sprites linger in title load | Clear them (hygiene bug exposed by wide view) |
| Generic | OAM budget 128 sprites: wider view -> more on-screen sprites -> flicker; "maxtile" budget helper | Priority/budget routine |

Takeaways for us:
1. Fixing a game costs hundreds of hand patches. We do NOT patch game code; we only show margins.
   So our concern shifts: which pieces of the world can we SEE beyond the camera without the
   game having prepared them? Answer: only what is in RAM/ROM tables, or what the game will draw
   if forced to (scout).
2. The patch list is exactly a catalogue of "things the game does NOT do for off-screen
   positions": spawn, update, collide, wake-up. So in the margins, enemies must come from
   the LIVE game only (they may be asleep/frozen/unspawned in game state; showing a sprite there
   would be a lie). Matches the brief's instinct.
3. Camera-vs-player coupling and pinned camera rooms appear in every game (boss rooms, bonus).

### Screen-width assumption checklist (generic "finder" input)

For any side-scroller, what a game may assume is 256 px (or 16 tiles) wide:
1. **Column streaming distance**: new level columns are decoded/streamed when the camera crosses a
   threshold; the ring buffer (NES 2 nametables = 512 px; SNES BG ring 512 px) only holds screen + one
   page. Beyond that, nothing exists in VRAM until streamed.
2. **Streaming trigger granularity**: one column per 8/16 px of camera DELTA, not of position.
   Teleporting the camera skips columns (test: columns appear blank).
3. **Streaming direction**: some engines stream only forward (SMB cannot scroll back), others both.
4. **Sprite/object spawn window**: spawn and despawn edges relative to camera (hard constants
   such as -0x30..+0x120); enemies pre-listed per screen are consumed once.
5. **Off-screen culling/freeze**: sprites outside the window are frozen/erased.
6. **Draw-visible tests**: "x < 256" with 8/9-bit OAM x; 16-bit sprites in a 9-bit field.
7. **8-bit proximity / wrap compares**: relative distances stored in 1 byte (wrap bugs).
8. **Camera clamps**: min/max camera per level or per room; scroll-lock flags; boss rooms.
9. **Camera derived from player each frame**: dead zone, lag, lookahead; separate
   "previous camera" copies used for delta streaming.
10. **Room/screen transition thresholds**: door, edge, side-exit tests at 0/0xF0/0xFF.
11. **HDMA / window / split tables**: 256 entries, centre 128, status-bar split lines, sprite-0-hit or
    IRQ split positions that assume the screen layout.
12. **Wrap-around playfield** (Kid Icarus, Mode 7 maps): world wraps modulo screen/map width, so
    margins show the opposite edge, not "more level".
13. **Fixed-width level**: level width = N screens, no data beyond; left/right of end is void
    (margins should show the end wall/background, not garbage).
14. **Layer 2/parallax**: BG2 scrolls at a ratio; margins need it extended consistently.
15. **Cutscenes/title/overworld/credits**: separate scroll systems; each its own bound.
16. **Mid-frame effects**: scanline IRQ (MMC3), sprite-0 splits, HDMA scroll per-line that make the
    "scroll register" differ per scanline (status bar vs playfield).
17. **Object counts**: OAM 64 (NES), 128 (SNES) -> flicker when more is on screen.
18. **Vertical levels**: all of the above transposed; horizontal margins in a vertical level are
    usually empty/wall.

## 2. Flagship games

Key to the "recommended" column: S = scout, D = ROM decoder (`[decode]` family), B = RAM level
buffer read, T = stitch-only (what exists today). Confidence: "doc" = fetched page said so,
"(bg)" = background knowledge, verify.

### 2a. NES

| Game | Scroll style | Storage (public docs) | Whole level in RAM? | Camera/scroll RAM (docs claim) | Scout suitability | Rec. |
|---|---|---|---|---|---|---|
| Super Mario Bros. 1 | Horizontal, forward-only (no scroll left) | Object list, 2 bytes/obj, plus separate enemy list; nesdev lists SMB as object-list + column based. Data Crystal RAM map [datacrystal.tcrf.net/wiki/Super_Mario_Bros./RAM_map] | No. Only ~2 pages of tile data at $0500-$069F (doc) | doc: $071A current screen, $071B next screen, $071C screen edge X, $0723 scroll lock, $0775 scroll amount; $006D player page. Page does not name a ScreenLeft_X; the repo profile found one by observation | Right side: OK if the player/camera can be advanced (camera follows player past mid screen, scroll amount tied to speed). Left of the live moment: impossible, the game never scrolls left, the stream is forward-only. Enemies spawn per page from list as it advances | D for the whole level (object format is widely documented, needs area/level pointer tables) + S for right-of-camera when D missing; stitch for history |
| Super Mario Bros. 3 | Horizontal or vertical per level, free of forward-only (can scroll left within level) | Object list 3 bytes/obj, plus enemy list; layout decoded when the level loads | YES per doc: Data Crystal SMB3 RAM map says "Active block buffer / tile map" $6000-$794F, 0x1B0 bytes per screen-block, ~15 screens [datacrystal.tcrf.net/wiki/Super_Mario_Bros._3/RAM_map] | doc: no explicit scroll address on that page; repo found it by observation (matches scroll reg 100%). Object set $070A, enemy data $7B40+ | Good scout target, camera follows player; and since the full buffer is in RAM, scout is unneeded | B (read tile buffer + tileset gfx), with the scout as cross-check. Best "oracle" game |
| Legend of Zelda | Screen-flip: overworld 16x8 screens, dungeons 8x8 grid of rooms; no free scroll | Per-screen tile "columns" (nesdev lists Zelda as metatile columns) and room descriptors | Only current screen in RAM/VRAM | n/a: scroll registers move only during the flip animation | Not applicable to the margin: neighbour rooms are separate maps, not an extension of the current one. Scout can flip rooms (write room id) and capture, i.e. "room scout" | D via `room_grid` (family exists; profile notes the table addresses were not plainly documented) ; neighbours via ROM. Margins would show the adjacent screen, or letterbox |
| Zelda II | Horizontal side-scroll in towns/palaces/caves (dozens of screens wide), overworld = separate free map | Map data per area, object/run-length encoded (bg) | Likely decoded to a RAM buffer on entry (bg, verify) | (bg) unknown | Side areas: scout plausible (camera follows player, can scroll back?) unknown; overworld: tile map decode | S for side areas (needs test); D/T for overworld. Low priority |
| Metroid | Free 2-axis but one axis per room, nametable-switch rooms; vertical shafts mirror horizontally | Rooms = 32x30 tile "screens" built from structures + "combos" (metatiles) and compressed (doc: $33-$3F pointers for room data, structures, combos) | One room at a time: RAM $6000 and $6400 hold the two screens' layout (doc) | doc: $FC scroll Y, $FD scroll X, $49 scroll direction, $50/$51 map x/screen x. Repo found $FD | Camera locked by room boundary: room is 1-2 screens; scout can scroll through room but not beyond (door transition loads next room) | B for the current room (the 2 screens at $6000/$6400) + D for neighbouring rooms if doors are decoded (huge scope). For ultrawide: margins at room edges show a wall/door of this room; probably T |
| Mega Man 2 | Horizontal AND vertical sections per stage, screen-by-screen camera with locks at doors/bosses | Per stage: screen map -> 32x32 meta-metatiles -> 16x16 metatiles (nesdev Level compression mentions Mega Man metametatile scheme) | No whole level; streams by screen | doc: camera state $1B (0 default, 1 nametable changing, 2 frozen, $80 vertical), camera X $1F, X screen $20, camera Y $22 | Scout feasible horizontal sections; locks/doors, vertical sections, boss rooms are walls. Camera-state byte may need reset | D (screen tables, `room_grid`/`metatile_screens` shape) preferred, S as fallback/validator |
| Castlevania | Horizontal screen scroll, with "blocks" (screen-sized) in stages; some stairs/vertical | Stage = sequence of blocks (bg), tile maps per block | Streams per block | (bg) block number, scroll X in low RAM, unverified | Scout plausible: camera follows Simon with left/right limits; block boundaries, doors (the stage transition door) lock camera | D (if block tables known) else S |
| Kid Icarus | Vertical mostly (stages 1-1, 1-2 etc.), horizontal fortress/ chamber rooms; the playfield WRAPS horizontally | Screen/ metatile rows (bg) | Streams rows | (bg) | Wrap makes horizontal margins ambiguous. Vertical margins not a concern for width. | T; margins = wrap or wall. Skip |
| Ninja Gaiden | Horizontal (some vertical), camera locks at boss/transition | Stage divided in "sections" of screens, metatile maps (bg) | Streams per screen | (bg) | Scout plausible; camera clamps at section ends. Cutscenes between stages | D or S (needs verify) |
| Contra | Mixed: horizontal stages (1,5,6,7), vertical (3 waterfall), pseudo-3D base rooms (2,4: screen-flip). Horizontal stages are forward-only (no scroll back) | Screens of super-tiles (bg) | Streams per screen | (bg) | Horizontal: forward-only like SMB, plus scroll lock until area cleared; base rooms: no | T + D only for stage 1,5,6,7; skip base |

### 2b. SNES

| Game | Scroll style | Storage | Whole level in RAM? | Camera RAM (docs claim) | Scout suitability | Rec. |
|---|---|---|---|---|---|---|
| Super Mario World | Horizontal OR vertical per level, with layer-2 backgrounds, camera follows player with lag/lookahead and lock flags | Object lists (3 bytes/obj, "level layout") decoded into Map16 (bg: low bytes at $7EC800, high at $7FC800, 16x27 per screen, up to 32 screens) ; SMWC RAM/ROM map pages (fetch blocked this session). wide-snes also tells us: per-level special cases for pinned camera (bonus, boss, Yoshi's House) | YES (bg). The whole level's Map16 is expanded in RAM at load | $1A/$1C layer 1 X/Y, $1E/$20 layer 2, $1462/$1464 next-frame layer1 positions (bg, the repo profile uses SMW Central rows for $1A); scroll-enable flags $1411/$1412 (bg, and wide-snes pokes $1411 itself) ; $5E screens in level (wide-snes references it) | Camera can be forced but game keeps a `next` camera copy and a scroll routine that re-centres; level only decodes Map16 so no streaming dependency. Perfect oracle | B (Map16 buffer + Map16->8x8 via VRAM/tileset data) ; S as second path / validation |
| ALttP | Overworld: free 8-way inside 512x512 areas (area transitions scroll); dungeons: rooms of 512x512 with camera quadrants and room-to-room transitions; houses/caves = single rooms | Compressed room object lists (dungeon) and Map32/Map16 (overworld); tilemap buffers in WRAM (bg: $7E2000-ish) | The current room/area is expanded into a tilemap buffer (bg) | (bg) scroll shadow regs in the $E0-$E9 range, camera $E1-$E8 | Camera clamps to the room quadrant; transitions are scripted. Scout can cover one room/area. Neighbouring rooms not | B for current room/area (verify tilemap buffer), margins beyond = T |
| Super Metroid | Rooms of N x M screens (each 256x256), camera clamped by per-screen scroll types (red/blue/green), door transitions load next room | Level data compressed per room, decompressed at load; layout = 16x16 blocks + BTS bytes (bg: $7F0002, size in blocks in header; stated in the repo profile's description) | YES (bg). Entire room | doc (Data Crystal, thin): Samus X 7E0AF6, Y 7E0AFA, map X/Y in room 7E0B12/16. (bg) layer 1 X/Y at $0911/$0915. repo profile has [camera] | Scout limited by scroll regions: camera not allowed into red scroll areas. Not needed since the buffer is complete | B (whole-room block grid; neighbour rooms via ROM decode optional). Gold standard for margins |
| Donkey Kong Country | Horizontal/vertical, camera leads the player, bonus rooms, mine carts, water | Compressed level maps/tiles (nothing definitive found: search returned only the GBC port notes) | Unknown. Treat as streamed | Not documented here; repo profile has no camera yet | Camera follows player with look-ahead and per-level bounds; scout via player teleport maybe. Unknown | S after camera found; otherwise T |
| Mega Man X | Horizontal and vertical, screen locks at boss doors and in "zones" | Tile blocks/maps per stage (bg), no doc found | Streams | doc (TASVideos): camera X $7E:00B4 (2 bytes), Y $7E:00B6, level ID $7E:1F7A (00 intro .. 0C Sigma 4). Repo found camera 97% | Scout suitable: documented camera, zone locks to handle (write past lock = re-clamped?). Game keeps per-zone scroll limits | S (first SNES scout), D later |
| Super Castlevania IV | Horizontal with some vertical, rotating rooms (Mode 7), camera tied to Simon | Compressed level data (bg) | Streams | (bg) | Mode 7 rooms break any scout; plain stages OK | T now; S later |
| Contra III | Side-scrolling stages + Mode 7 top-down stages | (bg) | Streams | (bg) | Camera forced forward; locks at bosses. Mode 7 stage: whole map in VRAM (see below) | T; Mode 7 stages: VRAM read |
| Yoshi's Island | Free 8-way, Super FX renders BGs (and polygons) | Object lists decoded into Map16-like (bg); level size per screen | Likely a decompressed map (bg, verify) | (bg) | FX games: BG3 drawn by GSU; scroll through stitching only; free camera with per-room limits | T; S experimental. Low priority |
| Kirby Super Star | Mostly horizontal rooms joined by doors; "Great Cave Offensive" is large rooms | (bg) | Per room | (bg) | Rooms + door transitions; fits "room" model | T/D per room. Low priority |
| Chrono Trigger overworld | Mode 7 map (flat in 1000AD/2300AD maps, perspective for Epoch) | Whole Mode 7 map resident in VRAM (Mode 7 tilemap lives in VRAM; this is a hardware fact) | YES, in VRAM | Mode 7 scroll regs ($210D/$210E for M7 H/V) | Scout unnecessary; read the tilemap from VRAM and re-render with the M7 matrix | B (VRAM) — but "ultrawide" for Mode 7 is a render problem, not level source |

Cross-cutting observation: in this table, "whole level already in RAM at load" is true for SMB3,
SMW, Super Metroid (rooms), ALttP (area/room). For those the scout is just a validator; read
the buffer. Scout earns its keep on the streamed games (Mega Man family, Castlevania, Ninja Gaiden,
DKC, MMX).

## 3. Scout feasibility risks and mitigations

Definition: a second `Core` restored from a save state of the live moment; write camera /
player; run frames; harvest.

| # | Risk | Why | Mitigation |
|---|---|---|---|
| 1 | Camera snaps back to player every frame | Camera is derived (SMW `next` copy + scroll routine; SMB scroll amount from speed) | Per-game recipe: (a) write camera AND the derived/shadow copies (b) write the player X so the game's own follow logic moves the camera (c) hold input (Right/Left) with invincibility. Detect failure: camera RAM after the frame != what was written and not progressing -> mark recipe failed, fall back to stitch |
| 2 | Writes at wrong point in the frame | Game reads camera in logic, then DMA during vblank | Write at the start of the frame (after vblank) before the game's logic; verify next frame the value kept |
| 3 | Column streaming skips if camera jumps | Streaming triggered by delta per 8/16 px (assumption #2) | Step <= 8 px (NES) / per game max; or per-game "max_step". Settle frames after each step. Assert the stream completed: nametable / tilemap diff between frames |
| 4 | Forward-only engines (SMB, Contra horizontal) | Assumption #3 | Scout only in the supported direction; left margin from decoder (SMB) or from stitch history. Record `scroll_dirs` in the profile |
| 5 | Stream direction asymmetry: game streams ahead only in the direction of motion, leaving the opposite side blank | | Capture the *tilemap rows/columns that got written* in the direction of motion, merge across passes; verify by double-pass |
| 6 | Sprites spawn/despawn while scout moves | Spawn window follows camera (wide-snes `horz_range` etc.) | Never take sprites from the scout. Capture from the scout with OBJ layer masked (NES PPUMASK bit 4 / SNES TM-TS bits; or the emulator's layer mask, preferred, game cannot undo). Live game supplies sprites. Optionally zero the sprite tables in the scout so nothing hits Mario |
| 7 | Scout player dies / gets hit / triggers a pipe, door, boss lock | Walking an actual path | Invincibility RAM poke; or write player to safe position each frame; abort when level id RAM (`level_id`) changes; frame budget |
| 8 | Camera clamp at room end (boss door, scroll lock) | Assumption #8 | Profile gives lock flag address and clear value, or bound = end of level; mark as `unreachable`. Margins show the wall |
| 9 | HUD and status bar contaminate frame captures | HUD baked into pixels | Prefer tilemap capture over framebuffer; otherwise mask rows by profile (`hud_rows`) |
| 10 | Split-screen: the scroll register differs per scanline (status bar) or IRQ splits | MMC3 IRQ, sprite-0 hit | Capture by playfield scanline range, take the scroll at that range (camera_finder already scores the low byte against the register) |
| 11 | RNG/state divergence | Throwaway copy | Not a problem. The scout must not share mutable state with live (no shared SRAM/files/audio). Mute audio, disable rewind/run-ahead hooks |
| 12 | Mapper bank switching / CHR-RAM | Margins use tiles of a CHR bank that is not mapped now, or CHR-RAM content is loaded per area | Capture tile IDs AND the CHR bank state used; render margins with the bank current at the capture; per-area tilesets: segment the capture whenever the bank/CHR hash changes. For CHR-RAM games the game uploads graphics at area entry (not scroll), so the live moment's CHR is right for that area. Flag segment boundaries so we do not stitch across tilesets |
| 13 | Animated tiles (water, coins) | CHR animation frame differs | Pick the capture frame by modulus; or normalise animation frame; tolerate seams |
| 14 | SNES DMA/VRAM timing | Streaming uses an NMI DMA queue; writes lost if queue overflows | Slow steps, wait for queue empty (profile: settle frames), assert VRAM tilemap == WRAM Map16 cell for oracle games in tests |
| 15 | Parallax BG2/BG3 and Mode 7 | Layers scroll at different rates | Capture per layer with each layer's own scroll; stitch layer by layer (the stitcher already knows layers) |
| 16 | Cost | Each scout frame is a full emulation frame plus capture | NES unthrottled is easily hundreds of fps, SNES tens-to-low hundreds on M2 Max (estimate, MEASURE). Level of 30 screens at 8 px steps + 4 settle frames = about 30*32*5 = 4800 frames (~10-40 s of CPU). Budget: run on a background thread, 1 core, cancel on level change; progressive reveal (margin fills as discovered); cap total frames and time; cache result keyed by (rom hash, level id) |
| 17 | Wrap-around playfields | Assumption #12 | `wrap = true` in profile; margins mirror the other edge |
| 18 | Vertical levels | | Horizontal margins in vertical levels: usually walls; skip scout (profile `axis`) |
| 19 | Level not the same after save state restore (e.g., loaded at a checkpoint) | | Scout is from live state, fine; but for forward-only games the area behind the live camera is unknowable by scout (SMB) |
| 20 | Detecting "I drew everything" | | Coverage = all columns [0, level_width) captured with a matching tilemap; level width from profile or from first lock/end-of-level condition (camera stops while player moves) |

Two cheaper alternatives to compare when designing the scout:
- **Tilemap harvest, not pixels:** drive camera, read BG nametable/tilemap VRAM (+ attributes + scroll
  + CHR state) rather than rendering. No sprites, no HUD, no pixel seams, ~1/3 the cost. Render the margin
  from tiles with current CHR. Needs profile "playfield" layer id.
- **Oracle check:** in games with a whole-level RAM buffer (SMW, SMB3, Super Metroid) run the scout and
  compare to the buffer: automated proof that the recipe is right before relying on it on games with no oracle.

## 4. Recommended implementation order

Principle: prove the scout where an independent oracle exists (so bugs are visible), then on
games with no oracle. Two consoles each.

1. **SMW (SNES)** — oracle: Map16 buffer in RAM; camera documented and used by the profile; camera
   derived copy ($1462) is exactly the "snap back" problem (#1) to solve; wide-snes tells us which
   levels pin the camera (boss, bonus, Yoshi's House) to blacklist. Deliverable: scout recipe with
   `camera_write` + shadow regs; diff vs Map16.
2. **SMB3 (NES)** — oracle: $6000 tile memory buffer (doc'd); proves NES path, MMC3 CHR banking and
   IRQ status-bar split (#10, #12), horizontal and vertical level axes.
3. **Mega Man X (SNES)** — no oracle, but documented camera ($B4/$B6) and found 97%; proves scout on a
   streamed game with zone locks (#8) and parallax BG2.
4. **Mega Man 2 (NES)** — no oracle, but `[decode]` screens can be built from its documented
   tile structure to cross-check; camera $1F/$20/$22 and camera-state $1B documented; proves scout on a
   streamed NES game with scroll locks and vertical sections.
Then (value over cost): Super Metroid (B only), SMB1 (D for left/right + forward scout), Castlevania, ALttP rooms.

### Per-game data a profile would need (proposed `[scout]` table; names provisional)

```
[scout]
mode        = "camera_write" | "player_move" | "input_walk" | "room_flip"
axis        = "x" | "y" | "both"
dirs        = ["right"] | ["left","right"]      # forward-only games
camera      = { x = ref to [camera] rows, shadow = [addresses to also write] }
step_px     = 8                                  # max camera delta per frame
settle      = 3                                  # frames after each step
write_at    = "frame_start"
bounds      = { min = 0, max_from = "level_width_ram"|const|"detect_stall" }
level_id    = address                            # abort when it changes
locks       = [{ addr, clear_value }]            # scroll-lock flags to clear
safe        = [{ addr, value }]                  # invincibility / hide player
playfield   = { layer, hud_rows = [..], split = scanline or irq }
capture     = "tilemap" | "frame"; layers = [..]; mask_obj = true
chr         = "rom_bank" | "ram_stable" | "segmented"
wrap        = false
oracle      = optional ref to a RAM level buffer (for self-test)
abort       = { frames_max, on_level_change = true, on_stall_frames }
```
Also needed per game (outside `[scout]`): level-end detection, whether to scout vertical levels at all,
and the exclusion list (pinned-camera levels, bonus rooms, Mode 7 stages).

## 5. Sources (all plain documentation unless noted)

- wide-snes asm (no license, exception granted; patch code, not documentation): https://github.com/VitorVilela7/wide-snes/tree/master/super-mario-world/asm
- Data Crystal SMB RAM map (documentation): https://datacrystal.tcrf.net/wiki/Super_Mario_Bros./RAM_map
- Data Crystal SMB3 RAM map: https://datacrystal.tcrf.net/wiki/Super_Mario_Bros._3/RAM_map
- Data Crystal Metroid RAM map: https://datacrystal.tcrf.net/wiki/Metroid/RAM_map
- Data Crystal Mega Man 2 RAM map: https://datacrystal.tcrf.net/wiki/Mega_Man_2/RAM_map
- Data Crystal Super Metroid RAM map: https://datacrystal.tcrf.net/wiki/Super_Metroid/RAM_map
- TASVideos Mega Man X RAM map: https://tasvideos.org/GameResources/SNES/MegamanX/RAMMap
- nesdev wiki Level compression: http://www.nesdev.org/wiki/Level_compression
- Not reachable this session (candidates to read next): SMW Central RAM/ROM map (https://www.smwcentral.net/?p=memorymap&game=smw&region=ram), Metroid Construction (SMMM docs), Data Crystal Castlevania/ALttP RAM maps, Data Crystal Mega Man 5 Level Data (same engine family as MM2).
- Honesty note: none of the "(bg)" cells were confirmed; the first scout ticket per game should
  begin with the camera census + a RAM diff, as already done for SMB3/MMX.
