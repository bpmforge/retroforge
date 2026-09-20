//! Does a real commercial library actually boot? (ticket W14-03)
//!
//! This project has only ever run its own fixtures plus blargg/NIST-style
//! test ROMs. Once a real collection is reachable, running it is the
//! strongest accuracy signal available — the same kind of evidence the
//! darwin runner gave CI in W12-01. This is that run, and it is bounded
//! before it is useful.
//!
//! # Law 8 is criterion 1, not a footnote
//!
//! Running thousands of unknown commercial ROMs on the developer's own
//! workstation is the RF-L-09 shape at scale, and **finding hangs is close
//! to the point of the exercise**. So:
//!
//! - **every ROM runs in a child process.** A hang, an abort or a stack
//!   overflow costs one bucket entry and cannot take the harness with it.
//!   The child is this same test binary, re-executed via
//!   `current_exe()` with [`ROM_VAR`] set — no second binary to keep in
//!   step, and no new dependency for the crate;
//! - **the cap is wall-clock and it is enforced by the parent**, outside
//!   any emulation loop, so it holds however tightly the child is spinning;
//! - **it never runs unattended.** It is `#[ignore]`d and needs
//!   [`LIBRARY_VAR`] set, so no gate, no workflow and no plain
//!   `cargo test` can start it. That is the written decision this ticket
//!   asked for, recorded here and in `docs/TESTING.md` §0 rather than
//!   after the first full run.
//!
//! # The buckets are triage, and nothing here is a pass
//!
//! A run lands in exactly one of: **rendered something**, **rendered
//! nothing**, **refused** (this build does not support the mapper or
//! chip), **crashed**, **timed out**. None of them is called a pass,
//! deliberately. "It booted" is not "it is correct", and a headline saying
//! otherwise would repeat exactly the mistake that reopened W7-08, where a
//! substring check reported a whole ROM green off its first subtest. What
//! this produces is a list for a human to read.
//!
//! # Running it
//!
//! ```text
//! RF_ROM_LIBRARY=~/Games/Roms/nes:~/Games/Roms/snes \
//!   cargo test --release -p rf-harness --test boot_census -- --ignored --nocapture
//! ```
//!
//! Release, always: a debug core is slow enough that the wall-clock cap
//! would measure the build profile rather than the ROM. No ROM bytes and
//! no home-directory path are committed (law 5) — only counts and titles.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, PpuPixel, Step};

/// Colon-separated directories of archives to run.
const LIBRARY_VAR: &str = "RF_ROM_LIBRARY";
/// Set by the parent on the child, naming the one archive to run.
const ROM_VAR: &str = "RF_CENSUS_ROM";

/// How long one ROM may take before the parent kills it. Generous against
/// a boot sequence (most titles draw within a second or two of emulated
/// time) and short enough that a hung library still finishes a census.
const PER_ROM_TIMEOUT: Duration = Duration::from_secs(30);

/// Emulated frames to run. Ten seconds of game time: past a licence
/// screen, into a title screen, without waiting out an attract mode.
const FRAMES: u32 = 600;

/// Exit codes the child uses to report its bucket. Anything else — a
/// panic's 101, a signal — is [`Bucket::Crashed`], which is the point of
/// running it out of process.
const EXIT_RENDERED: i32 = 0;
const EXIT_BLANK: i32 = 10;
/// The core never emitted a scanline at all, which is a different fault
/// from emitting a uniform one — see [`Bucket::NoVideo`].
const EXIT_NO_VIDEO: i32 = 13;
const EXIT_REFUSED: i32 = 11;
const EXIT_NO_ROM_IN_ARCHIVE: i32 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Bucket {
    RenderedSomething,
    /// Scanlines arrived, every pixel the same. The PPU ran; the game drew
    /// a blank screen.
    RenderedNothing,
    /// No scanline ever arrived. The PPU did not run at all, which is a
    /// core fault rather than a game's blank screen — worth its own bucket
    /// because the two have nothing to do with each other.
    NoVideo,
    Refused,
    Crashed,
    TimedOut,
}

impl Bucket {
    const fn label(self) -> &'static str {
        match self {
            Bucket::RenderedSomething => "rendered something",
            Bucket::RenderedNothing => "rendered a uniform screen",
            Bucket::NoVideo => "emitted NO video at all",
            Bucket::Refused => "refused (unsupported mapper or chip)",
            Bucket::Crashed => "CRASHED",
            Bucket::TimedOut => "TIMED OUT",
        }
    }
}

