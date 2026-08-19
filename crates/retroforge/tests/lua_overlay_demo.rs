//! Ticket W5-07, and `docs/MVP.md`'s last enhancement-demo checkbox: a
//! Lua script draws a live player-position overlay **using
//! profile-published addresses**.
//!
//! ## The vacuity trap, stated because W4-04's stub is still the obvious
//! way to fail this
//!
//! `rf.mem.read_u8` returned a literal `0` until this ticket. A test
//! asserting "the script read a number" or "an overlay command was
//! produced" passes against that stub, because 0 is a plausible memory
//! value and a no-op `rf.gui` that appended nothing still leaves a script
//! that ran without error.
//!
//! So the assertions here are:
//! * the value read equals what the host **published**, not zero;
//! * the marker's position **changes as the player moves**, which no
//!   stub and no hardcoded constant can produce;
//! * and the script names `player_x` through `rf.profile.addr`, never an
//!   address literal — which is what "profile-published" means and what
//!   makes the script portable to any game whose profile labels a player
//!   position.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use retroforge::stepper::EmuStepper;
use rf_core_api::{CoreEvent, CoreSink, InputFrame, PpuPixel};
use rf_plugin_sdk::host::{Budget, ScriptHost, ScriptState};
use rf_plugin_sdk::manifest::Manifest;
use rf_plugin_sdk::sandbox::{MemoryWindow, OverlayCmd};

struct NullSink;
impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/retroforge is two levels under the repo root")
        .to_path_buf()
}

fn fixture() -> Option<Vec<u8>> {
    match std::fs::read(repo_root().join("fixtures/nes/rf-scroller/build/rf-scroller.nes")) {
        Ok(b) => Some(b),
        Err(_) => {
            assert!(
                std::env::var_os("CI").is_none(),
                "fixture ROM missing on CI: its build step failed and this suite would pass \
                 having asserted nothing"
            );
            eprintln!("SKIP: fixture ROM not built — run fixtures/nes/rf-scroller/build.sh");
            None
        }
    }
}

const MANIFEST: &str = r#"
[plugin]
id = "player-marker"
version = "0.1.0"
api = "0.1"
license = "MIT OR Apache-2.0"
provenance = "ticket W5-07 test fixture"
[capabilities]
read_memory = true
draw_overlay = true
frame_events = true
"#;

/// The SHIPPED example script, not an inline copy.
///
/// Using the real one is the point: MVP.md's checkbox is about what a
/// user finds in `plugins/examples/`, and a test that exercised its own
/// private script could pass while the shipped example stayed broken —
/// which is exactly the state W4-04 left it in (a hardcoded, wrong
/// address read at the wrong width).
fn shipped_script() -> String {
    std::fs::read_to_string(repo_root().join("plugins/examples/player-overlay/main.lua"))
        .expect("the shipped example must exist")
}

