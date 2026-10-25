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
            ..ProfChunk::default()
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
        ..ProfChunk::default()
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

// ---- W36-06b: recipe mods and the hook ledger ride in PROF ------------------------------

fn session() -> mod_chunk::ModSession {
    use rf_rebuild::{HookPoints, PatchRow, RecipeFunction};
    mod_chunk::ModSession {
        mods: vec!["live-enemies".to_string()],
        recipe: rf_rebuild::Recipe {
            id: "super-mario-bros".to_string(),
            engine: "smb1".to_string(),
            functions: vec![RecipeFunction {
                name: "enemy_update".to_string(),
                shape_hash: 1,
                exact_hash: Some(0x6c18_ff48_7cf1_0e09),
                address_hint: None,
                hooks: HookPoints::default(),
                source: "test".to_string(),
            }],
            ram: Vec::new(),
            patches: vec![PatchRow {
                name: "cull-left".to_string(),
                mod_name: "live-enemies".to_string(),
                function: "enemy_update".to_string(),
                offset: 59,
                value: 0xff,
                source: "test".to_string(),
            }],
        },
    }
}

fn ledger() -> Vec<rf_plugin_sdk::LedgerEntry> {
    vec![rf_plugin_sdk::LedgerEntry {
        frame: 12,
        addr: 0x075a,
        value: 9,
        plugin: "lives-hook".to_string(),
    }]
}

#[test]
fn the_tail_round_trips_and_a_payload_without_one_is_still_v1() {
    let chunk = mod_chunk::session_chunk(Some(&session()), &ledger()).expect("a modded session");
    assert_eq!(chunk.mods, vec!["live-enemies".to_string()]);
    assert_eq!(chunk.patches.len(), 1);
    assert_eq!(chunk.patches[0].exact_hash, 0x6c18_ff48_7cf1_0e09);
    assert_eq!(
        mod_chunk::decode(&mod_chunk::encode(&chunk)),
        Ok(chunk.clone())
    );
    // Without patches or writes the payload is byte-for-byte the v1 one.
    let plain = ProfChunk {
        profile_id: "p".into(),
        revision: "r".into(),
        mods: vec!["m".into()],
        ..ProfChunk::default()
    };
    let v1 = mod_chunk::encode(&plain);
    assert_eq!(mod_chunk::decode(&v1), Ok(plain));
    assert_eq!(mod_chunk::ledger_of(&chunk)[0].addr, 0x075a);
    // A cut tail is refused, not half believed.
    let bytes = mod_chunk::encode(&chunk);
    assert!(matches!(
        mod_chunk::decode(&bytes[..bytes.len() - 3]),
        Err(ProfError::Truncated | ProfError::NotUtf8)
    ));
}

#[test]
fn an_unmodified_session_without_hook_writes_writes_no_chunk() {
    assert_eq!(mod_chunk::session_chunk(None, &[]), None);
    let empty = mod_chunk::ModSession::default();
    assert_eq!(mod_chunk::session_chunk(Some(&empty), &[]), None);
    // Hook writes alone are enough to stamp one.
    assert!(mod_chunk::session_chunk(None, &ledger()).is_some());
}

#[test]
fn loading_a_modded_state_into_an_unmodded_session_says_so() {
    let mut c = Container::new(0, [0; 32], "t", 0);
    let chunk = mod_chunk::session_chunk(Some(&session()), &ledger()).unwrap();
    mod_chunk::attach_chunk(&mut c, &chunk).unwrap();
    let lines = mod_chunk::mismatch_lines(&c, &[]);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("mod live-enemies"), "{}", lines[0]);
    assert!(lines[0].contains("1 hook write"), "{}", lines[0]);
    // The same mods on both sides: nothing to say. An unmodded state into a modded session: say so.
    assert!(mod_chunk::mismatch_lines(&c, &["live-enemies".to_string()]).is_empty());
    let plain = Container::new(0, [0; 32], "t", 0);
    assert!(mod_chunk::mismatch_lines(&plain, &[]).is_empty());
    assert_eq!(
        mod_chunk::mismatch_lines(&plain, &["live-enemies".to_string()]).len(),
        1
    );
}
