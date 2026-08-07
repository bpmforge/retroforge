//! Ticket W4-01: measure whether `FrameBundle` assembly (the third leg of
//! `core_thread::FanoutSink`, alongside the pre-existing accuracy/layer
//! sinks) costs enough to threaten W2-18's 16.64ms (60.0988 Hz NTSC) frame
//! budget — "measure it through the real core thread rather than
//! assuming" (this ticket's own instructions, citing W3-03a's precedent:
//! an earlier unconditional per-frame cost, two ~240KB layer-buffer
//! clones, was accepted specifically because it was MEASURED to fit the
//! budget, not assumed to).
//!
//! `#[ignore]`'d like `determinism.rs`'s 10k-frame suite: this drives a
//! real wall-clock core thread with real pacing, so it is a timing
//! measurement, not a hermetic correctness test — it belongs in the "5-min
//! soak / perf benches" tier (TESTING.md), not the default `cargo test
//! --workspace` run. Run directly with:
//!   cargo test -p retroforge --test frame_bundle_perf -- --ignored --nocapture
//!   cargo test --release -p retroforge --test frame_bundle_perf -- --ignored --nocapture
use std::time::{Duration, Instant};

use retroforge::core_thread::{self, CoreCommand, CoreEvent};

const INES_MAGIC: [u8; 4] = [0x4E, 0x45, 0x53, 0x1A]; // "NES\x1A"

/// Same many-sprites fixture `retroforge::mode_invariant`/
/// `sprite_overlay_mode_invariant.rs` use — a real per-scanline rendering
/// workload (10 sprites drawn every frame), not the all-`BRK`-forever
/// synthetic NROM `core_thread.rs`'s own tests use, so this measures a
/// realistic `FanoutSink`/`FrameBundleBuilder` cost rather than an
/// all-backdrop no-op frame.
#[rustfmt::skip]
const PROGRAM: &[(u16, &[u8])] = &[
    (0x8000, &[0x78]),
    (0x8001, &[0xA2, 0xFF]),
    (0x8003, &[0x9A]),
    (0x8004, &[0xA9, 0x00]),
    (0x8006, &[0x8D, 0x03, 0x20]),
    (0x8009, &[0xA2, 0x00]),
    (0x800B, &[0xBD, 0x1E, 0x80]),
    (0x800E, &[0x8D, 0x04, 0x20]),
    (0x8011, &[0xE8]),
    (0x8012, &[0xE0, 0x28]),
    (0x8014, &[0xD0, 0xF5]),
    (0x8016, &[0xA9, 0x10]),
    (0x8018, &[0x8D, 0x01, 0x20]),
    (0x801B, &[0x4C, 0x1B, 0x80]),
];
const SPRITE_TABLE_ADDR: u16 = 0x801E;

fn sprite_table() -> Vec<u8> {
    let mut table = Vec::with_capacity(40);
    for i in 0u8..10 {
        table.extend_from_slice(&[50, 1, 0, 8 + i * 9]);
    }
    table
}

fn many_sprites_rom() -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&INES_MAGIC);
    data.push(1);
    data.push(1);
    data.extend_from_slice(&[0u8; 10]);

    let mut prg = vec![0u8; 16 * 1024];
    for (addr, bytes) in PROGRAM {
        let offset = (*addr - 0x8000) as usize;
        prg[offset..offset + bytes.len()].copy_from_slice(bytes);
    }
    let table = sprite_table();
    let table_offset = (SPRITE_TABLE_ADDR - 0x8000) as usize;
    prg[table_offset..table_offset + table.len()].copy_from_slice(&table);
    prg[0x3FFC] = 0x00;
    prg[0x3FFD] = 0x80;
    data.extend(prg);

    let mut chr = vec![0u8; 8 * 1024];
    for row in 0..8 {
        chr[16 + row] = 0xFF;
        chr[16 + 8 + row] = 0x00;
    }
    data.extend(chr);
    data
}

#[test]
#[ignore = "wall-clock perf measurement, run manually or pre-phase-gate (W4-01)"]
fn measure_fps_with_frame_bundle_assembly_enabled() {
    let core =
        core_thread::spawn(many_sprites_rom()).expect("fixture ROM must spawn a core thread");
    core.cmd_tx
        .send(CoreCommand::Resume)
        .expect("core thread must accept Resume");

    // Warm up (JIT/cache effects, pacer resync) before starting the
    // measurement window, same shape as any steady-state throughput
    // benchmark.
    let warmup = Duration::from_millis(500);
    let measure_window = Duration::from_secs(3);
    let start = Instant::now();
    let mut warm_at: Option<Instant> = None;
    let mut count_after_warmup: u64 = 0;

    loop {
        match core.evt_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(CoreEvent::Frame(_)) => {
                let now = Instant::now();
                match warm_at {
                    None if now.duration_since(start) >= warmup => warm_at = Some(now),
                    None => {}
                    Some(w) => {
                        count_after_warmup += 1;
                        if now.duration_since(w) >= measure_window {
                            break;
                        }
                    }
                }
            }
            Ok(CoreEvent::Crashed(r)) => {
                panic!("core thread crashed mid-measurement: {}", r.message)
            }
            Err(_) => panic!("core thread stalled -- no frame within 2s"),
        }
    }

    let elapsed = warm_at
        .expect("warmup window must have completed")
        .elapsed();
    let fps =
        f64::from(u32::try_from(count_after_warmup).unwrap_or(u32::MAX)) / elapsed.as_secs_f64();
    println!(
        "measured fps with FrameBundle assembly enabled ({} build): {fps:.2}",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );

    // NTSC target is 60.0988 Hz (crate::pacer::NTSC_FRAME_PERIOD). W3-03's
    // own precedent measured 60.0/60.2 as "fully absorbed inside the
    // budget" -- a generous floor here (58 Hz) fails loudly if assembly
    // ever meaningfully eats into the budget, without being flaky over a
    // couple percent of scheduler jitter on a shared CI runner.
    assert!(
        fps > 58.0,
        "fps dropped to {fps:.2}, well under the ~60 Hz NTSC target -- FrameBundle assembly \
         may be spending real frame budget, see this ticket's perf-measurement instructions"
    );

    let _ = core.cmd_tx.send(CoreCommand::Shutdown);
    let _ = core.join_handle.join();
}
