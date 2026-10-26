//! **W37-03: the acceptance census through the app path** (ignored, release).
//!
//! For a seeded sample of the ROM library (NES and SNES) this opens each game
//! in the real `RetroForgeApp`, in Enhanced mode with every other enhancement
//! off, and steps it to a fixed frame with no input, twice, each time in a
//! child process of its own:
//!
//! - **off**: Rebuild stays off;
//! - **on**: Rebuild is turned on the way the Enhancements card's switch does
//!   it (`set_rebuild_for_test` writes the per-game setting, `sync_rebuild`
//!   sends the command, the core thread builds and installs the atlas), and
//!   the run waits, stepping slowly, until the first atlas is installed.
//!
//! Then it compares the hash of the picture the app shows at that frame.
//! **Any difference is a regression.** The run also records how much of the
//! work went through the atlas (observed + inferred share of instructions), so
//! "zero regressions" cannot hide a Rebuild that never engaged.
//!
//! Children, because a panic, abort or hang costs one row (the `boot_census`
//! and W37-02 designs); the parent's wall clock is the authority. No ROM bytes
//! and no paths are written out (law 5): titles of failures and counts only.
//!
//! ```text
//! cargo test --release -p retroforge --test w37_03_census -- --ignored --nocapture census
//! ```
//!
//! Every child's stderr (panic text, `RUST_BACKTRACE=1`) is captured and printed under
//! its row when a child dies. The 2026-10-26 sweep's seven exit-101 children were
//! `Harness::run` exceeding its step limit while the home screen's library-scan spinner
//! kept repainting on a loaded machine (not the atlas cache, not Rebuild: the crashed
//! child of the rerun was a Rebuild-off one); the child now uses `settle` (W37-03c).
//!
//! Knobs: `RF_ROM_ROOT` (default `~/Games/Roms`, holding `nes/` and `snes/`),
//! `RF_W37_03_NES` / `RF_W37_03_SNES` (sample sizes, 100), `RF_W37_03_SEED`,
//! `RF_W37_03_FRAMES` (600), `RF_W37_03_BUDGET_MIN` (120; ROMs not reached are
//! counted as skipped), `RF_W37_03_ONLY` (substring of a file name).

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::game_settings::Mode;
use retroforge::rebuild::RebuildStatus;

const ROM_VAR: &str = "RF_W37_03_ROM";
const REBUILD_VAR: &str = "RF_W37_03_REBUILD";
const PER_RUN_TIMEOUT: Duration = Duration::from_secs(240);

