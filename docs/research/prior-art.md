# RetroForge — Prior Art Research Brief

Date: 2026-07-06. All statuses verified via web as of this date. Confidence flags noted where evidence is thin.

---

## 1. Mesen / Mesen2 / MesenCE

**What**: The reference NES (now multi-system: NES, SNES, GB/GBC, GBA, PCE, SMS/GG, WonderSwan) emulator for enhancement + debugging.

**Status 2026 (verified)**: SourMesen/Mesen2 was **archived June 4, 2026** (last Sour build July 2025; final release 2.2.0, June 4 2026). Development continues as the community fork **MesenCE** (nesdev-org/MesenCE, v2.2.1+, active June 2026 git updates). Lesson: single-maintainer emulators die; community handoff worked here.
- https://github.com/SourMesen/Mesen2 (archived) / https://github.com/nesdev-org/MesenCE
- https://www.mesen.ca/

**HD pack system (how it works)** — https://www.mesen.ca/docs/hdpacks.html
- A pack = PNG tile sheets + `hires.txt` manifest (format version 105).
- **Tile identity = tile pixel data + palette.** CHR-ROM games: tile identified by CHR-ROM index (hex int). CHR-RAM games: identified by the full 16 bytes of tile data (32-char hex string) — i.e., content-hash matching at render time. Palette = 8 hex chars (sprites use FF prefix).
- `<tile>` maps (tiledata, palette) → (PNG index, x, y, brightness). Brightness multiplier lets one HD tile serve fade-in/out variants.
- **Conditions system** resolves the core ambiguity (same tile+palette used by different objects): `hmirror`/`vmirror`/`bgpriority`, spatial (`tileNearby`, `spriteNearby`, `tileAtPosition`, `spriteAtPosition`), **`memoryCheck` / `memoryCheckConstant`** (compare RAM addresses with ==/!=/>/< and bitmask), `frameRange` for animation. Combine with `&` and `!`.
- Extras: `<scale>` integer 1–4x+, `<overscan>`, parallax `<background>` layers with scroll ratios, **audio replacement** (`<bgm>`/`<sfx>` OGG, driven by memory-mapped registers $4100–$4106).
- **HD Pack Builder**: play the game while the emulator records every rendered (tile, palette) pair to PNG sheets and auto-generates hires.txt. This record-then-repaint workflow is the standard pipeline.

**Remove sprite limit**: disables the NES 8-sprites-per-scanline limit (up to 64/line), removing flicker. Documented failure mode: some games *rely* on the limit to hide objects, so Mesen adds "automatically re-enable sprite limit as needed" heuristics; even so, bugs are reported (Mesen issues #60, #188). https://www.mesen.ca/docs/configuration/video.html

**Debugger suite**: full-featured — event viewer (register writes/IRQ/NMI plotted on the frame), tilemap/sprite/palette viewers, memory tools + trace logger, Lua scripting. This tooling is exactly the substrate an enhancement engine needs (the HD Pack Builder is essentially a debugger feature productized).

**Lessons**: (a) tile-content + palette hashing with a declarative condition language is proven and community-adopted; (b) every enhancement needs an escape hatch/auto-heuristic because games exploit hardware quirks; (c) build the debugger first — enhancement tooling falls out of it.

## 2. bsnes-hd beta

**What**: DerKoun's bsnes fork adding **HD Mode 7** (Mode 7 transforms computed at up to 4x internal resolution — same math, more samples; no upscaling/custom art), **widescreen**, and true-color (8bpc instead of 5bpc math). https://github.com/DerKoun/bsnes-hd

**Widescreen (how)**: the PPU renderer emits extra pixel columns left/right (16:9 ≈ +64 columns) instead of stopping at 256. Per-BG-layer widescreen settings (enable/disable per BG1–4, scanline thresholds, crop — lets you keep the HUD at 4:3 while the playfield extends), sprite modes (clip / "safe" partial / "unsafe" full render beyond edges), and an "ignore window" fallback for games that clamp with HDMA windows. Works because SNES tilemaps are 512–1024px wide — the data beyond the seen 256px usually exists; the PPU just wasn't asked for it.

**Limitations (documented)**: sprites/objects don't behave correctly in the extended area without ROM hacks (game logic culls/despawns off-screen actors — same fundamental problem wideNES hit); non-Mode-7 widescreen artifacts in many games; per-game tweaking required; CPU-rendered so expensive. https://bsnes.org/docs/graphics/

**Status 2026**: last release **beta 10.6, June 17, 2021** — effectively dormant; lives on mainly as a libretro core (which proves widescreen/HD output *is* deliverable through libretro via enlarged geometry). https://www.libretro.com/index.php/bsnes-hd-beta-core-pushing-the-limits-of-the-snes-widescreen-and-ultrawide-support/

**Lessons**: widescreen = render more of data that already exists + per-layer policy knobs + accept that *game logic*, not the PPU, is the real limiter. A "widescreen compatibility profile" per game is unavoidable.

## 3. ares / higan (byuu/Near lineage)

**What**: accuracy/preservation-first multi-system emulator. **Status 2026 (verified)**: ares **v148, May 30 2026**, actively maintained, 40+ systems. https://ares-emu.net/ · https://github.com/ares-emulator/ares

**Architecture**: each chip (CPU/PPU/APU/coprocessors) is a **cooperative thread**; a scheduler keeps them aligned in time. byuu's articles (byuu.net is dead; mirrored at https://github.com/higan-emu/emulation-articles, design/schedulers): *relative* scheduler = signed 64-bit delta per thread pair (fast, fine for SNES's few chips); *absolute* scheduler = unsigned 64-bit timestamp per thread normalized to a common timebase (1s = 2^63−1 ticks; increment = cycles × scalar), scales to N:N chip topologies. Cooperative threads yield only at sync points; trade-off: dramatically clearer chip code (~half the LOC of state machines) but slower, and **save states are harder** (thread stacks must be unwound to serializable boundaries).

