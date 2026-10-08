//! **Find a game's camera by playing it** (ticket W25-01).
//!
//! Every scrolling game copies its camera position into the console's
//! scroll registers each frame — NES `$2005`/`$2006`
//! ([nesdev PPU scrolling](https://www.nesdev.org/wiki/PPU_scrolling)),
//! SNES `$210D`/`$210E` BG1HOFS/VOFS (fullsnes "PPU Scrolling"). This
//! harness plays a real dump with a crude script (press Start when nothing
//! is scrolling, otherwise hold Right and hop), records the scroll value
//! of every frame, and scores every work-RAM byte by how often its value
//! is the low byte of that frame's scroll while the screen is moving. The
//! byte the game keeps its camera in matches on almost every moving frame,
//! across many distinct values; nothing else does.
//!
//! The output is a TSV a person reads and `scripts/profile-census.py`
//! turns into profiles — the candidates are evidence, not answers, and
//! the census prints its ratios so a weak one is visible as weak.
//!
//! # Running it (never unattended — law 8 and boot_census's reasons)
//!
//! ```text
//! RF_PROFILE_GAMES=list.tsv RF_CENSUS_OUT=out.tsv \
//!   cargo test --release -p rf-harness --test profile_census -- --ignored --nocapture
//! ```
//!
//! `list.tsv` lines are `slug<TAB>archive path`. Each game runs in a child
//! process under a wall-clock cap. No ROM bytes and no paths are written
//! out — only slugs, hashes and addresses (law 5).

use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, EventMask, PpuPixel, Step};

const GAMES_VAR: &str = "RF_PROFILE_GAMES";
const ROM_VAR: &str = "RF_PCENSUS_ROM";
const OUT_VAR: &str = "RF_CENSUS_OUT";
const PER_ROM_TIMEOUT: Duration = Duration::from_secs(150);
/// Ninety seconds of game time: through a title, a file select or an
/// overworld map, and into play.
const FRAMES: u32 = 5400;
/// SNES work RAM searched: the low 8 KiB, where games keep their
/// per-frame state (`$7E0000-$7E1FFF`, mirrored into every bank's
/// `$0000-$1FFF`, fullsnes "Memory Map").
const SNES_SEARCH: usize = 0x2000;

/// Collects the frame's NES scroll writes.
#[derive(Default)]
struct ScrollSink {
    xs: Vec<u16>,
    ys: Vec<u16>,
}

impl CoreSink for ScrollSink {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, ev: CoreEvent) {
        if let CoreEvent::ScrollWrite { x, y, .. } = ev {
            self.xs.push(x);
            self.ys.push(y);
        }
    }
}

/// Per-offset evidence for one axis.
struct Axis {
    /// Matches on moving frames, same-frame and one-frame-late RAM.
    hits: Vec<[u32; 2]>,
    /// Which low-byte values matched, so a byte stuck at 0 cannot win by
    /// sitting under a scroll of 0.
    seen: Vec<[u64; 4]>,
    moving: u32,
    last: Vec<u8>,
}

impl Axis {
    fn new(len: usize) -> Self {
        Self {
            hits: vec![[0; 2]; len],
            seen: vec![[0; 4]; len],
            moving: 0,
            last: Vec::new(),
        }
    }

    /// Score this frame: `scroll` is every value the frame wrote, `ram`
    /// the RAM at its end, `prev` the RAM at the previous frame's end.
    fn frame(&mut self, scroll: &[u8], ram: &[u8], prev: &[u8]) {
        let now: Vec<u8> = {
            let mut v = scroll.to_vec();
            v.sort_unstable();
            v.dedup();
            v
        };
        if now.is_empty() || now == self.last {
            self.last = now;
            return;
        }
        self.moving += 1;
        for (at, (hit, seen)) in self.hits.iter_mut().zip(&mut self.seen).enumerate() {
            for (lag, src) in [ram, prev].into_iter().enumerate() {
                let Some(&b) = src.get(at) else { continue };
                if now.binary_search(&b).is_ok() {
                    hit[lag] += 1;
                    seen[usize::from(b >> 6)] |= 1 << (b & 63);
                }
            }
        }
        self.last = now;
    }

    /// The best offsets: `(offset, hits, distinct values)`.
    fn best(&self, n: usize) -> Vec<(usize, u32, u32)> {
        let mut v: Vec<(usize, u32, u32)> = self
            .hits
            .iter()
            .zip(&self.seen)
            .enumerate()
            .map(|(at, (h, s))| (at, h[0].max(h[1]), s.iter().map(|w| w.count_ones()).sum()))
            .filter(|&(_, _, distinct)| distinct >= 16)
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)));
        v.truncate(n);
        v
    }
}

