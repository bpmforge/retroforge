//! Ticket W7-12: the `PROF` chunk and criterion 3's refusal, through a
//! real `rf-state` container rather than a mock.

use retroforge::mod_chunk::{self, ModCheck, ProfChunk, ProfError};
use rf_enhance::mods::{ModEngine, Reconciliation};
use rf_profiles::schema::{ModPatch, Mods};
use rf_state::Container;

fn mods() -> Mods {
    Mods {
        patch: vec![
            ModPatch {
                id: "fast-text".to_string(),
                description: "one frame per glyph".to_string(),
                addr: 0x10,
                replace: vec![0x01],
            },
            ModPatch {
                id: "no-screen-shake".to_string(),
                description: "NOP the shake".to_string(),
                addr: 0x20,
                replace: vec![0xEA, 0xEA],
            },
        ],
    }
}

/// A container carrying the required core chunks as stubs.
///
/// `Container::decode` refuses a file missing any REQUIRED tag, so a
/// PROF-only container cannot survive a round trip through the encoder —
/// and these tests are about what survives a file, not what sits in
/// memory.
fn container() -> Container {
    let mut c = Container::new(0, [7u8; 32], "test", 1_700_000_000);
    for tag in [
        b"CPU_", b"PPU_", b"APU_", b"WRAM", b"VRAM", b"OAM_", b"CGRM", b"MAPR", b"CART",
    ] {
        let version = rf_state::tag_info(*tag)
            .expect("registry tag")
            .current_version;
        c.add_chunk(*tag, version, vec![0u8; 4])
            .expect("stub chunk");
    }
    c
}

/// Criterion 2: the state records which patches were active.
#[test]
fn a_state_records_the_mods_that_were_active_when_it_was_taken() {
    let mut engine = ModEngine::from_profile(Some(&mods()));
    engine.enable("fast-text", 5).unwrap();

    let mut c = container();
    mod_chunk::attach(&mut c, "rf-scroller", "W5-02c", &engine).unwrap();

    // Through the real encoder, so the chunk survives a file.
    let bytes = c.encode().unwrap();
    let (decoded, _) = Container::decode_default(&bytes).unwrap();
    let prof = mod_chunk::decode(&decoded.chunk(*b"PROF").unwrap().payload).unwrap();
    assert_eq!(
        prof,
        ProfChunk {
            profile_id: "rf-scroller".to_string(),
            revision: "W5-02c".to_string(),
            mods: vec!["fast-text".to_string()],
        }
    );
}

/// **Criterion 3.** Saved with a mod, loaded without it: refused, and the
/// diagnostic names the mod.
#[test]
fn a_state_saved_with_mods_is_refused_by_a_session_without_them() {
    let mut modded = ModEngine::from_profile(Some(&mods()));
    modded.enable("fast-text", 1).unwrap();
    let mut c = container();
    mod_chunk::attach(&mut c, "rf-scroller", "W5-02c", &modded).unwrap();

    let clean = ModEngine::from_profile(Some(&mods()));
    let check = mod_chunk::check(&c, &clean);
    assert!(
        !check.is_loadable(),
        "a state from a patched machine must not load silently into a clean one"
    );
    let msg = check.to_string();
    assert!(
        msg.contains("fast-text"),
        "the diagnostic must name the mod that differs: {msg}"
    );
}

/// And the reverse direction, which is the one most easily missed: an
/// UNMODDED state has no `PROF` chunk at all, and loading it into a
/// patched session is equally a mismatch.
#[test]
fn a_state_with_no_prof_chunk_is_refused_by_a_session_with_mods_on() {
    let c = container(); // no PROF attached at all
    let mut patched = ModEngine::from_profile(Some(&mods()));
    patched.enable("no-screen-shake", 1).unwrap();

    let check = mod_chunk::check(&c, &patched);
    assert!(
        !check.is_loadable(),
        "an unmodified state loaded into a patched session is still a mismatch"
    );
    match check {
        ModCheck::Mismatch(Reconciliation::Mismatch {
            only_in_state,
            only_active,
        }) => {
            assert!(only_in_state.is_empty());
            assert_eq!(only_active, vec!["no-screen-shake".to_string()]);
        }
        other => panic!("expected a mismatch naming the active mod, got {other}"),
    }
}

