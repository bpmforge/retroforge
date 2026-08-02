# RetroForge — Tech Stack

Status: Phase 3 decision record · 2026-07-06
Versions verified against crates.io/upstream on 2026-07-06
(`docs/research/rust-stack.md`). Re-verify before adding any dependency.

## 1. Language: Rust (decided)

Workspace of 16 crates (already scaffolded; edition 2021, MIT OR
Apache-2.0). Rationale: memory safety across a large long-lived codebase,
fearless-refactor for AI-agent-driven development, one toolchain from core
to UI to WASM-later. `unsafe_code = "warn"` workspace-wide.

**Proof-of-stack**: TetaNES (most active Rust NES emulator, v0.14.x 2026)
ships wgpu + egui + winit + cpal + gilrs + ringbuf + bincode — our exact
combination, validated for this exact workload.

## 2. Dependency decisions

| Concern | Crate | Version (2026-07-06) | Rationale | Risk / note |
|---|---|---|---|---|
| GPU | **wgpu** | 30.0 | One abstraction → Vulkan/Metal/DX12/GL/WebGPU; egui first-party backend shares device/queue | Breaking major ~quarterly (mechanical); upgrade in lockstep with egui (RISKS R-07) |
| Window/events | **winit** | 0.30.13 | De-facto standard; eframe wraps it | — |
| UI | **egui + eframe** | 0.35.0 | Immediate mode fits per-frame debug views; what real Rust emulators use; 0.35 inspection protocol enables agent-driven UI tests | Complex docking edge cases → R-08 |
| Docking | **egui_dock** | 0.20.1 | Mesen-style dockable viewer layout | Fallback: egui_docking (tear-off windows) |
| Audio I/O | **cpal** | 0.18.1 | Cross-platform low-level streams (CoreAudio/WASAPI/ALSA) | Backend buffer-size quirks → request-but-tolerate (R-15) |
| Audio ring | **rtrb** | 0.3.4 | Lock-free SPSC, realtime-safe; never allocate/lock in callback | ringbuf 0.5 acceptable alternative |
| Resampler | **rubato** | 3.0.0 | On-the-fly ratio adjustment (`set_resample_ratio_relative`) = dynamic rate control per libretro reference | Hand-rolled blip-buffer possible later for NES APU fidelity |
| Gamepad | **gilrs** | 0.11.2 | Maintained; SDL mapping DB; hotplug | Wrap in thin adapter trait (SDL3 swap stays cheap) |
| Scripting | **mlua** | 0.12.0 | Lua = emulator-community lingua franca (BizHawk/Mesen); in-process, ~zero per-frame overhead; sandbox knobs | Tier 2 = wasmtime 46 components, only when third-party demand is real |
| Hashing | **sha2, sha1, md-5, crc32fast** | sha2/sha1/md-5 0.11, crc32fast 1.5 | Normalized-ROM identity: SHA-256 primary, SHA-1 + MD5 (RetroAchievements), CRC32 (No-Intro) | RustCrypto 0.11 breaks the 0.10 `format!("{:x}", ...)` hex idiom (`finalize()` returns `Array<u8,N>`, no `LowerHex`) — rf-cart hex-encodes manually. See docs/research/game-identity-and-re-data.md |
| Save states | **serde + bincode** | bincode **2.0.1** | Fastest; wrapped in hand-rolled versioned TLV envelope (design/SAVE_STATES.md) — the *container* is ours, codec is swappable | ⚠️ bincode ≥2 has own Encode/Decode derives; **never write bincode 1.x idioms** (R-11). ⚠️ **`bincode 3.0.0` is NOT a release** — bincode-org published it as a placeholder whose entire `lib.rs` is `compile_error!("https://xkcd.com/2347/")`; pinning `"3"` fails the build (corrected 2026-08-02, was pinned 3.0 from 2026-07-06 research). Do not "upgrade" to it. `bincode-next`/`oxicode` are third-party forks — rejected for a PUBLIC format (CONTRACTS §3). postcard 1.1 remains the sanctioned alternative if wire-stability ever outranks speed |
| Compression | **zstd** | current | State/canvas/cache compression | — |
| Config/profiles | **toml** | current | Human-authored, diffable profiles (design/GAME_PROFILES.md) | serde-compatible |
| Benchmarks | **criterion** | current | Perf gates in CI (TESTING.md §7) | — |
| Future AI | **ort** (ONNX Runtime) | Phase 8+ | Local-first offline pack generation | Not a dependency until Phase 8 |

Deliberately **not** used: SDL2 (C dependency for little gain over
winit+gilrs+cpal), Tauri/webview (IPC hop wrong for 60fps memory viewers),
iced/Slint (wrong shape for per-frame debug UIs), savefile (adopt only if
manual chunk migrations become painful).

## 3. Rules for the coding agent

1. **Verify every API against docs.rs for the pinned version before use.**
   Training-data idioms are stale for wgpu (breaks quarterly), egui, and
   especially bincode (1.x `serialize`/`deserialize` free functions are a
   build error by policy — use `Encode`/`Decode` derives + `encode_to_vec`).
2. **Never bump wgpu or egui independently** — one lockstep PR, both majors,
   run the full golden-frame suite after.
3. **New dependency = TECH_STACK.md row first** (this file is the decision
   record), with version, rationale, risk. No transitive-only assumptions.
4. **No `unsafe`** without a justifying comment + a test exercising the
   invariant.
5. **Frame path discipline**: no allocation in the cpal callback; no locks
   shared with the core thread; async work goes through the job system.
6. **wgpu 30 specifics** (from release notes): `VertexState::buffers` is
   `&[Option<VertexBufferLayout>]`; integer shader inputs need explicit
   `@interpolate(flat)`; WGSL `i16`/`u16` behind `SHADER_I16` feature.

## 4. Platform targets

- **Primary dev/CI**: Apple Silicon macOS (Metal). CI also Linux (Vulkan,
  software-rasterizer golden frames) + Windows (DX12) for releases.
- Rust pinned by `rust-toolchain.toml` (`channel = "1.94"` — a pinned
  line, never floating `stable`: the `-D warnings` gate would break on
  every Rust release otherwise);
  MSRV = whatever egui 0.35/wgpu 30 require — record on first CI failure.
- Distribution: GitHub releases, three OS targets; no package managers yet.
