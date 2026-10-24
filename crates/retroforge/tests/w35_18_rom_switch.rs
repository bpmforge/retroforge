//! **W35-18: switching games while the Diorama (and the HD-2D preset) is on.**
//!
//! W35-13 made the live Diorama read back asynchronously (a frame is always in
//! flight) and W35-15 made the pass keep texture bytes between frames. Neither
//! tested what a ROM switch does to that state. This file does, at the app
//! level (`open_rom_path`, `close_rom`), with a real GPU:
//!
//! * the frames the Diorama shows right after a switch are the NEW game's, one
//!   for one, against a reference session that never saw the old game (the
//!   scenes are still, so the double-buffered pass equals the synchronous one,
//!   pinned by `a_still_scene_is_identical_double_buffered_and_creates_nothing`);
//! * none of them is a frame the old game showed, none is the wrong size;
//! * the old pass's staging buffers and kept textures are gone
//!   (`rf_renderer::diorama::live_resources`), the old core thread has exited,
//!   RSS is flat over ten switches;
//! * Diorama settings follow the per-game rule: a game's own SAVED settings load
//!   on open, unsaved edits die with the session, a game never saved opens at
//!   the defaults (Accuracy, Diorama off).
//!
//! Fixtures ship in the repo; the `#[ignore]` tests use commercial ROMs from
//! `$RF_SCREENS_ROMS_DIR` (default `~/Games/Roms`), one at a time:
//! `cargo test --release -p retroforge --test w35_18_rom_switch -- --ignored --nocapture <name>`
//!
//! The tests share the process-global `RETROFORGE_CONFIG_DIR`, so they take a
//! lock and run one after another.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::core_thread::{CoreCommand, SnesDebugFrame};
use retroforge::game_settings::Mode;
use retroforge::live_heights::HeightTable;
use retroforge::live_learn::HeightsFrame;
use rf_enhance::heights::{
    Class, HeightGrid, Orientation, OrientationGuess, OrientationSource, SpriteBox, Stats,
    TileCell, TileGrid,
};
use rf_enhance::widescreen::Aspect;
use rf_renderer::diorama::live_resources;

type H = Harness<'static, RetroForgeApp>;

const SNES_A: &str = "../../fixtures/snes/rf-scroller-s/build/rf-scroller-s.sfc";
const SNES_B: &str = "../../fixtures/snes/mirror-map/build/lorom.sfc";
const NES_A: &str = "../../fixtures/nes/action53/build/action53.nes";

// ---------------------------------------------------------------- the process

struct Env {
    dir: PathBuf,
    /// A synthetic NROM with a decoded level (the Diorama's decoded path),
    /// written once; its profile is the only one in the profiles dir.
    nrom: PathBuf,
}

fn env() -> &'static Env {
    static ENV: OnceLock<Env> = OnceLock::new();
    ENV.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("retroforge_w35_18_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let profiles = dir.join("profiles").join("w35-18");
        std::fs::create_dir_all(&profiles).expect("scratch");
        let rom = synthetic_nrom();
        let sha = match rf_cart::Cartridge::load(&rom).expect("valid NROM") {
            rf_cart::Cartridge::Nes { identity, .. }
            | rf_cart::Cartridge::Snes { identity, .. } => identity.normalized.sha256,
        };
        std::fs::write(profiles.join("profile.toml"), nrom_profile(&sha)).expect("profile");
        let nrom = dir.join("w35-18-nrom.nes");
        std::fs::write(&nrom, &rom).expect("rom");
        // SAFETY: first statements of the first test to run; every test of
        // this binary goes through `lock()`, which calls `env()` first.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
            std::env::set_var("RETROFORGE_PROFILES_DIR", dir.join("profiles"));
            std::env::set_var("TMPDIR", &dir);
        }
        Env { dir, nrom }
    })
}

fn lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    env();
    g
}

fn fixture(rel: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    assert!(p.exists(), "fixture not built: {}", p.display());
    p
}

/// The same bytes under a new name and one changed byte of PRG padding: a
/// different game identity (so it has its own settings file) that was never
/// saved.
fn unsaved_nes_copy() -> PathBuf {
    let mut bytes = std::fs::read(fixture(NES_A)).expect("read");
    let last = bytes.len() - 1;
    bytes[last] ^= 0x5A;
    let p = env().dir.join("never-saved.nes");
    std::fs::write(&p, bytes).expect("write");
    p
}

