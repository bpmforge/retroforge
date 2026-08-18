//! The `rom` command group (ticket W4-07; FR-DBG-007).
//!
//! ## Why this runs the real core
//!
//! `rom dump` and `rom trace` drive `rf_nes::NesBus`/`Cpu` — the same
//! machine the app runs — rather than a simplified headless model. A
//! developer tool that reported on a *different* emulator than the one
//! shipping would be worse than no tool: its output would look
//! authoritative and be about nothing.
//!
//! For the same reason `rom dump`'s tilemap goes through
//! `rf_debugger::nametable`, the decode the on-screen viewer uses, rather
//! than a second implementation that could drift from it.

use std::io::Write;
use std::path::{Path, PathBuf};

use rf_core_api::CoreSink;

/// Discards video/audio: these subcommands care about machine state at a
/// frame, not about pixels.
struct NullSink;
impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _p: &[rf_core_api::PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: rf_core_api::CoreEvent) {}
}

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("hash") => run_hash(&args[1..]),
        Some("inspect") => run_inspect(&args[1..]),
        Some("dump") => run_dump(&args[1..]),
        Some("trace") => run_trace(&args[1..]),
        Some(other) => {
            eprintln!("retroforge-tool rom: unknown subcommand `{other}`");
            2
        }
        None => {
            eprintln!("retroforge-tool rom: expected a subcommand");
            2
        }
    }
}

fn read_rom(path: &Path) -> Result<Vec<u8>, i32> {
    std::fs::read(path).map_err(|e| {
        eprintln!("{}: {e}", path.display());
        1
    })
}

/// `rom hash <rom>` — every hash, each labelled with what it is over.
///
/// Both sets are printed, and which one to USE is stated rather than left
/// to the reader: `normalized` is the header-stripped image
/// (RA/No-Intro-compatible, FR-CORE-011) and is what profiles, the ROM
/// manifest, `.rfreplay` and save states all key on. `raw` is the file as
/// it sits on disk and is diagnostics only — two differently-packaged
/// dumps of the same game share `normalized` but not `raw`, which is
/// exactly the confusion this labelling exists to prevent.
fn run_hash(args: &[String]) -> i32 {
    let Some(path) = args.first() else {
        eprintln!("retroforge-tool rom hash: expected <rom-path>");
        return 2;
    };
    let path = Path::new(path);
    let Ok(bytes) = read_rom(path) else { return 1 };
    let cart = match rf_cart::Cartridge::load(&bytes) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{}: {e}", path.display());
            return 1;
        }
    };
    let identity = match &cart {
        rf_cart::Cartridge::Nes { identity, .. } | rf_cart::Cartridge::Snes { identity, .. } => {
            identity
        }
    };
    println!("file: {}", path.display());
    println!("normalized (header-stripped; RA/No-Intro compatible — use this for identity):");
    println!("  crc32  {}", identity.normalized.crc32);
    println!("  md5    {}", identity.normalized.md5);
    println!("  sha1   {}", identity.normalized.sha1);
    println!("  sha256 {}", identity.normalized.sha256);
    println!("raw (whole file as on disk; DIAGNOSTICS ONLY — never identity):");
    println!("  crc32  {}", identity.raw.crc32);
    println!("  md5    {}", identity.raw.md5);
    println!("  sha1   {}", identity.raw.sha1);
    println!("  sha256 {}", identity.raw.sha256);
    0
}

/// `rom inspect <rom>` — the parsed header.
fn run_inspect(args: &[String]) -> i32 {
    let Some(path) = args.first() else {
        eprintln!("retroforge-tool rom inspect: expected <rom-path>");
        return 2;
    };
    let path = Path::new(path);
    let Ok(bytes) = read_rom(path) else { return 1 };
    let rom = match rf_nes::NesRom::from_ines_bytes(&bytes) {
        Ok(rom) => rom,
        Err(e) => {
            eprintln!("{}: {e}", path.display());
            return 1;
        }
    };
    let h = rom.header();
    println!("file:      {}", path.display());
    println!("format:    {:?}", h.format);
    println!("mapper:    {}", h.mapper);
    if let Some(sub) = h.submapper {
        println!("submapper: {sub}");
    }
    println!(
        "prg_rom:   {} bytes ({} x 16 KiB)",
        h.prg_rom_size,
        h.prg_rom_size / 16384
    );
    println!(
        "chr:       {} bytes ({})",
        h.chr_rom_size,
        if rom.chr_is_ram() {
            "CHR RAM"
        } else {
            "CHR ROM"
        }
    );
    println!("prg_ram:   {} bytes", h.prg_ram_size);
    println!("mirroring: {:?}", h.mirroring);
    println!("battery:   {}", h.battery);
    println!("trainer:   {}", h.trainer);
    0
}

