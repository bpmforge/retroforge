//! **The annotation → profile-export workflow reaches a user** (ticket
//! W13-02f; `docs/design/DEBUGGER.md` §4, bar B-0/B-6/B-7).
//!
//! W13-01's grading found this pipeline complete and **unreachable**:
//! `AnnotationStore`, `datacrystal::parse_tsv` and
//! `profile_export::export_skeleton` were called from
//! `crates/retroforge/tests/**` and from nowhere in `crates/retroforge/
//! src`. Unit tests could not have caught it — every piece worked, and
//! nothing assembled them. That is the same shape W11-05 had, so this test
//! has the same shape as `hdpack_reaches_the_app`: drive `RetroForgeApp`
//! and assert on what the *app* does.
//!
//! B-0 is the walkthrough this file performs: label an address, import
//! more labels from a table, export a profile skeleton, and have the
//! labels still be there after a restart.

use std::path::{Path, PathBuf};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use rf_debugger::layout::DebugTab;

const NES: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn open_app(rom: &Path) -> Harness<'static, RetroForgeApp> {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().open_rom_path(rom);
    harness.run_steps(2);
    harness
}

#[test]
fn a_user_can_label_import_export_and_come_back_to_it_tomorrow() {
    let dir = std::env::temp_dir().join(format!("retroforge_annot_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }

    let nes = Path::new(env!("CARGO_MANIFEST_DIR")).join(NES);
    assert!(nes.exists(), "NES fixture missing: {}", nes.display());
    let rom: PathBuf = dir.join("rf-scroller.nes");
    std::fs::copy(&nes, &rom).expect("copy fixture");

    let mut harness = open_app(&rom);

    // **Reachability, the half that reaches EXISTING users.** The tab is
    // in `default_layout` (pinned by a unit test in `rf_debugger::
    // layout`), but the debug dock persists its layout to a machine-global
    // temp file — `debug_dock::debug_layout_path`, which is
    // `std::env::temp_dir()`, NOT the config root — so anybody who has
    // opened the debug window has a saved layout that predates this
    // variant, and `RETROFORGE_CONFIG_DIR` does not isolate it. That is
    // exactly why the picker exists, and it is what this asserts: a tab
    // absent from a stale layout can still be opened.
    harness.state_mut().debug_open_tab(DebugTab::Annotations);
    assert!(
        harness
            .state()
            .debug_tabs_for_test()
            .contains(&DebugTab::Annotations),
        "the picker must be able to open a panel a saved layout omits"
    );

    // Opening the ROM gave the panel an identity to annotate against.
    {
        let panel = harness.state_mut().debug_annotations_mut();
        let identity = panel
            .identity
            .as_ref()
            .expect("an identified ROM must give the panel something to annotate");
        assert_eq!(identity.title, "rf-scroller");
        assert!(panel.store.is_empty(), "a fresh game starts unlabelled");
    }

    // 1. The user labels an address, through the same form the panel
    //    parses — including the source FR-DBG-005 requires.
    {
        let panel = harness.state_mut().debug_annotations_mut();
        panel.form.addr = "0086".to_string();
        panel.form.len = "2".to_string();
        panel.form.ty = "u16".to_string();
        panel.form.label = "player_x_screen".to_string();
        panel.form.source = "https://datacrystal.tcrf.net/wiki/Example".to_string();
        let parsed = panel.form.parse().expect("a fully filled form must parse");
        panel.store.add(parsed).expect("and must be accepted");
    }

    // 2. And imports more from a DataCrystal-style table.
    {
        let panel = harness.state_mut().debug_annotations_mut();
        // The header row is required and the column set is exactly five:
        // there is deliberately no slot for DataCrystal's prose
        // `Description` column (CONSTRAINTS §2).
        panel.import_text = concat!(
            "address\tsize\ttype\tlabel\tsource\n",
            "0090\t1\tu8\tplayer_state\thttps://example.test/ram-map\n"
        )
        .to_string();
        // The same call the panel's Import button makes.
        let rows = rf_debugger::datacrystal::parse_tsv(
            &panel.import_text,
            rf_debugger::annotation::AddressSpace::Ram,
        )
        .expect("a well-formed table must import");
        for row in rows {
            panel.store.add(row).expect("and its rows must be accepted");
        }
    }
    assert_eq!(
        harness.state_mut().debug_annotations_mut().store.len(),
        2,
        "one hand-authored label plus one imported row"
    );

    // 3. Save, through the app's own request path — not by calling the
    //    store module directly, because "the app persists it" is the
    //    claim under test.
    harness.state_mut().debug_annotations_mut().request =
        Some(retroforge::debug_dock::AnnotationRequest::Save);
    harness.run_steps(2);
    {
        let panel = harness.state_mut().debug_annotations_mut();
        assert!(
            panel.problem.is_none(),
            "saving must not fail: {:?}",
            panel.problem
        );
        assert!(!panel.dirty, "a successful save clears the dirty flag");
        assert!(
            panel
                .status
                .as_deref()
                .is_some_and(|s| s.starts_with("saved to")),
            "the panel must say where it saved: {:?}",
            panel.status
        );
    }

    // 4. Export a profile skeleton — it lands in the profile editor, and
    //    it carries the identity the export itself cannot know.
    harness.state_mut().debug_annotations_mut().request =
        Some(retroforge::debug_dock::AnnotationRequest::ExportSkeleton);
    harness.run_steps(2);
    let skeleton = harness
        .state_mut()
        .author_draft_mut()
        .expect("the export must open a draft in the profile editor")
        .text()
        .to_string();
    assert!(
        skeleton.contains("player_x_screen"),
        "the skeleton must carry the labels: {skeleton}"
    );
    assert!(
        skeleton.contains("[[memory_map]]"),
        "a RAM annotation exports as a memory_map row: {skeleton}"
    );
    assert!(
        skeleton.contains("[[identity]]"),
        "without an identity the profile could never match the game it came from: {skeleton}"
    );
    // And it is a profile the REAL loader accepts — the vacuity trap
    // W4-06b's criterion 2 names: an export test that only checks the
    // TOML parses proves nothing.
    rf_profiles::load_str(&skeleton).expect("the skeleton must load in the real validator");

    // 5. Tomorrow: a new app, the same ROM, the labels are still there.
    drop(harness);
    let harness = open_app(&rom);
    let mut harness = harness;
    let panel = harness.state_mut().debug_annotations_mut();
    assert!(
        panel
            .store
            .entries()
            .iter()
            .any(|a| a.label == "player_x_screen"),
        "annotations must survive a restart (bar B-7), got {:?}",
        panel.store.entries()
    );
    assert!(panel.problem.is_none(), "{:?}", panel.problem);

    let _ = std::fs::remove_dir_all(&dir);
}
