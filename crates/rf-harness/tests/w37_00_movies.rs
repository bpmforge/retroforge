//! **W37-00: input-movie coverage import on real cartridges** (ROM- and
//! movie-gated, law 5; every test is `#[ignore]` and skips when its files are
//! absent).
//!
//! Movies are inputs only. They are parsed (`rf_harness::movie`), laid out
//! against a boot (`movie_replay::plan`), replayed with the coverage recorder
//! armed, merged into the W36 script coverage and baked into an atlas per ROM
//! hash. Only counts, frame numbers and (world, level) / translevel numbers
//! are printed; nothing from a movie or a ROM is stored (D-021).
//!
//! ## What "desync" means here
//!
//! A movie is only a faithful script on the emulator it was made on. Ours is
//! another emulator, so a replay can drift. There is no ground-truth trace to
//! diff against, so each game gets a **heuristic** over its RAM facts (the
//! profile's `lives`, `world`, `level`; `$0770` / `$0100` game mode):
//!
//! * SMB (high confidence): lives drop; game over; back to the title or demo;
//!   world/level going backwards. (medium) no progress for 3000 frames.
//!   Success is the victory mode (`$0770 == 2`) in 8-4.
//! * SMW (medium confidence): game mode back to the title/file-select range
//!   after play began; no progress for 6000 frames. Lives drops are recorded
//!   but are not a stop (a SMW run may lose a life on purpose).
//!
//! A symptom lags the real drift (a death animation, a stall timer), so the
//! reported frame is the first the heuristic could see, an upper bound on
//! where the run left the movie. Coverage is kept up to that frame; it is
//! real executed code either way.
//!
//! ```text
//! cargo test --release -p rf-harness --test w37_00_movies smb -- --ignored --nocapture
//! cargo test --release -p rf-harness --test w37_00_movies smw -- --ignored --nocapture
//! ```

use std::collections::BTreeSet;
use std::io::Read as _;
use std::path::PathBuf;
use std::time::Instant;

use rf_cache::Cache;
use rf_core_api::coverage::CoverageLog;
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_harness::atlas::{build_nes_atlas, build_snes_atlas, store_atlas};
use rf_harness::movie::{self, Movie};
use rf_harness::movie_replay::{plan, replay_nes, replay_snes, Plan};
use rf_nes::core::NesCore;
use rf_nes::CpuBus as _;
use rf_snes::core::SnesCore;

mod smw_drive;

// ---------------------------------------------------------------- locations

fn roms_root() -> PathBuf {
    PathBuf::from(
        std::env::var("RF_SCREENS_ROMS_DIR")
            .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string()),
    )
}

/// A movie file: the env var if set, else the default under `tas/`.
fn movie_path(var: &str, default: &str) -> Option<PathBuf> {
    let p =
        std::env::var(var).map_or_else(|_| roms_root().join("tas").join(default), PathBuf::from);
    p.exists().then_some(p)
}

/// The first image in the zip that `rf-cart` accepts as `want`.
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

fn atlas_cache_dir() -> PathBuf {
    std::env::var("RF_W37_CACHE_DIR").map_or_else(
        |_| std::env::temp_dir().join("retroforge-w37-atlas-cache"),
        PathBuf::from,
    )
}

struct Null;

impl CoreSink for Null {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

// ------------------------------------------------------------- the outcome

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Conf {
    High,
    Medium,
}

#[derive(Debug, Clone)]
struct Symptom {
    /// Movie frame the heuristic first saw it on.
    movie_frame: usize,
    why: &'static str,
    conf: Conf,
}

#[derive(Debug, Default, Clone)]
struct Outcome {
    /// The game was seen in play at all (SMB `$0770 == 1`, SMW mode `0x14`).
    /// A run that never got there is the worst candidate, not a quiet one:
    /// SMB sits in its attract demo when the title Start is missed.
    armed: bool,
    /// Movie frames run (the run may stop on a symptom, success or the cap).
    ran: usize,
    symptom: Option<Symptom>,
    /// The game's own end state was reached (victory in SMB).
    finished: bool,
    /// (world, level) zero-indexed for SMB; translevel numbers for SMW.
    levels: BTreeSet<(u8, u8)>,
    /// Highest level index reached (SMB: world * 4 + level).
    best_index: usize,
    /// Furthest world x seen in that level (SMB only).
    best_x: u16,
    last_mode: u8,
    last_lives: u8,
    first_life_drop: Option<usize>,
}

impl Outcome {
    /// Frames the run stayed consistent with the movie.
    fn survived(&self) -> usize {
        if !self.armed {
            return 0;
        }
        self.symptom.as_ref().map_or(self.ran, |s| s.movie_frame)
    }

