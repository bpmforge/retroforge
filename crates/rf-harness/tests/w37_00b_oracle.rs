//! **W37-00b: per-frame state trace of a movie replay, for diffing against a
//! reference emulator run as a black box** (ROM- and movie-gated, law 5;
//! `#[ignore]`, skips when files are absent).
//!
//! Writes two files into `RF_W37B_OUT` (default `$TMPDIR/rf-w37b`):
//!
//! * `<name>.ours.txt`: one line per frame, `frame` then a fixed set of RAM
//!   facts and an FNV-1a hash of the zero page / direct page, from our core.
//! * `<name>.lua`: a script for a reference emulator's Lua test runner that
//!   feeds the same pad words frame by frame and prints the same line.
//!
//! Only frame numbers, byte values and hashes are written; no ROM or movie
//! content (D-021). The reference is used only as an oracle: run it, diff
//! the two files, never read its source.
//!
//! ```text
//! cargo test --release -p rf-harness --test w37_00b_oracle -- --ignored --nocapture
//! ```

use std::fmt::Write as _;
use std::io::Read as _;
use std::path::PathBuf;

use rf_core_api::EmulatorCore as _;
use rf_harness::movie::{self, Movie};
use rf_harness::movie_replay::{plan, replay_nes, replay_snes};

fn roms_root() -> PathBuf {
    PathBuf::from(
        std::env::var("RF_SCREENS_ROMS_DIR")
            .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string()),
    )
}

fn out_dir() -> PathBuf {
    let d = std::env::var("RF_W37B_OUT")
        .map_or_else(|_| std::env::temp_dir().join("rf-w37b"), PathBuf::from);
    std::fs::create_dir_all(&d).expect("out dir");
    d
}

