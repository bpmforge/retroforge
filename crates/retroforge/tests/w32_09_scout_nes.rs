//! **W32-09: the level scout on NES -- Super Mario Bros. 3 (World 1-1) and
//! Mega Man 2 (Air Man / Metal Man stage start).**
//!
//! ROM-gated (law 5): every test here is `#[ignore]` and skips with a message
//! when the ROM is absent. The probes find, by observation, the work-RAM
//! bytes the profiles' `[scout]` tables use; the evidence is written into
//! `docs/research/W32-09-scout-nes.md`.
//!
//! ```text
//! cargo test --release -p retroforge --test w32_09_scout_nes -- --ignored --nocapture <probe>
//! ```
//!
//! `RF_NES_SCRIPT` drives `script_shots`: whitespace separated tokens, `wN`
//! idle N frames, `<buttons>N` hold N frames (buttons: A B S(tart) E(select)
//! U D L R, joined with `+`), `shot:name` writes `target/screens-probe/<name>.png`.

use std::path::{Path, PathBuf};

use retroforge::canvas_accum::CanvasAccumulator;
use retroforge::scout::{Scout, ScoutDir, ScoutPlan, ScoutStatus, ScoutStrategy};
use retroforge::stepper::EmuStepper;
use rf_core_api::{CoreSink, FrameBundle, FrameBundleBuilder, InputFrame, PpuPixel};

pub const W: u16 = 256;
pub const H: u16 = 240;

fn rom_path(file: &str) -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = Path::new(&root).join("nes").join(file);
    p.exists().then_some(p)
}

pub struct Sink {
    pub bundle: FrameBundleBuilder,
}

impl CoreSink for Sink {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        self.bundle.video_scanline(y, pixels);
    }
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, ev: rf_core_api::CoreEvent) {
        self.bundle.event(ev);
    }
}

pub fn open(file: &str) -> Option<(EmuStepper, Sink)> {
    let Some(path) = rom_path(file) else {
        println!("SKIP: {file} not found");
        return None;
    };
    let raw = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
    let mut st = EmuStepper::open(&raw).expect("opens");
    st.set_event_mask(rf_core_api::EventMask::ALL);
    Some((
        st,
        Sink {
            bundle: FrameBundleBuilder::new(W, H),
        },
    ))
}

pub const BTN_A: u16 = 1 << 0;
pub const BTN_B: u16 = 1 << 1;
pub const BTN_SELECT: u16 = 1 << 2;
pub const BTN_START: u16 = 1 << 3;
pub const BTN_UP: u16 = 1 << 4;
pub const BTN_DOWN: u16 = 1 << 5;
pub const BTN_LEFT: u16 = 1 << 6;
pub const BTN_RIGHT: u16 = 1 << 7;

fn pad(mask: u16) -> InputFrame {
    let mut f = InputFrame::empty();
    f.ports[0] = mask;
    f
}

/// One frame; returns its bundle.
pub fn frame(st: &mut EmuStepper, sink: &mut Sink, mask: u16) -> FrameBundle {
    st.latch_and_advance_frame(pad(mask), sink);
    sink.bundle.take(st.frame_count())
}

pub fn run(st: &mut EmuStepper, sink: &mut Sink, mask: u16, n: u32) {
    for _ in 0..n {
        drop(frame(st, sink, mask));
    }
}

pub fn save_bundle(b: &FrameBundle, name: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/screens-probe");
    std::fs::create_dir_all(&dir).expect("dir");
    let mut rgba = Vec::with_capacity(b.video.len() * 4);
    for p in &b.video {
        let c = rf_renderer::resolve_index(p.palette_index, None);
        rgba.extend_from_slice(&[c[0], c[1], c[2], 255]);
    }
    let path = dir.join(format!("{name}.png"));
    image::save_buffer(
        &path,
        &rgba,
        u32::from(b.width),
        u32::from(b.height),
        image::ColorType::Rgba8,
    )
    .expect("png");
    println!("SHOT {}", path.display());
}

fn mask_of(tok: &str) -> u16 {
    tok.split('+').fold(0, |m, c| {
        m | match c {
            "A" => BTN_A,
            "B" => BTN_B,
            "S" => BTN_START,
            "E" => BTN_SELECT,
            "U" => BTN_UP,
            "D" => BTN_DOWN,
            "L" => BTN_LEFT,
            "R" => BTN_RIGHT,
            _ => 0,
        }
    })
}