fn env_num(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn root() -> PathBuf {
    std::env::var("RF_ROM_ROOT").map_or_else(
        |_| PathBuf::from("/Users/bmatthews/Games/Roms"),
        PathBuf::from,
    )
}

fn archives_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("zip" | "nes" | "sfc" | "smc" | "fig")
            )
        })
        .collect();
    out.sort();
    out
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// Fisher-Yates over a copy; the loop index counts down every pass.
fn shuffled<T: Clone>(items: &[T], seed: u64) -> Vec<T> {
    let mut v = items.to_vec();
    let mut rng = Rng(seed);
    for i in (1..v.len()).rev() {
        let j = (rng.next() % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
    v
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn title_of(p: &Path) -> String {
    p.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

// ---- child -------------------------------------------------------------------

/// Let the app draw its first frames. Never `Harness::run`: it panics once a
/// run exceeds its step limit, and the home screen keeps asking for repaints
/// while the library scan is in flight ("Scanning your ROM folders", a
/// spinner), which on a loaded machine outlasts the limit. That panic, not
/// Rebuild, was the exit-101 crashes of the 2026-10-26 sweep (W37-03c).
fn settle<S>(harness: &mut Harness<'_, S>) {
    harness.run_steps(4);
}

/// Step to `frames` and say what the app shows. One line on stdout.
#[test]
#[ignore = "child half of the census; the parent re-executes this"]
fn census_child() {
    let Ok(path) = std::env::var(ROM_VAR) else {
        return;
    };
    let rebuild = std::env::var(REBUILD_VAR).is_ok_and(|v| v == "1");
    let frames = env_num("RF_W37_03_FRAMES", 600);
    let dir = std::env::temp_dir().join(format!("rf_w37_03_cfg_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: the only thing this process does before it reads the variable.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let started = Instant::now();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0] + 200.0, WINDOW_SIZE[1] + 300.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    settle(&mut harness);
    harness.state_mut().launch_rom(Path::new(&path));
    harness.state_mut().pause_for_test();
    harness.state_mut().set_mode_for_test(Mode::Enhanced);
    let mut line = BTreeMap::new();
    line.insert("rebuild", u8::from(rebuild).to_string());
    if rebuild {
        harness.state_mut().set_rebuild_for_test(true);
    }
    // A game that refuses to open has no core: say so, not "mismatch".
    harness.run_steps(2);
    let step = |h: &mut Harness<'_, RetroForgeApp>| -> bool {
        let before = h.state().frame_count_for_test();
        h.state_mut().step_frame_for_test();
        let t = Instant::now();
        while h.state().frame_count_for_test() <= before {
            if t.elapsed() > Duration::from_secs(20) {
                return false;
            }
            h.run_steps(1);
            std::thread::sleep(Duration::from_millis(1));
        }
        true
    };
    let mut ok = step(&mut harness);
    if !ok {
        line.insert("booted", "no".to_string());
        println!("W3703\t{}", join(&line));
        return;
    }
    line.insert("booted", "yes".to_string());
    // Rebuild on: step slowly until the first atlas is installed (bounded).
    let mut engaged = !rebuild;
    let wait = Instant::now();
    while rebuild
        && !engaged
        && ok
        && harness.state().frame_count_for_test() < frames.saturating_sub(120)
        && wait.elapsed() < Duration::from_secs(90)
    {
        ok = step(&mut harness);
        std::thread::sleep(Duration::from_millis(15));
        engaged = matches!(
            harness.state().rebuild_status_for_test(),
            RebuildStatus::On(_)
        );
    }
    while ok && harness.state().frame_count_for_test() < frames {
        ok = step(&mut harness);
        if rebuild && !engaged {
            engaged = matches!(
                harness.state().rebuild_status_for_test(),
                RebuildStatus::On(_)
            );
        }
    }
    harness.run_steps(2);
    let at = harness.state().frame_count_for_test();
    line.insert("frame", at.to_string());
    line.insert("stepped", (if ok { "yes" } else { "stalled" }).to_string());
    let picture = harness.state().last_frame_rgba_for_test();
    let hash = picture
        .as_deref()
        .map_or_else(|| "none".to_string(), |p| format!("{:016x}", fnv(p)));
    line.insert("hash", hash);
    // A blank picture would match trivially: count the distinct colours shown.
    let colours: std::collections::BTreeSet<[u8; 4]> = picture
        .iter()
        .flat_map(|p| p.chunks_exact(4))
        .map(|c| [c[0], c[1], c[2], c[3]])
        .collect();
    line.insert("colours", colours.len().to_string());
    if rebuild {
        line.insert("engaged", (if engaged { "yes" } else { "no" }).to_string());
        if let RebuildStatus::On(info) = harness.state().rebuild_status_for_test() {
            let s = &info.stats;
            line.insert("insns", s.instructions().to_string());
            line.insert("observed", s.observed.to_string());
            line.insert("inferred", s.inferred.to_string());
            line.insert("fallback", s.fallback.to_string());
            line.insert("swaps", info.swaps.to_string());
        }
    }
    line.insert("secs", started.elapsed().as_secs().to_string());
    println!("W3703\t{}", join(&line));
    drop(harness);
    let _ = std::fs::remove_dir_all(&dir);
}

fn join(m: &BTreeMap<&str, String>) -> String {
    m.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("\t")
}

/// Regression (W37-03c): an app that never stops repainting, as the home
/// screen does during a library scan, kills `Harness::run` but not `settle`.
#[test]
fn settle_survives_an_app_that_keeps_repainting() {
    let busy = |ui: &mut egui::Ui| {
        ui.spinner();
        ui.ctx().request_repaint();
    };
    let mut harness = Harness::new_ui(busy);
    settle(&mut harness);
    let plain = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Harness::new_ui(busy).run();
    }));
    assert!(plain.is_err(), "Harness::run is the call that panics");
}

// ---- parent ------------------------------------------------------------------

fn run_child(exe: &Path, rom: &Path, rebuild: bool, deadline: Instant) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    let Ok(mut child) = Command::new(exe)
        .args(["--exact", "census_child", "--ignored", "--nocapture"])
        .env(ROM_VAR, rom)
        .env(REBUILD_VAR, if rebuild { "1" } else { "0" })
        .env("RUST_BACKTRACE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    else {
        m.insert("crash".into(), "spawn".into());
        return m;
    };
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    // Always captured: the 2026-10-26 sweep lost six children and the panic
    // text went to /dev/null, so the cause had to be guessed (W37-03c).
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(_) => break None,
        }
        if started.elapsed() > PER_RUN_TIMEOUT || Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            m.insert("stderr".into(), err_reader.join().unwrap_or_default());
            m.insert("crash".into(), "timeout".into());
            return m;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let out = reader.join().unwrap_or_default();
    let err = err_reader.join().unwrap_or_default();
    if let Some(l) = out.lines().find_map(|l| l.strip_prefix("W3703\t")) {
        m.extend(
            l.split('\t')
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string())),
        );
    } else {
        m.insert(
            "crash".into(),
            format!("child died: {:?}", status.and_then(|s| s.code())),
        );
        m.insert("stderr".into(), err);
    }
    m
}

#[derive(Default)]
struct Tally {
    sampled: u32,
    reached: u32,
    skipped: u32,
    refused: u32,
    crashed: u32,
    stalled: u32,
    same: u32,
    mismatched: Vec<String>,
    crashes: Vec<String>,
    engaged: u32,
    non_blank: u32,
    not_engaged: Vec<String>,
    instructions: u64,
    through_atlas: u64,
    swaps: u64,
}

