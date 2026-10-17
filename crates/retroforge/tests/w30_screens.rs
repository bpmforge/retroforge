//! **Screenshots of the W30/W31 enhancements** (before/after pairs).
//!
//! Not a correctness test. It drives real commercial ROMs (which never live
//! in git, law 5: only the rendered PNGs are committed) to a scene, then
//! renders the SAME simulated frame twice -- feature off, feature on -- and
//! writes both to `docs/screens/w30-w31/`.
//!
//! **How "the same frame" is arranged.** Pause on the scene, save a state to
//! a scratch slot, then for each variant: re-apply that state and step
//! exactly one frame. The simulation is deterministic and the feature is a
//! presentation setting, so the only difference between the two shots is the
//! feature. Features that live on the core thread (HD Mode 7, widescreen,
//! Super FX polygons) show up in the frame the core sends
//! (`last_frame_rgba_for_test`); post passes on the UI thread (lens, Diorama)
//! are cropped out of the fully rendered window instead.
//!
//! ```text
//! cargo test --release -p retroforge --test w30_screens -- --ignored --nocapture
//! RF_SCREENS_ONLY=lens ...   # only shots whose name contains this
//! ```
//!
//! One test per binary: `RETROFORGE_CONFIG_DIR` is process-global.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::core_thread::CoreCommand;
use retroforge::game_settings::Mode;
use rf_enhance::widescreen::{Aspect, MarginSource};

type H = Harness<'static, RetroForgeApp>;

/// An RGBA picture.
#[derive(Clone)]
struct Pic {
    w: usize,
    h: usize,
    rgba: Vec<u8>,
}

impl Pic {
    fn upscale(&self, n: usize) -> Pic {
        let (w, h) = (self.w * n, self.h * n);
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let s = ((y / n) * self.w + x / n) * 4;
                rgba[(y * w + x) * 4..][..4].copy_from_slice(&self.rgba[s..s + 4]);
            }
        }
        Pic { w, h, rgba }
    }

    fn crop(&self, x0: usize, y0: usize, cw: usize, ch: usize) -> Pic {
        let mut rgba = Vec::with_capacity(cw * ch * 4);
        for y in y0..y0 + ch {
            let s = (y * self.w + x0) * 4;
            rgba.extend_from_slice(&self.rgba[s..s + cw * 4]);
        }
        Pic { w: cw, h: ch, rgba }
    }

    /// Opaque black, so a transparent corner cannot hide in a viewer.
    fn opaque(&self) -> Pic {
        let mut me = self.clone();
        for px in me.rgba.chunks_exact_mut(4) {
            px[3] = 255;
        }
        me
    }

    /// Two pictures side by side with a gap, nearest-resized to the taller.
    fn beside(&self, other: &Pic, gap: usize) -> Pic {
        let h = self.h.max(other.h);
        let w = self.w + gap + other.w;
        let mut rgba = vec![0u8; w * h * 4];
        for px in rgba.chunks_exact_mut(4) {
            px.copy_from_slice(&[24, 24, 28, 255]);
        }
        for (pic, ox) in [(self, 0), (other, self.w + gap)] {
            for y in 0..pic.h {
                let d = (y * w + ox) * 4;
                rgba[d..d + pic.w * 4].copy_from_slice(&pic.rgba[y * pic.w * 4..][..pic.w * 4]);
            }
        }
        Pic { w, h, rgba }
    }

    /// Before/after split: the left half of `self`, the right half of
    /// `other` (same size), with a 3-px white divider.
    fn split(&self, other: &Pic) -> Pic {
        let mut me = self.clone();
        let mid = self.w / 2;
        for y in 0..self.h {
            for x in mid..self.w {
                let i = (y * self.w + x) * 4;
                let px = if x < mid + 3 {
                    [255, 255, 255, 255]
                } else {
                    [other.rgba[i], other.rgba[i + 1], other.rgba[i + 2], 255]
                };
                me.rgba[i..i + 4].copy_from_slice(&px);
            }
        }
        me
    }

    /// `self` centred above `other` (the widescreen pairs: 4:3 over wide),
    /// on the dark background, `gap` rows apart.
    fn above_centred(&self, other: &Pic, gap: usize) -> Pic {
        let w = self.w.max(other.w);
        let h = self.h + gap + other.h;
        let mut rgba = vec![0u8; w * h * 4];
        for px in rgba.chunks_exact_mut(4) {
            px.copy_from_slice(&[24, 24, 28, 255]);
        }
        for (pic, oy) in [(self, 0), (other, self.h + gap)] {
            let ox = (w - pic.w) / 2;
            for y in 0..pic.h {
                let d = ((oy + y) * w + ox) * 4;
                rgba[d..d + pic.w * 4].copy_from_slice(&pic.rgba[y * pic.w * 4..][..pic.w * 4]);
            }
        }
        Pic { w, h, rgba }
    }

    /// Nearest resize to a width (box-free; used only for the contact sheet).
    fn fit_width(&self, w: usize) -> Pic {
        let h = (self.h * w / self.w).max(1);
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            let sy = y * self.h / h;
            for x in 0..w {
                let sx = x * self.w / w;
                rgba[(y * w + x) * 4..][..4]
                    .copy_from_slice(&self.rgba[(sy * self.w + sx) * 4..][..4]);
            }
        }
        Pic { w, h, rgba }
    }
}

fn out_dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/screens/w30-w31");
    std::fs::create_dir_all(&d).expect("create screens dir");
    d
}

fn probe_dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/screens-probe");
    std::fs::create_dir_all(&d).expect("create probe dir");
    d
}

fn save(path: &Path, pic: &Pic) {
    image::save_buffer(
        path,
        &pic.rgba,
        u32::try_from(pic.w).unwrap(),
        u32::try_from(pic.h).unwrap(),
        image::ColorType::Rgba8,
    )
    .expect("write png");
    println!("SHOT {}", path.display());
}

fn wanted(name: &str) -> bool {
    std::env::var("RF_SCREENS_ONLY").map_or(true, |f| name.contains(&f))
}

fn rom(console: &str, file: &str) -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = Path::new(&root).join(console).join(file);
    p.exists().then_some(p)
}

fn new_harness() -> H {
    let mut h = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::default())
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h
}

