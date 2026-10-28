//! **W38-11 (D-032): ROM-gated 32:9 renders of the learned-from-play margins** (law 5: ROMs are
//! read from `RF_SCREENS_ROMS_DIR`, nothing ROM-derived is committed; PNGs go to the scratchpad).
//!
//! Each game is played through the real core thread with the recipe's camera/object reading on;
//! every frame feeds `rf_enhance::presentation_margins::Presentation` exactly as the app does
//! (`RetroForgeApp::presentation_margins_for_frame`); the best-covered moment is written as a
//! 32:9 PNG. A second run without the reading proves the accuracy frames and WRAM byte-identical,
//! and every composed frame's middle 256 columns equal the core's own picture. Only counts and
//! offsets are printed (D-021).
//!
//! ```text
//! cargo test --release -p retroforge --test w38_11_roms -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::time::Duration;

use retroforge::core_thread::{self, CoreCommand, CoreEvent};
use retroforge::presentation::Spec;
use rf_core_api::InputFrame;
use rf_enhance::presentation_margins::{Frame, Presentation};
use rf_enhance::widescreen::Aspect;
use rf_input::SnesButton;

const OUT_DIR: &str = "/private/tmp/claude-502/-Users-bmatthews-Code/10a3a02b-3b5c-46ad-afc7-f01185980ef7/scratchpad/w38-11";

#[derive(Clone, Copy, PartialEq)]
enum Console {
    Nes,
    Snes,
}

fn roms_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("RF_SCREENS_ROMS_DIR")
            .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".into()),
    )
}

/// `wN` idles, `<keys>N` holds them (`A B X Y E S T U D L R` joined by `+`).
fn script(console: Console, text: &str) -> Vec<u16> {
    let mut out = Vec::new();
    for tok in text.split_whitespace() {
        let (mask, n) = if let Some(n) = tok.strip_prefix('w') {
            (0u16, n.parse::<usize>().expect("wN"))
        } else {
            let split = tok.find(|c: char| c.is_ascii_digit()).expect("keys+N");
            let (keys, n) = tok.split_at(split);
            let mask = keys.split('+').fold(0u16, |m, k| {
                m | match console {
                    Console::Nes => match k {
                        "A" => 1,
                        "B" => 2,
                        "E" => 4,
                        "S" | "T" => 8,
                        "U" => 16,
                        "D" => 32,
                        "L" => 64,
                        "R" => 128,
                        other => panic!("key {other}"),
                    },
                    Console::Snes => {
                        1 << match k {
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
                            other => panic!("key {other}"),
                        }
                        .bit()
                    }
                }
            });
            (mask, n.parse().expect("N"))
        };
        out.extend(std::iter::repeat_n(mask, n));
    }
    out
}

struct Game {
    name: &'static str,
    console: Console,
    rom: &'static str,
    profile: &'static str,
    script: String,
}

fn games() -> Vec<Game> {
    let smw_to_level = "w200 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 w300 A8 w408 A8 w200 R40 w20 L40 w20 R40 w20 L40 w20 R40 w20 L40 w20 R60 w20 R200 w300 L8 w158 A8 w158";
    let sm_free_play = {
        let mut s = String::from("w300 w300 S6 w120 S6 w120 A6 w120 A6 w120 S6 w120 A6 w600");
        for _ in 0..55 {
            s += " A6 w150";
        }
        s + " w500 w600 w300"
    };
    let sm_rooms = "R40 w60 R40 w60 L40 w60 R40 w60 D10 w60 L40 w60 L40 w60 R40 w60 D10 w60 R40 R+A25 R30 R+A25 R30 w250 R+A30 R90 R+A25 R120 R120 R120 R90 R90 R90 L60 L60 L60 L60 R+A20 R80 R80 R80 R80 R120 R120 R120 R120 R120 R120 R120 R120 R60 w300 R60";
    let cv_patrol = "R+B3 R17 R+A8 R+B3 R12 R+B3 R12 w6 ".repeat(40);
    let mm2_right = "R+B30 A+R+B25 ".repeat(25);
    vec![
        Game {
            name: "smw",
            console: Console::Snes,
            rom: "snes/Super Mario World (USA).zip",
            profile: "snes/super-mario-world",
            script: format!("{smw_to_level} {}", "R+Y14 R22 R+A+Y14 R22 ".repeat(60)),
        },
        Game {
            name: "super-metroid",
            console: Console::Snes,
            rom: "snes/Super Metroid (Japan, USA) (En,Ja).zip",
            profile: "snes/super-metroid",
            script: format!("{sm_free_play} {sm_rooms}"),
        },
        Game {
            name: "castlevania",
            console: Console::Nes,
            rom: "nes/Castlevania (USA).zip",
            profile: "nes/castlevania",
            script: format!("w200 T4 w196 {cv_patrol}"),
        },
        Game {
            name: "mega-man-2",
            console: Console::Nes,
            rom: "nes/Mega Man 2 (USA).zip",
            profile: "nes/mega-man-2",
            script: format!("w220 S4 w400 S4 w300 S4 w300 R6 w20 S4 w300 S4 w700 {mm2_right}"),
        },
    ]
}