/// Run `frames` frames headlessly and hand back the machine.
fn run_frames(bytes: &[u8], frames: u64) -> Result<rf_nes::NesBus, i32> {
    let mut bus = rf_nes::NesBus::from_ines_bytes(bytes).map_err(|e| {
        eprintln!("invalid rom: {e}");
        1
    })?;
    let mut cpu = rf_nes::Cpu::power_on(&mut bus);
    let target = bus.frame_count() + frames;
    while bus.frame_count() < target {
        cpu.step(&mut bus);
        bus.drain_video(&mut NullSink);
        bus.drain_audio(&mut NullSink);
    }
    Ok(bus)
}

fn parse_flag(args: &[String], name: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone())
}

/// `rom dump <rom> [--frame N] [--out DIR]` — VRAM/OAM/palette + tilemap.
fn run_dump(args: &[String]) -> i32 {
    let Some(path) = args.first() else {
        eprintln!("retroforge-tool rom dump: expected <rom-path> [--frame N] [--out DIR]");
        return 2;
    };
    let path = Path::new(path);
    let Ok(bytes) = read_rom(path) else { return 1 };
    let frame: u64 = parse_flag(args, "--frame")
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    let out = PathBuf::from(parse_flag(args, "--out").unwrap_or_else(|| ".".to_string()));

    let bus = match run_frames(&bytes, frame) {
        Ok(bus) => bus,
        Err(code) => return code,
    };
    if let Err(e) = std::fs::create_dir_all(&out) {
        eprintln!("{}: {e}", out.display());
        return 1;
    }

    // The accessors are non-observing borrows (W4-06d), so dumping cannot
    // perturb the machine it is reporting on.
    for (name, data) in [
        ("vram.bin", &bus.vram()[..]),
        ("oam.bin", &bus.oam()[..]),
        ("palette.bin", &bus.palette()[..]),
    ] {
        let file = out.join(name);
        if let Err(e) = std::fs::write(&file, data) {
            eprintln!("{}: {e}", file.display());
            return 1;
        }
        println!("wrote {} ({} bytes)", file.display(), data.len());
    }

    // Tilemap through the SAME decode the on-screen nametable viewer uses
    // (rf_debugger::nametable), so the export and the viewer cannot
    // disagree about what the nametable says.
    let tilemap_path = out.join("tilemap.txt");
    match rf_debugger::nametable::decode_nametable(bus.vram()) {
        Some(grid) => {
            let mut text = String::new();
            for row in grid.iter() {
                for cell in row.iter() {
                    text.push_str(&format!("{:02X}[{}] ", cell.tile_index, cell.palette));
                }
                text.push('\n');
            }
            if let Err(e) = std::fs::write(&tilemap_path, text) {
                eprintln!("{}: {e}", tilemap_path.display());
                return 1;
            }
            println!("wrote {}", tilemap_path.display());
        }
        None => {
            eprintln!("tilemap: VRAM too short to decode (this should not happen)");
            return 1;
        }
    }
    println!("(machine state at frame {frame})");
    0
}

/// `rom trace <rom> [--instructions N] [--out FILE]` — nestest-format
/// trace.
///
/// Uses `rf_nes::trace::format_trace_line`, the same formatter the
/// nestest golden-trace gate compares byte-for-byte against
/// `nestest.log` — so a trace this writes is in the format that suite
/// already proves correct, rather than a lookalike.
fn run_trace(args: &[String]) -> i32 {
    let Some(path) = args.first() else {
        eprintln!("retroforge-tool rom trace: expected <rom-path> [--instructions N] [--out FILE]");
        return 2;
    };
    let path = Path::new(path);
    let Ok(bytes) = read_rom(path) else { return 1 };
    let count: u64 = parse_flag(args, "--instructions")
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000);

    let mut bus = match rf_nes::NesBus::from_ines_bytes(&bytes) {
        Ok(bus) => bus,
        Err(e) => {
            eprintln!("{}: {e}", path.display());
            return 1;
        }
    };
    let mut cpu = rf_nes::Cpu::power_on(&mut bus);

    let mut sink: Box<dyn Write> = match parse_flag(args, "--out") {
        Some(file) => match std::fs::File::create(&file) {
            Ok(f) => {
                println!("writing {count} instruction(s) to {file}");
                Box::new(std::io::BufWriter::new(f))
            }
            Err(e) => {
                eprintln!("{file}: {e}");
                return 1;
            }
        },
        None => Box::new(std::io::stdout().lock()),
    };

    for _ in 0..count {
        let line = rf_nes::trace::format_trace_line(&cpu, &bus, bus.master_cycle());
        if let Err(e) = writeln!(sink, "{line}") {
            eprintln!("write failed: {e}");
            return 1;
        }
        cpu.step(&mut bus);
        bus.drain_video(&mut NullSink);
        bus.drain_audio(&mut NullSink);
    }
    if let Err(e) = sink.flush() {
        eprintln!("flush failed: {e}");
        return 1;
    }
    0
}