/// Whether the byte after `at` carries when `at` wraps — the difference
/// between a 16-bit camera and an 8-bit one with its page kept elsewhere.
fn carries(history: &[(u8, u8)]) -> (u32, u32) {
    let (mut wraps, mut carried) = (0, 0);
    for pair in history.windows(2) {
        let ((lo0, hi0), (lo1, hi1)) = (pair[0], pair[1]);
        let fwd = lo0 >= 0xC0 && lo1 < 0x40;
        let back = lo0 < 0x40 && lo1 >= 0xC0;
        if fwd || back {
            wraps += 1;
            let want = if fwd {
                hi0.wrapping_add(1)
            } else {
                hi0.wrapping_sub(1)
            };
            if hi1 == want {
                carried += 1;
            }
        }
    }
    (wraps, carried)
}

enum Machine {
    Nes(Box<rf_nes::core::NesCore>),
    Snes(Box<rf_snes::core::SnesCore>),
}

#[test]
#[ignore = "child half of the profile census; the parent re-executes this"]
fn profile_census_child() {
    let Ok(path) = std::env::var(ROM_VAR) else {
        eprintln!("SKIP: {ROM_VAR} unset — this is the census's child half");
        return;
    };
    let Some(bytes) = rom_bytes(Path::new(&path)) else {
        println!("RESULT\tnorom");
        return;
    };
    let (mut m, hashes) = match rf_cart::Cartridge::load(&bytes) {
        Ok(rf_cart::Cartridge::Nes { .. }) => {
            match rf_nes::core::NesCore::from_ines_bytes(&bytes) {
                Ok(c) => (
                    Machine::Nes(Box::new(c)),
                    rf_cart::hash::identity_nes(&bytes),
                ),
                Err(_) => {
                    println!("RESULT\trefused");
                    return;
                }
            }
        }
        Ok(rf_cart::Cartridge::Snes { .. }) => match rf_snes::core::SnesCore::load(&bytes) {
            Ok(c) => (
                Machine::Snes(Box::new(c)),
                rf_cart::hash::identity_snes(&bytes),
            ),
            Err(_) => {
                println!("RESULT\trefused");
                return;
            }
        },
        Err(_) => {
            println!("RESULT\trefused");
            return;
        }
    };
    let snes = matches!(m, Machine::Snes(_));
    let len = if snes { SNES_SEARCH } else { 0x800 };
    let (mut ax, mut ay) = (Axis::new(len), Axis::new(len));
    let mut prev = vec![0u8; len];
    let mut stalled = 0u32;
    let mut history: Vec<Vec<u8>> = Vec::new();
    if let Machine::Nes(c) = &mut m {
        // The NES pushes its mask through the bus, not `CoreConfig`
        // (the shell's own `EmuStepper` does the same).
        c.bus_mut().set_event_mask(EventMask::SCROLL_WRITE);
    }
    let mut sink = ScrollSink::default();
    // Bounded: exactly FRAMES iterations, each a cycle-budgeted frame (law 8).
    for f in 0..FRAMES {
        // Stuck (a title, a menu, a map, or a pause this script caused):
        // cycle Start, A, Right, B — enough to leave most titles, pick a
        // file and step onto a map's first level. Otherwise Right, with
        // B held to run and A tapped to hop over things.
        let menu = (stalled > 60).then_some((stalled / 40) % 4);
        let phase = stalled % 40 < 6;
        let start = menu == Some(0) && phase;
        let menu_a = menu == Some(1) && phase;
        let menu_b = menu == Some(3) && phase;
        let right = (menu.is_none() && f > 240) || menu == Some(2);
        let jump = (menu.is_none() && right && f % 48 < 18) || menu_a;
        let run = (menu.is_none() && right) || menu_b;
        sink.xs.clear();
        sink.ys.clear();
        let (xs, ys) = match &mut m {
            Machine::Nes(c) => {
                let mut pad = 0u8;
                if start {
                    pad |= 0x08;
                }
                if right {
                    pad |= 0x80;
                }
                if run {
                    pad |= 0x02;
                }
                if jump {
                    pad |= 0x01;
                }
                let input = rf_core_api::InputFrame {
                    ports: [u16::from(pad), 0, 0, 0],
                };
                c.run_frame(&input, &mut sink);
                (
                    sink.xs
                        .iter()
                        .map(|v| (v & 0xFF) as u8)
                        .collect::<Vec<u8>>(),
                    sink.ys
                        .iter()
                        .map(|v| (v & 0xFF) as u8)
                        .collect::<Vec<u8>>(),
                )
            }
            Machine::Snes(c) => {
                // `$4218` layout (fullsnes "Joypad"): B=15, Start=12,
                // Right=8, A=7, Y=14 (run).
                let mut pad = 0u16;
                if start {
                    pad |= 1 << 12;
                }
                if right {
                    pad |= 1 << 8;
                }
                if run {
                    pad |= 1 << 14;
                }
                if jump {
                    pad |= 1 << 15;
                }
                if menu_a {
                    pad |= 1 << 7;
                }
                c.system_mut().bus.joypads.ports[0] = pad;
                let _ = c.step(Step::Frame, &mut sink);
                let bgs = &c.system().bus.ppu.bgs;
                (
                    vec![(bgs[0].hofs & 0xFF) as u8],
                    vec![(bgs[0].vofs & 0xFF) as u8],
                )
            }
        };
        let ram: Vec<u8> = match &m {
            Machine::Nes(c) => (0..len).map(|a| c.peek(a as u32)).collect(),
            Machine::Snes(c) => c.system().bus.wram[..len].to_vec(),
        };
        let moved_before = ax.moving;
        ax.frame(&xs, &ram, &prev);
        ay.frame(&ys, &ram, &prev);
        stalled = if ax.moving > moved_before {
            0
        } else {
            stalled + 1
        };
        if std::env::var("RF_PCENSUS_DEBUG").is_ok() && f % 300 == 0 {
            eprintln!(
                "DBG f={f} xs={xs:?} ys={ys:?} moving={} stalled={stalled}",
                ax.moving
            );
        }
        history.push(ram.clone());
        prev = ram;
    }

    let base: u32 = if snes { 0x7E_0000 } else { 0 };
    let mut out = format!(
        "ok\t{}\t{}\t{}\t{}",
        hashes.normalized.sha256,
        hashes.normalized.sha1,
        hashes.normalized.md5,
        hashes.normalized.crc32
    );
    for (name, axis) in [("x", &ax), ("y", &ay)] {
        let mut cells = Vec::new();
        for (at, hits, distinct) in axis.best(3) {
            let pairs: Vec<(u8, u8)> = history
                .iter()
                .map(|r| (r[at], r.get(at + 1).copied().unwrap_or(0)))
                .collect();
            let (wraps, carried) = carries(&pairs);
            cells.push(format!(
                "{:X}:{hits}:{distinct}:{wraps}:{carried}",
                base + at as u32
            ));
        }
        out.push_str(&format!("\t{name}={}/{}", axis.moving, cells.join(",")));
    }
    println!("RESULT\t{out}");
}