impl Tally {
    fn show(&self, name: &str) {
        println!(
            "{name}: sampled {}, reached {}, skipped (budget) {}, refused to open {}, crashed/timed out {}, stalled {}",
            self.sampled, self.reached, self.skipped, self.refused, self.crashed, self.stalled
        );
        println!(
            "  first-screen hash identical with Rebuild on and off: {} of {}; MISMATCHES: {}",
            self.same,
            self.reached,
            self.mismatched.len()
        );
        println!(
            "  pictures with more than one colour (not a blank match): {} of {}",
            self.non_blank, self.reached
        );
        println!(
            "  Rebuild engaged (atlas installed before the frame): {} of {}; through the atlas: {:.1}% of {} instructions; swaps {}",
            self.engaged,
            self.reached,
            if self.instructions == 0 {
                0.0
            } else {
                self.through_atlas as f64 * 100.0 / self.instructions as f64
            },
            self.instructions,
            self.swaps
        );
        for t in &self.mismatched {
            println!("  MISMATCH: {t}");
        }
        for t in &self.crashes {
            println!("  CRASHED (not a picture difference; rerun alone): {t}");
        }
        for t in &self.not_engaged {
            println!("  not engaged: {t}");
        }
    }
}

#[test]
#[ignore = "local: runs a real ROM library, two child processes per title"]
fn census() {
    if std::env::var(ROM_VAR).is_ok() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary");
    let seed = env_num("RF_W37_03_SEED", 0x5EED_3703);
    let only = std::env::var("RF_W37_03_ONLY").ok();
    let budget = Duration::from_secs(env_num("RF_W37_03_BUDGET_MIN", 120) * 60);
    let deadline = Instant::now() + budget;
    let started = Instant::now();
    let nes_n = env_num("RF_W37_03_NES", 100) as usize;
    let snes_n = env_num("RF_W37_03_SNES", 100) as usize;
    let mut jobs: Vec<(&str, PathBuf)> = Vec::new();
    for (console, n) in [("nes", nes_n), ("snes", snes_n)] {
        let all = archives_in(&root().join(console));
        jobs.extend(
            shuffled(&all, seed ^ if console == "nes" { 1 } else { 2 })
                .into_iter()
                .filter(|p| {
                    only.as_ref()
                        .is_none_or(|o| title_of(p).contains(o.as_str()))
                })
                .take(n)
                .map(|p| (console, p)),
        );
    }
    println!(
        "seed {seed:#x}, {} ROMs, {} frames, budget {} min",
        jobs.len(),
        env_num("RF_W37_03_FRAMES", 600),
        budget.as_secs() / 60
    );
    let mut tallies: BTreeMap<&str, Tally> = BTreeMap::new();
    for (console, path) in &jobs {
        let t = tallies.entry(console).or_default();
        t.sampled += 1;
        if Instant::now() > deadline {
            t.skipped += 1;
            continue;
        }
        let off = run_child(&exe, path, false, deadline);
        if off.contains_key("crash")
            && off.get("crash").is_some_and(|c| c == "timeout")
            && Instant::now() > deadline
        {
            t.skipped += 1;
            continue;
        }
        if off.get("booted").is_some_and(|b| b == "no") {
            t.refused += 1;
            continue;
        }
        let on = run_child(&exe, path, true, deadline);
        let title = title_of(path);
        if off.contains_key("crash") || on.contains_key("crash") {
            t.crashed += 1;
            t.crashes.push(format!(
                "{title}: child died (off {:?}, on {:?})\n--- stderr (off) ---\n{}\n--- stderr (on) ---\n{}",
                off.get("crash"),
                on.get("crash"),
                off.get("stderr").map_or("", String::as_str),
                on.get("stderr").map_or("", String::as_str)
            ));
            continue;
        }
        if off.get("stepped").is_some_and(|s| s != "yes")
            || on.get("stepped").is_some_and(|s| s != "yes")
        {
            t.stalled += 1;
            continue;
        }
        t.reached += 1;
        if off.get("hash") == on.get("hash") && off.get("frame") == on.get("frame") {
            t.same += 1;
        } else {
            t.mismatched.push(format!(
                "{title}: off {:?}@{:?} on {:?}@{:?}",
                off.get("hash"),
                off.get("frame"),
                on.get("hash"),
                on.get("frame")
            ));
        }
        let num = |m: &BTreeMap<String, String>, k: &str| {
            m.get(k).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0)
        };
        if num(&on, "colours") > 1 {
            t.non_blank += 1;
        }
        if on.get("engaged").is_some_and(|e| e == "yes") {
            t.engaged += 1;
        } else {
            t.not_engaged.push(title.clone());
        }
        t.instructions += num(&on, "insns");
        t.through_atlas += num(&on, "observed") + num(&on, "inferred");
        t.swaps += num(&on, "swaps");
    }
    for (name, t) in &tallies {
        t.show(name);
    }
    println!("wall time {} min", started.elapsed().as_secs() / 60);
    let bad: usize = tallies.values().map(|t| t.mismatched.len()).sum();
    assert_eq!(
        bad, 0,
        "Rebuild changed what some game shows on its first screen"
    );
}