    fn describe(&self) -> String {
        match &self.symptom {
            Some(s) => format!("{} ({:?}) at movie frame {}", s.why, s.conf, s.movie_frame),
            None if self.finished => "none, reached the ending".to_string(),
            None => "none within the frames run".to_string(),
        }
    }
}

// ------------------------------------------------------------------- SMB

mod smb {
    use super::*;

    pub const FILE: &str = "Super Mario Bros. + Duck Hunt (USA).zip";
    pub const START: u16 = 1 << 3;
    const LIVES: u32 = 0x075A;
    const WORLD: u32 = 0x075F;
    const LEVEL: u32 = 0x0760;
    const MODE: u32 = 0x0770;
    const AREA: u32 = 0x0750;
    const PAGE: u32 = 0x006D;
    const X: u32 = 0x0086;
    const STALL: usize = 3000;

    /// Watches one SMB run (module doc). Facts: the profile's `lives`,
    /// `world`, `level` rows; `$0770` is 0 title/demo, 1 playing, 2 victory,
    /// 3 game over (SMB RAM map, Data Crystal).
    pub struct Watch {
        movie_start: usize,
        cap: usize,
        armed: bool,
        prev_lives: u8,
        key: (u8, u8, u8, u8),
        best_x: u16,
        last_progress: usize,
        pub out: Outcome,
    }

    impl Watch {
        pub fn new(movie_start: usize, cap: usize) -> Self {
            Watch {
                movie_start,
                cap,
                armed: false,
                prev_lives: 0,
                key: (0, 0, 0, 0),
                best_x: 0,
                last_progress: 0,
                out: Outcome::default(),
            }
        }

        fn flag(&mut self, mf: usize, why: &'static str, conf: Conf) {
            if self.out.symptom.is_none() {
                self.out.symptom = Some(Symptom {
                    movie_frame: mf,
                    why,
                    conf,
                });
            }
        }

        /// `false` ends the run.
        pub fn step(&mut self, f: usize, core: &NesCore) -> bool {
            let Some(mf) = f.checked_sub(self.movie_start) else {
                return true;
            };
            self.out.ran = mf + 1;
            let (w, l) = (core.peek(WORLD), core.peek(LEVEL));
            let (mode, lives) = (core.peek(MODE), core.peek(LIVES));
            self.out.last_mode = mode;
            self.out.last_lives = lives;
            if !self.armed {
                if mode == 1 {
                    self.armed = true;
                    self.out.armed = true;
                    self.prev_lives = lives;
                    self.last_progress = mf;
                }
                return mf < self.cap;
            }
            let x = u16::from(core.peek(PAGE)) << 8 | u16::from(core.peek(X));
            if mode == 1 {
                self.out.levels.insert((w, l));
                let idx = usize::from(w) * 4 + usize::from(l);
                if idx > self.out.best_index {
                    self.out.best_index = idx;
                    self.out.best_x = 0;
                }
                if idx == self.out.best_index {
                    self.out.best_x = self.out.best_x.max(x);
                }
            }
            if mode == 2 && (w, l) == (7, 3) {
                self.out.finished = true;
                return false;
            }
            if lives < self.prev_lives {
                self.out.first_life_drop.get_or_insert(mf);
                self.flag(mf, "lives dropped", Conf::High);
            }
            self.prev_lives = lives;
            if mode == 3 {
                self.flag(mf, "game over", Conf::High);
            }
            if mode == 0 {
                self.flag(mf, "back to title or demo", Conf::High);
            }
            if usize::from(w) * 4 + usize::from(l) < self.out.best_index && mode == 1 {
                self.flag(mf, "level index went backwards", Conf::High);
            }
            let key = (w, l, mode, core.peek(AREA));
            if key != self.key || x > self.best_x {
                if key != self.key {
                    self.best_x = 0;
                }
                self.key = key;
                self.best_x = self.best_x.max(x);
                self.last_progress = mf;
            } else if mf - self.last_progress > STALL {
                self.flag(
                    self.last_progress,
                    "no progress for 3000 frames",
                    Conf::Medium,
                );
            }
            self.out.symptom.is_none() && mf < self.cap
        }
    }