#[test]
#[ignore = "local: plays real dumps, one child process per title"]
fn profile_census() {
    if std::env::var(ROM_VAR).is_ok() {
        return;
    }
    let Ok(list) = std::env::var(GAMES_VAR) else {
        eprintln!("SKIP: set {GAMES_VAR} to a slug<TAB>archive list");
        return;
    };
    let exe = std::env::current_exe().expect("own path");
    let text = std::fs::read_to_string(&list).expect("game list");
    let out = std::env::var(OUT_VAR).ok();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Some((slug, archive)) = line.split_once('\t') else {
            continue;
        };
        let started = Instant::now();
        let row = format!("{slug}\t{}", run_one(&exe, Path::new(archive)));
        println!("{row}  ({:.0}s)", started.elapsed().as_secs_f32());
        if let Some(out) = &out {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(out)
            {
                let _ = writeln!(f, "{row}");
            }
        }
    }
}

/// One title in a child process; its `RESULT` line, or why there is none.
fn run_one(exe: &Path, archive: &Path) -> String {
    let child = Command::new(exe)
        .args([
            "--exact",
            "profile_census_child",
            "--ignored",
            "--nocapture",
        ])
        .env(ROM_VAR, archive)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return "crashed".into();
    };
    let deadline = Instant::now() + PER_ROM_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => return "crashed".into(),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return "timeout".into();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut stdout = String::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_string(&mut stdout);
    }
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("RESULT\t"))
        .map_or_else(|| "crashed".into(), str::to_owned)
}

/// The cartridge inside `path` (bare or zipped), as boot_census reads it.
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
