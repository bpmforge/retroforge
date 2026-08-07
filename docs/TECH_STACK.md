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
| GPU | **wgpu** | **29.0.4 — direct dep in BOTH `retroforge` (via `eframe::wgpu`) and `rf-renderer` (pinned directly), same version, lockstep** | One abstraction → Vulkan/Metal/DX12/GL/WebGPU; egui first-party backend shares device/queue | **Corrected 2026-08-03 (W1-06 pre-flight): this row said 30.0, which is impossible today.** `egui-wgpu 0.35.0` depends on `wgpu 29.0.4`, and 0.35.0 is the newest egui/eframe published; wgpu 30.0.0 exists but no egui release pairs with it. Verified by resolving eframe 0.35.0 in a scratch crate. Pinning both would put **two** wgpu versions in the tree and break the shared-device/queue design, since `wgpu30::Device` and `wgpu29::Device` are distinct types. **RULING amended W3-01 (2026-08-07):** the original rule said "take wgpu through `eframe::wgpu`, never a direct dep" — written when the app shell (which depends on `eframe`) was the only wgpu consumer. `rf-renderer` (ticket W3-01's headless original pipeline) does **not** depend on `eframe` and must not — it is a host-service crate, and pulling the whole GUI framework in just to reach a type would be worse than the problem being avoided. `rf-renderer` therefore takes wgpu as a **direct** dependency, pinned to the **same** version eframe resolves (29.0.4 today), so cargo unifies them into exactly one `wgpu` in `Cargo.lock` — verified empirically (`grep -c '^name = "wgpu"$' Cargo.lock` == 1, now mechanically enforced by `scripts/validate-arch.sh` rule 5, not just this doc). The rule's *purpose* (exactly one wgpu version, ever) survives unchanged; only its mechanism does. **Standing obligation: `rf-renderer`'s pin moves in LOCKSTEP with egui's whenever egui bumps** — a stale pin against a newer egui reintroduces the two-version trap this whole rule exists to prevent (rule 2 below already says never bump wgpu/egui independently; this is that rule applied to a second crate) |
| Window/events | **winit** | 0.30.13 | De-facto standard; eframe wraps it | — |
| UI | **egui + eframe** | 0.35.0 | Immediate mode fits per-frame debug views; what real Rust emulators use; 0.35 inspection protocol enables agent-driven UI tests | Complex docking edge cases → R-08. **API drift confirmed 2026-08-03 (W1-06):** `eframe::App::ui` now takes `&mut egui::Ui` (not `&egui::Context`); `TopBottomPanel`/`SidePanel` were removed in favor of a unified `egui::Panel::top/bottom/left/right(id).show(ui, ...)`; `ui.close_menu()` renamed `ui.close()`. **Licence risk found (W1-06), NOT yet resolved:** `egui-winit`'s default features (`clipboard`→`arboard`→`clipboard-win`/`error-code`, Windows-only; `links`→`webbrowser`→`url`→`idna`→the `icu_*`/`zerovec`/`yoke`/`litemap`/`writeable`/`tinystr`/`potential_utf` family) plus `egui`'s bundled `epaint_default_fonts` pull three licence families `cargo deny check licenses` rejects against the current NFR-011 allowlist: `Unicode-3.0` (same family as the existing `unicode-ident` exception, just more crates), `BSL-1.0` (Boost Software License), and `(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0` (the embedded default font files). All three are OSI-approved permissive/font licences, not copyleft — no NFR-011 violation in spirit — but none is on `deny.toml`'s allowlist yet and W1-06 was instructed to report rather than silently add exceptions. Needs a licence-policy decision: widen the allowlist, or trim `default-features` on `eframe`/`egui-winit` (loses clipboard/hyperlink/bundled fonts) |
| Docking | **egui_dock** | 0.20.1 | Mesen-style dockable viewer layout | Fallback: egui_docking (tear-off windows) |
| File dialog | **rfd** | 0.17.2 | Native "Open ROM" dialog (macOS/Windows/Linux); synchronous `FileDialog::new().pick_file() -> Option<PathBuf>` API, no async runtime needed for the native backends this project targets | On macOS pulls the `objc2`/`objc2-app-kit` binding stack (clean tree, verified via scratch-crate resolve — no wgpu/egui overlap); `cargo deny check licenses` passes clean for this dependency specifically (its own licence risk is nil; the failures found in this ticket are all `eframe`/`egui-winit` transitive, see the UI row above) |
| Archive | **zip** | 8.6.0 | Open a zipped ROM (ticket W2-13): `.zip` is how homebrew and PD titles are normally distributed — the project's own fetched `alter_ego.zip` is the motivating case. Declared `default-features = false, features = ["deflate"]`: deflate is what real ROM archives actually use, and the default feature set additionally pulls bzip2/lzma/xz/zstd/ppmd decoders plus `getrandom` and time crates that nothing here needs | **Production home: `crates/retroforge` only, deliberately NOT in `rf-cart`** — `rf-nes` depends on `rf-cart`, so putting it there would drag flate2/zlib-rs and zopfli (a *compressor*) into the NES core; the default-off-feature workaround that would hide it is the tell that the placement is wrong. See W2-13's notes and `crate::system`'s "Mapper scope" precedent for deferring an abstraction until a second consumer exists. **Second, dev-only home added by ticket W2-11: `crates/rf-harness`'s `[dev-dependencies]`**, exercised only by `tests/alter_ego_replay.rs` (unpacking the fetched `alter_ego.zip` to run the 5-minute replay regression). This does NOT reopen the reasoning above: the "keep it out of the NES core" argument is specifically about `rf-cart` (which `rf-nes` depends on, unconditionally, in every build), not about "any crate that can reach `rf-nes`" — `rf-harness` already depends on `rf-nes` directly as the test harness (`scripts/validate-arch.sh` rule 3's named exemption) and this is a `[dev-dependencies]` entry, so it never ships in any `rf-harness` binary and never touches `rf-cart` or `rf-nes`'s own dependency tree at all. Licence-cleared before adoption by scratch-crate resolve: all 16 transitive crates are MIT / Apache-2.0 / Zlib / Unlicense / 0BSD-or-MIT-or-Apache, every one already on `deny.toml`'s allowlist — **no new per-crate exception needed**, unlike the W1-06 GUI stack |
| Audio I/O | **cpal** | 0.18.1 | Cross-platform low-level streams (CoreAudio/WASAPI/ALSA) | Backend buffer-size quirks → request-but-tolerate (R-15) |
| Audio ring | **rtrb** | 0.3.4 | Lock-free SPSC, realtime-safe; never allocate/lock in callback | ringbuf 0.5 acceptable alternative |
| Resampler | **rubato** | 3.0.0 | On-the-fly ratio adjustment (`set_resample_ratio_relative`) = dynamic rate control per libretro reference | Hand-rolled blip-buffer possible later for NES APU fidelity |
| Gamepad | **gilrs** | 0.11.2 | Maintained; SDL mapping DB; hotplug | Wrap in thin adapter trait (SDL3 swap stays cheap) |
| Scripting | **mlua** | 0.12.0 | Lua = emulator-community lingua franca (BizHawk/Mesen); in-process, ~zero per-frame overhead; sandbox knobs | Tier 2 = wasmtime 46 components, only when third-party demand is real |
| Hashing | **sha2, sha1, md-5, crc32fast** | sha2/sha1/md-5 0.11, crc32fast 1.5 | Normalized-ROM identity: SHA-256 primary, SHA-1 + MD5 (RetroAchievements), CRC32 (No-Intro) | RustCrypto 0.11 breaks the 0.10 `format!("{:x}", ...)` hex idiom (`finalize()` returns `Array<u8,N>`, no `LowerHex`) — rf-cart hex-encodes manually. See docs/research/game-identity-and-re-data.md |
| Save states | **serde + bincode** | bincode **2.0.1** | Fastest; wrapped in hand-rolled versioned TLV envelope (design/SAVE_STATES.md) — the *container* is ours, codec is swappable | ⚠️ bincode ≥2 has own Encode/Decode derives; **never write bincode 1.x idioms** (R-11). ⚠️ **`bincode 3.0.0` is NOT a release** — bincode-org published it as a placeholder whose entire `lib.rs` is `compile_error!("https://xkcd.com/2347/")`; pinning `"3"` fails the build (corrected 2026-08-02, was pinned 3.0 from 2026-07-06 research). Do not "upgrade" to it. `bincode-next`/`oxicode` are third-party forks — rejected for a PUBLIC format (CONTRACTS §3). postcard 1.1 remains the sanctioned alternative if wire-stability ever outranks speed |
| Compression | **zstd** | 0.13.3 | State/canvas/cache compression | — |
| Config/profiles | **toml** | **1** (1.1.4+, spec 1.1.0) | Human-authored, diffable profiles (design/GAME_PROFILES.md); also the test-ROM manifest (tests/rom-manifest.toml, W0-03) | serde-compatible. Pinned to major `1` at W0-03 (was "current") |
| Benchmarks | **criterion** | 0.8.2 | Perf gates in CI (TESTING.md §7); first real usage: `rf-nes` dev-dependency, `crates/rf-nes/benches/cpu_step.rs` (W1-01a) | `criterion::black_box` is deprecated in 0.8 in favor of `std::hint::black_box` — use the latter |
| Future AI | **ort** (ONNX Runtime) | Phase 8+ | Local-first offline pack generation | Not a dependency until Phase 8 |

Deliberately **not** used: SDL2 (C dependency for little gain over
winit+gilrs+cpal), Tauri/webview (IPC hop wrong for 60fps memory viewers),
iced/Slint (wrong shape for per-frame debug UIs), savefile (adopt only if
manual chunk migrations become painful).

### wgpu 29 API traps — VERIFIED by compile-probe 2026-08-06, not read off docs

Two subagent attempts at W3-01 stalled without landing a line of code. A
conductor compile-probe against the vendored `wgpu-29.0.4` source found the
API differs from every pre-29 idiom (and therefore from training data) in
three ways, each of which fails to compile rather than failing loudly:

| What you'd write from memory | What wgpu 29 actually needs |
|---|---|
| `InstanceDescriptor::default()` | **No `Default` impl.** Use `InstanceDescriptor::new_without_display_handle()` — which is exactly the headless constructor `rf-renderer` wants (or the `_from_env` variant to honour `WGPU_BACKEND`) |
| `Instance::new(&desc)` | Takes the descriptor **by value**: `Instance::new(desc)` |
| `let adapters = instance.enumerate_adapters(..)` | Returns an **`impl Future<Output = Vec<Adapter>>`** — must be awaited/blocked on |

`request_adapter` and `request_device` are likewise async and return
`Result`. A verified-working headless skeleton (Metal, Apple M5 Max, no
window):

```rust
let inst = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
let adapters = pollster::block_on(inst.enumerate_adapters(wgpu::Backends::all()));
let adapter = pollster::block_on(inst.request_adapter(&wgpu::RequestAdapterOptions::default()))?;
let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;
```

**Awaiting any of these in a sync context with no executor never returns** —
that, plus the `map_async` + `device.poll` readback pattern, is the most
likely cause of a hang. The headless path itself is proven to work in this
environment, so a hang means a blocking call, not a missing GPU.

**Readback-path traps — VERIFIED by attempt 3 (2026-08-07), landed and
working (`crates/rf-renderer/src/gpu.rs`, `original_pipeline.rs`).** The
skeleton above stops short of readback, which is exactly where attempt 1
froze ("verify the device-loss callback mechanism"). Checked against the
vendored `wgpu-29.0.4`/`wgpu-types-29.0.4` source, not memory:

| What you'd write from memory | What wgpu 29 actually needs |
|---|---|
| `device.poll(wgpu::Maintain::Wait)` | `Maintain` is gone; it's `PollType`, and **critically it takes a bound**: `device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(Duration::from_secs(10)) })` → `Result<PollStatus, PollError>`. **Always pass `timeout: Some(_)`, never `None`** — an unbounded wait is the leading suspect for both 600s stalls; a bounded one turns a real hang into `Err(PollError::Timeout)` you can report instead of silence |
| `wgpu::ImageCopyTexture` / `ImageCopyBuffer` / `ImageDataLayout` | Renamed to `TexelCopyTextureInfo` / `TexelCopyBufferInfo` / `TexelCopyBufferLayout` (used by `Queue::write_texture`, `CommandEncoder::copy_texture_to_buffer`) |
| `PipelineLayoutDescriptor { bind_group_layouts: &[&bgl], push_constant_ranges: &[] }` | `bind_group_layouts` is `&[Option<&BindGroupLayout>]` (wrap each in `Some`); `push_constant_ranges` is gone, replaced by `immediate_size: u32` (`0` if unused) |
| `RenderPassColorAttachment { view, resolve_target, ops }` | Gained a required `depth_slice: Option<u32>` field (`None` for a plain 2D target) |

`map_async`'s own shape (`BufferSlice::map_async(MapMode, callback)`) is
unchanged from pre-29 — the callback fires during `device.poll`, same as
always. The working pattern (bounded, reports rather than hangs):

```rust
let slice = buffer.slice(..);
let (tx, rx) = std::sync::mpsc::channel();
slice.map_async(wgpu::MapMode::Read, move |result| { let _ = tx.send(result); });
device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(Duration::from_secs(10)) })
    .map_err(|e| format!("readback timed out: {e}"))?;
match rx.try_recv() {
    Ok(Ok(())) => { /* slice.get_mapped_range() ... buffer.unmap() */ }
    Ok(Err(e)) => { /* map_async failed */ }
    Err(_) => { /* callback never fired even though poll returned -- report, don't retry */ }
}
```

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
6. **wgpu 30 specifics** (from release notes) — **these apply to wgpu 30,
   which this project is NOT on yet** (see the GPU row: egui 0.35 pins wgpu
   29). Keep them here for the eventual lockstep bump, and re-verify against
   the release notes then rather than trusting this list:
   `VertexState::buffers` is `&[Option<VertexBufferLayout>]`; integer shader
   inputs need explicit `@interpolate(flat)`; WGSL `i16`/`u16` behind the
   `SHADER_I16` feature.

## 4. Platform targets

- **Primary dev/CI**: Apple Silicon macOS (Metal). CI also Linux (Vulkan,
  software-rasterizer golden frames) + Windows (DX12) for releases.
- Rust pinned by `rust-toolchain.toml` (`channel = "1.94"` — a pinned
  line, never floating `stable`: the `-D warnings` gate would break on
  every Rust release otherwise);
  MSRV = whatever egui 0.35/wgpu 30 require — record on first CI failure.
- Distribution: GitHub releases, three OS targets; no package managers yet.
