//! W5-04's "demo capture recorded", produced by a harness rather than by
//! hand (Brad's ruling 2026-08-19: "automation counts for now").
//!
//! ## Why an automated capture is the better artefact here
//!
//! A screen recording shows what one person's machine did once. This
//! regenerates from the **checked-in replay**
//! (`fixtures/replays/unprofiled-scroller.rfreplay`) and the
//! deterministically-built fixture ROM, so anyone can reproduce it byte
//! for byte, and a change that broke the feature changes the image
//! rather than leaving a stale video that still looks right.
//!
//! ## What it captures, and why these panels
//!
//! Each row of the contact sheet is one sampled moment:
//!
//! * **left** — the unmodified console output, 256x240, exactly what the
//!   player sees. This is the control: it is what the enhanced panel must
//!   be consistent with.
//! * **right** — the **full level decoded from ROM**, 768x224, with the
//!   original-viewport outline and the live player marker drawn on it.
//!   The level is drawn from `metatile_screens` output through the game's
//!   own CHR and palette tables, so it is a reconstruction of geometry
//!   the player has not necessarily visited — which is the entire claim
//!   MVP.md's RF-Scroller line makes.
//!
//! Put side by side, the demo is legible without narration: the outline
//! on the right marks exactly the region the left panel is showing, and
//! it moves across the reconstruction as the player walks.
//!
//! `#[ignore]`d: it WRITES checked-in artefacts under `docs/demo/`, and a
//! test that rewrote them on every `cargo test` would dirty the working
//! tree on every run — the same call `unprofiled_scroller_replay`'s
//! recorder makes. Regenerate with:
//!   cargo test -p retroforge --test demo_capture -- --ignored --nocapture

use std::path::{Path, PathBuf};

use retroforge::level_view::LevelSession;
use retroforge::stepper::EmuStepper;
use rf_debugger::pattern::{self, PatternTable};
use rf_input::replay::ReplayLog;

/// Frames to sample. Four is enough to show the outline crossing the
/// reconstruction without making the sheet unreadable or the PNG large.
const SAMPLES: [usize; 4] = [30, 300, 600, 880];

const NES_W: usize = 256;
const NES_H: usize = 240;
const METATILE_PX: usize = 16;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/retroforge is two levels under the repo root")
        .to_path_buf()
}

/// A simple RGBA canvas, so the compositing below reads as drawing rather
/// than as index arithmetic.
struct Image {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Image {
    fn new(w: usize, h: usize, fill: [u8; 4]) -> Self {
        let mut px = Vec::with_capacity(w * h * 4);
        for _ in 0..w * h {
            px.extend_from_slice(&fill);
        }
        Self { w, h, px }
    }

    fn set(&mut self, x: usize, y: usize, rgba: [u8; 4]) {
        if x < self.w && y < self.h {
            let i = (y * self.w + x) * 4;
            self.px[i..i + 4].copy_from_slice(&rgba);
        }
    }

    /// Composite `src` over this image, skipping fully transparent
    /// source pixels.
    ///
    /// **An alpha test, not a copy, and the difference is visible.**
    /// `rf_debugger::pattern::tile_to_rgba` writes alpha 0 for palette
    /// index 0 — its own doc calls that "transparent where nothing drew",
    /// so a pattern-viewer image "composites sensibly over any
    /// background". Copying those pixels straight through instead leaves
    /// the whole sky transparent, which a PNG viewer renders as WHITE —
    /// and the first version of this capture did exactly that, producing
    /// a level whose sky was white while the console panel beside it was
    /// black. The backdrop this composites onto is the game's own
    /// `area_palette[0]`.
    fn blit(&mut self, src: &[u8], sw: usize, sh: usize, dx: usize, dy: usize) {
        for y in 0..sh {
            for x in 0..sw {
                let i = (y * sw + x) * 4;
                if src[i + 3] == 0 {
                    continue;
                }
                self.set(dx + x, dy + y, [src[i], src[i + 1], src[i + 2], src[i + 3]]);
            }
        }
    }