// ---------------------------------------------------------------------------
// child: one ROM, in its own process
// ---------------------------------------------------------------------------

/// Records whether the core ever drew two different pixels.
///
/// Not a frame hash and not a golden: the question here is only "did
/// anything appear?", and a uniform screen is the shape a core that loaded
/// and then did nothing produces.
#[derive(Default)]
struct SawAnything {
    first: Option<u8>,
    varied: bool,
    scanlines: u64,
}

impl CoreSink for SawAnything {
    fn video_scanline(&mut self, _y: u16, pixels: &[PpuPixel]) {
        self.scanlines += 1;
        for pixel in pixels {
            match self.first {
                None => self.first = Some(pixel.palette_index),
                Some(first) if first != pixel.palette_index => self.varied = true,
                Some(_) => {}
            }
        }
    }
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: CoreEvent) {}
}

/// The child half. Loads the one archive it was given, runs [`FRAMES`]
/// frames, and exits with the bucket's code.
#[test]
#[ignore = "child half of the boot census; the parent re-executes this"]
fn boot_census_child() {
    let Ok(path) = std::env::var(ROM_VAR) else {
        eprintln!("SKIP: {ROM_VAR} unset — this is the census's child half");
        return;
    };
    let Some(bytes) = rom_bytes(Path::new(&path)) else {
        std::process::exit(EXIT_NO_ROM_IN_ARCHIVE);
    };

    let mut sink = SawAnything::default();
    let mut core: Box<dyn EmulatorCore> = match rf_cart::Cartridge::load(&bytes) {
        Ok(rf_cart::Cartridge::Nes { .. }) => {
            match rf_nes::core::NesCore::from_ines_bytes(&bytes) {
                Ok(core) => Box::new(core),
                Err(_) => std::process::exit(EXIT_REFUSED),
            }
        }
        Ok(rf_cart::Cartridge::Snes { .. }) => match rf_snes::core::SnesCore::load(&bytes) {
            Ok(core) => Box::new(core),
            Err(_) => std::process::exit(EXIT_REFUSED),
        },
        Err(_) => std::process::exit(EXIT_REFUSED),
    };

    // Bounded by construction as well as by the parent's clock:
    // `Step::Frame` carries a cycle budget, so this loop runs exactly
    // FRAMES iterations whatever the ROM does (law 8).
    for _ in 0..FRAMES {
        core.step(Step::Frame, &mut sink);
    }

    std::process::exit(if sink.varied {
        EXIT_RENDERED
    } else if sink.scanlines == 0 {
        EXIT_NO_VIDEO
    } else {
        EXIT_BLANK
    });
}

// ---------------------------------------------------------------------------
// parent: the census
// ---------------------------------------------------------------------------

#[test]
#[ignore = "local: runs a real ROM library, one child process per title"]
fn boot_census() {
    if std::env::var(ROM_VAR).is_ok() {
        // This process IS a child; its work is `boot_census_child`.
        return;
    }
    let Ok(library) = std::env::var(LIBRARY_VAR) else {
        eprintln!("SKIP: set {LIBRARY_VAR} to colon-separated ROM directories");
        return;
    };

    let exe = std::env::current_exe().expect("the test binary must know its own path");
    let archives: Vec<PathBuf> = library
        .split(':')
        .flat_map(|dir| archives_in(Path::new(dir)))
        .collect();
    assert!(
        !archives.is_empty(),
        "{LIBRARY_VAR} names no archives — a census that ran nothing must not report success"
    );

    let started = Instant::now();
    let mut results: Vec<(Bucket, String)> = Vec::with_capacity(archives.len());
    for (index, archive) in archives.iter().enumerate() {
        let title = archive
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let bucket = run_one(&exe, archive);
        if matches!(bucket, Bucket::Crashed | Bucket::TimedOut) {
            // Printed as it happens, not only in the summary: these are
            // the two findings worth having even if the run is
            // interrupted.
            eprintln!("  [{}] {}: {title}", index + 1, bucket.label());
        }
        // Optional per-title record (ticket W14-24): `RF_CENSUS_OUT` names a
        // TSV file that gets one `bucket<TAB>title` line per archive, so two
        // census runs can be diffed title by title instead of by counts.
        // Titles only, never paths or ROM bytes (law 5).
        if let Ok(out) = std::env::var("RF_CENSUS_OUT") {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&out)
            {
                let _ = writeln!(f, "{}\t{title}", bucket.label());
            }
        }
        results.push((bucket, title));
    }

    report(&results, started.elapsed());
}

