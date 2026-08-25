//! **The HUD fits the window the application actually opens** (ticket
//! W10-01).
//!
//! ## Why this file exists
//!
//! On 2026-08-25 the bottom bar needed **1539 px** of content and
//! `main.rs` opened the window at **768 px**. Eight controls sat entirely
//! past the right edge and could not be clicked at any point in a normal
//! session — Heuristics, Compare, Layers (debug), Controls…, Library…,
//! Settings…, Debug Viewers and Camera — which meant the Library,
//! Settings and Controls windows had **no reachable opener at all**. The
//! status label was last in the row, so the one widget whose whole job is
//! reporting what happened was the most reliably invisible thing in the
//! app.
//!
//! A 1645-test suite was green throughout. `tests/ui_smoke.rs` pins its
//! harness to 1600x1000 — 2.08x the real window — behind a comment
//! calling that size "load-bearing, not cosmetic", *because smaller sizes
//! clip*. The clipping had been noticed and worked around in the test
//! instead of fixed in the app, and no test anywhere exercised the size
//! that ships.
//!
//! ## The trap this file is built to avoid
//!
//! A test that hardcodes `768.0, 720.0` would keep passing after someone
//! changed the window, which is the same class of mistake all over again.
//! [`WINDOW_SIZE`] is one constant, `main.rs` passes it to
//! `ViewportBuilder::with_inner_size`, and this file measures against it.
//! There is no second copy of the number to drift.
//!
//! ## Two instruments, because one is not enough
//!
//! egui does not report overflow: a widget laid out past the panel edge
//! is simply drawn nowhere and receives no clicks. Two different
//! measurements are needed to see all of it.
//!
//! 1. **The accessibility tree** ([`clipped_nodes`]) — every widget still
//!    publishes an AccessKit node with a real bounding box, so an
//!    invisible button is nonetheless measurable. This is what caught the
//!    original eight.
//! 2. **Direct layout instrumentation**
//!    ([`RetroForgeApp::status_readouts_for_test`]) — because egui
//!    publishes nodes for the bar's *buttons* and **not for its plain
//!    labels**. Verified by dumping every node in the panel: only
//!    `Role::Button` appears. So instrument 1 is structurally blind to
//!    the FPS, A/V, profile-chip and status readouts — which is to say,
//!    blind to all four things FRONTEND_UI §3.2 actually specifies. A
//!    first draft of this file asserted on those labels through the tree
//!    and passed while the readouts were being drawn 132 px off the left
//!    edge of the window.

use std::path::{Path, PathBuf};

use eframe::egui;
use eframe::egui::accesskit::Role;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, MIN_WINDOW_SIZE, WINDOW_SIZE};

/// A labelled node whose right edge lies outside the viewport.
struct Clipped {
    label: String,
    x0: f32,
    x1: f32,
}

// Hand-written rather than derived so the coordinates are genuinely READ.
// A `#[derive(Debug)]` here leaves `x0`/`x1` dead as far as the compiler
// is concerned, and a failure message that prints "Clipped { .. }" without
// saying where the widget went is a failure message that sends the next
// reader back to the harness to find out.
impl std::fmt::Debug for Clipped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} at x {:.0}..{:.0}", self.label, self.x0, self.x1)
    }
}

/// Every labelled node not fully inside `0..width`, plus the rightmost
/// edge any node reaches (the number that says *how much* too wide the
/// chrome is).
///
/// **Both edges, deliberately.** The transport buttons are laid out
/// left-to-right and overflow off the RIGHT — that is the failure this
/// ticket fixed. But §3.2's four readouts are laid out right-to-left, and
/// a right-to-left group that runs out of room allocates at progressively
/// *smaller* x: its items keep an `x1` inside the viewport and slide off
/// the LEFT, under the transport buttons. A one-sided `x1 > width` test
/// would be structurally blind to every widget this ticket moved.
fn clipped_nodes(harness: &Harness<'_, RetroForgeApp>, width: f32) -> (Vec<Clipped>, f32) {
    let mut out = Vec::new();
    let mut rightmost: f32 = 0.0;
    for node in harness.root().children_recursive() {
        let accesskit = node.accesskit_node();
        let Some(bounds) = accesskit.bounding_box() else {
            continue;
        };
        rightmost = rightmost.max(bounds.x1 as f32);
        let Some(label) = accesskit.label() else {
            continue;
        };
        if label.trim().is_empty() {
            continue;
        }
        // One pixel of slack: a widget whose edge lands exactly on the
        // boundary is drawn, and rounding at that boundary is not a bug
        // worth failing a build over.
        // One pixel of slack at each edge, for the same reason.
        if bounds.x1 as f32 > width - 1.0 || (bounds.x0 as f32) < -1.0 {
            out.push(Clipped {
                label: label.to_string(),
                x0: bounds.x0 as f32,
                x1: bounds.x1 as f32,
            });
        }
    }
    (out, rightmost)
}

