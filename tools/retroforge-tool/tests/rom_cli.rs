//! Integration tests for the `rom` command group (ticket W4-07,
//! FR-DBG-007), driven as an actual subprocess exactly like `cli.rs`.
//!
//! Vacuity trap, stated up front because this group is unusually easy to
//! test worthlessly: a `rom dump` test that asserts "vram.bin is 4096
//! bytes" or a `rom hash` test that asserts "prints four hex strings"
//! would pass against a tool that dumped zeros and hashed the wrong
//! image. So every test here pins CONTENT:
//!
//! * `rom hash` is checked against hashes computed OUTSIDE this workspace
//!   (Python `hashlib`/`zlib`), and against a second ROM that differs
//!   only in header padding — normalized must match across the pair,
//!   raw must not. A tool that printed raw under the "normalized" label
//!   fails.
//! * `rom dump` runs a fixture ROM that writes KNOWN bytes to KNOWN
//!   VRAM/palette/OAM addresses, and the test asserts exactly those bytes
//!   at exactly those offsets.
//! * `rom trace` asserts the first line's PC is the reset vector and the
//!   nestest column layout is present — not merely that N lines appeared.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_retroforge-tool")
}

static NEXT_SCRATCH_ID: AtomicU32 = AtomicU32::new(0);

struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(tag: &str) -> Self {
        let id = NEXT_SCRATCH_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "retroforge_tool_rom_test_{tag}_{}_{id}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create scratch dir");
        ScratchDir(path)
    }

    fn write_bytes(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The fixture program, assembled by hand and living at `$C000`.
///
/// It writes values this test can pin exactly, then parks in a
/// self-jump so the machine is deterministic for any frame count:
///
/// ```text
///   LDA #$00 / STA $2001   ; rendering off — nothing else may touch OAM
///   $2006 <- $20,$00       ; VRAM address $2000 (nametable 0, cell 0)
///   $2007 <- $AB, $CD      ; nametable[0] = $AB, nametable[1] = $CD
///   $2006 <- $3F,$00       ; VRAM address $3F00 (palette 0)
///   $2007 <- $21           ; palette[0] = $21
///   $2003 <- $00           ; OAMADDR = 0
///   $2004 <- $5A, $6B      ; oam[0] = $5A, oam[1] = $6B
///   JMP $C037              ; self
/// ```
const FIXTURE_PROGRAM: &[u8] = &[
    0xA9, 0x00, 0x8D, 0x01, 0x20, // LDA #$00 ; STA $2001
    0xA9, 0x20, 0x8D, 0x06, 0x20, // LDA #$20 ; STA $2006
    0xA9, 0x00, 0x8D, 0x06, 0x20, // LDA #$00 ; STA $2006
    0xA9, 0xAB, 0x8D, 0x07, 0x20, // LDA #$AB ; STA $2007
    0xA9, 0xCD, 0x8D, 0x07, 0x20, // LDA #$CD ; STA $2007
    0xA9, 0x3F, 0x8D, 0x06, 0x20, // LDA #$3F ; STA $2006
    0xA9, 0x00, 0x8D, 0x06, 0x20, // LDA #$00 ; STA $2006
    0xA9, 0x21, 0x8D, 0x07, 0x20, // LDA #$21 ; STA $2007
    0xA9, 0x00, 0x8D, 0x03, 0x20, // LDA #$00 ; STA $2003
    0xA9, 0x5A, 0x8D, 0x04, 0x20, // LDA #$5A ; STA $2004
    0xA9, 0x6B, 0x8D, 0x04, 0x20, // LDA #$6B ; STA $2004
    0x4C, 0x37, 0xC0, // JMP $C037 (this instruction)
];

/// Mapper 0, 16 KiB PRG + 8 KiB CHR. `header_padding` fills iNES bytes
/// 11.. — bytes the parser ignores and `normalize_nes` strips, which is
/// what makes the normalized-vs-raw distinction observable.
fn fixture_rom(header_padding: &[u8]) -> Vec<u8> {
    let mut header = vec![0u8; 16];
    header[0..4].copy_from_slice(&rf_cart::nes::INES_MAGIC);
    header[4] = 1; // 1 x 16 KiB PRG
    header[5] = 1; // 1 x 8 KiB CHR
    for (i, b) in header_padding.iter().enumerate() {
        header[11 + i] = *b;
    }

    let mut prg = vec![0u8; 16 * 1024];
    prg[..FIXTURE_PROGRAM.len()].copy_from_slice(FIXTURE_PROGRAM);
    prg[0x3FFC] = 0x00; // reset vector -> $C000
    prg[0x3FFD] = 0xC0;

    let mut rom = header;
    rom.extend_from_slice(&prg);
    rom.extend_from_slice(&vec![0u8; 8 * 1024]);
    rom
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(bin())
        .args(args)
        .output()
        .expect("run retroforge-tool")
}

/// **The hash test that could not pass by accident.** These four
/// expectations were computed outside this workspace entirely (Python
/// `zlib.crc32` / `hashlib`) over the 24576-byte header-stripped image,
/// so they check `rf-cart` rather than restate it.
///
/// Mutation: label raw as normalized (or hash the wrong slice) and all
/// four fail at once.
#[test]
fn rom_hash_prints_externally_computed_normalized_hashes() {
    let dir = ScratchDir::new("hash");
    let path = dir.write_bytes("fixture.nes", &fixture_rom(&[]));

    let out = run(&["rom", "hash", path.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);

    for expected in [
        "594a6cae",
        "f473228b869e505936fa5b5e501e7392",
        "56ca808e315b69195dc0104741ba13f997d6ae20",
        "951c1a9fb6d94fea7103201805b68bd5833f0142cf5dbb45e995b46e1cab5bdd",
    ] {
        assert!(
            stdout.contains(expected),
            "externally-computed normalized hash {expected} missing from:\n{stdout}"
        );
    }

    // The labelling is load-bearing (FR-CORE-011): a reader must be told
    // WHICH set is the identity, or the tool has just handed them eight
    // hashes and a coin flip.
    assert!(
        stdout.contains("RA/No-Intro compatible") && stdout.contains("use this for identity"),
        "the normalized set must be identified as the identity form:\n{stdout}"
    );
    assert!(
        stdout.contains("DIAGNOSTICS ONLY"),
        "the raw set must be marked non-identity:\n{stdout}"
    );
}

/// Two files, byte-identical apart from header padding: the normalized
/// hashes must match and the raw hashes must differ. This is the whole
/// point of hashing a normalized image, and it is the assertion a tool
/// that quietly hashed the raw file everywhere would fail.
#[test]
fn header_padding_changes_raw_hashes_but_not_normalized_ones() {
    let dir = ScratchDir::new("norm");
    let a = dir.write_bytes("a.nes", &fixture_rom(&[]));
    let b = dir.write_bytes("b.nes", &fixture_rom(&[0x11, 0x22]));

    let parse = |path: &Path| -> (Vec<String>, Vec<String>) {
        let out = run(&["rom", "hash", path.to_str().unwrap()]);
        assert!(out.status.success(), "{out:?}");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let (norm, raw) = text.split_once("raw (").expect("both sections printed");
        let hashes = |section: &str| -> Vec<String> {
            section
                .lines()
                .filter_map(|l| l.split_whitespace().nth(1).map(str::to_string))
                .filter(|t| t.len() >= 8 && t.chars().all(|c| c.is_ascii_hexdigit()))
                .collect()
        };
        (hashes(norm), hashes(raw))
    };

    let (norm_a, raw_a) = parse(&a);
    let (norm_b, raw_b) = parse(&b);
    assert_eq!(
        norm_a.len(),
        4,
        "expected four normalized hashes: {norm_a:?}"
    );
    assert_eq!(raw_a.len(), 4, "expected four raw hashes: {raw_a:?}");
    assert_eq!(
        norm_a, norm_b,
        "the same game in two packagings must share one identity"
    );
    assert_ne!(
        raw_a, raw_b,
        "raw hashes over different files must differ, or the two sets are the same number \
         printed twice and this tool's whole distinction is decorative"
    );
}

#[test]
fn rom_inspect_reports_the_parsed_header() {
    let dir = ScratchDir::new("inspect");
    let path = dir.write_bytes("fixture.nes", &fixture_rom(&[]));

    let out = run(&["rom", "inspect", path.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("mapper:    0"), "{stdout}");
    assert!(stdout.contains("16384 bytes (1 x 16 KiB)"), "{stdout}");
    assert!(stdout.contains("8192 bytes (CHR ROM)"), "{stdout}");
    assert!(stdout.contains("battery:   false"), "{stdout}");
}

/// **The dump test that pins content.** The fixture wrote known bytes to
/// known addresses; the dumps must carry exactly those, and the tilemap
/// export must show the same tile indices the nametable holds.
///
/// Mutation: dump the wrong PPU region, an off-by-one offset, or a
/// tilemap decoded from a different buffer, and this fails.
#[test]
fn rom_dump_writes_the_exact_ppu_state_the_fixture_produced() {
    let dir = ScratchDir::new("dump");
    let path = dir.write_bytes("fixture.nes", &fixture_rom(&[]));
    let out_dir = dir.path().join("dumps");

    let out = run(&[
        "rom",
        "dump",
        path.to_str().unwrap(),
        "--frame",
        "2",
        "--out",
        out_dir.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{out:?}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let vram = std::fs::read(out_dir.join("vram.bin")).expect("vram.bin written");
    let oam = std::fs::read(out_dir.join("oam.bin")).expect("oam.bin written");
    let palette = std::fs::read(out_dir.join("palette.bin")).expect("palette.bin written");
    assert_eq!(vram.len(), 0x1000);
    assert_eq!(oam.len(), 256);
    assert_eq!(palette.len(), 32);

    assert_eq!(vram[0], 0xAB, "nametable[0] must be what the fixture wrote");
    assert_eq!(vram[1], 0xCD, "nametable[1] must be what the fixture wrote");
    assert_eq!(
        palette[0], 0x21,
        "palette[0] must be what the fixture wrote"
    );
    assert_eq!(oam[0], 0x5A, "oam[0] must be what the fixture wrote");
    assert_eq!(oam[1], 0x6B, "oam[1] must be what the fixture wrote");

    let tilemap = std::fs::read_to_string(out_dir.join("tilemap.txt")).expect("tilemap written");
    let first_row = tilemap.lines().next().expect("at least one row");
    assert!(
        first_row.starts_with("AB[0] CD[0] "),
        "the tilemap export must agree with the VRAM it was decoded from: {first_row}"
    );
    assert_eq!(
        tilemap.lines().count(),
        30,
        "an NES nametable is 30 rows of tiles"
    );
}

/// The trace's FORMAT is already proven byte-for-byte against
/// `nestest.log` by the W1-03 golden-trace gate; what this test proves is
/// that the CLI emits that same format, from the right machine, starting
/// at the right place — the reset vector, `$C000`.
#[test]
fn rom_trace_writes_nestest_format_starting_at_the_reset_vector() {
    let dir = ScratchDir::new("trace");
    let path = dir.write_bytes("fixture.nes", &fixture_rom(&[]));
    let trace_path = dir.path().join("trace.log");

    let out = run(&[
        "rom",
        "trace",
        path.to_str().unwrap(),
        "--instructions",
        "24",
        "--out",
        trace_path.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{out:?}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let trace = std::fs::read_to_string(&trace_path).expect("trace file written");
    let lines: Vec<&str> = trace.lines().collect();
    assert_eq!(lines.len(), 24, "one line per requested instruction");

    let first = lines[0];
    assert!(
        first.starts_with("C000"),
        "the trace must start at the reset vector, not wherever power-on left the PC: {first}"
    );
    for column in ["A:", "X:", "Y:", "P:", "SP:"] {
        assert!(
            first.contains(column),
            "nestest column `{column}` missing from: {first}"
        );
    }

    // Non-vacuity: the PC must actually ADVANCE. A formatter that printed
    // the same registers every line would satisfy every check above.
    assert_ne!(
        &lines[1][..4],
        "C000",
        "the second traced instruction must be at a different PC"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("C037")),
        "the fixture's self-jump should appear within 24 instructions:\n{trace}"
    );
}

#[test]
fn rom_subcommands_reject_a_missing_file_without_panicking() {
    let dir = ScratchDir::new("missing");
    let missing = dir.path().join("nope.nes");
    for sub in ["hash", "inspect", "dump", "trace"] {
        let out = run(&["rom", sub, missing.to_str().unwrap()]);
        assert!(!out.status.success(), "{sub} must fail on a missing file");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("nope.nes"),
            "{sub}: the error must name the file: {stderr}"
        );
    }
}