    pub fn run(raw: &[u8], pl: &Plan, cap: usize, record: bool) -> (Outcome, Option<CoverageLog>) {
        let mut w = Watch::new(pl.movie_start, cap);
        let r = replay_nes(raw, pl, record, |f, c| w.step(f as usize, c)).expect("loads");
        (w.out, r.coverage)
    }

    // ---- the W36-04 drive scripts (the "before" coverage) -----------------

    const LEAD_IN: u32 = 500;
    pub const FRAMES: u32 = LEAD_IN + 1200;

    fn script_one(frame: u32) -> u8 {
        match frame {
            0..=199 => 0,
            200..=204 | 400..=404 => 8,
            205..=399 | 405..=499 => 0,
            f if (f - 500) % 60 < 12 => 0x80 | 1,
            _ => 0x80,
        }
    }

    fn script_two(frame: u32) -> u8 {
        match frame {
            0..=199 => 0,
            200..=204 | 400..=404 => 8,
            205..=399 | 405..=499 => 0,
            f => {
                let t = f - 500;
                let jump = t % 90 < 20 || t % 150 < 6;
                0x80 | 2 | u8::from(jump)
            }
        }
    }

    fn record_script(raw: &[u8], script: fn(u32) -> u8, level: Option<(u8, u8)>) -> CoverageLog {
        let mut core = NesCore::from_ines_bytes(raw).expect("loads");
        core.set_coverage_recording(true);
        core.reset(ResetKind::Hard);
        for f in 0..FRAMES {
            // Test-side level entry, as w36_04_lockstep_smb: write the level
            // into RAM during the title screen's second Start.
            if let (Some((w, l)), true) = (level, (380..=404).contains(&f)) {
                core.bus_mut().write(0x075F, w);
                core.bus_mut().write(0x0760, l);
            }
            let mut i = InputFrame::empty();
            i.ports[0] = u16::from(script(f));
            core.run_frame(&i, &mut Null);
        }
        core.take_coverage().expect("armed")
    }

