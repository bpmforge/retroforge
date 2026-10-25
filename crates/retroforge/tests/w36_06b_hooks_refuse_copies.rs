//! **W36-06b: a hook host that writes memory turns run-ahead (and the margin redraw) off, visibly.**
//!
//! Run-ahead's shadow machine, and the machine the margin redraw runs, are copies without the hook
//! host. A host holding `write_memory` changes the real machine in ways they cannot reproduce, so the
//! core thread refuses to run them and says so on every frame (`FrameMsg::copies_refused_by_hooks`,
//! which the UI puts on the badge). A read-only host leaves them on. Synthetic cartridge.

use std::time::Duration;

use retroforge::core_thread::{spawn, CoreCommand, CoreEvent, CoreHandle};
use retroforge::rebuild::HookSetup;
use rf_plugin_sdk::{Capabilities, Manifest};

fn rom() -> Vec<u8> {
    let mut rom = vec![0u8; 16 + 0x4000 + 0x2000];
    rom[0..4].copy_from_slice(b"NES\x1a");
    rom[4] = 1;
    rom[5] = 1;
    rom[16 + 0x3FFC] = 0x00;
    rom[16 + 0x3FFD] = 0x80;
    rom
}

fn setup(write: bool) -> Box<HookSetup> {
    Box::new(HookSetup {
        manifest: Manifest {
            id: "hooks".into(),
            version: "0.1.0".into(),
            api: "0.1".into(),
            license: "MIT".into(),
            provenance: "test".into(),
            capabilities: Capabilities {
                function_hooks: true,
                read_memory: true,
                write_memory: write,
                ..Capabilities::default()
            },
        },
        lua_source: String::new(),
        recipe: rf_rebuild::Recipe::default(),
    })
}

/// The flag on the next few running frames (it must hold on all of them after a short settle).
fn flags(core: &CoreHandle, n: usize) -> Vec<bool> {
    let mut out = Vec::new();
    while out.len() < n {
        if let CoreEvent::Frame(m) = core.evt_rx.recv_timeout(Duration::from_secs(30)).expect("a frame")
        {
            core.pending_frames
                .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            out.push(m.copies_refused_by_hooks);
        }
    }
    out
}

#[test]
fn a_write_capable_hook_host_refuses_the_copies_and_a_read_only_one_does_not() {
    let core = spawn(rom()).expect("spawns");
    core.cmd_tx.send(CoreCommand::SetRunAhead(1)).unwrap();
    core.cmd_tx.send(CoreCommand::Resume).unwrap();
    // No host: nothing refused.
    assert!(flags(&core, 5).iter().all(|f| !f));
    // A read-only host: run-ahead is allowed.
    core.cmd_tx.send(CoreCommand::SetHooks(Some(setup(false)))).unwrap();
    let _ = flags(&core, 3);
    assert!(flags(&core, 5).iter().all(|f| !f), "read-only hooks leave run-ahead on");
    // A host that holds write_memory: refused, on every frame once it is loaded.
    core.cmd_tx.send(CoreCommand::SetHooks(Some(setup(true)))).unwrap();
    let _ = flags(&core, 3);
    assert!(flags(&core, 5).iter().all(|f| *f), "write_memory refuses the copies");
    // Dropping the host brings run-ahead back.
    core.cmd_tx.send(CoreCommand::SetHooks(None)).unwrap();
    let _ = flags(&core, 3);
    assert!(flags(&core, 5).iter().all(|f| !f));
    core.cmd_tx.send(CoreCommand::Shutdown).unwrap();
    core.join_handle.join().unwrap();
}