/// Run one archive in a child process under the wall-clock cap.
fn run_one(exe: &Path, archive: &Path) -> Bucket {
    let child = Command::new(exe)
        .args(["--exact", "boot_census_child", "--ignored", "--nocapture"])
        .env(ROM_VAR, archive)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return Bucket::Crashed;
    };

    let deadline = Instant::now() + PER_ROM_TIMEOUT;
    // Poll rather than block: `std::process` has no wait-with-timeout, and
    // the whole point of this harness is that the parent's clock is the
    // authority. Terminates at the deadline whatever the child is doing.
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return bucket_for(status.code()),
            Ok(None) => {}
            Err(_) => return Bucket::Crashed,
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Bucket::TimedOut;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `None` is a signal — killed, aborted, segfaulted — which is a crash.
const fn bucket_for(code: Option<i32>) -> Bucket {
    match code {
        Some(EXIT_RENDERED) => Bucket::RenderedSomething,
        Some(EXIT_BLANK) => Bucket::RenderedNothing,
        Some(EXIT_NO_VIDEO) => Bucket::NoVideo,
        Some(EXIT_REFUSED | EXIT_NO_ROM_IN_ARCHIVE) => Bucket::Refused,
        _ => Bucket::Crashed,
    }
}

fn report(results: &[(Bucket, String)], elapsed: Duration) {
    let count = |want: Bucket| results.iter().filter(|(b, _)| *b == want).count();
    println!(
        "\nboot census: {} titles in {:.0}s",
        results.len(),
        elapsed.as_secs_f32()
    );
    for bucket in [
        Bucket::RenderedSomething,
        Bucket::RenderedNothing,
        Bucket::NoVideo,
        Bucket::Refused,
        Bucket::Crashed,
        Bucket::TimedOut,
    ] {
        println!("  {:>5}  {}", count(bucket), bucket.label());
    }
    println!(
        "\nNone of these is a pass. 'Rendered something' means pixels changed \
         within {FRAMES} frames, not that the game is correct."
    );

    // The two buckets worth a name, listed in full: a crash or a hang is a
    // bug in this emulator, and a count alone cannot be acted on.
    for bucket in [Bucket::Crashed, Bucket::TimedOut] {
        let titles: Vec<&str> = results
            .iter()
            .filter(|(b, _)| *b == bucket)
            .map(|(_, t)| t.as_str())
            .collect();
        if titles.is_empty() {
            continue;
        }
        println!("\n{} ({}):", bucket.label(), titles.len());
        for title in titles {
            println!("   {title}");
        }
    }
}

// ---------------------------------------------------------------------------
// shared
// ---------------------------------------------------------------------------

/// Every `.zip` directly inside `dir`, plus bare ROM files. One level,
/// never recursive: the depth is fixed by construction, so there is no
/// walk to prove terminates (law 8).
fn archives_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("zip" | "nes" | "sfc" | "smc" | "fig")
            )
        })
        .collect();
    // Alphabetical, so an interrupted census can be compared with the next
    // one and a title's position means something.
    out.sort();
    out
}

/// The cartridge bytes inside `path`, which may be a bare ROM or an
/// archive. Capped at 64 MiB, and a nested archive is skipped rather than
/// recursed — the same untrusted-input posture the shell's own reader
/// takes (`crates/retroforge/src/rom_open.rs`).
fn rom_bytes(path: &Path) -> Option<Vec<u8>> {
    let raw = std::fs::read(path).ok()?;
    if !raw.starts_with(b"PK\x03\x04") {
        return Some(raw);
    }
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(raw)).ok()?;
    for index in 0..archive.len() {
        let Ok(mut file) = archive.by_index(index) else {
            break;
        };
        if !file.is_file() || file.name().to_ascii_lowercase().ends_with(".zip") {
            continue;
        }
        let mut buf = Vec::new();
        if (&mut file).take(64 << 20).read_to_end(&mut buf).is_err() {
            continue;
        }
        if rf_cart::Cartridge::load(&buf).is_ok() {
            return Some(buf);
        }
    }
    None
}
