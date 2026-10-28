//! W38-06 probe (scratch): the trace stage of the recipe generator.
mod w38_06_common;

use rf_core_api::{EmulatorCore, ResetKind};
use rf_harness::object_tables::{snes_cam, snes_step_frame, Console, Recorder};
use rf_rebuild::objtable::{infer as infer_tables, Trace};
use rf_rebuild::widescreen::{camera_aliases, find_camera, find_x_page, oam_shadow, static_sprites};
use rf_snes::core::SnesCore;
use w38_06_common::*;

fn trace_of(raw: &[u8]) -> Trace {
    let ins = inputs_of(&script());
    let from = play_from();
    let mut core = SnesCore::load(raw).expect("loads");
    core.reset(ResetKind::Hard);
    let mut rec = Recorder::new(Console::Snes, false);
    for (f, input) in ins.iter().enumerate() {
        if f >= from {
            let cap = snes_step_frame(&mut core, input);
            rec.push(cap, snes_cam(&core));
        } else {
            core.system_mut().bus.joypads.ports[0] = input.ports[0];
            core.run_frame(input, &mut NullSink);
        }
    }
    rec.trace
}

#[test]
#[ignore = "scratch probe; needs Super Metroid"]
fn probe_trace() {
    let Some(path) = rom_path() else { return };
    let raw = load_rom(&path);
    let t = std::time::Instant::now();
    let trace = trace_of(&raw);
    println!("trace {} frames in {:?}", trace.frames.len(), t.elapsed());
    let found = infer_tables(&trace);
    println!("{} tables", found.len());
    for f in found.iter().take(5) {
        let tb = &f.table;
        println!(
            "TABLE base {:#x} slots {} stride {} x_lo {:?} x_hi {:?} y {:?} y_hi {:?} state {:?} z {:.0} rate {:.3} expected {} live_from_state {}",
            tb.base, tb.slots, tb.stride, tb.x_lo, tb.x_hi, tb.y, tb.y_hi, tb.state, f.z, f.binding.rate(), f.binding.expected, f.binding.live_from_state
        );
    }
    let cam = find_camera(&trace);
    println!("camera {cam:?}");
    if let Some(c) = &cam {
        println!("aliases {:x?}", camera_aliases(&trace, c));
        if let Some(f) = found.first() {
            println!("x page {:?}", find_x_page(&f.table, c, &trace));
        }
    }
    let statics = static_sprites(&trace);
    println!(
        "static sprites {:?}; oam shadow {:?}",
        statics.iter().map(|s| (s.index, s.x, s.y)).collect::<Vec<_>>(),
        oam_shadow(&trace)
    );
}
