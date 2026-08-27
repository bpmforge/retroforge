//! SETTLED (ticket W7-08, 2026-08-27): `spc_dsp6`'s failure was NOT a
//! CPU/DSP alignment problem. Kept so nobody re-runs the refactor this
//! ruled out.
//!
//! THE HYPOTHESIS. `Apu::step_counted` executes a whole instruction and
//! only then advances the DSP by that instruction's cycle cost, so a
//! `$F3` read inside the instruction sees the DSP as it stood BEFORE the
//! instruction began — a systematic lag of up to five cycles. blargg's
//! failing check counts how many reads return a written value before the
//! DSP overwrites it, which is exactly the quantity such a lag perturbs.
//! The obvious next move was sub-instruction bus timing in the SPC700:
//! days of work, and a large diff through the CPU core.
//!
//! THE TEST. Run the ROM at all 32 starting phases of the DSP's sample
//! loop. If any phase passed, alignment was the whole story and only the
//! offset needed fixing.
//!
//! THE RESULT, 2026-08-27: all 32 phases failed identically with
//! "Echo/basics Failed 03". Alignment could not be the cause, because no
//! alignment existed that fixed it. The real causes were three echo
//! details read live where hardware latches them — EDL's four-bit width,
//! the ring length latched at the wrap, and ESA latched at cycle 29 —
//! and with those fixed the ROM passes. See commit "ESA and EDL are
//! LATCHED, EDL is four bits".
//!
//! WHY IT IS STILL HERE. The sub-instruction refactor is a plausible
//! thing to reach for the next time an SPC ROM disagrees about timing.
//! This sweep is the cheap way to find out whether it would help before
//! spending the days: re-run it, and if every phase still agrees, the
//! problem is structural and lives somewhere else.
//!
//! Ignored by default. It builds 32 systems and runs each to 60M
//! instructions.

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
#[ignore = "diagnostic: 32 full ROM runs; see the module doc for what it settled"]
fn dsp_start_phase_does_not_change_the_verdict() {
    let Some(path) = rom() else {
        eprintln!("SKIP: spc_dsp6.sfc not fetched");
        return;
    };
    let bytes = std::fs::read(&path).expect("rom readable");
    let mut results = Vec::new();

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
        let verdict = screen(&s);
        println!("phase {phase:2}: {verdict}");
        results.push((phase, verdict));
    }

    // The invariant, now that the ROM passes: every phase must agree.
    // A future change that makes the verdict depend on where the DSP
    // happened to start is a real bug, and this is the only test that
    // would see it.
    let (_, first) = &results[0];
    for (phase, verdict) in &results {
        assert_eq!(
            verdict, first,
            "phase {phase} disagrees with phase 0 — the verdict must not \
             depend on the DSP's starting cycle"
        );
    }
    assert!(
        first.contains("Passed"),
        "spc_dsp6 should pass at every phase, got {first:?}"
    );
}