    /// The W36 script-only coverage: both drive scripts through all 32
    /// levels (the union `w36_04_lockstep_smb` builds its atlas from).
    pub fn baseline(raw: &[u8]) -> CoverageLog {
        let mut union: Option<CoverageLog> = None;
        for w in 0..8u8 {
            for l in 0..4u8 {
                for s in [script_one as fn(u32) -> u8, script_two] {
                    let log = record_script(raw, s, Some((w, l)));
                    match union.as_mut() {
                        Some(u) => {
                            u.merge(&log);
                        }
                        None => union = Some(log),
                    }
                }
            }
        }
        union.expect("recorded")
    }
}

// ----------------------------------------------------------------- sweeps

/// Narrow `offsets` stage by stage: at each cap keep the candidates with the
/// best (frames consistent with the movie, level index) pair; the winner is
/// the smallest offset left after the last cap. Returns it with the table of
/// the first stage.
fn sweep(
    offsets: impl Iterator<Item = i32>,
    caps: &[usize],
    mut run: impl FnMut(i32, usize) -> Outcome,
) -> (i32, Vec<(i32, usize, u16)>) {
    let mut alive: Vec<i32> = offsets.collect();
    let mut first_stage = Vec::new();
    for (stage, &cap) in caps.iter().enumerate() {
        let t = Instant::now();
        // (offset, rank) where rank = (level index, furthest x in it, frames
        // consistent with the movie): a run that merely idles safely loses to
        // one that got further.
        let rows: Vec<(i32, (usize, u16, usize))> = alive
            .iter()
            .map(|&d| {
                let o = run(d, cap);
                (d, (o.best_index, o.best_x, o.survived()))
            })
            .collect();
        if stage == 0 {
            first_stage = rows.iter().map(|r| (r.0, r.1 .0, r.1 .1)).collect();
        }
        let best = rows.iter().map(|r| r.1).max().expect("candidates");
        alive = rows.iter().filter(|r| r.1 == best).map(|r| r.0).collect();
        println!(
            "  sweep stage {stage} (cap {cap}): {} candidates, best level index {} / x {} / consistent frames {}, \
             {} tie, {:.0?}",
            rows.len(),
            best.0,
            best.1,
            best.2,
            alive.len(),
            t.elapsed()
        );
    }
    (*alive.iter().min().expect("a winner"), first_stage)
}

/// `lead`, then the movie slid by `offset` frames: positive idles that many
/// frames first, negative drops that many movie frames from the front (only
/// sensible while those are idle).
fn laid_out(lead: &[u16], offset: i32, m: &Movie) -> Plan {
    match usize::try_from(offset) {
        Ok(d) => plan(lead, d, 0, m),
        Err(_) => plan(lead, 0, offset.unsigned_abs() as usize, m),
    }
}

struct Row {
    label: String,
    movie_frames: usize,
    ran: usize,
    outcome: Outcome,
    alignment: String,
    prg_before: f64,
    prg_movie_only: f64,
    prg_after: f64,
    fns_before: usize,
    fns_after: usize,
}

fn print_row(r: &Row) {
    let o = &r.outcome;
    println!(
        "ROW | {} | {} of {} | {} | {} levels, best index {} | {} | PRG {:.1}% -> {:.1}% (movie alone {:.1}%) | functions {} -> {}",
        r.label,
        r.ran,
        r.movie_frames,
        o.describe(),
        o.levels.len(),
        o.best_index,
        r.alignment,
        r.prg_before * 100.0,
        r.prg_after * 100.0,
        r.prg_movie_only * 100.0,
        r.fns_before,
        r.fns_after
    );
    println!("     levels entered: {:?}", o.levels);
    if let Some(d) = o.first_life_drop {
        println!("     first life drop at movie frame {d}");
    }
}

fn load_movie(parse: impl FnOnce() -> Result<Movie, movie::MovieError>, what: &str) -> Movie {
    let m = parse().unwrap_or_else(|e| panic!("{what}: {e}"));
    println!(
        "{what}: {:?}, {} frames, {} re-records, {} dropped sub-frames, claims ROM {:?}",
        m.format,
        m.frames.len(),
        m.rerecords.unwrap_or(0),
        m.dropped_subframes,
        m.rom_hint
    );
    m
}

// -------------------------------------------------------------- SMB test

#[test]
#[ignore = "needs the SMB multicart ROM and the .fm2 movies; many minutes in release"]
fn smb_fm2_movies_replay_and_bake() {
    let rom_file = roms_root().join("nes").join(smb::FILE);
    let Some(raw) = rom_from_zip(&rom_file, true) else {
        println!("SKIP: {} not found", smb::FILE);
        return;
    };
    let movies: Vec<(&str, Movie)> = [
        (
            "warpless",
            "RF_W37_SMB_WARPLESS",
            "3728/x/happylee_mars608-smb-warpless.fm2",
        ),
        (
            "warps",
            "RF_W37_SMB_WARPS",
            "1715/x/happylee-supermariobros,warped.fm2",
        ),
    ]
    .into_iter()
    .filter_map(|(name, var, def)| {
        let p = movie_path(var, def)?;
        let text = std::fs::read_to_string(&p).ok()?;
        Some((name, load_movie(|| movie::fm2::parse(&text), name)))
    })
    .collect();
    if movies.is_empty() {
        println!("SKIP: no .fm2 movies found");
        return;
    }

    let t = Instant::now();
    let base = smb::baseline(&raw);
    let atlas0 = build_nes_atlas(&raw, &base).expect("atlas");
    println!(
        "baseline (W36 scripts, 64 runs) in {:.0?}: PRG {:.1}%, {} atlas functions",
        t.elapsed(),
        base.prg_coverage_fraction() * 100.0,
        atlas0.functions.len()
    );

    // Pre-roll: idle, then Start leaves the multicart's game select (the menu
    // ignores Start before about frame 120; found by probing, and a movie
    // that starts earlier presses that Start itself and never reaches SMB's
    // title). The sweep then slides movie frame 0 over a window after it.
    let lead: Vec<u16> = std::iter::repeat_n(0u16, 160)
        .chain(std::iter::repeat_n(smb::START, 5))
        .collect();

    let mut cumulative = base.clone();
    let mut rows = Vec::new();
    for (name, m) in &movies {
        println!("== {name}: aligning on the multicart");
        let (delay, table) = sweep(-22..=20, &[1200, 5000, 20000], |d, cap| {
            smb::run(&raw, &laid_out(&lead, d, m), cap, false).0
        });
        let spread: Vec<String> = table
            .iter()
            .map(|(d, s, i)| format!("{d}:{s}/{i}"))
            .collect();
        println!(
            "  first stage (offset:level-index/furthest-x): {}",
            spread.join(" ")
        );
        let pl = laid_out(&lead, delay, m);
        println!(
            "  chosen offset {delay} (movie frame {} at emulator frame {})",
            pl.skipped, pl.movie_start
        );

        let t = Instant::now();
        let (out, log) = smb::run(&raw, &pl, m.frames.len(), true);
        let log = log.expect("armed");
        println!("  full replay in {:.0?}: {}", t.elapsed(), out.describe());
        let mut merged = base.clone();
        assert!(merged.merge(&log), "same cartridge");
        let atlas_m = build_nes_atlas(&raw, &merged).expect("atlas");
        cumulative.merge(&log);
        rows.push(Row {
            label: format!("SMB {name}"),
            movie_frames: m.frames.len(),
            ran: out.ran,
            outcome: out,
            alignment: format!("offset {delay} after the menu Start"),
            prg_before: base.prg_coverage_fraction(),
            prg_movie_only: log.prg_coverage_fraction(),
            prg_after: merged.prg_coverage_fraction(),
            fns_before: atlas0.functions.len(),
            fns_after: atlas_m.functions.len(),
        });
    }

    let atlas = build_nes_atlas(&raw, &cumulative).expect("atlas");
    let sha = rf_harness::hex_sha256(&raw);
    let dir = atlas_cache_dir();
    let mut cache = Cache::open(&dir, 64 << 20).expect("cache");
    store_atlas(&mut cache, &sha, &atlas).expect("stores");
    println!(
        "baked: {} functions, PRG {:.1}% (baseline + {} movies) into {}",
        atlas.functions.len(),
        cumulative.prg_coverage_fraction() * 100.0,
        movies.len(),
        dir.display()
    );
    for r in &rows {
        print_row(r);
    }
}

// ------------------------------------------------------------------- SMW

mod smw {
    use super::*;
    use rf_input::SnesButton;