const PRG_LEN: usize = 0x4000;
const CHR_LEN: usize = 0x2000;

fn synthetic_nrom() -> Vec<u8> {
    let mut rom = vec![0u8; 16 + PRG_LEN + CHR_LEN];
    rom[0..4].copy_from_slice(b"NES\x1a");
    rom[4] = 1;
    rom[5] = 1;
    let prg = &mut rom[16..16 + PRG_LEN];
    prg[0x0004] = 0x00;
    prg[0x0005] = 0x01;
    prg[0x0006] = 0x00;
    prg[0x0007] = 0b1;
    rom
}

fn nrom_profile(sha256: &str) -> String {
    format!(
        r#"
[meta]
profile_version = "0.1"
title = "W35-18 switch fixture"
console = "nes"
region = "ntsc"
sources = ["synthetic, authored for ticket W35-18's tests -- not a real game"]

[[identity]]
sha256 = "{sha256}"

[decode]
kind = "metatile_screens"

[decode.metatile]
table = 0
size = 4

[decode.screens]
width = 1
height = 1
order = "column_rle"

[decode.collision]
table = 7
bits = "solid"

[[rom_map]]
offset = 4
len = 1
label = "level_column_offset"
type = "table"
source = "synthetic test fixture"

[[rom_map]]
offset = 5
len = 2
label = "level_rle_data"
type = "table"
source = "synthetic test fixture"
"#
    )
}

// ------------------------------------------------------------ synthetic frames

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// 256 x 224, the painted-table path (a BG tilemap on screen).
    Snes,
    /// 256 x 240, the console-free learned path.
    Nes,
}

impl Kind {
    fn size(self) -> (usize, usize) {
        match self {
            Kind::Snes => (256, 224),
            Kind::Nes => (256, 240),
        }
    }
}

/// One game's still scene: a picture the `seed` colours, so two scenes (or one
/// game at two moments) never produce the same Diorama.
#[derive(Clone, Copy, Debug)]
struct Scene {
    kind: Kind,
    seed: u8,
}

fn picture(size: (usize, usize), seed: u8) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(size.0 * size.1 * 4);
    for y in 0..size.1 {
        for x in 0..size.0 {
            rgba.extend_from_slice(&[
                ((x + usize::from(seed) * 37) % 256) as u8,
                ((y * 2 + usize::from(seed) * 91) % 256) as u8,
                60 + seed.wrapping_mul(53) % 150,
                255,
            ]);
        }
    }
    rgba
}

fn sprite_layer(size: (usize, usize), seed: u8) -> Vec<u8> {
    let mut s = vec![0u8; size.0 * size.1 * 4];
    let x0 = 60 + usize::from(seed) * 7;
    for y in 100..116 {
        for x in x0..x0 + 8 {
            let at = (y * size.0 + x) * 4;
            s[at..at + 4].copy_from_slice(&[255, seed.wrapping_mul(29), 0, 255]);
        }
    }
    s
}

fn snes_frame(seed: u8) -> SnesDebugFrame {
    let mut vram = vec![0u8; 0x10000];
    for (tx, ty, c) in [(8, 10, 0x0A4u16), (9, 10, 0x0A4), (12, 12, 0x0A5)] {
        let at = (0x0400 + ty * 32 + tx) * 2;
        vram[at] = c as u8;
        vram[at + 1] = (c >> 8) as u8;
    }
    let mut regs = vec![0u8; 0x40];
    regs[0x05] = 1; // BGMODE 1
    regs[0x07] = 0x04; // BG1SC: map at word 0x0400, 32x32
    let mut oam = vec![0u8; 544];
    oam[0] = 100;
    oam[1] = 100;
    SnesDebugFrame {
        vram,
        cgram: vec![0u8; 512],
        oam,
        ppu_regs: regs,
        aram: Vec::new(),
        mode7: rf_snes::ppu::mode7::Mode7::default(),
        hdma_lanes: Vec::new(),
        voices: Vec::new(),
        bg_scroll: [(u16::from(seed), 0), (0, 0), (0, 0), (0, 0)],
    }
}

