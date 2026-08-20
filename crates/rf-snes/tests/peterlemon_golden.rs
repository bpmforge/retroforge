//! PeterLemon PPU golden frames (ticket W6-03b; FR-CORE-033,
//! `docs/TESTING.md` §5's `peterlemon_ppu` row).
//!
//! ## The subset, and why it is these four
//!
//! W6-03a implements BG modes 0 and 1, so the subset is the BGMAP tests
//! those modes can actually render: the four 2BPP maps, one per
//! background. That exercises each of the four layers independently,
//! through real ROM code, against a photographic test image where any
//! tile-fetch, bitplane or palette error is immediately visible.
//!
//! The 4BPP BGMAP test is deliberately **excluded despite its name**:
//! running it shows it sets BGMODE to **3** (8bpp BG1 + 4bpp BG2), not
//! mode 1. On a modes-0/1 PPU its output is garbage, and a golden pinned
//! on garbage is worse than no golden — it would go green forever while
//! recording a wrong picture. [`assert_supported_mode`] makes that
//! failure mode impossible rather than relying on whoever adds the next
//! ROM to notice.
//!
//! ## What a self-generated golden does and does not prove
//!
//! These hashes were produced by this emulator, so on their own they only
//! pin current behaviour — they cannot prove the picture is *right*.
//! What makes them meaningful is that **each frame was rendered to a PNG
//! and looked at** before its hash was pinned: all four show the expected
//! landscape (sky, treeline, shoreline, water) rather than noise or a
//! blank screen. From here they are regression detectors, which is what a
//! golden is for.
//!
//! ## Waiting for the fade
//!
//! These ROMs fade the screen in over 15 vblanks (`BIT $4210` / `BPL`,
//! then `STA $2100` with a rising brightness) before settling into an
//! infinite loop. Composing a frame before that finishes captures a
//! forced-blank screen with empty VRAM — which is exactly what the first
//! attempt here did, producing four identical all-black frames that would
//! have hashed perfectly consistently. The run therefore waits for the
//! fade to COMPLETE, semantically, rather than for a magic instruction
//! count.

use rf_snes::SnesSystem;
use sha2::{Digest, Sha256};

/// Pinned goldens: `(rom file, sha256 of the frame's palette indices)`.
///
/// Hashing **palette indices, not colours**, matches
/// `rf_harness::golden_frame`'s rule: the golden is about the pixels the
/// PPU produced, not about how a renderer later resolves CGRAM.
///
/// A detail worth knowing before these hashes puzzle someone: the four
/// frames look IDENTICAL on screen yet hash differently. That is correct
/// and is itself evidence. In mode 0 each layer indexes a different
/// palette block — BG1 0-31, BG2 32-63, BG3 64-95, BG4 96-127 — and each
/// ROM loads CGRAM to match, so the same photograph drawn on a different
/// layer really is a different set of indices.
const GOLDENS: &[(&str, &str)] = &[
    (
        "8x8BG1Map2BPP32x328PAL.sfc",
        "5424cc9358ef99e49ebce0842d693fd89f0ec12bf642e9337b405f60e0303751",
    ),
    (
        "8x8BG2Map2BPP32x328PAL.sfc",
        "2040eb78cb7610dfb3de4255ea4d21660777c3a1240a4f10f2b94bbc09c160fc",
    ),
    (
        "8x8BG3Map2BPP32x328PAL.sfc",
        "ee52e98f504f055f9729e1a360cfaf49384308d251bf47cb68a2760ee16345cd",
    ),
    (
        "8x8BG4Map2BPP32x328PAL.sfc",
        "e936654961211bd301b70845d5184a45776efda6ecf730292ea40def4b5d4068",
    ),
];

/// Generous cap; the fade completes in well under this.
const MAX_INSTRUCTIONS: u64 = 20_000_000;
const HEIGHT: u16 = 224;
const WIDTH: usize = 256;

fn rom_dir() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("RF_PETERLEMON_PPU") {
        return Some(p.into());
    }
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../roms/snes/peterlemon-ppu");
    p.exists().then_some(p)
}

/// Refuse to hash a frame the PPU cannot legitimately render.
///
/// Without this, adding a ROM that uses an unimplemented mode would
/// silently pin whatever the mode-0 fallback drew — a green test
/// recording a wrong picture, which is the worst outcome a golden suite
/// can produce.
fn assert_supported_mode(name: &str, mode: u8) {
    assert!(
        mode == 0 || mode == 1,
        "{name} runs in BG mode {mode}, which W6-03a does not implement. \
         Pinning a golden for it would record the mode-0 fallback's garbage \
         as though it were correct. Implement the mode or drop the ROM."
    );
}