/// The ordinary case must stay ordinary: no mods anywhere, no ceremony.
#[test]
fn an_unmodified_state_loads_into_an_unmodified_session() {
    let c = container();
    let clean = ModEngine::from_profile(Some(&mods()));
    assert_eq!(mod_chunk::check(&c, &clean), ModCheck::NoModsEitherSide);
    assert!(mod_chunk::check(&c, &clean).is_loadable());

    // A PROF chunk with an empty list is also a match, and says something
    // stronger: this ran under a profile with nothing enabled.
    let mut c2 = container();
    mod_chunk::attach(&mut c2, "rf-scroller", "W5-02c", &clean).unwrap();
    assert_eq!(mod_chunk::check(&c2, &clean), ModCheck::Match);
}

/// Adopting is the explicit resolution, and it turns the refusal into a
/// match while leaving the transitions in the ledger.
#[test]
fn adopting_the_states_mods_resolves_the_mismatch_and_is_ledgered() {
    let mut modded = ModEngine::from_profile(Some(&mods()));
    modded.enable("fast-text", 1).unwrap();
    let mut c = container();
    mod_chunk::attach(&mut c, "rf-scroller", "W5-02c", &modded).unwrap();

    let mut session = ModEngine::from_profile(Some(&mods()));
    assert!(!mod_chunk::check(&c, &session).is_loadable());

    mod_chunk::adopt_state_mods(&c, &mut session, 99).unwrap();
    assert_eq!(mod_chunk::check(&c, &session), ModCheck::Match);
    assert!(
        session.ledger().iter().any(|l| l.frame == 99),
        "adopting must appear in the ledger as something that happened"
    );
    assert!(
        !session.is_accuracy_mode(),
        "and the session is no longer the reference"
    );
}

/// A corrupt chunk is reported, never treated as "no mods".
#[test]
fn an_unreadable_prof_chunk_is_not_silently_treated_as_unmodded() {
    let mut c = container();
    c.add_chunk(*b"PROF", 1, vec![0xFF, 0xFF, 0xFF, 0xFF, 0x00])
        .unwrap();
    let clean = ModEngine::from_profile(Some(&mods()));
    let check = mod_chunk::check(&c, &clean);
    assert!(matches!(check, ModCheck::Unreadable(_)), "{check}");
    assert!(!check.is_loadable());
}

/// Trailing bytes mean the writer and reader disagree, so the part that
/// parsed is not trustworthy either.
#[test]
fn a_payload_with_trailing_bytes_is_refused_rather_than_partly_believed() {
    let mut bytes = mod_chunk::encode(&ProfChunk {
        profile_id: "p".to_string(),
        revision: "r".to_string(),
        mods: vec!["fast-text".to_string()],
    });
    bytes.push(0);
    assert_eq!(mod_chunk::decode(&bytes), Err(ProfError::TrailingBytes(1)));
}

/// The wiring, end to end from a real profile on disk: `profiles/nes/
/// rf-scroller-demo` declares a `[[mods.patch]]`, and the engine must
/// offer it **off**.
///
/// A unit test over a hand-built `Mods` cannot see a break between the
/// TOML and the engine — this one can.
#[test]
fn a_real_profile_on_disk_offers_its_patch_switched_off() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/retroforge is two levels under the repo root");
    let path = root.join("profiles/nes/rf-scroller-demo/profile.toml");
    let outcome = rf_profiles::load_file(&path).expect("the demo profile loads");

    let engine = ModEngine::from_profile(outcome.profile.mods.as_ref());
    assert!(
        engine.declared().iter().any(|p| p.id == "no-hud"),
        "the profile's declared patch should reach the engine: {:?}",
        engine.declared().iter().map(|p| &p.id).collect::<Vec<_>>()
    );
    assert!(
        engine.enabled_ids().is_empty(),
        "FR-ENH-009: a profile may OFFER a patch, only a user may enable it"
    );
    assert!(engine.is_accuracy_mode());
}
