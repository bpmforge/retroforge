//! ROM-gated check of the patch parsers against a real, published patch
//! (W32-01). `RF_PATCH_ROM` = the user's ROM, `RF_PATCH_FILE` = a patch made
//! for it (e.g. wide-snes's `smw-widescreen.bps` with Super Mario World
//! (USA)). BPS carries CRC32s of source, target and patch, so `Ok` proves the
//! parser decodes the format exactly. Skips cleanly without the files; no
//! ROM or patch bytes live in git (law 5).

#[test]
#[ignore = "needs a user-supplied ROM and patch (RF_PATCH_ROM, RF_PATCH_FILE)"]
fn a_real_published_patch_applies_with_matching_crcs() {
    let (Ok(rom), Ok(patch)) = (
        std::env::var("RF_PATCH_ROM"),
        std::env::var("RF_PATCH_FILE"),
    ) else {
        eprintln!("skipped: set RF_PATCH_ROM and RF_PATCH_FILE");
        return;
    };
    let rom = std::fs::read(rom).expect("rom");
    let patch = std::fs::read(patch).expect("patch");
    let (out, header) = rf_enhance::patch::apply_patch(&rom, &patch).expect("patch applies");
    println!(
        "patched: {} -> {} bytes, header {header:?}",
        rom.len(),
        out.len()
    );
    assert!(!out.is_empty());
}
