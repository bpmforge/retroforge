# RetroForge — Rust Stack Research Brief (July 2026)

Researched 2026-07-06. All versions verified against crates.io API and upstream repos on that date;
do not trust these numbers past ~Q3 2026 without re-checking.

## Recommendations

| Concern | Recommendation | Version (2026-07-06) | Notes |
|---|---|---|---|
| Window/event loop | **winit** | 0.30.13 | De facto standard; egui/eframe wrap it |
| UI framework | **egui + eframe** | 0.35.0 | Immediate-mode; what real Rust emulators use |
| Dockable panels | **egui_dock** | 0.20.1 | Tracks egui 0.35; tabs, splits, drag-to-dock |
| GPU | **wgpu** | 30.0.0 | Single abstraction over Vulkan/Metal/DX12/GL/WebGPU |
| Audio I/O | **cpal** | 0.18.1 | Low-level cross-platform stream API |
| Audio ring buffer | **rtrb** 0.3.4 or **ringbuf** 0.5.0 | — | Lock-free SPSC; TetaNES uses ringbuf |
| Resampler | **rubato** | 3.0.0 | Ratio-adjustable sinc resampler → dynamic rate control |
| Gamepad input | **gilrs** | 0.11.2 | Maintained (May 2026 release); SDL3 bindings still immature |
| Scripting (tier 1) | **mlua** | 0.12.0 | Lua 5.1–5.5/LuaJIT/Luau; matches BizHawk/Mesen user culture |
| Plugins (tier 2) | **wasmtime** (+ wasmtime-wasi) | 46.0.1 | WASM component model, hard sandbox |
| Save states | **serde + bincode** | bincode 3.0.0 | Wrap in explicit versioned envelope (see §6) |
| Alt. save format | **postcard** | 1.1.3 | Stable documented wire format, smaller output |

