//! Ticket W15-07 acceptance 5: a kittest smoke proving the embedded fonts
//! actually loaded and the app still renders its home screen with them.
//!
//! Two things checked, together, because either one passing alone would
//! be the vacuity trap `tests/ui_smoke.rs`'s own module doc warns about:
//!
//! 1. `FontFamily::Name("display")` exists in the live `egui::Context` —
//!    proof `theme::install_fonts` actually ran and registered it, not
//!    merely that the crate compiles a function with that name.
//! 2. The library (the app's home screen, ticket W10-03) still renders a
//!    real node after that font swap — an `include_bytes!` pointed at a
//!    corrupt or mismatched font file panics inside egui's text layout the
//!    first time anything is drawn with it (the `Heading` style uses
//!    `Name("display")` — `app.rs`'s `apply_theme`), so a clean render is
//!    itself the check that the font FILE, not just the family name, is
//!    good.

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

#[test]
fn embedded_fonts_load_and_the_library_still_renders() {
    let dir = std::env::temp_dir().join(format!("retroforge_theme_fonts_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    // SAFETY: first statement of the only test in this binary, before any
    // app or harness exists — no concurrent reader (same reasoning as
    // every other single-test file that sets this: `ui_smoke.rs`,
    // `library_home.rs`).
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.run();

    let families = harness.ctx.fonts(|f| f.families());
    assert!(
        families.contains(&egui::FontFamily::Name("display".into())),
        "theme::install_fonts must register a Name(\"display\") family; got {families:?}"
    );

    // Ticket W21-01: the condensed title face is registered and lays
    // out, and readouts are tabular — "1111" and "8888" lay out to the
    // same width in the face `theme::numeric` names (Plex Sans's default
    // figures), while letters do not.
    assert!(
        families.contains(&egui::FontFamily::Name(
            retroforge::theme::CONDENSED_FAMILY.into()
        )),
        "theme::install_fonts must register the condensed family; got {families:?}"
    );
    let width = |text: &str, font: egui::FontId| {
        harness.ctx.fonts_mut(|f| {
            f.layout_no_wrap(text.to_owned(), font, egui::Color32::WHITE)
                .size()
                .x
        })
    };
    let num = retroforge::theme::numeric(retroforge::theme::type_scale::CAPTION);
    assert!(width("1111", num.clone()) > 0.0);
    assert_eq!(width("1111", num.clone()), width("8888", num.clone()));
    assert_ne!(
        width("iiii", num.clone()),
        width("WWWW", num),
        "self-test: the face is proportional for letters, so the digit equality is a real check"
    );
    let title = retroforge::theme::condensed(retroforge::theme::type_scale::TITLE);
    let semibold = egui::FontId::new(
        retroforge::theme::type_scale::TITLE,
        egui::FontFamily::Name("display".into()),
    );
    assert!(
        width("Save state", title) < width("Save state", semibold),
        "the condensed face must be narrower than the SemiBold display face"
    );

    // The library is the home screen with zero clicks (ticket W10-03) —
    // a fresh config dir with no folders configured renders the first of
    // §3.1's three first-run states, exactly what `library_home.rs`
    // asserts for the same starting condition.
    assert!(
        harness.query_by_label("No ROM folders yet").is_some(),
        "the library's empty first-run state did not render after installing the embedded \
         fonts — a corrupt or mismatched font file panics inside egui's own text layout the \
         first time it draws, so this failing (rather than the assertion above) would point at \
         the .ttf files themselves"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