/// One run of a game: per frame the Accuracy picture, WRAM and (if asked) the readings.
struct Run {
    /// FNV of each frame's accuracy rgba + wram.
    hashes: Vec<u64>,
    best: Option<Best>,
    stats: Stats,
}

#[derive(Default)]
struct Stats {
    frames: usize,
    scenes: usize,
    chunks: usize,
    objects_seen: usize,
    relative: Option<bool>,
    best_known_pct: f32,
    best_objects_drawn: usize,
    centre_checks: usize,
}

struct Best {
    png: Vec<u8>,
}

fn fnv(h: u64, bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(h, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

fn play(g: &Game, spec: Option<Spec>, rom: &[u8], aspect: Aspect) -> Run {
    let core = core_thread::spawn(rom.to_vec()).expect("spawns");
    let reading = spec.is_some();
    let trail = spec.as_ref().is_some_and(Spec::has_full_camera);
    let _ = trail;
    if let Some(spec) = spec {
        core.cmd_tx
            .send(CoreCommand::SetPresentation(Some(Box::new(spec))))
            .expect("accepts");
    }
    let pads = script(g.console, &g.script);
    let rows = match g.console {
        Console::Nes => 240,
        Console::Snes => 224,
    };
    let out_w = aspect.output_width(rows, 1.0);
    let mut p = Presentation::new();
    let mut run = Run {
        hashes: Vec::new(),
        best: None,
        stats: Stats::default(),
    };
    let mut last_scene = None;
    let mut best_score = -1.0f32;
    for (i, &pad) in pads.iter().enumerate() {
        let mut input = InputFrame::empty();
        input.ports[0] = pad;
        core.input.store(input);
        core.cmd_tx.send(CoreCommand::StepFrame).expect("accepts");
        let msg = loop {
            match core
                .evt_rx
                .recv_timeout(Duration::from_secs(60))
                .expect("a frame")
            {
                CoreEvent::Frame(m) => {
                    core.pending_frames
                        .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                    break m;
                }
                CoreEvent::Crashed(r) => panic!("crashed: {}", r.message),
                _ => {}
            }
        };
        run.hashes
            .push(fnv(fnv(0xcbf2_9ce4_8422_2325, &msg.rgba), &msg.wram[..]));
        run.stats.frames += 1;
        let Some(pf) = msg.presentation.as_deref() else {
            assert!(!reading || i == 0, "readings missing at frame {i}");
            continue;
        };
        let native = &msg.rgba[..msg.width * rows * 4];
        let frame = Frame {
            rgba: native,
            w: msg.width,
            h: rows,
            scene: pf.facts.scene,
            cam: pf.facts.cam,
            objects: pf.facts.objects.as_deref(),
            sprite: &pf.sprite,
            frame: msg.frame_count,
            trail: pf.facts.full_camera,
        };
        if last_scene != Some(pf.facts.scene) {
            run.stats.scenes += 1;
            last_scene = Some(pf.facts.scene);
        }
        p.observe(&frame, &|_| false);
        if i % 40 != 0 {
            continue;
        }
        let wide = p.compose(&frame, out_w, &|_| false);
        // The middle of the wide picture is the core's picture, byte for byte.
        let pad_l = (out_w - msg.width) / 2;
        for y in 0..rows {
            assert_eq!(
                wide[(y * out_w + pad_l) * 4..(y * out_w + pad_l + msg.width) * 4],
                native[y * msg.width * 4..(y + 1) * msg.width * 4],
                "{}: centre differs at frame {i} row {y}",
                g.name
            );
        }
        run.stats.centre_checks += 1;
        // How much of the margins is not the fallback black (a coverage score for choosing).
        let (mut known, mut total) = (0usize, 0usize);
        for y in 0..rows {
            for x in (0..pad_l).chain(pad_l + msg.width..out_w) {
                total += 1;
                known +=
                    usize::from(wide[(y * out_w + x) * 4..(y * out_w + x) * 4 + 3] != [0, 0, 0]);
            }
        }
        let pct = known as f32 * 100.0 / total as f32;
        let n_obj = p.objects.len();
        // Prefer moments that learned objects, then coverage.
        let score = pct + (n_obj.min(5) as f32) * 5.0;
        if score > best_score && i > 600 {
            best_score = score;
            run.stats.best_known_pct = pct;
            run.stats.objects_seen = n_obj;
            let drawn = p.objects.paint_margins(
                &mut vec![0u8; out_w * rows * 4],
                out_w,
                rows,
                pf.facts.cam.0,
                &pf.facts
                    .objects
                    .iter()
                    .flatten()
                    .map(|o| p.place(o, pf.facts.cam.0))
                    .collect::<Vec<_>>(),
            );
            run.stats.best_objects_drawn = drawn + p.trail.len().min(1);
            run.best = Some(Best {
                png: rf_renderer::png::encode_rgba(&wide, out_w as u32, rows as u32),
            });
        }
    }
    run.stats.chunks = p.world.chunk_count();
    run.stats.relative = p.relative();
    let _ = core.cmd_tx.send(CoreCommand::Shutdown);
    let _ = core.join_handle.join();
    run
}

#[test]
#[ignore = "ROM-gated (law 5); writes PNGs to the scratchpad, never the repo"]
fn learned_margins_32x9_on_four_games() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles");
    std::fs::create_dir_all(OUT_DIR).expect("out dir");
    let mut missing = 0;
    for g in games() {
        let path = roms_dir().join(g.rom);
        if !path.exists() {
            println!("SKIP {}: ROM not found", g.name);
            missing += 1;
            continue;
        }
        let rom = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
        let recipe = rf_profiles::load_recipe_file(&root.join(g.profile).join("recipe.toml"))
            .expect("recipe");
        let spec = Spec::from_recipe(&recipe).expect("camera rows");
        let blurb = spec.describe();
        let on = play(&g, Some(spec), &rom, Aspect::Wide32x9);
        let off = play(&g, None, &rom, Aspect::Wide32x9);
        assert_eq!(
            on.hashes, off.hashes,
            "{}: accuracy frames/WRAM differ with the margins on",
            g.name
        );
        let s = &on.stats;
        println!(
            "{}: {} frames, {} scenes, {} world chunks, {} objects remembered, best margin coverage {:.1}%, objects drawn at best {}, table x camera-relative: {:?}, centre checks {}, accuracy+WRAM identical over {} frames; mode: {blurb}",
            g.name, s.frames, s.scenes, s.chunks, s.objects_seen, s.best_known_pct, s.best_objects_drawn, s.relative, s.centre_checks, on.hashes.len()
        );
        assert!(s.chunks > 0, "{}: nothing learned", g.name);
        if let Some(b) = on.best {
            let out = Path::new(OUT_DIR).join(format!("{}-32x9.png", g.name));
            std::fs::write(&out, b.png).expect("writes png");
            println!("  wrote {}", out.display());
        }
    }
    assert!(missing < 4, "no ROMs found");
}