/// Run a script token string; returns the last bundle.
pub fn script(st: &mut EmuStepper, sink: &mut Sink, text: &str) -> Option<FrameBundle> {
    let mut last = None;
    for tok in text.split_whitespace() {
        if let Some(name) = tok.strip_prefix("shot:") {
            if let Some(b) = last.as_ref() {
                save_bundle(b, name);
            }
            continue;
        }
        if let Some(n) = tok.strip_prefix('w') {
            let n: u32 = n.parse().expect("wN");
            for _ in 0..n {
                last = Some(frame(st, sink, 0));
            }
            continue;
        }
        let split = tok.find(|c: char| c.is_ascii_digit()).expect("button+N");
        let (b, n) = tok.split_at(split);
        let (m, n) = (mask_of(b), n.parse::<u32>().expect("N"));
        for _ in 0..n {
            last = Some(frame(st, sink, m));
        }
    }
    last
}

#[test]
#[ignore = "needs a NES ROM; RF_NES_ROM + RF_NES_SCRIPT"]
fn script_shots() {
    let file =
        std::env::var("RF_NES_ROM").unwrap_or_else(|_| "Super Mario Bros. 3 (USA).zip".into());
    let Some((mut st, mut sink)) = open(&file) else {
        return;
    };
    let text = std::env::var("RF_NES_SCRIPT").unwrap_or_default();
    script(&mut st, &mut sink, &text);
    println!("frames {}", st.frame_count());
}

