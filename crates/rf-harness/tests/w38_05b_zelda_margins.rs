//! **W38-05b: Zelda widescreen margins from the neighbouring screens** (ROM-gated, law 5).
//!
//! Route (a) of the ticket, by observation only (D-021): the recipe's `screen_index`
//! byte moves by 1 sideways and 16 vertically at screen changes, so screens sit on a
//! 16x8 grid. To see a screen the player has not visited, a SCRATCH machine (a second
//! `NesCore` replayed from reset to the same overworld spot, never the live one) is
//! made to hop screen by screen: a test-side RAM write puts Link on the edge, the pad
//! is held, and the game's own loader brings in the next screen; the picture at rest
//! is captured (sprite pixels unknown, merged from captures with Link and the actors
//! in other places). The atlas feeds `rf_enhance::screen_margins`, which lays the
//! neighbours beside the live screen at 32:9. Margins are presentation only: the live
//! machine is only peeked, and a second run proves its frames and RAM byte-identical
//! with margins on.
//!
//! Only offsets and counts are printed; PNGs (ROM-derived) go to the scratchpad, never
//! the repo (D-024).
//!
//! ```text
//! cargo test --release -p rf-harness --test w38_05b_zelda_margins -- --ignored --nocapture
//! ```

mod w38_05_zelda_common;

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PixelLayer, PpuPixel, ResetKind};
use rf_enhance::screen_margins::{ScreenAtlas, ScreenImage, SCREEN_W, UNKNOWN};
use rf_nes::core::NesCore;
use rf_nes::CpuBus;
use rf_renderer::palette::NES_PALETTE;
use w38_05_zelda_common::{
    inputs, load_rom, rom_path, script, DOWN, FILE, FRAMES, LEFT, RIGHT, UP,
};

const SCREEN_INDEX: u32 = 0x00EB;
const PLAYER_X: u16 = 0x0070;
const PLAYER_Y: u16 = 0x0084;
const SCROLL_X: u32 = 0x00FD;
/// Frame at which the scripted play stands outside the first cave (idle base state).
const BASE_FRAME: u32 = 1650;
/// Picture rows: the playfield starts below the status band (observed in the PNGs).
const FIELD_FROM: usize = 64;
const H: usize = 240;
const OUT_DIR: &str = "/private/tmp/claude-502/-Users-bmatthews-Code/10a3a02b-3b5c-46ad-afc7-f01185980ef7/scratchpad/w38-05b";

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
    for f in 0..BASE_FRAME {
        frame(&mut core, script(f));
    }
    core
}

#[derive(Clone, Copy)]
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
) -> Option<(u8, ScreenImage, (u8, u8))> {
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
        img.fill_unknown_from(&ScreenImage::new(H, a.px)?);
        for _ in 0..2 {
            // Walk Link off his spot so the pixels he hid show.
            for _ in 0..14 {
                frame(core, h.opposite());
            }
            let c = frame(core, 0);
            img.fill_unknown_from(&ScreenImage::new(H, c.px)?);
        }
        return Some((u8::try_from(to).ok()?, img, spot));
    }
    None
}

const HOPS: [Hop; 4] = [Hop::Right, Hop::Left, Hop::Down, Hop::Up];

/// Learn the grid by frontier exploration: from the current screen, walk the hops that
/// are known to work to the nearest screen with an untried hop towards a screen not yet
/// seen, and try it. Every landing is stored. Bounded by a hop budget (law 8: each loop
/// iteration marks one edge tried or ends the walk). Returns (atlas, hops tried, hops
/// that moved the index).
fn learn(raw: &[u8]) -> (ScreenAtlas, usize, usize) {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    let mut core = scratch(raw);
    let mut atlas = ScreenAtlas::new();
    let mut tried_edges: BTreeSet<(u8, i32)> = BTreeSet::new();
    let mut good: BTreeMap<u8, Vec<(Hop, u8)>> = BTreeMap::new();
    let mut seen: BTreeSet<u8> = BTreeSet::new();
    // Edges back to a screen already seen, worth one try so the walk can return.
    let mut arrivals: BTreeMap<(u8, i32), (u8, u8)> = BTreeMap::new();
    let mut want_back: BTreeSet<(u8, i32)> = BTreeSet::new();
    seen.insert(core.peek(SCREEN_INDEX));
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
            match hop(&mut core, h, back) {
                Some((idx, img, spot)) => {
                    arrivals.insert((idx, -h.delta()), spot);
                    moved += 1;
                    store(&mut atlas, idx, img);
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
        if let Some((idx, img, spot)) = hop(&mut core, edge, back) {
            arrivals.insert((idx, -edge.delta()), spot);
            moved += 1;
            seen.insert(idx);
            good.entry(at).or_default().push((edge, idx));
            want_back.insert((idx, -edge.delta()));
            store(&mut atlas, idx, img);
        }
    }
    println!(
        "edges tried {}, edges that moved {}, replays {resets}",
        tried_edges.len(),
        good.values().map(Vec::len).sum::<usize>()
    );
    (atlas, tried, moved)
}

fn colour(px: &[u8], w: usize) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(px.len() * 4);
    for (i, &p) in px.iter().enumerate() {
        let [r, g, b] = if p == UNKNOWN {
            [0x30, 0, 0x30]
        } else {
            NES_PALETTE[usize::from(p & 0x3F)]
        };
        let _ = (i, w);
        rgba.extend_from_slice(&[r, g, b, 255]);
    }
    rgba
}

