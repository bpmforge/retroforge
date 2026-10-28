//! **W39-03b: the Zelda stage set from observed walking** (ROM-gated, law 5, D-021).
//!
//! On a scratch core (never the live one) the W38-05b explorer visits the overworld
//! screens by RAM-poking Link to an edge. On the first screens it lands on, each 16x16
//! cell is probed: Link is placed on the cell beside it (test-side write of the recipe's
//! player x/y) and the pad pushed towards it; if his position does not advance the cell
//! is solid. The tile ids shown in that cell (from the PPU's drawn-tile record) get the
//! verdict, which `rf_rebuild::walkability` turns into `[[tile]]` rows that hold on
//! every screen. The rows bake each learned screen (8 px cells, the footprint of the
//! screen's own canvas, `rf_enhance::baked::bake_canvas`), which is cached by ROM hash.
//!
//! Only ids, offsets and counts are printed. The proposed rows go to the scratchpad.
//!
//! ```text
//! cargo test --release -p rf-harness --test w39_03b_zelda_stage -- --ignored --nocapture
//! ```

mod w38_05_zelda_common;

use std::collections::BTreeMap;

use rf_cache::Cache;
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PixelLayer, PpuPixel, ResetKind};
use rf_enhance::baked::{
    bake_canvas, load_stage_set, save_stage_set, Kind, RuleClass, Solidity, StageSet, TileRule,
};
use rf_enhance::screen_margins::{ScreenAtlas, ScreenImage, SCREEN_W, UNKNOWN};
use rf_enhance::stitcher::Canvas;
use rf_nes::core::NesCore;
use rf_nes::CpuBus;
use rf_rebuild::walkability::{rows_toml, Verdict, WalkTally};
use rf_rebuild::Grade;
use w38_05_zelda_common::{load_rom, rom_path, script, DOWN, FILE, LEFT, RIGHT, UP};

const SCREEN_INDEX: u32 = 0x00EB;
const PLAYER_X: u16 = 0x0070;
const PLAYER_Y: u16 = 0x0084;
const SCROLL_X: u32 = 0x00FD;
/// Frame at which the scripted play stands outside the first cave (idle base state).
const BASE_FRAME: u32 = 1650;
/// Picture rows: the playfield starts below the status band (observed in the PNGs).
const FIELD_FROM: usize = 64;
const H: usize = 240;
const OUT_DIR: &str = "/private/tmp/claude-502/-Users-bmatthews-Code/10a3a02b-3b5c-46ad-afc7-f01185980ef7/scratchpad/w39-03b";

/// The frame as indexed colours (0..64), `UNKNOWN` where a sprite is on top.
#[derive(Default)]
struct Cap {
    px: Vec<u8>,
    rgb_hash: u64,
}

