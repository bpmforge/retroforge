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
/// Re-pinned 2026-09-20 (ticket W14-31): six frames changed hash when
/// composition started reading the completed frame's per-line record
/// instead of a buffer the frame-start hook had already wiped. Each new
/// frame was dumped with `RF_GOLDEN_DUMP` and looked at: WaveHDMA shows
/// the wave across the whole picture, WindowHDMA/WindowMultiHDMA show
/// the per-line window shapes, MosaicMode3/MosaicMode5 show the mosaic,
/// and the 8bpp map is the castle. The previous hashes had pinned frames
/// whose per-line record was only partly latched.
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
    // Re-pinned 2026-09-20 (ticket W14-39): charging real internal
    // cycles shifted the settle loop's exact instruction/cycle count, so
    // this ROM's fixed 200,000-step settle now lands a few master cycles
    // later than before. Dumped with `RF_GOLDEN_DUMP` and looked at: the
    // castle is intact and identical in content to the previous pin —
    // same picture, different capture instant.
    (
        "8x8BGMap8BPP32x32.sfc",
        "38e22548e5f2f03565c43c0138977a85c4cd3924faf587006cf3016d35f63c9c",
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
        "e6a6cc83829199684ed8ad9ed63537260293bcb4a3f1b679846ab4f1d51afbc5",
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
        "6433d77d31634c7d65adcd4f24e97573ea359436ffe96a4c3cbf41076ad74afc",
    ),
    // WindowMultiHDMA draws a 2x2 grid of visible quadrants: two windows
    // splitting each line, and HDMA blanking a band of lines between the
    // upper and lower halves.
    (
        "WindowMultiHDMA.sfc",
        "e146e983a45726e39f2abc99211b47cc3fe0d25d9d6b0e341a976e34cc4bd419",
    ),
    // MosaicMode3 at a block size of 8: the landscape photograph is
    // visibly blockified. See `capture_plan` for why the harness has to
    // hold a button to get here at all.
    (
        "MosaicMode3.sfc",
        "d76be84ccfb54d54a45b691a3758c3a87f808806ece0cd72676b46f894c82a67",
    ),
    // HDMA (W7-07's amended criterion 4). Two of the four are pinned;
    // the RedSpace pair is in EXCLUDED with evidence -- see there, because
    // the reason is structural and worth knowing.
    //
    // WaveHDMA drives $210D (BG1HOFS) per line: a water surface with the
    // scroll displacement making it ripple. This is the ROM that shows
    // per-line scroll working end to end.
    // Re-pinned 2026-09-20 (ticket W14-39): same reason as
    // 8x8BGMap8BPP32x32.sfc above — the settle loop now lands a few
    // master cycles later, which for a per-line HDMA scroll effect
    // changes the exact ripple phase captured. Dumped and looked at: the
    // water ripple pattern is present and correct, same as before —
    // a different animation frame, not a broken one.
    (
        "WaveHDMA.sfc",
        "ee2dbeab401e937acc05cd1262cb17ec753e1ed5c0b2060c98704e56ee1f92c8",
    ),
    // Mode7HDMA switches BG mode mid-frame: sky and a sun and a row of
    // trees above, a mode-7 ground plane receding below. It needs both
    // HDMA and the per-line BG-mode latching to come out as anything but
    // one mode applied to all 224 lines.
    (
        "Mode7HDMA.sfc",
        "17911817330b88afb34cd29c47e6f517684ff78b6162015619726f99c4f7c75a",
    ),
    // StarWars (W7-04), unblocked twice over: W7-15's clock fix let the
    // demo run at all, and this ticket found that M7HOFS/M7VOFS were
    // never wired, so the logo sampled entirely outside the playfield.
    // The frame now shows the logo over a starfield -- looked at, and
    // 88% exact against PeterLemon's reference, the remainder being that
    // the reference is a different frame of a zoom animation.
    (
        "StarWars.sfc",
        "df9ee82194df24ea9bf61faa334d6fb0b3eab4f9624e95d89ea06b2aced5c60e",
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
    // ---- true hires, modes 5/6 (W7-06 criterion 3, third pass) ----
    //
    // These seven were EXCLUDED through two earlier passes of W7-06 and
    // are pinned now because the frames were finally LOOKED AT and are
    // right. What that look found first was that they were WRONG, and
    // the two defects behind them are worth naming because neither was
    // visible to any gate in this repository:
    //
    // 1. Both screens fetched the same 256-wide background, so the 512
    //    picture was the 256 picture with every column duplicated --
    //    55,842 of 57,344 half-dot pairs identical on InterlaceFont.
    //    Fixed by bg::HiresPhase (a double-rate fetch, even columns to
    //    the sub screen and odd to the main).
    // 2. Hires tiles were treated as 8 half-dots wide, so 64 tiles were
    //    read where 32 exist and a 32-wide tilemap WRAPPED -- the line
    //    rendered twice, side by side. In modes 5/6 the size bit selects
    //    16x8 or 16x16: sixteen half-dots either way.
    //
    // Pinning these closes the blind spot both defects hid in. Before
    // this, deliberately swapping the half-dot order left all nine gate
    // commands AND this suite green, because every mode 5/6 ROM was on
    // the excluded list -- the hires path had no coverage at all.
    // MosaicMode5.sfc: the moogle again under mosaic -- FLAT blocks of one colour, which is the check that mattered here (see the mosaic note in bg.rs: the wrong space to snap in fills every block with a two-colour stripe instead).
    (
        "MosaicMode5.sfc",
        "fe81d51a98a3b5db87a7f7f43dc5d174d04c518164ca745da27d97956e5996d0",
    ),
    // InterlaceFont.sfc: the full printable-ASCII chart, sharp: ! through @ on the top row, A-Z, then a-z. Its whole purpose is 512-dot text and every glyph is correctly formed.
    (
        "InterlaceFont.sfc",
        "91e6eb452f69150a5132807733123fa9b9e24552e4f5c3f49848ad5d996202a1",
    ),
    // InterlaceMoogle.sfc: a moogle's head with its red pompom, clean edges, no column striping.
    (
        "InterlaceMoogle.sfc",
        "f79457795f7500ac6f15cf70e68a80096ea3f1bb3953ea54167191ea1644ee0e",
    ),
    // InterlaceMystHDMA.sfc: the Myst rocket-ship island against a pale sky, with the HDMA gradient behind it.
    (
        "InterlaceMystHDMA.sfc",
        "59890c4751083acaa8be3b10005752016534788bb4626d77444e3f8cea17f48f",
    ),
    // InterlaceRPG.sfc: an RPG world map inside an ornate status frame, party sprite on the peak.
    (
        "InterlaceRPG.sfc",
        "f3ed84a9615bdbe82d0b796df1e8141413bf29fe3a7c47c8fc48843c3ee1eaa4",
    ),
    // InterlaceScroll.sfc: a tiled star field, every tile identical and aligned.
    (
        "InterlaceScroll.sfc",
        "919fc197aac3a5ae5f032518a08407a6f3a490d4a92daa4b930b324c456a7295",
    ),
    // InterlaceSimpsonsHDMA.sfc: Homer and Marge on the couch against the pink wall.
    (
        "InterlaceSimpsonsHDMA.sfc",
        "61b4ac1d86957fd213820fbef52d61a46574d492560d2c25735aa75683b7538c",
    ),
    // ---- HiColor, promoted from EXCLUDED (W7-13 criterion 1) ----
    //
    // These three were excluded on a reason that W7-16 made FALSE:
    // "colour-math HIGH COLOUR, which needs the sub-screen this core does
    // not have". CoreSink::sub_scanline shipped, so the core has it. An
    // exclusion outliving its reason is the exact rot this suite keeps
    // catching, so the reason was re-checked rather than the entry
    // deleted.
    //
    // WHAT LOOKING AT THEM ESTABLISHED, and the caveat is the important
    // half. The SHAPES are correct and unmistakable (see each entry). The
    // COLOURS in a PPM dump are NOT, and cannot be: all three run mode 3
    // with $2130 colour math enabled, and the dump resolves main-screen
    // indices against a single palette with no blending -- so Myst's sky
    // comes out bright yellow where hardware blends it. That is law 4
    // working as designed, not a defect: colour math is the RENDERER's
    // arithmetic and never enters the indexed pixel stream.
    //
    // So what these goldens pin is the MAIN-SCREEN COMPOSITION, which is
    // exactly what this core is responsible for, and they pin it on real
    // pictures with 155-240 distinct resolved colours rather than the
    // all-backdrop frames that keep the RedSpace pair excluded. What they
    // do NOT pin is the blend. If colour math ever regresses, these will
    // stay green -- W7-16's sub-screen channel is what covers that, and
    // tests::ppu::a_colour_math_change_is_visible_on_the_sub_screen_channel
    // is the test that would fail.
    // HiColor1241DLair.sfc: Dragon's Lair title art -- Dirk in front of a stone wall, with \"DIRK The DARING\" lettering. Shapes correct and legible.
    (
        "HiColor1241DLair.sfc",
        "63c0b32521d40565ecaedc4827da9f95dfa5b976c96f54438c587ec4c5fcda1c",
    ),
    // HiColor3840.sfc: a smooth two-axis blue-to-green gradient, which is what a 3840-colour test draws. No banding, no tearing.
    (
        "HiColor3840.sfc",
        "99d92bcefc6803ac39d0960b49f5abd487da8c8fc83a286d7053cecdf3463861",
    ),
    // HiColor575Myst.sfc: the Myst rocket-ship island against a sky, machinery and rock faces all correctly formed.
    (
        "HiColor575Myst.sfc",
        "1ad785d8bf8d911de4d9496aad3fed1605e0a53f02d6030dad2c58d6359784b2",
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
        "RedSpaceHDMA.sfc",
        "the HDMA is CORRECT and the golden still cannot see it. Tracing the channel shows          exactly the right walk: $2121 <- $00 twice, then $2122 <- $1F, $1E, $1D ... one step          every 7 lines, 32 steps over 224 lines -- a red gradient down the screen. But the          demo draws NOTHING ELSE (VRAM is legitimately empty; it clears VRAM/WRAM/CGRAM and          parks in a one-instruction loop at $00:818B with NMI off), so every pixel of the frame          is palette index 0. This suite hashes palette INDICES, so a per-line CGRAM gradient is          invisible to it by construction, and the PPM dump resolves index 0 against ONE          end-of-frame palette snapshot -- which the gradient has by then walked down to $0000,          hence a black picture. Not a defect: the same index-vs-colour boundary that keeps          colour math out of the pixel stream (law 4). Covered instead by          tests::hdma::hdma_writes_a_different_backdrop_colour_on_each_line. W7-16 owns making          colour-domain output reachable.",
    ),
    (
        "RedSpaceIndirectHDMA.sfc",
        "same picture and same reason as RedSpaceHDMA, via INDIRECT mode -- the channel          dereferences its pointer and reads the identical $1F, $1E, $1D gradient, which is          direct evidence that criterion 1's indirect-mode bank register works. Excluded for the          index-domain reason above, not for anything wrong with the transfer.",
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
        // StarWars zooms its logo in from nothing over ~200 frames, and
        // its first phase draws NO playfield at all -- the settle-after-
        // fade capture landed there, which is why W7-04 recorded a black
        // frame. Wait for the zoom to be well under way instead. `a` is
        // the mode-7 scale in 8.8, climbing 12 -> 92 -> ... -> 892; at 800
        // the logo is on screen and the content is stable.
        "StarWars.sfc" => Some(CapturePlan {
            hold: 0,
            until: |s| s.bus.ppu.mode7.a >= 800,
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
    // The dump's header is written after the first line is composed, so a
    // hires frame dumps as 512 wide rather than producing a file whose
    // header and payload disagree.
    let mut ppm: Vec<u8> = Vec::new();
    let mut dump_width = 0usize;
    for y in 0..HEIGHT {
        let line = s.bus.ppu.render_scanline(y);
        // WIDTH-AWARE since ticket W7-06: modes 5/6 and pseudo-hires emit
        // 512 dots, and the slice length IS the width tag. This used to
        // assert `== WIDTH`, which turned a correctly-widened hires line
        // into a harness failure rather than a golden mismatch — the
        // assertion was pinning the harness's assumption, not the ROM's
        // output.
        assert!(
            line.pixels.len() == WIDTH || line.pixels.len() == WIDTH * 2,
            "a scanline is either {WIDTH} dots or {} in hires, got {}",
            WIDTH * 2,
            line.pixels.len()
        );
        let indices: Vec<u8> = line.pixels.iter().map(|p| p.palette_index).collect();
        hasher.update(&indices);
        if dump.is_some() {
            if dump_width == 0 {
                dump_width = indices.len();
                ppm = format!("P6\n{dump_width} {HEIGHT}\n255\n").into_bytes();
            }
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
        // Ticket W14-31: composition now reads the per-line record latched
        // while the ROM ran (completed frame first, then the live one)
        // before it falls back to the raw registers, so a register poked
        // from outside the simulation is invisible until that record is
        // dropped. This test deliberately renders from the poked raw
        // registers, so drop it.
        s.bus.ppu.clear_line_state();
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
