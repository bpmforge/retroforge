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

//! ## Verified against PeterLemon's own reference screenshots (W7-13)
//!
//! Every ROM in this repository ships a `.png` of what it is supposed to
//! look like, at 256x224. Comparing against those turned this suite from
//! self-generated pins into externally verified ones, and immediately
//! found a bug that eleven pinned goldens and every eyeball check had
//! missed: **the whole picture was one scanline low.**
//!
//! Rings matched its reference at 100% with a one-line offset and 79%
//! without; `8x8BG1Map2BPP32x328PAL` -- pinned since W6-03b -- did the
//! same at 100% vs 85%. Two ROMs, two BG modes, one answer. fullsnes
//! confirms why: the V counter runs 0-261 with "1-224 ... visible on the
//! screen", so line 0 is vblank and the picture starts at line 1. See
//! [`Ppu::render_scanline`] for the fix.
//!
//! Seven ROMs now match their reference **pixel-exactly**: the four 2BPP
//! maps, the 4BPP map, TileFlip, Rings and GreenSpace. The animating
//! demos (RotZoom, WaveHDMA, WindowMultiHDMA, MosaicMode3) cannot be
//! compared that way -- the shipped screenshot is a different frame of an
//! animation -- so for those the hash remains a regression detector
//! rather than a correctness proof, which is what a golden is for.
//!
//! ## What W7-05's four ROMs changed, and why looking mattered
//!
//! Criterion 4 originally named undisbeliever window/gradient ROMs. That
//! archive has neither; the GitHub contents API at this repo's already-
//! pinned PeterLemon commit has both. Adding them found three things that
//! all four hashes would have hidden:
//!
//! 1. **Windows and mosaic were not latched per scanline.** W7-07 latched
//!    the mode-7 matrix, BG mode and scroll and stopped there. WindowHDMA
//!    masked an identical 28 pixels on every one of 224 lines — an HDMA
//!    window rendered as a constant band. Fixed in `Ppu::latch_line`, and
//!    unit-tested in `tests::ppu` so it is not only this ignored file that
//!    covers it.
//! 2. **MosaicMode3's golden would have been vacuous.** It settles with
//!    `$2106` at size 1: mosaic *enabled* and doing nothing. The size is
//!    not on a timer — probing all twelve buttons showed L and R alone
//!    drive it. See [`capture_plan`].
//! 3. **A latent i32 overflow in the mode-7 transform**, which panics a
//!    debug build on legitimate registers. Perspective's line 0 gives
//!    `a * cx_fixed = -2,684,354,560`, past `i32::MIN`. No picture
//!    changes: those samples fall outside the playfield whether the
//!    product wraps or not, which is why the golden was pinned over it.
//!
//! Every previously-pinned hash is byte-identical after all three, which
//! is the control that says the fixes disturbed nothing that worked.
//!

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
        "85eae78722281d9a402d25d36c18c0158f799b6f574fb07882cbe4c1f69152db",
    ),
    (
        "8x8BG2Map2BPP32x328PAL.sfc",
        "38f2b0a50ffd94990d13de29bc7d52ca5091a1f88b3fd7dedc3fc612e2a4bbbe",
    ),
    (
        "8x8BG3Map2BPP32x328PAL.sfc",
        "a8f490acf86d2a1de520709f519cc1573673060322b283952b8fc9c1e3bf219d",
    ),
    (
        "8x8BG4Map2BPP32x328PAL.sfc",
        "a595e2c969644ef2704dba04e932fdbd13e7867518224db5b69ddacdce87ab32",
    ),
    (
        "8x8BGMap4BPP32x328PAL.sfc",
        "b2c05e68aec5b6e9c0073dfbcf7e4648b3b4fed581010696c4817d2fc0afe2d0",
    ),
    (
        "8x8BGMap8BPP32x32.sfc",
        "76e1ab67aa089a7c82f73e695f83362f400fc73fdbad5218b801d8e753ab2147",
    ),
    (
        "8x8BGMap8BPP32x64.sfc",
        "9f1f6ff7300aacecd20c35b1d122ba9b80f25aa7936d920a19e01b302f938c15",
    ),
    (
        "8x8BGMap8BPP64x32.sfc",
        "9f1f6ff7300aacecd20c35b1d122ba9b80f25aa7936d920a19e01b302f938c15",
    ),
    (
        "8x8BGMap8BPP64x64.sfc",
        "5cdcbdd87991077695387d1a3e77a650dd627ae032883563533ce9cd52669ee2",
    ),
    (
        "8x8BGMapTileFlip.sfc",
        "9df26505248a1f9f4d8127f76960c45809dfb1c1b05ffe3057ee4650c2d873dd",
    ),
    // Mode 7 (W7-04), rendered at hardware density. HD-Mode-7 is an
    // opt-in overlay and is deliberately NOT what this gates.
    //
    // Only RotZoom is pinned. Perspective and StarWars are in EXCLUDED
    // below, with evidence — see there.
    (
        "RotZoom.sfc",
        "6ebfb8ce1b9454734dba2f15ea96ddf533758a15d108623c6c3c3cc8eada50a2",
    ),
    // Pinned at W7-07, once HDMA and per-line register latching existed.
    // Before them this ROM drew a flat rotated track; it now draws a
    // ground plane receding to a horizon, verified by eye.
    (
        "Perspective.sfc",
        "7ab7001f3220feb5cda52af09f19ab1a18fcdf11b57cd7afdfdf36258b5de573",
    ),
    // Windows and mosaic (W7-05's amended criterion 4). Each was
    // rendered to a PNG and LOOKED AT before its hash was pinned, and
    // doing so changed three of the four outcomes -- see the module doc.
    //
    // WindowHDMA draws a lens-shaped reveal: the masked width narrows
    // toward the middle of the screen (30, 26, 22, 20, 22, 26, 28 masked
    // pixels sampled every 28 lines) and the cathedral shows through it.
    (
        "WindowHDMA.sfc",
        "ba01ba95aa9ffbdcdb5a9970c761df033ce34bad75ae53323552127c52af5b5f",
    ),
    // WindowMultiHDMA draws a 2x2 grid of visible quadrants: two windows
    // splitting each line, and HDMA blanking a band of lines between the
    // upper and lower halves.
    (
        "WindowMultiHDMA.sfc",
        "9c05fdd822d73a74465a600a11f56646c795118c0abb7a8c4a33cb360c0f8043",
    ),
    // MosaicMode3 at a block size of 8: the landscape photograph is
    // visibly blockified. See `capture_plan` for why the harness has to
    // hold a button to get here at all.
    (
        "MosaicMode3.sfc",
        "9606457f53ee2f7324cb96215e4ad5dd276bdc37d465021c7dfca31f8c601b69",
    ),
    // HDMA (W7-07's amended criterion 4). Two of the four are pinned;
    // the RedSpace pair is in EXCLUDED with evidence -- see there, because
    // the reason is structural and worth knowing.
    //
    // WaveHDMA drives $210D (BG1HOFS) per line: a water surface with the
    // scroll displacement making it ripple. This is the ROM that shows
    // per-line scroll working end to end.
    (
        "WaveHDMA.sfc",
        "2e6b2da85aff29e40f88c00cba976be3e84232804f50da6eb7fcbc24d4b60bc5",
    ),
    // Mode7HDMA switches BG mode mid-frame: sky and a sun and a row of
    // trees above, a mode-7 ground plane receding below. It needs both
    // HDMA and the per-line BG-mode latching to come out as anything but
    // one mode applied to all 224 lines.
    (
        "Mode7HDMA.sfc",
        "17911817330b88afb34cd29c47e6f517684ff78b6162015619726f99c4f7c75a",
    ),
    // Breadth (W7-13), both VERIFIED PIXEL-EXACT against PeterLemon's own
    // shipped reference screenshot -- see the module doc. The HiColor set
    // is in EXCLUDED with evidence.
    //
    // Rings draws the same ring pattern on BG1 and BG2 at different
    // scrolls, so it is a pure priority-and-scroll test; it is the ROM
    // whose reference comparison found the one-scanline offset.
    (
        "Rings.sfc",
        "a785a8cdfb4a1db0cc04e4866ab90c6fb39bb0430635639a44282e1ec2fd8d3c",
    ),
    // GreenSpace is a flat backdrop and matches its reference exactly.
    // Pinned with a caveat worth stating: its frame is entirely palette
    // index 0, so this hash equals every other all-backdrop frame's and
    // cannot tell a correct green screen from a PPU that drew nothing.
    // It guards against regressions that draw SOMETHING, and no more.
    (
        "GreenSpace.sfc",
        "4f81904a9b06c58572a0e5769b3b4ffb99e7bd4be88ee8c2b64a804f483d9dc6",
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
/// ROMs fetched but deliberately NOT pinned, with the reason for each.
///
/// **Excluded, not waived** — the same distinction the 65816 vector suite
/// draws. A waiver says "this is broken and we accept it"; an exclusion
/// says "pinning this would record a picture we know is not what the ROM
/// means to draw". Both entries name the ticket that will remove them.
const EXCLUDED: &[(&str, &str)] = &[
    (
        "StarWars.sfc",
        "renders fully transparent for its first ~170 frames: screen-over is 'transparent          outside the playfield' ($211A bits 6-7 = 2), the matrix sits static at A=410 D=256          Y0=-90, NMI is disabled, and the CPU loops at $00:8269 throughout. Whatever advances          this demo is not yet implemented, so there is nothing correct to pin — a black frame          would hash perfectly consistently forever. Diagnose with W7-07.",
    ),
    (
        "MosaicMode5.sfc",
        "mode 5 is HI-RES 512, and this PPU has no hi-res path at all: bg::bit_depths gives          mode 5 the right depths (4bpp/2bpp) but composition is 256 wide, and $2105 bit 3 is          consumed as bg3_priority with nothing reading a hi-res flag. The frame renders as a          half-width character squeezed against a backdrop-grey field. The MOSAIC half is          correct and visible (holding R blockifies it exactly as MosaicMode3 does), which is          what makes this an exclusion rather than a bug in this ticket: the mosaic path works,          the mode it is being drawn in does not exist yet. Pinning it would record a picture          nobody claims is right. Owned by W7-06, whose criterion 2 is hi-res modes 5/6.",
    ),
    (
        "RedSpaceHDMA.sfc",
        "the HDMA is CORRECT and the golden still cannot see it. Tracing the channel shows          exactly the right walk: $2121 <- $00 twice, then $2122 <- $1F, $1E, $1D ... one step          every 7 lines, 32 steps over 224 lines -- a red gradient down the screen. But the          demo draws NOTHING ELSE (VRAM is legitimately empty; it clears VRAM/WRAM/CGRAM and          parks in a one-instruction loop at $00:818B with NMI off), so every pixel of the frame          is palette index 0. This suite hashes palette INDICES, so a per-line CGRAM gradient is          invisible to it by construction, and the PPM dump resolves index 0 against ONE          end-of-frame palette snapshot -- which the gradient has by then walked down to $0000,          hence a black picture. Not a defect: the same index-vs-colour boundary that keeps          colour math out of the pixel stream (law 4). Covered instead by          tests::hdma::hdma_writes_a_different_backdrop_colour_on_each_line. W7-16 owns making          colour-domain output reachable.",
    ),
    (
        "RedSpaceIndirectHDMA.sfc",
        "same picture and same reason as RedSpaceHDMA, via INDIRECT mode -- the channel          dereferences its pointer and reads the identical $1F, $1E, $1D gradient, which is          direct evidence that criterion 1's indirect-mode bank register works. Excluded for the          index-domain reason above, not for anything wrong with the transfer.",
    ),
    (
        "InterlaceFont.sfc",
        "ALL SIX PPU/Interlace ROMs run in BG MODE 5, checked rather than assumed -- this one          included. Mode 5 is true hires, and fullsnes says how it works: 'the main/subscreen          pixels are rendered as half-pixels of the high-resolution image'. This core composes          ONE screen, so it draws the main screen's half-dots and nothing else, and the missing          half is not recoverable from what it has. InterlaceFont is the ROM that makes this          unmistakable: its point is sharp hires text, and every glyph comes out shredded with          alternate columns gone. Pinning any of these would assert that interlace and hires          render correctly when neither does. Unblocked by W7-16 (the sub-screen channel), which          W7-06's HANDOFF names.",
    ),
    (
        "InterlaceMoogle.sfc",
        "ALL SIX PPU/Interlace ROMs run in BG MODE 5, checked rather than assumed -- this one          included. Mode 5 is true hires, and fullsnes says how it works: 'the main/subscreen          pixels are rendered as half-pixels of the high-resolution image'. This core composes          ONE screen, so it draws the main screen's half-dots and nothing else, and the missing          half is not recoverable from what it has. InterlaceFont is the ROM that makes this          unmistakable: its point is sharp hires text, and every glyph comes out shredded with          alternate columns gone. Pinning any of these would assert that interlace and hires          render correctly when neither does. Unblocked by W7-16 (the sub-screen channel), which          W7-06's HANDOFF names.",
    ),
    (
        "InterlaceMystHDMA.sfc",
        "ALL SIX PPU/Interlace ROMs run in BG MODE 5, checked rather than assumed -- this one          included. Mode 5 is true hires, and fullsnes says how it works: 'the main/subscreen          pixels are rendered as half-pixels of the high-resolution image'. This core composes          ONE screen, so it draws the main screen's half-dots and nothing else, and the missing          half is not recoverable from what it has. InterlaceFont is the ROM that makes this          unmistakable: its point is sharp hires text, and every glyph comes out shredded with          alternate columns gone. Pinning any of these would assert that interlace and hires          render correctly when neither does. Unblocked by W7-16 (the sub-screen channel), which          W7-06's HANDOFF names.",
    ),
    (
        "InterlaceRPG.sfc",
        "ALL SIX PPU/Interlace ROMs run in BG MODE 5, checked rather than assumed -- this one          included. Mode 5 is true hires, and fullsnes says how it works: 'the main/subscreen          pixels are rendered as half-pixels of the high-resolution image'. This core composes          ONE screen, so it draws the main screen's half-dots and nothing else, and the missing          half is not recoverable from what it has. InterlaceFont is the ROM that makes this          unmistakable: its point is sharp hires text, and every glyph comes out shredded with          alternate columns gone. Pinning any of these would assert that interlace and hires          render correctly when neither does. Unblocked by W7-16 (the sub-screen channel), which          W7-06's HANDOFF names.",
    ),
    (
        "InterlaceScroll.sfc",
        "ALL SIX PPU/Interlace ROMs run in BG MODE 5, checked rather than assumed -- this one          included. Mode 5 is true hires, and fullsnes says how it works: 'the main/subscreen          pixels are rendered as half-pixels of the high-resolution image'. This core composes          ONE screen, so it draws the main screen's half-dots and nothing else, and the missing          half is not recoverable from what it has. InterlaceFont is the ROM that makes this          unmistakable: its point is sharp hires text, and every glyph comes out shredded with          alternate columns gone. Pinning any of these would assert that interlace and hires          render correctly when neither does. Unblocked by W7-16 (the sub-screen channel), which          W7-06's HANDOFF names.",
    ),
    (
        "InterlaceSimpsonsHDMA.sfc",
        "ALL SIX PPU/Interlace ROMs run in BG MODE 5, checked rather than assumed -- this one          included. Mode 5 is true hires, and fullsnes says how it works: 'the main/subscreen          pixels are rendered as half-pixels of the high-resolution image'. This core composes          ONE screen, so it draws the main screen's half-dots and nothing else, and the missing          half is not recoverable from what it has. InterlaceFont is the ROM that makes this          unmistakable: its point is sharp hires text, and every glyph comes out shredded with          alternate columns gone. Pinning any of these would assert that interlace and hires          render correctly when neither does. Unblocked by W7-16 (the sub-screen channel), which          W7-06's HANDOFF names.",
    ),
    (
        "HiColor1241DLair.sfc",
        "colour-math HIGH COLOUR, which needs the sub-screen this core does not have. Probed          rather than guessed: all three run in mode 3 with $2130 colour math ENABLED          (enable=0x21) and use_subscreen=true. The scene comes out recognisable -- Myst's          structures are all there -- and WRONG, because every pixel that should be a blend of          main and sub is drawn as the main screen's raw colour: Myst's sky is flat yellow where          it should be a gradient. Recognisable-but-wrong is the most dangerous thing to pin,          because it survives an eyeball check. Unblocked by W7-16.",
    ),
    (
        "HiColor3840.sfc",
        "colour-math HIGH COLOUR, which needs the sub-screen this core does not have. Probed          rather than guessed: all three run in mode 3 with $2130 colour math ENABLED          (enable=0x21) and use_subscreen=true. The scene comes out recognisable -- Myst's          structures are all there -- and WRONG, because every pixel that should be a blend of          main and sub is drawn as the main screen's raw colour: Myst's sky is flat yellow where          it should be a gradient. Recognisable-but-wrong is the most dangerous thing to pin,          because it survives an eyeball check. Unblocked by W7-16.",
    ),
    (
        "HiColor575Myst.sfc",
        "colour-math HIGH COLOUR, which needs the sub-screen this core does not have. Probed          rather than guessed: all three run in mode 3 with $2130 colour math ENABLED          (enable=0x21) and use_subscreen=true. The scene comes out recognisable -- Myst's          structures are all there -- and WRONG, because every pixel that should be a blend of          main and sub is drawn as the main screen's raw colour: Myst's sky is flat yellow where          it should be a gradient. Recognisable-but-wrong is the most dangerous thing to pin,          because it survives an eyeball check. Unblocked by W7-16.",
    ),
];

fn assert_supported_mode(name: &str, mode: u8) {
    assert!(
        mode <= 7,
        "{name} runs in BG mode {mode}, which W6-03a does not implement. \
         Pinning a golden for it would record the mode-0 fallback's garbage \
         as though it were correct. Implement the mode or drop the ROM."
    );
}

/// How to drive a ROM whose settled frame does not exercise the feature
/// it is named for (ticket W7-05).
///
/// **This exists because looking at the picture caught a vacuous golden.**
/// MosaicMode3 reaches its terminal loop with `$2106` at size 1 -- mosaic
/// ENABLED on BG1 and set to a block size of one pixel, which is by
/// definition a no-op. The frame is a perfectly good photograph, it hashes
/// consistently, and it would have gone green forever while testing none
/// of the mosaic path.
///
/// The size is not on a timer: probing all twelve buttons for six million
/// instructions each showed ten of them leave it at 1, and **L and R alone
/// drive it, reaching 16**. This is a demo with a control, not an
/// animation -- so the harness has to press the button. Holding `R` and
/// stopping at a size of 8 gives a block big enough to be unmistakable in
/// the frame and is reached deterministically from reset.
struct CapturePlan {
    /// Joypad-1 bit mask to hold. `$4218` bit 4 is R.
    hold: u16,
    /// Stop when this is true.
    until: fn(&SnesSystem) -> bool,
}

fn capture_plan(name: &str) -> Option<CapturePlan> {
    match name {
        "MosaicMode3.sfc" | "MosaicMode5.sfc" => Some(CapturePlan {
            hold: 1 << 4,
            until: |s| s.bus.ppu.mosaic.size >= 8,
        }),
        _ => None,
    }
}

/// Run to a settled, faded-in frame and hash its palette indices.
fn render_and_hash(path: &std::path::Path) -> (String, u8) {
    let rom = std::fs::read(path).expect("rom readable");
    let mut s = SnesSystem::load(&rom).expect("PeterLemon ROMs are plain LoROM carts");
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let plan = capture_plan(&name);

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
    match plan {
        // Settle into the terminal loop; the frame is stable there.
        None => {
            for _ in 0..200_000 {
                s.step().expect("implemented");
            }
        }
        // Drive the control until the feature is actually being
        // exercised, and render THERE. Settling further would walk the
        // size straight past it, since the button is still held.
        Some(plan) => {
            while n < MAX_INSTRUCTIONS && !(plan.until)(&s) {
                s.bus.joypads.ports[0] = plan.hold;
                s.step().expect("implemented");
                n += 1;
            }
            assert!(
                (plan.until)(&s),
                "{} never reached its capture condition in {n} instructions - \
                 the golden would pin a frame that exercises nothing",
                path.display()
            );
            s.bus.joypads.ports[0] = 0;
        }
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
    // Excluded ROMs are still RUN and reported — skipping them by name
    // would let an exclusion outlive its reason silently.
    for (name, why) in EXCLUDED {
        let path = dir.join(name);
        if !path.exists() {
            continue;
        }
        let (hash, mode) = render_and_hash(&path);
        assert_supported_mode(name, mode);
        eprintln!("  excluded {name}: mode {mode}, sha256 {hash}\n    {why}");
    }

    assert!(
        failures.is_empty(),
        "golden frame mismatch:\n{}",
        failures.join("\n")
    );
}

/// Anti-vacuity for the per-LAYER set: the four 2BPP ROMs must produce
/// four DIFFERENT frames.
///
/// They draw the same photograph on four different background layers, so
/// identical hashes would mean the layer under test is not the one being
/// rendered — the exact bug a per-layer suite exists to catch, and one
/// that four separately-pinned goldens would otherwise hide perfectly.
///
/// Scoped to those four on purpose. Two of the 8BPP geometry ROMs
/// (`32x64` and `64x32`) legitimately hash the SAME, because unscrolled
/// they both show the first 32x28 tiles, which live in screen 0 either
/// way. `the_tilemap_geometry_is_honoured_once_you_scroll_into_it` below
/// is what proves the geometry is actually read.
#[test]
#[ignore = "local: run scripts/fetch-peterlemon-ppu.sh first"]
fn the_four_layer_roms_do_not_all_render_the_same_frame() {
    let Some(dir) = rom_dir() else {
        eprintln!("SKIP");
        return;
    };
    const PER_LAYER: [&str; 4] = [
        "8x8BG1Map2BPP32x328PAL.sfc",
        "8x8BG2Map2BPP32x328PAL.sfc",
        "8x8BG3Map2BPP32x328PAL.sfc",
        "8x8BG4Map2BPP32x328PAL.sfc",
    ];
    let mut hashes = std::collections::BTreeSet::new();
    let mut seen = 0;
    for name in PER_LAYER {
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

/// `32x64` and `64x32` hash identically, and this proves that is a
/// property of the VIEW rather than of the mapping being ignored.
///
/// Unscrolled, both show the first 32x28 tiles, which sit in screen 0 in
/// either geometry — so identical output is correct. Scroll one screen
/// right and they must diverge: a 64-wide map has a second screen there,
/// while a 32-wide map wraps back to its first. Without this, "the two
/// geometries look the same" would be equally consistent with
/// `tilemap_size` never being read at all.
#[test]
#[ignore = "local: run scripts/fetch-peterlemon-ppu.sh first"]
fn the_tilemap_geometry_is_honoured_once_you_scroll_into_it() {
    let Some(dir) = rom_dir() else {
        eprintln!("SKIP");
        return;
    };
    let (wide, tall) = (
        dir.join("8x8BGMap8BPP64x32.sfc"),
        dir.join("8x8BGMap8BPP32x64.sfc"),
    );
    if !wide.exists() || !tall.exists() {
        eprintln!("SKIP: geometry ROMs not fetched");
        return;
    }

    let scrolled = |path: &std::path::Path| -> String {
        let rom = std::fs::read(path).expect("readable");
        let mut s = SnesSystem::load(&rom).expect("loads");
        let mut n = 0u64;
        while n < MAX_INSTRUCTIONS {
            s.step().expect("implemented");
            n += 1;
            if !s.bus.ppu.forced_blank && s.bus.ppu.brightness == 0x0F {
                break;
            }
        }
        for _ in 0..200_000 {
            s.step().expect("implemented");
        }
        // One screen right, into the territory the two maps disagree about.
        s.bus.ppu.bgs[0].hofs = 256;
        s.bus.ppu.bgs[1].hofs = 256;
        let mut hasher = Sha256::new();
        for y in 0..HEIGHT {
            let line = s.bus.ppu.render_scanline(y);
            let idx: Vec<u8> = line.pixels.iter().map(|p| p.palette_index).collect();
            hasher.update(&idx);
        }
        use std::fmt::Write;
        hasher.finalize().iter().fold(String::new(), |mut a, b| {
            let _ = write!(a, "{b:02x}");
            a
        })
    };

    assert_ne!(
        scrolled(&wide),
        scrolled(&tall),
        "a 64-wide and a 32-wide tilemap must differ once scrolled a screen \
         right — if they match, tilemap_size is not being read"
    );
}