/// Run to a settled, faded-in frame and hash its palette indices.
fn render_and_hash(path: &std::path::Path) -> (String, u8) {
    let rom = std::fs::read(path).expect("rom readable");
    let mut s = SnesSystem::load(&rom).expect("PeterLemon ROMs are plain LoROM carts");

    // Wait for the fade to finish: screen on at full brightness.
    let mut n = 0u64;
    while n < MAX_INSTRUCTIONS {
        s.step().expect("all 256 opcodes are implemented");
        n += 1;
        if !s.bus.ppu.forced_blank && s.bus.ppu.brightness == 0x0F {
            break;
        }
    }
    assert!(
        !s.bus.ppu.forced_blank && s.bus.ppu.brightness == 0x0F,
        "{} never finished fading in after {n} instructions",
        path.display()
    );
    // Let it settle into its terminal loop so the frame is stable.
    for _ in 0..200_000 {
        s.step().expect("implemented");
    }

    let mut hasher = Sha256::new();
    // Optional PPM dump, so the frame a golden pins can be LOOKED AT
    // rather than trusted. This is how the mode-3 ROM was caught and how
    // the first all-black attempt was caught; keeping it wired means the
    // next person can re-verify instead of re-deriving.
    let dump = std::env::var("RF_GOLDEN_DUMP").ok();
    let palette = s.palette_rgb();
    let mut ppm = format!("P6\n{WIDTH} {HEIGHT}\n255\n").into_bytes();
    for y in 0..HEIGHT {
        let line = s.bus.ppu.render_scanline(y);
        assert_eq!(line.pixels.len(), WIDTH);
        let indices: Vec<u8> = line.pixels.iter().map(|p| p.palette_index).collect();
        hasher.update(&indices);
        if dump.is_some() {
            for i in &indices {
                ppm.extend_from_slice(&palette[*i as usize]);
            }
        }
    }
    if let Some(dir) = dump {
        let name = path.file_stem().unwrap_or_default().to_string_lossy();
        std::fs::write(format!("{dir}/{name}.ppm"), &ppm).expect("dump writable");
    }
    // sha2 0.11's digest is an `Array`, which has no `LowerHex` — same
    // fold rf-harness's golden_frame helper uses.
    use std::fmt::Write;
    let hex = hasher.finalize().iter().fold(String::new(), |mut acc, b| {
        let _ = write!(acc, "{b:02x}");
        acc
    });
    (hex, s.bus.ppu.bg_mode)
}

#[test]
#[ignore = "local: run scripts/fetch-peterlemon-ppu.sh first"]
fn peterlemon_bg_map_goldens_match() {
    let Some(dir) = rom_dir() else {
        eprintln!("SKIP: run scripts/fetch-peterlemon-ppu.sh");
        return;
    };
    let mut failures = Vec::new();
    for (name, expected) in GOLDENS {
        let path = dir.join(name);
        if !path.exists() {
            eprintln!("SKIP {name}: not fetched");
            continue;
        }
        let (actual, mode) = render_and_hash(&path);
        assert_supported_mode(name, mode);
        eprintln!("{name}: mode {mode}, sha256 {actual}");
        if actual != *expected {
            failures.push(format!(
                "{name}\n    expected {expected}\n    actual   {actual}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "golden frame mismatch:\n{}",
        failures.join("\n")
    );
}

/// Anti-vacuity: the four ROMs must produce FOUR DIFFERENT frames.
///
/// They draw the same photograph on four different background layers, so
/// identical hashes would mean the layer under test is not actually the
/// one being rendered — the exact bug a per-layer suite exists to catch,
/// and one that four separately-pinned goldens would otherwise hide
/// perfectly.
#[test]
#[ignore = "local: run scripts/fetch-peterlemon-ppu.sh first"]
fn the_four_layer_roms_do_not_all_render_the_same_frame() {
    let Some(dir) = rom_dir() else {
        eprintln!("SKIP");
        return;
    };
    let mut hashes = std::collections::BTreeSet::new();
    let mut seen = 0;
    for (name, _) in GOLDENS {
        let path = dir.join(name);
        if !path.exists() {
            continue;
        }
        seen += 1;
        hashes.insert(render_and_hash(&path).0);
    }
    if seen == 0 {
        eprintln!("SKIP: nothing fetched");
        return;
    }
    assert_eq!(
        hashes.len(),
        seen,
        "the {seen} per-layer ROMs produced only {} distinct frames — \
         a layer under test is not the one being rendered",
        hashes.len()
    );
}