/// Frames to trace (`RF_W37B_FRAMES`, default 8000).
fn frame_cap() -> usize {
    std::env::var("RF_W37B_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8000)
}

/// Idle frames before movie frame 0 (`RF_W37B_DELAY`, default 0).
fn delay() -> usize {
    std::env::var("RF_W37B_DELAY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn rom_from_zip(path: &std::path::Path, nes: bool) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).ok()?;
        if !entry.is_file() {
            continue;
        }
        let mut buf = Vec::new();
        entry.by_ref().take(64 << 20).read_to_end(&mut buf).ok()?;
        match (rf_cart::Cartridge::load(&buf), nes) {
            (Ok(rf_cart::Cartridge::Nes { .. }), true)
            | (Ok(rf_cart::Cartridge::Snes { .. }), false) => return Some(buf),
            _ => {}
        }
    }
    None
}

fn fnv(bytes: impl Iterator<Item = u8>) -> u32 {
    bytes.fold(0x811c_9dc5u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

/// The pad words of the laid-out plan, four hex digits per frame.
fn pads_hex(frames: &[movie::MovieFrame], cap: usize) -> String {
    let mut s = String::with_capacity(cap * 4);
    for f in frames.iter().take(cap) {
        let _ = write!(s, "{:04x}", f.pads[0]);
    }
    s
}

// ---------------------------------------------------------------- SNES

/// Bytes printed per frame (bank `$7E`), then the hash of `$0000-$00FF`.
const SNES_FACTS: [u32; 13] = [
    0x0100, 0x0DBE, 0x13BF, 0x0094, 0x0095, 0x0096, 0x0097, 0x0013, 0x0014, 0x148D, 0x008A, 0x008B,
    0x008D,
];

/// Our bit layout (`rf_input::SnesButton::bit`) to the runner's input keys.
const SNES_KEYS: [&str; 12] = [
    "b", "y", "select", "start", "up", "down", "left", "right", "a", "x", "l", "r",
];

fn snes_lua(pads: &str, cap: usize) -> String {
    let mut bits = String::new();
    for (i, k) in SNES_KEYS.iter().enumerate() {
        let b = rf_input::SnesButton::ALL[i].bit();
        let _ = write!(bits, "{{\"{k}\",{b}}},");
    }
    let facts: Vec<String> = SNES_FACTS.iter().map(|a| format!("0x{a:04x}")).collect();
    format!(
        r#"local P = "{pads}"
local CAP = {cap}
local DUMP = {dump}
local KEYS = {{{bits}}}
local FACTS = {{{facts}}}
-- Frames are counted at the runner's frame START (line 0), which is where
-- our run_frame ends: the pads polled in the vblank before the n-th frame
-- start are movie frame n, and the state printed at that start is ours
-- after run_frame n.
local frame = 0
local function word(f)
  local i = f * 4 + 1
  if i > #P then return 0 end
  return tonumber(string.sub(P, i, i + 3), 16)
end
emu.addEventCallback(function()
  local w = word(frame)
  local t = {{}}
  for _, kb in ipairs(KEYS) do t[kb[1]] = ((w >> kb[2]) & 1) == 1 end
  emu.setInput(t, 0)
end, emu.eventType.inputPolled)
emu.addEventCallback(function()
  local parts = {{tostring(frame)}}
  parts[#parts + 1] = string.format("%02x%02x", emu.read(0x4219, emu.memType.snesMemory), emu.read(0x4218, emu.memType.snesMemory))
  for _, a in ipairs(FACTS) do
    parts[#parts + 1] = string.format("%02x", emu.read(a, emu.memType.snesWorkRam))
  end
  local h = 0x811c9dc5
  for a = 0, 255 do
    h = ((h ~ emu.read(a, emu.memType.snesWorkRam)) * 0x01000193) & 0xffffffff
  end
  parts[#parts + 1] = string.format("%08x", h)
  print(table.concat(parts, " "))
  if frame == DUMP then
    local d = {{}}
    for a = 0, 0x1fff do d[#d + 1] = string.format("%02x", emu.read(a, emu.memType.snesWorkRam)) end
    print("D " .. table.concat(d))
  end
  frame = frame + 1
  if frame >= CAP then emu.stop(0) end
end, emu.eventType.startFrame)
"#,
        facts = facts.join(","),
        dump = dump_frame().map_or(-1, |d| d as i64),
    )
}

/// A frame whose `$0000-$1FFF` both sides dump in full (`RF_W37B_DUMP`).
fn dump_frame() -> Option<usize> {
    std::env::var("RF_W37B_DUMP").ok()?.parse().ok()
}

#[test]
#[ignore = "needs Super Mario World (USA) and the lsnes movie"]
fn smw_lsmv_trace_and_reference_script() {
    let Some(raw) = rom_from_zip(
        &roms_root().join("snes").join("Super Mario World (USA).zip"),
        false,
    ) else {
        println!("SKIP: SMW not found");
        return;
    };
    let path = roms_root().join("tas/4144/x/brunovisnadi-smw-warps.lsmv");
    let Ok(bytes) = std::fs::read(&path) else {
        println!("SKIP: movie not found");
        return;
    };
    let m: Movie = movie::lsmv::parse_archive(&bytes).expect("lsmv");
    let pl = plan(&[], delay(), 0, &m);
    let cap = frame_cap().min(pl.frames.len());
    let dir = out_dir();
    std::fs::write(
        dir.join("smw.lua"),
        snes_lua(&pads_hex(&pl.frames, cap), cap),
    )
    .expect("lua");

    let mut out = String::new();
    replay_snes(&raw, &pl, false, |f, c| {
        let _ = write!(out, "{f} {:02x}{:02x}", c.peek(0x4219), c.peek(0x4218));
        for a in SNES_FACTS {
            let _ = write!(out, " {:02x}", c.peek(0x7E_0000 | a));
        }
        let h = fnv((0..256u32).map(|a| c.peek(0x7E_0000 | a)));
        let _ = writeln!(out, " {h:08x}");
        if dump_frame() == Some(f as usize) {
            let _ = write!(out, "D ");
            for a in 0..0x2000u32 {
                let _ = write!(out, "{:02x}", c.peek(0x7E_0000 | a));
            }
            let _ = writeln!(out);
        }
        (f as usize) + 1 < cap
    })
    .expect("loads");
    std::fs::write(dir.join("smw.ours.txt"), out).expect("trace");
    println!("wrote {cap} frames to {}", dir.display());
}

// ---------------------------------------------------------------- NES

/// CPU RAM bytes printed per frame, then the hash of `$0000-$00FF`.
const NES_FACTS: [u32; 8] = [
    0x0770, 0x075A, 0x075F, 0x075C, 0x006D, 0x0086, 0x00CE, 0x07A7,
];

const NES_KEYS: [&str; 8] = ["a", "b", "select", "start", "up", "down", "left", "right"];

fn nes_lua(pads: &str, cap: usize) -> String {
    let mut bits = String::new();
    for (i, k) in NES_KEYS.iter().enumerate() {
        let b = rf_input::NesButton::ALL[i].bit();
        let _ = write!(bits, "{{\"{k}\",{b}}},");
    }
    let facts: Vec<String> = NES_FACTS.iter().map(|a| format!("0x{a:04x}")).collect();
    format!(
        r#"local P = "{pads}"
local CAP = {cap}
local KEYS = {{{bits}}}
local FACTS = {{{facts}}}
local frame = 0
local function word(f)
  local i = f * 4 + 1
  if i > #P then return 0 end
  return tonumber(string.sub(P, i, i + 3), 16)
end
emu.addEventCallback(function()
  local w = word(frame)
  local t = {{}}
  for _, kb in ipairs(KEYS) do t[kb[1]] = ((w >> kb[2]) & 1) == 1 end
  emu.setInput(t, 0)
end, emu.eventType.inputPolled)
emu.addEventCallback(function()
  local parts = {{tostring(frame)}}
  for _, a in ipairs(FACTS) do
    parts[#parts + 1] = string.format("%02x", emu.read(a, emu.memType.nesInternalRam))
  end
  local h = 0x811c9dc5
  for a = 0, 255 do
    h = ((h ~ emu.read(a, emu.memType.nesInternalRam)) * 0x01000193) & 0xffffffff
  end
  parts[#parts + 1] = string.format("%08x", h)
  print(table.concat(parts, " "))
  frame = frame + 1
  if frame >= CAP then emu.stop(0) end
end, emu.eventType.endFrame)
"#,
        facts = facts.join(","),
    )
}

/// `RF_W37B_NES_ROM` (an iNES file or a zip) and `RF_W37B_FM2`.
#[test]
#[ignore = "needs an NES ROM and an .fm2 movie named by env vars"]
fn nes_fm2_trace_and_reference_script() {
    let (Ok(rom), Ok(fm2)) = (
        std::env::var("RF_W37B_NES_ROM"),
        std::env::var("RF_W37B_FM2"),
    ) else {
        println!("SKIP: set RF_W37B_NES_ROM and RF_W37B_FM2");
        return;
    };
    let rom = PathBuf::from(rom);
    let raw = if rom.extension().is_some_and(|e| e == "zip") {
        rom_from_zip(&rom, true).expect("an iNES image in the zip")
    } else {
        std::fs::read(&rom).expect("rom")
    };
    let text = std::fs::read_to_string(&fm2).expect("fm2");
    let m: Movie = movie::fm2::parse(&text).expect("fm2 parses");
    let pl = plan(&[], delay(), 0, &m);
    let cap = frame_cap().min(pl.frames.len());
    let dir = out_dir();
    std::fs::write(
        dir.join("nes.lua"),
        nes_lua(&pads_hex(&pl.frames, cap), cap),
    )
    .expect("lua");

    let mut out = String::new();
    replay_nes(&raw, &pl, false, |f, c| {
        let _ = write!(out, "{f}");
        for a in NES_FACTS {
            let _ = write!(out, " {:02x}", c.peek(a));
        }
        let h = fnv((0..256u32).map(|a| c.peek(a)));
        let _ = writeln!(out, " {h:08x}");
        (f as usize) + 1 < cap
    })
    .expect("loads");
    std::fs::write(dir.join("nes.ours.txt"), out).expect("trace");
    println!("wrote {cap} frames to {}", dir.display());
}

/// Boot timing: per frame, master cycles since reset, the S-CPU's PBR:PC and
/// the SPC700's PC at the end of each of the first `RF_W37B_FRAMES` frames,
/// idle pads. The reference script prints the same from its own state.
#[test]
#[ignore = "needs Super Mario World (USA)"]
fn smw_boot_timing_trace() {
    let Some(raw) = rom_from_zip(
        &roms_root().join("snes").join("Super Mario World (USA).zip"),
        false,
    ) else {
        println!("SKIP: SMW not found");
        return;
    };
    let cap = frame_cap();
    let m = Movie {
        format: movie::Format::Lsmv,
        console: movie::Console::Snes,
        frames: vec![movie::MovieFrame::default(); cap],
        rerecords: None,
        rom_hint: None,
        dropped_subframes: 0,
    };
    let mut out = String::new();
    replay_snes(&raw, &plan(&[], 0, 0, &m), false, |f, c| {
        let s = c.system();
        let _ = writeln!(
            out,
            "{f} {} {:02x}{:04x} {:04x} {:02x}{:02x}{:02x}{:02x} xfer={} {:?}",
            s.master_cycles,
            s.cpu.pbr,
            s.cpu.pc,
            s.bus.apu.cpu.pc,
            c.peek(0x2140),
            c.peek(0x2141),
            c.peek(0x2142),
            c.peek(0x2143),
            s.bus.apu.boot.transferred,
            s.bus.apu.boot.state,
        );
        true
    })
    .expect("loads");
    let dir = out_dir();
    std::fs::write(dir.join("boot.ours.txt"), out).expect("trace");
    std::fs::write(
        dir.join("boot.lua"),
        format!(
            r#"local CAP = {cap}
local frame = 0
emu.addEventCallback(function()
  local s = emu.getState()
  print(string.format("%d %d %02x%04x %04x %02x%02x%02x%02x", frame, s["masterClock"], s["cpu.k"], s["cpu.pc"], s["spc.pc"],
    emu.read(0x2140, emu.memType.snesMemory), emu.read(0x2141, emu.memType.snesMemory), emu.read(0x2142, emu.memType.snesMemory), emu.read(0x2143, emu.memType.snesMemory)))
  frame = frame + 1
  if frame >= CAP then emu.stop(0) end
end, emu.eventType.endFrame)
"#
        ),
    )
    .expect("lua");
}