fn run_n(h: &mut H, n: usize) {
    for _ in 0..n {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

fn run_frames(h: &mut H, target: u64, deadline: Duration) {
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < deadline {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

fn key_event(h: &mut H, key: egui::Key, pressed: bool) {
    h.input_mut().events.push(egui::Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    });
}

/// Press, hold `frames` core frames, release, settle a few frames. Paced
/// (real time), so a tap is a tap.
fn tap(h: &mut H, key: egui::Key, frames: u64) {
    key_event(h, key, true);
    let t = h.state().frame_count_for_test() + frames;
    run_frames(h, t, Duration::from_secs(60));
    key_event(h, key, false);
    let t = h.state().frame_count_for_test() + 8;
    run_frames(h, t, Duration::from_secs(60));
}

/// Advance `frames` core frames with pacing off (no input): fast scripting.
fn wait(h: &mut H, tag: &str, frames: u64) {
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
    let end = h.state().frame_count_for_test() + frames;
    let mut next_probe = h.state().frame_count_for_test() + 600;
    while h.state().frame_count_for_test() < end {
        run_frames(h, next_probe.min(end), Duration::from_secs(300));
        if h.state().frame_count_for_test() >= next_probe {
            if let Some(p) = grab(h) {
                let name = format!("probe-{tag}-{:06}.png", h.state().frame_count_for_test());
                save(&probe_dir().join(name), &p);
            }
            next_probe += 600;
        }
    }
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(true));
}

/// Hold keys for `frames` core frames at real speed, then release.
fn hold_for(h: &mut H, keys: &[egui::Key], frames: u64) {
    for &k in keys {
        key_event(h, k, true);
    }
    let t = h.state().frame_count_for_test() + frames;
    run_frames(h, t, Duration::from_secs(300));
    for &k in keys {
        key_event(h, k, false);
    }
    run_n(h, 6);
}

fn grab(h: &H) -> Option<Pic> {
    let rgba = h.state().last_frame_rgba_for_test()?;
    let (w, hh) = h.state().frame_size_for_test()?;
    (rgba.len() == w * hh * 4).then_some(Pic { w, h: hh, rgba })
}

/// The whole window as the user sees it, then the Original view's rect.
fn grab_play_area(h: &mut H) -> Option<Pic> {
    let img = h.render().ok()?;
    let (iw, ih) = (img.width() as usize, img.height() as usize);
    let full = Pic {
        w: iw,
        h: ih,
        rgba: img.into_raw(),
    };
    save(&probe_dir().join("probe-window.png"), &full);
    let (rect, _) = h.state().play_rect_for_test()?;
    let scale = iw as f32 / WINDOW_SIZE[0];
    let x0 = (rect.min.x * scale).max(0.0) as usize;
    let y0 = (rect.min.y * scale).max(0.0) as usize;
    let cw = ((rect.width() * scale) as usize).min(iw - x0);
    let ch = ((rect.height() * scale) as usize).min(ih - y0);
    Some(full.crop(x0, y0, cw, ch))
}

fn open_and_run(h: &mut H, path: &Path) {
    h.state_mut().open_rom_path(path);
    run_n(h, 3);
    h.state_mut().resume_for_test();
    run_frames(h, 20, Duration::from_secs(60));
}

/// Pause on the current frame. Pairs are captured on consecutive frames:
/// "off" after one stepped frame, "on" after the next (a save-state replay
/// would give the identical frame, but the SNES state slots are not
/// readable through this harness's NES migration path).
fn snapshot(h: &mut H, _tag: &str) {
    h.state_mut().pause_for_test();
    run_n(h, 30);
}

/// Step exactly one frame and let the UI consume it.
fn step_from(h: &mut H, _state: &()) {
    h.state().send_core_command_for_test(CoreCommand::StepFrame);
    run_n(h, 120);
}

/// Save the current frame under `target/screens-probe/` (scene checks).
fn probe(h: &H, tag: &str) {
    if let Some(p) = grab(h) {
        save(&probe_dir().join(format!("probe-{tag}.png")), &p);
    }
}

fn release_all(h: &mut H) {
    use egui::Key as K;
    for k in [
        K::ArrowUp,
        K::ArrowDown,
        K::ArrowLeft,
        K::ArrowRight,
        K::Z,
        K::X,
        K::A,
        K::S,
        K::Enter,
    ] {
        key_event(h, k, false);
    }
    run_n(h, 10);
}

fn skip(readme: &mut Vec<String>, name: &str, why: &str) {
    println!("SKIP {name}: {why}");
    readme.push(format!("{name}|SKIPPED|{why}"));
}

fn note(readme: &mut Vec<String>, line: String) {
    println!("NOTE {line}");
    readme.push(line);
}

// ---------------------------------------------------------------- shots

fn shot_hdmode7(readme: &mut Vec<String>) {
    use egui::Key as K;
    let name = "hdmode7-4x";
    if !wanted(name) {
        return;
    }
    let game = ["F-Zero (USA).zip", "Super Mario Kart (USA).zip"]
        .iter()
        .find_map(|f| rom("snes", f).map(|p| (p, (*f).to_string())));
    let Some((path, file)) = game else {
        return skip(
            readme,
            name,
            "neither F-Zero (USA) nor Super Mario Kart (USA) found",
        );
    };
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    wait(&mut h, "fzero", 240);
    // Through title, mode, class, car, into the race.
    for i in 0..14 {
        tap(&mut h, if i % 2 == 0 { K::Enter } else { K::X }, 6);
        wait(&mut h, "fzero", 100);
    }
    wait(&mut h, "fzero", 400);
    hold_for(&mut h, &[K::X], 240);
    release_all(&mut h);
    let st = snapshot(&mut h, "hdm7");
    // Off: native.
    h.state_mut().set_mode_for_test(Mode::Enhanced);
    h.state_mut().set_hd_mode7_for_test(false, 4);
    run_n(&mut h, 10);
    step_from(&mut h, &st);
    let Some(off) = grab(&h) else {
        return skip(readme, name, "no frame");
    };
    // On: 4x with supersampling.
    h.state_mut().edit_game_settings_for_test(|s| {
        s.hd_mode7_supersample = true;
    });
    h.state_mut().set_hd_mode7_for_test(true, 4);
    run_n(&mut h, 20);
    step_from(&mut h, &st);
    let Some(on) = grab(&h) else {
        return skip(readme, name, "no frame");
    };
    println!("hdmode7: off {}x{} on {}x{}", off.w, off.h, on.w, on.h);
    let off4 = if off.w * 4 == on.w {
        off.upscale(4)
    } else {
        off.upscale(on.w / off.w.max(1))
    };
    let d = out_dir();
    save(&d.join("hdmode7-4x-off.png"), &off4.opaque());
    save(&d.join("hdmode7-4x-on.png"), &on.opaque());
    note(
        readme,
        format!(
            "hdmode7-4x|{file}|{}x{} native, nearest to {}x{}; HD {}x{}",
            off.w, off.h, off4.w, off4.h, on.w, on.h
        ),
    );
}

/// Menus, the map, Yoshi's Island 1, and the fade-in: the shared start of
/// the Super Mario World widescreen shots.
fn smw_enter_yi1(h: &mut H) {
    use egui::Key as K;
    wait(h, "smw", 200);
    for i in 0..10 {
        tap(h, if i % 2 == 0 { K::Enter } else { K::X }, 6);
        wait(h, "smw", 100);
    }
    wait(h, "smw", 300);
    // Close the "Welcome to Dinosaur Land" message; the game then shows the
    // map at Yoshi's House.
    tap(h, K::X, 8);
    wait(h, "smw", 400);
    let dir = match std::env::var("RF_SMW_MAP_DIR").as_deref() {
        Ok("right") => K::ArrowRight,
        Ok("up") => K::ArrowUp,
        _ => K::ArrowLeft,
    };
    tap(h, dir, 8);
    wait(h, "smw", 150);
    probe(h, "smw-map");
    tap(h, K::X, 8);
    // "MARIO START!" and the fade-in, but not long enough for the first
    // enemy to walk into a Mario standing still.
    let enter: u64 = std::env::var("RF_SMW_ENTER_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(150);
    wait(h, "smw", enter);
    probe(h, "smw-level-start");
}

/// Ticket W32-07: the level scout. 32:9 right after entering Yoshi's Island
/// 1 (Mario has not moved), first WITHOUT the scout (only what the camera
/// has shown: nothing to the right), then with it. Also measures what the
/// scout costs: live frames per second while it runs, against without.
fn shot_scout(readme: &mut Vec<String>) {
    let name = "scout";
    if !wanted(name) {
        return;
    }
    let Some(path) = rom("snes", "Super Mario World (USA).zip") else {
        return skip(readme, name, "Super Mario World (USA) not found");
    };
    use std::sync::atomic::Ordering::Relaxed;
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    smw_enter_yi1(&mut h);
    let telemetry = h.state().scout_telemetry_for_test();
    // Control: widescreen on, scout off, a few frames in (the first screen
    // is all the camera has shown).
    h.state_mut().set_scout_enabled_for_test(false);
    h.state_mut()
        .set_widescreen_aspect_for_test(Aspect::Wide32x9);
    h.state_mut().set_widescreen(true);
    let t = h.state().frame_count_for_test() + 20;
    run_frames(&mut h, t, Duration::from_secs(60));
    let off = grab(&h);
    // `RF_SCOUT_PACED=1`: the live fps of 150 paced frames with the scout
    // OFF, the control for the scout-on figure below (W32-08). 150, not
    // more: a standing Mario dies ~220 frames into the level.
    let paced = std::env::var("RF_SCOUT_PACED").is_ok();
    let fps_off = paced.then(|| {
        let f0 = h.state().frame_count_for_test();
        let t0 = Instant::now();
        run_frames(&mut h, f0 + 150, Duration::from_secs(120));
        (h.state().frame_count_for_test() - f0) as f64 / t0.elapsed().as_secs_f64()
    });
    // Scout on: re-arm widescreen with the scout enabled and let it run to
    // the end of its passes at the real (paced) speed, as a player gets it.
    // (Mario is left standing: the first enemy kills him ~220 frames into
    // the level; the scout works from a snapshot and does not care.)
    h.state_mut().set_scout_enabled_for_test(true);
    // `RF_SCOUT_PACED=1` measures the real cost at the real speed (the game
    // is then long over for Mario, so the picture is not the point);
    // otherwise the live loop is unpaced and paused once the worker is
    // running, so Mario is still standing in the level when it ends.
    if !paced {
        h.state()
            .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
    }
    h.state_mut().set_widescreen(true);
    let t1 = Instant::now();
    let f1 = h.state().frame_count_for_test();
    // The scout runs on its own thread beside the live loop (W32-08), and a
    // standing Mario dies ~220 frames into the level, which sends the game
    // back to the map. So for the PICTURE the live game is paused as soon as
    // the worker has started (the worker goes on, the paused core keeps
    // merging its deltas); a paced run lets the game run for the fps figure.
    let mut finished_at = None;
    if paced {
        while t1.elapsed() < Duration::from_secs(300) {
            let t = h.state().frame_count_for_test() + 10;
            run_frames(&mut h, t, Duration::from_secs(10));
            if telemetry.finished.load(Relaxed) >= 1 {
                finished_at = Some((h.state().frame_count_for_test() - f1, t1.elapsed()));
                break;
            }
        }
    } else {
        while telemetry.started.load(Relaxed) == 0 && t1.elapsed() < Duration::from_secs(60) {
            let t = h.state().frame_count_for_test() + 2;
            run_frames(&mut h, t, Duration::from_secs(10));
        }
        h.state().send_core_command_for_test(CoreCommand::Pause);
        while t1.elapsed() < Duration::from_secs(300) {
            std::thread::sleep(Duration::from_millis(100));
            if telemetry.finished.load(Relaxed) >= 1 {
                finished_at = Some((h.state().frame_count_for_test() - f1, t1.elapsed()));
                break;
            }
        }
        h.state().send_core_command_for_test(CoreCommand::Resume);
    }
    let (Some((live_frames, took)), Some(off)) = (finished_at, off) else {
        return skip(readme, name, "the scout never finished");
    };
    // Settle a few frames so the margins use the merged canvases.
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(true));
    let t = h.state().frame_count_for_test() + 30;
    run_frames(&mut h, t, Duration::from_secs(60));
    let Some(on) = grab(&h) else {
        return skip(readme, name, "no frame");
    };
    let (sf, sw, mx, sl, cells) = (
        telemetry.frames.load(Relaxed),
        telemetry.wall_us.load(Relaxed),
        telemetry.max_slice_us.load(Relaxed),
        telemetry.slices.load(Relaxed),
        telemetry.cells.load(Relaxed),
    );
    // A second control, after the scout is done (the game is on the map by
    // now, so this is not the same scene, only the same machine load).
    let fps_after = paced.then(|| {
        let f0 = h.state().frame_count_for_test();
        let t0 = Instant::now();
        run_frames(&mut h, f0 + 300, Duration::from_secs(120));
        (h.state().frame_count_for_test() - f0) as f64 / t0.elapsed().as_secs_f64()
    });
    let load = std::process::Command::new("uptime")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!(
        "SCOUT {sf} scout frames over {live_frames} live frames ({:.1} s wall; worker ran {:.1} s), \
         {sl} deltas merged on the core thread ({:.2} ms in total, longest {:.2} ms), {cells} cells \
         merged; scouts started {} finished {} aborted {}, passes ended {:?}; live fps with the \
         scout ON {:.1} ({}), OFF before {} / after {}; {load}",
        took.as_secs_f64(),
        sw as f64 / 1e6,
        telemetry.core_us.load(Relaxed) as f64 / 1000.0,
        mx as f64 / 1000.0,
        telemetry.started.load(Relaxed),
        telemetry.finished.load(Relaxed),
        telemetry.aborted.load(Relaxed),
        telemetry.last_ends.lock().map(|e| e.clone()).unwrap_or_default(),
        live_frames as f64 / took.as_secs_f64(),
        if paced { "paced" } else { "UNPACED, not comparable" },
        fps_off.map_or("n/a".to_string(), |f| format!("{f:.1}")),
        fps_after.map_or("n/a".to_string(), |f| format!("{f:.1}")),
    );
    if paced {
        return; // the game is long over for Mario: no picture worth keeping
    }
    let d = out_dir();
    save(&d.join("scout-32x9-off.png"), &off.opaque());
    save(&d.join("scout-32x9-on.png"), &on.opaque());
    note(
        readme,
        format!(
            "scout-32x9|Super Mario World (USA)|{}x{}, {sf} scout frames over {live_frames} live frames",
            on.w, on.h
        ),
    );
}

/// Ticket W32-09: Super Mario Bros. 3, to World 1-1 (NES: Enter = Start, X = A).
fn smb3_enter_level(h: &mut H) {
    use egui::Key as K;
    wait(h, "smb3", 300);
    tap(h, K::Enter, 6);
    wait(h, "smb3", 120);
    tap(h, K::Enter, 6);
    wait(h, "smb3", 400);
    tap(h, K::ArrowRight, 8);
    wait(h, "smb3", 60);
    tap(h, K::ArrowUp, 8);
    wait(h, "smb3", 60);
    tap(h, K::X, 8);
    // The level loads and fades in (the scout's start gate, $000F, turns 1
    // 80 frames after the A press, and Mario stands unhurt for 300+).
    wait(h, "smb3", 100);
    probe(h, "smb3-level-start");
}

/// Ticket W32-09: Mega Man 2, to the start of Air Man's stage.
fn mm2_enter_level(h: &mut H) {
    use egui::Key as K;
    wait(h, "mm2", 400);
    tap(h, K::Enter, 6);
    wait(h, "mm2", 120);
    tap(h, K::Enter, 6);
    wait(h, "mm2", 300);
    tap(h, K::Enter, 6);
    wait(h, "mm2", 300);
    // The stage select opens on Dr. Wily's square; up is Air Man.
    tap(h, K::ArrowUp, 8);
    wait(h, "mm2", 20);
    tap(h, K::Enter, 6);
    wait(h, "mm2", 800);
    probe(h, "mm2-level-start");
}

/// Ticket W32-09: the level scout on the NES. 32:9 right after entering the
/// level (the player has not moved), scout off and then on; the live game is
/// paused once the worker runs so the player is still at the start when the
/// picture is taken.
fn shot_scout_nes(readme: &mut Vec<String>) {
    let name = "scout-nes";
    if !wanted(name) {
        return;
    }
    for (tag, file, enter) in [
        (
            "smb3",
            "Super Mario Bros. 3 (USA).zip",
            smb3_enter_level as fn(&mut H),
        ),
        ("mm2", "Mega Man 2 (USA).zip", mm2_enter_level as fn(&mut H)),
    ] {
        let Some(path) = rom("nes", file) else {
            skip(
                readme,
                &format!("scout-{tag}"),
                &format!("{file} not found"),
            );
            continue;
        };
        use std::sync::atomic::Ordering::Relaxed;
        let mut h = new_harness();
        open_and_run(&mut h, &path);
        enter(&mut h);
        let telemetry = h.state().scout_telemetry_for_test();
        h.state_mut().set_scout_enabled_for_test(false);
        h.state_mut()
            .set_widescreen_aspect_for_test(Aspect::Wide32x9);
        h.state_mut().set_widescreen(true);
        let t = h.state().frame_count_for_test() + 20;
        run_frames(&mut h, t, Duration::from_secs(60));
        let off = grab(&h);
        h.state_mut().set_scout_enabled_for_test(true);
        h.state()
            .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
        h.state_mut().set_widescreen(true);
        let t1 = Instant::now();
        while telemetry.started.load(Relaxed) == 0 && t1.elapsed() < Duration::from_secs(60) {
            let t = h.state().frame_count_for_test() + 2;
            run_frames(&mut h, t, Duration::from_secs(10));
        }
        h.state().send_core_command_for_test(CoreCommand::Pause);
        let mut finished = false;
        while t1.elapsed() < Duration::from_secs(300) {
            std::thread::sleep(Duration::from_millis(100));
            if telemetry.finished.load(Relaxed) >= 1 {
                finished = true;
                break;
            }
        }
        h.state().send_core_command_for_test(CoreCommand::Resume);
        h.state()
            .send_core_command_for_test(CoreCommand::SetPacingEnabled(true));
        let t = h.state().frame_count_for_test() + 90;
        run_frames(&mut h, t, Duration::from_secs(60));
        let (Some(off), Some(on)) = (off, grab(&h)) else {
            skip(readme, &format!("scout-{tag}"), "no frame");
            continue;
        };
        println!(
            "SCOUT-NES {tag}: finished {finished}; {} scout frames, {} cells merged; started {} finished {} aborted {}, passes ended {:?}",
            telemetry.frames.load(Relaxed),
            telemetry.cells.load(Relaxed),
            telemetry.started.load(Relaxed),
            telemetry.finished.load(Relaxed),
            telemetry.aborted.load(Relaxed),
            telemetry.last_ends.lock().map(|e| e.clone()).unwrap_or_default(),
        );
        let d = out_dir();
        save(&d.join(format!("scout-{tag}-32x9-off.png")), &off.opaque());
        save(&d.join(format!("scout-{tag}-32x9-on.png")), &on.opaque());
        note(readme, format!("scout-{tag}-32x9|{file}|{}x{}", on.w, on.h));
    }
}

fn shot_widescreen(readme: &mut Vec<String>) {
    let name = "widescreen";
    if !wanted(name) {
        return;
    }
    let Some(path) = rom("snes", "Super Mario World (USA).zip") else {
        return skip(readme, name, "Super Mario World (USA) not found");
    };
    use egui::Key as K;
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    // Into a real level: menus to the map (as the lens shot), walk to
    // Yoshi's Island 1 (`RF_SMW_MAP_DIR`, default left), enter it, then run
    // right with a jump now and then, so the screen scrolls and the
    // stitched canvas fills the margins.
    smw_enter_yi1(&mut h);
    h.state_mut().set_widescreen(true);
    h.state_mut()
        .set_widescreen_aspect_for_test(Aspect::Wide32x9);
    let run_s: u64 = std::env::var("RF_SMW_RUN_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    // An enemy stands next to Mario at the start: jump first, then walk
    // right hopping often (landing on an enemy is a stomp, walking into
    // one is not).
    key_event(&mut h, K::ArrowRight, true);
    let mut ran = 0;
    while ran < run_s {
        hold_for(&mut h, &[K::X], 18);
        let t = h.state().frame_count_for_test() + 22;
        run_frames(&mut h, t, Duration::from_secs(120));
        ran += 40;
        probe(&h, &format!("smw-run-{ran:04}"));
    }
    key_event(&mut h, K::ArrowRight, false);
    run_n(&mut h, 6);
    // The stitched canvas only holds what the camera has seen, so walk back
    // left for a while: then the level both ahead and behind is known and
    // fills a 32:9 view.
    let back: u64 = std::env::var("RF_SMW_BACK_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(160);
    key_event(&mut h, K::ArrowLeft, true);
    let mut walked = 0;
    while walked < back {
        hold_for(&mut h, &[K::X], 18);
        let t = h.state().frame_count_for_test() + 22;
        run_frames(&mut h, t, Duration::from_secs(120));
        walked += 40;
        probe(&h, &format!("smw-back-{walked:04}"));
    }
    key_event(&mut h, K::ArrowLeft, false);
    run_n(&mut h, 6);
    probe(&h, "smw-ran");
    let mut sources = Vec::new();
    for (aspect, tagname) in [(Aspect::Wide16x9, "16x9"), (Aspect::Wide32x9, "32x9")] {
        // Captured LIVE (Mario standing still), not paused and stepped: a
        // stepped frame carries no scroll position, so its margins fall back
        // to VRAM. "On" then "off" a few frames apart.
        h.state_mut().set_widescreen_aspect_for_test(aspect);
        h.state_mut().set_widescreen(true);
        let t = h.state().frame_count_for_test() + 30;
        run_frames(&mut h, t, Duration::from_secs(60));
        // "On" is the stitched canvas (per-layer since W31-07); the VRAM
        // margins are kept as a probe for comparison.
        let on = grab(&h);
        let src = h.state().widescreen_source_for_test();
        h.state_mut().force_vram_margins_for_test(true);
        let t = h.state().frame_count_for_test() + 4;
        run_frames(&mut h, t, Duration::from_secs(60));
        if let Some(p) = grab(&h) {
            save(
                &probe_dir().join(format!("widescreen-{tagname}-vram.png")),
                &p,
            );
        }
        h.state_mut().force_vram_margins_for_test(false);
        h.state_mut().set_widescreen(false);
        let t = h.state().frame_count_for_test() + 4;
        run_frames(&mut h, t, Duration::from_secs(60));
        let off = grab(&h);
        let (Some(off), Some(on)) = (off, on) else {
            skip(readme, &format!("widescreen-{tagname}"), "no frame");
            continue;
        };
        println!(
            "widescreen {tagname}: off {}x{} on {}x{} source {src:?}",
            off.w, off.h, on.w, on.h
        );
        let d = out_dir();
        // Off is the native 4:3 frame; show it centred in the wide canvas'
        // height at the same pixel scale (no resampling).
        save(
            &d.join(format!("widescreen-{tagname}-off.png")),
            &off.opaque(),
        );
        save(
            &d.join(format!("widescreen-{tagname}-on.png")),
            &on.opaque(),
        );
        sources.push((tagname, src == MarginSource::StitchedCanvas, src));
        note(
            readme,
            format!(
                "widescreen-{tagname}|Super Mario World (USA)|off {}x{}, on {}x{}, margin source {src:?}",
                off.w, off.h, on.w, on.h
            ),
        );
    }
}

/// Ticket W32-03: the real wide-snes Super Mario World patch (`.bps` +
/// `.bso`, W48 -> 352 wide) under the bsnes-hd convention. The ROM and
/// patch are the user's own files in `RF_WIDESNES_DIR` (`rom/Super Mario
/// World (USA).sfc`, `smw-widescreen.bps`, `smw-widescreen.bso`), copied to
/// a scratch dir as `SMW.sfc/.bps/.bso`; nothing is written back.
fn shot_widesnes(readme: &mut Vec<String>) {
    let name = "widesnes";
    if !wanted(name) {
        return;
    }
    let Ok(src) = std::env::var("RF_WIDESNES_DIR") else {
        return skip(readme, name, "RF_WIDESNES_DIR not set");
    };
    let src = PathBuf::from(src);
    let work = std::env::temp_dir().join(format!("rf_widesnes_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("scratch");
    for (from, to) in [
        (src.join("rom/Super Mario World (USA).sfc"), "SMW.sfc"),
        (src.join("smw-widescreen.bps"), "SMW.bps"),
        (src.join("smw-widescreen.bso"), "SMW.bso"),
    ] {
        if std::fs::copy(&from, work.join(to)).is_err() {
            return skip(readme, name, &format!("{} missing", from.display()));
        }
    }
    let path = work.join("SMW.sfc");
    use egui::Key as K;
    let mut h = new_harness();
    // First open: opt in to the patch and Enhanced mode (persisted per
    // game), then reopen so the patch applies.
    h.state_mut().open_rom_path(&path);
    run_n(&mut h, 3);
    // `RF_WIDESNES_MODE=accuracy` keeps the patch but drops the convention,
    // to tell a patch that does not boot from a convention that draws nothing.
    let mode = if std::env::var("RF_WIDESNES_MODE").as_deref() == Ok("accuracy") {
        Mode::Accuracy
    } else {
        Mode::Enhanced
    };
    h.state_mut().set_mode_for_test(mode);
    h.state_mut().enable_patch_and_save_for_test("SMW.bps");
    open_and_run(&mut h, &path);
    println!(
        "widesnes: reference={} convention={:?} dots={:?}",
        h.state().is_reference_session(),
        h.state().widescreen_convention(),
        h.state().patch_widescreen_dots()
    );
    wait(&mut h, "widesnes", 200);
    probe(&h, "widesnes-title");
    if let Some(p) = grab(&h) {
        println!("widesnes title frame {}x{}", p.w, p.h);
    }
    for i in 0..10 {
        tap(&mut h, if i % 2 == 0 { K::Enter } else { K::X }, 6);
        wait(&mut h, "widesnes", 100);
    }
    wait(&mut h, "widesnes", 300);
    tap(&mut h, K::X, 8);
    wait(&mut h, "widesnes", 400);
    let dir = match std::env::var("RF_SMW_MAP_DIR").as_deref() {
        Ok("right") => K::ArrowRight,
        Ok("up") => K::ArrowUp,
        _ => K::ArrowLeft,
    };
    tap(&mut h, dir, 8);
    wait(&mut h, "widesnes", 150);
    probe(&h, "widesnes-map");
    tap(&mut h, K::X, 8);
    let enter: u64 = std::env::var("RF_SMW_ENTER_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(150);
    wait(&mut h, "widesnes", enter);
    probe(&h, "widesnes-level-start");
    // Walk right, hopping, so enemies and items come into the margins.
    let run_s: u64 = std::env::var("RF_SMW_RUN_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    key_event(&mut h, K::ArrowRight, true);
    let mut ran = 0;
    while ran < run_s {
        hold_for(&mut h, &[K::X], 18);
        let t = h.state().frame_count_for_test() + 22;
        run_frames(&mut h, t, Duration::from_secs(120));
        ran += 40;
        probe(&h, &format!("widesnes-run-{ran:04}"));
    }
    key_event(&mut h, K::ArrowRight, false);
    run_n(&mut h, 6);
    let t = h.state().frame_count_for_test() + 30;
    run_frames(&mut h, t, Duration::from_secs(60));
    let Some(on) = grab(&h) else {
        return skip(readme, name, "no frame");
    };
    println!(
        "widesnes: frame {}x{} source {:?}",
        on.w,
        on.h,
        h.state().widescreen_source_for_test()
    );
    save(&out_dir().join("widesnes-352.png"), &on.opaque());
    // A wider aspect fills beyond the patch's own width.
    h.state_mut()
        .set_widescreen_aspect_for_test(Aspect::Wide32x9);
    let t = h.state().frame_count_for_test() + 30;
    run_frames(&mut h, t, Duration::from_secs(60));
    probe(&h, "widesnes-32x9");
    note(
        readme,
        format!(
            "widesnes-352|Super Mario World + wide-snes patch|{}x{}, bsnes-hd convention",
            on.w, on.h
        ),
    );
}

fn shot_hd2d(readme: &mut Vec<String>) {
    use egui::Key as K;
    let name = "hd2d-look";
    if !wanted(name) {
        return;
    }
    // No A Link to the Past heights are painted yet, and NES Zelda only
    // decodes its level in play (past name registration). The in-repo
    // RF-Scroller test game's profile decodes its level with collision at
    // load, so Diorama walls - and the HD-2D look - work on it.
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/nes/rf-scroller/build/rf-scroller.nes");
    if !path.exists() {
        return skip(readme, name, "RF-Scroller fixture not built");
    }
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    wait(&mut h, "scroller", 300);
    hold_for(&mut h, &[K::ArrowRight], 90);
    release_all(&mut h);
    h.state_mut().set_mode_for_test(Mode::GameAware);
    run_n(&mut h, 30);
    println!(
        "hd2d: decoded level {}",
        h.state().has_decoded_level_for_test()
    );
    let st = snapshot(&mut h, "zelda");
    step_from(&mut h, &st);
    let Some(off) = grab_play_area(&mut h) else {
        return skip(readme, name, "no play rect");
    };
    if let Some(why) = h.state().hd2d_unavailable_for_test() {
        return skip(readme, name, &format!("HD-2D look unavailable: {why}"));
    }
    h.state_mut().toggle_hd2d_for_test();
    run_n(&mut h, 30);
    step_from(&mut h, &st);
    step_from(&mut h, &st);
    let on = grab_play_area(&mut h).unwrap_or_else(|| off.clone());
    println!(
        "hd2d: opaque diorama px {:?}",
        h.state().diorama_opaque_pixels_for_test()
    );
    let d = out_dir();
    save(&d.join("hd2d-look-off.png"), &off.opaque());
    save(&d.join("hd2d-look-on.png"), &on.opaque());
    note(
        readme,
        "hd2d-look|RF-Scroller (in-repo NES test game; decoded level + collision)|Game-Aware; HD-2D look preset; window crop of the play area. A Link to the Past has no painted heights yet".to_string(),
    );
}

fn shot_lens(readme: &mut Vec<String>) {
    use egui::Key as K;
    let name = "lens";
    if !wanted(name) {
        return;
    }
    let Some(path) = rom("snes", "Super Mario World (USA).zip") else {
        return skip(readme, name, "Super Mario World (USA) not found");
    };
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    wait(&mut h, "smwlens", 200);
    for i in 0..10 {
        tap(&mut h, if i % 2 == 0 { K::Enter } else { K::X }, 6);
        wait(&mut h, "smwlens", 100);
    }
    wait(&mut h, "smwlens", 300);
    hold_for(&mut h, &[K::ArrowRight, K::A], 120);
    release_all(&mut h);
    let st = snapshot(&mut h, "lens");
    h.state_mut().edit_lens_for_test(|s| s.lens_clear());
    run_n(&mut h, 10);
    step_from(&mut h, &st);
    let Some(off) = grab_play_area(&mut h) else {
        return skip(readme, name, "no play rect");
    };
    h.state_mut().edit_lens_for_test(|s| {
        s.lens_bloom = true;
        s.lens_tiltshift = true;
        s.lens_shadows = true;
        s.lens_grade = true;
        s.lens_follow_clock = false;
        s.lens_time_of_day = 20;
    });
    run_n(&mut h, 30);
    step_from(&mut h, &st);
    step_from(&mut h, &st);
    let on = grab_play_area(&mut h).unwrap_or_else(|| off.clone());
    let d = out_dir();
    save(&d.join("lens-off.png"), &off.opaque());
    save(&d.join("lens-on.png"), &on.opaque());
    note(
        readme,
        "lens|Super Mario World (USA)|bloom + tilt-shift + soft shadows + grading at 0.2 time, no weather; window crop of the play area".to_string(),
    );
}

fn shot_superfx_hd(readme: &mut Vec<String>) {
    let name = "superfx-hd";
    if !wanted(name) {
        return;
    }
    let game = ["Star Fox (USA).zip", "Star Fox (USA) (Rev 1).zip"]
        .iter()
        .find_map(|f| rom("snes", f).map(|p| (p, (*f).to_string())));
    let Some((path, file)) = game else {
        return skip(readme, name, "Star Fox (USA) not found");
    };
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    // No input: the intro (Great Fox and the Arwings launching, frames
    // ~850-1100) is where the W31 proof ran.
    wait(&mut h, "starfox", 880);
    // A render job spans several frames, so the HD path needs to run for a
    // while before it has a job to match: run with it on, pause, capture
    // "on", then switch it off and step one frame for "off" (consecutive
    // frames, not the identical one).
    h.state_mut()
        .set_superfx_hd_for_test(true, 3, Mode::Enhanced, true);
    run_n(&mut h, 30);
    println!(
        "superfx effective: {}",
        h.state().superfx_hd_effective_for_test()
    );
    h.state_mut().resume_for_test();
    let t = h.state().frame_count_for_test() + 120;
    run_frames(&mut h, t, Duration::from_secs(120));
    let st = snapshot(&mut h, "sfx");
    step_from(&mut h, &st);
    let Some(on) = grab(&h) else {
        return skip(readme, name, "no frame");
    };
    h.state_mut()
        .set_superfx_hd_for_test(false, 3, Mode::Enhanced, true);
    run_n(&mut h, 20);
    step_from(&mut h, &st);
    let Some(off) = grab(&h) else {
        return skip(readme, name, "no frame");
    };
    println!("superfx: off {}x{} on {}x{}", off.w, off.h, on.w, on.h);
    let off3 = off.upscale(3);
    let d = out_dir();
    save(&d.join("superfx-hd-3x-off.png"), &off3.clone().opaque());
    save(&d.join("superfx-hd-3x-on.png"), &on.clone().opaque());
    // Zoom: 64x64 native pixels at 8x; the HD picture is `k`x native.
    let k = (on.w / off.w.max(1)).max(1);
    let (zx, zy) = zoom_origin(&off, 64);
    let zoom_off = off.crop(zx, zy, 64, 64).upscale(8);
    let zoom_on = on.crop(zx * k, zy * k, 64 * k, 64 * k).fit_width(512);
    save(
        &d.join("superfx-hd-3x-zoom.png"),
        &zoom_off.beside(&zoom_on, 8).opaque(),
    );
    note(
        readme,
        format!(
            "superfx-hd-3x|{file}|native {}x{} (nearest 3x) vs HD {}x{}; zoom crop at native ({zx},{zy}) 64x64, 8x nearest (left) vs HD crop at 512 wide (right)",
            off.w, off.h, on.w, on.h
        ),
    );
}

/// Ticket W31-05: the GPU path, for the product tour. Every image here is
/// one simulated frame: the native side is the exact frame the HD composite
/// was made from (`superfx_hd_native_for_test`), so a split lines up.
/// Variants (4x/3x, smoothing on/off) are consecutive stepped frames.
fn shot_superfx_gpu(readme: &mut Vec<String>) {
    let name = "superfx-gpu";
    if !wanted(name) {
        return;
    }
    let game = ["Star Fox (USA).zip", "Star Fox (USA) (Rev 1).zip"]
        .iter()
        .find_map(|f| rom("snes", f).map(|p| (p, (*f).to_string())));
    let Some((path, file)) = game else {
        return skip(readme, name, "Star Fox (USA) not found");
    };
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    wait(&mut h, "starfox", 880);
    h.state_mut()
        .set_superfx_hd_for_test(true, 4, Mode::Enhanced, true);
    h.state_mut().set_superfx_hd_msaa_for_test(4);
    run_n(&mut h, 30);
    h.state_mut().resume_for_test();
    let t = h.state().frame_count_for_test() + 120;
    run_frames(&mut h, t, Duration::from_secs(120));
    let st = snapshot(&mut h, "sfx-gpu");
    let mut shots: Vec<(&str, Pic, Pic)> = Vec::new();
    for (tag, scale, msaa) in [
        ("4x-smooth", 4u8, 4u8),
        ("3x-smooth", 3, 4),
        ("3x-sharp", 3, 1),
    ] {
        h.state_mut()
            .set_superfx_hd_for_test(true, scale, Mode::Enhanced, true);
        h.state_mut().set_superfx_hd_msaa_for_test(msaa);
        step_from(&mut h, &st);
        let readout = h.state().superfx_hd_readout_for_test().unwrap_or_default();
        println!("superfx-gpu {tag}: {readout}");
        if !readout.contains("HD (GPU") {
            return skip(
                readme,
                name,
                &format!("{tag}: not drawn by the GPU ({readout})"),
            );
        }
        let (Some(hd), Some((rgba, w, hh))) = (grab(&h), h.state().superfx_hd_native_for_test())
        else {
            return skip(readme, name, "no frame");
        };
        shots.push((tag, hd, Pic { w, h: hh, rgba }));
    }
    let d = out_dir();
    for (tag, hd, _) in &shots {
        save(&d.join(format!("superfx-gpu-{tag}.png")), &hd.opaque());
    }
    let (_, hd4, native4) = &shots[0];
    save(
        &d.join("superfx-gpu-native.png"),
        &native4.upscale(4).opaque(),
    );
    // Product-tour comparison: native (nearest 4x) | 4x smoothed, one frame.
    save(
        &d.join("compare-superfx-gpu.png"),
        &native4.upscale(4).split(hd4).opaque(),
    );
    // Zoom, 64x64 native pixels at 8x: native | 3x no smoothing | 3x
    // smoothed (the last two are one frame apart; the native one is the
    // smoothed one's own frame).
    let (_, sharp, _) = &shots[2];
    let (_, smooth, native3) = &shots[1];
    let (zx, zy) = zoom_origin(native3, 64);
    let zoom = |p: &Pic| {
        let k = (p.w / native3.w.max(1)).max(1);
        p.crop(zx * k, zy * k, 64 * k, 64 * k).fit_width(512)
    };
    save(
        &d.join("superfx-gpu-zoom.png"),
        &native3
            .crop(zx, zy, 64, 64)
            .upscale(8)
            .beside(&zoom(sharp), 8)
            .beside(&zoom(smooth), 8)
            .opaque(),
    );
    note(
        readme,
        format!(
            "superfx-gpu|{file}|GPU path: 4x+4x smoothing, 3x+4x smoothing, 3x no smoothing, native; zoom at native ({zx},{zy}) 64x64 8x: native | 3x sharp | 3x smooth"
        ),
    );
}

/// Product-tour comparisons: one image per feature, OFF on the left half and
/// ON on the right half with a white divider (pictures of different sizes,
/// e.g. widescreen, go side by side instead).
fn compare_images(readme: &mut Vec<String>) {
    let d = out_dir();
    let load = |p: PathBuf| -> Option<Pic> {
        let img = image::open(p).ok()?.to_rgba8();
        Some(Pic {
            w: img.width() as usize,
            h: img.height() as usize,
            rgba: img.into_raw(),
        })
    };
    // Only the pairs good enough for the tour: the HD-2D pair is a flat
    // test fixture, so it waits for a game with painted heights.
    for n in ["hdmode7-4x", "lens", "widescreen-16x9", "widescreen-32x9"] {
        let (Some(a), Some(b)) = (
            load(d.join(format!("{n}-off.png"))),
            load(d.join(format!("{n}-on.png"))),
        ) else {
            continue;
        };
        let out = if (a.w, a.h) == (b.w, b.h) {
            a.split(&b)
        } else {
            a.above_centred(&b, 8)
        };
        save(&d.join(format!("compare-{n}.png")), &out.opaque());
    }
    note(
        readme,
        "compare-*|-|tour comparisons: OFF left half | ON right half (split), or side by side when sizes differ".to_string(),
    );
}

/// The 64x64 window with the most edge energy: a slanted polygon edge is
/// where horizontal and vertical neighbours differ the most.
fn zoom_origin(p: &Pic, size: usize) -> (usize, usize) {
    let mut best = (-1i64, (0usize, 0usize));
    let lum = |x: usize, y: usize| -> i32 {
        let i = (y * p.w + x) * 4;
        i32::from(p.rgba[i]) + i32::from(p.rgba[i + 1]) + i32::from(p.rgba[i + 2])
    };
    for y0 in (0..p.h.saturating_sub(size)).step_by(8) {
        for x0 in (0..p.w.saturating_sub(size)).step_by(8) {
            let mut e = 0i64;
            for y in y0..y0 + size - 1 {
                for x in x0..x0 + size - 1 {
                    e += i64::from((lum(x, y) - lum(x + 1, y)).abs().min(60));
                    e += i64::from((lum(x, y) - lum(x, y + 1)).abs().min(60));
                }
            }
            if e > best.0 {
                best = (e, (x0, y0));
            }
        }
    }
    best.1
}

fn shot_stunt(readme: &mut Vec<String>) {
    use egui::Key as K;
    let name = "superfx-3d-drawing";
    if !wanted(name) {
        return;
    }
    let game = ["Stunt Race FX (USA) (Rev 1).zip", "Stunt Race FX (USA).zip"]
        .iter()
        .find_map(|f| rom("snes", f).map(|p| (p, (*f).to_string())));
    let Some((path, file)) = game else {
        return skip(readme, name, "Stunt Race FX not found");
    };
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    wait(&mut h, "stunt", 600);
    for i in 0..12 {
        tap(&mut h, if i % 2 == 0 { K::Enter } else { K::X }, 6);
        wait(&mut h, "stunt", 120);
    }
    wait(&mut h, "stunt", 600);
    hold_for(&mut h, &[K::X], 300);
    release_all(&mut h);
    run_n(&mut h, 20);
    let Some(p) = grab(&h) else {
        return skip(readme, name, "no frame");
    };
    save(&out_dir().join("superfx-3d-drawing.png"), &p.opaque());
    note(
        readme,
        format!("superfx-3d-drawing|{file}|a race frame; single image"),
    );
}

fn contact_sheet(readme: &mut Vec<String>) {
    let d = out_dir();
    let pairs = [
        "hdmode7-4x",
        "widescreen-16x9",
        "widescreen-32x9",
        "hd2d-look",
        "lens",
        "superfx-hd-3x",
    ];
    let cell_w = 480usize;
    let load = |p: PathBuf| -> Option<Pic> {
        let img = image::open(p).ok()?.to_rgba8();
        Some(Pic {
            w: img.width() as usize,
            h: img.height() as usize,
            rgba: img.into_raw(),
        })
    };
    let mut rows: Vec<Pic> = Vec::new();
    for n in pairs {
        let (Some(a), Some(b)) = (
            load(d.join(format!("{n}-off.png"))),
            load(d.join(format!("{n}-on.png"))),
        ) else {
            continue;
        };
        rows.push(a.fit_width(cell_w).beside(&b.fit_width(cell_w), 6));
    }
    if let Some(z) = load(d.join("superfx-hd-3x-zoom.png")) {
        rows.push(z.fit_width(cell_w * 2 + 6));
    }
    if let Some(s) = load(d.join("superfx-3d-drawing.png")) {
        rows.push(s.fit_width(cell_w));
    }
    if rows.is_empty() {
        return;
    }
    let w = rows.iter().map(|r| r.w).max().unwrap();
    let gap = 10;
    let h: usize = rows.iter().map(|r| r.h).sum::<usize>() + gap * (rows.len() - 1);
    let mut rgba = vec![0u8; w * h * 4];
    for px in rgba.chunks_exact_mut(4) {
        px.copy_from_slice(&[24, 24, 28, 255]);
    }
    let mut y = 0;
    for r in &rows {
        for yy in 0..r.h {
            let dst = ((y + yy) * w) * 4;
            rgba[dst..dst + r.w * 4].copy_from_slice(&r.rgba[yy * r.w * 4..][..r.w * 4]);
        }
        y += r.h + gap;
    }
    save(&d.join("contact-sheet.png"), &Pic { w, h, rgba });
    note(
        readme,
        "contact-sheet|-|rows in README order; pairs are OFF (left) | ON (right)".to_string(),
    );
}

#[test]
#[ignore = "drives real ROMs and writes PNGs under docs/screens/w30-w31/; run deliberately"]
fn w30_w31_screens() {
    let dir = std::env::temp_dir().join(format!("retroforge_screens_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
        std::env::set_var(
            "RETROFORGE_PROFILES_DIR",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles"),
        );
    }
    let mut readme: Vec<String> = Vec::new();
    shot_hdmode7(&mut readme);
    shot_widescreen(&mut readme);
    shot_scout(&mut readme);
    shot_scout_nes(&mut readme);
    shot_widesnes(&mut readme);
    shot_hd2d(&mut readme);
    shot_lens(&mut readme);
    shot_superfx_hd(&mut readme);
    shot_stunt(&mut readme);
    shot_superfx_gpu(&mut readme);
    if std::env::var("RF_SCREENS_ONLY").is_err() || wanted("compare") {
        compare_images(&mut readme);
    }
    if std::env::var("RF_SCREENS_ONLY").is_err() {
        contact_sheet(&mut readme);
    }
    println!("--- summary ---");
    for l in &readme {
        println!("{l}");
    }
}