**Lessons**: this is the gold standard for *correctness*, and a warning for RetroForge: cooperative-thread purity conflicts with cycle-hooking enhancement layers and cheap savestates. A scanline/cycle-stepped state-machine core (Mesen-style) is friendlier to hooks, rewind, and deterministic scripting.

## 4. BizHawk / FCEUX — scripting + determinism

**Status 2026 (verified)**: BizHawk **2.10 (May 1, 2026)**, 2.11.x current; very active (TASEmulators). https://github.com/TASEmulators/BizHawk

**Lua API surface** (https://tasvideos.org/Bizhawk/LuaFunctions) — the de-facto standard an enhancement platform should meet or exceed:
- `memory`/`mainmemory`: typed reads/writes (u8..s32, LE/BE) across named **memory domains**.
- `event`: `onframestart/onframeend`, `on_bus_read/write/exec` (address-triggered callbacks), `onloadstate/onsavestate`, `oninputpoll`.
- `gui`: drawRectangle/Line/Text/Image, canvases, pixel-space text overlays.
- `emu`: frameadvance, register get/set, lag detection, cycle counts. Plus `joypad` (read/inject input), `savestate`, `movie`, `client`.
- **Determinism contract**: same game + core + sync settings + input ⇒ identical execution; all drawing is side-effect-free w.r.t. emulation. FCEUX pioneered this API shape on NES; BizHawk generalized it.

**Lessons**: address-triggered callbacks + memory domains + overlay-only drawing is the proven script contract; determinism (input log as ground truth) is what makes rewind, TAS, and A/B enhancement testing possible. Design the enhancement engine as a *pure observer* of a deterministic core.

## 5. RetroArch / libretro — native platform vs core

**API constraints** (https://docs.libretro.com/development/cores/developing-cores/, https://www.libretro.com/index.php/api/):
- Core emits **one fixed-format framebuffer** (or one HW-rendered surface via `retro_hw_render_callback` GL/Vulkan) at fixed fps/audio rate; frontend owns the window, UI, shaders, save states, input. No multi-window, no scene-graph/compositor output, no core-owned UI beyond core options; single framebuffer is a long-standing pain point even for dual-screen systems (https://github.com/libretro/RetroArch/issues/11838).
- What *is* possible in-core: enlarged `retro_game_geometry` (bsnes-hd's widescreen core proves it), HW-context high-res rendering, core options for toggles.
- What is *not* reasonably possible: interactive map/level explorer panes, HD-pack authoring UI, debugger-grade viewers, multi-surface output (playfield + minimap window), arbitrary asset hot-reload UX — all frontend territory you don't control.

**Verdict for RetroForge**: build native (own windowing/compositor/UI, scene-graph output where sprites and BG layers are separate draw items — required for de-flicker, widescreen sprite policy, HD asset compositing, level visualization). **Later libretro export is feasible** for the "flattened" feature subset: composite the enhanced scene to one HW surface with wide geometry, expose toggles as core options. Plan the core/frontend seam accordingly.

## 6. Widescreen / map-reconstruction precedents

**wideNES (Daniel Prilik, ANESE, 2018)** — the key generic technique. https://prilik.com/blog/post/widenes/ · https://github.com/daniel5151/ANESE/blob/master/wideNES.md
- **Scroll observation**: watch PPUSCROLL ($2005) writes; per-frame deltas tell you exactly how the camera moved; paste each frame onto a growing canvas at the accumulated offset.
- **Wraparound heuristic**: PPUSCROLL is 8-bit; games roll it over and flip PPUCTRL nametable bits. wideNES ignores PPUCTRL and instead infers: unexpected jump to ~256 ⇒ moved left/up a screen; jump to ~0 ⇒ right/down.
- **HUD masking**: detect mid-frame mapper IRQs (e.g., SMB3 IRQ at scanline 195 pins the status bar) and drop scanlines after (or before, if IRQ fires early) the split; sprites are excluded entirely (kills sprite HUDs); respect PPUMASK left-8px masking.
- **Scene detection**: perceptual hash (sum of framebuffer pixels) per frame; a large frame-to-frame delta ⇒ scene transition ⇒ start new canvas. Hash+scroll map makes scenes **re-entrant** (recognize a revisited area, keep painting the same canvas).
- **Limitations (author-stated)**: false-positive scene cuts (palette-dependent threshold); games with nonstandard scrolling need per-game handling (Zelda scrolls vertically via PPUADDR, not PPUSCROLL); and fundamentally **off-screen sprites can't be shown** — enemies pop in/out at the original 256px boundary, since the game never simulates them off-camera. So wideNES gives widescreen *terrain*, not widescreen *gameplay*.
- Academic follow-up: **MappyLand** (AAAI 2021) generalizes fast, accurate map extraction for console games — https://cdn.aaai.org/ojs/18892/18892-52-22658-1-2-20211004.pdf
- NESticle-era "hacks" precedent: thin evidence of true widescreen there; NESticle's legacy is speed/UX, not enhancement — don't over-cite it. HD Mode 7 widescreen ROM-hack patches exist for individual SNES games to fix sprite pop-in (per bsnes-hd README's pointer to game patches).

**Lesson**: generic map reconstruction = (scroll-register telemetry) + (IRQ/raster-split awareness) + (perceptual-hash scene identity). Per-game quirks are handled by small "profiles," not per-game engines. Sprite pop-in is the hard wall; fixing it requires game-logic patches (ROM hacks) or speculative simulation — be honest in scoping.

## 7. Upscaling & filters

- **Pixel-art filters** (real-time, cheap, solved): xBRZ, ScaleFx, HQx, SuperXBR ship as RetroArch slang shaders; NNEDI3 neural doubler is ported but slow (luma-only tricks used). https://github.com/ZironZ/NNEDI3-Slang-Shaders · https://emulation.gametechwiki.com/index.php/Texture_filtering
- **Offline ESRGAN texture packs** (proven precedent): Mupen64 Rice-format packs, Dolphin/PCSX2 HD packs; workflow = dump textures → batch Real-ESRGAN/manual cleanup → load pack. E.g., SM64 RESRGAN pack (https://github.com/pokeheadroom/RESRGAN-16xre-upscale-HD-texture-pack); process write-up: https://readonlymemo.com/how-emulator-hd-texture-upscales-are-made/
- **Real-time full-model AI upscaling in 2026**: still not practical as a per-frame path for a 60fps emulator on typical hardware — Real-ESRGAN-class models cost tens of ms/frame; lightweight options (FSR-class spatial upscalers, compact CNNs/NNEDI3) work but add latency for modest gains over xBRZ on flat-shaded pixel art. Evidence here is forum/vendor-grade, not benchmarked papers — treat as directional. https://forums.libretro.com/t/ai-upscale/43471
- **Realistic RetroForge posture**: AI = **offline pack generation** (auto-generate a first-draft Mesen-style HD pack from recorded tiles via ESRGAN + palette-aware models, human-curated after), plus cheap real-time shader filters. Don't promise live neural upscaling.

## 8. Per-game level extraction (well-trodden ground)

- SMB1: full commented disassembly exists; level format documented; extraction from ROM data demonstrated end-to-end in Python (https://matthewearl.github.io/2018/06/28/smb-level-extractor/); editors: GreatEd (https://www.romhacking.net/utilities/1468/), web viewer (https://hlorenzi.github.io/smbview/).
- Zelda/SMW/etc.: large tool ecosystems (SMW Central, romhacking.net, Zophar utilities) — per-game decoders for flagship titles are a known-solvable, community-documented problem.
- **Lesson**: RetroForge's full-level visualization should be two-tier: **generic tier** = wideNES/MappyLand-style runtime reconstruction (works everywhere, terrain-only); **curated tier** = per-game ROM decoders for flagship titles (exact, includes object placement), leveraging existing format docs rather than reverse-engineering from scratch.

---

## Design implications for RetroForge

1. **Copy Mesen's HD-pack contract**: tile identity = content hash (tile bytes) + palette, declarative conditions (mirroring, neighbors, position, RAM checks, frame ranges) — proven, and existing NES HD packs become a compatibility target/import path.
2. **Ship a Pack Builder from day one** (record-while-playing → PNG sheets + manifest); it's the reason Mesen packs exist at all.
3. **Build the debugger substrate first** (event viewer, tilemap/sprite viewers, trace, memory domains) — every enhancement feature is a productized debugger view.
4. **Every enhancement needs a per-game override + auto-heuristic** (sprite-limit auto-re-enable, widescreen per-layer settings): games exploit hardware quirks, so global toggles alone always break something.
5. **Adopt wideNES's telemetry pattern** for generic map building: scroll-register deltas + IRQ split detection + perceptual-hash scene identity + re-entrant scene canvases; keep per-game quirks as small data-driven profiles (e.g., "Zelda scrolls via PPUADDR").
6. **Be honest about the sprite wall**: off-screen actors aren't simulated; widescreen/full-map shows terrain only unless paired with ROM patches. Design a "gameplay-extended" tier as explicitly per-game (patch/profile) work.
7. **Copy bsnes-hd's per-layer policy model**: widescreen enable/crop per BG layer, sprite policy (clip/safe/unsafe), window-effect fallback — expose as a per-game profile format.
8. **HD Mode 7 approach generalizes**: prefer "same hardware math at higher internal resolution" before asset replacement — no art needed, huge win for Mode 7-class effects.
9. **Choose a hook-friendly core architecture**: scanline/cycle-stepped state machines (Mesen-style) over byuu-style cooperative threads — cleaner savestates, rewind, deterministic hooks; accept the LOC cost. Use ares as accuracy oracle in CI, not as the design template.
10. **Meet the BizHawk Lua contract**: memory domains, typed r/w, `on_bus_read/write/exec`, frame/savestate events, overlay-only drawing, input injection — and keep the deterministic-replay guarantee (input log ⇒ identical run) since rewind, TAS, and enhancement A/B testing all depend on it.
11. **Scene-graph output natively; flatten for export**: internal renderer should emit layers/sprites as separate compositable items (needed for de-flicker, widescreen sprites, HD compositing, map view). Native app first; a later libretro core export (single wide HW surface + core options) is feasible for the subset — plan that seam now.
12. **AI = offline, filters = realtime**: auto-draft HD packs with ESRGAN offline; ship xBRZ/ScaleFx/NNEDI3-class shaders for realtime. Do not architect around live neural upscaling in 2026.
13. **Two-tier level maps**: generic runtime stitching (every game, imperfect) + curated ROM decoders for flagships (SMB1/Zelda formats are already documented) — the curated tier is cheap because the romhacking community did the format work.
14. **Plan for project longevity**: Mesen2 archived in 2026 despite being best-in-class; bsnes-hd dormant since 2021. Permissive licensing, docs, and community-handoff readiness are product features.
15. **Avoid**: promising "widescreen gameplay" generically (bsnes-hd's top complaint), sprite-limit removal without auto-fallback, building inside libretro's single-framebuffer box, and cooperative-thread cores if deep hooking is the product.

---

### Source index
Mesen HD packs: https://www.mesen.ca/docs/hdpacks.html · video options: https://www.mesen.ca/docs/configuration/video.html · Mesen2 archive: https://github.com/SourMesen/Mesen2 · MesenCE: https://github.com/nesdev-org/MesenCE
bsnes-hd: https://github.com/DerKoun/bsnes-hd · https://bsnes.org/docs/graphics/ · libretro core: https://www.libretro.com/index.php/bsnes-hd-beta-core-pushing-the-limits-of-the-snes-widescreen-and-ultrawide-support/
ares: https://ares-emu.net/ · byuu articles mirror: https://github.com/higan-emu/emulation-articles
BizHawk: https://tasvideos.org/Bizhawk/LuaFunctions · https://github.com/TASEmulators/BizHawk/releases
libretro: https://docs.libretro.com/development/cores/developing-cores/ · https://github.com/libretro/RetroArch/issues/11838
wideNES: https://prilik.com/blog/post/widenes/ · https://github.com/daniel5151/ANESE/blob/master/wideNES.md · MappyLand: https://cdn.aaai.org/ojs/18892/18892-52-22658-1-2-20211004.pdf
Upscaling: https://github.com/ZironZ/NNEDI3-Slang-Shaders · https://readonlymemo.com/how-emulator-hd-texture-upscales-are-made/ · https://forums.libretro.com/t/ai-upscale/43471
Level formats: https://matthewearl.github.io/2018/06/28/smb-level-extractor/ · https://www.romhacking.net/utilities/1468/ · https://hlorenzi.github.io/smbview/
