//! **W41-03: cheats reach the machine through the core thread** (`docs/design/CHEATS_ACHIEVEMENTS.md`
//! sections 2-3). Runs against our own CC0 RF-Scroller fixture; no commercial ROM.
//!
//! * a RAM cheat is written at the start of every frame, so the byte holds after every frame;
//! * a ROM override answers CPU reads (`peek` shows it) and honours its compare byte;
//! * cheats are mods: the `PROF` chunk names them and records their writes, and a session without
//!   them is told so;
//! * a ROM cheat pauses the Rebuild lab and says why.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

use retroforge::cheats::{mod_id, CheatSet, RamCheat};
use retroforge::core_thread::{spawn, CoreCommand, CoreEvent, CoreHandle, FrameMsg};
use retroforge::mod_chunk;
use rf_core_api::{RamWrite, RomOverride};

const NES: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";
// cc65's RAM (the heap, below the C stack at $07FF): the fixture never touches it.
const RAM_ADDR: u32 = 0x0600;
const ROM_ADDR: u32 = 0x8000;

fn rom() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(NES);
    std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {}: {e}", path.display()))
}

fn frame(core: &CoreHandle) -> Box<FrameMsg> {
    loop {
        if let CoreEvent::Frame(m) = core
            .evt_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("a frame")
        {
            core.pending_frames.fetch_sub(1, Ordering::AcqRel);
            return m;
        }
    }
}

fn step(core: &CoreHandle) -> Box<FrameMsg> {
    core.cmd_tx.send(CoreCommand::StepFrame).unwrap();
    frame(core)
}

fn ram_set(addr: u32, value: u8, compare: Option<u8>) -> CheatSet {
    let id = mod_id(&format!("{addr:04X}:{value:02X}"));
    CheatSet {
        ram: vec![RamCheat {
            id_index: 0,
            write: RamWrite {
                addr,
                value,
                compare,
            },
        }],
        ram_ids: vec![id.clone()],
        ids: vec![id],
        ..CheatSet::default()
    }
}

fn rom_set(o: RomOverride) -> CheatSet {
    CheatSet {
        rom: vec![o],
        ids: vec![mod_id("TESTROM")],
        ..CheatSet::default()
    }
}

fn byte(m: &FrameMsg, row: usize) -> u8 {
    m.items[row][0]
}

#[test]
fn a_ram_cheat_holds_its_value_every_frame_and_a_compare_gates_it() {
    let core = spawn(rom()).expect("spawns");
    core.cmd_tx
        .send(CoreCommand::WatchItems(vec![(RAM_ADDR, 1)]))
        .unwrap();
    // Two frames in: the fixture does not use this byte.
    step(&core);
    assert_ne!(byte(&step(&core), 0), 0x5A, "baseline");

    core.cmd_tx
        .send(CoreCommand::SetCheats(Box::new(ram_set(
            RAM_ADDR, 0x5A, None,
        ))))
        .unwrap();
    for n in 0..10 {
        assert_eq!(byte(&step(&core), 0), 0x5A, "frame {n} after the cheat");
    }
    // The game (here: the memory door) changes it; the next frame puts it back.
    core.cmd_tx
        .send(CoreCommand::PokeBus {
            addr: RAM_ADDR as u16,
            value: 0x11,
        })
        .unwrap();
    assert_eq!(byte(&step(&core), 0), 0x5A);

    // A compare byte that does not match leaves the byte alone.
    core.cmd_tx
        .send(CoreCommand::SetCheats(Box::new(ram_set(
            RAM_ADDR,
            0x99,
            Some(0x42),
        ))))
        .unwrap();
    assert_eq!(
        byte(&step(&core), 0),
        0x5A,
        "compare 0x42 != 0x5A: no write"
    );
    // One that matches does.
    core.cmd_tx
        .send(CoreCommand::SetCheats(Box::new(ram_set(
            RAM_ADDR,
            0x99,
            Some(0x5A),
        ))))
        .unwrap();
    assert_eq!(byte(&step(&core), 0), 0x99);

    // Clearing the set stops the writes.
    core.cmd_tx
        .send(CoreCommand::SetCheats(Box::default()))
        .unwrap();
    core.cmd_tx
        .send(CoreCommand::PokeBus {
            addr: RAM_ADDR as u16,
            value: 0x00,
        })
        .unwrap();
    assert_eq!(byte(&step(&core), 0), 0);
    core.cmd_tx.send(CoreCommand::Shutdown).unwrap();
    core.join_handle.join().unwrap();
}