/// Right held, B (run), A (jump) for the first 20 of every 40 frames.
fn hop(f: u32) -> u16 {
    let phase: u32 = std::env::var("RF_HOP_PHASE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    BTN_RIGHT | BTN_B | if (f + phase) % 64 < 40 { BTN_A } else { 0 }
}

/// Closed-loop walker for SMB3 1-1: run right, hop; if Mario's X ($0090 lo,
/// $0075 hi) has not moved for 6 frames, hold a full jump for 45 frames.
#[derive(Default)]
struct Walker {
    last: u32,
    still: u32,
    jump_left: u32,
}

impl Walker {
    fn input(&mut self, f: u32, st: &EmuStepper) -> u16 {
        let x = u32::from(st.peek_bus(0x90)) | u32::from(st.peek_bus(0x75)) << 8;
        self.still = if x == self.last { self.still + 1 } else { 0 };
        self.last = x;
        if self.still >= 6 && self.jump_left == 0 {
            self.jump_left = 45;
        }
        let jump = if self.jump_left > 0 {
            self.jump_left -= 1;
            true
        } else {
            f % 64 < 30
        };
        BTN_RIGHT | BTN_B | if jump { BTN_A } else { 0 }
    }
}

const SMB3_TO_LEVEL: &str = "w300 S6 w120 S6 w400 R8 w60 U8 w60 A8 w300";

/// Compact event dump: `S<first>-<last>` runs of Scanline events and
/// `W(x,y)@<n scanlines seen so far>` writes, in arrival order.
fn events_line(b: &FrameBundle) -> String {
    use rf_core_api::CoreEvent as E;
    let mut out = String::new();
    let (mut n, mut first, mut last) = (0u32, None::<u16>, 0u16);
    let flush = |out: &mut String, first: &mut Option<u16>, last: u16| {
        if let Some(f) = first.take() {
            out.push_str(&format!("S{f}-{last} "));
        }
    };
    for e in &b.events {
        match e {
            E::Scanline(y) => {
                n += 1;
                if first.is_none() {
                    first = Some(*y);
                }
                last = *y;
            }
            E::ScrollWrite { x, y, .. } => {
                flush(&mut out, &mut first, last);
                out.push_str(&format!("W({x},{y})@{n} "));
            }
            E::MapperIrq => {
                flush(&mut out, &mut first, last);
                out.push_str("IRQ ");
            }
            E::VblankStart => {
                flush(&mut out, &mut first, last);
                out.push_str("VBL ");
            }
            _ => {}
        }
    }
    flush(&mut out, &mut first, last);
    out
}

#[test]
#[ignore = "needs SMB3"]
fn smb3_events() {
    let Some((mut st, mut sink)) = open("Super Mario Bros. 3 (USA).zip") else {
        return;
    };
    script(&mut st, &mut sink, SMB3_TO_LEVEL);
    let mut last = String::new();
    for f in 0..200 {
        let b = frame(&mut st, &mut sink, hop(f));
        let line = events_line(&b);
        if f < 4 || line != last {
            println!("f{f} {line}");
        }
        last = line;
    }
}

/// Work RAM $0000..$07FF followed by PRG-RAM $6000..$7FFF.
fn ram_dump(st: &EmuStepper) -> Vec<u8> {
    (0..0x800u32)
        .chain(0x6000..0x8000)
        .map(|a| st.peek_bus(a))
        .collect()
}

fn addr_of(i: usize) -> u32 {
    if i < 0x800 {
        i as u32
    } else {
        0x6000 + (i as u32 - 0x800)
    }
}

/// Per frame: the scroll x that DREW the frame (the last write of the frame
/// before it: `carry`), the trailing write of its own slice, and RAM after it.
struct Trace {
    drawn_x: Vec<i64>,
    trailing_x: Vec<i64>,
    ram: Vec<Vec<u8>>,
}

fn last_write_x(b: &FrameBundle) -> Option<i64> {
    b.events.iter().rev().find_map(|e| match e {
        rf_core_api::CoreEvent::ScrollWrite { x, .. } => Some(i64::from(*x)),
        _ => None,
    })
}

/// Walk `n` frames from the current state with `input(f)`.
fn trace(
    st: &mut EmuStepper,
    sink: &mut Sink,
    n: u32,
    mut input: impl FnMut(u32, &EmuStepper) -> u16,
) -> Trace {
    let mut t = Trace {
        drawn_x: Vec::new(),
        trailing_x: Vec::new(),
        ram: Vec::new(),
    };
    let mut carry = 0;
    for f in 0..n {
        let m = input(f, st);
        let b = frame(st, sink, m);
        t.drawn_x.push(carry);
        // The trailing write of THIS slice (vblank), the one the tracker
        // carries into the next frame.
        let tw = b
            .events
            .iter()
            .rev()
            .take_while(|e| !matches!(e, rf_core_api::CoreEvent::Scanline(_)))
            .find_map(|e| match e {
                rf_core_api::CoreEvent::ScrollWrite { x, .. } => Some(i64::from(*x)),
                _ => None,
            });
        carry = tw.or_else(|| last_write_x(&b)).unwrap_or(carry);
        t.trailing_x.push(carry);
        t.ram.push(ram_dump(st));
    }
    t
}

/// 16-bit counters: (lo, hi) pairs where lo moves by 1..=12 on most moving
/// frames and hi steps +1 exactly when lo wraps (or -1 going left).
fn counters(t: &Trace) -> Vec<(usize, usize, usize)> {
    let n = t.ram.len();
    let size = t.ram[0].len();
    let mut out = Vec::new();
    for a in 0..size {
        let mut fwd = 0;
        let mut moved = 0;
        for f in 1..n {
            let d = t.ram[f][a].wrapping_sub(t.ram[f - 1][a]);
            if d != 0 {
                moved += 1;
                if (1..=12).contains(&d) {
                    fwd += 1;
                }
            }
        }
        if moved < n / 4 || fwd * 10 < moved * 9 {
            continue;
        }
        // hi candidates: changes only at wraps of `a`
        for b in 0..size {
            if b == a {
                continue;
            }
            let (mut wraps, mut ok, mut stray) = (0, 0, 0);
            for f in 1..n {
                let wrapped = t.ram[f][a] < t.ram[f - 1][a];
                let step = t.ram[f][b].wrapping_sub(t.ram[f - 1][b]);
                if wrapped {
                    wraps += 1;
                    if step == 1 {
                        ok += 1;
                    }
                } else if step != 0 {
                    stray += 1;
                }
            }
            if wraps >= 2 && ok == wraps && stray == 0 {
                out.push((a, b, wraps));
            }
        }
    }
    out
}

#[test]
#[ignore = "needs SMB3"]
fn smb3_ram() {
    let Some((mut st, mut sink)) = open("Super Mario Bros. 3 (USA).zip") else {
        return;
    };
    script(&mut st, &mut sink, SMB3_TO_LEVEL);
    let mut w = Walker::default();
    let t = trace(&mut st, &mut sink, 1500, |f, st| {
        if f % 20 == 0 && f < 600 {
            println!(
                "f{f} fd={:#04x} x90={:#04x} x75={:#04x} y={:#04x}",
                st.peek_bus(0xFD),
                st.peek_bus(0x90),
                st.peek_bus(0x75),
                st.peek_bus(0xA2)
            );
        }
        w.input(f, st)
    });
    let n = t.ram.len();
    // Frames where the camera moved.
    let moving: Vec<usize> = (1..n)
        .filter(|&f| t.trailing_x[f] != t.trailing_x[f - 1])
        .collect();
    println!(
        "{} frames, {} with a moving scroll, final scroll x {}",
        n,
        moving.len(),
        t.trailing_x[n - 1]
    );
    // Scroll low byte vs RAM, at lags -1/0/+1 against the trailing write
    // (trailing[f] draws frame f+1).
    for (name, series) in [("trailing[f]", &t.trailing_x), ("drawn[f]", &t.drawn_x)] {
        let mut hits: Vec<(usize, usize)> = (0..t.ram[0].len())
            .map(|a| {
                let c = moving
                    .iter()
                    .filter(|&&f| i64::from(t.ram[f][a]) == series[f] & 0xFF)
                    .count();
                (c, a)
            })
            .collect();
        hits.sort_by(|a, b| b.cmp(a));
        println!(
            "{name}: top matches {:?}",
            hits.iter()
                .take(4)
                .map(|(c, a)| format!("{:#06x}:{c}/{}", addr_of(*a), moving.len()))
                .collect::<Vec<_>>()
        );
    }
    let cs = counters(&t);
    println!(
        "16-bit counters (lo,hi,wraps): {:?}",
        cs.iter()
            .map(|&(a, b, w)| format!("{:#06x}/{:#06x}:{w}", addr_of(a), addr_of(b)))
            .collect::<Vec<_>>()
    );
}

#[test]
#[ignore = "needs SMB3"]
fn smb3_walk_shots() {
    let Some((mut st, mut sink)) = open("Super Mario Bros. 3 (USA).zip") else {
        return;
    };
    script(&mut st, &mut sink, SMB3_TO_LEVEL);
    for f in 0..600u32 {
        let b = frame(&mut st, &mut sink, hop(f));
        if f % 50 == 0 {
            save_bundle(&b, &format!("walk{f:03}"));
            println!(
                "f{f} fd={:#04x} x90={:#04x} x75={:#04x}",
                st.peek_bus(0xFD),
                st.peek_bus(0x90),
                st.peek_bus(0x75)
            );
        }
    }
}

/// Bytes that hold one value on every frame in `frames` of `t`.
fn constant_over(t: &Trace, frames: std::ops::Range<usize>) -> Vec<Option<u8>> {
    (0..t.ram[0].len())
        .map(|a| {
            let v = t.ram[frames.start][a];
            frames.clone().all(|f| t.ram[f][a] == v).then_some(v)
        })
        .collect()
}

#[test]
#[ignore = "needs SMB3"]
fn smb3_state() {
    let Some((mut st, mut sink)) = open("Super Mario Bros. 3 (USA).zip") else {
        return;
    };
    script(&mut st, &mut sink, "w300 S6 w120 S6 w400 R8 w60 U8 w60");
    let map = trace(&mut st, &mut sink, 30, |_, _| 0);
    script(&mut st, &mut sink, "A8 w300");
    let mut w = Walker::default();
    let lvl = trace(&mut st, &mut sink, 700, |f, st| w.input(f, st));
    // Mario's Y low byte: find the frame he died (y wraps to 0xfb after a hit).
    let y: Vec<u8> = lvl.ram.iter().map(|r| r[0xA2]).collect();
    let dead_at = (30..y.len()).find(|&f| y[f] > 0xE0 || y[f] == 0x9f);
    println!("Y trace {:?}", y.iter().step_by(20).collect::<Vec<_>>());
    println!("first sign of death at frame {dead_at:?}");
    let alive = constant_over(&lvl, 80..215);
    let map_c = constant_over(&map, 0..30);
    let dead = constant_over(&lvl, 250..350);
    let mut map_rows = Vec::new();
    let mut dead_rows = Vec::new();
    for a in 0..0x800 {
        let (Some(al), Some(mp), Some(de)) = (alive[a], map_c[a], dead[a]) else {
            continue;
        };
        if al != mp {
            map_rows.push(format!("{a:#05x}:{mp:#04x}>{al:#04x}"));
        }
        if al != de {
            dead_rows.push(format!("{a:#05x}:{al:#04x}>{de:#04x}"));
        }
    }
    println!("map->alive ({}): {}", map_rows.len(), map_rows.join(" "));
    println!("alive->dead ({}): {}", dead_rows.len(), dead_rows.join(" "));
}

#[test]
#[ignore = "needs SMB3"]
fn smb3_death() {
    let Some((mut st, mut sink)) = open("Super Mario Bros. 3 (USA).zip") else {
        return;
    };
    script(&mut st, &mut sink, SMB3_TO_LEVEL);
    let lvl = trace(&mut st, &mut sink, 200, |_, _| BTN_RIGHT | BTN_B);
    let xy: Vec<(u8, u8)> = lvl.ram.iter().map(|r| (r[0x90], r[0xA2])).collect();
    println!(
        "x,y every 5: {:?}",
        xy.iter().step_by(5).collect::<Vec<_>>()
    );
    // Frames where x stops advancing = the hit.
    let hit = (5..xy.len())
        .find(|&f| xy[f].0 == xy[f - 1].0 && xy[f - 1].0 == xy[f - 2].0 && xy[f].0 > 0x20);
    println!("hit at {hit:?}");
    let Some(h) = hit else { return };
    let pre = constant_over(&lvl, h.saturating_sub(10)..h - 2);
    let post = constant_over(&lvl, h + 4..h + 14);
    let mut rows = Vec::new();
    for a in 0..0x800 {
        if let (Some(p), Some(q)) = (pre[a], post[a]) {
            if p != q {
                rows.push(format!("{a:#05x}:{p:#04x}>{q:#04x}"));
            }
        }
    }
    println!("pre->post ({}): {}", rows.len(), rows.join(" "));
}

fn smb3_plan(step: u16, hold: Vec<(u32, u8)>) -> ScoutPlan {
    ScoutPlan {
        strategy: ScoutStrategy::PlayerMove,
        camera: Vec::new(),
        player_x: vec![0x0090],
        player_x_hi: vec![0x0075],
        composite: true,
        step_px: step,
        frame_budget: 1500,
        stall_frames: 60,
        settle_frames: 0,
        dirs: vec![ScoutDir::Right],
        hold,
        start_when: vec![(0x000F, 1)],
    }
}

/// Reach 1-1, stand still `idle` frames (the live canvas sees one screen),
/// then scout with `plan`; prints a line every 20 scout frames.
fn scout_run(file: &str, to_level: &str, idle: u32, plan: ScoutPlan, probes: &[u32]) {
    let Some((mut st, mut sink)) = open(file) else {
        return;
    };
    script(&mut st, &mut sink, to_level);
    let mut live = CanvasAccumulator::new("probe".into(), None);
    let mut scene = None;
    for _ in 0..idle {
        let b = frame(&mut st, &mut sink, 0);
        scene = Some(live.observe_frame(&b, &[]));
    }
    let scene = scene.expect("idle frames");
    let snap = st.snapshot().expect("snapshot");
    let mut scout = Scout::start_composite(
        &retroforge::rom_open::load_rom_bytes(&rom_path(file).unwrap()).unwrap(),
        snap,
        plan,
        &live,
        scene,
        (W, H),
    )
    .expect("scout");
    let before = live.scene_canvas(scene).map(|c| (c.width(), c.height()));
    println!(
        "live canvas {before:?}, scene {scene:?}, gate $0F={}",
        st.peek_bus(0x0F)
    );
    let none = |_: &EmuStepper| None;
    for i in 0..4000u32 {
        let status = scout.step(&none);
        if probes.is_empty() || i % 20 == 0 {
            let c = scout.accumulator();
            let _ = c;
        }
        if i % 10 == 0 && i < 60 {
            println!(
                "scout f{i} x={:#06x} gate={:#04x} fd={:#04x} hi12={:#04x} ay={:#04x} frames {}",
                scout.peek_word(0x90) & 0xFF | (scout.peek_word(0x75) & 0xFF) << 8,
                scout.peek_word(0x0F) & 0xFF,
                scout.peek_word(0xFD) & 0xFF,
                scout.peek_word(0x12) & 0xFF,
                scout.peek_word(0xA2) & 0xFF,
                scout.frames_run()
            );
            println!(
                "   mm2 x={:#04x}{:02x} cam={:#04x}{:02x} y={:#04x}",
                scout.peek_word(0x440) & 0xFF,
                scout.peek_word(0x460) & 0xFF,
                scout.peek_word(0x20) & 0xFF,
                scout.peek_word(0x1f) & 0xFF,
                scout.peek_word(0x4a0) & 0xFF
            );
            println!(
                "   pos {:?} fc={:#04x} 87={:#04x}",
                scout.frame_position().map(|p| p.origin),
                scout.peek_word(0xFC) & 0xFF,
                scout.peek_word(0x87) & 0xFF
            );
        }
        if status != ScoutStatus::Running {
            break;
        }
    }
    println!("ends {:?} frames {}", scout.ends, scout.frames_run());
    let merged = scout.merge_into_composite(&mut live);
    let after = live.scene_canvas(scene).map(|c| (c.width(), c.height()));
    println!("merged {merged} cells; live canvas {before:?} -> {after:?}");
    if let Some(c) = live.scene_canvas(scene) {
        for x0 in [300i64, 600, 900, 1500] {
            let mut hist = std::collections::BTreeMap::new();
            for x in x0..x0 + 100 {
                for y in 239..430 {
                    *hist
                        .entry(
                            c.get(x, y)
                                .map(|p| (p.palette_index, format!("{:?}", p.layer))),
                        )
                        .or_insert(0u32) += 1;
                }
            }
            println!("x {x0}..+100: {hist:?}");
        }
        save_canvas(c, "scout_canvas");
    }
}

fn save_canvas(c: &rf_enhance::stitcher::Canvas, name: &str) {
    let (ox, oy) = c.origin();
    let (w, h) = (c.width(), c.height());
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let px = c.get(ox + x as i64, oy + y as i64);
            let rgb = px.map_or([255, 0, 255], |p| {
                rf_renderer::resolve_index(p.palette_index, None)
            });
            let i = (y * w + x) * 4;
            rgba[i..i + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/screens-probe");
    let path = dir.join(format!("{name}.png"));
    image::save_buffer(&path, &rgba, w as u32, h as u32, image::ColorType::Rgba8).expect("png");
    println!("SHOT {} {w}x{h} origin {:?}", path.display(), c.origin());
}

#[test]
#[ignore = "needs SMB3"]
fn smb3_scout_probe() {
    scout_run(
        "Super Mario Bros. 3 (USA).zip",
        SMB3_TO_LEVEL,
        20,
        smb3_plan(
            std::env::var("RF_STEP")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8),
            std::env::var("RF_HOLD")
                .unwrap_or_default()
                .split(',')
                .filter_map(|kv| {
                    let (a, v) = kv.split_once('=')?;
                    Some((
                        u32::from_str_radix(a, 16).ok()?,
                        u8::from_str_radix(v, 16).ok()?,
                    ))
                })
                .collect(),
        ),
        &[],
    );
}

#[test]
#[ignore = "needs SMB3"]
fn smb3_cam() {
    let Some((mut st, mut sink)) = open("Super Mario Bros. 3 (USA).zip") else {
        return;
    };
    script(&mut st, &mut sink, SMB3_TO_LEVEL);
    run(&mut st, &mut sink, 0, 20);
    let snap = st.snapshot().unwrap();
    for (name, step, input) in [("poke4", 4u32, 0)] {
        st.restore(&snap).unwrap();
        println!("== {name}");
        let mut x = 0x20u32 + 0;
        for f in 0..400u32 {
            x += step;
            if step > 0 {
                st.poke_bus(0x90, x as u8);
                st.poke_bus(0x75, (x >> 8) as u8);
            }
            st.poke_bus(0xA2, 0x28);
            let b = frame(&mut st, &mut sink, input);
            if f == 200 || f == 300 {
                save_bundle(&b, &format!("cam-{name}-{f}"));
            }
            if f % 40 == 0 {
                println!(
                    "f{f} x={:#06x} fd={:#04x} 0f={:#04x} 12={:#04x} ab={:#04x}",
                    u32::from(st.peek_bus(0x90)) | u32::from(st.peek_bus(0x75)) << 8,
                    st.peek_bus(0xFD),
                    st.peek_bus(0x0F),
                    st.peek_bus(0x12),
                    st.peek_bus(0xAB)
                );
            }
        }
    }
}

#[test]
#[ignore = "needs SMB3"]
fn smb3_wrap() {
    let Some((mut st, mut sink)) = open("Super Mario Bros. 3 (USA).zip") else {
        return;
    };
    script(&mut st, &mut sink, SMB3_TO_LEVEL);
    let mut live = CanvasAccumulator::new("probe".into(), None);
    let mut last = None;
    for f in 0..600u32 {
        st.poke_bus(0xA2, 0x10);
        let b = frame(&mut st, &mut sink, BTN_RIGHT | BTN_B);
        let scene = live.observe_frame(&b, &[]);
        let cam = i64::from(st.peek_bus(0xFD)) | i64::from(st.peek_bus(0x12)) << 8;
        if f % 25 == 0 || last != Some(scene) {
            println!(
                "f{f} cam={cam:#06x} pos {:?} scene {scene:?}",
                live.frame_position().map(|p| p.origin)
            );
            last = Some(scene);
        }
    }
}

#[test]
#[ignore = "needs SMB3"]
fn smb3_restored() {
    let Some((mut st, mut sink)) = open("Super Mario Bros. 3 (USA).zip") else {
        return;
    };
    script(&mut st, &mut sink, SMB3_TO_LEVEL);
    run(&mut st, &mut sink, 0, 20);
    let snap = st.snapshot().unwrap();
    let raw =
        retroforge::rom_open::load_rom_bytes(&rom_path("Super Mario Bros. 3 (USA).zip").unwrap())
            .unwrap();
    let mut st2 = EmuStepper::open(&raw).unwrap();
    st2.restore(&snap).unwrap();
    st2.set_event_mask(rf_core_api::EventMask::NONE);
    let mut sink2 = Sink {
        bundle: FrameBundleBuilder::new(W, H),
    };
    let mut x = 0x20u32;
    let mut acc = CanvasAccumulator::new("p".into(), None);
    let mut prev_frame: Option<FrameBundle> = None;
    for f in 0..300u32 {
        x += 4;
        st2.poke_ram(0x90, x as u8);
        st2.poke_ram(0x75, (x >> 8) as u8);
        st2.poke_ram(0xA2, 0x28);
        let b = frame(&mut st2, &mut sink2, 0);
        let sc = acc.observe_frame(&b, &[]);
        if let Some(pv) = prev_frame.take() {
            if (100..110).contains(&f) {
                let mut best = (0usize, 0i32);
                for sh in -12i32..=12 {
                    let mut m = 0usize;
                    for y in 0..190usize {
                        for xx in 20..230usize {
                            let a = pv.video[y * 256 + (xx as i32 + sh) as usize].palette_index;
                            let c = b.video[y * 256 + xx].palette_index;
                            if a == c {
                                m += 1;
                            }
                        }
                    }
                    if m > best.0 {
                        best = (m, sh);
                    }
                }
                println!(
                    "shift f{f}: best {:?} of {}, pos {:?} x90={:#x} fd={:#x}",
                    best,
                    190 * 210,
                    acc.frame_position().map(|p| p.origin.0),
                    x,
                    st2.peek_bus(0xFD)
                );
            }
        }
        prev_frame = Some(b.clone());
        if f >= 98 && f < 140 {
            let cv = acc.scene_canvas(sc).unwrap();
            println!(
                "g{f} canvas(370,400) = {:?} pos {:?}",
                cv.get(370, 400).map(|p| (p.palette_index, p.layer)),
                acc.frame_position().map(|p| p.origin)
            );
        }
        if f >= 100 && f < 104 {
            let pos = acc.frame_position().unwrap();
            let cv = acc.scene_canvas(sc).unwrap();
            let px = b.video[170 * 256 + 100];
            println!(
                "f{f} frame px (100,170)={:?}/{:?} canvas at {:?} = {:?}",
                px.palette_index,
                px.layer,
                (pos.origin.0 + 100, pos.origin.1 + 170),
                cv.get(pos.origin.0 + 100, pos.origin.1 + 170)
                    .map(|p| (p.palette_index, p.layer))
            );
        }
        if f % 50 == 0 {
            let nb = b
                .video
                .iter()
                .filter(|p| matches!(p.layer, rf_core_api::PixelLayer::Background(_)))
                .count();
            let nd = b
                .video
                .iter()
                .filter(|p| matches!(p.layer, rf_core_api::PixelLayer::Backdrop))
                .count();
            println!(
                "f{f} scene {sc:?} pos {:?} bg px {nb} backdrop px {nd} events {}",
                acc.frame_position(),
                events_line(&b)
            );
        }
        if f == 100 || f == 250 {
            save_bundle(&b, &format!("restored-{f}"));
        }
    }
    let c = acc.scene_canvas(acc.current_scene_id().unwrap()).unwrap();
    for x0 in [0i64, 100, 200, 250, 256, 280, 300] {
        let mut hist = std::collections::BTreeMap::new();
        for x in x0..x0 + 100 {
            for y in 239..430 {
                *hist
                    .entry(
                        c.get(x, y)
                            .map(|p| (p.palette_index, format!("{:?}", p.layer))),
                    )
                    .or_insert(0u32) += 1;
            }
        }
        println!("direct x {x0}..+100: {} kinds", hist.len());
    }
    let mut runs: Vec<(i64, String)> = Vec::new();
    for y in -5..440 {
        let k = format!("{:?}", c.get(150, y).map(|p| (p.palette_index, p.layer)));
        if runs.last().map(|r| &r.1) != Some(&k) {
            runs.push((y, k));
        }
    }
    println!(
        "col150 runs {:?} origin {:?} size {}x{}",
        runs,
        c.origin(),
        c.width(),
        c.height()
    );
    save_canvas(c, "restored_canvas");
}

const MM2_TO_AIRMAN: &str = "w400 S6 w120 S6 w300 S6 w300 U8 w20 S6 w800";
/// Menus, stage select, Air Man, and the stage intro (Mega Man standing on the first platform).
const MM2_TO_AIRMAN_SHORT: &str = "w400 S6 w120 S6 w300 S6 w300 U8 w20 S6 w800";
const MM2: &str = "Mega Man 2 (USA).zip";

#[test]
#[ignore = "needs Mega Man 2"]
fn mm2_ram() {
    let Some((mut st, mut sink)) = open(MM2) else {
        return;
    };
    script(&mut st, &mut sink, MM2_TO_AIRMAN);
    let t = trace(&mut st, &mut sink, 900, |f, _| {
        BTN_RIGHT | if f % 50 < 12 { BTN_A } else { 0 }
    });
    let n = t.ram.len();
    let moving: Vec<usize> = (1..n)
        .filter(|&f| t.trailing_x[f] != t.trailing_x[f - 1])
        .collect();
    println!(
        "{n} frames, {} with a moving scroll, final x {}",
        moving.len(),
        t.trailing_x[n - 1]
    );
    for (name, series) in [("trailing[f]", &t.trailing_x), ("drawn[f]", &t.drawn_x)] {
        let mut hits: Vec<(usize, usize)> = (0..t.ram[0].len())
            .map(|a| {
                (
                    moving
                        .iter()
                        .filter(|&&f| i64::from(t.ram[f][a]) == series[f] & 0xFF)
                        .count(),
                    a,
                )
            })
            .collect();
        hits.sort_by(|a, b| b.cmp(a));
        println!(
            "{name}: {:?}",
            hits.iter()
                .take(4)
                .map(|(c, a)| format!("{:#06x}:{c}/{}", addr_of(*a), moving.len()))
                .collect::<Vec<_>>()
        );
    }
    for f in (0..n).step_by(60) {
        let r = &t.ram[f];
        println!("f{f} hp={:#04x} scroll {} 1f={:#04x} 20={:#04x} 460={:#04x} 440={:#04x} 4a0={:#04x} 1b={:#04x} 22={:#04x}", r[0x6c0], t.trailing_x[f], r[0x1f], r[0x20], r[0x460], r[0x440], r[0x4a0], r[0x1b], r[0x22]);
    }
    let alive = constant_over(&t, 130..170);
    let dead = constant_over(&t, 330..440);
    let sel = constant_over(&t, 560..660);
    let mut rows = Vec::new();
    for a in 0..0x800 {
        if let (Some(x), Some(d), Some(z)) = (alive[a], dead[a], sel[a]) {
            if x != d {
                rows.push(format!("{a:#05x}:{x:#04x}>dead{d:#04x}/sel{z:#04x}"));
            }
        }
    }
    println!("alive vs dead: {}", rows.join(" "));
}

fn mm2_plan(step: u16, hold: Vec<(u32, u8)>) -> ScoutPlan {
    let cam = std::env::var("RF_CAM").is_ok();
    ScoutPlan {
        strategy: if cam {
            ScoutStrategy::CameraWrite
        } else {
            ScoutStrategy::Walk
        },
        camera: if cam { vec![0x1F] } else { Vec::new() },
        player_x: vec![0x0460],
        player_x_hi: vec![0x0440],
        composite: true,
        step_px: step,
        frame_budget: 3000,
        stall_frames: 60,
        settle_frames: 0,
        dirs: vec![ScoutDir::Right],
        hold,
        start_when: vec![(0x06C0, 0x1C)],
    }
}

fn env_hold() -> Vec<(u32, u8)> {
    std::env::var("RF_HOLD")
        .unwrap_or_default()
        .split(',')
        .filter_map(|kv| {
            let (a, v) = kv.split_once('=')?;
            Some((
                u32::from_str_radix(a, 16).ok()?,
                u8::from_str_radix(v, 16).ok()?,
            ))
        })
        .collect()
}

#[test]
#[ignore = "needs Mega Man 2"]
fn mm2_scout_probe() {
    let step = std::env::var("RF_STEP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    scout_run(
        MM2,
        MM2_TO_AIRMAN_SHORT,
        20,
        mm2_plan(step, env_hold()),
        &[],
    );
}

#[test]
#[ignore = "needs SMB3"]
fn smb3_gate() {
    let Some((mut st, mut sink)) = open("Super Mario Bros. 3 (USA).zip") else {
        return;
    };
    script(&mut st, &mut sink, "w300 S6 w120 S6 w400 R8 w60 U8 w60 A8");
    let mut line = String::new();
    for f in 0..320u32 {
        drop(frame(&mut st, &mut sink, 0));
        if f % 10 == 0 {
            line += &format!("{f}:{:x}/{:x} ", st.peek_bus(0x0F), st.peek_bus(0x90));
        }
    }
    println!("gate$0F/x90 after A: {line}");
}
