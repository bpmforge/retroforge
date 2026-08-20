//! Ticket W9-02: the profile editor driven through W4-09's headless egui
//! harness — the real widget code, the real accessibility tree, the real
//! loader, and a real file on disk.
//!
//! The unit tests in `retroforge::profile_editor` pin the rules. This pins
//! that the rules are actually *wired to the buttons*: a `save_to` that
//! refuses correctly is worth nothing if the panel's Save button calls
//! something else, and no unit test can see that.
//!
//! One `#[test]` in the binary on purpose. The env-var write below is only
//! sound while nothing else in the process has started, and a second test
//! running in parallel would be exactly that something.

use eframe::egui::accesskit::Role;
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;

const HARNESS_SIZE: (f32, f32) = (1600.0, 1000.0);

#[test]
fn a_profile_is_created_validated_and_saved_from_the_gui_alone() {
    let dir = std::env::temp_dir().join(format!("rf_profed_ui_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // SAFETY: the app reads `RETROFORGE_CONFIG_DIR` during construction
    // and there is no non-env way in; without it this test would read and
    // write the developer's real config directory. This is the first
    // statement of the only test in this binary — no app, no core thread,
    // no harness exists yet, so there is no concurrent reader to race.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }

    let mut harness = Harness::builder()
        .with_size(eframe::egui::Vec2::new(HARNESS_SIZE.0, HARNESS_SIZE.1))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();

    // ---- the editor opens with no ROM and no profile ---------------
    //
    // This is the case the panel used to `return` on. A profile editor
    // that needs a profile before it will open cannot create the first
    // one.
    harness.state_mut().open_author_editor();
    harness.run_steps(3);

    for control in ["Save profile", "Close editor", "Profile TOML"] {
        assert!(
            harness.query_by_label_contains(control).is_some(),
            "editor control `{control}` is missing from the accessibility tree"
        );
    }
    assert!(
        harness.query_by_label_contains("Valid").is_some(),
        "a freshly created draft should validate — the skeleton is the \
         one thing the author did not write"
    );

    // ---- an invalid edit is refused *inline*, before any save ------
    let path = dir.join("profile.toml");
    harness
        .state_mut()
        .set_author_save_path(path.display().to_string());
    {
        let draft = harness
            .state_mut()
            .author_draft_mut()
            .expect("the editor is open");
        // FR-PROF-003: a map row with no `source` citation.
        *draft.text_mut() +=
            "\n[[memory_map]]\naddr = 0x0300\nlen = 1\ntype = \"u8\"\nlabel = \"lives\"\n";
    }
    harness.run_steps(3);

    let shown = harness
        .query_by_label_contains("Invalid:")
        .expect("the refusal is not shown in the panel");
    let shown = shown.accesskit_node().label().unwrap_or_default();
    assert!(
        shown.contains("memory_map") && shown.contains("lives") && shown.contains("source"),
        "the inline diagnostic must be the loader's own, naming the table, \
         the row and the rule; got: {shown}"
    );

    // And Save is not offered at all while that is true — the criterion
    // is that the error is removed, not deferred to a failed write.
    let save = harness
        .get_by_role_and_label(Role::Button, "Save profile")
        .accesskit_node()
        .is_disabled();
    assert!(save, "Save is clickable on a draft the loader would refuse");
    assert!(
        !path.exists(),
        "nothing may reach disk while the draft is invalid"
    );

    // ---- fix it, and the same button writes a loadable profile -----
    {
        let draft = harness.state_mut().author_draft_mut().unwrap();
        *draft.text_mut() += "source = \"docs/design/GAME_PROFILES.md §2\"\n";
    }
    harness.run_steps(3);
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Save profile")
            .accesskit_node()
            .is_disabled(),
        "Save is still disabled after the draft was made valid"
    );
    harness
        .get_by_role_and_label(Role::Button, "Save profile")
        .click();
    harness.run_steps(3);

    let status = harness
        .state_mut()
        .author_editor_status()
        .expect("the panel reports what it did")
        .to_string();
    assert!(status.starts_with("saved"), "save reported: {status}");
    assert!(path.exists(), "the Save button wrote nothing");

    // Criterion 2, through the GUI rather than the API: what the button
    // wrote loads, and loads *without warnings*.
    let outcome =
        rf_profiles::loader::load_file(&path).expect("rf-profiles loads the GUI's output");
    assert!(
        outcome.warnings.is_empty(),
        "the GUI wrote a profile that warns: {:?}",
        outcome.warnings
    );
    assert_eq!(outcome.profile.memory_map.len(), 1);
    assert_eq!(outcome.profile.memory_map[0].label, "lives");

    let _ = std::fs::remove_dir_all(&dir);
}
