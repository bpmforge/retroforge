//! **W36-08: the SNES recorder and the 65816 code atlas on a real cartridge**
//! (ROM-gated, law 5).
//!
//! Drives Super Mario World from power-on through the menus and the map into
//! Yoshi's Island 1 (the W33-07 token script, `SMW_TO_LEVEL`), plays the level
//! for at least 1,500 frames, builds the atlas from the recording and checks
//! that every executed opcode is an instruction start in the atlas at the
//! width it ran in. Only counts and offsets are printed; no ROM bytes leave
//! the test (clean room, D-021).
//!
//! "In real play" is checked with the profile's own RAM facts, not assumed:
//! game mode `$7E0100 == $14` (the profile's `start_when`), the per-tick
//! counter `$7E0014` advancing, and the camera `$7E001A` and player X
//! `$7E0094` moving while the pad holds Right.
//!
//! ```text
//! cargo test --release -p retroforge --test w36_08_atlas_smw -- --ignored --nocapture
//! ```

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use rf_core_api::coverage::{MODE_BITS, OPCODE, OPERAND};
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_input::SnesButton;
use rf_rebuild::{DecodeMode, Isa, Provenance};
use rf_snes::core::SnesCore;
use rf_snes::rebuild::Wdc65816;

/// Only for the shared script constants.
#[allow(dead_code, unused_imports)]
#[path = "w33_07_common/mod.rs"]
mod discovery;

const FILE: &str = "Super Mario World (USA).zip";
const GAME_MODE: u32 = 0x7E_0100;
const IN_LEVEL: u8 = 0x14;
const TICK: u32 = 0x7E_0014;
const CAMERA_X: u32 = 0x7E_001A;
const PLAYER_X: u32 = 0x7E_0094;
/// Frames of play (after the level has started) the recording must cover.
const PLAY_FRAMES: u32 = 1500;

fn rom_path() -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = Path::new(&root).join("snes").join(FILE);
    p.exists().then_some(p)
}

/// Hashes what the player would see and hear.
struct Hasher(u64);

impl Hasher {
    fn feed(&mut self, b: u8) {
        self.0 ^= u64::from(b);
        self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
    }
}

impl CoreSink for Hasher {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        self.feed(y as u8);
        for p in pixels {
            self.feed(p.palette_index);
        }
    }
    fn audio(&mut self, samples: &[i16]) {
        for s in samples {
            self.feed(*s as u8);
        }
    }
    fn event(&mut self, _event: CoreEvent) {}
}

/// `wN` idle N frames, `<buttons>N` hold N frames (`A B X Y S E U D L R`
/// joined by `+`), as `Rig::script` reads them.
fn frames_of(text: &str) -> Vec<u16> {
    let mut out = Vec::new();
    for tok in text.split_whitespace() {
        let (mask, n) = if let Some(n) = tok.strip_prefix('w') {
            (0u16, n.parse::<usize>().expect("wN"))
        } else {
            let split = tok.find(|c: char| c.is_ascii_digit()).expect("button+N");
            let (b, n) = tok.split_at(split);
            let mask = b.split('+').fold(0u16, |m, c| {
                let bit = match c {
                    "A" => SnesButton::A.bit(),
                    "B" => SnesButton::B.bit(),
                    "X" => SnesButton::X.bit(),
                    "Y" => SnesButton::Y.bit(),
                    "S" => SnesButton::Start.bit(),
                    "E" => SnesButton::Select.bit(),
                    "U" => SnesButton::Up.bit(),
                    "D" => SnesButton::Down.bit(),
                    "L" => SnesButton::Left.bit(),
                    "R" => SnesButton::Right.bit(),
                    other => panic!("button {other}"),
                };
                m | (1 << bit)
            });
            (mask, n.parse().expect("N"))
        };
        out.extend(std::iter::repeat(mask).take(n));
    }
    out
}

