//! Scratch probe (ROM-gated): prints what the finders see in the forest run.
mod w38_08_mm2_common;
use rf_core_api::{EmulatorCore, ResetKind};
use rf_harness::object_tables::{nes_cam, nes_prepare, nes_step_frame, Console, Recorder};
use rf_nes::core::NesCore;
use rf_rebuild::objtable::{grade_of, infer, to_row};
use rf_rebuild::widescreen::{find_camera, oam_shadow, static_sprites};
use rf_rebuild::objtable::{Frame, Trace};
use w38_08_mm2_common::*;

#[test]
#[ignore = "probe"]
fn probe() {
    let raw = load_rom(&rom_path().expect("rom"));
    let ins = inputs_of(&forest_script());
    let mut core = NesCore::from_ines_bytes(&raw).unwrap();
    core.reset(ResetKind::Hard);
    nes_prepare(&mut core);
    let mut rec = Recorder::new(Console::Nes, false);
    for (f, input) in ins.iter().enumerate() {
        poke(f, &mut core);
        let cap = nes_step_frame(&mut core, input);
        if f as u32 >= PLAY_FROM { rec.push(cap, nes_cam(&core)); }
    }
    let trace = rec.trace;
    println!("trace {} frames", trace.frames.len());
    let found = infer(&trace);
    println!("inferred {} tables", found.len());
    for f in &found {
        let row = to_row(f, grade_of(f), "; MM2");
        let b = &f.binding;
        println!("TABLE base {:#x} slots {} stride {} x_lo {:?} x_hi {:?} y {:?} y_hi {:?} kind {:?} state {:?} z {:.0} rate {:.4} lag {} dx {} dy {} grade {:?}",
            row.base, row.slots, row.stride, row.x_lo, row.x_hi, row.y, row.y_hi, row.kind, row.state, f.z, b.rate(), b.lag, b.dx, b.dy, row.grade);
    }
    for c in columns(&trace, false, 0.25).iter().chain(columns(&trace, true, 0.25).iter()) {
        println!("COLUMN start {:#x} len {} shares {:.2?} deltas {:?}", c.start, c.shares.len(), c.shares, c.deltas);
    }
    println!("scroll {:x?}", scroll_x_bytes(&trace));
    for (a, ch) in screen_index(&trace) { println!("IDX {a:#x} {} changes", ch.len()); }
    // Input differential: bytes that step by +1..+3 while Right is held (and nothing else changes it).
    let pad: Vec<u8> = forest_script();
    let n = trace.frames.len();
    let off = PLAY_FROM as usize;
    let mut cands: Vec<(f64, f64, f64, usize)> = Vec::new();
    for a in 0..0x800usize {
        let (mut r_up, mut r_n, mut l_dn, mut l_n, mut idle_ch, mut idle_n) = (0, 0, 0, 0, 0, 0);
        for i in 1..n {
            let pd = pad[off + i];
            let d = trace.frames[i].ram[a].wrapping_sub(trace.frames[i - 1].ram[a]);
            if pd & 128 != 0 && pd & 64 == 0 { r_n += 1; if (1..=3).contains(&d) { r_up += 1; } }
            else if pd & 64 != 0 && pd & 128 == 0 { l_n += 1; if (253..=255).contains(&d) { l_dn += 1; } }
            else if pd & 0xC0 == 0 { idle_n += 1; if d != 0 { idle_ch += 1; } }
        }
        let rs = f64::from(r_up) / f64::from(r_n.max(1));
        let ls = f64::from(l_dn) / f64::from(l_n.max(1));
        let is = f64::from(idle_ch) / f64::from(idle_n.max(1));
        cands.push((rs, ls, is, a));
    }
    cands.sort_by(|x, y| (y.0 + y.1 - 3.0 * y.2).partial_cmp(&(x.0 + x.1 - 3.0 * x.2)).unwrap());
    for (rs, ls, is, a) in cands.iter().take(12) { println!("XBYTE {a:#x} right-steps {rs:.2} left-steps {ls:.2} idle-change {is:.3}"); }
    for fi in [900usize, 1500] {
        let fr = &trace.frames[fi];
        println!("FRAME {fi} cam {} sprites {:?}", fr.cam_x, fr.sprites.iter().map(|s| (s.x, s.y)).collect::<Vec<_>>());
        for base in (0x400..0x500usize).step_by(32) { println!("RAM {base:#x} {:02x?}", &fr.ram[base..base + 32]); }
        for base in (0x1c..0x28usize).step_by(12) { println!("RAM {base:#x} {:02x?}", &fr.ram[base..base + 12]); }
    }
    let n_f = trace.frames.len();
    let usage: Vec<f64> = (0..0x800usize).map(|a| share(&trace.frames, |f| f.ram[a] != 0)).collect();
    let px = 0x460usize;
    let corr = |pp: usize| -> f64 {
        let (lo, hi) = (px.saturating_sub(0x60), (px + 0x60).min(0x7ff - pp));
        let n = (hi - lo) as f64;
        let (mut sx, mut sy, mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for a in lo..hi { let (x, y) = (usage[a], usage[a + pp]); sx += x; sy += y; sxx += x * x; syy += y * y; sxy += x * y; }
        let cov = sxy / n - sx / n * sy / n;
        let vx = sxx / n - (sx / n).powi(2); let vy = syy / n - (sy / n).powi(2);
        cov / (vx * vy).sqrt().max(1e-9)
    };
    for pp in [8usize, 16, 32, 64] { println!("PERIOD {pp} corr {:.3}", corr(pp)); }
    let pitch = 32usize;
    for k in -4i32..=8 {
        let a = (px as i32 + k * pitch as i32) as usize;
        // slot 0 only: y match and x-high wrap behaviour
        let mut best = (0.0f64, 0i32);
        for dy in -40i32..=40 {
            let s = share(&trace.frames, |f| f.sprites.iter().any(|sp| i32::from(sp.y) == i32::from(f.ram[a]) + dy));
            if s > best.0 { best = (s, dy); }
        }
        let wraps: Vec<i32> = trace.frames.windows(2).filter(|w| (w[0].ram[px] as i32 - w[1].ram[px] as i32).abs() > 128).map(|w| i32::from(w[1].ram[a]) - i32::from(w[0].ram[a])).collect();
        println!("ARRAY k {k} addr {a:#x} usage slot0 {:.2} slot_nz_count {} y-share {:.2} dy {} wraps {} steps {:?}", usage[a], (0..pitch).filter(|i| usage[a + i] > 0.0).count(), best.0, best.1, wraps.len(), &wraps[..wraps.len().min(6)]);
    }
    let _ = n_f;
    println!("camera {:?}", find_camera(&trace));
    println!("statics {:?} shadow {:?}", static_sprites(&trace).iter().map(|s| (s.index, s.x, s.y)).collect::<Vec<_>>(), oam_shadow(&trace));
}

/// Share of `frames` for which `pred` holds.
fn share(frames: &[Frame], pred: impl Fn(&Frame) -> bool) -> f64 {
    frames.iter().filter(|f| pred(f)).count() as f64 / frames.len().max(1) as f64
}

/// A run of consecutive RAM bytes (stride 1) that line up with the picture.
#[derive(Debug, Clone)]
struct Column {
    start: usize,
    /// Per byte: share of frames it matched a sprite coordinate.
    shares: Vec<f64>,
    /// Per byte: the constant added to the byte to give the sprite coordinate.
    deltas: Vec<i32>,
}

/// Runs of at least three bytes, each matching some sprite's `coord` (plus a
/// small constant) in at least `min` of the frames. RAM that mirrors the OAM
/// (a sprite buffer) is skipped.
fn columns(t: &Trace, y: bool, min: f64) -> Vec<Column> {
    let n = t.frames[0].ram.len();
    let mirror = |a: usize| {
        share(&t.frames, |f| {
            f.oam.get(a & 0xFF).copied() == Some(f.ram[a])
        }) > 0.9
    };
    let best = |a: usize| -> (f64, i32) {
        let deltas: &[i32] = if y { &[-3, -2, -1, 0, 1, 2, 3] } else { &[0] };
        deltas
            .iter()
            .map(|&d| {
                let s = share(&t.frames, |f| {
                    f.sprites.iter().any(|sp| {
                        let c = if y { sp.y } else { sp.x };
                        i32::from(c) == i32::from(f.ram[a]) + d
                    })
                });
                (s, d)
            })
            .fold((0.0, 0), |b, c| if c.0 > b.0 { c } else { b })
    };
    let scored: Vec<(f64, i32)> = (0..n)
        .map(|a| if mirror(a) { (0.0, 0) } else { best(a) })
        .collect();
    let mut out = Vec::new();
    let mut a = 0;
    while a < n {
        // Progress: `a` advances on every path below.
        if scored[a].0 < min {
            a += 1;
            continue;
        }
        let start = a;
        while a < n && scored[a].0 >= min {
            a += 1;
        }
        if a - start >= 3 {
            out.push(Column {
                start,
                shares: scored[start..a].iter().map(|s| s.0).collect(),
                deltas: scored[start..a].iter().map(|s| s.1).collect(),
            });
        }
    }
    out
}

/// The byte that only ever moves by 1 or 16 (mod 256), at least three times,
/// in both sizes: the screen index of a grid map.
type Change = (usize, u8, u8);

fn screen_index(t: &Trace) -> Vec<(usize, Vec<Change>)> {
    let n = t.frames[0].ram.len();
    let mut out = Vec::new();
    for a in 0..n {
        let ch: Vec<Change> = t
            .frames
            .windows(2)
            .enumerate()
            .filter(|(_, w)| w[0].ram[a] != w[1].ram[a])
            .map(|(i, w)| (i + 1, w[0].ram[a], w[1].ram[a]))
            .collect();
        let step = |c: &Change| c.2.wrapping_sub(c.1);
        if ch.len() >= 3
            && ch.len() <= 24
            && ch.iter().all(|c| matches!(step(c), 1 | 255 | 16 | 240))
            && ch.iter().any(|c| matches!(step(c), 1 | 255))
            && ch.iter().any(|c| matches!(step(c), 16 | 240))
        {
            out.push((a, ch));
        }
    }
    out
}

/// The byte that equals the low eight bits of the playfield scroll in at least
/// 90% of the frames in which the scroll moved horizontally.
fn scroll_x_bytes(t: &Trace) -> Vec<(usize, usize, usize)> {
    let moved: Vec<usize> = (1..t.frames.len())
        .filter(|&i| t.frames[i].cam_x != t.frames[i - 1].cam_x)
        .collect();
    if moved.len() < 20 {
        return Vec::new();
    }
    let n = t.frames[0].ram.len();
    (0..n)
        .filter_map(|a| {
            let hit = moved
                .iter()
                .filter(|&&i| {
                    let d = t.frames[i].ram[a].wrapping_sub((t.frames[i].cam_x & 0xFF) as u8);
                    d <= 9 || d >= 247
                })
                .count();
            (hit * 10 >= moved.len() * 9).then_some((a, hit, moved.len()))
        })
        .collect()
}