    fn rect_outline(&mut self, x: i64, y: i64, w: usize, h: usize, rgba: [u8; 4]) {
        for i in 0..w {
            for (yy, on) in [(y, true), (y + h as i64 - 1, true)] {
                if on && yy >= 0 {
                    self.set(
                        usize::try_from(x + i as i64).unwrap_or(usize::MAX),
                        yy as usize,
                        rgba,
                    );
                }
            }
        }
        for i in 0..h {
            for xx in [x, x + w as i64 - 1] {
                if xx >= 0 {
                    self.set(
                        xx as usize,
                        usize::try_from(y + i as i64).unwrap_or(usize::MAX),
                        rgba,
                    );
                }
            }
        }
    }

    fn fill_rect(&mut self, x: i64, y: i64, w: usize, h: usize, rgba: [u8; 4]) {
        for dy in 0..h {
            for dx in 0..w {
                if x + dx as i64 >= 0 && y + dy as i64 >= 0 {
                    self.set((x + dx as i64) as usize, (y + dy as i64) as usize, rgba);
                }
            }
        }
    }
}

/// Render the decoded level through the game's own CHR and palette.
///
/// Not a colour-coded diagram of metatile ids: the claim being
/// demonstrated is "the full level rendered from ROM decode", so it is
/// drawn with the same tiles and colours the console would use, and a
/// wrong decode looks wrong rather than looking like a different diagram.
fn render_level(session: &LevelSession, chr: &[u8], area_palette: [u8; 4]) -> Image {
    let level = &session.level;
    let w = level.width as usize * METATILE_PX;
    let h = level.height as usize * METATILE_PX;
    let rgb = |i: u8| {
        let c = rf_renderer::palette_index_to_rgb(i);
        [c[0], c[1], c[2], 255]
    };
    let pal = [
        rf_renderer::palette_index_to_rgb(area_palette[0]),
        rf_renderer::palette_index_to_rgb(area_palette[1]),
        rf_renderer::palette_index_to_rgb(area_palette[2]),
        rf_renderer::palette_index_to_rgb(area_palette[3]),
    ];
    let mut img = Image::new(w, h, rgb(area_palette[0]));

    for col in 0..level.width {
        for row in 0..level.height {
            let Some(id) = level.at(col, row) else {
                continue;
            };
            let Some(tiles) = level.tiles_for(id) else {
                continue;
            };
            // {TL, TR, BL, BR}, per FORMAT.md's metatile table.
            for (quadrant, tile_id) in tiles.iter().enumerate().take(4) {
                let Some(tile) = pattern::decode_tile(chr, PatternTable::Left, *tile_id) else {
                    continue;
                };
                let rgba = pattern::tile_to_rgba(&tile, pal);
                let ox = col as usize * METATILE_PX + (quadrant % 2) * 8;
                let oy = row as usize * METATILE_PX + (quadrant / 2) * 8;
                img.blit(&rgba, 8, 8, ox, oy);
            }
        }
    }
    img
}

#[test]
#[ignore = "writes checked-in artefacts under docs/demo/; run deliberately"]
fn capture_the_full_level_demo() {
    let root = repo_root();
    let Ok(rom) = std::fs::read(root.join("fixtures/nes/rf-scroller/build/rf-scroller.nes")) else {
        eprintln!("SKIP: fixture ROM not built — run fixtures/nes/rf-scroller/build.sh");
        return;
    };
    let hashes = rf_cart::hash::identity_nes(&rom).normalized;
    let sha = hashes.sha256.clone();
    let session = LevelSession::open(&root.join("profiles"), &rom, &hashes)
        .expect("the shipped RF-Scroller profile must match the fixture");

    // The game's own CHR and background palette, read through the
    // profile's rom_map rather than hardcoded.
    let normalized = &rom[16..];
    let chr = rf_nes::NesRom::from_ines_bytes(&rom)
        .expect("valid rom")
        .chr_rom()
        .to_vec();
    let pal_off = session
        .profile
        .rom_map
        .iter()
        .find(|e| e.label == "area_palette")
        .expect("the profile declares area_palette")
        .offset as usize;
    let area_palette: [u8; 4] = normalized[pal_off..pal_off + 4]
        .try_into()
        .expect("four bytes");

    let level_img = render_level(&session, &chr, area_palette);

    // Replay the checked-in session so the capture is reproducible.
    let text = std::fs::read_to_string(root.join("fixtures/replays/unprofiled-scroller.rfreplay"))
        .expect("the checked-in replay must exist");
    let log = ReplayLog::parse(&text).expect("replay parses");
    log.verify_rom_sha256(&sha)
        .expect("the replay must match this ROM");

    let mut stepper = EmuStepper::from_ines_bytes(&rom).expect("fixture loads");
    stepper.resume();
    let mut sink = rf_renderer::frame::FrameBuffer::new();

    let row_h = level_img.h.max(NES_H) + 8;
    let mut sheet = Image::new(
        NES_W + 8 + level_img.w,
        row_h * SAMPLES.len(),
        [16, 16, 20, 255],
    );

    let mut captured = 0;
    for (i, input) in log.frames.iter().enumerate() {
        stepper.latch_and_advance_frame(*input, &mut sink);
        if !SAMPLES.contains(&i) {
            continue;
        }
        let row_y = captured * row_h;

        // Left: unmodified console output.
        sheet.blit(sink.rgba(), NES_W, NES_H, 0, row_y);

        // Right: the reconstruction, with the outline and marker on it.
        let mut panel = Image::new(level_img.w, level_img.h, [0, 0, 0, 255]);
        panel.px.copy_from_slice(&level_img.px);

        let camera_x = i64::from(stepper.peek(0x602B)) | (i64::from(stepper.peek(0x602C)) << 8);
        let player_x = i64::from(stepper.peek(0x6029)) | (i64::from(stepper.peek(0x602A)) << 8);
        // The original-viewport outline, in white.
        panel.rect_outline(
            camera_x,
            0,
            NES_W,
            level_img.h.min(NES_H),
            [255, 255, 255, 255],
        );
        // The live player marker, in red, at the profile-published
        // world position — the same value the Lua example reads.
        panel.fill_rect(
            player_x - 4,
            (level_img.h / 2) as i64 - 4,
            8,
            8,
            [255, 64, 64, 255],
        );

        sheet.blit(&panel.px, panel.w, panel.h, NES_W + 8, row_y);
        captured += 1;
    }
    assert_eq!(
        captured,
        SAMPLES.len(),
        "every sample frame must be captured"
    );

    let out = root.join("docs/demo");
    std::fs::create_dir_all(&out).expect("create docs/demo");

    // Downscaled 3x before encoding, and the reason is worth recording
    // rather than leaving as an unexplained magic number:
    // `rf_renderer::png` writes STORED (uncompressed) deflate blocks —
    // it was built for W3-04's screenshot path with no compression
    // dependency — so a PNG costs exactly width*height*4 bytes. At full
    // size this sheet is 4 MB, which is not a thing to check into git and
    // re-commit on every regeneration. At 3x it is ~450 KB and still
    // legible: the level panel is 256px wide, enough to see the outline
    // travel across the reconstruction, which is what the capture is for.
    // Teaching that encoder to deflate is a real follow-up and would let
    // this go back to full resolution.
    let (small, sw, sh) = retroforge::state_slots::downscale(
        &sheet.px,
        u32::try_from(sheet.w).unwrap(),
        u32::try_from(sheet.h).unwrap(),
        u32::try_from(sheet.w / 3).unwrap(),
    );
    let png = rf_renderer::png::encode_rgba(&small, sw, sh);
    let path = out.join("rf-scroller-full-level.png");
    std::fs::write(&path, &png).expect("write the contact sheet");
    eprintln!(
        "wrote {} ({sw}x{sh} from {}x{}, {} KiB) — {} sampled frames",
        path.display(),
        sheet.w,
        sheet.h,
        png.len() / 1024,
        captured
    );
}
