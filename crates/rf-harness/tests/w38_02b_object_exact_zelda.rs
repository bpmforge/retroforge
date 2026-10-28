//! **W38-02b: the object-table finder on The Legend of Zelda** (ROM-gated,
//! law 5). The W38-02 finder missed this game's actor table because the hero's
//! sprite sits two pixels below its RAM y while the other actors' do not; the
//! finder now learns the offsets per slot ([`rf_rebuild::objtable`]). Plays the
//! scripted run of `w38_05_zelda_common`; only offsets, strides and counts are
//! printed (D-021).
//!
//! ```text
//! cargo test --release -p rf-harness --test w38_02b_object_exact_zelda -- --ignored --nocapture
//! ```

mod w38_05_zelda_common;

use rf_core_api::{EmulatorCore, ResetKind};
use rf_harness::object_tables::{nes_cam, nes_prepare, nes_step_frame, Console, Recorder};
use rf_nes::core::NesCore;
use rf_rebuild::objtable::{grade_of, infer, to_row};
use w38_05_zelda_common::{inputs, load_rom, rom_path, FILE, FRAMES, PLAY_FROM};

#[test]
#[ignore = "needs The Legend of Zelda (USA) (RF_SCREENS_ROMS_DIR)"]
fn zelda_actor_table_matches_oam() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let mut core = NesCore::from_ines_bytes(&raw).expect("loads");
    core.reset(ResetKind::Hard);
    nes_prepare(&mut core);
    let mut rec = Recorder::new(Console::Nes, false);
    for (f, input) in inputs(FRAMES).iter().enumerate() {
        let cap = nes_step_frame(&mut core, input);
        if f as u32 >= PLAY_FROM {
            rec.push(cap, nes_cam(&core));
        }
    }
    let trace = rec.trace;
    if let Ok(p) = std::env::var("RF_OBJ_TRACE_OUT") {
        rf_harness::object_tables::save_trace(&trace, Console::Nes, std::path::Path::new(&p))
            .expect("saves");
    }
    println!("sampled {} frames", trace.frames.len());
    let found = infer(&trace);
    println!("inferred {} tables", found.len());
    for f in &found {
        let row = to_row(f, grade_of(f), "; Zelda");
        let b = &f.binding;
        println!(
            "TABLE base {:#x} slots {} stride {} x_lo {:?} x_hi {:?} y {:?} y_hi {:?} kind {:?} state {:?} z {:.0}",
            row.base, row.slots, row.stride, row.x_lo, row.x_hi, row.y, row.y_hi, row.kind, row.state, f.z
        );
        println!(
            "  lag {} dx {} dy {} expected {} matched {} rate {:.4} frames {} clean {} from_state {} grade {:?}",
            b.lag, b.dx, b.dy, b.expected, b.matched, b.rate(), b.frames_checked, b.frames_clean, b.live_from_state, row.grade
        );
        println!(
            "  misses: undrawn {} off_set {} excluded {} (blank frames {}) non_drawn {:?}",
            b.undrawn, b.off_set, b.excluded, b.blank_frames, b.non_drawn
        );
        for s in &b.slots {
            println!(
                "  slot {} live {} matched {} control {} back {} offsets {}",
                s.slot,
                s.live_frames,
                s.matched_frames,
                s.control_frames,
                s.back,
                s.offsets.len()
            );
        }
    }
    // The actor table the W38-05 recipe committed by hand: x bytes at $70..,
    // y bytes at $84.. (hero first). The finder must now produce it unaided.
    let (rank, hit) = found
        .iter()
        .enumerate()
        .find(|(_, f)| {
            let t = &f.table;
            t.stride == 1
                && t.slots >= 5
                && t.x_lo.map(|o| t.base + o) == Some(0x70)
                && t.y.map(|o| t.base + o) == Some(0x84)
        })
        .expect("the actor table ($70 x, $84 y) was found");
    println!(
        "ACTOR TABLE rank {rank} of {} slots {} rate {:.4}",
        found.len(),
        hit.table.slots,
        hit.binding.rate()
    );
    assert!(hit.binding.rate() >= 0.95, "{:?}", hit.binding);
    // The hero's sprite is drawn lower than its RAM y; the others' are not.
    let hero = &hit.binding.slots[0];
    let other = &hit.binding.slots[1];
    assert_ne!(hero.offsets, other.offsets, "{hero:?} {other:?}");
}
