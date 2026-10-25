//! Ticket W30-07: run-ahead (1-2 frames), the presentation half.
//!
//! Many games take a frame or two to react to a button press. Run-ahead
//! hides that: after the REAL frame has run exactly as it always does, the
//! real machine is snapshotted (a read-only act), the snapshot is loaded
//! into a second, private "shadow" machine of the same ROM, and the shadow
//! runs `N` more frames with the same held input into a throwaway sink. The
//! last of those frames is the picture to show.
//!
//! **The real machine is never written to**, and that is the whole point of
//! the shadow. The first design restored the real machine from its own
//! snapshot after speculating; it hashed identically but the audio drifted,
//! because a save state deliberately omits the audio decimator's phase
//! (rf-nes `Apu::save_state`), so every restore nudged the real timeline's
//! sound. With a shadow, the simulation, its audio, its events and
//! everything the shell derives from them (rewind ring, canvas accumulator,
//! replays) see only the real timeline (law 4). Only the picture is
//! borrowed from the future.
//!
//! The whole thing runs on the core thread, which is the only thread that
//! may touch either machine (ARCHITECTURE section 6).
//!
//! What a speculative frame does NOT get: de-flicker. The sprite historian
//! is a stateful observer that must only ever see real frames, and cannot
//! be copied, so the core thread skips run-ahead while it is on rather than
//! show a flickering speculative picture.

use rf_core_api::{FrameBundleBuilder, InputFrame, OverlayPixel};
use rf_renderer::FrameBuffer;

use crate::core_thread::{hd_mode7_frame, FanoutSink, HdMode7Frame};
use crate::stepper::EmuStepper;

/// The most frames ahead the player can ask for.
pub const MAX_RUN_AHEAD: u8 = 2;

/// The picture run-ahead wants shown in place of the real one.
pub struct SpecFrame {
    /// Resolved RGBA of the last speculative frame, same size as the real
    /// frame it replaces.
    pub rgba: Vec<u8>,
    /// Per-row palettes of that frame (the SNES line palette table).
    pub line_palettes: Vec<Option<rf_renderer::LinePalette>>,
    /// The HD Mode 7 composite of that frame, when HD Mode 7 is on.
    pub hd_mode7: Option<Box<HdMode7Frame>>,
    /// Ticket W36-06b: the OAM that drew that frame (the shadow's OAM before its last speculative
    /// frame ran), for the margin redraw's wrapped-copy check.
    pub oam: Box<[u8; 256]>,
    /// The OAM index that won each dot of that frame, when `margin` was asked for.
    pub sprite_ids: Option<Vec<u8>>,
    /// The background colour under each dot of that frame, when `margin` was asked for.
    pub bg_under: Vec<u8>,
}

/// The shadow machine and the buffers speculative frames are drawn into,
/// kept between frames so a steady session allocates nothing per frame.
pub struct Speculator {
    /// Built from the ROM on first use, so a session that never turns
    /// run-ahead on pays nothing for it.
    shadow: Option<EmuStepper>,
    sink: FrameBuffer,
    bundle: FrameBundleBuilder,
    overlay: Option<Vec<OverlayPixel>>,
}

impl Default for Speculator {
    fn default() -> Self {
        Self::new()
    }
}

impl Speculator {
    #[must_use]
    pub fn new() -> Self {
        Self {
            shadow: None,
            sink: FrameBuffer::new(),
            bundle: FrameBundleBuilder::new(
                rf_renderer::frame::NES_WIDTH as u16,
                rf_renderer::frame::NES_HEIGHT as u16,
            ),
            overlay: None,
        }
    }