/// The whole session: menus to the level, then the W33-07 play script, then
/// jump-and-run until the level has seen `PLAY_FRAMES` frames.
fn session() -> (Vec<u16>, usize) {
    let to_level = frames_of(discovery::SMW_TO_LEVEL);
    let level_start = to_level.len();
    let mut pad = to_level;
    pad.extend(frames_of(discovery::SMW_PLAY));
    let more = frames_of("R+B14 R22");
    while pad.len() < level_start + PLAY_FRAMES as usize {
        pad.extend(&more);
    }
    (pad, level_start)
}

fn fingerprint(core: &SnesCore, video_audio: u64) -> u64 {
    let s = core.system();
    let mut h = Hasher(video_audio);
    let c = &s.cpu;
    for v in [c.a, c.x, c.y, c.sp, c.d, c.pc] {
        v.to_le_bytes().into_iter().for_each(|b| h.feed(b));
    }
    [c.dbr, c.pbr, c.p, u8::from(c.e)]
        .into_iter()
        .for_each(|b| h.feed(b));
    s.master_cycles
        .to_le_bytes()
        .into_iter()
        .for_each(|b| h.feed(b));
    s.bus.wram.iter().for_each(|&b| h.feed(b));
    s.bus.ppu.vram.iter().for_each(|&b| h.feed(b));
    s.bus.ppu.oam.iter().for_each(|&b| h.feed(b));
    h.0
}

fn word(core: &SnesCore, addr: u32) -> u16 {
    u16::from(core.peek(addr)) | (u16::from(core.peek(addr + 1)) << 8)
}