#[test]
fn a_rom_override_is_what_the_cpu_and_peek_see_and_a_compare_gates_it() {
    let core = spawn(rom()).expect("spawns");
    core.cmd_tx
        .send(CoreCommand::WatchItems(vec![(ROM_ADDR, 1)]))
        .unwrap();
    let original = byte(&step(&core), 0);
    let patched = original ^ 0xFF;

    core.cmd_tx
        .send(CoreCommand::SetCheats(Box::new(rom_set(RomOverride {
            addr: ROM_ADDR,
            value: patched,
            compare: Some(original),
        }))))
        .unwrap();
    assert_eq!(
        byte(&step(&core), 0),
        patched,
        "compare matches the cartridge"
    );

    core.cmd_tx
        .send(CoreCommand::SetCheats(Box::new(rom_set(RomOverride {
            addr: ROM_ADDR,
            value: patched,
            compare: Some(original ^ 1),
        }))))
        .unwrap();
    assert_eq!(byte(&step(&core), 0), original, "compare does not match");

    core.cmd_tx
        .send(CoreCommand::SetCheats(Box::default()))
        .unwrap();
    assert_eq!(byte(&step(&core), 0), original, "cleared");
    core.cmd_tx.send(CoreCommand::Shutdown).unwrap();
    core.join_handle.join().unwrap();
}

#[test]
fn a_state_taken_with_cheats_names_them_and_a_session_without_them_is_told() {
    let dir = std::env::temp_dir().join(format!("rf_w41_03_state_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let core = spawn(rom()).expect("spawns");
    core.cmd_tx
        .send(CoreCommand::SetCheats(Box::new(ram_set(
            RAM_ADDR, 0x5A, None,
        ))))
        .unwrap();
    for _ in 0..3 {
        step(&core);
    }
    core.cmd_tx
        .send(CoreCommand::SaveStateToSlot {
            dir: dir.clone(),
            stem: "slot1".to_string(),
        })
        .unwrap();
    core.cmd_tx.send(CoreCommand::Shutdown).unwrap();
    core.join_handle.join().unwrap();

    // A session that never had a cheat writes no PROF at all.
    let core = spawn(rom()).expect("spawns");
    step(&core);
    core.cmd_tx
        .send(CoreCommand::SaveStateToSlot {
            dir: dir.clone(),
            stem: "slot2".to_string(),
        })
        .unwrap();
    core.cmd_tx.send(CoreCommand::Shutdown).unwrap();
    core.join_handle.join().unwrap();

    let decode = |stem: &str| {
        let bytes = std::fs::read(dir.join(format!("{stem}.rfstate"))).expect("slot written");
        rf_state::Container::decode(&bytes, &retroforge::save_state::nes_migrations())
            .expect("decodes")
            .0
    };
    let with = decode("slot1");
    let prof = mod_chunk::decode(&with.chunk(*b"PROF").expect("PROF").payload).unwrap();
    let id = mod_id("0600:5A");
    assert_eq!(prof.mods, vec![id.clone()]);
    assert!(
        !prof.ledger.is_empty()
            && prof
                .ledger
                .iter()
                .all(|w| w.plugin == id && w.addr == RAM_ADDR),
        "the write is ledgered: {:?}",
        prof.ledger
    );
    // A session with no cheats is told; one with the same cheat is not.
    assert_eq!(mod_chunk::mismatch_lines(&with, &[]).len(), 1);
    assert!(mod_chunk::mismatch_lines(&with, std::slice::from_ref(&id)).is_empty());
    // An unmodified state loaded into a session with the cheat is told too.
    let without = decode("slot2");
    assert!(without.chunk(*b"PROF").is_none());
    assert_eq!(
        mod_chunk::mismatch_lines(&without, std::slice::from_ref(&id)).len(),
        1
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_rom_cheat_pauses_the_rebuild_lab_and_says_why() {
    let core = spawn(rom()).expect("spawns");
    core.cmd_tx
        .send(CoreCommand::SetCheats(Box::new(rom_set(RomOverride {
            addr: ROM_ADDR,
            value: 0xEA,
            compare: None,
        }))))
        .unwrap();
    core.cmd_tx.send(CoreCommand::SetCoverage(true)).unwrap();
    core.cmd_tx.send(CoreCommand::SetRebuild(true)).unwrap();
    let mut said = None;
    for _ in 0..4 {
        let m = step(&core);
        assert!(m.coverage.is_none(), "no coverage is recorded");
        if let Some(status) = m.rebuild {
            said = Some(status);
        }
    }
    match said {
        Some(retroforge::rebuild::RebuildStatus::Failed(why)) => {
            assert_eq!(why, "Rebuild lab paused: cheats change the game's code");
        }
        other => panic!("expected the pause message, got {other:?}"),
    }
    core.cmd_tx.send(CoreCommand::Shutdown).unwrap();
    core.join_handle.join().unwrap();
}