    pub const FILE: &str = "Super Mario World (USA).zip";
    const MODE: u32 = 0x7E_0100;
    const LIVES: u32 = 0x7E_0DBE;
    /// The translevel number: SMW Central's RAM map, `$7E13BF`. (The
    /// profile has no row for it; game facts live in tests.)
    const TRANSLEVEL: u32 = 0x7E_13BF;
    const PLAYER_X: u32 = 0x7E_0094;
    const IN_LEVEL: u8 = 0x14;
    /// Game modes at or below this are the title/file-select range
    /// (`$0100`: 0x0E is the overworld, 0x14 a level).
    const TITLE_RANGE: u8 = 0x0A;
    const STALL: usize = 6000;

    pub struct Watch {
        movie_start: usize,
        cap: usize,
        armed: bool,
        prev_lives: u8,
        key: (u8, u8),
        best_x: u16,
        last_progress: usize,
        pub modes: BTreeSet<u8>,
        pub out: Outcome,
    }

    impl Watch {
        pub fn new(movie_start: usize, cap: usize) -> Self {
            Watch {
                movie_start,
                cap,
                armed: false,
                prev_lives: 0,
                key: (0, 0),
                best_x: 0,
                last_progress: 0,
                modes: BTreeSet::new(),
                out: Outcome::default(),
            }
        }