#[test]
#[ignore = "needs Super Mario World (USA) (RF_SCREENS_ROMS_DIR)"]
fn smw_yoshis_island_one_atlas_covers_every_executed_opcode() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
    let (pad, level_start) = session();

    // ---- armed vs unarmed, bit for bit, over menus and the first level
    // frames (the full session would double the test for no extra evidence).
    let check_frames = level_start + 300;
    let play = |armed: bool| {
        let mut core = SnesCore::load(&raw).expect("loads");
        core.set_coverage_recording(armed);
        core.reset(ResetKind::Hard);
        let mut sink = Hasher(0xcbf2_9ce4_8422_2325);
        let mut input = InputFrame::empty();
        for &mask in pad.iter().take(check_frames) {
            input.ports[0] = mask;
            core.run_frame(&input, &mut sink);
        }
        fingerprint(&core, sink.0)
    };
    let (armed, unarmed) = (play(true), play(false));
    assert_eq!(armed, unarmed, "recording changed the machine");
    println!("armed == unarmed over {check_frames} frames: {armed:#018x}");

    // ---- the recorded session ---------------------------------------------
    let mut core = SnesCore::load(&raw).expect("loads");
    core.set_coverage_recording(true);
    core.reset(ResetKind::Hard);
    let mut sink = Hasher(0xcbf2_9ce4_8422_2325);
    let mut input = InputFrame::empty();
    let (mut in_level_frames, mut ticks_seen, mut last_tick) = (0u32, 0u32, core.peek(TICK));
    let mut at_level_start = None;
    let (mut cam_min, mut cam_max) = (u16::MAX, 0u16);
    let (mut px_min, mut px_max) = (u16::MAX, 0u16);
    for (frame, &mask) in pad.iter().enumerate() {
        input.ports[0] = mask;
        core.run_frame(&input, &mut sink);
        if frame + 1 == level_start {
            assert_eq!(
                core.peek(GAME_MODE),
                IN_LEVEL,
                "not in a level when the script says Yoshi's Island 1 starts"
            );
            at_level_start = Some((word(&core, CAMERA_X), word(&core, PLAYER_X)));
        }
        if frame >= level_start {
            if core.peek(GAME_MODE) == IN_LEVEL {
                in_level_frames += 1;
                let (cam, px) = (word(&core, CAMERA_X), word(&core, PLAYER_X));
                (cam_min, cam_max) = (cam_min.min(cam), cam_max.max(cam));
                (px_min, px_max) = (px_min.min(px), px_max.max(px));
            }
            let tick = core.peek(TICK);
            ticks_seen += u32::from(tick != last_tick);
            last_tick = tick;
        }
    }
    let play_frames = (pad.len() - level_start) as u32;
    let (cam0, px0) = at_level_start.expect("reached the level");
    println!(
        "play: {play_frames} frames after the level start, {in_level_frames} in game mode \
         {IN_LEVEL:#x}; tick counter moved on {ticks_seen}; camera x {cam0} -> {cam_min}..{cam_max}; \
         player x {px0} -> {px_min}..{px_max}"
    );
    assert!(play_frames >= PLAY_FRAMES);
    assert!(
        in_level_frames * 10 >= play_frames * 9,
        "the level did not run for at least 90% of the recorded play"
    );
    assert!(ticks_seen * 10 >= play_frames * 9, "the game did not tick");
    assert!(
        cam_max.saturating_sub(cam0) >= 200 && px_max.saturating_sub(px0) >= 200,
        "Mario did not run through the level"
    );

    let log = core.take_coverage().expect("armed");
    // The ROM exactly as the bus maps it (copier header stripped).
    let rom: Vec<u8> = core.system().bus.rom.clone();
    let map_mode = core.system().bus.mode;
    assert!(log.instructions_recorded > 1_000_000);
    let atlas = rf_snes::rebuild::build_atlas_from_image(&raw, &log).expect("atlas");

    // ---- every executed opcode is an atlas instruction start at its width --
    let isa = Wdc65816::new(map_mode, rom.len());
    let starts: HashSet<(u32, u8)> = atlas
        .functions
        .iter()
        .flat_map(|f| f.blocks.iter())
        .flat_map(|&b| atlas.blocks[b as usize].insns.iter())
        .map(|i| (i.rom, i.mode))
        .collect();
    let mut executed = 0usize;
    let mut missing = Vec::new();
    let mut wrong_len = 0usize;
    let mut by_mode = [0usize; 5];
    for (off, &f) in log.prg_flags.iter().enumerate() {
        if f & OPCODE == 0 {
            continue;
        }
        for mode in log.modes_at(off) {
            executed += 1;
            by_mode[usize::from(mode)] += 1;
            if !starts.contains(&(off as u32, mode)) {
                missing.push((off, mode));
            }
            // The decoded length agrees with the operand bytes the recorder
            // marked for that instruction.
            let end = (off + 4).min(rom.len());
            let d = isa
                .decode(&rom[off..end], 0, DecodeMode(mode))
                .expect("decodes");
            if (1..usize::from(d.len)).any(|k| log.prg_flags[off + k] & OPERAND == 0) {
                wrong_len += 1;
            }
        }
        assert_ne!(
            f & MODE_BITS,
            0,
            "executed opcode {off:#x} carries no width"
        );
    }
    let first: Vec<String> = missing
        .iter()
        .take(10)
        .map(|(o, m)| format!("{o:#x}/m{m}"))
        .collect();
    assert!(
        missing.is_empty(),
        "{} executed (offset, width) pairs are not atlas instruction starts; first: {first:?}",
        missing.len()
    );
    assert_eq!(
        wrong_len, 0,
        "decode length disagrees with recorded operands"
    );
    assert!(!atlas.stats.budget_hit, "a step budget tripped on SMW");
    assert_eq!(atlas.stats.undecodable_observed, 0);
    assert_eq!(atlas.stats.observed_uncovered, 0);
    assert_eq!(
        (log.coproc_exec, log.other_exec),
        (0, 0),
        "the main CPU fetched from non-ROM, non-WRAM memory"
    );

    // ---- determinism, round trip, no ROM bytes -------------------------------
    let bytes = atlas.to_bytes();
    assert_eq!(
        bytes,
        rf_snes::rebuild::build_atlas_from_image(&raw, &log)
            .unwrap()
            .to_bytes()
    );
    assert_eq!(rf_rebuild::Atlas::from_bytes(&bytes).unwrap(), atlas);
    let windows: HashSet<&[u8]> = bytes.windows(16).collect();
    let leaked = (0..rom.len().saturating_sub(16))
        .step_by(97)
        .map(|o| &rom[o..o + 16])
        .filter(|w| w.iter().any(|&b| b != w[0]))
        .filter(|w| windows.contains(w))
        .count();
    assert_eq!(leaked, 0, "ROM bytes found in the serialised atlas");

    // ---- the report ---------------------------------------------------------
    let s = &atlas.stats;
    let mut fn_by_mode: BTreeMap<u8, usize> = BTreeMap::new();
    for f in &atlas.functions {
        *fn_by_mode.entry(f.entry_mode).or_default() += 1;
    }
    let two_width_entries = atlas
        .functions
        .iter()
        .fold(BTreeMap::<u32, BTreeSet<u8>>::new(), |mut m, f| {
            m.entry(f.entry_rom).or_default().insert(f.entry_mode);
            m
        })
        .values()
        .filter(|modes| modes.len() > 1)
        .count();
    let mut site_kinds: BTreeMap<String, usize> = BTreeMap::new();
    for site in &atlas.indirect_sites {
        *site_kinds.entry(format!("{:?}", site.kind)).or_default() += 1;
    }
    let mut insn_by_mode = [(0usize, 0usize); 5];
    for i in atlas.insns() {
        let slot = &mut insn_by_mode[usize::from(i.mode)];
        if i.prov == Provenance::Observed {
            slot.0 += 1;
        } else {
            slot.1 += 1;
        }
    }
    println!(
        "recorded: {} instructions, {} executed (offset, width) pairs by mode \
         [emu, m16x16, m8x16, m16x8, m8x8] = {by_mode:?}; ROM bytes run {} of {} ({:.1}%)",
        log.instructions_recorded,
        executed,
        log.executed_prg_bytes(),
        log.prg_flags.len(),
        log.prg_coverage_fraction() * 100.0,
    );
    println!(
        "atlas: {} functions by entry mode {fn_by_mode:?} ({two_width_entries} offsets entered \
         in more than one width), {} blocks, {} insns ({} observed / {} inferred); \
         observed/inferred by mode {insn_by_mode:?}",
        atlas.functions.len(),
        atlas.blocks.len(),
        s.observed_insns + s.inferred_insns,
        s.observed_insns,
        s.inferred_insns,
    );
    println!(
        "edges: {} unresolved static ({} ambiguous), {} overlap-rejected, {} dangling; \
         {} indirect sites {site_kinds:?}; {} RAM-executed indirect sites; \
         volatile regions {} ({} WRAM bytes run); coprocessor fetches {}, other non-ROM fetches {}",
        s.unresolved_static,
        s.ambiguous_static,
        s.overlap_rejected,
        s.dangling_edges,
        atlas.indirect_sites.len(),
        s.ram_indirect_sites,
        atlas.volatile.len(),
        log.executed_ram_bytes(),
        log.coproc_exec,
        log.other_exec,
    );
    println!(
        "entries: {} interrupt, {} indirect, {} call, {} orphan; interrupts {:?}; \
         superset: {} never-run (offset, width) pairs decode ({} also in the atlas); \
         cache {} bytes (code-atlas-v3)",
        count_kind(&atlas, rf_rebuild::entry::INTERRUPT),
        count_kind(&atlas, rf_rebuild::entry::INDIRECT),
        count_kind(&atlas, rf_rebuild::entry::CALL),
        s.orphan_entries,
        log.interrupts
            .iter()
            .map(|i| (i.kind, i.target.mode))
            .collect::<Vec<_>>(),
        s.superset_candidates,
        s.superset_in_atlas,
        bytes.len(),
    );
}

fn count_kind(a: &rf_rebuild::Atlas, kind: u8) -> usize {
    a.functions
        .iter()
        .filter(|f| f.entry_kinds & kind != 0)
        .count()
}
