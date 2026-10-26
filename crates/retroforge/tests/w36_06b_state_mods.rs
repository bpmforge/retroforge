//! **W36-06b: a state carries the mods that made its machine** (NON_GOALS 13).
//!
//! The live save path (`CoreCommand::SaveStateToSlot`, `EmuStepper::save_state_with_mods`) stamps the
//! session's recipe mods, their patch rows and the hook host's write ledger into the `PROF` chunk;
//! loading puts the ledger back and warns when the session lacks the mods. Synthetic cartridge, so
//! this runs everywhere.

use std::time::Duration;

use retroforge::core_thread::{spawn, CoreCommand, CoreEvent};
use retroforge::mod_chunk::{self, ModSession};
use retroforge::stepper::EmuStepper;
use rf_plugin_sdk::{Budget, Capabilities, HookHost, LedgerEntry, Manifest};
use rf_rebuild::{HookPoints, PatchRow, Recipe, RecipeFunction};
use rf_state::LoadWarning;

fn rom() -> Vec<u8> {
    let mut rom = vec![0u8; 16 + 0x4000 + 0x2000];
    rom[0..4].copy_from_slice(b"NES\x1a");
    rom[4] = 1;
    rom[5] = 1;
    rom[16 + 0x3FFC] = 0x00;
    rom[16 + 0x3FFD] = 0x80;
    rom
}

fn session() -> ModSession {
    ModSession {
        mods: vec!["live-enemies".to_string()],
        recipe: Recipe {
            id: "super-mario-bros".to_string(),
            engine: "smb1".to_string(),
            functions: vec![RecipeFunction {
                grade: rf_rebuild::Grade::Observed,
                entry: None,
                insn_count: None,
                mode: None,
                confidence: None,
                name: "enemy_update".to_string(),
                shape_hash: 1,
                exact_hash: Some(0x6c18_ff48_7cf1_0e09),
                address_hint: None,
                hooks: HookPoints::default(),
                source: "test".to_string(),
            }],
            ram: Vec::new(),
            patches: vec![PatchRow {
                grade: rf_rebuild::Grade::Observed,
                name: "cull-left".to_string(),
                mod_name: "live-enemies".to_string(),
                function: "enemy_update".to_string(),
                offset: 59,
                value: 0xff,
                source: "test".to_string(),
            }],
            ..Default::default()
        },
    }
}

fn host(write: bool) -> HookHost {
    HookHost::new(
        Manifest {
            id: "lives-hook".into(),
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
        Budget::default(),
    )
}

fn write_entry() -> LedgerEntry {
    LedgerEntry {
        frame: 12,
        addr: 0x075a,
        value: 9,
        plugin: "lives-hook".to_string(),
    }
}

#[test]
fn save_has_the_chunk_load_restores_the_ledger_and_an_unmodded_session_is_warned() {
    // A session that made one hook write (the host's ledger is what a run would have filled).
    let mut a = EmuStepper::from_ines_bytes(&rom()).expect("loads");
    let host_a = host(true);
    host_a.restore_ledger(vec![write_entry()]);
    a.set_hook_host(Some(host_a));
    let container = a
        .save_state_with_mods(7, Some(&session()))
        .expect("saves with the mod stamps");
    let chunk = container
        .chunk(*b"PROF")
        .expect("the PROF chunk is present");
    let prof = mod_chunk::decode(&chunk.payload).expect("decodes");
    assert_eq!(prof.mods, vec!["live-enemies".to_string()]);
    assert_eq!(prof.patches.len(), 1);
    assert_eq!(
        (
            prof.patches[0].offset,
            prof.patches[0].value,
            prof.patches[0].exact_hash
        ),
        (59, 0xff, 0x6c18_ff48_7cf1_0e09)
    );
    assert_eq!(prof.ledger.len(), 1);

    // Through the file format and back, with the migrations the app loads with.
    let bytes = container.encode().expect("encodes");
    let (decoded, _) =
        rf_state::Container::decode(&bytes, &retroforge::save_state::nes_migrations())
            .expect("decodes");

    // Loaded into a session with a hook host: the ledger is back and shown, nothing is skipped.
    let mut b = EmuStepper::from_ines_bytes(&rom()).expect("loads");
    let host_b = host(true);
    b.set_hook_host(Some(host_b.clone()));
    assert!(host_b.ledger_entries().is_empty());
    let warnings = b.load_state(&decoded).expect("loads");
    assert_eq!(host_b.ledger_entries(), vec![write_entry()]);
    assert!(
        host_b
            .panel_lines()
            .iter()
            .any(|l| l.contains("MOD write: lives-hook wrote 0x09 to 0x075a at frame 12")),
        "{:?}",
        host_b.panel_lines()
    );
    assert!(
        !warnings
            .iter()
            .any(|w| matches!(w, LoadWarning::UnknownChunk { tag } if tag == b"PROF")),
        "{warnings:?}"
    );
    assert!(mod_chunk::mismatch_lines(&decoded, &["live-enemies".to_string()]).is_empty());

    // Loaded into an unmodded session (no host, no mods): the existing skipped-chunk warning fires and
    // the new line says what is different.
    let mut c = EmuStepper::from_ines_bytes(&rom()).expect("loads");
    let warnings = c.load_state(&decoded).expect("loads");
    assert!(
        warnings
            .iter()
            .any(|w| matches!(w, LoadWarning::UnknownChunk { tag } if tag == b"PROF")),
        "{warnings:?}"
    );
    let lines = retroforge::state_slots::warning_lines(&warnings);
    assert!(lines.iter().any(|l| l.contains("PROF")), "{lines:?}");
    let lines = mod_chunk::mismatch_lines(&decoded, &[]);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("mod live-enemies") && lines[0].contains("1 hook write"));
}