        fn flag(&mut self, mf: usize, why: &'static str, conf: Conf) {
            if self.out.symptom.is_none() {
                self.out.symptom = Some(Symptom {
                    movie_frame: mf,
                    why,
                    conf,
                });
            }
        }

        pub fn step(&mut self, f: usize, core: &SnesCore) -> bool {
            let Some(mf) = f.checked_sub(self.movie_start) else {
                return true;
            };
            self.out.ran = mf + 1;
            let mode = core.peek(MODE);
            let lives = core.peek(LIVES);
            let tl = core.peek(TRANSLEVEL);
            self.out.last_mode = mode;
            self.out.last_lives = lives;
            self.modes.insert(mode);
            if !self.armed {
                if mode == IN_LEVEL {
                    self.armed = true;
                    self.out.armed = true;
                    self.prev_lives = lives;
                    self.last_progress = mf;
                } else if mf >= STALL {
                    self.flag(mf, "no level entered in 6000 frames", Conf::Medium);
                }
                return self.out.symptom.is_none() && mf < self.cap;
            }
            if mode == IN_LEVEL {
                self.out.levels.insert((0, tl));
                self.out.best_index = self.out.levels.len();
            }
            if lives < self.prev_lives {
                self.out.first_life_drop.get_or_insert(mf);
            }
            self.prev_lives = lives;
            if mode <= TITLE_RANGE {
                self.flag(mf, "back to the title or file select", Conf::Medium);
            }
            let x = u16::from(core.peek(PLAYER_X)) | u16::from(core.peek(PLAYER_X + 1)) << 8;
            let key = (mode, if mode == IN_LEVEL { tl } else { 0 });
            if key != self.key || x > self.best_x {
                if key != self.key {
                    self.best_x = 0;
                }
                self.key = key;
                self.best_x = self.best_x.max(x);
                self.last_progress = mf;
            } else if mf - self.last_progress > STALL {
                self.flag(
                    self.last_progress,
                    "no progress for 6000 frames",
                    Conf::Medium,
                );
            }
            self.out.symptom.is_none() && mf < self.cap
        }
    }

    pub fn run(
        raw: &[u8],
        pl: &Plan,
        cap: usize,
        record: bool,
    ) -> (Outcome, BTreeSet<u8>, Option<CoverageLog>) {
        let mut w = Watch::new(pl.movie_start, cap);
        let r = replay_snes(raw, pl, record, |f, c| w.step(f as usize, c)).expect("loads");
        (w.out, w.modes, r.coverage)
    }

    // ---- the W36-08 drive script (the "before" coverage) -------------------

