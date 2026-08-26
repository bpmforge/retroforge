//! **A decoded level becomes pixels** (ticket W11-02).
//!
//! The gap this closes: `SceneLayer::DecodedLevel` had no renderer
//! anywhere in the repository. The decode was tested, the scene graph was
//! tested, and neither could produce an image — `level_view_demo.rs`
//! asserts on the scene's shape and stayed green throughout.
//!
//! So this test asserts on **pixels**, and specifically that they are not
//! uniform. A producer that returned a correctly-sized buffer of zeroes
//! would satisfy every structural check anyone might write.

use std::path::{Path, PathBuf};

use retroforge::enhanced_view::render_level_rgba;
use retroforge::level_view::LevelSession;

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn fixture() -> Option<(PathBuf, Vec<u8>)> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let bytes = std::fs::read(&p).ok()?;
    Some((p, bytes))
}

#[test]
fn the_rf_scroller_level_renders_to_a_non_uniform_image() {
    let Some((_, rom)) = fixture() else {
        panic!("fixture ROM missing — this test is about pixels and cannot run without one");
    };
    let cart = rf_cart::Cartridge::load(&rom).expect("valid cartridge");
    let hash = match &cart {
        rf_cart::Cartridge::Nes { identity, .. } | rf_cart::Cartridge::Snes { identity, .. } => {
            identity.normalized.sha256.clone()
        }
    };
    let profiles = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles");
    let session = LevelSession::open(&profiles, &rom, &hash)
        .expect("RF-Scroller has a profile with a decodable level");

    // CHR comes from the ROM's own character bank, the same source the
    // pattern viewer uses.
    let chr = rom
        .strip_prefix(b"NES\x1a")
        .map(|_| {
            let prg_banks = rom[4] as usize;
            let start = 16 + prg_banks * 0x4000;
            rom.get(start..).unwrap_or(&[]).to_vec()
        })
        .unwrap_or_default();
    assert!(!chr.is_empty(), "the fixture must carry CHR to draw with");

    // A four-shade greyscale ramp: enough to tell tiles apart without
    // claiming to know the game's real palette, which is what the
    // profile-driven path supplies in the app.
    let palette = [[0, 0, 0], [85, 85, 85], [170, 170, 170], [255, 255, 255]];
    let rgba = render_level_rgba(
        &session.level,
        &chr,
        rf_debugger::pattern::PatternTable::Left,
        palette,
        session.geometry,
    );

    let (w, h) = (session.geometry.width_px, session.geometry.height_px);
    assert!(
        w > 256,
        "a full level should be wider than one screen; got {w}px"
    );
    assert_eq!(
        rgba.len(),
        (w as usize) * (h as usize) * 4,
        "the buffer must match the geometry it claims"
    );

    // **The load-bearing assertion.** A producer returning the right
    // number of zero bytes passes every size and type check ever written
    // about it.
    let distinct: std::collections::HashSet<[u8; 4]> = rgba
        .chunks_exact(4)
        .map(|c| [c[0], c[1], c[2], c[3]])
        .collect();
    assert!(
        distinct.len() >= 3,
        "the rendered level has only {} distinct pixel value(s) — it is a flat rectangle, \
         which is what an empty producer and a working one both look like to a size check",
        distinct.len()
    );

    // Written out so a human can look. The whole reason this producer
    // was missing for so long is that every check anyone wrote about the
    // level was structural, and a level is a picture.
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-tour");
    if std::fs::create_dir_all(&out).is_ok() {
        if let Some(img) = image::RgbaImage::from_raw(w, h, rgba.clone()) {
            let _ = img.save(out.join("08-decoded-level.png"));
        }
    }

    // And it drew something in more than one place: a level whose whole
    // image is one tile repeated would also clear the check above.
    let opaque = rgba.chunks_exact(4).filter(|c| c[3] != 0).count();
    assert!(
        opaque > (w as usize) * 8,
        "only {opaque} opaque pixels in a {w}x{h} level — the grid walk is not covering it"
    );
}