/// A grid of 8 px cells over `rows_px` rows: the bottom three rows are ground
/// and one pillar rises from it.
fn nes_heights(rows_px: u16) -> (TileGrid, HeightsFrame) {
    let (cols, rows) = (32u16, rows_px / 8);
    let n = usize::from(cols) * usize::from(rows);
    let g = TileGrid {
        layer: 0,
        cell_px: 8,
        hud: false,
        exact: true,
        rows_px: (0, rows_px),
        origin_world: (0, 0),
        offset: (0, 0),
        cols,
        rows,
        cells: vec![TileCell::default(); n],
    };
    let mut h = HeightGrid {
        cols,
        rows,
        cell_px: 8,
        offset: (0, 0),
        class: vec![Class::Unknown; n],
        depth: vec![0; n],
        confidence: vec![3; n],
    };
    let at = |c: usize, r: usize| r * usize::from(cols) + c;
    for r in usize::from(rows) - 3..usize::from(rows) {
        for c in 0..32 {
            h.class[at(c, r)] = Class::Wall;
            h.depth[at(c, r)] = 8;
        }
    }
    for r in usize::from(rows) - 7..usize::from(rows) - 3 {
        h.class[at(16, r)] = Class::Wall;
        h.depth[at(16, r)] = 8;
    }
    let frame = HeightsFrame {
        grids: vec![h],
        foreground: Some(0),
        orientation: OrientationGuess {
            orientation: Orientation::SideView,
            confidence: 3,
            source: OrientationSource::Gravity,
        },
        stats: Stats {
            keys: 40,
            drawn: 12,
            solid: 9,
            by_level: [20, 8, 4, 8],
        },
        backdrop_ratio: None,
        player: None,
        frames: 1200,
        level: None,
        layers: Default::default(),
    };
    (g, frame)
}

fn inject(app: &mut RetroForgeApp, s: Scene) {
    let size = s.kind.size();
    let (rgba, sprites) = (picture(size, s.seed), sprite_layer(size, s.seed));
    match s.kind {
        Kind::Snes => app.inject_one_snes_frame_for_test(rgba, sprites, snes_frame(s.seed), size),
        Kind::Nes => {
            let (g, h) = nes_heights(size.1 as u16);
            let boxes = vec![SpriteBox {
                x: 60 + i16::from(s.seed) * 7,
                y: 100,
                w: 8,
                h: 16,
                behind_bg: false,
                tile: 1,
            }];
            app.inject_one_learned_frame_for_test(rgba, sprites, vec![g], boxes, h, size);
        }
    }
}

// ------------------------------------------------------------------- the rig

/// What the player would see of the Diorama at one moment.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Shown {
    hash: Option<u64>,
    size: Option<(u32, u32)>,
}

struct Rig {
    h: H,
}

fn new_harness(w: f32, h: f32) -> H {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(w, h))
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::default())
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness
}

impl Rig {
    fn new() -> Option<Self> {
        let h = new_harness(WINDOW_SIZE[0], WINDOW_SIZE[1]);
        if !h.state().gpu_available_for_test() {
            assert!(std::env::var_os("CI").is_none(), "cannot skip in CI");
            eprintln!("SKIP: no wgpu adapter");
            return None;
        }
        Some(Self { h })
    }

    fn app(&mut self) -> &mut RetroForgeApp {
        self.h.state_mut()
    }

    /// Open a ROM and let the UI settle; the core stays paused (no frames of
    /// its own), so the frames in these tests are the ones injected.
    fn open(&mut self, p: &Path) {
        self.app().open_rom_path(p);
        self.h.run_steps(3);
    }

    /// Diorama on, the HD-2D preset on, saved: the way a player leaves a game
    /// they play with it. SNES games also get a table (the painted path).
    fn setup(&mut self, kind: Kind) {
        let app = self.app();
        app.set_mode_for_test(Mode::Enhanced);
        app.set_diorama_for_test(true);
        if !retroforge::hd2d_preset::is_applied(app.game_settings_for_test()) {
            app.edit_game_settings_for_test(|s| {
                retroforge::hd2d_preset::apply(s);
            });
        }
        if kind == Kind::Snes {
            app.set_live_heights_for_test(HeightTable::new(
                1,
                8,
                (0..1024u16).map(|c| (c, 8u8)).collect(),
            ));
        }
        app.save_game_settings_for_test();
        self.h.run_steps(3);
    }

