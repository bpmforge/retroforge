//! **The window is rendered, headlessly, and the pixels are checked**
//! (ticket W10-03).
//!
//! ## The gap this closes
//!
//! Three defects in this arc were visible in a screenshot and **nowhere
//! else**. `◆` in the profile chip was a tofu box; its fix, `◇`, is also
//! absent from the bundled font and shipped too; the healthy A/V dot
//! `●` was absent as well and could not appear in any screenshot,
//! because it only renders with an audio device open. None of it failed,
//! warned, or logged — a missing glyph draws a box and carries on, and
//! the accessibility tree reports the *correct* label either way.
//!
//! Everything else in this repo tests the widget tree: what exists, what
//! it is called, where its rectangle is. Nothing tested **what is
//! actually drawn**. `egui_kittest`'s `wgpu` feature renders the real
//! window to an image off-screen, so a machine with no display — or a
//! developer whose Mac just locked — can still look.
//!
//! ## What is asserted, and what is deliberately not
//!
//! Asserted: the frame renders at all, is the size asked for, and is
//! **not degenerate** — not one flat colour, which is what a window that
//! failed to lay anything out looks like, and what every "it rendered
//! fine" claim would otherwise rest on.
//!
//! **No golden images.** A committed reference PNG for a UI under active
//! design goes stale on every legitimate change, and a golden nobody
//! trusts gets `--force`d back into place until it asserts nothing.
//! These tests instead write their frames to `target/ui-renders/` so a
//! human can look at them, which is the part a diff cannot do.

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, MIN_WINDOW_SIZE, WINDOW_SIZE};

/// Distinct colours below which a frame is considered blank.
///
/// A window that laid nothing out is one flat fill; a real frame has
/// chrome, text antialiasing and two surfaces. Four is far under any
/// genuine frame and far over any degenerate one, so this catches
/// "rendered nothing" without failing on a design change.
const MIN_DISTINCT_COLOURS: usize = 4;

fn render(size: [f32; 2], setup: impl FnOnce(&mut RetroForgeApp)) -> image::RgbaImage {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(size[0], size[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    setup(harness.state_mut());
    harness.run();
    harness.run();
    harness.render().expect(
        "the app must render off-screen; without this nobody can see it on a \
                 machine with no display",
    )
}

fn assert_not_blank(img: &image::RgbaImage, size: [f32; 2], what: &str) {
    assert_eq!(
        (img.width(), img.height()),
        (size[0] as u32, size[1] as u32),
        "{what}: rendered at the wrong size"
    );
    let mut seen = std::collections::HashSet::new();
    for px in img.pixels() {
        seen.insert(px.0);
        if seen.len() >= MIN_DISTINCT_COLOURS {
            return;
        }
    }
    panic!(
        "{what}: the frame has only {} distinct colour(s) — it is blank. Every other test \
         in this crate would still pass on a window that drew nothing, because the widget \
         tree is built either way.",
        seen.len()
    );
}

/// Save for human review. Not an assertion — the point is that somebody
/// can *look*, which is what caught all three tofu boxes.
fn save(img: &image::RgbaImage, name: &str) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-renders");
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = img.save(dir.join(format!("{name}.png")));
    }
}

/// One test per binary: `RETROFORGE_CONFIG_DIR` is process-global and
/// building the app reads *and writes* the config root, so a second test
/// here would race — or run against the developer's real config.
#[test]
fn the_window_actually_draws_something_at_every_size_and_state() {
    let dir = std::env::temp_dir().join(format!("retroforge_renders_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }

    // Boot: no roots configured. The first thing any new user sees.
    let boot = render(WINDOW_SIZE, |_| {});
    assert_not_blank(&boot, WINDOW_SIZE, "boot, no ROM folders");
    save(&boot, "home-no-folders");

    // Populated: §3.1's list, which is what the home is FOR.
    let games = dir.join("games");
    std::fs::create_dir_all(&games).expect("create games root");
    for name in ["Alpha Quest.nes", "Beta Racer.nes"] {
        let mut data = vec![0u8; 16 + 0x4000 + 0x2000];
        data[0..4].copy_from_slice(b"NES\x1a");
        data[4] = 1;
        data[5] = 1;
        data[16 + 0x3FFD] = 0x80;
        std::fs::write(games.join(name), &data).expect("write fixture rom");
    }
    let roots = vec![games];
    let populated = render(WINDOW_SIZE, |app| {
        app.set_library_roots_for_test(roots.clone());
    });
    assert_not_blank(&populated, WINDOW_SIZE, "populated library home");
    save(&populated, "home-populated");

    // The smallest size a user can drag to. `main.rs` enforces this
    // floor, so it is reachable — and a home that collapses there is a
    // home that collapses in practice.
    let small = render(MIN_WINDOW_SIZE, |app| {
        app.set_library_roots_for_test(roots.clone());
    });
    assert_not_blank(&small, MIN_WINDOW_SIZE, "populated home at minimum size");
    save(&small, "home-minimum-size");
}