impl CoreSink for Cap {
    fn video_scanline(&mut self, y: u16, p: &[PpuPixel]) {
        if self.px.is_empty() {
            self.px = vec![UNKNOWN; SCREEN_W * H];
        }
        let y = usize::from(y);
        if y >= H {
            return;
        }
        for (x, q) in p.iter().take(SCREEN_W).enumerate() {
            self.px[y * SCREEN_W + x] = if q.layer == PixelLayer::Sprite {
                UNKNOWN
            } else {
                q.palette_index & 0x3F
            };
            self.rgb_hash = (self.rgb_hash
                ^ u64::from(q.palette_index)
                ^ u64::from(q.layer == PixelLayer::Sprite)
                ^ (u64::from(q.priority) << 8))
                .wrapping_mul(0x0100_0000_01b3);
        }
    }
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn frame(core: &mut NesCore, pad: u8) -> Cap {
    let mut i = InputFrame::empty();
    i.ports[0] = u16::from(pad);
    let mut cap = Cap::default();
    core.run_frame(&i, &mut cap);
    cap
}

fn ram_print(core: &NesCore) -> u64 {
    core.state_view()
        .wram
        .iter()
        .fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
}

/// A machine replayed to the idle overworld spot after the cave (the scratch machine).
fn scratch(raw: &[u8]) -> NesCore {
    let mut core = NesCore::from_ines_bytes(raw).expect("loads");
    core.reset(ResetKind::Hard);
    core.bus_mut().set_tile_capture(true);
    for f in 0..BASE_FRAME {
        frame(&mut core, script(f));
    }
    core
}

/// Background tile ids per 8 px cell (32 x 30), from the PPU's drawn-tile record of the
/// frame just run; `None` where nothing was drawn.
type Tiles = Vec<Option<u8>>;

fn tile_grid(core: &NesCore) -> Tiles {
    let mut g: Tiles = vec![None; 32 * 30];
    for t in core.bus().completed_tiles() {
        let (Ok(x), Some(y)) = (usize::try_from(t.x), t.y.checked_sub(u16::from(t.fine_y))) else {
            continue;
        };
        let (col, row) = (x / 8, usize::from(y) / 8);
        if col < 32 && row < 30 {
            g[row * 32 + col] = Some(t.tile);
        }
    }
    g
}

/// Playfield: 16 x 11 cells of 16 px below the status band.
const CELLS_W: usize = 16;
const CELLS_H: usize = 11;
const PUSH_FRAMES: usize = 8;
/// Link rests on y = tile top + 5 (observed: his y is 5 mod 8 whenever he stands still).
const LINK_Y_OFF: u32 = 5;

#[derive(Default)]
struct ProbeStats {
    open: u32,
    blocked: u32,
    partial: u32,
    skipped: u32,
    /// The cell whose push began a screen change (a door or stair): to be avoided.
    door: Option<(i32, i32)>,
}

/// What a flood fill found out about one screen's 16 px cells.
type Known = BTreeMap<(i32, i32), bool>;

/// How far Link advanced along the push `pad`.
fn advance(pad: u8, from: (u8, u8), to: (u8, u8)) -> i32 {
    match pad {
        RIGHT => i32::from(to.0) - i32::from(from.0),
        LEFT => i32::from(from.0) - i32::from(to.0),
        DOWN => i32::from(to.1) - i32::from(from.1),
        _ => i32::from(from.1) - i32::from(to.1),
    }
}

/// Put Link at `(x, y)` and let him come to rest. Returns where he rests when that is within
/// 2 px of the spot (a start inside a wall, or a slide left over from the last push, is not
/// the cell asked for); `None` after a few tries.
fn place(core: &mut NesCore, x: u8, y: u8) -> Option<(u8, u8)> {
    for _ in 0..4 {
        core.bus_mut().write(PLAYER_X, x);
        core.bus_mut().write(PLAYER_Y, y);
        let mut last = (0u8, 0u8);
        let mut still = 0;
        // Bounded: each pass runs a frame and stops after 3 equal positions or 20 frames.
        for _ in 0..20 {
            frame(core, 0);
            if std::env::var("PROBE_TRACE").is_ok() {
                println!(
                    "    place f: ({}, {}) idx {:#x}",
                    core.peek(PLAYER_X as u32),
                    core.peek(PLAYER_Y as u32),
                    core.peek(SCREEN_INDEX)
                );
            }
            let now = (core.peek(PLAYER_X as u32), core.peek(PLAYER_Y as u32));
            still = if now == last { still + 1 } else { 0 };
            last = now;
            if still >= 3 {
                break;
            }
        }
        if last.0.abs_diff(x) <= 2 && last.1.abs_diff(y) <= 2 {
            return Some(last);
        }
    }
    None
}

const DIRS: [(i32, i32, u8); 4] = [(1, 0, RIGHT), (-1, 0, LEFT), (0, 1, DOWN), (0, -1, UP)];

/// Link can stand on cells up to column 14 and row 9; the last column and row are where he
/// leaves the screen (observed: standing at y 229 or x 240 starts a screen change), so
/// they are judged only as targets.
const STAND_COLS: i32 = 14;
const STAND_ROWS: i32 = 9;

fn cell_spot(c: i32, r: i32) -> Option<(u8, u8)> {
    if c > STAND_COLS || r > STAND_ROWS {
        return None;
    }
    let x = u8::try_from(c * 16).ok()?;
    let y = u8::try_from(FIELD_FROM as i32 + r * 16 + LINK_Y_OFF as i32).ok()?;
    Some((x, y))
}

/// The tile ids shown in a 16 px cell (`None`: some tile was not drawn).
fn cell_ids(tiles: &Tiles, c: i32, r: i32) -> Option<[u32; 4]> {
    let mut out = [0u32; 4];
    for (i, (dx, dy)) in [(0, 0), (1, 0), (0, 1), (1, 1)].into_iter().enumerate() {
        let (tc, tr) = (c as usize * 2 + dx, FIELD_FROM / 8 + r as usize * 2 + dy);
        out[i] = u32::from(tiles[tr * 32 + tc]?);
    }
    Some(out)
}

/// Flood-fill the cells Link can reach on the current screen (at rest), judging each cell
/// beside a cell he stands on: Link is put on the known-free cell (test-side write of his x/y)
/// and the pad held towards the neighbour for [`PUSH_FRAMES`]; he advanced (>= 4 px along the
/// push) -> the neighbour is walkable and joins the fill, did not (<= 1 px) -> it is solid. A
/// cell in `avoid` is not pushed at. If a push changes the screen (a door), the fill stops
/// and reports the cell in `door`.
fn probe_screen(
    core: &mut NesCore,
    tiles: &Tiles,
    avoid: &[(i32, i32)],
    known: &mut Known,
) -> ProbeStats {
    let mut st = ProbeStats::default();
    // Seed: the cell Link stands in (cells round to his position).
    let (lx, ly) = (
        i32::from(core.peek(PLAYER_X as u32)),
        i32::from(core.peek(PLAYER_Y as u32)),
    );
    let seed = (
        ((lx + 8) / 16).clamp(0, STAND_COLS),
        ((ly - LINK_Y_OFF as i32 - FIELD_FROM as i32 + 8) / 16).clamp(0, STAND_ROWS),
    );
    let mut queue = std::collections::VecDeque::from([seed]);
    known.entry(seed).or_insert(true);
    // Each pop removes one cell and a cell is queued once (it enters `known` first): bounded
    // by the grid.
    while let Some((c, r)) = queue.pop_front() {
        for (dc, dr, pad) in DIRS {
            let (nc, nr) = (c + dc, r + dr);
            // The last column and row are where Link leaves the screen: pushing at them
            // starts a screen change, so they are not judged.
            if !(0..=STAND_COLS).contains(&nc)
                || !(0..=STAND_ROWS).contains(&nr)
                || known.contains_key(&(nc, nr))
                || avoid.contains(&(nc, nr))
            {
                continue;
            }
            let Some((x0, y0)) = cell_spot(c, r) else {
                continue;
            };
            let Some(start) = place(core, x0, y0) else {
                st.skipped += 1;
                continue;
            };
            for _ in 0..PUSH_FRAMES {
                frame(core, pad);
                if std::env::var("PROBE_TRACE").is_ok() {
                    println!(
                        "    f: ({}, {}) idx {:#x} sx {}",
                        core.peek(PLAYER_X as u32),
                        core.peek(PLAYER_Y as u32),
                        core.peek(SCREEN_INDEX),
                        core.peek(SCROLL_X)
                    );
                }
            }
            let end = (core.peek(PLAYER_X as u32), core.peek(PLAYER_Y as u32));
            // A door or stair cell swaps the screen: stop, the picture is not this one.
            let now = tile_grid(core);
            let diff = now
                .iter()
                .zip(tiles)
                .skip(FIELD_FROM / 8 * 32)
                .filter(|(a, b)| a != b)
                .count();
            if diff > 60 {
                if std::env::var("PROBE_DEBUG").is_ok() {
                    let ch: Vec<_> = (FIELD_FROM / 8 * 32..now.len())
                        .filter(|&i| now[i] != tiles[i])
                        .take(8)
                        .map(|i| (i % 32, i / 32, tiles[i], now[i]))
                        .collect();
                    for (name, g) in [("before", tiles), ("after", &now)] {
                        for r in 8..30 {
                            let line: String = (0..32)
                                .map(|c| {
                                    g[r * 32 + c].map_or("..".to_string(), |t| format!("{t:02x}"))
                                })
                                .collect::<Vec<_>>()
                                .join(" ");
                            println!("{name} {r:2}: {line}");
                        }
                    }
                    println!("  diff {diff} start {start:?} end {end:?} sample {ch:?}");
                }
                st.door = Some((nc, nr));
                return st;
            }
            match advance(pad, start, end) {
                a if a >= 4 => {
                    known.insert((nc, nr), true);
                    queue.push_back((nc, nr));
                    st.open += 1;
                }
                a if a <= 1 => {
                    known.insert((nc, nr), false);
                    st.blocked += 1;
                }
                _ => st.partial += 1,
            }
        }
    }
    st
}

/// Give every tile id of every judged cell its verdict.
fn tally_known(tiles: &Tiles, known: &Known, tally: &mut WalkTally) {
    for (&(c, r), &free) in known {
        if let Some(ids) = cell_ids(tiles, c, r) {
            tally.record_cell(&ids, !free);
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Hop {
    Left,
    Right,
    Up,
    Down,
}

impl Hop {
    fn delta(self) -> i32 {
        match self {
            Hop::Left => -1,
            Hop::Right => 1,
            Hop::Up => -16,
            Hop::Down => 16,
        }
    }
    fn pad(self) -> u8 {
        match self {
            Hop::Left => LEFT,
            Hop::Right => RIGHT,
            Hop::Up => UP,
            Hop::Down => DOWN,
        }
    }
    fn opposite(self) -> u8 {
        match self {
            Hop::Left => RIGHT,
            Hop::Right => LEFT,
            Hop::Up => DOWN,
            Hop::Down => UP,
        }
    }
    /// Link's start spots on the edge facing the hop (tried in turn: a wall can block one).
    fn starts(self) -> Vec<(u8, u8)> {
        // Spots along the facing edge, tried in turn (middle out): a wall tile can
        // block one, and the gaps in an edge are a tile or two wide.
        let ys: Vec<u8> = (0..19u8)
            .map(|i| {
                0x6D + if i % 2 == 0 {
                    i / 2 * 8
                } else {
                    0u8.wrapping_sub((i / 2 + 1) * 8)
                }
            })
            .filter(|&y| (0x3D..=0xCD).contains(&y))
            .collect();
        let xs: Vec<u8> = (0..27u8)
            .map(|i| {
                0x80u8.wrapping_add(if i % 2 == 0 {
                    i / 2 * 8
                } else {
                    0u8.wrapping_sub((i / 2 + 1) * 8)
                })
            })
            .filter(|&x| (0x10..=0xE0).contains(&x))
            .collect();
        match self {
            Hop::Right => ys.iter().map(|&y| (0xE8, y)).collect(),
            Hop::Left => ys.iter().map(|&y| (0x08, y)).collect(),
            Hop::Up => xs.iter().map(|&x| (x, 0x46)).collect(),
            Hop::Down => xs.iter().map(|&x| (x, 0xD6)).collect(),
        }
    }
}

/// Make the game load the screen beside the current one, by observation: Link is put on
/// the facing edge (test-side RAM write) and the pad held; returns the new index and the
/// picture at rest (merged from three captures), or `None` if the index never moved.
/// `arrived`: where Link stood when the screen was entered from the other side (the gap the
/// way back goes through), tried first.
fn hop(
    core: &mut NesCore,
    h: Hop,
    arrived: Option<(u8, u8)>,
    walk_off: bool,
) -> Option<(u8, ScreenImage, (u8, u8), Tiles)> {
    let from = i32::from(core.peek(SCREEN_INDEX));
    let to = from + h.delta();
    for (x, y) in arrived.into_iter().chain(h.starts()) {
        core.bus_mut().write(PLAYER_X, x);
        core.bus_mut().write(PLAYER_Y, y);
        let mut moved = false;
        for _ in 0..16 {
            frame(core, h.pad());
            if i32::from(core.peek(SCREEN_INDEX)) == to {
                moved = true;
                break;
            }
        }
        if !moved {
            continue;
        }
        // Let the scroll finish with the pad released (bounded), then confirm: the
        // index flickers for a frame at some walls without the screen ever changing.
        for _ in 0..90 {
            frame(core, 0);
        }
        if i32::from(core.peek(SCREEN_INDEX)) != to || core.peek(SCROLL_X) != 0 {
            continue;
        }
        let spot = (core.peek(PLAYER_X as u32), core.peek(PLAYER_Y as u32));
        let mut img = ScreenImage::unknown(H);
        let a = frame(core, 0);
        let tiles = tile_grid(core);
        img.fill_unknown_from(&ScreenImage::new(H, a.px)?);
        // The walk-off pushes Link back at the edge he came in by: the explorer wants the
        // hidden pixels; a replay that only wants to stand on the screen does not.
        for _ in 0..if walk_off { 2 } else { 0 } {
            // Walk Link off his spot so the pixels he hid show.
            for _ in 0..14 {
                frame(core, h.opposite());
            }
            let c = frame(core, 0);
            img.fill_unknown_from(&ScreenImage::new(H, c.px)?);
        }
        return Some((u8::try_from(to).ok()?, img, spot, tiles));
    }
    None
}

const HOPS: [Hop; 4] = [Hop::Right, Hop::Left, Hop::Down, Hop::Up];

/// Learn the grid by frontier exploration: from the current screen, walk the hops that
/// are known to work to the nearest screen with an untried hop towards a screen not yet
/// seen, and try it. Every landing is stored. Bounded by a hop budget (law 8: each loop
/// iteration marks one edge tried or ends the walk). Returns (atlas, hops tried, hops
/// that moved the index).
struct Learned {
    atlas: ScreenAtlas,
    tiles: BTreeMap<u8, Tiles>,
    tally: WalkTally,
    probed: Vec<(u8, ProbeStats)>,
    /// Hops from the base screen (the scratch machine's start) to each screen, found first.
    routes: BTreeMap<u8, Vec<Hop>>,
}

/// Record a landing: keep the screen's tile grid and, the first time, the hops that reach it.
fn land(idx: u8, tg: Tiles, route: Option<Vec<Hop>>, out: &mut Learned) {
    if let Some(r) = route {
        out.routes.entry(idx).or_insert(r);
    }
    out.tiles.entry(idx).or_insert(tg);
}

fn learn(raw: &[u8]) -> (Learned, usize, usize) {
    use std::collections::{BTreeSet, VecDeque};
    let mut core = scratch(raw);
    let mut out = Learned {
        atlas: ScreenAtlas::new(),
        tiles: BTreeMap::new(),
        tally: WalkTally::new(),
        probed: Vec::new(),
        routes: BTreeMap::new(),
    };
    let mut tried_edges: BTreeSet<(u8, i32)> = BTreeSet::new();
    let mut good: BTreeMap<u8, Vec<(Hop, u8)>> = BTreeMap::new();
    let mut seen: BTreeSet<u8> = BTreeSet::new();
    // Edges back to a screen already seen, worth one try so the walk can return.
    let mut arrivals: BTreeMap<(u8, i32), (u8, u8)> = BTreeMap::new();
    let mut want_back: BTreeSet<(u8, i32)> = BTreeSet::new();
    seen.insert(core.peek(SCREEN_INDEX));
    // `scratch` ends on a frame run with tile capture on: its record is the base screen's.
    let (base_idx, base_tiles) = (core.peek(SCREEN_INDEX), tile_grid(&core));
    land(base_idx, base_tiles, Some(Vec::new()), &mut out);
    let (mut tried, mut moved) = (0usize, 0usize);
    let store = |atlas: &mut ScreenAtlas, idx: u8, img: ScreenImage| match atlas.get(idx).cloned() {
        Some(mut have) => {
            have.fill_unknown_from(&img);
            atlas.insert(idx, have);
        }
        None => atlas.insert(idx, img),
    };
    let base = core.peek(SCREEN_INDEX);
    let mut resets = 0u32;
    for _ in 0..300 {
        let search = |here: u8| {
            // Nearest screen (by verified hops) with an untried hop to an unseen screen.
            let mut prev: BTreeMap<u8, (u8, Hop)> = BTreeMap::new();
            let mut q = VecDeque::from([here]);
            let mut found: Option<(u8, Hop)> = None;
            let mut visited = BTreeSet::from([here]);
            while let Some(c) = q.pop_front() {
                for h in HOPS {
                    let n = i32::from(c) + h.delta();
                    let ok = (0..128).contains(&n)
                        && (h.delta().abs() != 1 || n / 16 == i32::from(c) / 16);
                    if ok
                        && (!seen.contains(&(n as u8)) || want_back.contains(&(c, h.delta())))
                        && !tried_edges.contains(&(c, h.delta()))
                    {
                        found = Some((c, h));
                        break;
                    }
                }
                if found.is_some() {
                    break;
                }
                for &(h, n) in good.get(&c).map(Vec::as_slice).unwrap_or(&[]) {
                    if visited.insert(n) {
                        prev.insert(n, (c, h));
                        q.push_back(n);
                    }
                }
            }
            (found, prev)
        };
        let mut here = core.peek(SCREEN_INDEX);
        let (mut found, mut prev) = search(here);
        // Stranded (a return hop failed): replay the scratch machine and resume from the base screen.
        if found.is_none() && resets < 12 {
            let (f, p) = search(base);
            if f.is_some() {
                core = scratch(raw);
                resets += 1;
                (here, found, prev) = (base, f, p);
            }
        }
        let Some((at, edge)) = found else { break };
        // Walk to `at` along verified hops.
        let mut route = Vec::new();
        let mut c = at;
        while c != here {
            let Some(&(p, h)) = prev.get(&c) else { break };
            route.push(h);
            c = p;
        }
        let mut ok_walk = c == here;
        for h in route.into_iter().rev() {
            tried += 1;
            let back = arrivals.get(&(core.peek(SCREEN_INDEX), h.delta())).copied();
            match hop(&mut core, h, back, true) {
                Some((idx, img, spot, tg)) => {
                    arrivals.insert((idx, -h.delta()), spot);
                    moved += 1;
                    store(&mut out.atlas, idx, img);
                    land(idx, tg, None, &mut out);
                }
                None => {
                    ok_walk = false;
                    break;
                }
            }
        }
        if !ok_walk {
            // A verified hop failed this time: forget the edge and re-plan from where we are.
            tried_edges.remove(&(at, edge.delta()));
            continue;
        }
        tried_edges.insert((at, edge.delta()));
        tried += 1;
        let back = arrivals.get(&(at, edge.delta())).copied();
        if let Some((idx, img, spot, tg)) = hop(&mut core, edge, back, true) {
            arrivals.insert((idx, -edge.delta()), spot);
            moved += 1;
            seen.insert(idx);
            good.entry(at).or_default().push((edge, idx));
            want_back.insert((idx, -edge.delta()));
            store(&mut out.atlas, idx, img);
            let mut route = out.routes.get(&at).cloned().unwrap_or_default();
            route.push(edge);
            land(idx, tg, Some(route), &mut out);
        }
    }
    println!(
        "edges tried {}, edges that moved {}, replays {resets}",
        tried_edges.len(),
        good.values().map(Vec::len).sum::<usize>()
    );
    (out, tried, moved)
}

fn report(l: &Learned) {
    for (idx, p) in &l.probed {
        println!(
            "screen {idx:#04x}: {} open pushes, {} blocked, {} partial, {} skipped, door {:?}",
            p.open, p.blocked, p.partial, p.skipped, p.door
        );
    }
    let t = &l.tally;
    println!(
        "tile ids probed {}: {} solid, {} walkable, {} ambiguous, {} unseen",
        t.ids(),
        t.ids_with(Verdict::Solid).len(),
        t.ids_with(Verdict::Walkable).len(),
        t.ids_with(Verdict::Ambiguous).len(),
        t.ids_with(Verdict::Unseen).len()
    );
    for id in t.ids_with(Verdict::Ambiguous) {
        println!("  ambiguous {id:#04x}: {:?}", t.counts(id));
    }
}

/// Visit the first `n` screens (nearest the start first), each on fresh scratch machines:
/// replay to the base, walk the screen's route with the explorer's hops, and flood-fill it.
/// A door's transition ends a fill and spoils the machine, so the cell is avoided and the
/// screen redone (up to 4 times); what each fill judged is kept.
fn probe_routes(raw: &[u8], l: &mut Learned, n: usize) {
    let mut order: Vec<(u8, Vec<Hop>)> = l.routes.iter().map(|(&i, r)| (i, r.clone())).collect();
    order.sort_by_key(|(i, r)| (r.len(), *i));
    let mut done = 0;
    for (idx, route) in order.into_iter().take(n * 2) {
        if done >= n {
            break;
        }
        let (mut avoid, mut known, mut tiles) = (Vec::new(), Known::new(), None);
        let mut total = ProbeStats::default();
        for _ in 0..4 {
            let mut core = scratch(raw);
            let mut tg = Some(tile_grid(&core));
            for &h in &route {
                tg = hop(&mut core, h, None, false).map(|(_, _, _, g)| g);
                if tg.is_none() {
                    break;
                }
            }
            if tg.is_none() || core.peek(SCREEN_INDEX) != idx {
                println!("screen {idx:#04x}: route did not reproduce");
                break;
            }
            // The picture as it rests now (the explorer's capture is of the first frame).
            for _ in 0..4 {
                frame(&mut core, 0);
            }
            let tg = tile_grid(&core);
            if std::env::var("PROBE_DEBUG").is_ok() {
                println!(
                    "  link at ({}, {}), idx {:#x}",
                    core.peek(PLAYER_X as u32),
                    core.peek(PLAYER_Y as u32),
                    core.peek(SCREEN_INDEX)
                );
            }
            let st = probe_screen(&mut core, &tg, &avoid, &mut known);
            total.open += st.open;
            total.blocked += st.blocked;
            total.partial += st.partial;
            total.skipped += st.skipped;
            tiles = Some(tg);
            match st.door {
                Some(d) => avoid.push(d),
                None => break,
            }
        }
        if let Some(tg) = tiles {
            total.door = avoid.first().copied();
            println!(
                "screen {idx:#04x}: {} cells judged, doors avoided {avoid:?}",
                known.len()
            );
            tally_known(&tg, &known, &mut l.tally);
            l.probed.push((idx, total));
            done += 1;
        }
    }
}

/// The learned screen as a canvas of its playfield (256 x 176 px) on a 16 x 8 world grid.
fn screen_canvas(idx: u8, img: &ScreenImage) -> Canvas {
    let cells = img.pixels[FIELD_FROM * SCREEN_W..H * SCREEN_W]
        .iter()
        .map(|&p| {
            (p != UNKNOWN).then_some(PpuPixel {
                palette_index: p,
                layer: PixelLayer::Background(0),
                sprite_id: None,
                priority: 0,
            })
        })
        .collect();
    let (col, row) = (i64::from(idx % 16), i64::from(idx / 16));
    Canvas::from_raw_parts(
        col * SCREEN_W as i64,
        row * (H - FIELD_FROM) as i64,
        SCREEN_W,
        H - FIELD_FROM,
        cells,
    )
}

fn temp_root(label: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("rf-w39-03b-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

#[test]
#[ignore = "needs The Legend of Zelda (USA) (RF_SCREENS_ROMS_DIR); a few minutes in release"]
fn zelda_stage_set_from_observed_walking() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let sha = rf_cart::hash::hash_all(&raw).sha256;

    // 1. Learn the screens, then probe solidity on the first few.
    let (mut learned, tried, moved) = learn(&raw);
    println!(
        "learned {} screens ({moved} of {tried} hops moved), {} with routes",
        learned.atlas.len(),
        learned.routes.len()
    );
    probe_routes(&raw, &mut learned, 20);
    report(&learned);
    let rows = learned.tally.to_rows(
        "pushes at 16 px cells on a scratch machine; Link placed beside the cell, pad held 8 frames, blocked = no advance and he can walk the other way",
        Grade::Candidate,
    );
    let toml = rows_toml(&rows);
    std::fs::create_dir_all(OUT_DIR).expect("dir");
    std::fs::write(format!("{OUT_DIR}/zelda_tile_rows.toml"), &toml).expect("writes rows");
    println!("{} solid [[tile]] rows proposed:\n{toml}", rows.len());
    assert!(!rows.is_empty(), "collision facts were found");

    // 2. Bake every learned screen over its own canvas footprint (8 px cells).
    let rules: Vec<TileRule> = rows
        .iter()
        .map(|r| TileRule {
            first: r.first,
            last: r.last,
            class: RuleClass::Solid,
        })
        .collect();
    let mut set = StageSet::default();
    let (mut cells, mut visited) = (0usize, 0usize);
    for idx in 0..128u8 {
        let (Some(img), Some(tg)) = (learned.atlas.get(idx), learned.tiles.get(&idx)) else {
            continue;
        };
        let canvas = screen_canvas(idx, img);
        let (ox, oy) = canvas.origin();
        let id_at = |c: i64, r: i64| -> Option<u32> {
            // World cell to this screen's tile grid: columns from its left edge, rows below the band.
            let (tc, tr) = (c - ox / 8, r - oy / 8 + (FIELD_FROM / 8) as i64);
            if !(0..32).contains(&tc) || !(0..30).contains(&tr) {
                return None;
            }
            tg[tr as usize * 32 + tc as usize].map(u32::from)
        };
        let Some(screen) = bake_canvas(
            u32::from(idx),
            &canvas,
            8,
            id_at,
            &Solidity {
                rules: &rules,
                painted: None,
            },
        ) else {
            continue;
        };
        cells += screen.cells.len();
        visited += screen
            .cells
            .iter()
            .filter(|c| c.kind != Kind::Unknown)
            .count();
        set.screens.push(screen);
    }
    println!(
        "stage set: {} screens, {cells} cells ({visited} known), {} body cells, {} quads",
        set.screens.len(),
        set.body_count(),
        set.quad_count()
    );
    assert!(set.screens.len() >= 20, "many screens baked");
    assert!(
        set.body_count() > 0 && set.quad_count() > 0,
        "walls extruded"
    );
    assert!(
        set.screens.iter().all(|s| s.cols <= 32 && s.rows <= 22),
        "each screen is its canvas footprint, not more"
    );

    // 3. Cached under the ROM hash and reloaded.
    let root = temp_root("zelda");
    let mut cache = Cache::open(&root, 256 << 20).expect("cache");
    let stored = save_stage_set(&mut cache, &sha, &set).expect("saves");
    println!("stored {stored} bytes");
    drop(cache);
    let mut again = Cache::open(&root, 256 << 20).expect("reopen");
    assert_eq!(load_stage_set(&mut again, &sha).expect("loads"), Some(set));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
#[ignore = "needs The Legend of Zelda (USA) (RF_SCREENS_ROMS_DIR)"]
fn probe_the_base_screen_only() {
    let Some(path) = rom_path() else {
        return;
    };
    let raw = load_rom(&path);
    let mut l = Learned {
        atlas: ScreenAtlas::new(),
        tiles: BTreeMap::new(),
        tally: WalkTally::new(),
        probed: Vec::new(),
        routes: BTreeMap::from([(0x77, Vec::new())]),
    };
    probe_routes(&raw, &mut l, 1);
    report(&l);
}

#[test]
#[ignore = "debug: learn and save the routes"]
fn learn_and_save_routes() {
    let Some(path) = rom_path() else {
        return;
    };
    let raw = load_rom(&path);
    let (l, _, _) = learn(&raw);
    std::fs::create_dir_all(OUT_DIR).expect("dir");
    let text: String = l
        .routes
        .iter()
        .map(|(i, r)| {
            let hops: String = r
                .iter()
                .map(|h| match h {
                    Hop::Left => 'L',
                    Hop::Right => 'R',
                    Hop::Up => 'U',
                    Hop::Down => 'D',
                })
                .collect();
            format!("{i} {hops}\n")
        })
        .collect();
    std::fs::write(format!("{OUT_DIR}/routes.txt"), text).expect("writes");
}

#[test]
#[ignore = "debug: probe screens from saved routes"]
fn probe_saved_route() {
    let Some(path) = rom_path() else {
        return;
    };
    let raw = load_rom(&path);
    let want: u8 = std::env::var("SCREEN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(103);
    let text = std::fs::read_to_string(format!("{OUT_DIR}/routes.txt")).expect("routes");
    let mut l = Learned {
        atlas: ScreenAtlas::new(),
        tiles: BTreeMap::new(),
        tally: WalkTally::new(),
        probed: Vec::new(),
        routes: BTreeMap::new(),
    };
    for line in text.lines() {
        let (i, hops) = line.split_once(' ').unwrap_or((line, ""));
        let idx: u8 = i.parse().expect("idx");
        if idx != want {
            continue;
        }
        let r = hops
            .chars()
            .map(|c| match c {
                'L' => Hop::Left,
                'R' => Hop::Right,
                'U' => Hop::Up,
                _ => Hop::Down,
            })
            .collect();
        l.routes.insert(idx, r);
    }
    println!("routes {:?}", l.routes);
    probe_routes(&raw, &mut l, 1);
    report(&l);
}
