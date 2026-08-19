//! Ticket W4-11: the save-state manager's rules, and the modal asserted
//! through W4-09's headless egui harness rather than a manual checklist.
//!
//! Criterion 3 says so explicitly, and the ticket's own note says why:
//! without the harness "this ticket's only possible acceptance is a
//! manual checklist in a commit body, which is exactly what the split
//! exists to avoid".

use std::path::{Path, PathBuf};

use eframe::egui::accesskit::Role;
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;
use retroforge::state_slots::{self, ModeAtSave, SlotId, AUTO_SLOTS, NUMBERED_SLOTS};

const HARNESS_SIZE: (f32, f32) = (1600.0, 1000.0);

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rf_states_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A synthetic NROM, and a real container saved from it — built through
/// the shipped save path (`EmuStepper::save_state`), never hand-rolled,
/// so what the manager reads is what the app writes.
fn real_container(enhanced: bool) -> (Vec<u8>, String) {
    use retroforge::stepper::EmuStepper;
    let mut rom = vec![0u8; 16 + 0x4000 + 0x2000];
    rom[0..4].copy_from_slice(b"NES\x1a");
    rom[4] = 1;
    rom[5] = 1;
    rom[16 + 0x3FFC] = 0x00;
    rom[16 + 0x3FFD] = 0x80;
    let stepper = EmuStepper::from_ines_bytes(&rom).expect("synthetic rom loads");
    let mut container = stepper.save_state(1_700_000_000).expect("saves");
    if enhanced {
        // An Enhanced-mode save carries ENHC (and a modded one PROF) —
        // `rf_state::tags`' own doc contrasts exactly these two shapes.
        container
            .add_chunk(*b"ENHC", 1, vec![0u8; 4])
            .expect("optional chunk");
        container
            .add_chunk(*b"PROF", 1, vec![0u8; 4])
            .expect("optional chunk");
    }
    let hash = "a".repeat(64);
    (container.encode().expect("encodes"), hash)
}

