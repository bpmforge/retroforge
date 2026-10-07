//! **Settings speaks to players** (ticket W20-08; `docs/design/UX_WAVE_20.md`
//! §2 principle 9).
//!
//! Until W20-08 player-facing Settings copy cited ticket ids ("Shader
//! names arrive with W3-02a") and module paths ("crate::accessibility's
//! module doc"). This opens every tab of the real window and reads every
//! label and value the accessibility tree publishes.

use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

fn texts(harness: &Harness<'_, RetroForgeApp>) -> Vec<String> {
    harness
        .root()
        .children_recursive()
        .filter_map(|n| {
            let a = n.accesskit_node();
            a.label().or_else(|| a.value())
        })
        .collect()
}

fn looks_internal(text: &str) -> bool {
    let bytes = text.as_bytes();
    // W<digits>-<digits>: a ticket id.
    let ticket = (0..bytes.len()).any(|i| {
        bytes[i] == b'W' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) && {
            let mut j = i + 1;
            while bytes.get(j).is_some_and(u8::is_ascii_digit) {
                j += 1;
            }
            bytes.get(j) == Some(&b'-') && bytes.get(j + 1).is_some_and(u8::is_ascii_digit)
        }
    });
    ticket || text.contains("crate::") || text.contains("rf_")
}

#[test]
fn no_settings_tab_shows_a_ticket_id_or_module_path() {
    assert!(looks_internal("arrive with W3-02a"), "the check must bite");
    assert!(looks_internal("see crate::accessibility"));
    assert!(!looks_internal("Wide 4:3"));

    let dir = std::env::temp_dir().join(format!("retroforge_settings_copy_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let mut harness = Harness::builder()
        .with_size(eframe::egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1] + 400.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().show_settings_for_test(true);
    harness.run_steps(2);

    let mut seen = 0;
    let index = retroforge::app::settings_index_for_test();
    for tab in ["Video", "Audio", "Paths", "Accessibility"] {
        harness.get_by_label(tab).click();
        harness.run_steps(3);
        let all = texts(&harness);
        seen += all.len();
        // Ticket W21-06: every setting the search can jump to is on its
        // tab ("Apply to" only exists with a game open; MetalFX only in a
        // metalfx build).
        for (label, _) in index
            .iter()
            .filter(|(l, t)| *t == tab && !matches!(*l, "Apply to" | "MetalFX"))
        {
            assert!(
                all.iter()
                    .any(|t| t.to_lowercase().contains(&label.to_lowercase())),
                "search names {label:?} on {tab}, but {tab} does not draw it: {all:?}"
            );
        }
        let bad: Vec<&String> = all.iter().filter(|t| looks_internal(t)).collect();
        assert!(
            bad.is_empty(),
            "Settings › {tab} shows internal references: {bad:?}"
        );
    }
    assert!(
        seen > 40,
        "the walk read almost nothing ({seen} texts) — it is not looking"
    );

    // Ticket W21-06: searching finds a setting on another tab and jumps
    // there.
    harness.get_by_label("Accessibility").click();
    harness.run_steps(2);
    harness
        .get_by_role(eframe::egui::accesskit::Role::TextInput)
        .click();
    harness.run_steps(1);
    harness
        .get_by_role(eframe::egui::accesskit::Role::TextInput)
        .type_text("crt");
    harness.run_steps(2);
    harness.get_by_label("Shader  \u{b7}  Video").click();
    harness.run_steps(2);
    assert!(
        texts(&harness).iter().any(|t| t == "Shader"),
        "the search result opened the Video tab"
    );

    // The Light theme is selectable (W20-08) and the Audio device is a
    // list, not a text box.
    harness.get_by_label("Accessibility").click();
    harness.run_steps(2);
    assert!(
        harness.query_by_label("Light").is_some(),
        "Theme: Light must be offered"
    );
    harness.get_by_label("Audio").click();
    harness.run_steps(2);
    let audio = texts(&harness);
    assert!(
        audio.iter().any(|t| t.contains("System default")),
        "the audio device control must offer System default: {audio:?}"
    );
}