/// A minimal NROM image, built in-test: `roms/` is gitignored (NFR-006)
/// and no fixture ROM may enter git (law 5). Same image `ui_smoke.rs`
/// uses, for the same reasons.
fn write_fixture_rom(dir: &Path) -> PathBuf {
    let mut data = vec![0u8; 16 + 0x4000 + 0x2000];
    data[0..4].copy_from_slice(b"NES\x1a");
    data[4] = 1; // 16 KiB PRG
    data[5] = 1; // 8 KiB CHR
    let prg = &mut data[16..16 + 0x4000];
    let code: &[u8] = &[
        0xA9, 0x1E, 0x8D, 0x01, 0x20, // LDA #$1E ; STA $2001 (rendering on)
        0xEE, 0x00, 0x00, // INC $0000
        0x4C, 0x05, 0x80, // JMP $8005
    ];
    prg[..code.len()].copy_from_slice(code);
    prg[0x3FFC] = 0x00;
    prg[0x3FFD] = 0x80;
    let path = dir.join("fixture.nes");
    std::fs::write(&path, &data).expect("write fixture rom");
    path
}

fn app_at(size: [f32; 2]) -> Harness<'static, RetroForgeApp> {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(size[0], size[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    // Two passes: egui sizes many widgets from the previous frame's
    // galley, so a single run can report a layout that never appears on
    // screen.
    harness.run();
    harness.run();
    harness
}

/// The §3.2 readouts must not reach back over the transport controls.
///
/// The failure this catches cannot be seen in the accessibility tree at
/// all. In a right-to-left layout an unbounded string claims its space
/// first and pushes everything after it to *smaller* x — with no
/// truncation and a real ROM path, the readouts started at **x = -132**,
/// entirely outside a 768 px window, while every AccessKit assertion in
/// this file stayed green.
fn assert_readouts_fit(harness: &Harness<'_, RetroForgeApp>, width: f32, what: &str) {
    let (rect, left_edge) = harness
        .state()
        .status_readouts_for_test()
        .unwrap_or_else(|| panic!("{what}: the status bar did not lay out at all"));
    assert!(
        rect.left() >= left_edge - 1.0,
        "{what}: the status readouts start at x={:.0} but the transport controls end at \
         x={left_edge:.0} — they are drawn on top of each other",
        rect.left(),
    );
    assert!(
        rect.left() >= -1.0 && rect.right() <= width + 1.0,
        "{what}: the status readouts occupy x {:.0}..{:.0}, outside a {width}px window",
        rect.left(),
        rect.right(),
    );
}

fn assert_bar_fits(harness: &Harness<'_, RetroForgeApp>, width: f32, what: &str) {
    assert_readouts_fit(harness, width, what);
    let (clipped, rightmost) = clipped_nodes(harness, width);
    assert!(
        clipped.is_empty(),
        "{what}: {} control(s) are laid out outside a {width}px window and cannot be \
         clicked (off the right edge, or pushed off the left by a full right-to-left \
         group). Widest node ends at x={rightmost:.0}. Clipped: {clipped:#?}",
        clipped.len(),
    );
}