/// **Mode and the mods flag are derived from the chunk list, and this is
/// the test that pins it.** A sidecar field would be a second source of
/// truth that can disagree with the state itself — and disagree in the
/// worst direction, since a state whose label says Accuracy but which
/// restores enhancement data carries a session out of the reference mode
/// (project law 6).
#[test]
fn mode_and_mods_flag_come_from_the_containers_own_chunks() {
    let dir = scratch("derive");
    let (plain, _) = real_container(false);
    let (enhanced, _) = real_container(true);
    state_slots::save(&dir, SlotId::Numbered(1), &plain, None).unwrap();
    state_slots::save(&dir, SlotId::Numbered(2), &enhanced, None).unwrap();

    let slots = state_slots::scan(&dir);
    let one = slots[0].saved.as_ref().expect("slot 1 occupied");
    let two = slots[1].saved.as_ref().expect("slot 2 occupied");

    assert_eq!(one.mode, ModeAtSave::Accuracy);
    assert!(!one.contains_mods, "a plain save carries no PROF chunk");
    assert_eq!(
        two.mode,
        ModeAtSave::Enhanced,
        "a container with ENHC was saved in Enhanced mode"
    );
    assert!(
        two.contains_mods,
        "FRONTEND_UI §3.2's 'contains mods' flag is the PROF chunk's presence"
    );
    assert_eq!(one.timestamp, 1_700_000_000, "timestamp is the header's");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every slot is listed, occupied or not — "which slots are free" is half
/// of what the user opened the manager to find out.
#[test]
fn every_slot_is_listed_including_the_empty_ones() {
    let dir = scratch("listing");
    let (bytes, _) = real_container(false);
    state_slots::save(&dir, SlotId::Auto(0), &bytes, None).unwrap();

    let slots = state_slots::scan(&dir);
    assert_eq!(
        slots.len() as u8,
        NUMBERED_SLOTS + AUTO_SLOTS,
        "FRONTEND_UI §3.2: 10 slots plus auto-slots"
    );
    assert_eq!(slots.iter().filter(|s| s.saved.is_some()).count(), 1);
    assert!(
        slots.iter().filter(|s| s.id.is_auto()).count() as u8 == AUTO_SLOTS,
        "the auto-slots must be distinguishable from the numbered ones"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A corrupt file must blank its own slot, not the other twelve.
#[test]
fn one_unreadable_slot_does_not_blank_the_rest() {
    let dir = scratch("corrupt");
    let (bytes, _) = real_container(false);
    state_slots::save(&dir, SlotId::Numbered(3), &bytes, None).unwrap();
    std::fs::write(dir.join("slot4.rfstate"), b"not a container at all").unwrap();

    let slots = state_slots::scan(&dir);
    assert!(slots[2].saved.is_some(), "slot 3 must still be readable");
    assert!(
        slots[3].saved.is_none(),
        "the corrupt slot 4 must read as empty rather than taking the listing down"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// FRONTEND_UI §3.2's last clause: "load warns on version-migrated
/// states." Asserted on the wording as well as the presence, because a
/// warning that does not say what changed is one the user cannot act on.
#[test]
fn a_migrated_load_produces_a_warning_that_names_the_chunk_and_versions() {
    use rf_state::LoadWarning;
    let lines = state_slots::warning_lines(&[
        LoadWarning::UnknownChunk { tag: *b"ENHC" },
        LoadWarning::Migrated {
            tag: *b"PPU_",
            from: 1,
            to: 2,
        },
    ]);
    assert_eq!(lines.len(), 2);
    // Migration warnings come FIRST — the one class §3.2 names by name.
    assert!(
        lines[0].contains("PPU_") && lines[0].contains("v1") && lines[0].contains("v2"),
        "the migration warning must name the chunk and both versions: {:?}",
        lines[0]
    );
    assert!(
        lines[0].to_lowercase().contains("older build"),
        "it must say WHY the state was migrated: {:?}",
        lines[0]
    );
    assert!(lines[1].contains("ENHC"));
}

/// The thumbnail must average, not sample. A nearest-neighbour pick drops
/// most of a 256-pixel frame and can make a whole feature vanish from its
/// own picture.
#[test]
fn thumbnails_average_rather_than_sample() {
    // A frame that is black everywhere except one 2x2 white block: a
    // sampler that happened to miss it returns pure black.
    let (w, h) = (256u32, 240u32);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 100..102u32 {
        for x in 100..102u32 {
            let i = ((y * w + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
    }
    let (thumb, tw, th) = state_slots::downscale(&rgba, w, h, state_slots::THUMB_WIDTH);
    assert_eq!(tw, 128, "256 / an integer scale of 2");
    assert_eq!(th, 120);
    assert_eq!(thumb.len() as u32, tw * th * 4);
    assert!(
        thumb.iter().any(|b| *b > 0),
        "the white block vanished from the thumbnail — the downscale is dropping pixels"
    );
    // ...and it must not smear across the whole image either.
    assert!(
        thumb.iter().filter(|b| **b > 0).count() < thumb.len() / 4,
        "the downscale brightened everything, which is not a box filter"
    );
}

/// Round-trip through the thread-boundary encoding — a save command
/// carries a stem, not a `SlotId`, and a mismatch there would write every
/// save into the wrong slot.
#[test]
fn slot_ids_survive_the_stem_round_trip_and_reject_nonsense() {
    for id in SlotId::all() {
        assert_eq!(SlotId::from_stem(&id.stem()), Some(id), "{id:?}");
    }
    assert_eq!(SlotId::from_stem("slot0"), None, "there is no slot 0");
    assert_eq!(SlotId::from_stem("slot11"), None, "only 10 numbered slots");
    assert_eq!(SlotId::from_stem("auto9"), None);
    assert_eq!(SlotId::from_stem("../../etc/passwd"), None);
}

/// **Criterion 3.** The modal opened and asserted through W4-09's
/// headless harness: every slot row present, and a saved slot showing its
/// mode badge and mods flag.
#[test]
fn the_modal_lists_every_slot_and_shows_a_saved_slots_mode_and_flags() {
    let dir = scratch("modal");
    // SAFETY: first statement of the only test in this binary that sets
    // it; nothing else in this process has started.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }

    let mut harness = Harness::builder()
        .with_size(eframe::egui::Vec2::new(HARNESS_SIZE.0, HARNESS_SIZE.1))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();

    harness.state_mut().open_states_modal();
    harness.run_steps(3);

    // Every slot has a row.
    for id in SlotId::all() {
        // `query_all`, not `query`: each row carries its label plus a
        // "Save <label>" button, so a single-match query would fail on
        // ambiguity rather than on absence and read as a missing row.
        assert!(
            harness.query_all_by_label_contains(&id.label()).count() > 0,
            "no row for {} — the modal must list empty slots too",
            id.label()
        );
    }

    // Now put a real Enhanced+modded state in slot 1 and reopen.
    let hash = "a".repeat(64);
    let states = state_slots::slots_dir(Path::new(&dir), &hash);
    let (bytes, _) = real_container(true);
    state_slots::save(&states, SlotId::Numbered(1), &bytes, None).unwrap();
    harness.state_mut().set_game_hash_for_test(Some(hash));
    harness.state_mut().open_states_modal();
    harness.run_steps(3);

    let labels: Vec<String> = harness
        .root()
        .children_recursive()
        .filter_map(|n| n.accesskit_node().label())
        .collect();
    assert!(
        labels.iter().any(|l| l == "Enhanced"),
        "the saved slot's mode-at-save badge is missing from: {labels:?}"
    );
    assert!(
        labels.iter().any(|l| l.contains("contains mods")),
        "the PROF-chunk warning flag is missing from: {labels:?}"
    );
    assert!(
        labels.iter().any(|l| l.contains("2023-11-14")),
        "the timestamp is missing from: {labels:?}"
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Load Slot 1")
            .is_some(),
        "an occupied slot must offer a Load button"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