    /// Re-arm the table a ROM open clears (profile match) after reopening.
    fn arm(&mut self, kind: Kind) {
        if kind == Kind::Snes {
            self.app().set_live_heights_for_test(HeightTable::new(
                1,
                8,
                (0..1024u16).map(|c| (c, 8u8)).collect(),
            ));
        }
        self.h.run_steps(3);
    }

    fn shown(&mut self) -> Shown {
        let a = self.h.state();
        Shown {
            hash: a.diorama_shown_hash_for_test().map(|x| x.0),
            size: a.diorama_size_for_test(),
        }
    }

    /// One frame of `s` through the live path, then what is shown.
    fn frame(&mut self, s: Scene) -> Shown {
        inject(self.app(), s);
        self.shown()
    }

    /// `n` frames of one still scene; what each showed.
    fn frames(&mut self, s: Scene, n: usize) -> Vec<Shown> {
        (0..n).map(|_| self.frame(s)).collect()
    }
}

fn rss_mb() -> f64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<f64>()
        .unwrap_or(0.0)
        / 1024.0
}

fn assert_nothing_alive(what: &str) {
    let live = live_resources();
    assert_eq!(
        (live.passes, live.readback_buffers, live.sampled_textures),
        (0, 0, 0),
        "{what}: the old pass's GPU resources are still alive: {live:?}"
    );
}

fn assert_one_pass_alive(what: &str) {
    let live = live_resources();
    assert_eq!(live.passes, 1, "{what}: {live:?}");
    assert_eq!(
        live.readback_buffers, 2,
        "{what}: a fixed pair of staging buffers: {live:?}"
    );
    assert!(live.sampled_textures <= 2, "{what}: {live:?}");
}

// ------------------------------------------------------------ the one scenario

/// Where a case ends and what it does at the switch.
#[derive(Clone, Copy)]
struct Case {
    name: &'static str,
    /// Quit to the library (`close_rom`) first, then open.
    via_library: bool,
    /// The first game was at 32:9 with widescreen on.
    wide: bool,
}

/// The new game's frames from a session that never ran another game.
fn reference(path: &Path, to: Scene, n: usize) -> Vec<Shown> {
    let mut rig = Rig::new().expect("gpu checked by the caller");
    rig.open(path);
    rig.setup(to.kind);
    rig.arm(to.kind);
    rig.frames(to, n)
}

const N: usize = 8;

