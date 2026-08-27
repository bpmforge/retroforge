//! DIAGNOSTIC (ticket W7-08): is `spc_dsp6`'s remaining failure a wrong
//! per-cycle PLACEMENT, or a wrong ALIGNMENT between the SPC700 and the
//! DSP?
//!
//! The distinction matters and is cheap to settle. `Apu::step_counted`
//! executes a whole instruction and only then advances the DSP by that
//! instruction's cycle cost, so a `$F3` read inside the instruction sees
//! the DSP as it stood BEFORE the instruction began — a systematic lag of
//! up to five cycles. blargg's failing check counts how many reads return
//! a written value before the DSP overwrites it, which is exactly the
//! quantity such a lag perturbs.
//!
//! So: run the ROM at all 32 starting phases. If some phase passes, the
//! schedule is right and only the CPU/DSP alignment is off. If none does,
//! the remaining gap is structural and a phase shift cannot hide it.
//!
//! Ignored by default — this is an investigation, not an assertion.

use rf_snes::SnesSystem;

const STATUS_WORD: u16 = 0x0B60;
const STATUS_LEN: usize = 32 * 20;
const MAX_INSTRUCTIONS: u64 = 60_000_000;

fn rom() -> Option<std::path::PathBuf> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../roms/snes/blargg-spc-6/spc_dsp6.sfc");
    p.exists().then_some(p)
}

fn screen(s: &SnesSystem) -> String {
    let raw: String = s
        .bus
        .vram_low_bytes(STATUS_WORD, STATUS_LEN)
        .iter()
        .map(|&b| {
            if (0x20..0x7F).contains(&b) {
                b as char
            } else {
                ' '
            }
        })
        .collect();
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
#[ignore = "diagnostic: sweeps DSP start phase against spc_dsp6"]
fn sweep_the_dsp_start_phase() {
    let Some(path) = rom() else {
        eprintln!("SKIP: spc_dsp6.sfc not fetched");
        return;
    };
    let bytes = std::fs::read(&path).expect("rom readable");

    for phase in 0..32u16 {
        let mut s = SnesSystem::load(&bytes).expect("LoROM");
        // Offset the DSP within its own loop before the ROM starts.
        for _ in 0..phase {
            let mut aram = std::mem::take(&mut s.bus.apu.aram);
            s.bus.apu.dsp.tick(&mut aram);
            s.bus.apu.aram = aram;
        }
        let mut ran = 0u64;
        // `ran` advances unconditionally on every path (law 8).
        while ran < MAX_INSTRUCTIONS {
            if s.step().is_err() {
                break;
            }
            ran += 1;
        }
        println!("phase {phase:2}: {}", screen(&s));
    }
}