    /// Run `frames` (clamped to 1..=[`MAX_RUN_AHEAD`]) speculative frames
    /// from `real`'s CURRENT state, with `input` held, on the shadow
    /// machine. `rom` is the image `real` was opened from. `real_size` is
    /// the real frame's `(width, height)`; `overlay_on` says whether the
    /// dropped-sprite overlay is being captured for the real frame.
    ///
    /// `real` is only read. `None` when a snapshot cannot be taken or
    /// loaded, or a speculative frame did not complete; the caller then
    /// shows the real frame.
    pub fn run(
        &mut self,
        real: &EmuStepper,
        rom: &[u8],
        input: InputFrame,
        frames: u8,
        real_size: (usize, usize),
        overlay_on: bool,
        margin: bool,
    ) -> Option<SpecFrame> {
        if self.shadow.is_none() {
            self.shadow = Some(EmuStepper::open(rom).ok()?);
        }
        let shadow = self.shadow.as_mut()?;
        let snapshot = real.snapshot().ok()?;
        shadow.restore(&snapshot).ok()?;
        shadow.copy_picture_config_from(real);
        fit(
            &mut self.sink,
            &mut self.overlay,
            real_size,
            overlay_on,
            shadow.hd_mode7_on(),
        );
        // Ticket W36-06b: what the margin redraw needs of the shown frame.
        self.sink.set_sprite_id_capture(margin);
        shadow.set_bg_under_capture(margin);
        let mut oam = Box::new(*shadow.oam());

        // Terminates: a bounded `for` over at most MAX_RUN_AHEAD frames.
        for _ in 0..frames.clamp(1, MAX_RUN_AHEAD) {
            // The OAM before this frame runs is the one that draws it.
            *oam = *shadow.oam();
            let ran = shadow.latch_and_advance_frame(
                input,
                &mut FanoutSink {
                    frame: &mut self.sink,
                    layers: None,
                    bundle: &mut self.bundle,
                    // Speculative audio is discarded: the device only ever
                    // hears the real timeline.
                    audio: None,
                    overlay: self.overlay.as_mut(),
                },
            );
            // Drop this frame's events (and the pixel copy) so the builder
            // never carries them into the next frame.
            drop(self.bundle.take(shadow.frame_count()));
            if ran == 0 {
                return None;
            }
        }

        let rgba = self.sink.to_vec();
        Some(SpecFrame {
            hd_mode7: hd_mode7_frame(shadow, &self.sink, &rgba),
            line_palettes: self.sink.line_palettes().to_vec(),
            rgba,
            oam,
            sprite_ids: self.sink.sprite_ids().map(<[u8]>::to_vec),
            bg_under: shadow.bg_under().to_vec(),
        })
    }
}