/// Run `from` (path, scene) then switch to `to`; assert every property.
fn switch_case(case: Case, from: (&Path, Scene), to: (&Path, Scene)) {
    let Some(mut rig) = Rig::new() else { return };
    // Seed both games' saved settings: Diorama on, HD-2D preset on.
    rig.open(to.0);
    rig.setup(to.1.kind);
    rig.open(from.0);
    rig.setup(from.1.kind);
    rig.arm(from.1.kind);
    let want = reference(to.0, to.1, N);
    // A reference session is deterministic, or nothing below means anything.
    assert_eq!(
        want,
        reference(to.0, to.1, N),
        "{}: reference differs run to run",
        case.name
    );
    assert!(
        want.iter().all(|s| s.hash.is_some() && s.size.is_some()),
        "{}: the reference never drew a Diorama: {want:?}",
        case.name
    );
    let want_size = want[0].size;
    assert!(
        want.iter().all(|s| s.size == want_size),
        "{}: sizes {want:?}",
        case.name
    );

    // The old game, running: a few settled frames, one more in flight.
    let old = rig.frames(from.1, N);
    assert!(
        old.iter().all(|s| s.hash.is_some()),
        "{}: old game drew nothing",
        case.name
    );
    assert_one_pass_alive("old game running");
    let old_hashes: Vec<u64> = old.iter().filter_map(|s| s.hash).collect();
    let old_size = old.last().and_then(|s| s.size);
    let old_sender = rig.app().core_cmd_sender_for_test().expect("old core");
    // Unsaved edit on the old game: must not outlive it.
    rig.app().set_diorama_camera_for_test(10, 5, 300, false);
    if case.wide {
        rig.app().set_widescreen_aspect_for_test(Aspect::Wide32x9);
        rig.app().set_widescreen(true);
        rig.h.run_steps(3);
    }
    // One more frame, so a readback is pending at the moment of the switch.
    let _in_flight = rig.frame(from.1);

    // --- the switch
    if case.via_library {
        rig.app().close_rom_for_test();
        assert_nothing_alive(&format!("{}: quit to library", case.name));
        assert_eq!(
            rig.shown(),
            Shown {
                hash: None,
                size: None
            },
            "{}: library shows a Diorama",
            case.name
        );
        rig.h.run_steps(2);
    }
    rig.open(to.0);
    assert!(
        old_sender.send(CoreCommand::Pause).is_err(),
        "{}: the old core thread is still alive",
        case.name
    );
    assert_nothing_alive(&format!("{}: right after the switch", case.name));
    assert_eq!(
        rig.shown(),
        Shown {
            hash: None,
            size: None
        },
        "{}: a Diorama frame of the old game survived the open",
        case.name
    );
    // Settings: the new game's own SAVED ones; the old game's unsaved camera
    // edit is gone, and widescreen (a per-session switch) is off.
    let s = rig.app().game_settings_for_test().clone();
    assert!(
        s.diorama,
        "{}: the new game's saved Diorama setting was not restored",
        case.name
    );
    assert_eq!(
        (s.diorama_pitch, s.diorama_zoom, s.diorama_follow),
        (40, 160, true),
        "{}: the old game's unsaved camera edit leaked into the new game",
        case.name
    );
    rig.arm(to.1.kind);

    // --- the first N frames of the new game
    let got = rig.frames(to.1, N);
    assert_eq!(
        got, want,
        "{}: the first {N} Diorama frames after the switch differ from the new game's own",
        case.name
    );
    for (i, g) in got.iter().enumerate() {
        assert!(
            g.hash.is_some_and(|h| !old_hashes.contains(&h)),
            "{}: frame {i} after the switch is a frame of the old game",
            case.name
        );
        if old_size != want_size {
            assert_ne!(
                g.size, old_size,
                "{}: frame {i} has the old game's size",
                case.name
            );
        }
        let (w, h) = g.size.expect("size");
        // The Diorama is made at the play rect's shape: the new picture's.
        let frame = to.1.kind.size();
        let (fa, ga) = (frame.0 as f32 / frame.1 as f32, w as f32 / h as f32);
        assert!(
            (ga / fa - 1.0).abs() < 0.2,
            "{}: frame {i} is {w}x{h}, the new game's picture is {}x{}",
            case.name,
            frame.0,
            frame.1
        );
    }
    assert_one_pass_alive("new game running");
    assert!(
        !rig.app().widescreen_on_for_test(),
        "{}: widescreen outlived the switch",
        case.name
    );

    // Leaving: nothing is left behind.
    rig.app().close_rom_for_test();
    assert_nothing_alive(&format!("{}: end", case.name));
}

// -------------------------------------------------------------------- cases

#[test]
fn snes_to_snes() {
    let _g = lock();
    switch_case(
        Case {
            name: "snes->snes",
            via_library: false,
            wide: false,
        },
        (
            &fixture(SNES_A),
            Scene {
                kind: Kind::Snes,
                seed: 1,
            },
        ),
        (
            &fixture(SNES_B),
            Scene {
                kind: Kind::Snes,
                seed: 2,
            },
        ),
    );
}

#[test]
fn snes_to_nes_different_frame_size() {
    let _g = lock();
    switch_case(
        Case {
            name: "snes->nes",
            via_library: false,
            wide: false,
        },
        (
            &fixture(SNES_A),
            Scene {
                kind: Kind::Snes,
                seed: 1,
            },
        ),
        (
            &fixture(NES_A),
            Scene {
                kind: Kind::Nes,
                seed: 2,
            },
        ),
    );
}

#[test]
fn nes_to_snes_different_frame_size() {
    let _g = lock();
    switch_case(
        Case {
            name: "nes->snes",
            via_library: false,
            wide: false,
        },
        (
            &fixture(NES_A),
            Scene {
                kind: Kind::Nes,
                seed: 1,
            },
        ),
        (
            &fixture(SNES_A),
            Scene {
                kind: Kind::Snes,
                seed: 2,
            },
        ),
    );
}

