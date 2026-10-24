//! Ticket W35-16: "Objects past the edge" at the core thread, on the repo's own SNES fixture (no
//! ROM needed). Off (never sent, or sent false) means no mask at all; on means a mask the size of the
//! widened frame; and the picture the core sends is byte-identical whichever it is (the paint is the
//! UI thread's, over a copy).

use std::time::Duration;

use retroforge::core_thread::{self, CoreCommand, CoreEvent, WidescreenRequest};

const SNES: &str = "../../fixtures/snes/rf-scroller-s/build/rf-scroller-s.sfc";
const WIDTH: usize = 400;

/// Six stepped frames at 400 dots wide; `toggle` is what is sent for the mask, if anything.
fn run(toggle: Option<bool>) -> Vec<(Vec<u8>, usize, usize, usize)> {
    let rom = std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(SNES))
        .expect("the SNES fixture");
    let core = core_thread::spawn(rom).expect("spawns");
    let policies = rf_enhance::widescreen::WidescreenPolicies {
        enabled: true,
        ..Default::default()
    };
    core.cmd_tx
        .send(CoreCommand::SetWidescreen(Some(WidescreenRequest {
            width: WIDTH,
            policies,
            convention: false,
        })))
        .expect("accepts");
    if let Some(on) = toggle {
        core.cmd_tx
            .send(CoreCommand::SetMarginSprites(on))
            .expect("accepts");
    }
    let mut frames = Vec::new();
    while frames.len() < 6 {
        core.cmd_tx.send(CoreCommand::StepFrame).expect("accepts");
        match core
            .evt_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("a frame")
        {
            CoreEvent::Frame(msg) => {
                // The core stops sending once two frames sit unconsumed (W14-20).
                core.pending_frames
                    .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                frames.push((
                    msg.rgba.clone(),
                    msg.margin_sprites.len(),
                    msg.width,
                    msg.height,
                ));
            }
            CoreEvent::Crashed(r) => panic!("crashed: {}", r.message),
            _ => {}
        }
    }
    let _ = core.cmd_tx.send(CoreCommand::Shutdown);
    let _ = core.join_handle.join();
    frames
}

#[test]
fn the_mask_exists_only_while_asked_for_and_never_changes_the_picture() {
    let untouched = run(None);
    let off = run(Some(false));
    let on = run(Some(true));
    for (_, _, w, _) in &untouched {
        assert_eq!(*w, WIDTH, "the core must be sending the widened frame");
    }
    assert!(
        untouched.iter().chain(&off).all(|f| f.1 == 0),
        "no mask while the toggle is off or was never touched"
    );
    // The first stepped frame may predate the command; every later one carries the mask.
    for (_, n, w, h) in on.iter().skip(1) {
        assert_eq!(*n, w * h, "one flag per dot of the widened frame");
    }
    for ((a, ..), ((b, ..), (c, ..))) in untouched.iter().zip(off.iter().zip(&on)) {
        assert_eq!(a, b, "off is the old picture");
        assert_eq!(a, c, "the mask never alters the core's own picture");
    }
}