/// The labels a profile publishes, as the shell hands them over.
fn labels_from_profile() -> BTreeMap<String, u32> {
    let path = repo_root().join("profiles/nes/rf-scroller/profile.toml");
    let profile = rf_profiles::load_file(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .profile;
    profile
        .memory_map
        .iter()
        .map(|e| (e.label.clone(), e.addr))
        .collect()
}

/// Build a host with the profile's labels ALREADY published.
///
/// The shipped script resolves `rf.profile.addr` once at load, which is
/// the natural way to write it — so the labels have to be there before
/// `load`, not after. See `ScriptHost::load_with_bridge`.
fn host() -> ScriptHost {
    let manifest = Manifest::parse(MANIFEST).expect("manifest parses");
    let bridge = rf_plugin_sdk::sandbox::Bridge::default();
    bridge.publish(MemoryWindow::default(), labels_from_profile());
    ScriptHost::load_with_bridge(manifest, &shipped_script(), Budget::default(), bridge)
        .expect("script loads")
}

/// Publish a window covering PRG-RAM, where cc65 puts this game's
/// globals.
fn publish(host: &ScriptHost, stepper: &EmuStepper, labels: &BTreeMap<String, u32>) {
    const BASE: u32 = 0x6000;
    const LEN: usize = 0x2000;
    let bytes: Vec<u8> = (0..LEN)
        .map(|i| stepper.peek(u16::try_from(BASE as usize + i).unwrap()))
        .collect();
    host.bridge()
        .publish(MemoryWindow { base: BASE, bytes }, labels.clone());
}

fn run(stepper: &mut EmuStepper, frames: u64, buttons: u8) {
    stepper.resume();
    for _ in 0..frames {
        let mut input = InputFrame::empty();
        input.ports[0] = u16::from(buttons);
        stepper.latch_and_advance_frame(input, &mut NullSink);
    }
}

/// **The MVP checkbox, end to end.** The marker must follow the player.
#[test]
fn a_lua_script_draws_a_marker_that_follows_the_live_player() {
    let Some(rom) = fixture() else { return };
    let labels = labels_from_profile();
    assert!(
        labels.contains_key("player_x"),
        "the shipped profile must publish player_x, or the script has nothing to ask for"
    );

    let mut stepper = EmuStepper::from_ines_bytes(&rom).expect("fixture loads");
    let host_a = host();

    // Early: the player has barely moved.
    run(&mut stepper, 30, 0);
    publish(&host_a, &stepper, &labels);
    let mut h = host_a;
    h.on_frame(1);
    let early = h.bridge().take_overlay();

    // Later: hold Right and walk.
    run(&mut stepper, 600, 0x80);
    publish(&h, &stepper, &labels);
    h.on_frame(2);
    let late = h.bridge().take_overlay();

    assert_eq!(
        *h.state(),
        ScriptState::Running,
        "the script must not have faulted: {:?}",
        h.log.lines()
    );

    let rect_x = |cmds: &[OverlayCmd]| match cmds.first() {
        Some(OverlayCmd::Rect { x, .. }) => *x,
        other => panic!("expected a rect, got {other:?}"),
    };
    assert_eq!(early.len(), 1, "one marker per frame: {early:?}");
    assert_eq!(late.len(), 1);

    let (x0, x1) = (rect_x(&early), rect_x(&late));
    assert!(
        x1 > x0,
        "the marker did not move with the player ({x0} -> {x1}) — either the memory read is \
         still W4-04's stub zero, or the overlay is not being redrawn"
    );
    // ...and it must be the PLAYER's position, not an arbitrary moving
    // number: compare against the same address read directly.
    let live = u32::from(stepper.peek(0x6029)) | (u32::from(stepper.peek(0x602A)) << 8);
    assert_eq!(
        x1,
        i32::try_from(live).unwrap() - 4,
        "the marker is not at player_x (the script draws it offset by half its 8px width)"
    );
}

/// **The stub-detector.** `rf.mem.read_u8` returned a literal 0 until
/// this ticket, and 0 is a plausible memory value — so this asserts the
/// read returns what the host PUBLISHED, at a value no stub would.
#[test]
fn reads_return_published_memory_rather_than_the_old_stub_zero() {
    let manifest = Manifest::parse(MANIFEST).expect("manifest parses");
    let mut h = ScriptHost::load(
        manifest,
        r#"
        function on_frame(n)
          seen8 = rf.mem.read_u8(0x6100)
          seen16 = rf.mem.read_u16(0x6100)
          missing = rf.mem.read_u8(0x0000)
        end
        "#,
        Budget::default(),
    )
    .expect("loads");

    let mut bytes = vec![0u8; 0x2000];
    bytes[0x100] = 0xAB;
    bytes[0x101] = 0xCD;
    h.bridge().publish(
        MemoryWindow {
            base: 0x6000,
            bytes,
        },
        BTreeMap::new(),
    );
    h.on_frame(1);
    assert_eq!(*h.state(), ScriptState::Running, "{:?}", h.log.lines());

    // `eval_repl` returns a debug rendering of the Lua value, so the
    // expected strings carry the type — which is worth keeping rather
    // than stripping: a read that came back as a string or a float
    // instead of an integer would be a real API problem, and a bare
    // "171" comparison would hide it.
    assert_eq!(
        h.eval_repl("return seen8"),
        "Integer(171)",
        "0xAB, not W4-04's stub 0"
    );
    // Little-endian, as every profile's `u16` means.
    assert_eq!(h.eval_repl("return seen16"), "Integer(52651)", "0xCDAB");
    // Outside the published window reads 0 rather than faulting.
    assert_eq!(h.eval_repl("return missing"), "Integer(0)");
}

/// `rf.profile.addr` must resolve a real label and return nil for one it
/// does not know — a script that got a plausible-looking address for a
/// typo would read garbage and look like a game bug.
#[test]
fn profile_addr_resolves_published_labels_and_nil_for_unknown_ones() {
    let manifest = Manifest::parse(MANIFEST).expect("manifest parses");
    let bridge = rf_plugin_sdk::sandbox::Bridge::default();
    bridge.publish(MemoryWindow::default(), labels_from_profile());
    let mut h = ScriptHost::load_with_bridge(
        manifest,
        "function on_frame(n) known = rf.profile.addr('player_x'); unknown = rf.profile.addr('nope') end",
        Budget::default(),
        bridge,
    )
    .expect("loads");
    h.on_frame(1);
    assert_eq!(h.eval_repl("return known"), "Integer(24617)", "$6029");
    assert_eq!(
        h.eval_repl("return unknown"),
        "",
        "an unknown label must be nil — a plausible-looking address for a typo would have the \
         script read garbage and look like a game bug"
    );
}

/// The capability gate is W4-04's and must be unchanged: a denied
/// capability leaves the function ABSENT, not present-and-refusing.
#[test]
fn denying_read_memory_removes_the_whole_data_path() {
    let manifest = Manifest::parse(
        r#"
        [plugin]
        id = "no-read"
        version = "0.1.0"
        api = "0.1"
        license = "MIT OR Apache-2.0"
        provenance = "ticket W5-07 test fixture"
        [capabilities]
        draw_overlay = true
        "#,
    )
    .expect("manifest parses");
    let mut h = ScriptHost::load(
        manifest,
        "function on_frame(n) ok = (rf.mem == nil) and (rf.profile == nil) end",
        Budget::default(),
    )
    .expect("loads");
    h.on_frame(1);
    assert_eq!(*h.state(), ScriptState::Running, "{:?}", h.log.lines());
    assert_eq!(
        h.eval_repl("return ok"),
        "Boolean(true)",
        "rf.mem and rf.profile must be absent without read_memory, not present and refusing"
    );
}