/// Size the scratch buffers to the real frame.
fn fit(
    sink: &mut FrameBuffer,
    overlay: &mut Option<Vec<OverlayPixel>>,
    (w, h): (usize, usize),
    overlay_on: bool,
    hd_mode7: bool,
) {
    if sink.width() != w || sink.height() != h {
        *sink = FrameBuffer::with_size(w, h);
    }
    sink.set_mode7_mask_capture(hd_mode7);
    match (&mut *overlay, overlay_on) {
        (Some(buf), true) if buf.len() == w * h => {}
        (slot, true) => {
            *slot = Some(vec![
                OverlayPixel {
                    palette_index: 0,
                    opaque: false,
                };
                w * h
            ]);
        }
        (slot, false) => *slot = None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core_api::CoreSink;
    use rf_input::{NesButton, SnesButton};

    /// A sink that keeps the frame and every audio sample of a real frame.
    struct Probe {
        frame: FrameBuffer,
        audio: Vec<i16>,
    }

    impl Probe {
        fn new() -> Self {
            Self {
                frame: FrameBuffer::new(),
                audio: Vec::new(),
            }
        }
    }

    impl CoreSink for Probe {
        fn video_scanline(&mut self, y: u16, pixels: &[rf_core_api::PpuPixel]) {
            self.frame.video_scanline(y, pixels);
        }
        fn sub_scanline(&mut self, y: u16, pixels: &[rf_core_api::SubPixel], fixed: u16) {
            self.frame.sub_scanline(y, pixels, fixed);
        }
        fn palette_scanline(&mut self, y: u16, palette: &[u16], brightness: u8) {
            self.frame.palette_scanline(y, palette, brightness);
        }
        fn overlay_scanline(&mut self, y: u16, pixels: &[OverlayPixel]) {
            self.frame.overlay_scanline(y, pixels);
        }
        fn audio(&mut self, samples: &[i16]) {
            self.audio.extend_from_slice(samples);
        }
        fn event(&mut self, _: rf_core_api::CoreEvent) {}
    }

    /// A tiny NROM program (hand-assembled 6502, mapper 0): enable rendering,
    /// then once per vblank read the A button and, while it is held, count
    /// up a WRAM byte that is written to the universal backdrop colour. So
    /// the picture and the machine both depend on the held input.
    ///
    /// | Addr  | Bytes       | Instruction                  |
    /// |-------|-------------|------------------------------|
    /// | $8000 | 78          | SEI                          |
    /// | $8001 | A2 FF 9A    | LDX #$FF ; TXS               |
    /// | $8004 | A9 00 8D 00 20 | LDA #0 ; STA $2000        |
    /// | $8009 | A9 0A 8D 01 20 | LDA #$0A ; STA $2001 (BG on) |
    /// | $800E | A9 00 85 10 | LDA #0 ; STA $10             |
    /// | $8012 | 2C 02 20 10 FB | wait: BIT $2002 ; BPL wait |
    /// | $8017 | A9 01 8D 16 40 | LDA #1 ; STA $4016 (strobe) |
    /// | $801C | A9 00 8D 16 40 | LDA #0 ; STA $4016        |
    /// | $8021 | AD 16 40 29 01 | LDA $4016 ; AND #1 (A)    |
    /// | $8026 | F0 02 E6 10 | BEQ +2 ; INC $10             |
    /// | $802A | A9 3F 8D 06 20 | LDA #$3F ; STA $2006      |
    /// | $802F | A9 00 8D 06 20 | LDA #0 ; STA $2006        |
    /// | $8034 | A5 10 29 3F 8D 07 20 | LDA $10 ; AND #$3F ; STA $2007 |
    /// | $803B | A9 00 8D 06 20 8D 06 20 | reset the VRAM address |
    /// | $8043 | 4C 12 80    | JMP wait                     |
    fn counter_nrom() -> Vec<u8> {
        let code: &[u8] = &[
            0x78, 0xA2, 0xFF, 0x9A, 0xA9, 0x00, 0x8D, 0x00, 0x20, 0xA9, 0x0A, 0x8D, 0x01, 0x20,
            0xA9, 0x00, 0x85, 0x10, 0x2C, 0x02, 0x20, 0x10, 0xFB, 0xA9, 0x01, 0x8D, 0x16, 0x40,
            0xA9, 0x00, 0x8D, 0x16, 0x40, 0xAD, 0x16, 0x40, 0x29, 0x01, 0xF0, 0x02, 0xE6, 0x10,
            0xA9, 0x3F, 0x8D, 0x06, 0x20, 0xA9, 0x00, 0x8D, 0x06, 0x20, 0xA5, 0x10, 0x29, 0x3F,
            0x8D, 0x07, 0x20, 0xA9, 0x00, 0x8D, 0x06, 0x20, 0x8D, 0x06, 0x20, 0x4C, 0x12, 0x80,
        ];
        let mut data = Vec::new();
        data.extend_from_slice(&rf_cart::nes::INES_MAGIC);
        data.push(1);
        data.push(1);
        data.extend_from_slice(&[0u8; 10]);
        let mut prg = vec![0u8; 16 * 1024];
        prg[..code.len()].copy_from_slice(code);
        prg[0x3FFC] = 0x00;
        prg[0x3FFD] = 0x80;
        data.extend(prg);
        data.extend(vec![0u8; 8 * 1024]);
        data
    }

    /// The synthetic Mode 7 floor, with its idle `JMP` replaced by a loop
    /// that turns on auto-joypad read and, once per vblank, bumps a WRAM
    /// counter while B is held and writes `counter | $40` into the Mode 7
    /// matrix A -- so the floor changes with the held input.
    fn mode7_input_rom() -> Vec<u8> {
        let mut rom = crate::mode_invariant::mode7_floor_rom();
        // The original program ends in `JMP spin` (4C lo hi) at its own address.
        let jmp = (0..0x7000)
            .rev()
            .find(|&i| {
                rom[i] == 0x4C && rom[i + 1] as usize | (rom[i + 2] as usize) << 8 == 0x8000 + i
            })
            .expect("the idle JMP");
        let at = 0x8000 + jmp as u16;
        let main = at + 5;
        let wait_end = at + 29;
        #[rustfmt::skip]
        let tail: Vec<u8> = vec![
            0xA9, 0x01, 0x8D, 0x00, 0x42,             // LDA #1 ; STA $4200 (auto-joypad)
            0xAD, 0x12, 0x42, 0x10, 0xFB,             // main: LDA $4212 ; BPL main
            0xAD, 0x19, 0x42, 0x10, 0x02,             // LDA $4219 ; BPL +2 (B not held)
            0xE6, 0x10,                               // INC $10
            0xA5, 0x10, 0x09, 0x40,                   // LDA $10 ; ORA #$40
            0x8D, 0x1B, 0x21, 0xA9, 0x00, 0x8D, 0x1B, 0x21, // STA $211B x2 (M7A)
            0xAD, 0x12, 0x42, 0x30, 0xFB,             // wait_end: LDA $4212 ; BMI wait_end
            0x4C, main as u8, (main >> 8) as u8,      // JMP main
        ];
        assert_eq!(wait_end, at + 29, "layout");
        rom[jmp..jmp + tail.len()].copy_from_slice(&tail);
        rom
    }

    fn pad(console_snes: bool, held: bool) -> InputFrame {
        let mut f = InputFrame::empty();
        if held {
            f.ports[0] = 1
                << if console_snes {
                    SnesButton::B.bit()
                } else {
                    NesButton::A.bit()
                };
        }
        f
    }

    /// Held for stretches long enough for run-ahead to land inside one, with
    /// presses and releases in between (so the "same input" window is also
    /// broken in places, which the comparison must skip).
    fn held_at(frame: usize) -> bool {
        matches!(frame, 10..=39 | 50..=51 | 60..=99 | 104 | 110..=119)
    }

    struct Run {
        /// State hash of the REAL machine after each frame's run-ahead call.
        hashes: Vec<[u8; 32]>,
        /// The same hash taken just before that call (restore is exact).
        before: Vec<[u8; 32]>,
        frame_counts: Vec<u64>,
        real: Vec<Vec<u8>>,
        shown: Vec<Vec<u8>>,
        audio: Vec<Vec<i16>>,
    }

    fn run(rom: &[u8], snes: bool, ahead: u8, frames: usize) -> Run {
        let mut stepper = EmuStepper::open(rom).expect("rom opens");
        stepper.resume();
        let mut speculator = Speculator::new();
        let mut out = Run {
            hashes: vec![],
            before: vec![],
            frame_counts: vec![],
            real: vec![],
            shown: vec![],
            audio: vec![],
        };
        for k in 0..frames {
            let input = pad(snes, held_at(k));
            let mut probe = Probe::new();
            assert!(stepper.tick_running_with_input(input, &mut probe));
            let real = probe.frame.to_vec();
            out.before.push(stepper.snapshot().unwrap().state_hash());
            let shown = if ahead > 0 {
                speculator
                    .run(
                        &stepper,
                        rom,
                        input,
                        ahead,
                        (probe.frame.width(), probe.frame.height()),
                        false,
                        false,
                    )
                    .expect("a speculative frame")
                    .rgba
            } else {
                real.clone()
            };
            out.hashes.push(stepper.snapshot().unwrap().state_hash());
            out.frame_counts.push(stepper.frame_count());
            out.real.push(real);
            out.shown.push(shown);
            out.audio.push(probe.audio);
        }
        out
    }

    fn roms() -> [(&'static str, Vec<u8>, bool); 2] {
        [
            ("NES", counter_nrom(), false),
            ("SNES Mode 7", mode7_input_rom(), true),
        ]
    }

    const FRAMES: usize = 120;

    /// Acceptance 1 and 2: the real machine, its picture and its audio are
    /// the same with run-ahead 1 or 2 as with it off, frame by frame.
    #[test]
    fn the_real_timeline_is_identical_with_run_ahead_on() {
        for (name, rom, snes) in roms() {
            let off = run(&rom, snes, 0, FRAMES);
            for ahead in [1, 2] {
                let on = run(&rom, snes, ahead, FRAMES);
                assert_eq!(on.hashes, off.hashes, "{name} +{ahead}: state hashes");
                assert_eq!(on.before, on.hashes, "{name} +{ahead}: restore is exact");
                assert_eq!(on.frame_counts, off.frame_counts, "{name} +{ahead}: frames");
                assert_eq!(on.real, off.real, "{name} +{ahead}: the real pictures");
                // Sample for sample, which implies the counts too.
                for (k, (a, b)) in on.audio.iter().zip(&off.audio).enumerate() {
                    if a != b {
                        let i = a.iter().zip(b).position(|(x, y)| x != y);
                        panic!("{name} +{ahead}: frame {k} audio differs: len {} vs {}, first diff {:?} {:?}", a.len(), b.len(), i, i.map(|i| (a[i], b[i])));
                    }
                }
                assert_eq!(on.audio, off.audio, "{name} +{ahead}: audio");
            }
            assert!(
                off.audio.iter().any(|a| !a.is_empty()),
                "{name}: the audio comparison must have something to compare"
            );
        }
    }

    /// The observable proof it is "ahead": with the input held across the
    /// step, the picture shown at frame k is the real frame k+N.
    #[test]
    fn the_shown_frame_is_the_real_frame_n_ahead() {
        for (name, rom, snes) in roms() {
            let off = run(&rom, snes, 0, FRAMES);
            for ahead in [1usize, 2] {
                let on = run(&rom, snes, ahead as u8, FRAMES);
                let mut compared = 0;
                let mut differed = 0;
                for k in 0..FRAMES - ahead {
                    if differs(&on.shown[k], &on.real[k]) {
                        differed += 1;
                    }
                    if (k..=k + ahead).any(|i| held_at(i) != held_at(k)) {
                        continue; // the input changed inside the window
                    }
                    assert_eq!(
                        on.shown[k],
                        off.real[k + ahead],
                        "{name} +{ahead}: shown frame {k} must be real frame {}",
                        k + ahead
                    );
                    compared += 1;
                }
                assert!(compared > 60, "{name} +{ahead}: only {compared} compared");
                assert!(
                    differed > 20,
                    "{name} +{ahead}: the shown frame never differed from the real one \
                     ({differed}); the test would pass vacuously"
                );
            }
        }
    }

    fn differs(a: &[u8], b: &[u8]) -> bool {
        a != b
    }

    /// Ticket W36-06: a patched session (a soft patch or a recipe mod edits
    /// the image in memory) boots the patched bytes, and `core_thread` hands
    /// the speculator that same `Vec`, so the shadow machine runs the patched
    /// image too. This proves the speculator is only as good as the image it
    /// is given: with the patched image the shown frame is the real frame one
    /// ahead; with the original image handed in by mistake it is not (the
    /// speculative frame is refused or wrong).
    #[test]
    fn the_shadow_machine_must_run_the_patched_image() {
        let original = counter_nrom();
        let mut patched = original.clone();
        // `AND #$3F` before the palette write becomes `AND #$0F`: once the
        // counter passes 15 the two images paint different colours.
        let at = patched
            .windows(4)
            .position(|w| w == [0xA5, 0x10, 0x29, 0x3F])
            .expect("the mask instruction")
            + 3;
        patched[at] = 0x0F;
        let off = run(&patched, false, 0, FRAMES);
        let shown_with = |shadow: &[u8]| {
            let mut stepper = EmuStepper::open(&patched).expect("opens");
            stepper.resume();
            let mut speculator = Speculator::new();
            let mut shown = Vec::new();
            for k in 0..FRAMES {
                let input = pad(false, held_at(k));
                let mut probe = Probe::new();
                assert!(stepper.tick_running_with_input(input, &mut probe));
                let size = (probe.frame.width(), probe.frame.height());
                shown.push(
                    speculator
                        .run(&stepper, shadow, input, 1, size, false, false)
                        .map(|s| s.rgba),
                );
            }
            shown
        };
        let stable = |k: usize| held_at(k) == held_at(k + 1) && k + 1 < FRAMES;
        let right = shown_with(&patched);
        let wrong = shown_with(&original);
        let (mut compared, mut mismatched) = (0, 0);
        for k in (0..FRAMES).filter(|&k| stable(k)) {
            assert_eq!(
                right[k].as_ref(),
                Some(&off.real[k + 1]),
                "patched shadow, frame {k}"
            );
            // Either no speculative frame (the restore refuses a machine of
            // another image) or a wrong one: never the right one.
            mismatched += usize::from(wrong[k].as_ref() != Some(&off.real[k + 1]));
            compared += 1;
        }
        assert!(compared >= 60, "only {compared} frames compared");
        assert!(
            mismatched >= 5,
            "an original-image shadow should visibly disagree ({mismatched})"
        );
    }

    /// Run-ahead of 0 is not a thing the speculator does, and the frame
    /// count is clamped to the documented maximum.
    #[test]
    fn the_frame_count_is_clamped() {
        let rom = counter_nrom();
        let off = run(&rom, false, 2, 40);
        let wild = {
            let mut stepper = EmuStepper::open(&rom).unwrap();
            stepper.resume();
            let mut sp = Speculator::new();
            let mut hashes = vec![];
            for k in 0..40 {
                let input = pad(false, held_at(k));
                assert!(stepper.tick_running_with_input(input, &mut Probe::new()));
                let _ = sp.run(&stepper, &rom, input, 200, (256, 240), false, false);
                hashes.push(stepper.snapshot().unwrap().state_hash());
            }
            hashes
        };
        assert_eq!(wild, off.hashes);
    }

    /// Measures the per-frame cost of run-ahead 2 on the SNES synthetic ROM.
    /// Informational (prints; asserts only that it is not absurd), because a
    /// wall-clock bound would be flaky on a loaded machine. Run with
    /// `cargo test --release -p retroforge run_ahead_cost -- --nocapture`.
    #[test]
    fn run_ahead_cost_is_measured() {
        let rom = mode7_input_rom();
        let mut stepper = EmuStepper::open(&rom).unwrap();
        stepper.resume();
        let mut speculator = Speculator::new();
        let input = pad(true, true);
        let mut sink = FrameBuffer::new();
        for _ in 0..10 {
            stepper.tick_running_with_input(input, &mut sink);
        }
        let n = 200u32;
        let t = std::time::Instant::now();
        for _ in 0..n {
            stepper.tick_running_with_input(input, &mut sink);
        }
        let plain = t.elapsed() / n;
        let t = std::time::Instant::now();
        for _ in 0..n {
            stepper.tick_running_with_input(input, &mut sink);
            let _ = speculator.run(&stepper, &rom, input, 2, (256, 240), false, false);
        }
        let with = t.elapsed() / n;
        println!("run-ahead cost, SNES synthetic ROM: plain {plain:?}/frame, +2 {with:?}/frame");
        assert!(with < std::time::Duration::from_millis(500));
    }

    /// The wiring, end to end: a real core thread with `SetRunAhead(1)`
    /// sends, at frame k, the picture a run-ahead-off core sends at k+1
    /// (the input is held throughout), and the frame counter it reports is
    /// still the real one.
    #[test]
    fn a_core_thread_shows_the_next_frame_and_reports_the_real_count() {
        use crate::core_thread::{spawn, CoreCommand, CoreEvent};
        use std::collections::HashMap;
        use std::sync::atomic::Ordering;
        use std::time::{Duration, Instant};

        fn collect(ahead: u8) -> HashMap<u64, Vec<u8>> {
            let core = spawn(counter_nrom()).expect("spawns");
            core.input.store(pad(false, true));
            core.cmd_tx.send(CoreCommand::SetRunAhead(ahead)).unwrap();
            core.cmd_tx.send(CoreCommand::Resume).unwrap();
            let mut frames = HashMap::new();
            let deadline = Instant::now() + Duration::from_secs(20);
            while frames.len() < 40 && Instant::now() < deadline {
                if let Ok(CoreEvent::Frame(msg)) = core.evt_rx.recv_timeout(Duration::from_secs(2))
                {
                    core.pending_frames.fetch_sub(1, Ordering::AcqRel);
                    frames.insert(msg.frame_count, msg.rgba);
                }
            }
            let _ = core.cmd_tx.send(CoreCommand::Shutdown);
            let _ = core.join_handle.join();
            frames
        }

        let off = collect(0);
        let on = collect(1);
        let (mut compared, mut differing) = (0, 0);
        for (&k, shown) in &on {
            if k < 4 {
                continue; // the held input has not reached the counter yet
            }
            if let Some(next) = off.get(&(k + 1)) {
                assert_eq!(shown, next, "frame {k} must show real frame {}", k + 1);
                // (Some counter values map to the same colour, e.g. the
                // NES's several blacks, so "differs" is checked in bulk.)
                differing += usize::from(shown != &off[&k]);
                compared += 1;
            }
        }
        assert!(compared >= 10, "only {compared} frames compared");
        assert!(differing >= 10, "the shown frames were never ahead");
    }
}