Real-world validation: **TetaNES** (most active Rust NES emulator, v0.14.x, June 2026) ships on
exactly this stack — wgpu 29 + egui 0.34 + winit 0.30 + cpal 0.17 + gilrs 0.11 + ringbuf 0.5 + bincode.
([Cargo.toml](https://github.com/lukexor/tetanes/blob/main/tetanes/Cargo.toml))

---

## 1. wgpu — GPU abstraction

- **Current: 30.0.0**, released 2026-07-01 (crates.io updated 2026-07-02). v29.x ran Mar–Jun 2026.
  wgpu ships a **major (breaking) release roughly quarterly** — this is its normal cadence, not a red flag.
- v30 breaking changes are mechanical, not architectural: `VertexState::buffers` now
  `&[Option<VertexBufferLayout>]`; integer shader inputs need explicit `@interpolate(flat)`;
  `BufferSlice::size()` returns `u64`. v30 adds HDR output via `SurfaceConfiguration::color_space`
  (relevant to an enhancement engine) and `i16`/`u16` in WGSL behind `SHADER_I16`.
  ([releases](https://github.com/gfx-rs/wgpu/releases), [CHANGELOG](https://github.com/gfx-rs/wgpu/blob/trunk/CHANGELOG.md))
- **WebGPU spec status**: W3C Candidate Recommendation (CR draft dated 2026-05-12), on track for full
  Recommendation late 2026. Shipping in Chrome, Edge, Safari, Firefox. ([w3.org/TR/webgpu](https://www.w3.org/TR/webgpu/),
  [publication history](https://www.w3.org/standards/history/webgpu/))
- **Verdict**: use wgpu as the *single* renderer abstraction. Raw Vulkan/Metal buys nothing for a
  2D-scanout + shader-enhancement workload and triples platform code. egui has a first-party wgpu
  backend (`egui-wgpu`), so UI and emulator output share one device/queue. Pin the major version
  (`wgpu = "30"`), budget ~half a day per quarterly bump. Risk: crates that depend on wgpu (egui)
  lag one major behind for a few weeks after each release — upgrade when egui does.

## 2. Frontend / UI

- **egui 0.35.0** (2026-06-25; 0.34.3 May 27, 0.34.2 May 4 — steady release train). 0.35 added an
  inspection protocol (AccessKit tree over port 5719, `EGUI_INSPECTION=1`) and `egui_mcp` for
  agent-driven UI testing — genuinely useful for automated debug-panel testing.
  ([releases](https://github.com/emilk/egui/releases))
- **egui_dock 0.20.1** (2026-06-28) supports egui 0.35: dockable/tearable tabs, splits — exactly the
  Mesen-style tile/memory/PPU viewer layout. Active repo (170 commits, egui Discord channel).
  ([github.com/Adanos020/egui_dock](https://github.com/Adanos020/egui_dock)) A newer alternative
  `egui_docking` adds tear-off floating windows if egui_dock falls short.
- **What real emulators use**: TetaNES → egui; plastic → egui (+ TUI); rustico → custom SDL-ish shell.
  No notable emulator uses Slint, iced, or Tauri for its main shell. Immediate mode fits emulators:
  debug panels re-render from live core state every frame with no sync layer.
- Alternatives (verified current): **iced 0.14.0** (Dec 2025) — Elm architecture, weaker docking
  story; **Slint 1.17.0** (2026-06-24) — declarative DSL, commercial licensing dimension, wrong shape
  for per-frame debug views; **Tauri 2.11.5** (2026-07-01) — webview frontend, IPC hop between core
  and UI is exactly wrong for a 60 fps memory viewer.
- **Verdict**: eframe + egui + egui_dock. Render the emulator framebuffer as a wgpu texture inside an
  egui panel (via `egui-wgpu` paint callbacks) or run your own wgpu surface with egui layered on top.

## 3. Audio — cpal + dynamic rate control

- **cpal 0.18.1** (2026-06-07) — actively maintained ([RustAudio/cpal](https://github.com/RustAudio/cpal)).
  Backends: CoreAudio, WASAPI (+ optional ASIO), ALSA/JACK, AAudio, WebAudio worklet.
- Standard emulator audio architecture (what TetaNES and libretro-style frontends do):
  1. Core thread produces samples into a **lock-free SPSC ring buffer** — `rtrb` 0.3.4 (designed
     realtime-safe) or `ringbuf` 0.5.0 (TetaNES's choice).
  2. cpal callback drains the ring; never allocate/lock in the callback.
  3. **Dynamic rate control**: measure ring fill level each callback; nudge the resample ratio by up
     to ~±0.5% around nominal (NES APU ~1.789 MHz → 48 kHz) so the buffer converges to half-full.
     Canonical reference: Arntzen, *Dynamic Rate Control for Retro Game Emulators* (libretro docs,
     https://docs.libretro.com/development/cores/dynamic-rate-control/).
  4. Resampler: **rubato 3.0.0** (2026-05-20) supports on-the-fly ratio adjustment
     (`SincFixedOut::set_resample_ratio_relative`) — purpose-built for this. For NES fidelity a
     hand-rolled blip-buffer (band-limited step) synth is the classic alternative.
- Latency expectation: 5–15 ms buffers are achievable on CoreAudio/WASAPI-shared; don't chase lower
  than one video frame. Evidence note: cpal has open issues about fixed buffer sizes on some
  backends (e.g. [#902](https://github.com/RustAudio/cpal/issues/902) on Android) — request a buffer
  size but tolerate what you get.

## 4. Input — gilrs

- **gilrs 0.11.2**, updated 2026-05-30 on crates.io — maintained, if slow-moving. SDL_GameController-
  style unified mappings (SDL controller DB), hotplug, force feedback on some platforms.
  ([docs.rs/gilrs](https://docs.rs/gilrs/latest/gilrs/))
- Used by TetaNES (with `serde-serialize` feature for bindable config) and widely in Bevy ecosystem.
- Alternatives: `sdl3` / `sdl3-sys` Rust bindings exist but are young; SDL2 bindings drag in the C
  dependency for little gain. Keyboard/mouse come free via winit.
- **Verdict**: gilrs. Thin adapter trait around it so a later SDL3 swap stays cheap.

## 5. Plugin sandboxing / scripting — two tiers

- **What incumbents do**: BizHawk and Mesen 2 both expose **Lua** to users (memory read/write,
  frame-advance callbacks, drawing overlays) — it's the lingua franca of TAS/romhack tooling
  ([tasvideos.org/Bizhawk/LuaFunctions](https://tasvideos.org/Bizhawk/LuaFunctions),
  [tasvideos.org/LuaScripting](https://tasvideos.org/LuaScripting)). Neither uses WASM. No emulator
  we could find ships wasmtime-based plugins yet — that would be novel, not proven, territory.
- **mlua 0.12.0** (2026-07-05 — released *this week*; 0.11.6 is the prior stable line). Lua
  5.1–5.5, LuaJIT, and Luau; sandboxing knobs (instruction count hooks, memory limits, stripped
  stdlib). ([github.com/mlua-rs/mlua](https://github.com/mlua-rs/mlua))
- **wasmtime 46.0.1** (2026-06-24). Component model is production-real in 2026: wasmtime is the
  reference implementation; WASI 0.2 is stable; WASI 0.3 (async, `stream<T>`/`future<T>`) shipped in
  wasmtime 43+. ([releases](https://github.com/bytecodealliance/wasmtime/releases),
  [component-model docs](https://component-model.bytecodealliance.org/running-components/wasmtime.html))
- **Recommended two-tier approach**:
  - **Tier 1 — mlua (Luau or 5.4)**: user scripting — memory watch, cheats, overlays, TAS hooks.
    In-process, ~zero call overhead (matters at per-frame/per-scanline granularity), compatible with
    the existing emulator-Lua idiom. Trust model: "user runs scripts they chose," soft limits only.
  - **Tier 2 — wasmtime components**: third-party *plugins* (video filters, cores, achievement
    engines) where you want a hard capability sandbox and language independence. Define a WIT world
    (`frame-in → frame-out`, capability-gated memory access). Cost: host-boundary copies per frame —
    fine for per-frame filters, wrong for per-instruction hooks.
  - Ship Tier 1 first; Tier 2 only when a real third-party plugin need appears.

## 6. Save-state serialization

- **bincode 3.0.0** (2025-12-16) — note bincode ≥2 has its own `Encode`/`Decode` derive traits; serde
  interop is behind the `serde` feature. Fastest option, non-self-describing.
  ([crates.io/crates/bincode](https://crates.io/crates/bincode))
  > **Correction recorded 2026-08-02** (implementation-time verification, W0 run): the version
  > number above is wrong in effect. `bincode 3.0.0` on crates.io is a **placeholder** published by
  > bincode-org whose entire `src/lib.rs` is `compile_error!("https://xkcd.com/2347/")` — depending
  > on it fails to build. The usable release providing the `Encode`/`Decode` derives described here
  > is **2.0.1** (compile-verified: `encode_to_vec` / `decode_from_slice` + zstd roundtrip). The
  > research claim about the ≥2 derive API is correct; only the version numeral was wrong.
  > `bincode-next` 3.1.1 (Apich-Organization) and `oxicode` 0.2.5 (cool-japan) are third-party
  > forks, rejected for a PUBLIC format. Normative pins corrected in TECH_STACK §2 + SAVE_STATES §2.
- **postcard 1.1.3** — the alternative if you want a *documented, committed-stable* wire format
  (unchanged since 0.1) at ~70% of bincode's size, ~1.5x slower.
  ([postcard 1.0 post](https://jamesmunns.com/blog/postcard-1-0-run/))
- Neither format is version-tolerant by itself. **Versioned snapshot pattern** (standard practice):
  - Envelope: magic bytes + `format_version: u32` + core ID + optional zstd flag, then the payload.
  - On load, match version → deserialize into that version's struct → migrate forward. Keep old
    snapshot structs around (cheap for console-sized state).
  - Crates that automate this: **savefile 0.20.4** (2026-06-14, per-field `#[savefile_versions]`)
    and `versionize` (bincode-backed, from the Firecracker project). savefile is the more active.
    ([docs.rs/savefile](https://docs.rs/savefile), [docs.rs/versionize](https://docs.rs/versionize))
  - For rewind (many snapshots/sec), serialize with bincode into a reused `Vec<u8>` ring; only the
    on-disk format needs the version envelope.
- **Verdict**: serde derives on core state + bincode 3 payload inside a hand-rolled version envelope;
  adopt savefile only if migration bookkeeping becomes painful.

## 7. Rust emulator projects worth studying (verified July 2026)

| Project | Status | Why study it |
|---|---|---|
| **TetaNES** ([lukexor/tetanes](https://github.com/lukexor/tetanes)) | **Active** — v0.14.1 Apr 2026, tetanes-core 0.14.2 Jun 2026 | Best-in-class Rust NES: wgpu+egui+cpal+gilrs, 30+ mappers, WASM build, clean core/frontend split (`tetanes-core` on crates.io) |
| **plastic** ([Amjad50/plastic](https://github.com/Amjad50/plastic)) | Alive, low activity | NES with egui *and* TUI frontends; small readable core |
| **rustico** ([zeta0134/rustico](https://github.com/zeta0134/rustico)) | Active-ish; monorepo rename of rusticnes-core | Very accurate APU work; author is an NESdev regular |
| **pinky** ([koute/pinky](https://github.com/koute/pinky)) | Dormant (historic) | Cycle-accurate design + test-suite methodology still worth reading |
| **rsnes** ([nat-rix/rsnes](https://github.com/nat-rix/rsnes)) | WIP, incomplete | One of few Rust SNES attempts; runs DKC/SMW/F-Zero partially |
| **ares-rs** | **Does not exist.** ares ([ares-emulator/ares](https://github.com/ares-emulator/ares)) is C++ and has no Rust port | — |

**Honest gap**: there is **no mature Rust SNES core** in 2026. NES-in-Rust is a solved, crowded space;
SNES (65C816 + 5A22 timing, SA-1/SuperFX chips) is not. Plan to study C++ references (ares, bsnes,
Mesen 2) for SNES accuracy behavior, and treat the SNES core as the project's main original work.

---

### Source index
crates.io API (versions, fetched 2026-07-06) · [wgpu releases](https://github.com/gfx-rs/wgpu/releases) ·
[W3C WebGPU](https://www.w3.org/TR/webgpu/) · [egui releases](https://github.com/emilk/egui/releases) ·
[egui_dock](https://github.com/Adanos020/egui_dock) · [cpal](https://github.com/RustAudio/cpal) ·
[libretro dynamic rate control](https://docs.libretro.com/development/cores/dynamic-rate-control/) ·
[gilrs](https://docs.rs/gilrs/latest/gilrs/) · [mlua](https://github.com/mlua-rs/mlua) ·
[wasmtime releases](https://github.com/bytecodealliance/wasmtime/releases) ·
[component-model.bytecodealliance.org](https://component-model.bytecodealliance.org/running-components/wasmtime.html) ·
[BizHawk Lua](https://tasvideos.org/Bizhawk/LuaFunctions) · [postcard 1.0](https://jamesmunns.com/blog/postcard-1-0-run/) ·
[savefile](https://docs.rs/savefile) · [TetaNES](https://github.com/lukexor/tetanes) ·
[plastic](https://github.com/Amjad50/plastic) · [rustico](https://github.com/zeta0134/rustico) ·
[pinky](https://github.com/koute/pinky) · [rsnes](https://github.com/nat-rix/rsnes)