fn write_png(name: &str, rgba: &[u8], w: usize, h: usize) {
    std::fs::create_dir_all(OUT_DIR).expect("dir");
    let png = rf_renderer::png::encode_rgba(rgba, w as u32, h as u32);
    std::fs::write(format!("{OUT_DIR}/{name}"), png).expect("writes png");
}

/// The 32:9 picture: the live frame in the middle, the HUD band kept as drawn on the
/// live frame's own rows, margins from the atlas beside the playfield.
fn widened(atlas: &ScreenAtlas, idx: u8, live: &[u8], out_w: usize) -> Vec<u8> {
    let pad = (out_w - SCREEN_W) / 2;
    let margins = atlas.margins(idx, out_w, 0..H);
    let mut out = vec![UNKNOWN; out_w * H];
    for y in 0..H {
        for x in 0..out_w {
            let v = if (pad..pad + SCREEN_W).contains(&x) {
                live[y * SCREEN_W + x - pad]
            } else if y >= FIELD_FROM {
                margins[y * out_w + x]
            } else {
                UNKNOWN
            };
            out[y * out_w + x] = v;
        }
    }
    colour(&out, out_w)
}

#[test]
#[ignore = "needs The Legend of Zelda (USA) (RF_SCREENS_ROMS_DIR); about a minute in release"]
fn zelda_margins_are_laid_from_neighbour_screens_and_change_nothing() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);

    // 1. Learn the grid on the scratch machine.
    let (atlas, tried, moved) = learn(&raw);
    let unknown: usize = (0..128u8)
        .filter_map(|i| atlas.get(i))
        .map(|s| s.unknown_count())
        .sum();
    println!(
        "learned {} of 128 screens ({moved} of {tried} hops moved the index); {unknown} unknown pixels left",
        atlas.len()
    );
    assert!(
        atlas.len() >= 20,
        "the loader gave many screens ({})",
        atlas.len()
    );

    // 2. The control run, then the run with margins computed every frame at rest.
    let play = inputs(FRAMES);
    let run = |margins: bool| {
        let mut core = NesCore::from_ines_bytes(&raw).expect("loads");
        core.reset(ResetKind::Hard);
        let (mut frames_hash, mut laid, mut gated_out, mut agree_sum, mut agree_n) =
            (0xcbf2_9ce4_8422_2325u64, 0u32, 0u32, 0f64, 0u32);
        let wide = rf_enhance::widescreen::Aspect::Wide32x9.output_width(H, 1.0);
        let mut samples: Vec<(u32, u8, Vec<u8>)> = Vec::new();
        for (f, input) in play.iter().enumerate() {
            let mut cap = Cap::default();
            core.run_frame(input, &mut cap);
            frames_hash = (frames_hash ^ cap.rgb_hash).wrapping_mul(0x0100_0000_01b3);
            let idx = core.peek(SCREEN_INDEX);
            if margins && f % 8 == 0 && core.peek(SCROLL_X) == 0 && f as u32 >= BASE_FRAME {
                match atlas.agreement(idx, &cap.px, FIELD_FROM..H) {
                    Some(a) if a >= 0.9 => {
                        agree_sum += f64::from(a);
                        agree_n += 1;
                        let m = atlas.margins(idx, wide, FIELD_FROM..H);
                        assert_eq!(m.len(), wide * (H - FIELD_FROM));
                        laid += 1;
                        if laid % 40 == 1 {
                            samples.push((f as u32, idx, cap.px.clone()));
                        }
                    }
                    _ => gated_out += 1,
                }
            }
        }
        (
            frames_hash,
            ram_print(&core),
            laid,
            gated_out,
            agree_sum,
            agree_n,
            samples,
        )
    };
    let (h0, r0, ..) = run(false);
    let (h1, r1, laid, gated, asum, an, samples) = run(true);
    println!(
        "margins laid on {laid} sampled frames, {gated} gated out (not that overworld screen), mean agreement {:.3}",
        if an > 0 { asum / f64::from(an) } else { 0.0 }
    );
    println!("frames hash {h0:016x} vs {h1:016x}; ram hash {r0:016x} vs {r1:016x}");
    assert_eq!(h0, h1, "the picture is byte-identical with margins on");
    assert_eq!(r0, r1, "the machine is byte-identical with margins on");
    assert!(laid >= 20, "margins were laid ({laid})");

    // 3. PNGs at 32:9 from live frames, and a neighbourhood sheet.
    let wide = rf_enhance::widescreen::Aspect::Wide32x9.output_width(H, 1.0);
    for (f, idx, live) in samples.iter().take(6) {
        write_png(
            &format!("zelda_32x9_f{f:05}_s{idx:02x}.png"),
            &widened(&atlas, *idx, live, wide),
            wide,
            H,
        );
    }
    // Contact sheet of the whole learned grid (half scale: every 2nd pixel).
    let (sw, sh) = (16 * SCREEN_W / 2, 8 * (H - FIELD_FROM) / 2);
    let mut sheet = vec![UNKNOWN; sw * sh];
    for i in 0..128u8 {
        let Some(s) = atlas.get(i) else { continue };
        let (c, r) = (usize::from(i % 16), usize::from(i / 16));
        for y in 0..(H - FIELD_FROM) / 2 {
            for x in 0..SCREEN_W / 2 {
                sheet[(r * (H - FIELD_FROM) / 2 + y) * sw + c * SCREEN_W / 2 + x] =
                    s.pixels[(FIELD_FROM + y * 2) * SCREEN_W + x * 2];
            }
        }
    }
    write_png("zelda_grid_sheet.png", &colour(&sheet, sw), sw, sh);
}