/// Case 4 is every case: each one injects a frame right before the switch, so
/// a `map_async` is pending when the pass is dropped (a wgpu validation error
/// would panic the test). This one does it over and over, flipping sizes.
#[test]
fn switching_with_a_readback_in_flight_never_panics_or_leaks() {
    let _g = lock();
    let Some(mut rig) = Rig::new() else { return };
    let games = [
        (
            fixture(SNES_A),
            Scene {
                kind: Kind::Snes,
                seed: 1,
            },
        ),
        (
            fixture(NES_A),
            Scene {
                kind: Kind::Nes,
                seed: 2,
            },
        ),
        (
            fixture(SNES_B),
            Scene {
                kind: Kind::Snes,
                seed: 3,
            },
        ),
    ];
    for (p, s) in &games {
        rig.open(p);
        rig.setup(s.kind);
    }
    let mut last_sender = None;
    for round in 0..12 {
        let (p, s) = &games[round % games.len()];
        rig.open(p);
        if let Some(prev) = last_sender.take() {
            let prev: std::sync::mpsc::Sender<CoreCommand> = prev;
            assert!(
                prev.send(CoreCommand::Pause).is_err(),
                "round {round}: old core alive"
            );
        }
        assert_nothing_alive(&format!("round {round}"));
        rig.arm(s.kind);
        // Exactly one frame: it leaves a map pending, and the next open
        // drops the pass before anything collects it.
        let shown = rig.frame(*s);
        assert!(shown.hash.is_some(), "round {round}: no Diorama");
        assert_one_pass_alive(&format!("round {round}"));
        last_sender = rig.app().core_cmd_sender_for_test();
    }
}

#[test]
fn quit_to_library_and_reopen_the_same_rom() {
    let _g = lock();
    // The same game again: its first frames are its own (a boot), not the ones
    // it showed when the player quit (a later scene).
    switch_case(
        Case {
            name: "reopen",
            via_library: true,
            wide: false,
        },
        (
            &fixture(SNES_A),
            Scene {
                kind: Kind::Snes,
                seed: 5,
            },
        ),
        (
            &fixture(SNES_A),
            Scene {
                kind: Kind::Snes,
                seed: 6,
            },
        ),
    );
}

#[test]
fn switching_with_32_9_widescreen_on() {
    let _g = lock();
    switch_case(
        Case {
            name: "32:9",
            via_library: false,
            wide: true,
        },
        (
            &fixture(SNES_A),
            Scene {
                kind: Kind::Snes,
                seed: 1,
            },
        ),
        (
            &fixture(NES_A),
            Scene {
                kind: Kind::Nes,
                seed: 2,
            },
        ),
    );
}

/// Settings rule, stated and pinned: a game's own SAVED settings load when it
/// opens (Diorama and the HD-2D preset included); unsaved edits die with the
/// session; a game never saved opens at the defaults (Accuracy, Diorama off,
/// nothing drawn, no pass).
#[test]
fn diorama_settings_follow_the_per_game_rule() {
    let _g = lock();
    let Some(mut rig) = Rig::new() else { return };
    let (a, b) = (fixture(SNES_A), fixture(NES_A));
    rig.open(&a);
    rig.setup(Kind::Snes);
    rig.open(&b);
    rig.setup(Kind::Nes);
    rig.app()
        .edit_game_settings_for_test(|s| s.diorama_pitch = 52);
    rig.app().save_game_settings_for_test();

    rig.open(&a);
    let s = rig.app().game_settings_for_test().clone();
    assert!(
        s.diorama && s.diorama_pitch == 40,
        "A's saved settings: {s:?}"
    );
    rig.app().set_diorama_camera_for_test(10, 5, 300, false); // unsaved
    rig.open(&b);
    let s = rig.app().game_settings_for_test().clone();
    assert!(
        s.diorama && s.diorama_pitch == 52,
        "B has its own saved pitch: {s:?}"
    );
    rig.open(&a);
    let s = rig.app().game_settings_for_test().clone();
    assert_eq!(
        (s.diorama, s.diorama_pitch, s.diorama_zoom),
        (true, 40, 160),
        "A's unsaved edit must not survive a reopen"
    );
    // A game never saved: the defaults, and the Diorama off and drawing nothing.
    rig.open(&unsaved_nes_copy());
    let s = rig.app().game_settings_for_test().clone();
    assert!(
        !s.diorama,
        "a never-saved game must open with Diorama off: {s:?}"
    );
    assert_eq!(s.mode, Mode::Accuracy);
    rig.h.run_steps(3);
    inject(
        rig.app(),
        Scene {
            kind: Kind::Nes,
            seed: 9,
        },
    );
    assert_eq!(rig.shown().hash, None);
    assert_nothing_alive("a never-saved game");
}