/// **The whole measurement, in one test on purpose.**
///
/// `RETROFORGE_CONFIG_DIR` is process-global, and building
/// `RetroForgeApp` reads *and writes* the config root — so a second test
/// in this binary setting it while this one reads it is a race that
/// would surface as a rare confusing failure, or, worse, would let a
/// test run against the developer's real config directory. One test, one
/// config root, no ordering assumptions. `ui_smoke.rs` reached the same
/// conclusion and says so in the same words.
#[test]
fn the_hud_fits_the_window_the_app_actually_opens() {
    let dir = std::env::temp_dir().join(format!("retroforge_hud_fits_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    // SAFETY: Rust 2024 makes `set_var` unsafe because another thread may
    // be reading the environment concurrently. This is the first
    // statement of the only test in this binary, before any app, core
    // thread or harness exists, so there is no concurrent reader.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }

    // ---- 1. the size that ships ------------------------------------
    //
    // The assertion whose absence let eight unreachable controls ship.
    // It measures `WINDOW_SIZE`, the same constant `main.rs` passes to
    // `ViewportBuilder::with_inner_size`, so shrinking the window without
    // shrinking the chrome turns this red. A test with its own copy of
    // `768.0` would keep passing — the same mistake one level up.
    let mut harness = app_at(WINDOW_SIZE);
    assert_bar_fits(&harness, WINDOW_SIZE[0], "shipping window size, no ROM");

    // ---- 2. the status text is laid out, and last -----------------
    //
    // It used to be last in a left-to-right row of sixteen widgets, so it
    // was the first thing to disappear. It is now the last item placed in
    // a right-to-left group, which means it is the LEFTMOST readout and
    // lives on whatever space the four fixed §3.2 readouts leave — the
    // priority order §3.2 implies, and the opposite of what shipped
    // before. Asserted through the layout instrument, because a plain
    // label publishes no accessibility node here at all.
    assert!(
        !harness.state().status().is_empty(),
        "precondition: the app boots with a status message"
    );

    // ---- 3. the bar in its WIDEST state ----------------------------
    //
    // The load-bearing case, and the one an empty app cannot reach.
    // Before a ROM is open, `position` is `None` (no frame/scanline
    // readout and no separator), `fps` is `None`, `audio_fill` is `None`,
    // and the profile chip renders its short "no profile" branch — every
    // state that ADDS width to the bar is absent. Asserting only the
    // boot layout would be the original bug moved up a level: green
    // because the case that clips is never exercised.
    let rom = write_fixture_rom(&dir);
    harness.state_mut().open_rom_path(&rom);
    harness.run_steps(2);
    harness.get_by_label("Run").click();
    // Run until a frame lands, so `position` and eventually `fps` are
    // populated — bounded, because a hang here is a denial of service,
    // not a failing test (project law 8).
    for _ in 0..600 {
        harness.run_steps(1);
        if harness.state().has_presented_frame() {
            break;
        }
    }
    assert!(
        harness.state().has_presented_frame(),
        "no frame reached the screen, so the wide-bar case below is vacuous"
    );
    harness.run_steps(2);
    assert_bar_fits(
        &harness,
        WINDOW_SIZE[0],
        "shipping window size, ROM running",
    );

    // ---- 3b. a ROM at a DEEP path ----------------------------------
    //
    // The status line is `format!("Loaded {}", path.display())`, so its
    // width is decided by the user's directory depth, not by this
    // codebase. A fixture called `fixture.nes` two levels down would keep
    // this green forever while a real library — `~/Games/Consoles/NES/
    // Licensed/...` — re-created W10-01's exact failure at run time. This
    // is the case that makes `app::elide_front` load-bearing rather than
    // decorative.
    let deep = dir.join("a-fairly-long-directory-name/and-another-one-here/plus-a-third-level");
    std::fs::create_dir_all(&deep).expect("create deep dir");
    let deep_rom = write_fixture_rom(&deep);
    assert!(
        format!("Loaded {}", deep_rom.display()).len() > 100,
        "precondition: the deep path must actually produce a long status line"
    );
    harness.state_mut().open_rom_path(&deep_rom);
    harness.run_steps(3);
    assert_bar_fits(
        &harness,
        WINDOW_SIZE[0],
        "shipping window size, deep ROM path",
    );

    // ---- 3c. a panel window must not cover the menu bar ------------
    //
    // The Controls window is two players x every NES button, each with a
    // Clear: **1105 px of content in a 720 px window**. egui cannot
    // honour a position for a window that does not fit, so it pinned the
    // window to y=0 — on top of File / View / Enhance — and the menus
    // were unclickable for as long as it was open. Since the View menu
    // is now the only way to reach Library, Settings and Debug Viewers,
    // that made the app navigationally dead, which is W10-01's original
    // complaint wearing a different hat.
    //
    // Asserted at the SHIPPING height, because that is where a tall
    // window overflows; at `ui_smoke.rs`'s 1000 px harness the Controls
    // window fits and this failure does not occur at all.
    let mut small_win = app_at(WINDOW_SIZE);
    small_win.state_mut().show_controls_for_test(true);
    small_win.run_steps(3);
    let menu_bottom = small_win
        .get_by_role_and_label(Role::Button, "View")
        .rect()
        .bottom();
    for node in small_win.root().children_recursive() {
        let accesskit = node.accesskit_node();
        if accesskit.role() != Role::Window {
            continue;
        }
        let (Some(label), Some(bounds)) = (accesskit.label(), accesskit.bounding_box()) else {
            continue;
        };
        assert!(
            bounds.y0 as f32 >= menu_bottom,
            "the `{label}` window starts at y={:.0}, above the menu bar's bottom edge at \
             y={menu_bottom:.0} — it is covering the only navigation the app has",
            bounds.y0,
        );
    }

    // ---- 4. the smallest size a user can drag to -------------------
    //
    // `main.rs` sets `with_min_inner_size(MIN_WINDOW_SIZE)`, so this is a
    // reachable state, not a hypothetical one — and the floor is only
    // honest if the chrome survives it.
    let small = app_at(MIN_WINDOW_SIZE);
    assert_bar_fits(&small, MIN_WINDOW_SIZE[0], "minimum window size");

    // ---- 5. the measurement can fail -------------------------------
    //
    // Guards the vacuity that would make everything above meaningless:
    // if `clipped_nodes` returned empty because the app rendered no
    // labelled nodes at all — the exact trap `ui_smoke.rs`'s module doc
    // records — every assertion here would pass on a blank window.
    let narrow = [WINDOW_SIZE[0] / 4.0, WINDOW_SIZE[1]];
    let harness = app_at(narrow);
    let (clipped, _) = clipped_nodes(&harness, narrow[0]);
    let (rect, left_edge) = harness
        .state()
        .status_readouts_for_test()
        .expect("the status bar must lay out even when squeezed");
    assert!(
        rect.left() < left_edge,
        "at {}px the readouts still fit beside the transport controls — the layout \
         instrument is measuring nothing, so section 2's claim proves nothing",
        narrow[0],
    );
    assert!(
        !clipped.is_empty(),
        "at {}px nothing was reported clipped — the detector is measuring nothing, so \
         everything above proves nothing",
        narrow[0],
    );
}
