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

/// Ticket W33-07: the scripted sessions and the discovery driver.
#[allow(dead_code, unused_imports)]
#[path = "w33_07_common/mod.rs"]
mod discovery;

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

/// W34's pictures (Diorama, HD-2D, the tour) live apart from W30/W31's.
fn w34_dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/screens/w34");
    std::fs::create_dir_all(&d).expect("create screens dir");
    d
}

/// W35's pictures.
fn w35_dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/screens/w35");
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

/// `RF_SCREENS_ONLY=scout$` (a trailing `$`) names one shot exactly; without it, any shot whose
/// name contains the text.
fn wanted(name: &str) -> bool {
    std::env::var("RF_SCREENS_ONLY").map_or(true, |f| match f.strip_suffix('$') {
        Some(exact) => name == exact,
        None => name.contains(&f),
    })
}

fn rom(console: &str, file: &str) -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = Path::new(&root).join(console).join(file);
    p.exists().then_some(p)
}

/// The harness window in points. `RF_SCREENS_WINDOW=1300x1100` makes it big
/// enough for the Diorama to render at about 4x (W34-05's cost rows).
fn window_size() -> [f32; 2] {
    std::env::var("RF_SCREENS_WINDOW")
        .ok()
        .and_then(|v| {
            let (w, h) = v.split_once('x')?;
            Some([w.parse().ok()?, h.parse().ok()?])
        })
        .unwrap_or(WINDOW_SIZE)
}

