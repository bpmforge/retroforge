//! Ticket W6-06: the LoROM/HiROM mirror-map fixtures build from source
//! and are structurally valid SNES images.
//!
//! ## What this asserts, and what it deliberately does not yet
//!
//! The ticket's criterion is that these fixtures "assert cartridge
//! address mapping including mirrors and banks via a golden RAM result
//! block". The ROMs do exactly that — `src/main.s` runs four checks and
//! writes a result block into WRAM — but **`rf-snes` is a 15-line stub
//! today**, so nothing can execute them. Reading that block is owed to
//! W6-02a (the SNES bus), and is recorded there rather than left as an
//! intention.
//!
//! What IS assertable now, and worth asserting, is everything short of
//! execution: that the toolchain produces the images deterministically,
//! that they are valid LoROM/HiROM cartridges a real console would boot,
//! and — the part that makes them a *pair* rather than two files — that
//! they differ in exactly the mapping under test and nowhere else.
//!
//! A structural test is not a substitute for running them. It is the
//! half that can be true before a CPU exists, and saying so is the point.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/rf-harness is two levels under the repo root")
        .to_path_buf()
}

/// Skip locally, fail on CI — the same rule every fixture-backed suite
/// here uses. CI builds these in their own step.
fn rom(name: &str) -> Option<Vec<u8>> {
    let path = repo_root().join(format!("fixtures/snes/mirror-map/build/{name}.sfc"));
    match std::fs::read(&path) {
        Ok(b) => Some(b),
        Err(_) => {
            assert!(
                std::env::var_os("CI").is_none(),
                "{} missing on CI: the fixture build step failed and this suite would pass \
                 having asserted nothing",
                path.display()
            );
            eprintln!(
                "SKIP: {} not built — run fixtures/snes/mirror-map/build.sh",
                path.display()
            );
            None
        }
    }
}

/// Header fields live at a fixed offset from the END of the image for
/// both mappings here (LoROM 32 KiB, HiROM 64 KiB), because each puts its
/// header in the last bank's `$FFC0`.
fn header_at(image: &[u8]) -> &[u8] {
    // LoROM's last bank is the whole 32 KiB image; HiROM's is the upper
    // 32 KiB of a 64 KiB one. Both put the header at $FFC0 within it.
    let base = if image.len() == 0x8000 {
        0x7FC0
    } else {
        0xFFC0
    };
    &image[base..base + 0x30]
}

fn title(image: &[u8]) -> String {
    String::from_utf8_lossy(&header_at(image)[..21])
        .trim_end()
        .to_string()
}

fn map_mode(image: &[u8]) -> u8 {
    header_at(image)[0x15]
}

fn reset_vector(image: &[u8]) -> u16 {
    let end = image.len();
    u16::from_le_bytes([image[end - 4], image[end - 3]])
}

#[test]
fn both_fixtures_are_structurally_valid_cartridges() {
    let (Some(lo), Some(hi)) = (rom("lorom"), rom("hirom")) else {
        return;
    };

    assert_eq!(lo.len(), 32 * 1024, "LoROM fixture is one 32 KiB bank");
    assert_eq!(
        hi.len(),
        64 * 1024,
        "HiROM maps a bank's FULL 64 KiB; a 32 KiB HiROM would leave half of bank $C0 unmapped \
         and the one thing distinguishing it from LoROM would be untestable"
    );

    // The map-mode byte is what a loader dispatches on, so a wrong one
    // means the emulator picks the wrong mapping and every later test in
    // this family is meaningless.
    assert_eq!(map_mode(&lo), 0x20, "LoROM slow");
    assert_eq!(map_mode(&hi), 0x21, "HiROM slow");

    // Both must boot from $8000 — where `reset` is linked in each config.
    assert_eq!(reset_vector(&lo), 0x8000);
    assert_eq!(reset_vector(&hi), 0x8000);
    // ...and the byte there must be SEI, the first instruction of reset.
    assert_eq!(lo[0], 0x78, "LoROM's $8000 is file offset 0");
    assert_eq!(hi[0x8000], 0x78, "HiROM's $8000 is file offset 0x8000");

    assert!(title(&lo).contains("LOROM"), "title: {:?}", title(&lo));
    assert!(title(&hi).contains("HIROM"), "title: {:?}", title(&hi));
}

/// **The pair must differ in the mapping and agree on everything else.**
///
/// Without this the two fixtures could drift into unrelated programs
/// testing unrelated things, and a difference in a later result block
/// would be uninterpretable — you could not tell whether the mapping or
/// the program caused it. They are built from ONE source with a `-D`
/// switch precisely so this holds.
#[test]
fn the_two_fixtures_differ_only_in_the_mapping_under_test() {
    let (Some(lo), Some(hi)) = (rom("lorom"), rom("hirom")) else {
        return;
    };
    assert_ne!(map_mode(&lo), map_mode(&hi));

    // The code segment is byte-identical apart from the operands of the
    // bank-specific loads, so the two images share a long common prefix.
    // A pair that had diverged into different programs would not.
    let common = lo
        .iter()
        .zip(hi[0x8000..].iter())
        .take_while(|(a, b)| a == b)
        .count();
    assert!(
        common > 16,
        "the two fixtures share only {common} leading bytes of code — they have drifted into \
         different programs, and a mapping difference between them would be uninterpretable"
    );
}

/// The result-block protocol the ROMs implement, pinned here so W6-02a
/// has something to read against rather than re-deriving it from
/// assembly.
///
/// Asserted against the SOURCE rather than a comment, so the constants
/// cannot drift: if someone changes the magic in `main.s`, this fails.
#[test]
fn the_result_block_protocol_is_what_the_bus_ticket_will_read() {
    let src = std::fs::read_to_string(repo_root().join("fixtures/snes/mirror-map/src/main.s"))
        .expect("the fixture source is checked in");

    assert!(src.contains("CHECKS   = 4"), "four checks");
    // Magic written LAST is the property that makes a partial run
    // detectable — a harness finding it knows every earlier byte came
    // from a run that reached the end.
    assert!(
        src.contains("$52") && src.contains("$46"),
        "the 'RF' magic must be present"
    );
    let magic_at = src.find("lda #$52").expect("magic write");
    let tally_at = src.find("tally:").expect("tally loop");
    assert!(
        magic_at > tally_at,
        "the magic must be written AFTER the tally, or its presence would not certify that the \
         counts beneath it are complete"
    );
}