/// Ten switches between a SNES and an NES game with REAL cores (the SNES one
/// on its fixture with a table; the NES one on the synthetic decoded level):
/// every old thread exits, live GPU resources never exceed one pass, and RSS
/// returns to where it was after warm-up.
#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "release only: with the HD-2D lens on, an unoptimised build trips the Diorama cost gate and draws nothing"
)]
fn ten_switches_with_real_cores_are_flat() {
    let _g = lock();
    let Some(mut rig) = Rig::new() else { return };
    let snes = fixture(SNES_A);
    let nes = env().nrom.clone();
    rig.open(&snes);
    rig.setup(Kind::Snes);
    rig.open(&nes);
    rig.setup(Kind::Nes);
    let mut senders: Vec<std::sync::mpsc::Sender<CoreCommand>> = Vec::new();
    let mut rss = Vec::new();
    let mut sizes = [None, None];
    for round in 0..16 {
        let (path, kind) = if round % 2 == 0 {
            (&snes, Kind::Snes)
        } else {
            (&nes, Kind::Nes)
        };
        rig.open(path);
        for s in senders.drain(..) {
            assert!(
                s.send(CoreCommand::Pause).is_err(),
                "round {round}: an old core thread lives"
            );
        }
        assert_nothing_alive(&format!("round {round}"));
        rig.arm(kind);
        rig.app().resume_for_test();
        let start = Instant::now();
        let mut drew = false;
        while start.elapsed() < Duration::from_secs(20)
            && !(drew && rig.h.state().frame_count_for_test() >= 40)
        {
            rig.h.run_steps(1);
            std::thread::sleep(Duration::from_millis(3));
            if let Some(size) = rig.h.state().diorama_size_for_test() {
                drew = true;
                // Every frame shown is this game's shape.
                let slot = &mut sizes[round % 2];
                match slot {
                    None => *slot = Some(size),
                    Some(first) => assert_eq!(*first, size, "round {round}: a wrong-sized frame"),
                }
            }
        }
        assert!(drew, "round {round}: no Diorama frame drawn for {kind:?}");
        assert!(
            live_resources().passes <= 1,
            "round {round}: {:?}",
            live_resources()
        );
        senders.extend(rig.app().core_cmd_sender_for_test());
        rss.push(rss_mb());
    }
    // Rounds 0..6 warm up (caches, allocator); the last ten must not grow.
    let (base, end) = (rss[5], rss[15]);
    println!("w35-18: RSS MB per round {rss:.0?}");
    println!(
        "w35-18: diorama sizes snes {:?} nes {:?}",
        sizes[0], sizes[1]
    );
    assert!(
        end - base < 40.0,
        "RSS grew {:.0} MB over ten switches ({base:.0} -> {end:.0})",
        end - base
    );
    rig.app().close_rom_for_test();
    assert_nothing_alive("closed");
}

// ----------------------------------------------------- commercial ROMs (gated)

fn rom(console: &str, file: &str) -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = Path::new(&root).join(console).join(file);
    p.exists().then_some(p)
}

const SMW: &str = "Super Mario World (USA).zip";
const ALTTP: &str = "Legend of Zelda, The - A Link to the Past (USA).zip";
const SMB: &str = "Super Mario Bros. + Duck Hunt (USA).zip";