#[test]
fn an_unmodified_session_without_hook_writes_saves_no_prof() {
    let a = EmuStepper::from_ines_bytes(&rom()).expect("loads");
    let plain = a.save_state(7).expect("saves");
    let with = a.save_state_with_mods(7, None).expect("saves");
    assert!(with.chunk(*b"PROF").is_none());
    assert_eq!(
        plain.encode().unwrap(),
        with.encode().unwrap(),
        "exactly the plain container"
    );
}

#[test]
fn the_live_save_path_writes_the_chunk_through_the_core_thread() {
    let dir = std::env::temp_dir().join(format!("rf_w36_06b_state_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let core = spawn(rom()).expect("spawns");
    core.cmd_tx
        .send(CoreCommand::SetModSession(Some(Box::new(session()))))
        .unwrap();
    core.cmd_tx.send(CoreCommand::StepFrame).unwrap();
    // The commands are handled in order; wait for the stepped frame, then save.
    loop {
        if let CoreEvent::Frame(_) = core
            .evt_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("a frame")
        {
            core.pending_frames
                .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            break;
        }
    }
    core.cmd_tx
        .send(CoreCommand::SaveStateToSlot {
            dir: dir.clone(),
            stem: "slot1".to_string(),
        })
        .unwrap();
    core.cmd_tx.send(CoreCommand::Shutdown).unwrap();
    core.join_handle.join().unwrap();
    let bytes = std::fs::read(dir.join("slot1.rfstate")).expect("the slot was written");
    let (c, _) = rf_state::Container::decode(&bytes, &retroforge::save_state::nes_migrations())
        .expect("decodes");
    let prof = mod_chunk::decode(&c.chunk(*b"PROF").expect("PROF on the live path").payload)
        .expect("decodes");
    assert_eq!(prof.mods, vec!["live-enemies".to_string()]);
    assert_eq!(prof.profile_id, "super-mario-bros");
    assert_eq!(prof.patches.len(), 1);

    // And the same core without a session writes none.
    let core = spawn(rom()).expect("spawns");
    core.cmd_tx.send(CoreCommand::StepFrame).unwrap();
    loop {
        if let CoreEvent::Frame(_) = core
            .evt_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("a frame")
        {
            core.pending_frames
                .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            break;
        }
    }
    core.cmd_tx
        .send(CoreCommand::SaveStateToSlot {
            dir: dir.clone(),
            stem: "slot2".to_string(),
        })
        .unwrap();
    core.cmd_tx.send(CoreCommand::Shutdown).unwrap();
    core.join_handle.join().unwrap();
    let bytes = std::fs::read(dir.join("slot2.rfstate")).expect("the slot was written");
    let (c, _) = rf_state::Container::decode(&bytes, &retroforge::save_state::nes_migrations())
        .expect("decodes");
    assert!(c.chunk(*b"PROF").is_none());
    let _ = std::fs::remove_dir_all(&dir);
}
