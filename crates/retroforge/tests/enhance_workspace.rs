//! **The Enhance workspace is §3.3's three tabs** (ticket W10-02;
//! `docs/design/FRONTEND_UI.md` §3.3).
//!
//! What shipped before W10-02 was the **Features** third of the spec:
//! Compare lived in a bottom-bar menu and Map existed nowhere at all.
//!
//! ## Why `open_tabs()` rather than querying tab titles
//!
//! A dock area renders only the *active* tab in each leaf, so asserting
//! on rendered titles would pass just as happily with two tabs stacked
//! invisibly behind the third — and would fail for a purely cosmetic
//! reason the moment someone rearranged the default split. The tree is
//! the thing being asserted, so the tree is what gets read.

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::enhance_dock::{EnhanceTab, EnhanceWorkspace};

/// §3.3's three tabs all exist, in the default layout, with no window.
#[test]
fn the_workspace_opens_with_all_three_tabs_of_the_spec() {
    let tabs = EnhanceWorkspace::new().open_tabs();
    for want in [EnhanceTab::Compare, EnhanceTab::Features, EnhanceTab::Map] {
        assert!(
            tabs.contains(&want),
            "§3.3 specifies Compare, Features and Map; {want:?} is missing. Present: {tabs:?}"
        );
    }
    assert_eq!(
        tabs.len(),
        3,
        "the default layout should open exactly the three tabs §3.3 names, not {tabs:?}"
    );
}

/// **Compare has exactly one route.** It used to be a bottom-bar menu;
/// §3.3 makes it a tab. Both would be two controls editing one piece of
/// state — the duplication W10-03 removed for the Library, in a smaller
/// place.
///
/// Asserted through the *menu*, because that is where the stale route
/// would survive: a leftover `Compare…` entry in the Enhance menu is
/// invisible until someone opens the menu and finds two of them.
#[test]
fn compare_is_not_also_a_menu_entry() {
    let dir = std::env::temp_dir().join(format!("retroforge_enhance_ws_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    // SAFETY: first statement of the only test that builds an app in this
    // binary, before any core thread or harness exists.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.run();

    harness
        .get_by_role_and_label(eframe::egui::accesskit::Role::Button, "Enhance")
        .click();
    harness.run_steps(2);

    let labels: Vec<String> = {
        use egui_kittest::kittest::NodeT as _;
        harness
            .root()
            .children_recursive()
            .filter_map(|n| {
                let a = n.accesskit_node();
                a.label().or_else(|| a.value())
            })
            .collect()
    };
    assert!(
        labels.iter().any(|l| l == "Enhance\u{2026}"),
        "precondition: the Enhance menu must be open. Tree: {labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l.starts_with("Compare")),
        "Compare is still reachable from the Enhance menu as well as from its §3.3 tab \u{2014} \
         two controls editing one piece of state. Tree: {labels:?}"
    );
    // The Heuristics menu is untouched by W10-02 and proves this test is
    // reading a populated menu rather than an empty one.
    assert!(
        labels.iter().any(|l| l.starts_with("Heuristics")),
        "the Enhance menu looks empty, so the assertion above proves nothing. Tree: {labels:?}"
    );
    assert!(
        harness.query_by_label("Quit").is_none(),
        "wrong menu is open"
    );
}