/// Run a real core for `frames` core frames, recording every distinct Diorama
/// the player would see (hash, size), unpaced.
fn watch(rig: &mut Rig, frames: u64, shown: &mut Vec<Shown>) {
    rig.app().resume_for_test();
    rig.app()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
    let end = rig.h.state().frame_count_for_test() + frames;
    let start = Instant::now();
    while rig.h.state().frame_count_for_test() < end && start.elapsed() < Duration::from_secs(120) {
        rig.h.run_steps(1);
        std::thread::sleep(Duration::from_millis(2));
        let s = rig.shown();
        if s.hash.is_some() && shown.last() != Some(&s) {
            shown.push(s);
        }
    }
}

/// Two commercial games back to back, Diorama and HD-2D on in both; the new
/// game's pictures must be none of the old game's.
fn real_switch(name: &str, a: (&str, &str, Kind), b: (&str, &str, Kind)) {
    let _g = lock();
    let (Some(pa), Some(pb)) = (rom(a.0, a.1), rom(b.0, b.1)) else {
        eprintln!("SKIP {name}: ROMs not found");
        return;
    };
    let Some(mut rig) = Rig::new() else { return };
    rig.open(&pb);
    rig.setup(b.2);
    rig.open(&pa);
    rig.setup(a.2);
    rig.arm(a.2);
    let mut old = Vec::new();
    watch(&mut rig, 400, &mut old);
    let old_size = old.last().and_then(|s| s.size);
    let old_sender = rig.app().core_cmd_sender_for_test().expect("old core");
    // Without pausing: the core is mid-frame, a readback is pending.
    rig.open(&pb);
    assert!(
        old_sender.send(CoreCommand::Pause).is_err(),
        "{name}: old core alive"
    );
    assert_nothing_alive(name);
    assert_eq!(
        rig.shown(),
        Shown {
            hash: None,
            size: None
        },
        "{name}: old frame survived"
    );
    rig.arm(b.2);
    let mut fresh = Vec::new();
    watch(&mut rig, 400, &mut fresh);
    println!(
        "w35-18 {name}: old {} distinct Dioramas (last size {old_size:?}); new {} (sizes {:?})",
        old.len(),
        fresh.len(),
        fresh
            .iter()
            .map(|s| s.size)
            .collect::<std::collections::BTreeSet<_>>()
    );
    // A new-game picture that equals an old-game one is a leak, unless the
    // new game's own cold start produces it (a blank boot screen is blank in
    // both): ask a clean session what the new game shows first.
    let reference = {
        let mut r = Rig::new().expect("gpu");
        r.open(&pb);
        r.setup(b.2);
        r.arm(b.2);
        let mut own = Vec::new();
        watch(&mut r, 400, &mut own);
        own
    };
    let own: std::collections::HashSet<u64> = reference.iter().filter_map(|s| s.hash).collect();
    let olds: std::collections::HashSet<u64> = old.iter().filter_map(|s| s.hash).collect();
    for (i, s) in fresh.iter().enumerate() {
        let h = s.hash.expect("hash");
        assert!(
            !olds.contains(&h) || own.contains(&h),
            "{name}: Diorama picture {i} after the switch is one the old game showed"
        );
    }
    if let (Some(os), false) = (old_size, fresh.is_empty()) {
        let ns: std::collections::HashSet<_> = fresh.iter().filter_map(|s| s.size).collect();
        if a.2 != b.2 {
            assert!(
                !ns.contains(&os),
                "{name}: a frame at the old game's size {os:?}: {ns:?}"
            );
        }
    }
    rig.app().close_rom_for_test();
    assert_nothing_alive(&format!("{name}: closed"));
}

#[test]
#[ignore = "needs Super Mario World and A Link to the Past"]
fn real_smw_to_alttp() {
    real_switch(
        "smw->alttp",
        ("snes", SMW, Kind::Snes),
        ("snes", ALTTP, Kind::Snes),
    );
}

#[test]
#[ignore = "needs Super Mario World and an NES ROM"]
fn real_smw_to_nes() {
    real_switch(
        "smw->smb",
        ("snes", SMW, Kind::Snes),
        ("nes", SMB, Kind::Nes),
    );
}

#[test]
#[ignore = "needs an NES ROM and Super Mario World"]
fn real_nes_to_smw() {
    real_switch(
        "smb->smw",
        ("nes", SMB, Kind::Nes),
        ("snes", SMW, Kind::Snes),
    );
}