    /// Menus to Yoshi's Island 1 (`smw_drive::level_pad`, which waits on the
    /// game mode) and play: the token script of
    /// `crates/retroforge/tests/w33_07_common/mod.rs` (`SMW_PLAY`), as
    /// `w36_08_atlas_smw` plays them. `wN` idles N frames, `<buttons>N` holds
    /// them (`A B X Y S E U D L R`).
    const PLAY: &str = "L2 w3 B8 w52 B8 w52 B8 w52 B8 w52 R+B14 R22 R+B14 R22 S6 w100 S6 w60 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22";

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
                        "A" => SnesButton::A,
                        "B" => SnesButton::B,
                        "X" => SnesButton::X,
                        "Y" => SnesButton::Y,
                        "S" => SnesButton::Start,
                        "E" => SnesButton::Select,
                        "U" => SnesButton::Up,
                        "D" => SnesButton::Down,
                        "L" => SnesButton::Left,
                        "R" => SnesButton::Right,
                        other => panic!("button {other}"),
                    }
                    .bit();
                    m | (1 << bit)
                });
                (mask, n.parse().expect("N"))
            };
            out.extend(std::iter::repeat_n(mask, n));
        }
        out
    }

    pub fn baseline(raw: &[u8]) -> CoverageLog {
        let mut pad = super::smw_drive::level_pad(raw);
        let level_start = pad.len();
        pad.extend(frames_of(PLAY));
        let more = frames_of("R+B14 R22");
        while pad.len() < level_start + 1500 {
            pad.extend(&more);
        }
        let mut core = SnesCore::load(raw).expect("loads");
        core.reset(ResetKind::Hard);
        core.set_coverage_recording(true);
        for &mask in &pad {
            core.system_mut().bus.joypads.ports[0] = mask;
            core.run_frame(&InputFrame::empty(), &mut Null);
        }
        core.take_coverage().expect("armed")
    }
}

#[test]
#[ignore = "needs Super Mario World (USA) and the .lsmv/.smv movies; many minutes in release"]
fn smw_lsmv_and_smv_movies_replay_and_bake() {
    let rom_file = roms_root().join("snes").join(smw::FILE);
    let Some(raw) = rom_from_zip(&rom_file, false) else {
        println!("SKIP: {} not found", smw::FILE);
        return;
    };
    let mut movies: Vec<(&str, Movie)> = Vec::new();
    if let Some(p) = movie_path("RF_W37_SMW_LSMV", "4144/x/brunovisnadi-smw-warps.lsmv") {
        let bytes = std::fs::read(p).expect("reads");
        movies.push((
            "warps (lsnes)",
            load_movie(|| movie::lsmv::parse_archive(&bytes), "lsmv"),
        ));
    }
    if let Some(p) = movie_path(
        "RF_W37_SMW_SMV",
        "2307/x/bahamete,kaizoman666,masterjun-supermarioworld-96exit.smv",
    ) {
        let bytes = std::fs::read(p).expect("reads");
        movies.push((
            "96 exits (Snes9x)",
            load_movie(|| movie::smv::parse(&bytes), "smv"),
        ));
    }
    if movies.is_empty() {
        println!("SKIP: no SMW movies found");
        return;
    }

    let t = Instant::now();
    let base = smw::baseline(&raw);
    let atlas0 = build_snes_atlas(&raw, &base).expect("atlas");
    println!(
        "baseline (W36-08 script) in {:.0?}: PRG {:.1}%, {} atlas functions",
        t.elapsed(),
        base.prg_coverage_fraction() * 100.0,
        atlas0.functions.len()
    );

    let mut cumulative = base.clone();
    let mut rows = Vec::new();
    for (name, m) in &movies {
        println!("== {name}: lining up from power-on");
        // Power-on to power-on: only a few frames of slack can be wrong.
        let (delay, table) = sweep(0..=4, &[4000, 30000], |d, cap| {
            smw::run(&raw, &laid_out(&[], d, m), cap, false).0
        });
        let spread: Vec<String> = table
            .iter()
            .map(|(d, s, i)| format!("{d}:{s}/{i}"))
            .collect();
        println!("  first stage (offset:levels/-): {}", spread.join(" "));
        let pl = laid_out(&[], delay, m);
        let t = Instant::now();
        let (out, modes, log) = smw::run(&raw, &pl, m.frames.len(), true);
        let log = log.expect("armed");
        println!(
            "  full replay in {:.0?}: {}; ended in mode {:#04x}, lives byte {}; modes seen {:02x?}",
            t.elapsed(),
            out.describe(),
            out.last_mode,
            out.last_lives,
            modes
        );
        let mut merged = base.clone();
        assert!(merged.merge(&log), "same cartridge");
        let atlas_m = build_snes_atlas(&raw, &merged).expect("atlas");
        cumulative.merge(&log);
        rows.push(Row {
            label: format!("SMW {name}"),
            movie_frames: m.frames.len(),
            ran: out.ran,
            outcome: out,
            alignment: format!("power-on, offset {delay}"),
            prg_before: base.prg_coverage_fraction(),
            prg_movie_only: log.prg_coverage_fraction(),
            prg_after: merged.prg_coverage_fraction(),
            fns_before: atlas0.functions.len(),
            fns_after: atlas_m.functions.len(),
        });
    }

    let atlas = build_snes_atlas(&raw, &cumulative).expect("atlas");
    let sha = rf_harness::hex_sha256(&raw);
    let dir = atlas_cache_dir();
    let mut cache = Cache::open(&dir, 64 << 20).expect("cache");
    store_atlas(&mut cache, &sha, &atlas).expect("stores");
    println!(
        "baked: {} functions, PRG {:.1}% (baseline + {} movies) into {}",
        atlas.functions.len(),
        cumulative.prg_coverage_fraction() * 100.0,
        movies.len(),
        dir.display()
    );
    for r in &rows {
        print_row(r);
    }
}

