//! **W42-05: player surfaces speak player words.** Opens every Quick Menu section with a game
//! loaded, then the library view, and reads every label and value the accessibility tree
//! publishes. None may show a ticket id, a decision id, a `$` RAM address, a module path or the
//! code mode name "Accuracy" (players see "Original").
//!
//! One test function: `RETROFORGE_CONFIG_DIR` is process-global. The rules are a copy of
//! `settings_copy.rs`'s `looks_internal`, plus the W42-05 additions.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::NodeT as _;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::quick_menu::Section;

const NES: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn run_frames(h: &mut Harness<'_, RetroForgeApp>, target: u64) {
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < Duration::from_secs(60) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

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
    let boundary = |i: usize| i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
    let digits_at = |mut j: usize| {
        let start = j;
        while bytes.get(j).is_some_and(u8::is_ascii_digit) {
            j += 1;
        }
        (j, j - start)
    };
    // W<digits>-<digits>: a ticket id.
    let ticket = (0..bytes.len()).any(|i| {
        bytes[i] == b'W' && boundary(i) && {
            let (j, n) = digits_at(i + 1);
            n > 0 && bytes.get(j) == Some(&b'-') && digits_at(j + 1).1 > 0
        }
    });
    // D-<3 digits>: a decision id.
    let decision = (0..bytes.len()).any(|i| {
        bytes[i] == b'D'
            && boundary(i)
            && bytes.get(i + 1) == Some(&b'-')
            && digits_at(i + 2).1 >= 3
    });
    // $ followed by 4+ hex digits: a RAM address.
    let address = (0..bytes.len()).any(|i| {
        bytes[i] == b'$'
            && bytes[i + 1..]
                .iter()
                .take_while(|c| c.is_ascii_hexdigit())
                .count()
                >= 4
    });
    // The code mode name; players see "Original".
    let accuracy = text
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w == "Accuracy");
    ticket || decision || address || accuracy || text.contains("crate::") || text.contains("rf_")
}

#[test]
fn quick_menu_sections_and_library_show_no_internal_references() {
    assert!(looks_internal("arrive with W3-02a"));
    assert!(looks_internal("Wired in W42-05."));
    assert!(looks_internal("Trust ladder (D-004)"));
    assert!(looks_internal("camera ($7E0004)"));
    assert!(looks_internal("Accuracy"));
    assert!(looks_internal("see crate::accessibility"));
    assert!(!looks_internal("Wide 4:3"));
    assert!(!looks_internal("Original"));
    assert!(!looks_internal("Costs $5 and 100 W"));
    assert!(!looks_internal("Inaccuracy"), "only the whole word");
    assert!(!looks_internal("D-pad"));

    let dir = std::env::temp_dir().join(format!("retroforge_w42_05_copy_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let rom = Path::new(env!("CARGO_MANIFEST_DIR")).join(NES);
    let mut h = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1] + 400.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run_steps(2);
    h.state_mut().launch_rom(&rom);
    run_frames(&mut h, 10);

    let mut seen = 0;
    for section in Section::ALL {
        h.state_mut().open_quick_menu_for_test(section);
        h.run_steps(4);
        let all = texts(&h);
        seen += all.len();
        let bad: Vec<&String> = all.iter().filter(|t| looks_internal(t)).collect();
        assert!(
            bad.is_empty(),
            "Quick Menu › {section:?} shows internal references: {bad:?}"
        );
    }
    assert!(seen > 60, "the walk read almost nothing ({seen} texts)");

    // The library view (no game).
    h.key_press(egui::Key::Escape);
    h.run_steps(4);
    h.state_mut().close_rom_for_test();
    h.run_steps(4);
    let library = texts(&h);
    assert!(!library.is_empty(), "the library drew nothing");
    let bad: Vec<&String> = library.iter().filter(|t| looks_internal(t)).collect();
    assert!(bad.is_empty(), "Library shows internal references: {bad:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