fn new_harness() -> H {
    let size = window_size();
    let mut h = Harness::builder()
        .with_size(egui::vec2(size[0], size[1]))
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
    let scale = iw as f32 / window_size()[0];
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
fn step_from(h: &mut H) {
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
    snapshot(&mut h, "hdm7");
    // Off: native.
    h.state_mut().set_mode_for_test(Mode::Enhanced);
    h.state_mut().set_hd_mode7_for_test(false, 4);
    run_n(&mut h, 10);
    step_from(&mut h);
    let Some(off) = grab(&h) else {
        return skip(readme, name, "no frame");
    };
    // On: 4x with supersampling.
    h.state_mut().edit_game_settings_for_test(|s| {
        s.hd_mode7_supersample = true;
    });
    h.state_mut().set_hd_mode7_for_test(true, 4);
    run_n(&mut h, 20);
    step_from(&mut h);
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
    // A cold canvas cache (see `shot_scout_nes`): a warm one holds the scene
    // as scouted and the app starts no scout on a second visit.
    let cold = std::env::temp_dir().join(format!("rf_scout_smw_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cold);
    std::fs::create_dir_all(&cold).expect("cold cache dir");
    // SAFETY: the harness is built after this and nothing else in this
    // process reads the environment concurrently at this point.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("TMPDIR", &cold);
    }
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
            if telemetry.finished.load(Relaxed) + telemetry.gave_up.load(Relaxed) >= 1 {
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
            if telemetry.finished.load(Relaxed) + telemetry.gave_up.load(Relaxed) >= 1 {
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
         scout ON {:.1} ({}), OFF before {} / after {}; {load}; self-check: agreement {} over {} \
         shared cells, deltas merged {} slow {} rejected {} unjudged {}, probes {} (rejected {}), \
         gave up {}; {}",
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
        agreement_text(&telemetry),
        telemetry.shared_cells.load(Relaxed),
        telemetry.deltas_merged.load(Relaxed),
        telemetry.deltas_slow.load(Relaxed),
        telemetry.deltas_rejected.load(Relaxed),
        telemetry.deltas_unjudged.load(Relaxed),
        telemetry.probes.load(Relaxed),
        telemetry.probes_rejected.load(Relaxed),
        telemetry.gave_up.load(Relaxed),
        telemetry.last_summary().unwrap_or_default(),
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

/// Ticket W33-06: the scout's overall agreement with what it was checked
/// against, as a percentage.
fn agreement_text(t: &retroforge::scout::ScoutTelemetry) -> String {
    t.agreement_permille()
        .map_or("n/a".to_string(), |p| format!("{:.1}%", p as f64 / 10.0))
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
        // `RF_NES_TAG=mm2`: only that game.
        if std::env::var("RF_NES_TAG").is_ok_and(|only| only != tag) {
            continue;
        }
        let Some(path) = rom("nes", file) else {
            skip(
                readme,
                &format!("scout-{tag}"),
                &format!("{file} not found"),
            );
            continue;
        };
        use std::sync::atomic::Ordering::Relaxed;
        // The NES canvas persists across sessions (`core_thread::canvas_cache_root`
        // = `$TMPDIR/retroforge-canvas-cache`, keyed by ROM and scene). A warm
        // cache already holds the level an earlier scout drew, so the live
        // canvas knows every cell, the scout merges 0 and `-on` equals `-off`
        // (the "0 cells merged" of 2026-10-17). Give each shot a cold cache.
        let cold = std::env::temp_dir().join(format!("rf_scout_nes_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cold);
        std::fs::create_dir_all(&cold).expect("cold cache dir");
        // SAFETY: the harness is built after this and nothing else in this
        // process reads the environment concurrently at this point.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("TMPDIR", &cold);
        }
        let mut h = new_harness();
        open_and_run(&mut h, &path);
        enter(&mut h);
        let telemetry = h.state().scout_telemetry_for_test();
        // W33-06: `RF_SCOUT_FORCE_STEP=8` starts every scout at 8 px per
        // frame whatever the profile says: the self-check has to refuse it.
        let forced: Option<u16> = std::env::var("RF_SCOUT_FORCE_STEP")
            .ok()
            .and_then(|v| v.parse().ok());
        h.state_mut().set_scout_step_for_test(forced);
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
            // W33-06: a scout the self-check gave up on is over as well.
            if telemetry.finished.load(Relaxed) + telemetry.gave_up.load(Relaxed) >= 1 {
                finished = telemetry.finished.load(Relaxed) >= 1;
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
            "SCOUT-NES {tag}: finished {finished}; {} scout frames, {} cells merged; started {} finished {} aborted {}, passes ended {:?}; agreement {} over {} shared cells, deltas merged {} slow {} rejected {} unjudged {}, probes {} (rejected {}), gave up {}, forced step {forced:?}; {}",
            telemetry.frames.load(Relaxed),
            telemetry.cells.load(Relaxed),
            telemetry.started.load(Relaxed),
            telemetry.finished.load(Relaxed),
            telemetry.aborted.load(Relaxed),
            telemetry.last_ends.lock().map(|e| e.clone()).unwrap_or_default(),
            agreement_text(&telemetry),
            telemetry.shared_cells.load(Relaxed),
            telemetry.deltas_merged.load(Relaxed),
            telemetry.deltas_slow.load(Relaxed),
            telemetry.deltas_rejected.load(Relaxed),
            telemetry.deltas_unjudged.load(Relaxed),
            telemetry.probes.load(Relaxed),
            telemetry.probes_rejected.load(Relaxed),
            telemetry.gave_up.load(Relaxed),
            telemetry.last_summary().unwrap_or_default(),
        );
        // W33-08: a scout that aborted at its first frame (the live model and
        // the scout's tracker disagreeing about the level key) is no scout.
        // A scout at a standing start must also merge: the status bar once
        // corrupted the canvas and dropped its confidence to 0 (W33-10).
        assert!(
            finished && telemetry.aborted.load(Relaxed) == 0,
            "{tag}: the scout did not finish cleanly (aborted {})",
            telemetry.aborted.load(Relaxed)
        );
        assert!(
            telemetry.cells.load(Relaxed) > 0,
            "{tag}: the scout merged 0 cells into the live canvas"
        );
        // W33-06: the check refuses a forced 8 px on Super Mario Bros. 3 and
        // the scout is retried at a smaller step and accepted.
        if tag == "smb3" && forced == Some(8) {
            let report = telemetry.last_report.lock().ok().and_then(|r| r.clone());
            let winner = report.as_ref().and_then(|r| r.winner());
            assert!(
                telemetry.probes_rejected.load(Relaxed) + telemetry.deltas_rejected.load(Relaxed)
                    >= 1,
                "smb3: nothing refused the forced 8 px"
            );
            assert!(
                winner.is_some_and(|(_, step)| step < 8),
                "smb3: the scout must finish below 8 px, not {winner:?}"
            );
        }
        let d = out_dir();
        save(&d.join(format!("scout-{tag}-32x9-off.png")), &off.opaque());
        save(&d.join(format!("scout-{tag}-32x9-on.png")), &on.opaque());
        note(readme, format!("scout-{tag}-32x9|{file}|{}x{}", on.w, on.h));
    }
}

/// Ticket W33-07: Mega Man X (USA) has no `[scout]` table. A scripted session
/// finds one (camera, player, gate, hold) and the app scouts with it: 32:9,
/// scout off and then on, on a cold canvas cache.
fn shot_scout_mmx(readme: &mut Vec<String>) {
    use std::sync::atomic::Ordering::Relaxed;
    let name = "scout-mmx";
    if !wanted(name) {
        return;
    }
    let file = "Mega Man X (USA).zip";
    let Some(path) = rom("snes", file) else {
        skip(readme, name, &format!("{file} not found"));
        return;
    };
    let Some((skeleton, width, _)) = discovery::discover_game(
        discovery::MMX,
        discovery::MMX_TO_LEVEL,
        discovery::MMX_PLAY,
        false,
    ) else {
        skip(readme, name, "the finder found no table");
        return;
    };
    let mut plan = retroforge::scout_discovery::plan_of(&skeleton);
    plan.settle_frames = 30;
    let cold = std::env::temp_dir().join(format!("rf_scout_mmx_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cold);
    std::fs::create_dir_all(&cold).expect("cold cache dir");
    // SAFETY: the harness is built after this and nothing else in this
    // process reads the environment concurrently at this point.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("TMPDIR", &cold);
    }
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    // The title, then Game Start (the same presses the session used).
    {
        use egui::Key as K;
        wait(&mut h, "mmx", 300);
        // Press Start only while the picture is not yet the stage (a Start
        // press inside the stage would pause it): the stage is a lit frame
        // with many distinct colours, the story text and the fade are not.
        let in_stage = |h: &H| {
            grab(h).is_some_and(|p| {
                let mut seen = std::collections::HashSet::new();
                for c in p.rgba.chunks_exact(4) {
                    seen.insert([c[0], c[1], c[2]]);
                }
                seen.len() >= 40
            })
        };
        // Intro story text (few colours) -> title (many) -> Game Start ->
        // fade (few) -> stage (many): the stage is the second rich frame
        // after a dark one; Start is pressed on everything before it.
        let mut presses = 0;
        let mut seen_title = false;
        let mut dark_after_title = false;
        for n in 0..60 {
            let rich = in_stage(&h);
            println!(
                "MMXSEQ {n} rich {rich} frame {}",
                h.state().frame_count_for_test()
            );
            if rich && seen_title && dark_after_title {
                break;
            }
            if rich {
                seen_title = true;
            } else if seen_title {
                dark_after_title = true;
            }
            tap(&mut h, K::Enter, 6);
            presses += 1;
            wait(&mut h, "mmx", 100);
        }
        println!("MMX: {presses} Start presses to reach the stage");
        wait(&mut h, "mmx", 240);
    }
    probe(&h, "mmx-level-start");
    let telemetry = h.state().scout_telemetry_for_test();
    h.state_mut().set_scout_plan_for_test(Some(plan));
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
        if telemetry.finished.load(Relaxed) + telemetry.gave_up.load(Relaxed) >= 1 {
            finished = telemetry.finished.load(Relaxed) >= 1;
            break;
        }
    }
    h.state().send_core_command_for_test(CoreCommand::Resume);
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(true));
    let t = h.state().frame_count_for_test() + 90;
    run_frames(&mut h, t, Duration::from_secs(60));
    let (Some(off), Some(on)) = (off, grab(&h)) else {
        skip(readme, name, "no frame");
        return;
    };
    println!(
        "SCOUT-MMX: finished {finished}; scout width {width}; {} scout frames, {} cells merged; started {} finished {} aborted {}, passes ended {:?}; agreement {} over {} shared cells; gave up {}; {}",
        telemetry.frames.load(Relaxed),
        telemetry.cells.load(Relaxed),
        telemetry.started.load(Relaxed),
        telemetry.finished.load(Relaxed),
        telemetry.aborted.load(Relaxed),
        telemetry.last_ends.lock().map(|e| e.clone()).unwrap_or_default(),
        agreement_text(&telemetry),
        telemetry.shared_cells.load(Relaxed),
        telemetry.gave_up.load(Relaxed),
        telemetry.last_summary().unwrap_or_default(),
    );
    let d = out_dir();
    save(&d.join("scout-mmx-32x9-off.png"), &off.opaque());
    save(&d.join("scout-mmx-32x9-on.png"), &on.opaque());
    note(readme, format!("scout-mmx-32x9|{file}|{}x{}", on.w, on.h));
}

/// Ticket W33-08: Mega Man 2, Air Man's stage, the shaft the first horizontal
/// section ends in (the scene tracker names its axis vertical). The headless
/// rig walks the section the way the scout does (Right held, Mega Man's Y
/// held high) up to 650 frames before the end and saves the machine; the app
/// loads it and plays on the same way (the app pokes through core commands),
/// lets Mega Man fall into the shaft, and takes 32:9 shots with the scout off
/// and on, on a cold canvas cache.
fn shot_scout_mm2_climb(readme: &mut Vec<String>) {
    use std::sync::atomic::Ordering::Relaxed;
    let name = "scout-mm2-climb";
    if !wanted(name) {
        return;
    }
    let file = "Mega Man 2 (USA).zip";
    let Some(path) = rom("nes", file) else {
        skip(readme, name, &format!("{file} not found"));
        return;
    };
    let Some(mut rig) =
        discovery::Rig::open(discovery::MM2.0, discovery::MM2.1, Some(discovery::MM2.2))
    else {
        skip(readme, name, "no rig");
        return;
    };
    rig.script(discovery::MM2_TO_LEVEL);
    for _ in 0..1500u32 {
        rig.st.poke_bus(0x4A0, 0x20);
        rig.st.poke_bus(0x6C0, 0x1C);
        rig.frame(1 << 7);
    }
    let container = rig.st.save_state(1).expect("container");
    let cold = std::env::temp_dir().join(format!("rf_scout_mm2_climb_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cold);
    std::fs::create_dir_all(&cold).expect("cold cache dir");
    // SAFETY: the harness is built after this and nothing else in this
    // process reads the environment concurrently at this point.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("TMPDIR", &cold);
    }
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    let telemetry = h.state().scout_telemetry_for_test();
    h.state_mut().set_scout_enabled_for_test(false);
    h.state_mut()
        .set_widescreen_aspect_for_test(Aspect::Wide32x9);
    h.state_mut().set_widescreen(true);
    h.state()
        .send_core_command_for_test(CoreCommand::ApplyState(Box::new(container)));
    run_n(&mut h, 30);
    // Play: Right held; Mega Man's Y held high for the first 650 frames (the
    // rest of the section: the tracker needs 256 px of walking before the
    // climb to know the axis) and let go for the fall; health kept full.
    key_event(&mut h, egui::Key::ArrowRight, true);
    let poke = |h: &H, addr: u16, value: u8| {
        h.state()
            .send_core_command_for_test(CoreCommand::PokeBus { addr, value });
    };
    let t0 = h.state().frame_count_for_test();
    let deadline = Instant::now() + Duration::from_secs(120);
    while h.state().frame_count_for_test() < t0 + 1100 && Instant::now() < deadline {
        let played = h.state().frame_count_for_test().saturating_sub(t0);
        if played < 650 {
            poke(&h, 0x04A0, 0x20);
        }
        poke(&h, 0x06C0, 0x1C);
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
    probe(&h, "mm2-climb-landed");
    // Off: a few frames, still keeping Mega Man alive.
    let t = h.state().frame_count_for_test() + 20;
    while h.state().frame_count_for_test() < t {
        poke(&h, 0x06C0, 0x1C);
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
    let off = grab(&h);
    h.state_mut().set_scout_enabled_for_test(true);
    h.state_mut().set_widescreen(true);
    let t1 = Instant::now();
    while telemetry.started.load(Relaxed) == 0 && t1.elapsed() < Duration::from_secs(90) {
        poke(&h, 0x06C0, 0x1C);
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
    h.state().send_core_command_for_test(CoreCommand::Pause);
    let mut finished = false;
    while t1.elapsed() < Duration::from_secs(400) {
        std::thread::sleep(Duration::from_millis(100));
        if telemetry.finished.load(Relaxed) + telemetry.gave_up.load(Relaxed) >= 1 {
            finished = telemetry.finished.load(Relaxed) >= 1;
            break;
        }
    }
    h.state().send_core_command_for_test(CoreCommand::Resume);
    let t = h.state().frame_count_for_test() + 90;
    while h.state().frame_count_for_test() < t {
        poke(&h, 0x06C0, 0x1C);
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
    let (Some(off), Some(on)) = (off, grab(&h)) else {
        skip(readme, name, "no frame");
        return;
    };
    let dirs: Vec<String> = telemetry
        .last_report
        .lock()
        .ok()
        .and_then(|r| r.clone())
        .map(|r| r.passes.iter().map(|p| format!("{:?}", p.dir)).collect())
        .unwrap_or_default();
    println!(
        "SCOUT-MM2-CLIMB: finished {finished}; passes {dirs:?}; {} scout frames, {} cells merged; started {} finished {} aborted {}, passes ended {:?}; agreement {} over {} shared cells; gave up {}; {}",
        telemetry.frames.load(Relaxed),
        telemetry.cells.load(Relaxed),
        telemetry.started.load(Relaxed),
        telemetry.finished.load(Relaxed),
        telemetry.aborted.load(Relaxed),
        telemetry.last_ends.lock().map(|e| e.clone()).unwrap_or_default(),
        agreement_text(&telemetry),
        telemetry.shared_cells.load(Relaxed),
        telemetry.gave_up.load(Relaxed),
        telemetry.last_summary().unwrap_or_default(),
    );
    assert!(
        finished
            && telemetry.aborted.load(Relaxed) == 0
            && dirs.first().is_some_and(|d| d == "Down"),
        "the shaft was not scouted down: finished {finished}, aborted {}, passes {dirs:?}",
        telemetry.aborted.load(Relaxed)
    );
    let d = out_dir();
    save(&d.join("scout-mm2-climb-32x9-off.png"), &off.opaque());
    save(&d.join("scout-mm2-climb-32x9-on.png"), &on.opaque());
    note(
        readme,
        format!("scout-mm2-climb-32x9|{file}|{}x{}", on.w, on.h),
    );
}

/// One session of [`shot_persist`].
struct Visit {
    /// `(live frames since widescreen went on, scene, known margin cells,
    /// margin cells, known cells of the whole canvas)`.
    samples: Vec<(u64, u64, usize, usize, usize)>,
    started: u64,
    finished: u64,
    skipped: u64,
    /// First visit: the live frames run before the scout started, and the
    /// wall time from widescreen-on to the scout finishing.
    wall_to_finish: Option<(u64, Duration)>,
}

impl Visit {
    /// The samples of one scene.
    fn of(&self, scene: u64) -> impl Iterator<Item = &(u64, u64, usize, usize, usize)> {
        self.samples.iter().filter(move |s| s.1 == scene)
    }

    /// The scene with the most remembered cells at any point, and that count.
    fn fullest(&self) -> Option<(u64, usize)> {
        self.samples
            .iter()
            .map(|&(_, scene, .., cells)| (scene, cells))
            .max_by_key(|&(_, cells)| cells)
    }

    /// The first sample of `scene` remembering at least `cells`.
    fn first_at(&self, scene: u64, cells: usize) -> Option<u64> {
        self.of(scene).find(|s| s.4 >= cells).map(|s| s.0)
    }
}

/// Ticket W33-05: a second visit. Two sessions of the same level in ONE cache
/// directory (a fresh TMPDIR, so the first starts cold): the first plays the
/// level with the scout on and measures how long the margins take to fill;
/// the second, a new app over the same cache, must have them on its first
/// frames and start no scout.
fn shot_persist(readme: &mut Vec<String>) {
    let name = "persist";
    if !wanted(name) {
        return;
    }
    type Game = (&'static str, &'static str, &'static str, fn(&mut H));
    let games: [Game; 2] = [
        ("smw", "snes", "Super Mario World (USA).zip", smw_enter_yi1),
        (
            "smb3",
            "nes",
            "Super Mario Bros. 3 (USA).zip",
            smb3_enter_level,
        ),
    ];
    for (tag, console, file, enter) in games {
        if std::env::var("RF_PERSIST_GAME").is_ok_and(|g| g != tag) {
            continue;
        }
        let Some(path) = rom(console, file) else {
            skip(
                readme,
                &format!("persist-{tag}"),
                &format!("{file} not found"),
            );
            continue;
        };
        // `RF_PERSIST_REUSE=<dir>`: skip the first visit and play the second
        // over a cache an earlier run left there (iterating on the second).
        let reuse = std::env::var_os("RF_PERSIST_REUSE").map(PathBuf::from);
        let cold = reuse.clone().unwrap_or_else(|| {
            std::env::temp_dir().join(format!("rf_persist_{tag}_{}", std::process::id()))
        });
        if reuse.is_none() {
            let _ = std::fs::remove_dir_all(&cold);
        }
        std::fs::create_dir_all(&cold).expect("cold cache dir");
        // SAFETY: nothing else in this process reads the environment while the
        // harness is between sessions.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("TMPDIR", &cold);
        }
        println!("PERSIST {tag}: cache dir {}", cold.display());
        let first = (reuse.is_none()).then(|| persist_visit(&path, enter, true));
        let cache_dir = cold.join("retroforge-canvas-cache").join("entries");
        let sizes: Vec<u64> = std::fs::read_dir(&cache_dir)
            .map(|d| {
                d.flatten()
                    .filter_map(|e| e.metadata().ok())
                    .map(|m| m.len())
                    .collect()
            })
            .unwrap_or_default();
        let second = persist_visit(&path, enter, false);
        // The scene the first visit scouted: where it remembered the most.
        let (scene, peak) = first
            .as_ref()
            .and_then(Visit::fullest)
            .or_else(|| second.fullest())
            .unwrap_or((0, 0));
        let target = peak - peak / 100;
        let at = |v: &Visit| {
            v.of(scene)
                .next()
                .map_or(String::from("-"), |&(f, _, k, t, cells)| {
                    format!("f{f}: {cells} cells, margins {k}/{t}")
                })
        };
        let line = format!(
            "PERSIST {tag}: scene {scene:016x}, full = {target} of {peak} remembered cells; \
             first visit: first sample {}, full at live frame {:?}; scout started after {:?} live \
             frames, finished {:?} after widescreen went on (started {} finished {}); \
             second visit: first sample {}, full at live frame {:?}, scouts started {} skipped {}; \
             cache entries {sizes:?} bytes",
            first.as_ref().map_or(String::from("-"), &at),
            first.as_ref().and_then(|v| v.first_at(scene, target)),
            first.as_ref().and_then(|v| v.wall_to_finish).map(|w| w.0),
            first.as_ref().and_then(|v| v.wall_to_finish).map(|w| w.1),
            first.as_ref().map_or(0, |v| v.started),
            first.as_ref().map_or(0, |v| v.finished),
            at(&second),
            second.first_at(scene, target),
            second.started,
            second.skipped,
        );
        println!("{line}");
        note(readme, line);
        assert_eq!(second.started, 0, "{tag}: the second visit started a scout");
        assert!(second.skipped >= 1, "{tag}: the scout was not skipped");
        assert!(
            second.first_at(scene, target).is_some(),
            "{tag}: the second visit never filled the margins"
        );
    }
}

/// One session: open the ROM, enter the level, 32:9 on, sample the margins.
/// `first`: scout on and run until it finishes; otherwise 240 live frames.
fn persist_visit(path: &Path, enter: fn(&mut H), first: bool) -> Visit {
    use std::sync::atomic::Ordering::Relaxed;
    let mut h = new_harness();
    open_and_run(&mut h, path);
    enter(&mut h);
    let telemetry = h.state().scout_telemetry_for_test();
    h.state_mut().set_scout_enabled_for_test(true);
    h.state_mut()
        .set_widescreen_aspect_for_test(Aspect::Wide32x9);
    h.state_mut().set_measure_margins_for_test(true);
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
    let t0 = Instant::now();
    h.state_mut().set_widescreen(true);
    let base = h.state().frame_count_for_test();
    let mut samples = Vec::new();
    let mut wall_to_finish = None;
    let mut sample = |h: &mut H| {
        if let Some((scene, k, t, cells)) = h.state().margin_known_for_test() {
            let at = h.state().frame_count_for_test() - base;
            samples.push((at, scene, k, t, cells));
        }
    };
    if first {
        // As `shot_scout`: unpaced until the worker runs, then the live game
        // is paused (a standing Mario dies ~220 frames in, which would end
        // the scene being scouted) while the scout works.
        while telemetry.started.load(Relaxed) == 0 && t0.elapsed() < Duration::from_secs(120) {
            let t = h.state().frame_count_for_test() + 2;
            run_frames(&mut h, t, Duration::from_secs(10));
            sample(&mut h);
        }
        let live_at_start = h.state().frame_count_for_test() - base;
        h.state().send_core_command_for_test(CoreCommand::Pause);
        while telemetry.finished.load(Relaxed) == 0 && t0.elapsed() < Duration::from_secs(600) {
            std::thread::sleep(Duration::from_millis(100));
        }
        wall_to_finish = Some((live_at_start, t0.elapsed()));
        h.state().send_core_command_for_test(CoreCommand::Resume);
        h.state()
            .send_core_command_for_test(CoreCommand::SetPacingEnabled(true));
        // The merged canvases reach the margins within a few snapshots.
        for _ in 0..90 {
            h.run_steps(1);
            std::thread::sleep(Duration::from_millis(4));
            sample(&mut h);
        }
    } else {
        for _ in 0..240 {
            let t = h.state().frame_count_for_test() + 1;
            run_frames(&mut h, t, Duration::from_secs(10));
            sample(&mut h);
        }
        // And on: no scout may start at any point of the visit.
        let t = h.state().frame_count_for_test() + 600;
        run_frames(&mut h, t, Duration::from_secs(120));
    }
    let visit = Visit {
        samples,
        started: telemetry.started.load(Relaxed),
        finished: telemetry.finished.load(Relaxed),
        skipped: telemetry.skipped.load(Relaxed),
        wall_to_finish,
    };
    // The core thread writes the scene as it stops; give it the time.
    h.state().send_core_command_for_test(CoreCommand::Shutdown);
    std::thread::sleep(Duration::from_secs(3));
    drop(h);
    visit
}

/// Ticket W33-04: a normal run (scout off, 32:9 on) walking right through
/// SMW Yoshi's Island 1 and SMB3 1-1 with the registration check on. The
/// check must not fire on healthy placement: the layers stay at confidence 3
/// and agree with the canvas. Prints the agreement and the per-frame cost.
fn shot_confidence(readme: &mut Vec<String>) {
    let name = "confidence";
    if !wanted(name) {
        return;
    }
    for (tag, console, file, enter, run_keys) in [
        (
            "smw",
            "snes",
            "Super Mario World (USA).zip",
            smw_enter_yi1 as fn(&mut H),
            vec![egui::Key::ArrowRight, egui::Key::A],
        ),
        (
            "smb3",
            "nes",
            "Super Mario Bros. 3 (USA).zip",
            smb3_enter_level as fn(&mut H),
            vec![egui::Key::ArrowRight, egui::Key::Z],
        ),
    ] {
        let Some(path) = rom(console, file) else {
            skip(
                readme,
                &format!("confidence-{tag}"),
                &format!("{file} not found"),
            );
            continue;
        };
        // A cold canvas cache (see `shot_scout_nes`): a warm one would hold
        // the level already and the check would compare against a stranger.
        let cold = std::env::temp_dir().join(format!("rf_confidence_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cold);
        std::fs::create_dir_all(&cold).expect("cold cache dir");
        // SAFETY: the harness is built after this; nothing else reads the
        // environment concurrently at this point.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("TMPDIR", &cold);
        }
        let mut h = new_harness();
        open_and_run(&mut h, &path);
        enter(&mut h);
        h.state_mut().set_scout_enabled_for_test(false);
        h.state_mut()
            .set_widescreen_aspect_for_test(Aspect::Wide32x9);
        h.state_mut().set_widescreen(true);
        let t = h.state().frame_count_for_test() + 20;
        run_frames(&mut h, t, Duration::from_secs(60));
        for &k in &run_keys {
            key_event(&mut h, k, true);
        }
        let mut min_level = [3u8; 4];
        for _ in 0..60 {
            let t = h.state().frame_count_for_test() + 10;
            run_frames(&mut h, t, Duration::from_secs(60));
            if let Some(conf) = h.state().stitch_confidence_for_test() {
                for (n, c) in conf.iter().enumerate().filter(|(_, c)| c.frames > 0) {
                    min_level[n] = min_level[n].min(c.level());
                }
            }
        }
        for &k in &run_keys {
            key_event(&mut h, k, false);
        }
        run_n(&mut h, 6);
        let readout = h.state().margin_readout_for_test().unwrap_or_default();
        println!("CONFIDENCE {tag}: {readout}");
        if let Some(conf) = h.state().stitch_confidence_for_test() {
            for (n, c) in conf.iter().enumerate().filter(|(_, c)| c.frames > 0) {
                println!(
                    "CONFIDENCE {tag} layer {n}: level {} (lowest seen {}), agreed {}/{} = {:.3}% over {} frames this scene, {} stale drops, mean check {:.1} us",
                    c.level(),
                    min_level[n],
                    c.total_agreed,
                    c.total_compared,
                    c.agreement_percent().unwrap_or(100.0),
                    c.frames,
                    c.resets,
                    c.total_nanos as f64 / c.frames.max(1) as f64 / 1e3,
                );
            }
        }
        note(readme, format!("confidence-{tag}|{readout}"));
    }
}

/// Ticket W33-02 acceptance, measured on the shot: for every picture row
/// that shows backdrop, the margins either carry that exact colour (sky) or
/// something else entirely (ground, hills, pipes), but never a NEAR miss
/// (banding: a sky one colour step off the picture's). Also counts pure black
/// pixels in the margins (the pre-W33-02 below-ground fill).
fn backdrop_agreement(readme: &mut Vec<String>, tag: &str, on: &Pic, rows: &[Option<[u8; 3]>]) {
    let pad = on.w.saturating_sub(256) / 2;
    if pad == 0 {
        return;
    }
    let (mut exact, mut near, mut none, mut skipped) = (0, 0, 0, 0);
    let mut black = 0usize;
    let mut sky_cols = 0usize;
    for (y, &row) in rows.iter().enumerate().take(on.h) {
        let Some(c) = row else {
            skipped += 1;
            continue;
        };
        let (mut has_exact, mut has_near) = (false, false);
        let cols = (0..pad).chain(on.w - pad..on.w);
        for x in cols {
            let i = (y * on.w + x) * 4;
            let p = [on.rgba[i], on.rgba[i + 1], on.rgba[i + 2]];
            if p == [0, 0, 0] {
                black += 1;
            }
            if p == c {
                has_exact = true;
                sky_cols += 1;
            } else if p.iter().zip(&c).all(|(a, b)| a.abs_diff(*b) <= 12) {
                has_near = true;
            }
        }
        match (has_exact, has_near) {
            (true, _) => exact += 1,
            (false, true) => near += 1,
            (false, false) => none += 1,
        }
    }
    let line = format!(
        "backdrop-{tag}|rows with backdrop: exact {exact}, near-miss (banding) {near}, no sky in margin {none}, no backdrop in picture {skipped}; black margin px {black}; sky margin px {sky_cols}"
    );
    println!("{line}");
    note(readme, line);
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
    h.state_mut().enable_backdrop_probe_for_test();
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
        let probe_rows = h.state().backdrop_probe_for_test();
        if let (Some(on), Some(rows)) = (on.as_ref(), probe_rows) {
            backdrop_agreement(readme, tagname, on, &rows);
        }
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

/// Ticket W33-00: live frames per second, unpaced, release, back to back on
/// one machine and one session: 4:3 (widescreen off), 32:9 with the scout
/// off, 32:9 with the scout on. `RF_FPS_GAME=smw|smb3|mm2` (default smw).
/// Each figure is the MEDIAN over 20 chunks of 30 frames (600 frames) so a
/// load spike on the shared machine moves one chunk, not the number; the
/// load average is printed with it. The game is not held in one scene (a
/// standing player dies ~220 frames in), so compare the figures to each other
/// only through the repeated 4:3 control.
fn shot_fps(readme: &mut Vec<String>) {
    use std::sync::atomic::Ordering::Relaxed;
    let name = "fps";
    if !wanted(name) {
        return;
    }
    let game = std::env::var("RF_FPS_GAME").unwrap_or_else(|_| "smw".into());
    let (console, file, enter): (&str, &str, fn(&mut H)) = match game.as_str() {
        "smb3" => ("nes", "Super Mario Bros. 3 (USA).zip", smb3_enter_level),
        "mm2" => ("nes", "Mega Man 2 (USA).zip", mm2_enter_level),
        _ => ("snes", "Super Mario World (USA).zip", smw_enter_yi1),
    };
    let Some(path) = rom(console, file) else {
        return skip(readme, name, &format!("{file} not found"));
    };
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    enter(&mut h);
    let load = || {
        std::process::Command::new("uptime")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let measure = |h: &mut H| -> (f64, f64) {
        h.state()
            .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
        let mut fps = Vec::new();
        let t_all = Instant::now();
        let f_all = h.state().frame_count_for_test();
        for _ in 0..20 {
            let f0 = h.state().frame_count_for_test();
            let t0 = Instant::now();
            run_frames(h, f0 + 30, Duration::from_secs(120));
            let n = h.state().frame_count_for_test() - f0;
            fps.push(n as f64 / t0.elapsed().as_secs_f64());
        }
        let total =
            (h.state().frame_count_for_test() - f_all) as f64 / t_all.elapsed().as_secs_f64();
        h.state()
            .send_core_command_for_test(CoreCommand::SetPacingEnabled(true));
        fps.sort_by(|a, b| a.total_cmp(b));
        (fps[fps.len() / 2], total)
    };
    let telemetry = h.state().scout_telemetry_for_test();
    let mut line = format!("FPS {game}:");
    // Scout ON first: it needs the player in the level to start.
    h.state_mut().set_scout_enabled_for_test(true);
    h.state_mut()
        .set_widescreen_aspect_for_test(Aspect::Wide32x9);
    h.state_mut().set_widescreen(true);
    let (med, all) = measure(&mut h);
    line += &format!(
        " [32:9 scout ON] median {med:.1} overall {all:.1} (scouts started {} finished {} aborted {}; {})",
        telemetry.started.load(Relaxed),
        telemetry.finished.load(Relaxed),
        telemetry.aborted.load(Relaxed),
        load()
    );
    h.state_mut().set_scout_enabled_for_test(false);
    let (med, all) = measure(&mut h);
    line += &format!(
        " [32:9 scout off] median {med:.1} overall {all:.1} ({})",
        load()
    );
    h.state_mut().set_widescreen(false);
    let (med, all) = measure(&mut h);
    line += &format!(" [4:3 off] median {med:.1} overall {all:.1} ({})", load());
    h.state_mut().set_widescreen(true);
    let (med, all) = measure(&mut h);
    line += &format!(
        " [32:9 scout off again] median {med:.1} overall {all:.1} ({})",
        load()
    );
    h.state_mut().set_widescreen(false);
    let (med, all) = measure(&mut h);
    line += &format!(
        " [4:3 off again] median {med:.1} overall {all:.1} ({})",
        load()
    );
    note(readme, line);
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
    snapshot(&mut h, "zelda");
    step_from(&mut h);
    let Some(off) = grab_play_area(&mut h) else {
        return skip(readme, name, "no play rect");
    };
    if let Some(why) = h.state().hd2d_unavailable_for_test() {
        return skip(readme, name, &format!("HD-2D look unavailable: {why}"));
    }
    h.state_mut().toggle_hd2d_for_test();
    run_n(&mut h, 30);
    step_from(&mut h);
    step_from(&mut h);
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
    snapshot(&mut h, "lens");
    h.state_mut().edit_lens_for_test(|s| s.lens_clear());
    run_n(&mut h, 10);
    step_from(&mut h);
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
    step_from(&mut h);
    step_from(&mut h);
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
    snapshot(&mut h, "sfx");
    step_from(&mut h);
    let Some(on) = grab(&h) else {
        return skip(readme, name, "no frame");
    };
    h.state_mut()
        .set_superfx_hd_for_test(false, 3, Mode::Enhanced, true);
    run_n(&mut h, 20);
    step_from(&mut h);
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
    snapshot(&mut h, "sfx-gpu");
    let mut shots: Vec<(&str, Pic, Pic)> = Vec::new();
    for (tag, scale, msaa) in [
        ("4x-smooth", 4u8, 4u8),
        ("3x-smooth", 3, 4),
        ("3x-sharp", 3, 1),
    ] {
        h.state_mut()
            .set_superfx_hd_for_test(true, scale, Mode::Enhanced, true);
        h.state_mut().set_superfx_hd_msaa_for_test(msaa);
        step_from(&mut h);
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
    for n in [
        "hdmode7-4x",
        "lens",
        "widescreen-16x9",
        "widescreen-32x9",
        "scout-32x9",
        "scout-smb3-32x9",
        "scout-mm2-32x9",
        "scout-mmx-32x9",
        "scout-mm2-climb-32x9",
    ] {
        let (Some(a), Some(b)) = (
            load(d.join(format!("{n}-off.png"))),
            load(d.join(format!("{n}-on.png"))),
        ) else {
            continue;
        };
        let out = if (a.w, a.h) == (b.w, b.h) && !n.starts_with("scout") {
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

/// Rows stacked top to bottom on the dark background (None when empty).
fn stack_rows(rows: &[Pic]) -> Option<Pic> {
    if rows.is_empty() {
        return None;
    }
    let w = rows.iter().map(|r| r.w).max()?;
    let gap = 10;
    let h: usize = rows.iter().map(|r| r.h).sum::<usize>() + gap * (rows.len() - 1);
    let mut rgba = vec![0u8; w * h * 4];
    for px in rgba.chunks_exact_mut(4) {
        px.copy_from_slice(&[24, 24, 28, 255]);
    }
    let mut y = 0;
    for r in rows {
        for yy in 0..r.h {
            let dst = ((y + yy) * w) * 4;
            rgba[dst..dst + r.w * 4].copy_from_slice(&r.rgba[yy * r.w * 4..][..r.w * 4]);
        }
        y += r.h + gap;
    }
    Some(Pic { w, h, rgba })
}

/// W33-10: the scout contact sheet, three games, each OFF over ON.
fn scout_contact_sheet(readme: &mut Vec<String>) {
    let d = out_dir();
    let load = |p: PathBuf| -> Option<Pic> {
        let img = image::open(p).ok()?.to_rgba8();
        Some(Pic {
            w: img.width() as usize,
            h: img.height() as usize,
            rgba: img.into_raw(),
        })
    };
    let mut rows = Vec::new();
    for n in ["scout-32x9", "scout-smb3-32x9", "scout-mmx-32x9"] {
        let (Some(a), Some(b)) = (
            load(d.join(format!("{n}-off.png"))),
            load(d.join(format!("{n}-on.png"))),
        ) else {
            continue;
        };
        rows.push(a.fit_width(800));
        rows.push(b.fit_width(800));
    }
    if let Some(sheet) = stack_rows(&rows) {
        save(&d.join("scout-contact-sheet.png"), &sheet);
        note(
            readme,
            "scout-contact-sheet|SMW, SMB3, Mega Man X|each game: scout OFF row over scout ON row"
                .to_string(),
        );
    }
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
    let Some(sheet) = stack_rows(&rows) else {
        return;
    };
    save(&d.join("contact-sheet.png"), &sheet);
    note(
        readme,
        "contact-sheet|-|rows in README order; pairs are OFF (left) | ON (right)".to_string(),
    );
}

/// Ticket W34-00: what the live Diorama costs in the whole app, on Super
/// Mario World's first level. `RF_SCREENS_ONLY=w34-baseline`.
///
/// SMW has no painted `[heights]`, so a table is made from what is on screen
/// (the tiles of the lowest rows, minus the tile the top rows are made of),
/// which gives the mesh a realistic count of walls. Three states alternate
/// chunk by chunk, 20 chunks of 30 frames each (600 frames per state), the
/// pace limiter off, so drift in the scene and load on the machine hit all
/// three alike: Diorama off (the control), Diorama on, and the HD-2D preset
/// (Diorama plus the lens). Today the app renders the Diorama at the
/// frame's own size (256x224) and the UI scales the texture, so this is
/// the "1x" figure; the 4x figure is the pass alone, `rf-renderer`'s
/// `w34_00_diorama_cost`.
fn shot_w34_baseline(readme: &mut Vec<String>) {
    use std::collections::HashMap;
    let name = "w34-baseline";
    if !wanted(name) {
        return;
    }
    let Some(path) = rom("snes", "Super Mario World (USA).zip") else {
        return skip(readme, name, "Super Mario World (USA).zip not found");
    };
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    if !h.state().gpu_available_for_test() {
        return skip(readme, name, "no wgpu adapter");
    }
    smw_enter_yi1(&mut h);
    // A stand-in table first: it makes the live Diorama active, which is
    // what arms the SNES snapshot the real table is made from.
    h.state_mut()
        .set_live_heights_for_test(retroforge::live_heights::HeightTable::new(
            1,
            8,
            HashMap::from([(0x0001, 8)]),
        ));
    h.state_mut().set_mode_for_test(Mode::GameAware);
    h.state_mut().set_diorama_for_test(true);
    run_n(&mut h, 30);
    let table = {
        let Some(f) = h.state().snes_debug_frame_for_test() else {
            return skip(readme, name, "no SNES snapshot (capture not armed)");
        };
        let Some(g) = retroforge::live_heights::character_grid(
            1,
            8,
            &f.vram,
            &f.ppu_regs,
            f.bg_scroll,
            (256, 224),
        ) else {
            return skip(readme, name, "BG1 has no plain tilemap here");
        };
        let mut sky: HashMap<u16, u32> = HashMap::new();
        for row in 0..6 {
            for col in 0..g.cols {
                *sky.entry(g.char_at(col, row).unwrap()).or_default() += 1;
            }
        }
        let sky_tile = sky.iter().max_by_key(|(_, n)| **n).map(|(c, _)| *c);
        let mut heights: HashMap<u16, u8> = HashMap::new();
        for row in g.rows.saturating_sub(9)..g.rows {
            for col in 0..g.cols {
                let c = g.char_at(col, row).unwrap();
                if Some(c) != sky_tile {
                    heights.insert(c, 8);
                }
            }
        }
        println!(
            "w34-baseline: sky tile {sky_tile:?}, {} wall tiles in the table",
            heights.len()
        );
        retroforge::live_heights::HeightTable::new(1, 8, heights)
    };
    h.state_mut().set_live_heights_for_test(table);
    h.state_mut().set_mode_for_test(Mode::GameAware);
    h.state_mut()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
    let load = || {
        std::process::Command::new("uptime")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let mut fps: [Vec<f64>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    let mut p95 = [0.0f64; 3];
    let mut opaque = [0usize; 3];
    // W35-15: mean per-frame stage times (ms), summed over chunks per state.
    let mut stages = [[0.0f64; 10]; 3];
    let mut preset_on = false;
    for chunk in 0..60 {
        let state = chunk % 3;
        // Enter the state, then let three frames settle before timing.
        if preset_on && state != 2 {
            h.state_mut().toggle_hd2d_for_test();
            preset_on = false;
        }
        match state {
            0 => h.state_mut().set_diorama_for_test(false),
            1 => h.state_mut().set_diorama_for_test(true),
            _ => {
                h.state_mut().set_diorama_for_test(true);
                if !preset_on {
                    if let Some(why) = h.state().hd2d_unavailable_for_test() {
                        return skip(readme, name, &format!("HD-2D unavailable: {why}"));
                    }
                    h.state_mut().toggle_hd2d_for_test();
                    preset_on = true;
                }
            }
        }
        let f0 = h.state().frame_count_for_test();
        run_frames(&mut h, f0 + 3, Duration::from_secs(60));
        let _ = h.state_mut().diorama_stage_ms_for_test();
        let f0 = h.state().frame_count_for_test();
        let t0 = Instant::now();
        run_frames(&mut h, f0 + 30, Duration::from_secs(120));
        let n = h.state().frame_count_for_test() - f0;
        if state > 0 {
            let ps = h.state().diorama_pass_stage_ms_for_test();
            let ([wide, split, pass, mesh], _) = h.state_mut().diorama_stage_ms_for_test();
            let all = [
                wide, split, pass, mesh, ps[0], ps[1], ps[2], ps[3], ps[4], ps[5],
            ];
            for (a, b) in stages[state].iter_mut().zip(all) {
                *a += b / 20.0;
            }
        }
        fps[state].push(n as f64 / t0.elapsed().as_secs_f64());
        if state > 0 {
            p95[state] = h.state().diorama_p95_ms_for_test();
            opaque[state] = h.state().diorama_opaque_pixels_for_test().unwrap_or(0);
        }
    }
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(true));
    let med = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.total_cmp(b));
        v[v.len() / 2]
    };
    for (st, m) in stages.iter().enumerate().skip(1) {
        println!(
            "W35-15 stages state {st} mean ms: wide {:.3} split {:.3} pass {:.3} (mesh {:.3}); \
             in render: tex {:.3} bind {:.3} vtx {:.3} encode {:.3} submit {:.3} readback {:.3}",
            m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8], m[9]
        );
    }
    let [mut a, mut b, mut c] = fps;
    note(
        readme,
        format!(
            "W34-BASELINE smw yi1 (20 chunks x 30 frames per state, interleaved): \
             median fps off {:.1} | diorama {:.1} | hd2d preset {:.1}; \
             diorama pass p95 (app gate, build+render+readback) {:.2} ms / {:.2} ms; \
             opaque diorama px {} / {}; {}",
            med(&mut a),
            med(&mut b),
            med(&mut c),
            p95[1],
            p95[2],
            opaque[1],
            opaque[2],
            load()
        ),
    );
}

// ------------------------------------------------ W34-04: side-view diorama

/// Press the keys of one `+`-joined token (`R`, `B+R`) for `frames` core
/// frames at real speed. `A` is the A button (key X), `B` the B button (key
/// Z), `Y` the SNES Y (key A).
fn pad_keys(buttons: &str) -> Vec<egui::Key> {
    use egui::Key as K;
    buttons
        .split('+')
        .filter_map(|b| match b {
            "R" => Some(K::ArrowRight),
            "L" => Some(K::ArrowLeft),
            "U" => Some(K::ArrowUp),
            "D" => Some(K::ArrowDown),
            "A" => Some(K::X),
            "B" => Some(K::Z),
            "Y" => Some(K::A),
            "S" => Some(K::Enter),
            _ => None,
        })
        .collect()
}

/// A play script in the W34-03 oracle's notation (`R90 B+R30 w20`): buttons
/// then frames, `wN` idles. Real time.
fn play_script(h: &mut H, script: &str) {
    for tok in script.split_whitespace() {
        if let Some(n) = tok.strip_prefix('w') {
            let t = h.state().frame_count_for_test() + n.parse::<u64>().expect("wN");
            run_frames(h, t, Duration::from_secs(60));
            continue;
        }
        let at = tok.find(|c: char| c.is_ascii_digit()).expect("buttons+N");
        let keys = pad_keys(&tok[..at]);
        hold_for(h, &keys, tok[at..].parse().expect("N"));
    }
}

/// Test-only help, never shipped: poke `value` into the console's work RAM
/// at `addr` (a CPU-bus address) from the harness, the way a cheat device
/// would. Used to keep the player alive; the learner and the renderer never
/// see it any other way than as more play.
fn poke(h: &H, addr: u16, value: u8) {
    h.state()
        .send_core_command_for_test(CoreCommand::PokeBus { addr, value });
}

/// Play for `frames` core frames with `lap` repeated, re-poking `pokes`
/// every step. `enter` runs at the start of each lap and puts the player
/// (back) in the level: a death sends the game back to its map.
fn play_for(h: &mut H, frames: u64, lap: &str, pokes: &[(u16, u8)], enter: fn(&mut H)) {
    let end = h.state().frame_count_for_test() + frames;
    while h.state().frame_count_for_test() < end {
        enter(h);
        for tok in lap.split_whitespace() {
            if h.state().frame_count_for_test() >= end {
                break;
            }
            for &(a, v) in pokes {
                poke(h, a, v);
            }
            play_script(h, tok);
        }
    }
}

/// SMW: a death or a finished level shows the map; A enters the level.
fn enter_smw(h: &mut H) {
    tap(h, egui::Key::X, 6);
}

/// SMB3 1-1's sky is (178, 240, 249); the map's top row is not.
fn smb3_in_level(h: &H) -> bool {
    grab(h).is_some_and(|p| {
        let at = (12 * p.w + 128) * 4;
        p.rgba[at..at + 3] == [178, 240, 249]
    })
}

/// SMB3: after a death Mario stands at START on the map; right, up and A
/// walk to world 1-1 and enter it.
fn enter_smb3(h: &mut H) {
    use egui::Key as K;
    if smb3_in_level(h) {
        return;
    }
    tap(h, K::X, 6);
    wait(h, "smb3", 150);
    if smb3_in_level(h) {
        return;
    }
    tap(h, K::ArrowRight, 8);
    wait(h, "smb3", 60);
    tap(h, K::ArrowUp, 8);
    wait(h, "smb3", 60);
    tap(h, K::X, 8);
    wait(h, "smb3", 150);
}

/// One off/on pair of the side-view diorama, with what the learner knew.
fn diorama_pair(h: &mut H, name: &str, what: &str, readme: &mut Vec<String>) {
    diorama_pair_as(h, "diorama-side", name, what, readme);
}

/// [`diorama_pair`] with the file prefix chosen (`diorama-top` for top-down).
fn diorama_pair_as(h: &mut H, prefix: &str, name: &str, what: &str, readme: &mut Vec<String>) {
    // A resized window is for the cost rows: its pictures stay out of the docs.
    let d = if std::env::var("RF_SCREENS_WINDOW").is_ok()
        && std::env::var("RF_SCREENS_KEEP").is_err()
    {
        probe_dir()
    } else {
        w34_dir()
    };
    // OFF: the flat view of a stepped frame.
    h.state_mut().set_diorama_for_test(false);
    snapshot(h, name);
    step_from(h);
    let Some(off) = grab_play_area(h) else {
        return skip(readme, name, "no play rect");
    };
    // ON: the next frames, diorama ticked again (the learner kept its table).
    h.state_mut().set_diorama_for_test(true);
    run_n(h, 30);
    step_from(h);
    step_from(h);
    let on = grab_play_area(h).unwrap_or_else(|| off.clone());
    let numbers = h.state().heights_numbers_for_test();
    let readout = h.state().heights_readout_for_test();
    println!("{name}: numbers {numbers:?}\n{name}: readout {readout:?}");
    save(&d.join(format!("{prefix}-{name}-off.png")), &off.opaque());
    save(&d.join(format!("{prefix}-{name}-on.png")), &on.opaque());
    note(
        readme,
        format!(
            "{prefix}-{name}|{what}|OFF flat / ON diorama; learner {numbers:?}; readout {readout:?}"
        ),
    );
}

/// The cost of the side-view diorama in the whole app: median frames per
/// second with Diorama off and on, the pace limiter off, alternating 30-frame
/// chunks (10 per state) so drift and machine load hit both alike, plus the
/// pass's own p95 (build + render + readback) from the app's budget gate.
fn diorama_fps(h: &mut H, name: &str, readme: &mut Vec<String>) {
    h.state_mut().resume_for_test();
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
    run_n(h, 10);
    let mut fps: [Vec<f64>; 2] = [Vec::new(), Vec::new()];
    for chunk in 0..20 {
        let on = chunk % 2 == 1;
        h.state_mut().set_diorama_for_test(on);
        let f0 = h.state().frame_count_for_test();
        run_frames(h, f0 + 3, Duration::from_secs(60));
        let f0 = h.state().frame_count_for_test();
        let t0 = Instant::now();
        run_frames(h, f0 + 30, Duration::from_secs(120));
        let n = h.state().frame_count_for_test() - f0;
        fps[usize::from(on)].push(n as f64 / t0.elapsed().as_secs_f64());
    }
    let p95 = h.state().diorama_p95_ms_for_test();
    let size = h.state().diorama_size_for_test();
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(true));
    let med = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.total_cmp(b));
        v[v.len() / 2]
    };
    let [mut off, mut on] = fps;
    note(
        readme,
        format!(
            "DIORAMA-SIDE-FPS {name} (10 chunks x 30 frames per state, interleaved, pacing off): \
             median fps off {:.1} | diorama on {:.1}; diorama pass p95 {:.2} ms; diorama image {size:?}",
            med(&mut off),
            med(&mut on),
            p95
        ),
    );
}

/// W34-04 acceptance, shot 1: the in-repo RF-Scroller fixture, a vertical
/// level with blocks (D8). `RF_SCREENS_ONLY=diorama-side-rfscroller`.
fn shot_diorama_side_rfscroller(readme: &mut Vec<String>) {
    let name = "rfscroller";
    if !wanted("diorama-side-rfscroller") {
        return;
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/nes/rf-scroller/build/rf-scroller.nes");
    if !path.exists() {
        return skip(readme, name, "RF-Scroller fixture not built");
    }
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    if !h.state().gpu_available_for_test() {
        return skip(readme, name, "no wgpu adapter");
    }
    wait(&mut h, "scroller", 300);
    h.state_mut().set_mode_for_test(Mode::Enhanced);
    h.state_mut().set_diorama_for_test(true);
    run_n(&mut h, 30);
    play_script(&mut h, "R90 w10 R60 w20");
    release_all(&mut h);
    diorama_pair(
        &mut h,
        name,
        "RF-Scroller (in-repo NES test game; decoded level + collision, profile says side-scroller)",
        readme,
    );
}

/// W34-04 acceptance, shot 2: Super Mario World, Yoshi's Island 1 after 30 s
/// of play. `RF_SCREENS_ONLY=diorama-side-smw`.
fn shot_diorama_side_smw(readme: &mut Vec<String>) {
    let name = "smw";
    if !wanted("diorama-side-smw") {
        return;
    }
    let Some(path) = rom("snes", "Super Mario World (USA).zip") else {
        return skip(readme, name, "Super Mario World (USA).zip not found");
    };
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    if !h.state().gpu_available_for_test() {
        return skip(readme, name, "no wgpu adapter");
    }
    smw_enter_yi1(&mut h);
    h.state_mut().set_mode_for_test(Mode::Enhanced);
    h.state_mut().set_diorama_for_test(true);
    run_n(&mut h, 30);
    // $1490 is the star-power timer: invincible, kills what it touches. A
    // test-only poke so the 30 s are spent in the level (documented in the
    // README row; never shipped).
    let frames: u64 = std::env::var("RF_DIORAMA_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1800);
    play_for(
        &mut h,
        frames,
        "w4 R90 B+R30 R60 A+R24 R50 L30 R+B40 w20",
        &[(0x1490, 0xFF)],
        enter_smw,
    );
    // End in the level, alive: the last lap may have ended on the map or in
    // a fade, so re-enter and walk a little (as the SMB3 shot does).
    enter_smw(&mut h);
    wait(&mut h, "smw", 150);
    play_script(&mut h, "R40 B+R20");
    release_all(&mut h);
    probe(&h, "smw-diorama-after-play");
    diorama_pair(
        &mut h,
        name,
        "Super Mario World, Yoshi's Island 1, after 30 s of scripted play (star-power poke keeps Mario alive)",
        readme,
    );
    diorama_fps(&mut h, name, readme);
}

/// W34-04 acceptance, shot 3: Super Mario Bros. 3, world 1-1 after 30 s of
/// play. `RF_SCREENS_ONLY=diorama-side-smb3`.
fn shot_diorama_side_smb3(readme: &mut Vec<String>) {
    let name = "smb3";
    if !wanted("diorama-side-smb3") {
        return;
    }
    let Some(path) = rom("nes", "Super Mario Bros. 3 (USA).zip") else {
        return skip(readme, name, "Super Mario Bros. 3 (USA).zip not found");
    };
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    if !h.state().gpu_available_for_test() {
        return skip(readme, name, "no wgpu adapter");
    }
    smb3_enter_level(&mut h);
    h.state_mut().set_mode_for_test(Mode::Enhanced);
    h.state_mut().set_diorama_for_test(true);
    run_n(&mut h, 30);
    let frames: u64 = std::env::var("RF_DIORAMA_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1800);
    play_for(
        &mut h,
        frames,
        "w4 R90 B+R30 R60 A+R24 R50 L30 R+B40 w20",
        &[],
        enter_smb3,
    );
    // End in the level, alive: re-enter if the last lap ended in a death and
    // walk a little so the picture is a moment of play, not a fade.
    enter_smb3(&mut h);
    play_script(&mut h, "R60 B+R20");
    release_all(&mut h);
    probe(&h, "smb3-diorama-after-play");
    diorama_pair(
        &mut h,
        name,
        "Super Mario Bros. 3, world 1-1, after 30 s of scripted play (laps restart the level after a death)",
        readme,
    );
    diorama_fps(&mut h, name, readme);
}

/// Boot A Link to the Past, register a name (Start ends the name entry) and
/// click through the opening text until Link is in control (W34-02's script).
fn alttp_take_control(h: &mut H) {
    wait(h, "alttp", 600);
    for i in 0..30 {
        tap(
            h,
            if i % 2 == 0 {
                egui::Key::Enter
            } else {
                egui::Key::X
            },
            6,
        );
        wait(h, "alttp", 60);
    }
    for _ in 0..24 {
        tap(h, egui::Key::X, 4);
        wait(h, "alttp", 50);
    }
}

/// W34-05 acceptance: A Link to the Past, the top-down camera on the player.
/// `RF_SCREENS_ONLY=diorama-top-alttp`.
fn shot_diorama_top_alttp(readme: &mut Vec<String>) {
    let name = "alttp";
    if !wanted("diorama-top-alttp") {
        return;
    }
    let Some(path) = rom(
        "snes",
        "Legend of Zelda, The - A Link to the Past (USA).zip",
    ) else {
        return skip(readme, name, "A Link to the Past (USA).zip not found");
    };
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    if !h.state().gpu_available_for_test() {
        return skip(readme, name, "no wgpu adapter");
    }
    alttp_take_control(&mut h);
    h.state_mut().set_mode_for_test(Mode::Enhanced);
    h.state_mut().set_diorama_for_test(true);
    run_n(&mut h, 30);
    let laps: u64 = std::env::var("RF_DIORAMA_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1800);
    let end = h.state().frame_count_for_test() + laps;
    while h.state().frame_count_for_test() < end {
        play_script(&mut h, "w4 D30 w15 R40 w15 U20 w15 L40 w15 D20 w20");
    }
    release_all(&mut h);
    probe(&h, "alttp-diorama-after-play");
    diorama_pair_as(
        &mut h,
        "diorama-top",
        name,
        "A Link to the Past, Link's house and yard, top-down; the camera follows Link",
        readme,
    );
    diorama_fps(&mut h, name, readme);
}

/// Set the lens's time of day (percent of a day) the way its slider does, with
/// the wall clock out of it, and let the next frames draw with it.
fn set_time_of_day(h: &mut H, pct: u8) {
    h.state_mut().edit_lens_for_test(|s| {
        s.lens_follow_clock = false;
        s.lens_time_of_day = pct;
    });
    run_n(h, 20);
    step_from(h);
    step_from(h);
}

/// W34-06 acceptance: A Link to the Past at time of day 0.3 / 0.5 / 0.8, the
/// sun and its shadows moving. `RF_SCREENS_ONLY=diorama-light-alttp`.
fn shot_diorama_light_alttp(readme: &mut Vec<String>) {
    let name = "alttp";
    if !wanted("diorama-light-alttp") {
        return;
    }
    let Some(path) = rom(
        "snes",
        "Legend of Zelda, The - A Link to the Past (USA).zip",
    ) else {
        return skip(readme, name, "A Link to the Past (USA).zip not found");
    };
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    if !h.state().gpu_available_for_test() {
        return skip(readme, name, "no wgpu adapter");
    }
    alttp_take_control(&mut h);
    h.state_mut().set_mode_for_test(Mode::Enhanced);
    h.state_mut().set_diorama_for_test(true);
    run_n(&mut h, 30);
    let laps: u64 = std::env::var("RF_DIORAMA_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1800);
    let end = h.state().frame_count_for_test() + laps;
    while h.state().frame_count_for_test() < end {
        play_script(&mut h, "w4 D30 w15 R40 w15 U20 w15 L40 w15 D20 w20");
    }
    release_all(&mut h);
    let d = w34_dir();
    for pct in [30u8, 50, 80] {
        set_time_of_day(&mut h, pct);
        let Some(shot) = grab_play_area(&mut h) else {
            return skip(readme, name, "no play rect");
        };
        println!(
            "light t={pct}: {:?} px, diorama px {:?}",
            h.state().diorama_size_for_test(),
            h.state().diorama_opaque_pixels_for_test()
        );
        save(
            &d.join(format!("diorama-light-alttp-t{pct}.png")),
            &shot.opaque(),
        );
    }
    note(
        readme,
        "diorama-light-alttp|A Link to the Past, Link's house and yard, top-down; sun at time of day 0.3 / 0.5 / 0.8".to_string(),
    );
}

/// W34-06 acceptance: Super Mario World side view with the sun's shadows.
/// `RF_SCREENS_ONLY=diorama-light-smw`.
fn shot_diorama_light_smw(readme: &mut Vec<String>) {
    let name = "smw";
    if !wanted("diorama-light-smw") {
        return;
    }
    let Some(path) = rom("snes", "Super Mario World (USA).zip") else {
        return skip(readme, name, "Super Mario World (USA).zip not found");
    };
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    if !h.state().gpu_available_for_test() {
        return skip(readme, name, "no wgpu adapter");
    }
    smw_enter_yi1(&mut h);
    h.state_mut().set_mode_for_test(Mode::Enhanced);
    h.state_mut().set_diorama_for_test(true);
    run_n(&mut h, 30);
    let frames: u64 = std::env::var("RF_DIORAMA_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1800);
    play_for(
        &mut h,
        frames,
        "w4 R90 B+R30 R60 A+R24 R50 L30 R+B40 w20",
        &[(0x1490, 0xFF)],
        enter_smw,
    );
    enter_smw(&mut h);
    wait(&mut h, "smw", 150);
    play_script(&mut h, "R40 B+R20");
    release_all(&mut h);
    let d = w34_dir();
    for (pct, file) in [
        (30u8, "diorama-light-smw-on.png"),
        (50, "diorama-light-smw-t50.png"),
    ] {
        set_time_of_day(&mut h, pct);
        let Some(shot) = grab_play_area(&mut h) else {
            return skip(readme, name, "no play rect");
        };
        save(&d.join(file), &shot.opaque());
    }
    note(
        readme,
        "diorama-light-smw|Super Mario World, Yoshi's Island 1; side view, sun at time of day 0.3 (on) and 0.5 (t50)".to_string(),
    );
}

/// W34-06b acceptance: the HD-2D preset in full (lit 3D, shadow map, bloom,
/// tilt-shift, and the grade ticked on top) against the flat picture, on A
/// Link to the Past and Super Mario World. `RF_SCREENS_ONLY=hd2d-full`.
fn shot_hd2d_full(readme: &mut Vec<String>) {
    if !wanted("hd2d-full") {
        return;
    }
    for game in ["alttp", "smw"] {
        let mut h = new_harness();
        if game == "alttp" {
            let Some(path) = rom(
                "snes",
                "Legend of Zelda, The - A Link to the Past (USA).zip",
            ) else {
                skip(readme, game, "A Link to the Past (USA).zip not found");
                continue;
            };
            open_and_run(&mut h, &path);
            alttp_take_control(&mut h);
        } else {
            let Some(path) = rom("snes", "Super Mario World (USA).zip") else {
                skip(readme, game, "Super Mario World (USA).zip not found");
                continue;
            };
            open_and_run(&mut h, &path);
            smw_enter_yi1(&mut h);
        }
        if !h.state().gpu_available_for_test() {
            skip(readme, game, "no wgpu adapter");
            continue;
        }
        h.state_mut().set_mode_for_test(Mode::Enhanced);
        h.state_mut().set_diorama_for_test(true);
        run_n(&mut h, 30);
        let laps: u64 = std::env::var("RF_DIORAMA_FRAMES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1800);
        if game == "alttp" {
            let end = h.state().frame_count_for_test() + laps;
            while h.state().frame_count_for_test() < end {
                play_script(&mut h, "w4 D30 w15 R40 w15 U20 w15 L40 w15 D20 w20");
            }
        } else {
            play_for(
                &mut h,
                laps,
                "w4 R90 B+R30 R60 A+R24 R50 L30 R+B40 w20",
                &[(0x1490, 0xFF)],
                enter_smw,
            );
            enter_smw(&mut h);
            wait(&mut h, "smw", 150);
            play_script(&mut h, "R40 B+R20");
        }
        release_all(&mut h);
        // OFF: the flat picture, nothing from the preset.
        h.state_mut().set_diorama_for_test(false);
        h.state_mut().edit_lens_for_test(|s| s.lens_clear());
        snapshot(&mut h, game);
        step_from(&mut h);
        let Some(off) = grab_play_area(&mut h) else {
            skip(readme, game, "no play rect");
            continue;
        };
        // ON: the preset, with the grade ticked and a morning sun.
        if let Some(why) = h.state().hd2d_unavailable_for_test() {
            skip(readme, game, &format!("preset unavailable: {why}"));
            continue;
        }
        h.state_mut().toggle_hd2d_for_test();
        h.state_mut().edit_lens_for_test(|s| {
            s.lens_grade = true;
            s.lens_grade_pct = 100;
            s.lens_follow_clock = false;
            s.lens_time_of_day = 30;
        });
        run_n(&mut h, 40);
        step_from(&mut h);
        step_from(&mut h);
        let on = grab_play_area(&mut h).unwrap_or_else(|| off.clone());
        // Lens off over the same 3D frame, for the difference.
        h.state_mut().edit_lens_for_test(|s| {
            s.lens_bloom = false;
            s.lens_tiltshift = false;
            s.lens_grade = false;
        });
        run_n(&mut h, 20);
        step_from(&mut h);
        step_from(&mut h);
        let no_lens = grab_play_area(&mut h).unwrap_or_else(|| off.clone());
        let d = w34_dir();
        save(&d.join(format!("hd2d-full-{game}-off.png")), &off.opaque());
        save(&d.join(format!("hd2d-full-{game}-on.png")), &on.opaque());
        save(
            &d.join(format!("hd2d-full-{game}-3d-no-lens.png")),
            &no_lens.opaque(),
        );
        // Cost: unpaced, the lens on again, p95 from the app's own gates.
        h.state_mut().edit_lens_for_test(|s| {
            s.lens_bloom = true;
            s.lens_tiltshift = true;
            s.lens_grade = true;
        });
        h.state_mut().resume_for_test();
        h.state()
            .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
        run_n(&mut h, 150);
        h.state()
            .send_core_command_for_test(CoreCommand::SetPacingEnabled(true));
        note(
            readme,
            format!(
                "hd2d-full-{game}|OFF flat / ON preset + grade + 3D lens; diorama p95 {:.2} ms, lens p95 {:.2} ms, image {:?}",
                h.state().diorama_p95_ms_for_test(),
                h.state().lens_p95_ms_for_test(),
                h.state().diorama_size_for_test()
            ),
        );
    }
}

/// The flat picture, then the HD-2D preset (grade on by default) at morning,
/// noon and dusk, for a game already learned and in play. Files
/// `{game}-flat.png` and `{game}-hd2d-{morning,noon,dusk}.png` in
/// `docs/screens/w34/`.
fn tour_hd2d_set(
    h: &mut H,
    game: &str,
    cam: Option<(u8, i8, u16, bool)>,
    readme: &mut Vec<String>,
) {
    // Let the follow camera settle on the player after a fresh start.
    run_n(h, 90);
    h.state_mut().set_diorama_for_test(false);
    h.state_mut().edit_lens_for_test(|s| s.lens_clear());
    snapshot(h, game);
    step_from(h);
    let Some(off) = grab_play_area(h) else {
        return skip(readme, game, "no play rect");
    };
    if let Some(why) = h.state().hd2d_unavailable_for_test() {
        return skip(readme, game, &format!("preset unavailable: {why}"));
    }
    // The preset is a toggle: press again if the first press turned it off.
    let mut grade = false;
    for _ in 0..2 {
        h.state_mut().toggle_hd2d_for_test();
        h.state_mut().edit_lens_for_test(|s| grade = s.lens_grade);
        if grade {
            break;
        }
    }
    if let Some((pitch, yaw, zoom, follow)) = cam {
        h.state_mut()
            .set_diorama_camera_for_test(pitch, yaw, zoom, follow);
    }
    let d = w34_dir();
    save(&d.join(format!("{game}-flat.png")), &off.opaque());
    for (pct, tag) in [(30u8, "morning"), (50, "noon"), (80, "dusk")] {
        set_time_of_day(h, pct);
        run_n(h, 20);
        step_from(h);
        step_from(h);
        let on = grab_play_area(h).unwrap_or_else(|| off.clone());
        save(&d.join(format!("{game}-hd2d-{tag}.png")), &on.opaque());
    }
    note(
        readme,
        format!(
            "w34-tour|{game}: flat vs HD-2D preset at 0.3/0.5/0.8; grade on by default: {grade}; learner {:?}",
            h.state().heights_numbers_for_test()
        ),
    );
}

/// W34-10: the screenshot tour. `RF_SCREENS_ONLY=w34-tour-alttp` (or
/// `-smb3`, `-wide` which also takes the SMW 4:3 set, `-sheet`).
fn shot_w34_tour(readme: &mut Vec<String>) {
    let frames: u64 = std::env::var("RF_DIORAMA_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1800);
    if wanted("w34-tour-alttp") {
        if let Some(path) = rom(
            "snes",
            "Legend of Zelda, The - A Link to the Past (USA).zip",
        ) {
            let mut h = new_harness();
            open_and_run(&mut h, &path);
            if h.state().gpu_available_for_test() {
                alttp_take_control(&mut h);
                h.state_mut().set_mode_for_test(Mode::Enhanced);
                h.state_mut().set_diorama_for_test(true);
                run_n(&mut h, 30);
                let end = h.state().frame_count_for_test() + frames;
                while h.state().frame_count_for_test() < end {
                    play_script(&mut h, "w4 D30 w15 R40 w15 U20 w15 L40 w15 D20 w20");
                }
                release_all(&mut h);
                tour_hd2d_set(&mut h, "alttp", None, readme);
            }
        }
    }
    if wanted("w34-tour-smb3") {
        if let Some(path) = rom("nes", "Super Mario Bros. 3 (USA).zip") {
            let mut h = new_harness();
            open_and_run(&mut h, &path);
            if h.state().gpu_available_for_test() {
                smb3_enter_level(&mut h);
                h.state_mut().set_mode_for_test(Mode::Enhanced);
                h.state_mut().set_diorama_for_test(true);
                run_n(&mut h, 30);
                play_for(
                    &mut h,
                    frames,
                    "w4 R90 B+R30 R60 A+R24 R50 L30 R+B40 w20",
                    &[],
                    enter_smb3,
                );
                // Let the last lap end (a death shows the map), then start the
                // level afresh so the picture has Mario alive in it.
                release_all(&mut h);
                for _ in 0..8 {
                    wait(&mut h, "smb3", 120);
                    if !smb3_in_level(&h) {
                        break;
                    }
                }
                enter_smb3(&mut h);
                wait(&mut h, "smb3", 60);
                play_script(&mut h, "R30");
                release_all(&mut h);
                tour_hd2d_set(&mut h, "smb3", None, readme);
            }
        }
    }
    if wanted("w34-tour-wide") {
        // SAFETY: set before any harness of this shot is built.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("RF_SCREENS_WINDOW", "1900x800");
        }
        use std::sync::atomic::Ordering::Relaxed;
        let cold = std::env::temp_dir().join(format!("rf_tour_wide_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cold);
        std::fs::create_dir_all(&cold).expect("cold cache dir");
        // SAFETY: nothing else reads the environment between sessions.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("TMPDIR", &cold);
        }
        let Some(path) = rom("snes", "Super Mario World (USA).zip") else {
            return skip(readme, "smw", "Super Mario World (USA).zip not found");
        };
        {
            let Some(mut h) = smw_yi1_harness(&path) else {
                return skip(readme, "smw", "never reached Yoshi's Island 1");
            };
            h.state_mut().set_mode_for_test(Mode::Enhanced);
            h.state_mut().set_diorama_for_test(true);
            run_n(&mut h, 30);
            play_for(
                &mut h,
                frames,
                "w4 R90 B+R30 R60 A+R24 R50 L30 R+B40 w20",
                &[(0x1490, 0xFF), (0x1497, 0xFF)],
                enter_smw,
            );
            release_all(&mut h);
            h.state().send_core_command_for_test(CoreCommand::Shutdown);
            std::thread::sleep(Duration::from_secs(3));
        }
        let Some(mut h) = smw_yi1_harness(&path) else {
            return skip(readme, "smw", "never reached Yoshi's Island 1 again");
        };
        h.state_mut().set_mode_for_test(Mode::Enhanced);
        h.state_mut().set_diorama_for_test(true);
        run_n(&mut h, 30);
        // The tiles learned in session 1 come back from the cache. 32:9 with
        // the scout first, then the 4:3 set.
        h.state_mut()
            .set_widescreen_aspect_for_test(Aspect::Wide32x9);
        h.state_mut().set_diorama_for_test(true);
        run_n(&mut h, 30);
        let telemetry = h.state().scout_telemetry_for_test();
        h.state_mut().set_scout_enabled_for_test(true);
        h.state()
            .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
        let t1 = Instant::now();
        h.state_mut().set_widescreen(true);
        while telemetry.started.load(Relaxed) == 0 && t1.elapsed() < Duration::from_secs(60) {
            let t = h.state().frame_count_for_test() + 2;
            run_frames(&mut h, t, Duration::from_secs(10));
        }
        h.state().send_core_command_for_test(CoreCommand::Pause);
        while t1.elapsed() < Duration::from_secs(300) {
            std::thread::sleep(Duration::from_millis(100));
            run_n(&mut h, 1);
            if telemetry.finished.load(Relaxed) + telemetry.gave_up.load(Relaxed) >= 1 {
                break;
            }
        }
        run_n(&mut h, 120);
        tour_hd2d_set(&mut h, "smw-32x9", None, readme);
        // Back to 4:3 at the start of the level, Mario alive and standing; the
        // follow camera is left off for this set (the whole window in view).
        h.state_mut().set_widescreen(false);
        h.state()
            .send_core_command_for_test(CoreCommand::SetPacingEnabled(true));
        h.state().send_core_command_for_test(CoreCommand::Resume);
        run_n(&mut h, 20);
        tour_hd2d_set(&mut h, "smw", Some((40, 0, 100, false)), readme);
        h.state().send_core_command_for_test(CoreCommand::Shutdown);
        std::thread::sleep(Duration::from_secs(3));
    }
    if wanted("w34-tour-sheet") {
        tour_sheet(readme);
    }
}

/// W35-01: the Diorama's follow camera at a fresh level start, a steep yaw,
/// and ALttP. `RF_SCREENS_ONLY=w35-smw` or `w35-alttp`. Files in
/// `docs/screens/w34/` named `w35-*`.
fn shot_w35_diorama(readme: &mut Vec<String>) {
    let d = w34_dir();
    let frames: u64 = std::env::var("RF_DIORAMA_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(900);
    // The HD-2D preset's camera: 40 degrees, zoom 160 %, following.
    let preset = |h: &mut H| h.state_mut().set_diorama_camera_for_test(40, 0, 160, true);
    // A stepped frame leaves the core paused: resume it for what follows.
    let shoot = |h: &mut H, name: &str| {
        step_from(h);
        if let Some(p) = grab_play_area(h) {
            save(&d.join(name), &p.opaque());
        }
        h.state().send_core_command_for_test(CoreCommand::Resume);
        run_n(h, 2);
    };
    if wanted("w35-smw") {
        // SAFETY: set before any harness of this shot is built.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("RF_SCREENS_WINDOW", "1900x800");
        }
        let cold = std::env::temp_dir().join(format!("rf_w35_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cold);
        std::fs::create_dir_all(&cold).expect("cold cache dir");
        // SAFETY: nothing else reads the environment between sessions.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("TMPDIR", &cold);
        }
        let Some(path) = rom("snes", "Super Mario World (USA).zip") else {
            return skip(readme, "w35-smw", "Super Mario World (USA).zip not found");
        };
        {
            let Some(mut h) = smw_yi1_harness(&path) else {
                return skip(readme, "w35-smw", "never reached Yoshi's Island 1");
            };
            h.state_mut().set_mode_for_test(Mode::Enhanced);
            h.state_mut().set_diorama_for_test(true);
            preset(&mut h);
            // The first frames of the level, cold cache: follow on, the
            // player not named yet.
            let first: usize = std::env::var("RF_W35_FRAMES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(6);
            run_n(&mut h, first);
            shoot(&mut h, "w35-smw-follow-start-4x3.png");
            // Steep yaw and pitch, no follow, so the plane's edges are in view.
            h.state_mut()
                .set_diorama_camera_for_test(75, 40, 100, false);
            run_n(&mut h, 10);
            shoot(&mut h, "w35-smw-steep-yaw-4x3.png");
            preset(&mut h);
            h.state_mut().set_diorama_for_test(true);
            run_n(&mut h, 30);
            // Play: the detector names Mario, the camera closes in on him.
            let lap = "w4 R90 B+R30 R60 A+R24 R50 L30 R+B40 w20";
            let pokes = [(0x1490, 0xFF), (0x1497, 0xFF)];
            for _ in 0..4 {
                play_for(&mut h, 20, "R20", &pokes, enter_smw);
                println!("w35 follow {:?}", h.state().diorama_follow_for_test());
            }
            shoot(&mut h, "w35-smw-follow-playing-4x3.png");
            println!("w35 follow {:?}", h.state().diorama_follow_for_test());
            play_for(&mut h, frames, lap, &pokes, enter_smw);
            release_all(&mut h);
            println!("w35 smw learned {:?}", h.state().heights_numbers_for_test());
            h.state().send_core_command_for_test(CoreCommand::Shutdown);
            std::thread::sleep(Duration::from_secs(3));
        }
        // Session 2: 32:9 with the level learned, the scout's canvas, a fresh
        // start of the level (as the tour does), follow on.
        let Some(mut h) = smw_yi1_harness(&path) else {
            return skip(readme, "w35-smw", "never reached Yoshi's Island 1 again");
        };
        use std::sync::atomic::Ordering::Relaxed;
        h.state_mut().set_mode_for_test(Mode::Enhanced);
        h.state_mut()
            .set_widescreen_aspect_for_test(Aspect::Wide32x9);
        h.state_mut().set_diorama_for_test(true);
        run_n(&mut h, 30);
        let telemetry = h.state().scout_telemetry_for_test();
        h.state_mut().set_scout_enabled_for_test(true);
        h.state()
            .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
        let t1 = Instant::now();
        h.state_mut().set_widescreen(true);
        while telemetry.started.load(Relaxed) == 0 && t1.elapsed() < Duration::from_secs(60) {
            let t = h.state().frame_count_for_test() + 2;
            run_frames(&mut h, t, Duration::from_secs(10));
        }
        h.state().send_core_command_for_test(CoreCommand::Pause);
        while t1.elapsed() < Duration::from_secs(300) {
            std::thread::sleep(Duration::from_millis(100));
            run_n(&mut h, 1);
            if telemetry.finished.load(Relaxed) + telemetry.gave_up.load(Relaxed) >= 1 {
                break;
            }
        }
        run_n(&mut h, 120);
        h.state()
            .send_core_command_for_test(CoreCommand::SetPacingEnabled(true));
        h.state().send_core_command_for_test(CoreCommand::Resume);
        preset(&mut h);
        run_n(&mut h, 10);
        shoot(&mut h, "w35-smw-follow-start-32x9.png");
        h.state_mut()
            .set_diorama_camera_for_test(75, 40, 100, false);
        run_n(&mut h, 10);
        shoot(&mut h, "w35-smw-steep-yaw-32x9.png");
        note(
            readme,
            format!(
                "w35-smw|follow at start 4:3 / playing / 32:9, steep yaw; {:?}",
                h.state().heights_numbers_for_test()
            ),
        );
        h.state().send_core_command_for_test(CoreCommand::Shutdown);
        std::thread::sleep(Duration::from_secs(3));
    }
    if wanted("w35-alttp") {
        let Some(path) = rom(
            "snes",
            "Legend of Zelda, The - A Link to the Past (USA).zip",
        ) else {
            return skip(readme, "w35-alttp", "ALttP not found");
        };
        let mut h = new_harness();
        open_and_run(&mut h, &path);
        if !h.state().gpu_available_for_test() {
            return skip(readme, "w35-alttp", "no wgpu adapter");
        }
        alttp_take_control(&mut h);
        h.state_mut().set_mode_for_test(Mode::Enhanced);
        h.state_mut().set_diorama_for_test(true);
        h.state_mut().set_diorama_camera_for_test(52, 0, 160, true);
        run_n(&mut h, 30);
        shoot(&mut h, "w35-alttp-follow-start.png");
        let end = h.state().frame_count_for_test() + 600;
        while h.state().frame_count_for_test() < end {
            play_script(&mut h, "w4 D30 w15 R40 w15 U20 w15 L40 w15 D20 w20");
        }
        release_all(&mut h);
        run_n(&mut h, 40);
        shoot(&mut h, "w35-alttp-follow-playing.png");
        h.state_mut()
            .set_diorama_camera_for_test(75, 40, 100, false);
        run_n(&mut h, 10);
        shoot(&mut h, "w35-alttp-steep-yaw.png");
        note(
            readme,
            format!(
                "w35-alttp|follow, steep yaw; {:?}",
                h.state().heights_numbers_for_test()
            ),
        );
        h.state().send_core_command_for_test(CoreCommand::Shutdown);
        std::thread::sleep(Duration::from_secs(3));
    }
}

/// W35-01: D6's soak. SMW at the 4x window with the Diorama (layers, shadows)
/// for `RF_W35_SOAK_FRAMES` frames (default 36 000: ten game minutes at 60 Hz),
/// pacing off, in 600-frame chunks: the budget gate must never trip and the
/// Diorama must still be drawing at the end. `RF_SCREENS_ONLY=w35-soak`.
fn shot_w35_soak(readme: &mut Vec<String>) {
    if !wanted("w35-soak") {
        return;
    }
    // SAFETY: set before any harness of this shot is built.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RF_SCREENS_WINDOW", "1300x1100");
    }
    let total: u64 = std::env::var("RF_W35_SOAK_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(36_000);
    let Some(path) = rom("snes", "Super Mario World (USA).zip") else {
        return skip(readme, "w35-soak", "Super Mario World (USA).zip not found");
    };
    let Some(mut h) = smw_yi1_harness(&path) else {
        return skip(readme, "w35-soak", "never reached Yoshi's Island 1");
    };
    h.state_mut().set_mode_for_test(Mode::Enhanced);
    h.state_mut().set_diorama_for_test(true);
    h.state_mut().set_diorama_camera_for_test(40, 0, 160, true);
    run_n(&mut h, 30);
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
    let (mut worst_p95, mut worst_trips) = (0.0_f64, 0u32);
    let t0 = Instant::now();
    let f0 = h.state().frame_count_for_test();
    let mut chunks = 0u32;
    while h.state().frame_count_for_test() < f0 + total {
        play_for(
            &mut h,
            600,
            "w4 R90 B+R30 R60 A+R24 R50 L30 R+B40 w20",
            &[(0x1490, 0xFF), (0x1497, 0xFF)],
            enter_smw,
        );
        chunks += 1;
        worst_p95 = worst_p95.max(h.state().diorama_p95_ms_for_test());
        worst_trips = worst_trips.max(h.state().diorama_gate_trips_for_test());
        if chunks.is_multiple_of(10) {
            println!(
                "w35 soak {} frames {:.0} s: p95 {:.2} ms trips {} image {:?}",
                h.state().frame_count_for_test() - f0,
                t0.elapsed().as_secs_f64(),
                h.state().diorama_p95_ms_for_test(),
                h.state().diorama_gate_trips_for_test(),
                h.state().diorama_size_for_test(),
            );
        }
    }
    let frames = h.state().frame_count_for_test() - f0;
    note(
        readme,
        format!(
            "w35-soak|{frames} frames in {:.0} s ({:.1} fps, pacing off), worst window p95 {worst_p95:.2} ms, gate trips {worst_trips}, Diorama still drawing: {}",
            t0.elapsed().as_secs_f64(),
            frames as f64 / t0.elapsed().as_secs_f64(),
            h.state().diorama_size_for_test().is_some()
        ),
    );
    assert_eq!(worst_trips, 0, "the budget gate tripped");
    assert!(h.state().diorama_size_for_test().is_some());
    h.state().send_core_command_for_test(CoreCommand::Shutdown);
    std::thread::sleep(Duration::from_secs(3));
}

/// The tour's contact sheet: one row per game, flat | morning | noon | dusk.
fn tour_sheet(readme: &mut Vec<String>) {
    let d = w34_dir();
    let load = |name: String| -> Option<Pic> {
        let img = image::open(d.join(name)).ok()?.to_rgba8();
        Some(Pic {
            w: img.width() as usize,
            h: img.height() as usize,
            rgba: img.into_raw(),
        })
    };
    let cell_w = 360usize;
    let mut rows: Vec<Pic> = Vec::new();
    for game in ["smw", "smw-32x9", "smb3", "alttp"] {
        let mut row: Option<Pic> = None;
        for tag in ["flat", "hd2d-morning", "hd2d-noon", "hd2d-dusk"] {
            let Some(p) = load(format!("{game}-{tag}.png")) else {
                continue;
            };
            let p = p.fit_width(cell_w);
            row = Some(match row {
                Some(r) => r.beside(&p, 4),
                None => p,
            });
        }
        rows.extend(row);
    }
    let Some(sheet) = stack_rows(&rows) else {
        return;
    };
    save(&d.join("contact-sheet.png"), &sheet);
    note(
        readme,
        "contact-sheet|-|rows: smw, smw 32:9, smb3, alttp; flat | morning | noon | dusk"
            .to_string(),
    );
}

/// SMW up to "MARIO START!", then sampled one core frame at a time for
/// `frames` frames: `watch` runs after each (ticket W34-07's first-frame
/// proof). The same taps as [`smw_enter_yi1`].
fn smw_enter_yi1_watched(
    h: &mut H,
    frames: u64,
    before_level: &mut dyn FnMut(&mut H),
    watch: &mut dyn FnMut(&mut H, u64),
) {
    use egui::Key as K;
    wait(h, "smw", 200);
    for i in 0..10 {
        tap(h, if i % 2 == 0 { K::Enter } else { K::X }, 6);
        wait(h, "smw", 100);
    }
    wait(h, "smw", 300);
    tap(h, K::X, 8);
    wait(h, "smw", 400);
    tap(h, K::ArrowLeft, 8);
    wait(h, "smw", 150);
    before_level(h);
    tap(h, K::X, 8);
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
    for n in 0..frames {
        let t = h.state().frame_count_for_test() + 1;
        run_frames(h, t, Duration::from_secs(10));
        watch(h, n);
    }
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(true));
}

/// Yoshi's Island 1 has a blue sky; Yoshi's House (where a missed map tap
/// lands) a pale one.
fn smw_sky_is_blue(h: &H) -> bool {
    grab(h).is_some_and(|p| {
        // A widened frame has the 256 dots in its middle.
        let pad = p.w.saturating_sub(256) / 2;
        [(20usize, 40usize), (60, 40), (100, 40)]
            .iter()
            .all(|&(x, y)| {
                let at = (y * p.w + pad + x) * 4;
                p.rgba[at + 2] > p.rgba[at] + 40
            })
    })
}

/// Open SMW and enter YI1, trying again from a fresh harness when the map
/// taps (paced in real time) landed on Yoshi's House instead.
fn smw_yi1_harness(path: &Path) -> Option<H> {
    for attempt in 0..4 {
        let mut h = new_harness();
        open_and_run(&mut h, path);
        if !h.state().gpu_available_for_test() {
            return None;
        }
        smw_enter_yi1(&mut h);
        if smw_sky_is_blue(&h) {
            return Some(h);
        }
        println!("LEVEL attempt {attempt}: not in Yoshi's Island 1, again");
        probe(&h, &format!("smw-wrong-level-{attempt}"));
    }
    None
}

fn level_numbers(h: &H) -> String {
    let lh = h.state().level_heights_for_test();
    let hn = h.state().heights_numbers_for_test();
    format!("level {lh:?}; learner {hn:?}")
}

/// W34-07 acceptance: Super Mario World, Yoshi's Island 1, at 32:9 with the
/// scout: the Diorama raises the scouted right margin from the tiles learned
/// in play; a second visit shows heights on the first frame. Cold cache for
/// the first visit. `RF_SCREENS_ONLY=diorama-level-smw`.
fn shot_diorama_level_smw(readme: &mut Vec<String>) {
    use std::sync::atomic::Ordering::Relaxed;
    let name = "smw-32x9";
    if !wanted("diorama-level-smw") {
        return;
    }
    let Some(path) = rom("snes", "Super Mario World (USA).zip") else {
        return skip(readme, name, "Super Mario World (USA).zip not found");
    };
    let cold = std::env::temp_dir().join(format!("rf_level_smw_{}", std::process::id()));
    let reuse = std::env::var_os("RF_LEVEL_REUSE").map(PathBuf::from);
    let cold = reuse.clone().unwrap_or(cold);
    if reuse.is_none() {
        let _ = std::fs::remove_dir_all(&cold);
    }
    std::fs::create_dir_all(&cold).expect("cold cache dir");
    // SAFETY: nothing else in this process reads the environment while the
    // harness is between sessions.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("TMPDIR", &cold);
    }
    println!("LEVEL cache dir {}", cold.display());
    // A window wide enough for the play rect to be 32:9 (and the pictures
    // are wanted in the docs all the same).
    // SAFETY: set before any harness of this shot is built.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RF_SCREENS_WINDOW", "1700x760");
        std::env::set_var("RF_SCREENS_KEEP", "1");
    }
    let tel = retroforge::level_heights::telemetry();
    if reuse.is_none() {
        // ---- session 1: learn at 4:3 (the learner needs no canvas), then
        // quit: the table is what the next session starts from.
        {
            let Some(mut h) = smw_yi1_harness(&path) else {
                return skip(
                    readme,
                    name,
                    "no wgpu adapter, or never reached Yoshi's Island 1",
                );
            };
            h.state_mut().set_mode_for_test(Mode::Enhanced);
            h.state_mut().set_diorama_for_test(true);
            run_n(&mut h, 30);
            let frames: u64 = std::env::var("RF_DIORAMA_FRAMES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1800);
            play_for(
                &mut h,
                frames,
                "w4 R90 B+R30 R60 A+R24 R50 L30 R+B40 w20",
                // $1490 star power, $1497 the hurt-flash timer.
                &[(0x1490, 0xFF), (0x1497, 0xFF)],
                enter_smw,
            );
            release_all(&mut h);
            let learned = h.state().heights_numbers_for_test();
            println!("LEVEL session 1, learned in play at 4:3: {learned:?}");
            h.state().send_core_command_for_test(CoreCommand::Shutdown);
            std::thread::sleep(Duration::from_secs(3));
        }
        // ---- session 2: a fresh start of the level, the scout, 32:9.
        let Some(mut h) = smw_yi1_harness(&path) else {
            return skip(readme, name, "never reached Yoshi's Island 1 again");
        };
        h.state_mut().set_mode_for_test(Mode::Enhanced);
        h.state_mut().set_diorama_for_test(true);
        // The scout, then 32:9, as `shot_scout`: unpaced until the worker
        // runs, the live game paused while it works.
        let telemetry = h.state().scout_telemetry_for_test();
        h.state_mut().set_scout_enabled_for_test(true);
        h.state_mut()
            .set_widescreen_aspect_for_test(Aspect::Wide32x9);
        h.state()
            .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
        let t1 = Instant::now();
        let f1 = h.state().frame_count_for_test();
        h.state_mut().set_widescreen(true);
        while telemetry.started.load(Relaxed) == 0 && t1.elapsed() < Duration::from_secs(60) {
            let t = h.state().frame_count_for_test() + 2;
            run_frames(&mut h, t, Duration::from_secs(10));
        }
        h.state().send_core_command_for_test(CoreCommand::Pause);
        let mut finished = false;
        while t1.elapsed() < Duration::from_secs(300) {
            std::thread::sleep(Duration::from_millis(100));
            run_n(&mut h, 1);
            if telemetry.finished.load(Relaxed) + telemetry.gave_up.load(Relaxed) >= 1 {
                finished = true;
                break;
            }
        }
        let scout_wall = t1.elapsed();
        println!(
            "LEVEL scout telemetry: frames {} cells {} started {} finished {} aborted {} gave_up {}",
            telemetry.frames.load(Relaxed),
            telemetry.cells.load(Relaxed),
            telemetry.started.load(Relaxed),
            telemetry.finished.load(Relaxed),
            telemetry.aborted.load(Relaxed),
            telemetry.gave_up.load(Relaxed),
        );
        if !finished {
            return skip(readme, name, "the scout never finished");
        }
        let live_frames = h.state().frame_count_for_test() - f1;
        // The merged canvas reaches the margins within a few repaints; the
        // game stays paused (nothing more is played, so nothing dies).
        run_n(&mut h, 120);
        probe(&h, "level-2-after-scout");
        println!(
            "LEVEL scout: {:.1} s wall, {live_frames} live frames; {}",
            scout_wall.as_secs_f64(),
            level_numbers(&h)
        );
        for layer in 0..3 {
            println!(
                "LEVEL canvas census layer {layer}: {:?}",
                h.state().canvas_census_for_test(layer, 1)
            );
        }
        diorama_pair_as(
            &mut h,
            "diorama-level",
            name,
            "Super Mario World, Yoshi's Island 1, 32:9 with the level scout; heights learned in play, applied to the canvas (Live and Scout cells)",
            readme,
        );
        let line = format!(
            "LEVEL-HEIGHTS {name}: tracker frames {} scene changes {} restored {} no-format frames {}; \
             audit of live cells against canvas keys: equal {} pattern-only {} palette-only {} different {} unkeyed {}; {}; passes {} (total {:.1} ms, longest {:.2} ms, blocks {}), rebuilds {} (total {:.1} ms, longest {:.2} ms)",
            tel.frames.load(Relaxed),
            tel.scene_changes.load(Relaxed),
            tel.restored.load(Relaxed),
            tel.no_format.load(Relaxed),
            tel.audit_equal.load(Relaxed),
            tel.audit_pattern_only.load(Relaxed),
            tel.audit_palette_only.load(Relaxed),
            tel.audit_different.load(Relaxed),
            tel.audit_unkeyed.load(Relaxed),
            level_numbers(&h),
            tel.passes.load(Relaxed),
            tel.total_us.load(Relaxed) as f64 / 1000.0,
            tel.max_us.load(Relaxed) as f64 / 1000.0,
            tel.blocks.load(Relaxed),
            tel.rebuilds.load(Relaxed),
            tel.rebuild_us.load(Relaxed) as f64 / 1000.0,
            tel.max_rebuild_us.load(Relaxed) as f64 / 1000.0,
        );
        note(readme, line);
        // The flat 32:9 frame as the core and the margin fill make it, at the
        // same moment (the window crop stretches a wide frame to 4:3).
        if let Some(flat) = grab(&h) {
            save(
                &w34_dir().join("diorama-level-smw-32x9-off.png"),
                &flat.opaque(),
            );
        }
        // And the same frame from a raised, turned camera, where the depth of
        // the scouted ground can be seen.
        h.state_mut()
            .set_diorama_camera_for_test(85, -25, 100, false);
        run_n(&mut h, 30);
        step_from(&mut h);
        step_from(&mut h);
        if let Some(tilted) = grab_play_area(&mut h) {
            save(
                &w34_dir().join("diorama-level-smw-32x9-tilted.png"),
                &tilted.opaque(),
            );
        }
        println!(
            "LEVEL alignment vote: {} decisions, last shift ({}, {}) dots, {:.2} ms spent voting",
            tel.calibrations.load(Relaxed),
            tel.shift_dx.load(Relaxed),
            tel.shift_dy.load(Relaxed),
            tel.calibration_us.load(Relaxed) as f64 / 1000.0
        );
        if std::env::var("RF_LEVEL_FPS").is_ok() {
            diorama_fps(&mut h, "smw-32x9", readme);
        }
        // The core thread writes the heights as it stops.
        h.state().send_core_command_for_test(CoreCommand::Shutdown);
        std::thread::sleep(Duration::from_secs(3));
        drop(h);
    }
    let cache_dir = cold.join("retroforge-canvas-cache").join("entries");
    let sizes: Vec<u64> = std::fs::read_dir(&cache_dir)
        .map(|d| {
            d.flatten()
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .collect()
        })
        .unwrap_or_default();
    note(
        readme,
        format!("LEVEL-HEIGHTS {name}: cache entries {sizes:?} bytes"),
    );

    // ---- second visit: no play, no scout, heights on the first frame.
    for attempt in 0..4 {
        let mut h = new_harness();
        open_and_run(&mut h, &path);
        let telemetry = h.state().scout_telemetry_for_test();
        let mut first_level: Option<u64> = None;
        let mut first_raised: Option<(u64, usize)> = None;
        let mut first_lit: Option<u64> = None;
        let mut wide_on_at: Option<u64> = None;
        let mut first_scene_cells: Option<u64> = None;
        let base_started = telemetry.started.load(Relaxed);
        smw_enter_yi1_watched(
            &mut h,
            240,
            &mut |h| {
                // Enhanced and Diorama on for the whole visit; 32:9 goes on
                // in the level itself, as the persistence shot does (the
                // canvas axes start from the level's own scroll).
                h.state_mut().set_mode_for_test(Mode::Enhanced);
                h.state_mut().set_diorama_for_test(true);
                h.state_mut()
                    .set_widescreen_aspect_for_test(Aspect::Wide32x9);
                h.state_mut().set_scout_enabled_for_test(true);
                h.state_mut().set_measure_margins_for_test(true);
            },
            &mut |h, n| {
                if wide_on_at.is_none() && smw_sky_is_blue(h) {
                    h.state_mut().set_widescreen(true);
                    wide_on_at = Some(n);
                }
                if first_level.is_none() && h.state().level_heights_for_test().is_some() {
                    first_level = Some(n);
                }
                if first_raised.is_none() {
                    if let Some((_, raised, ..)) = h.state().heights_numbers_for_test() {
                        if raised > 0 {
                            first_raised = Some((n, raised));
                        }
                    }
                }
                if first_lit.is_none() && smw_sky_is_blue(h) {
                    first_lit = Some(n);
                }
                if first_scene_cells.is_none() && h.state().margin_known_for_test().is_some() {
                    first_scene_cells = Some(n);
                }
            },
        );
        run_n(&mut h, 30);
        probe(&h, "level-3-second-visit");
        println!(
            "LEVEL second visit margins known {:?}",
            h.state().margin_known_for_test()
        );
        if !smw_sky_is_blue(&h) {
            println!("LEVEL second visit attempt {attempt}: not in Yoshi's Island 1, again");
            drop(h);
            continue;
        }
        let numbers = level_numbers(&h);
        let line = format!(
            "LEVEL-HEIGHTS {name} second visit (frames counted from the X press that starts the level; 32:9 went on at frame {wide_on_at:?}, when the level's sky first showed): \
             the level's sky first on screen at frame {first_lit:?}, level heights held from \
             {first_level:?}, first raised cells {first_raised:?}, canvas margins known from \
             {first_scene_cells:?}; scouts started {} (must be 0); {numbers}",
            telemetry.started.load(Relaxed) - base_started
        );
        note(readme, line);
        // The shot: the game pauses, one frame steps, Diorama on.
        h.state_mut().pause_for_test();
        run_n(&mut h, 30);
        step_from(&mut h);
        step_from(&mut h);
        if let Some(on) = grab_play_area(&mut h) {
            save(
                &w34_dir().join("diorama-level-smw-32x9-second-visit.png"),
                &on.opaque(),
            );
        }
        if first_raised.is_none() {
            println!("LEVEL second visit: no raised cell in 240 frames");
        }
        drop(h);
        break;
    }
}

/// W34-07: what a second visit gets on the NES. The canvas cannot be re-tiled
/// there (its cells hold master colours, not tile bytes), so there is no
/// whole-level view; what persists is the learned table and the call on the
/// camera. Cold cache; learn, quit, come back, count the frames until blocks
/// stand. `RF_SCREENS_ONLY=diorama-level-smb3`.
fn shot_diorama_level_smb3(readme: &mut Vec<String>) {
    let name = "smb3";
    if !wanted("diorama-level-smb3") {
        return;
    }
    let Some(path) = rom("nes", "Super Mario Bros. 3 (USA).zip") else {
        return skip(readme, name, "Super Mario Bros. 3 (USA).zip not found");
    };
    let cold = std::env::temp_dir().join(format!("rf_level_smb3_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cold);
    std::fs::create_dir_all(&cold).expect("cold cache dir");
    // SAFETY: nothing else in this process reads the environment between sessions.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("TMPDIR", &cold);
        std::env::remove_var("RF_SCREENS_WINDOW");
        std::env::remove_var("RF_SCREENS_KEEP");
    }
    {
        let mut h = new_harness();
        open_and_run(&mut h, &path);
        if !h.state().gpu_available_for_test() {
            return skip(readme, name, "no wgpu adapter");
        }
        smb3_enter_level(&mut h);
        h.state_mut().set_mode_for_test(Mode::Enhanced);
        h.state_mut().set_diorama_for_test(true);
        run_n(&mut h, 30);
        let frames: u64 = std::env::var("RF_DIORAMA_FRAMES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1800);
        play_for(
            &mut h,
            frames,
            "w4 R90 B+R30 R60 A+R24 R50 L30 R+B40 w20",
            &[],
            enter_smb3,
        );
        release_all(&mut h);
        println!(
            "LEVEL smb3 session 1, learned in play: {:?}",
            h.state().heights_numbers_for_test()
        );
        h.state().send_core_command_for_test(CoreCommand::Shutdown);
        std::thread::sleep(Duration::from_secs(3));
    }
    // Session 2: into the level with Diorama off, then on, one frame at a time.
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    smb3_enter_level(&mut h);
    h.state_mut().set_mode_for_test(Mode::Enhanced);
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
    h.state_mut().set_diorama_for_test(true);
    let mut first: Option<(u64, usize, String)> = None;
    let mut at_one = None;
    for n in 0..120u64 {
        let t = h.state().frame_count_for_test() + 1;
        run_frames(&mut h, t, Duration::from_secs(10));
        let nums = h.state().heights_numbers_for_test();
        if n == 1 {
            at_one = nums.clone();
        }
        if let (None, Some((plan, raised, ..))) = (&first, &nums) {
            if *raised > 0 {
                first = Some((n, *raised, plan.clone()));
            }
        }
    }
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(true));
    note(
        readme,
        format!(
            "LEVEL-HEIGHTS {name} second visit (NES: table and camera call restored, no canvas re-tile): \
             frames after Diorama went on until blocks stood: {first:?}; learner at frame 1: {at_one:?}; now {:?}",
            h.state().heights_numbers_for_test()
        ),
    );
    h.state_mut().pause_for_test();
    run_n(&mut h, 30);
    step_from(&mut h);
    step_from(&mut h);
    if let Some(on) = grab_play_area(&mut h) {
        save(
            &w34_dir().join("diorama-level-smb3-second-visit.png"),
            &on.opaque(),
        );
    }
}

/// W34-08: the Diorama with layers. Per game: the off/on pair (the picture
/// pulled apart into planes, the HUD flat on top), then the layered cost
/// against the one-plane picture of W34-07 in the same binary.
/// `RF_SCREENS_ONLY=diorama-layers-smw` (or `-smb3`, `-alttp`).
fn layers_cost(h: &mut H, name: &str, readme: &mut Vec<String>) {
    if std::env::var("RF_LAYERS_NO_COST").is_ok() {
        return;
    }
    h.state_mut().set_diorama_for_test(true);
    h.state_mut().resume_for_test();
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
    run_n(h, 10);
    let mut rows = Vec::new();
    for layers in [false, true, false, true] {
        h.state_mut().set_diorama_layers_for_test(layers);
        run_n(h, 20);
        let _ = h.state_mut().diorama_stage_ms_for_test();
        let f0 = h.state().frame_count_for_test();
        let t0 = Instant::now();
        run_frames(h, f0 + 240, Duration::from_secs(300));
        let n = h.state().frame_count_for_test() - f0;
        let ([wide, split, pass, mesh], _) = h.state_mut().diorama_stage_ms_for_test();
        rows.push((
            layers,
            n as f64 / t0.elapsed().as_secs_f64(),
            h.state().diorama_p95_ms_for_test(),
            h.state().diorama_p50_ms_for_test(),
            [wide, split, pass, mesh],
        ));
    }
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(true));
    h.state_mut().set_diorama_layers_for_test(true);
    let text: Vec<String> = rows
        .iter()
        .map(|(l, fps, p95, p50, [wide, split, pass, mesh])| {
            format!(
                "{} {fps:.1} fps p50 {p50:.2} p95 {p95:.2} ms (mean wide {wide:.2} split {split:.2} pass {pass:.2} of which mesh {mesh:.2})",
                if *l { "layers" } else { "one plane" }
            )
        })
        .collect();
    note(
        readme,
        format!(
            "DIORAMA-LAYERS-COST {name} (240 frames per row, alternating, pacing off, image {:?}): {}",
            h.state().diorama_size_for_test(),
            text.join(" | ")
        ),
    );
}

fn shot_diorama_layers_smw(readme: &mut Vec<String>) {
    let name = "smw";
    if !wanted("diorama-layers-smw") {
        return;
    }
    let Some(path) = rom("snes", "Super Mario World (USA).zip") else {
        return skip(readme, name, "Super Mario World (USA).zip not found");
    };
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    if !h.state().gpu_available_for_test() {
        return skip(readme, name, "no wgpu adapter");
    }
    smw_enter_yi1(&mut h);
    h.state_mut().set_mode_for_test(Mode::Enhanced);
    h.state_mut().set_diorama_for_test(true);
    run_n(&mut h, 30);
    let frames: u64 = std::env::var("RF_DIORAMA_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1800);
    play_for(
        &mut h,
        frames,
        "w4 R90 B+R30 R60 A+R24 R50 L30 R+B40 w20",
        &[(0x1490, 0xFF)],
        enter_smw,
    );
    enter_smw(&mut h);
    wait(&mut h, "smw", 150);
    play_script(&mut h, "R40 B+R20");
    release_all(&mut h);
    // A fixed mid-morning sun (the wall clock would make the shots depend on
    // the hour they are taken).
    set_time_of_day(&mut h, 40);
    diorama_pair_as(
        &mut h,
        "diorama-layers",
        name,
        "Super Mario World, Yoshi's Island 1: ground, hills and sky on their own planes, the status bar flat on top",
        readme,
    );
    // A resized window is for the cost rows: its pictures stay out of the docs.
    let d = if std::env::var("RF_SCREENS_WINDOW").is_ok()
        && std::env::var("RF_SCREENS_KEEP").is_err()
    {
        probe_dir()
    } else {
        w34_dir()
    };
    // The hills behind the ground: a yawed camera, planes on and off.
    for (tag, layers, pitch, yaw, zoom, follow) in [
        ("yaw15", true, 52u8, 15i8, 100u16, false),
        ("oneplane-yaw15", false, 52, 15, 100, false),
        ("pitch-lo", true, 20, 0, 100, false),
        ("pitch-hi", true, 80, 0, 100, false),
        ("oneplane-pitch-hi", false, 80, 0, 100, false),
        ("zoom", true, 40, 0, 160, true),
        ("oneplane-zoom", false, 40, 0, 160, true),
    ] {
        h.state_mut().set_diorama_layers_for_test(layers);
        h.state_mut()
            .set_diorama_camera_for_test(pitch, yaw, zoom, follow);
        run_n(&mut h, 20);
        step_from(&mut h);
        step_from(&mut h);
        if let Some(p) = grab_play_area(&mut h) {
            save(
                &d.join(format!("diorama-layers-{name}-{tag}.png")),
                &p.opaque(),
            );
        }
    }
    h.state_mut().set_diorama_layers_for_test(true);
    h.state_mut().set_diorama_camera_for_test(52, 0, 100, false);
    layers_cost(&mut h, name, readme);
}

/// W34-09 acceptance: the Diorama at 16:9 and 32:9 (Super Mario World, planes
/// standing in the canvas margins, the flat HUD in the middle) and at 16:9 on a
/// top-down game (A Link to the Past). One 1700 x 760 window, so the play rect
/// is wide; the cost rows are `RF_WIDE_COST=1`. `RF_SCREENS_ONLY=diorama-wide`
/// (or `-smw`, `-alttp`).
fn shot_diorama_wide(readme: &mut Vec<String>) {
    if !(wanted("diorama-wide-smw") || wanted("diorama-wide-alttp")) {
        return;
    }
    // SAFETY: set before any harness of this shot is built.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RF_SCREENS_WINDOW", "1900x800");
        std::env::set_var("RF_SCREENS_KEEP", "1");
    }
    let frames: u64 = std::env::var("RF_DIORAMA_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1800);
    if wanted("diorama-wide-smw") {
        use std::sync::atomic::Ordering::Relaxed;
        let name = "smw";
        // Cold cache for the first visit (the level's tiles are learned in
        // session 1 at 4:3, as `shot_diorama_level_smw` does; session 2
        // starts the level again at the wide aspect).
        let cold = std::env::temp_dir().join(format!("rf_wide_smw_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cold);
        std::fs::create_dir_all(&cold).expect("cold cache dir");
        // SAFETY: nothing else in this process reads the environment while
        // the harness is between sessions.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("TMPDIR", &cold);
        }
        match rom("snes", "Super Mario World (USA).zip") {
            None => skip(readme, name, "Super Mario World (USA).zip not found"),
            Some(path) => 'smw: {
                {
                    let Some(mut h) = smw_yi1_harness(&path) else {
                        skip(
                            readme,
                            name,
                            "no wgpu adapter, or never reached Yoshi's Island 1",
                        );
                        break 'smw;
                    };
                    h.state_mut().set_mode_for_test(Mode::Enhanced);
                    h.state_mut().set_diorama_for_test(true);
                    run_n(&mut h, 30);
                    play_for(
                        &mut h,
                        frames,
                        "w4 R90 B+R30 R60 A+R24 R50 L30 R+B40 w20",
                        &[(0x1490, 0xFF), (0x1497, 0xFF)],
                        enter_smw,
                    );
                    release_all(&mut h);
                    println!(
                        "WIDE session 1 learned at 4:3: {:?}",
                        h.state().heights_numbers_for_test()
                    );
                    h.state().send_core_command_for_test(CoreCommand::Shutdown);
                    std::thread::sleep(Duration::from_secs(3));
                }
                let Some(mut h) = smw_yi1_harness(&path) else {
                    skip(readme, name, "never reached Yoshi's Island 1 again");
                    break 'smw;
                };
                h.state_mut().set_mode_for_test(Mode::Enhanced);
                h.state_mut()
                    .set_widescreen_aspect_for_test(Aspect::Wide16x9);
                h.state_mut().set_diorama_for_test(true);
                run_n(&mut h, 30);
                // A fresh start of the level; the scout fills the canvas
                // beyond what the camera has seen (Live and Scout cells),
                // as `shot_diorama_level_smw` does (`RF_WIDE_NOSCOUT=1`
                // leaves it to the live frames).
                if std::env::var("RF_WIDE_NOSCOUT").is_err() {
                    let telemetry = h.state().scout_telemetry_for_test();
                    h.state_mut().set_scout_enabled_for_test(true);
                    h.state()
                        .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
                    let t1 = Instant::now();
                    h.state_mut().set_widescreen(true);
                    while telemetry.started.load(Relaxed) == 0
                        && t1.elapsed() < Duration::from_secs(60)
                    {
                        let t = h.state().frame_count_for_test() + 2;
                        run_frames(&mut h, t, Duration::from_secs(10));
                    }
                    h.state().send_core_command_for_test(CoreCommand::Pause);
                    while t1.elapsed() < Duration::from_secs(300) {
                        std::thread::sleep(Duration::from_millis(100));
                        run_n(&mut h, 1);
                        if telemetry.finished.load(Relaxed) + telemetry.gave_up.load(Relaxed) >= 1 {
                            break;
                        }
                    }
                    run_n(&mut h, 120);
                } else {
                    h.state_mut().set_widescreen(true);
                    run_n(&mut h, 120);
                }
                probe(&h, "wide-smw-ready");
                set_time_of_day(&mut h, 40);
                for (aspect, tag) in [(Aspect::Wide16x9, "16x9"), (Aspect::Wide32x9, "32x9")] {
                    h.state_mut().set_widescreen_aspect_for_test(aspect);
                    h.state_mut().set_widescreen(true);
                    diorama_pair_as(
                        &mut h,
                        "diorama-wide",
                        &format!("smw-{tag}"),
                        &format!("Super Mario World, Yoshi's Island 1, {tag}: the planes stand in the canvas margins, the status bar flat in the middle"),
                        readme,
                    );
                    note(
                        readme,
                        format!(
                            "DIORAMA-WIDE smw-{tag}: image {:?}; {}",
                            h.state().diorama_size_for_test(),
                            level_numbers(&h)
                        ),
                    );
                    // A swung, raised camera: the planes and the edges.
                    h.state_mut()
                        .set_diorama_camera_for_test(70, 20, 100, false);
                    run_n(&mut h, 20);
                    step_from(&mut h);
                    step_from(&mut h);
                    if let Some(p) = grab_play_area(&mut h) {
                        save(
                            &w34_dir().join(format!("diorama-wide-smw-{tag}-yaw.png")),
                            &p.opaque(),
                        );
                    }
                    h.state_mut().set_diorama_camera_for_test(52, 0, 100, false);
                    run_n(&mut h, 10);
                }
                // The cost rows last: they run the game unpaced.
                if std::env::var("RF_WIDE_COST").is_ok() {
                    for (aspect, tag) in [(Aspect::Wide16x9, "16x9"), (Aspect::Wide32x9, "32x9")] {
                        h.state_mut().set_widescreen_aspect_for_test(aspect);
                        h.state_mut().set_widescreen(true);
                        layers_cost(&mut h, &format!("smw-{tag}"), readme);
                    }
                }
                h.state().send_core_command_for_test(CoreCommand::Shutdown);
                std::thread::sleep(Duration::from_secs(3));
            }
        }
    }
    if wanted("diorama-wide-alttp") {
        let name = "alttp";
        match rom(
            "snes",
            "Legend of Zelda, The - A Link to the Past (USA).zip",
        ) {
            None => skip(readme, name, "A Link to the Past (USA).zip not found"),
            Some(path) => 'alttp: {
                let mut h = new_harness();
                open_and_run(&mut h, &path);
                if !h.state().gpu_available_for_test() {
                    skip(readme, name, "no wgpu adapter");
                    break 'alttp;
                }
                alttp_take_control(&mut h);
                h.state_mut().set_mode_for_test(Mode::Enhanced);
                h.state_mut()
                    .set_widescreen_aspect_for_test(Aspect::Wide16x9);
                h.state_mut().set_widescreen(true);
                h.state_mut().set_diorama_for_test(true);
                run_n(&mut h, 30);
                let end = h.state().frame_count_for_test() + frames;
                while h.state().frame_count_for_test() < end {
                    play_script(&mut h, "w4 D30 w15 R40 w15 U20 w15 L40 w15 D20 w20");
                }
                release_all(&mut h);
                set_time_of_day(&mut h, 40);
                diorama_pair_as(
                    &mut h,
                    "diorama-wide",
                    "alttp-16x9",
                    "A Link to the Past, Link's house, 16:9: the floor goes on into the margins, the HUD flat in the middle",
                    readme,
                );
                note(
                    readme,
                    format!(
                        "DIORAMA-WIDE alttp-16x9: image {:?}",
                        h.state().diorama_size_for_test()
                    ),
                );
                if std::env::var("RF_WIDE_COST").is_ok() {
                    layers_cost(&mut h, "alttp-16x9", readme);
                }
            }
        }
    }
}

fn shot_diorama_layers_smb3(readme: &mut Vec<String>) {
    let name = "smb3";
    if !wanted("diorama-layers-smb3") {
        return;
    }
    let Some(path) = rom("nes", "Super Mario Bros. 3 (USA).zip") else {
        return skip(readme, name, "Super Mario Bros. 3 (USA).zip not found");
    };
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    if !h.state().gpu_available_for_test() {
        return skip(readme, name, "no wgpu adapter");
    }
    smb3_enter_level(&mut h);
    h.state_mut().set_mode_for_test(Mode::Enhanced);
    h.state_mut().set_diorama_for_test(true);
    run_n(&mut h, 30);
    let frames: u64 = std::env::var("RF_DIORAMA_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1800);
    play_for(
        &mut h,
        frames,
        "w4 R90 B+R30 R60 A+R24 R50 L30 R+B40 w20",
        &[],
        enter_smb3,
    );
    enter_smb3(&mut h);
    play_script(&mut h, "R60 B+R20");
    release_all(&mut h);
    // A fixed mid-morning sun (the wall clock would make the shots depend on
    // the hour they are taken).
    set_time_of_day(&mut h, 40);
    diorama_pair_as(
        &mut h,
        "diorama-layers",
        name,
        "Super Mario Bros. 3, world 1-1: sky behind the playfield, the status bar flat on top",
        readme,
    );
    // A resized window is for the cost rows: its pictures stay out of the docs.
    let d = if std::env::var("RF_SCREENS_WINDOW").is_ok()
        && std::env::var("RF_SCREENS_KEEP").is_err()
    {
        probe_dir()
    } else {
        w34_dir()
    };
    for (tag, layers, pitch, yaw, zoom, follow) in [
        ("yaw15", true, 52u8, 15i8, 100u16, false),
        ("oneplane-yaw15", false, 52, 15, 100, false),
        ("pitch-hi", true, 80, 0, 100, false),
        ("oneplane-pitch-hi", false, 80, 0, 100, false),
    ] {
        h.state_mut().set_diorama_layers_for_test(layers);
        h.state_mut()
            .set_diorama_camera_for_test(pitch, yaw, zoom, follow);
        run_n(&mut h, 20);
        step_from(&mut h);
        step_from(&mut h);
        if let Some(p) = grab_play_area(&mut h) {
            save(
                &d.join(format!("diorama-layers-{name}-{tag}.png")),
                &p.opaque(),
            );
        }
    }
    h.state_mut().set_diorama_layers_for_test(true);
    h.state_mut().set_diorama_camera_for_test(52, 0, 100, false);
    layers_cost(&mut h, name, readme);
}

fn shot_diorama_layers_alttp(readme: &mut Vec<String>) {
    let name = "alttp";
    if !wanted("diorama-layers-alttp") {
        return;
    }
    let Some(path) = rom(
        "snes",
        "Legend of Zelda, The - A Link to the Past (USA).zip",
    ) else {
        return skip(readme, name, "A Link to the Past (USA).zip not found");
    };
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    if !h.state().gpu_available_for_test() {
        return skip(readme, name, "no wgpu adapter");
    }
    alttp_take_control(&mut h);
    h.state_mut().set_mode_for_test(Mode::Enhanced);
    h.state_mut().set_diorama_for_test(true);
    run_n(&mut h, 30);
    let laps: u64 = std::env::var("RF_DIORAMA_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1800);
    let end = h.state().frame_count_for_test() + laps;
    while h.state().frame_count_for_test() < end {
        play_script(&mut h, "w4 D30 w15 R40 w15 U20 w15 L40 w15 D20 w20");
    }
    release_all(&mut h);
    // A fixed mid-morning sun (the wall clock would make the shots depend on
    // the hour they are taken).
    set_time_of_day(&mut h, 40);
    diorama_pair_as(
        &mut h,
        "diorama-layers",
        name,
        "A Link to the Past, Link's house, top-down: the HUD flat on top, Link one billboard",
        readme,
    );
    // A resized window is for the cost rows: its pictures stay out of the docs.
    let d = if std::env::var("RF_SCREENS_WINDOW").is_ok()
        && std::env::var("RF_SCREENS_KEEP").is_err()
    {
        probe_dir()
    } else {
        w34_dir()
    };
    // The same frame on one plane, for the comparison.
    h.state_mut().set_diorama_layers_for_test(false);
    run_n(&mut h, 20);
    step_from(&mut h);
    step_from(&mut h);
    if let Some(p) = grab_play_area(&mut h) {
        save(
            &d.join(format!("diorama-layers-{name}-oneplane.png")),
            &p.opaque(),
        );
    }
    h.state_mut().set_diorama_layers_for_test(true);
    layers_cost(&mut h, name, readme);
}

/// Ticket W35-02: Super Metroid with NO hand-made `[scout]` table. A scripted session in Ceres
/// (the shaft, its door, the corridor) finds one; the app's own "Save scout settings" writes it
/// to the player's profile; the app, reading that profile, scouts the corridor at 32:9 (off,
/// then on, cold canvas cache); a second visit over the same cache must have the margins at once
/// and start no scout.
fn shot_sm_scout(readme: &mut Vec<String>) {
    use egui_kittest::kittest::Queryable as _;
    use std::sync::atomic::Ordering::Relaxed;
    let name = "sm-scout";
    if !wanted(name) {
        return;
    }
    let file = discovery::SM.1;
    let Some(path) = rom("snes", file) else {
        skip(readme, name, &format!("{file} not found"));
        return;
    };
    let Some(mut rig) = discovery::Rig::open(discovery::SM.0, discovery::SM.1, None) else {
        skip(readme, name, "no rig");
        return;
    };
    rig.script(&discovery::sm_to_free_play());
    let mut corridor = None;
    let mut corridor_frame = 0;
    let t0 = Instant::now();
    let skeleton = discovery::sm_find_table(&mut rig, 600, &mut |r| {
        corridor = Some(r.st.snapshot().expect("snapshot"));
        corridor_frame = r.st.frame_count();
    });
    println!(
        "SM-FIND: {:.1}s\n{}",
        t0.elapsed().as_secs_f32(),
        skeleton.to_toml()
    );
    let corridor = corridor.expect("the corridor's snapshot");
    drop(rig);

    let cold = std::env::temp_dir().join(format!("rf_scout_sm_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cold);
    std::fs::create_dir_all(&cold).expect("cold cache dir");
    // SAFETY: the harness is built after this and nothing else in this process reads the
    // environment concurrently at this point.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("TMPDIR", &cold);
    }
    let config = PathBuf::from(
        std::env::var("RETROFORGE_CONFIG_DIR").expect("w30_w31_screens sets the config dir"),
    );
    let shipped = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../profiles/snes/super-metroid/profile.toml");
    let before = std::fs::read(&shipped).expect("the shipped profile");
    let user = config.join("retroforge/profiles/snes/super-metroid/profile.toml");
    let _ = std::fs::remove_file(&user);

    // The player's save: through the card, into the player's own profile folder.
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    h.state_mut().offer_scout_for_test((*skeleton).clone());
    run_n(&mut h, 3);
    h.get_by_label("Save scout settings").click();
    run_n(&mut h, 3);
    assert_eq!(
        std::fs::read(&shipped).expect("still there"),
        before,
        "the shipped profile was written to"
    );
    let saved = std::fs::read_to_string(&user).expect("the player's profile");
    let table = rf_profiles::load_str(&saved)
        .expect("the saved profile loads")
        .profile
        .scout
        .expect("a [scout] table");
    assert!(table.addr.iter().all(|r| r
        .source
        .as_deref()
        .is_some_and(|s| s.starts_with("observed:"))));
    println!("SM-SAVED: {} rows in {}", table.addr.len(), user.display());
    // Read the profile again (the app picks the player's copy first).
    open_and_run(&mut h, &path);

    let telemetry = h.state().scout_telemetry_for_test();
    h.state_mut().set_scout_enabled_for_test(false);
    h.state_mut()
        .set_widescreen_aspect_for_test(Aspect::Wide32x9);
    h.state_mut().set_widescreen(true);
    h.state_mut().set_measure_margins_for_test(true);
    h.state()
        .send_core_command_for_test(CoreCommand::ApplySnapshot(Box::new(corridor.clone())));
    run_n(&mut h, 40);
    probe(&h, "sm-corridor");
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
    let live_at_start = h.state().frame_count_for_test();
    h.state().send_core_command_for_test(CoreCommand::Pause);
    let mut finished = false;
    while t1.elapsed() < Duration::from_secs(300) {
        std::thread::sleep(Duration::from_millis(100));
        if telemetry.finished.load(Relaxed) + telemetry.gave_up.load(Relaxed) >= 1 {
            finished = telemetry.finished.load(Relaxed) >= 1;
            break;
        }
    }
    h.state().send_core_command_for_test(CoreCommand::Resume);
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(true));
    let t = h.state().frame_count_for_test() + 90;
    run_frames(&mut h, t, Duration::from_secs(60));
    let (Some(off), Some(on)) = (off, grab(&h)) else {
        skip(readme, name, "no frame");
        return;
    };
    let first_known = h.state().margin_known_for_test();
    println!(
        "SCOUT-SM: finished {finished} (scout started at live frame {live_at_start}); {} scout frames, {} cells merged; started {} finished {} aborted {}, passes ended {:?}; agreement {} over {} shared cells; gave up {}; {}; margins after the scout {first_known:?}",
        telemetry.frames.load(Relaxed),
        telemetry.cells.load(Relaxed),
        telemetry.started.load(Relaxed),
        telemetry.finished.load(Relaxed),
        telemetry.aborted.load(Relaxed),
        telemetry.last_ends.lock().map(|e| e.clone()).unwrap_or_default(),
        agreement_text(&telemetry),
        telemetry.shared_cells.load(Relaxed),
        telemetry.gave_up.load(Relaxed),
        telemetry.last_summary().unwrap_or_default(),
    );
    let d = w35_dir();
    save(&d.join("sm-32x9-off.png"), &off.opaque());
    save(&d.join("sm-32x9-on.png"), &on.opaque());
    note(readme, format!("sm-32x9|{file}|{}x{}", on.w, on.h));
    let first_started = telemetry.started.load(Relaxed);
    assert!(
        finished && telemetry.aborted.load(Relaxed) == 0 && first_started >= 1,
        "the corridor was not scouted from the saved table: finished {finished}"
    );
    // The core thread writes the scene as it stops; give it the time.
    h.state().send_core_command_for_test(CoreCommand::Shutdown);
    std::thread::sleep(Duration::from_secs(3));
    drop(h);

    // The second visit: a new app over the same cache and profile folder.
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    let telemetry = h.state().scout_telemetry_for_test();
    h.state_mut().set_scout_enabled_for_test(true);
    h.state_mut()
        .set_widescreen_aspect_for_test(Aspect::Wide32x9);
    h.state_mut().set_measure_margins_for_test(true);
    h.state_mut().set_widescreen(true);
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
    h.state()
        .send_core_command_for_test(CoreCommand::ApplySnapshot(Box::new(corridor)));
    // The snapshot sets the frame counter to its own: count from there.
    let base = corridor_frame;
    let mut samples = Vec::new();
    for _ in 0..240 {
        let t = h.state().frame_count_for_test() + 1;
        run_frames(&mut h, t, Duration::from_secs(10));
        if let Some((scene, k, t, cells)) = h.state().margin_known_for_test() {
            samples.push((h.state().frame_count_for_test() - base, scene, k, t, cells));
        }
    }
    let t = h.state().frame_count_for_test() + 600;
    run_frames(&mut h, t, Duration::from_secs(120));
    let peak = samples.iter().map(|s| s.4).max().unwrap_or(0);
    let target = peak - peak / 100;
    let full_at = samples.iter().find(|s| s.4 >= target).map(|s| s.0);
    let line = format!(
        "SM-PERSIST: first visit margins {first_known:?}; second visit: first sample {:?}, peak {peak} cells, full (99%) at live frame {full_at:?}; scouts started {} skipped {}",
        samples.first(),
        telemetry.started.load(Relaxed),
        telemetry.skipped.load(Relaxed),
    );
    println!("{line}");
    note(readme, line);
    assert_eq!(
        telemetry.started.load(Relaxed),
        0,
        "the second visit started a scout"
    );
    assert!(
        telemetry.skipped.load(Relaxed) >= 1,
        "the scout was not skipped"
    );
}

/// Ticket W35-04: A Link to the Past with NO hand-made `[scout]` table. A scripted session
/// outside Link's house (the door, the road, the trees) finds one; the app's own "Save scout settings" writes it
/// to the player's profile; the app, reading that profile, scouts the road at 32:9 (off,
/// then on, cold canvas cache); a second visit over the same cache must have the margins at once
/// and start no scout.
fn shot_alttp_scout(readme: &mut Vec<String>) {
    use egui_kittest::kittest::Queryable as _;
    use std::sync::atomic::Ordering::Relaxed;
    let name = "alttp-scout";
    if !wanted(name) {
        return;
    }
    let file = discovery::ALTTP.1;
    let Some(path) = rom("snes", file) else {
        skip(readme, name, &format!("{file} not found"));
        return;
    };
    let Some(mut rig) = discovery::Rig::open(discovery::ALTTP.0, discovery::ALTTP.1, None) else {
        skip(readme, name, "no rig");
        return;
    };
    rig.script(&discovery::alttp_to_free_play());
    let mut road = None;
    let mut road_frame = 0;
    let t0 = Instant::now();
    let skeleton = discovery::alttp_find_table(&mut rig, 600, &mut |r| {
        road = Some(r.st.snapshot().expect("snapshot"));
        road_frame = r.st.frame_count();
    });
    println!(
        "ALTTP-FIND: {:.1}s\n{}",
        t0.elapsed().as_secs_f32(),
        skeleton.to_toml()
    );
    let road = road.expect("the road's snapshot");
    drop(rig);

    let cold = std::env::temp_dir().join(format!("rf_scout_alttp_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cold);
    std::fs::create_dir_all(&cold).expect("cold cache dir");
    // SAFETY: the harness is built after this and nothing else in this process reads the
    // environment concurrently at this point.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("TMPDIR", &cold);
    }
    let config = PathBuf::from(
        std::env::var("RETROFORGE_CONFIG_DIR").expect("w30_w31_screens sets the config dir"),
    );
    let shipped = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../profiles/snes/the-legend-of-zelda-a-link-to-the-past/profile.toml");
    let before = std::fs::read(&shipped).expect("the shipped profile");
    let user =
        config.join("retroforge/profiles/snes/the-legend-of-zelda-a-link-to-the-past/profile.toml");
    let _ = std::fs::remove_file(&user);

    // The player's save: through the card, into the player's own profile folder.
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    h.state_mut().offer_scout_for_test((*skeleton).clone());
    run_n(&mut h, 3);
    h.get_by_label("Save scout settings").click();
    run_n(&mut h, 3);
    assert_eq!(
        std::fs::read(&shipped).expect("still there"),
        before,
        "the shipped profile was written to"
    );
    let saved = std::fs::read_to_string(&user).expect("the player's profile");
    let table = rf_profiles::load_str(&saved)
        .expect("the saved profile loads")
        .profile
        .scout
        .expect("a [scout] table");
    assert!(table.addr.iter().all(|r| r
        .source
        .as_deref()
        .is_some_and(|s| s.starts_with("observed:"))));
    println!(
        "ALTTP-SAVED: {} rows in {}",
        table.addr.len(),
        user.display()
    );
    // Read the profile again (the app picks the player's copy first).
    open_and_run(&mut h, &path);

    let telemetry = h.state().scout_telemetry_for_test();
    h.state_mut().set_scout_enabled_for_test(false);
    h.state_mut()
        .set_widescreen_aspect_for_test(Aspect::Wide32x9);
    h.state_mut().set_widescreen(true);
    h.state_mut().set_measure_margins_for_test(true);
    h.state()
        .send_core_command_for_test(CoreCommand::ApplySnapshot(Box::new(road.clone())));
    run_n(&mut h, 40);
    probe(&h, "alttp-road");
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
    let live_at_start = h.state().frame_count_for_test();
    h.state().send_core_command_for_test(CoreCommand::Pause);
    let mut finished = false;
    while t1.elapsed() < Duration::from_secs(300) {
        std::thread::sleep(Duration::from_millis(100));
        if telemetry.finished.load(Relaxed) + telemetry.gave_up.load(Relaxed) >= 1 {
            finished = telemetry.finished.load(Relaxed) >= 1;
            break;
        }
    }
    h.state().send_core_command_for_test(CoreCommand::Resume);
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(true));
    let t = h.state().frame_count_for_test() + 90;
    run_frames(&mut h, t, Duration::from_secs(60));
    let (Some(off), Some(on)) = (off, grab(&h)) else {
        skip(readme, name, "no frame");
        return;
    };
    let first_known = h.state().margin_known_for_test();
    println!(
        "SCOUT-ALTTP: finished {finished} (scout started at live frame {live_at_start}); {} scout frames, {} cells merged; started {} finished {} aborted {}, passes ended {:?}; agreement {} over {} shared cells; gave up {}; {}; margins after the scout {first_known:?}",
        telemetry.frames.load(Relaxed),
        telemetry.cells.load(Relaxed),
        telemetry.started.load(Relaxed),
        telemetry.finished.load(Relaxed),
        telemetry.aborted.load(Relaxed),
        telemetry.last_ends.lock().map(|e| e.clone()).unwrap_or_default(),
        agreement_text(&telemetry),
        telemetry.shared_cells.load(Relaxed),
        telemetry.gave_up.load(Relaxed),
        telemetry.last_summary().unwrap_or_default(),
    );
    let d = w35_dir();
    save(&d.join("alttp-32x9-off.png"), &off.opaque());
    save(&d.join("alttp-32x9-on.png"), &on.opaque());
    note(readme, format!("alttp-32x9|{file}|{}x{}", on.w, on.h));
    let first_started = telemetry.started.load(Relaxed);
    assert!(
        finished && telemetry.aborted.load(Relaxed) == 0 && first_started >= 1,
        "the road was not scouted from the saved table: finished {finished}"
    );
    // The core thread writes the scene as it stops; give it the time.
    h.state().send_core_command_for_test(CoreCommand::Shutdown);
    std::thread::sleep(Duration::from_secs(3));
    drop(h);

    // The second visit: a new app over the same cache and profile folder.
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    let telemetry = h.state().scout_telemetry_for_test();
    h.state_mut().set_scout_enabled_for_test(true);
    h.state_mut()
        .set_widescreen_aspect_for_test(Aspect::Wide32x9);
    h.state_mut().set_measure_margins_for_test(true);
    h.state_mut().set_widescreen(true);
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
    h.state()
        .send_core_command_for_test(CoreCommand::ApplySnapshot(Box::new(road)));
    // The snapshot sets the frame counter to its own: count from there.
    let base = road_frame;
    let mut samples = Vec::new();
    for _ in 0..240 {
        let t = h.state().frame_count_for_test() + 1;
        run_frames(&mut h, t, Duration::from_secs(10));
        if let Some((scene, k, t, cells)) = h.state().margin_known_for_test() {
            samples.push((h.state().frame_count_for_test() - base, scene, k, t, cells));
        }
    }
    let t = h.state().frame_count_for_test() + 600;
    run_frames(&mut h, t, Duration::from_secs(120));
    let peak = samples.iter().map(|s| s.4).max().unwrap_or(0);
    let target = peak - peak / 100;
    let full_at = samples.iter().find(|s| s.4 >= target).map(|s| s.0);
    let line = format!(
        "ALTTP-PERSIST: first visit margins {first_known:?}; second visit: first sample {:?}, peak {peak} cells, full (99%) at live frame {full_at:?}; scouts started {} skipped {}",
        samples.first(),
        telemetry.started.load(Relaxed),
        telemetry.skipped.load(Relaxed),
    );
    println!("{line}");
    note(readme, line);
    assert_eq!(
        telemetry.started.load(Relaxed),
        0,
        "the second visit started a scout"
    );
    assert!(
        telemetry.skipped.load(Relaxed) >= 1,
        "the scout was not skipped"
    );
}

/// Ticket W35-03: Castlevania (NES) with NO hand-made `[scout]` table. A scripted session in
/// stage 1 (the courtyard, its door, the entrance hall) finds one; the app's own "Save scout settings" writes it
/// to the player's profile; the app, reading that profile, scouts the midblock at 32:9 (off,
/// then on, cold canvas cache); a second visit over the same cache must have the margins at once
/// and start no scout.
fn shot_cv_scout(readme: &mut Vec<String>) {
    use egui_kittest::kittest::Queryable as _;
    use std::sync::atomic::Ordering::Relaxed;
    let name = "cv-scout";
    if !wanted(name) {
        return;
    }
    let file = discovery::CV.1;
    let Some(path) = rom("nes", file) else {
        skip(readme, name, &format!("{file} not found"));
        return;
    };
    let Some(mut rig) = discovery::Rig::open(discovery::CV.0, discovery::CV.1, None) else {
        skip(readme, name, "no rig");
        return;
    };
    rig.script(discovery::CV_TO_STAGE);
    rig.script("w60");
    let mut midblock = None;
    let mut midblock_frame = 0;
    let t0 = Instant::now();
    let skeleton = discovery::cv_find_table(&mut rig, 600, &mut |r| {
        // W35-08: two more screens of walking, so the snapshot is mid-level and has a left side.
        r.script("R60 R+A30 R60 R+A30");
        midblock = Some(r.st.snapshot().expect("snapshot"));
        midblock_frame = r.st.frame_count();
    });
    println!(
        "CV-FIND: {:.1}s\n{}",
        t0.elapsed().as_secs_f32(),
        skeleton.to_toml()
    );
    let midblock = midblock.expect("the midblock's snapshot");
    drop(rig);

    let cold = std::env::temp_dir().join(format!("rf_scout_cv_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cold);
    std::fs::create_dir_all(&cold).expect("cold cache dir");
    // SAFETY: the harness is built after this and nothing else in this process reads the
    // environment concurrently at this point.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("TMPDIR", &cold);
    }
    let config = PathBuf::from(
        std::env::var("RETROFORGE_CONFIG_DIR").expect("w30_w31_screens sets the config dir"),
    );
    let shipped =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/nes/castlevania/profile.toml");
    let before = std::fs::read(&shipped).expect("the shipped profile");
    let user = config.join("retroforge/profiles/nes/castlevania/profile.toml");
    let _ = std::fs::remove_file(&user);

    // The player's save: through the card, into the player's own profile folder.
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    h.state_mut().offer_scout_for_test((*skeleton).clone());
    run_n(&mut h, 3);
    h.get_by_label("Save scout settings").click();
    run_n(&mut h, 3);
    assert_eq!(
        std::fs::read(&shipped).expect("still there"),
        before,
        "the shipped profile was written to"
    );
    let saved = std::fs::read_to_string(&user).expect("the player's profile");
    let table = rf_profiles::load_str(&saved)
        .expect("the saved profile loads")
        .profile
        .scout
        .expect("a [scout] table");
    assert!(table.addr.iter().all(|r| r
        .source
        .as_deref()
        .is_some_and(|s| s.starts_with("observed:"))));
    println!("CV-SAVED: {} rows in {}", table.addr.len(), user.display());
    // Read the profile again (the app picks the player's copy first).
    open_and_run(&mut h, &path);

    let telemetry = h.state().scout_telemetry_for_test();
    h.state_mut().set_scout_enabled_for_test(false);
    h.state_mut()
        .set_widescreen_aspect_for_test(Aspect::Wide32x9);
    h.state_mut().set_widescreen(true);
    h.state_mut().set_measure_margins_for_test(true);
    h.state()
        .send_core_command_for_test(CoreCommand::ApplySnapshot(Box::new(midblock.clone())));
    run_n(&mut h, 40);
    probe(&h, "cv-midblock");
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
    let live_at_start = h.state().frame_count_for_test();
    h.state().send_core_command_for_test(CoreCommand::Pause);
    let mut finished = false;
    while t1.elapsed() < Duration::from_secs(300) {
        std::thread::sleep(Duration::from_millis(100));
        if telemetry.finished.load(Relaxed) + telemetry.gave_up.load(Relaxed) >= 1 {
            finished = telemetry.finished.load(Relaxed) >= 1;
            break;
        }
    }
    h.state().send_core_command_for_test(CoreCommand::Resume);
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(true));
    let t = h.state().frame_count_for_test() + 90;
    run_frames(&mut h, t, Duration::from_secs(60));
    let (Some(off), Some(on)) = (off, grab(&h)) else {
        skip(readme, name, "no frame");
        return;
    };
    let first_known = h.state().margin_known_for_test();
    println!(
        "SCOUT-Z1: finished {finished} (scout started at live frame {live_at_start}); {} scout frames, {} cells merged; started {} finished {} aborted {}, passes ended {:?}; agreement {} over {} shared cells; gave up {}; {}; margins after the scout {first_known:?}",
        telemetry.frames.load(Relaxed),
        telemetry.cells.load(Relaxed),
        telemetry.started.load(Relaxed),
        telemetry.finished.load(Relaxed),
        telemetry.aborted.load(Relaxed),
        telemetry.last_ends.lock().map(|e| e.clone()).unwrap_or_default(),
        agreement_text(&telemetry),
        telemetry.shared_cells.load(Relaxed),
        telemetry.gave_up.load(Relaxed),
        telemetry.last_summary().unwrap_or_default(),
    );
    let d = w35_dir();
    save(&d.join("cv1-32x9-off.png"), &off.opaque());
    save(&d.join("cv1-32x9-on.png"), &on.opaque());
    note(readme, format!("cv1-32x9|{file}|{}x{}", on.w, on.h));
    let first_started = telemetry.started.load(Relaxed);
    assert!(
        finished && telemetry.aborted.load(Relaxed) == 0 && first_started >= 1,
        "the midblock was not scouted from the saved table: finished {finished}"
    );
    // The core thread writes the scene as it stops; give it the time.
    h.state().send_core_command_for_test(CoreCommand::Shutdown);
    std::thread::sleep(Duration::from_secs(3));
    drop(h);

    // The second visit: a new app over the same cache and profile folder.
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    let telemetry = h.state().scout_telemetry_for_test();
    h.state_mut().set_scout_enabled_for_test(true);
    h.state_mut()
        .set_widescreen_aspect_for_test(Aspect::Wide32x9);
    h.state_mut().set_measure_margins_for_test(true);
    h.state_mut().set_widescreen(true);
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
    h.state()
        .send_core_command_for_test(CoreCommand::ApplySnapshot(Box::new(midblock)));
    // The snapshot sets the frame counter to its own: count from there.
    let base = midblock_frame;
    let mut samples = Vec::new();
    for _ in 0..240 {
        let t = h.state().frame_count_for_test() + 1;
        run_frames(&mut h, t, Duration::from_secs(10));
        if let Some((scene, k, t, cells)) = h.state().margin_known_for_test() {
            samples.push((h.state().frame_count_for_test() - base, scene, k, t, cells));
        }
    }
    let t = h.state().frame_count_for_test() + 600;
    run_frames(&mut h, t, Duration::from_secs(120));
    let peak = samples.iter().map(|s| s.4).max().unwrap_or(0);
    let target = peak - peak / 100;
    let full_at = samples.iter().find(|s| s.4 >= target).map(|s| s.0);
    let line = format!(
        "CV-PERSIST: first visit margins {first_known:?}; second visit: first sample {:?}, peak {peak} cells, full (99%) at live frame {full_at:?}; scouts started {} skipped {}",
        samples.first(),
        telemetry.started.load(Relaxed),
        telemetry.skipped.load(Relaxed),
    );
    println!("{line}");
    note(readme, line);
    assert_eq!(
        telemetry.started.load(Relaxed),
        0,
        "the second visit started a scout"
    );
    assert!(
        telemetry.skipped.load(Relaxed) >= 1,
        "the scout was not skipped"
    );
}

/// Ticket W35-08: Super Mario World from the MIDDLE of Yoshi's Island 1 (Mario ran right ~2
/// screens first), the shipped profile's table (`right`, `left`), cold canvas cache: 32:9 with the
/// scout off, then on. The canvas of the app starts empty at the snapshot, so the left margin is
/// what the scout's Left pass found.
fn shot_smw_left(readme: &mut Vec<String>) {
    use std::sync::atomic::Ordering::Relaxed;
    let name = "smw-left";
    if !wanted(name) {
        return;
    }
    let Some(path) = rom("snes", "Super Mario World (USA).zip") else {
        return skip(readme, name, "Super Mario World (USA) not found");
    };
    let Some(mut rig) = discovery::Rig::open(discovery::SMW.0, discovery::SMW.1, None) else {
        return skip(readme, name, "no rig");
    };
    rig.script(discovery::SMW_TO_LEVEL);
    rig.script("w60");
    rig.script(discovery::SMW_PLAY);
    let mid = rig.st.snapshot().expect("snapshot");
    drop(rig);
    let cold = std::env::temp_dir().join(format!("rf_scout_smwl_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cold);
    std::fs::create_dir_all(&cold).expect("cold cache dir");
    // SAFETY: the harness is built after this and nothing else in this process reads the
    // environment concurrently at this point.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("TMPDIR", &cold);
    }
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    let telemetry = h.state().scout_telemetry_for_test();
    h.state_mut().set_scout_enabled_for_test(false);
    h.state_mut()
        .set_widescreen_aspect_for_test(Aspect::Wide32x9);
    h.state_mut().set_widescreen(true);
    h.state_mut().set_measure_margins_for_test(true);
    h.state()
        .send_core_command_for_test(CoreCommand::ApplySnapshot(Box::new(mid)));
    run_n(&mut h, 40);
    probe(&h, "smw-midlevel");
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
        if telemetry.finished.load(Relaxed) + telemetry.gave_up.load(Relaxed) >= 1 {
            finished = telemetry.finished.load(Relaxed) >= 1;
            break;
        }
    }
    h.state().send_core_command_for_test(CoreCommand::Resume);
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(true));
    let t = h.state().frame_count_for_test() + 90;
    run_frames(&mut h, t, Duration::from_secs(60));
    let (Some(off), Some(on)) = (off, grab(&h)) else {
        return skip(readme, name, "no frame");
    };
    println!(
        "SCOUT-SMWL: finished {finished}; {} scout frames, {} cells merged; started {} finished {} aborted {}, passes ended {:?}; agreement {} over {} shared cells; margins {:?}",
        telemetry.frames.load(Relaxed),
        telemetry.cells.load(Relaxed),
        telemetry.started.load(Relaxed),
        telemetry.finished.load(Relaxed),
        telemetry.aborted.load(Relaxed),
        telemetry.last_ends.lock().map(|e| e.clone()).unwrap_or_default(),
        agreement_text(&telemetry),
        telemetry.shared_cells.load(Relaxed),
        h.state().margin_known_for_test(),
    );
    let d = w35_dir();
    save(&d.join("smw-left-32x9-off.png"), &off.opaque());
    save(&d.join("smw-left-32x9-on.png"), &on.opaque());
    note(
        readme,
        format!("smw-left-32x9|Super Mario World (USA)|{}x{}", on.w, on.h),
    );
}

/// Ticket W35-16: the script `script` cut into one-frame tokens, so a session can stop after `n`
/// frames of it.
fn frame_tokens(script: &str) -> Vec<String> {
    let mut out = Vec::new();
    for tok in script.split_whitespace() {
        let split = tok.find(|c: char| c.is_ascii_digit()).expect("N");
        let (b, n) = tok.split_at(split);
        for _ in 0..n.parse::<u32>().expect("N") {
            out.push(format!("{b}1"));
        }
    }
    out
}

/// Ticket W35-16: dots that differ between two same-size pictures, split into those inside the
/// column band `[lo, hi)` and those outside it.
fn diff_dots(a: &Pic, b: &Pic, lo: usize, hi: usize) -> (usize, usize) {
    let (mut inside, mut outside) = (0, 0);
    for i in 0..a.w * a.h {
        if a.rgba[i * 4..i * 4 + 3] != b.rgba[i * 4..i * 4 + 3] {
            if (lo..hi).contains(&(i % a.w)) {
                inside += 1;
            } else {
                outside += 1;
            }
        }
    }
    (inside, outside)
}

/// Ticket W35-16: one game at 32:9 from a saved moment, the same frame twice: "Objects past the edge" off,
/// then on (the snapshot is re-applied and the same number of frames stepped, so the only difference is
/// the toggle). Returns the pair and checks the contract: the 256 console dots never change, the dots
/// that do change are in the margins, and a second off render equals the first (no residue).
fn margin_sprites_pair(
    path: &Path,
    snap: &retroforge::stepper::MachineSnapshot,
    steps: usize,
) -> Option<(Pic, Pic, Pic)> {
    let mut h = new_harness();
    open_and_run(&mut h, path);
    h.state_mut().set_scout_enabled_for_test(false);
    h.state_mut()
        .set_widescreen_aspect_for_test(Aspect::Wide32x9);
    h.state_mut().set_widescreen(true);
    let take = |h: &mut H, on: bool| -> Option<Pic> {
        h.state_mut().set_margin_sprites_for_test(on);
        h.state().send_core_command_for_test(CoreCommand::Pause);
        run_n(h, 20);
        h.state()
            .send_core_command_for_test(CoreCommand::ApplySnapshot(Box::new(snap.clone())));
        run_n(h, 30);
        for _ in 0..steps {
            step_from(h);
        }
        println!(
            "MARGIN-SPRITES take on={on}: source {:?}, readout {:?}",
            h.state().widescreen_source_for_test(),
            h.state().margin_readout_for_test()
        );
        grab(h)
    };
    // Warm-up passes: the margins come from the stitched canvas only once the scene has settled and
    // the core's snapshot of it has arrived, and the canvas keeps filling for a while. Re-applying
    // the same moment keeps the scene; go on until two "off" passes in a row give the same picture
    // from the canvas, so the "on" pass differs by the toggle alone.
    let mut previous: Option<Pic> = None;
    let mut off = None;
    for _ in 0..10 {
        run_n(&mut h, 150);
        let pic = take(&mut h, false)?;
        let settled = h.state().widescreen_source_for_test() == MarginSource::StitchedCanvas
            && previous.as_ref().is_some_and(|p| p.rgba == pic.rgba);
        previous = Some(pic.clone());
        if settled {
            off = Some(pic);
            break;
        }
    }
    let off = off?;
    let on = take(&mut h, true)?;
    let off_again = take(&mut h, false)?;
    h.state().send_core_command_for_test(CoreCommand::Shutdown);
    Some((off, on, off_again))
}

/// Ticket W35-16: one game's "Objects past the edge" pair: the session `head` then the first `upto`
/// frames of `play` are played in the rig, the snapshot is taken there, and the app steps `steps` frames
/// from it with the toggle off and on. Shots `docs/screens/w35/margin-sprites-{tag}-{off,on}.png`.
fn shot_margin_sprites(
    readme: &mut Vec<String>,
    tag: &str,
    game: (&str, &str, &str),
    (head, play, upto, steps): (&str, &str, usize, usize),
) {
    let name = format!("margin-sprites-{tag}");
    if !wanted(&name) {
        return;
    }
    let Some(path) = rom(game.0, game.1) else {
        return skip(readme, &name, &format!("{} not found", game.1));
    };
    let Some(mut rig) = discovery::Rig::open(game.0, game.1, None) else {
        return skip(readme, &name, "no rig");
    };
    rig.script(head);
    for u in &frame_tokens(play)[..upto] {
        rig.script(u);
    }
    let snap = rig.st.snapshot().expect("snapshot");
    drop(rig);
    let Some((off, on, off_again)) = margin_sprites_pair(&path, &snap, steps) else {
        return skip(readme, &name, "no frame");
    };
    let centre = (off.w - 256) / 2;
    let (inside, outside) = diff_dots(&off, &on, centre, centre + 256);
    let (res_in, res_out) = diff_dots(&off, &off_again, 0, off.w);
    println!(
        "MARGIN-SPRITES {tag}: {}x{}; differing dots on vs off: {inside} in the 256 console dots, {outside} in the margins; off vs off-again: {}",
        on.w,
        on.h,
        res_in + res_out
    );
    save(&w35_dir().join(format!("{name}-off.png")), &off.opaque());
    save(&w35_dir().join(format!("{name}-on.png")), &on.opaque());
    note(readme, format!("{name}|{}|{}x{}", game.1, on.w, on.h));
    assert_eq!(inside, 0, "the console's own 256 dots changed");
    assert!(outside > 0, "nothing was painted in the margins");
    assert_eq!(res_in + res_out, 0, "switching back off left residue");
}

/// Ticket W35-16: Super Mario World's Banzai Bill coming in from the right. 686 frames into the play
/// script it is 4 frames from being 15 px over the right edge.
fn shot_margin_sprites_smw(readme: &mut Vec<String>) {
    shot_margin_sprites(
        readme,
        "smw",
        discovery::SMW,
        (
            &format!("{} w60", discovery::SMW_TO_LEVEL),
            discovery::SMW_PLAY,
            686,
            4,
        ),
    );
}

/// Ticket W35-16: A Link to the Past's soldier standing over the right edge (the game's own
/// sprites 16 px wide at X = 248), 3,905 frames into the road session.
fn shot_margin_sprites_alttp(readme: &mut Vec<String>) {
    shot_margin_sprites(
        readme,
        "alttp",
        discovery::ALTTP,
        (
            &discovery::alttp_to_free_play(),
            &discovery::alttp_play(),
            3905,
            8,
        ),
    );
}

/// Ticket W35-07: The Legend of Zelda (NES) with NO hand-made `[scout]` table. A scripted session
/// on the first overworld screen (over its edges and back) finds one; the app's own "Save scout
/// settings" writes it to the player's profile; the app, reading that profile, scouts the start
/// screen at 32:9 (off,
/// then on, cold canvas cache); a second visit over the same cache must have the margins at once
/// and start no scout.
fn shot_z1_scout(readme: &mut Vec<String>) {
    use egui_kittest::kittest::Queryable as _;
    use std::sync::atomic::Ordering::Relaxed;
    let name = "z1-scout";
    if !wanted(name) {
        return;
    }
    let file = discovery::ZELDA1.1;
    let Some(path) = rom("nes", file) else {
        skip(readme, name, &format!("{file} not found"));
        return;
    };
    let Some(mut rig) = discovery::Rig::open(discovery::ZELDA1.0, discovery::ZELDA1.1, None) else {
        skip(readme, name, "no rig");
        return;
    };
    rig.script(discovery::Z1_TO_PLAY);
    rig.script("w60");
    let t0 = Instant::now();
    let start_screen = rig.st.snapshot().expect("snapshot");
    let start_screen_frame = rig.st.frame_count();
    let skeleton = discovery::z1_find_table(&mut rig, 600);
    println!(
        "Z1-FIND: {:.1}s\n{}",
        t0.elapsed().as_secs_f32(),
        skeleton.to_toml()
    );
    drop(rig);

    let cold = std::env::temp_dir().join(format!("rf_scout_z1_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cold);
    std::fs::create_dir_all(&cold).expect("cold cache dir");
    // SAFETY: the harness is built after this and nothing else in this process reads the
    // environment concurrently at this point.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("TMPDIR", &cold);
    }
    let config = PathBuf::from(
        std::env::var("RETROFORGE_CONFIG_DIR").expect("w30_w31_screens sets the config dir"),
    );
    let shipped = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../profiles/nes/legend-of-zelda/profile.toml");
    let before = std::fs::read(&shipped).expect("the shipped profile");
    let user = config.join("retroforge/profiles/nes/legend-of-zelda/profile.toml");
    let _ = std::fs::remove_file(&user);

    // The player's save: through the card, into the player's own profile folder.
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    h.state_mut().offer_scout_for_test((*skeleton).clone());
    run_n(&mut h, 3);
    h.get_by_label("Save scout settings").click();
    run_n(&mut h, 3);
    assert_eq!(
        std::fs::read(&shipped).expect("still there"),
        before,
        "the shipped profile was written to"
    );
    let saved = std::fs::read_to_string(&user).expect("the player's profile");
    let table = rf_profiles::load_str(&saved)
        .expect("the saved profile loads")
        .profile
        .scout
        .expect("a [scout] table");
    assert!(table.addr.iter().all(|r| r
        .source
        .as_deref()
        .is_some_and(|s| s.starts_with("observed:"))));
    println!("Z1-SAVED: {} rows in {}", table.addr.len(), user.display());
    // Read the profile again (the app picks the player's copy first).
    open_and_run(&mut h, &path);

    let telemetry = h.state().scout_telemetry_for_test();
    h.state_mut().set_scout_enabled_for_test(false);
    h.state_mut()
        .set_widescreen_aspect_for_test(Aspect::Wide32x9);
    h.state_mut().set_widescreen(true);
    h.state_mut().set_measure_margins_for_test(true);
    h.state()
        .send_core_command_for_test(CoreCommand::ApplySnapshot(Box::new(start_screen.clone())));
    run_n(&mut h, 40);
    probe(&h, "z1-start");
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
    let live_at_start = h.state().frame_count_for_test();
    h.state().send_core_command_for_test(CoreCommand::Pause);
    let mut finished = false;
    while t1.elapsed() < Duration::from_secs(300) {
        std::thread::sleep(Duration::from_millis(100));
        if telemetry.finished.load(Relaxed) + telemetry.gave_up.load(Relaxed) >= 1 {
            finished = telemetry.finished.load(Relaxed) >= 1;
            break;
        }
    }
    h.state().send_core_command_for_test(CoreCommand::Resume);
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(true));
    let t = h.state().frame_count_for_test() + 90;
    run_frames(&mut h, t, Duration::from_secs(60));
    let (Some(off), Some(on)) = (off, grab(&h)) else {
        skip(readme, name, "no frame");
        return;
    };
    let first_known = h.state().margin_known_for_test();
    println!(
        "SCOUT-Z1: finished {finished} (scout started at live frame {live_at_start}); {} scout frames, {} cells merged; started {} finished {} aborted {}, passes ended {:?}; agreement {} over {} shared cells; gave up {}; {}; margins after the scout {first_known:?}",
        telemetry.frames.load(Relaxed),
        telemetry.cells.load(Relaxed),
        telemetry.started.load(Relaxed),
        telemetry.finished.load(Relaxed),
        telemetry.aborted.load(Relaxed),
        telemetry.last_ends.lock().map(|e| e.clone()).unwrap_or_default(),
        agreement_text(&telemetry),
        telemetry.shared_cells.load(Relaxed),
        telemetry.gave_up.load(Relaxed),
        telemetry.last_summary().unwrap_or_default(),
    );
    let d = w35_dir();
    save(&d.join("zelda1-32x9-off.png"), &off.opaque());
    save(&d.join("zelda1-32x9-on.png"), &on.opaque());
    note(readme, format!("zelda1-32x9|{file}|{}x{}", on.w, on.h));
    let first_started = telemetry.started.load(Relaxed);
    assert!(
        finished && telemetry.aborted.load(Relaxed) == 0 && first_started >= 1,
        "the start_screen was not scouted from the saved table: finished {finished}"
    );
    // The core thread writes the scene as it stops; give it the time.
    h.state().send_core_command_for_test(CoreCommand::Shutdown);
    std::thread::sleep(Duration::from_secs(3));
    drop(h);

    // The second visit: a new app over the same cache and profile folder.
    let mut h = new_harness();
    open_and_run(&mut h, &path);
    let telemetry = h.state().scout_telemetry_for_test();
    h.state_mut().set_scout_enabled_for_test(true);
    h.state_mut()
        .set_widescreen_aspect_for_test(Aspect::Wide32x9);
    h.state_mut().set_measure_margins_for_test(true);
    h.state_mut().set_widescreen(true);
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
    h.state()
        .send_core_command_for_test(CoreCommand::ApplySnapshot(Box::new(start_screen)));
    // The snapshot sets the frame counter to its own: count from there.
    let base = start_screen_frame;
    let mut samples = Vec::new();
    for _ in 0..240 {
        let t = h.state().frame_count_for_test() + 1;
        run_frames(&mut h, t, Duration::from_secs(10));
        if let Some((scene, k, t, cells)) = h.state().margin_known_for_test() {
            samples.push((h.state().frame_count_for_test() - base, scene, k, t, cells));
        }
    }
    let t = h.state().frame_count_for_test() + 600;
    run_frames(&mut h, t, Duration::from_secs(120));
    let peak = samples.iter().map(|s| s.4).max().unwrap_or(0);
    let target = peak - peak / 100;
    let full_at = samples.iter().find(|s| s.4 >= target).map(|s| s.0);
    let line = format!(
        "Z1-PERSIST: first visit margins {first_known:?}; second visit: first sample {:?}, peak {peak} cells, full (99%) at live frame {full_at:?}; scouts started {} skipped {}",
        samples.first(),
        telemetry.started.load(Relaxed),
        telemetry.skipped.load(Relaxed),
    );
    println!("{line}");
    note(readme, line);
    assert_eq!(
        telemetry.started.load(Relaxed),
        0,
        "the second visit started a scout"
    );
    assert!(
        telemetry.skipped.load(Relaxed) >= 1,
        "the scout was not skipped"
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
    shot_scout_mmx(&mut readme);
    shot_scout_mm2_climb(&mut readme);
    shot_sm_scout(&mut readme);
    shot_cv_scout(&mut readme);
    shot_smw_left(&mut readme);
    shot_margin_sprites_smw(&mut readme);
    shot_margin_sprites_alttp(&mut readme);
    shot_alttp_scout(&mut readme);
    shot_z1_scout(&mut readme);
    shot_persist(&mut readme);
    shot_confidence(&mut readme);
    shot_fps(&mut readme);
    shot_widesnes(&mut readme);
    shot_hd2d(&mut readme);
    shot_w34_baseline(&mut readme);
    shot_diorama_side_rfscroller(&mut readme);
    shot_diorama_side_smw(&mut readme);
    shot_diorama_side_smb3(&mut readme);
    shot_diorama_top_alttp(&mut readme);
    shot_diorama_layers_smw(&mut readme);
    shot_diorama_layers_smb3(&mut readme);
    shot_diorama_layers_alttp(&mut readme);
    shot_diorama_light_alttp(&mut readme);
    shot_diorama_light_smw(&mut readme);
    shot_diorama_level_smw(&mut readme);
    shot_diorama_level_smb3(&mut readme);
    shot_diorama_wide(&mut readme);
    shot_hd2d_full(&mut readme);
    shot_w34_tour(&mut readme);
    shot_w35_diorama(&mut readme);
    shot_w35_soak(&mut readme);
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
    if std::env::var("RF_SCREENS_ONLY").is_err() || wanted("scout-contact") {
        scout_contact_sheet(&mut readme);
    }
    println!("--- summary ---");
    for l in &readme {
        println!("{l}");
    }
}