/// The four movies parse, with the frame counts the files carry (counts are
/// facts about the files, not content).
#[test]
#[ignore = "needs the four movie files (RF_W37_* or ~/Games/Roms/tas)"]
fn the_four_real_movies_parse() {
    let mut seen = 0;
    if let Some(p) = movie_path(
        "RF_W37_SMB_WARPLESS",
        "3728/x/happylee_mars608-smb-warpless.fm2",
    ) {
        let m = movie::fm2::parse(&std::fs::read_to_string(p).unwrap()).expect("fm2");
        assert_eq!(m.frames.len(), 67117);
        assert_eq!(m.frames[0].reset, Some(movie::Reset::Soft));
        seen += 1;
    }
    if let Some(p) = movie_path(
        "RF_W37_SMB_WARPS",
        "1715/x/happylee-supermariobros,warped.fm2",
    ) {
        let m = movie::fm2::parse(&std::fs::read_to_string(p).unwrap()).expect("fm2");
        assert_eq!(m.frames.len(), 17868);
        seen += 1;
    }
    if let Some(p) = movie_path("RF_W37_SMW_LSMV", "4144/x/brunovisnadi-smw-warps.lsmv") {
        let m = movie::lsmv::parse_archive(&std::fs::read(p).unwrap()).expect("lsmv");
        assert_eq!((m.frames.len(), m.dropped_subframes), (35720, 0));
        assert!(m.frames.iter().all(|f| f.reset.is_none()));
        // The movie names its ROM by SHA-256: is ours that ROM? (Printed, not
        // asserted: a different dump is a finding, not a failure.)
        if let Some(raw) = rom_from_zip(&roms_root().join("snes").join(smw::FILE), false) {
            let norm = match rf_cart::Cartridge::load(&raw) {
                Ok(rf_cart::Cartridge::Snes { identity, .. }) => identity.normalized.sha256,
                _ => String::new(),
            };
            let claim = m.rom_hint.clone().unwrap_or_default();
            println!(
                "lsmv ROM hash claim matches: raw image {}, normalized image {}",
                claim == rf_harness::hex_sha256(&raw),
                claim == norm
            );
        }
        seen += 1;
    }
    if let Some(p) = movie_path(
        "RF_W37_SMW_SMV",
        "2307/x/bahamete,kaizoman666,masterjun-supermarioworld-96exit.smv",
    ) {
        let m = movie::smv::parse(&std::fs::read(p).unwrap()).expect("smv");
        assert_eq!(m.frames.len(), 269_100);
        // Bits 0-3 of a SNES pad word are always clear.
        assert!(m.frames.iter().all(|f| f.pads[0] & 0xF == 0));
        seen += 1;
    }
    if seen == 0 {
        println!("SKIP: no movies found");
    }
}
