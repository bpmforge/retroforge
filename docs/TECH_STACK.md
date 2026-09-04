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
| Trace compression | **lz4_flex** | 0.14.0 | Trace-to-file framing for ticket W4-10a (`docs/design/DEBUGGER.md` §2: "background writer with lz4 framing"). Chosen over the `lz4` crate deliberately: `lz4` is C bindings, which adds a build-time toolchain requirement and `unsafe` across an FFI boundary in a workspace whose lints flag `unsafe_code`; `lz4_flex` is pure Rust, MIT, no-unsafe-by-default. Frame API verified against the vendored 0.14.0 source rather than recall (`FrameEncoder::new(w)`, `io::Write`, `finish()`), since 0.14 is newer than most training data. `cargo deny check licenses` passes clean; `Cargo.lock` gains it plus `twox-hash` (the frame format's xxhash checksum). | `FrameEncoder::finish()` is **not optional** — the encoder buffers, so a frame that is merely dropped produces a truncated file that decoders reject, after the UI has reported the trace saved. `crate::trace_capture::write_loop` calls it explicitly and the test decompresses what it wrote rather than checking the file is non-empty |
| Annotation JSON | **serde_json** | 1.0 (resolves to 1.0.151, already in `Cargo.lock`) | Direct dependency of `rf-debugger` as of ticket W13-02f. `docs/design/DEBUGGER.md` §4 names JSON as the annotation import/export format, and this ticket made that same format the on-disk store (one format, not two — `crate::annotation_store`'s module doc says why). Was already resolved transitively, so this adds **no new licence surface**: `cargo deny check licenses` reports the same single pre-existing rejection (`libfuzzer-sys`, NCSA) before and after. | Import goes through `AnnotationStore::add`, never straight through `Deserialize` — FR-DBG-005's required `source` lives in `add`, not in the type, so a hand-edited file could otherwise put an unsourced annotation into a store every other path assumes cannot hold one |
| UI testing | **egui_kittest** | 0.35.0 (dev-dependency of `retroforge` only) | Headless, AccessKit-driven UI harness for ticket W4-09's CI smoke flow (R-A1). Published from **emilk/egui itself**, same repo and release train, so it satisfies rule 2's lockstep requirement instead of dragging a second egui version in — verified by resolving it (`Cargo.lock` gains exactly two crates, `egui_kittest` and `kittest`; AccessKit was already in the tree). Its `eframe` feature turns on `eframe/accesskit` and gives `eframe::CreationContext::_new_kittest`, so the smoke test drives the **real** `RetroForgeApp` rather than a test-only shim. `cargo deny check licenses` passes clean with it added. | **The UI row above is wrong and this row is the correction, since amending that row is outside W4-09's write scope.** It says egui "0.35 inspection protocol enables agent-driven UI tests"; checked against the crates, that conflates two things and neither is a CI mechanism. `egui::Context::inspection_ui` is a debug-info **panel** you draw into a `Ui`. `egui_inspection` + `egui_mcp` is an MCP **server** driving a live app over a socket — an interactive agent tool, useful, but not something CI can run. Ticket W4-09's acceptance inherited the same error ("via egui 0.35 inspection protocol"); it was built against `egui_kittest` instead, recorded as a ruling to veto. The `wgpu` and `snapshot` features are deliberately NOT enabled: `wgpu` would pull a second wgpu resolution path into a dev-dependency graph rule 5 mechanically enforces to one version, and `snapshot` adds `dify`/`image`/`open`/`tempfile` — four new crates and a licence surface — for image diffing this flow does not do |
| PNG decoding | **image** | 0.25, `default-features = false`, `features = ["png"]` — a **runtime** dependency of `retroforge` as of ticket W11-05, having been dev-only before | A Mesen HD pack is a `hires.txt` beside PNG tilesets, so applying one at runtime needs a PNG decoder in the shipped binary. Deliberately kept to the `png` feature: the default set pulls every codec the crate supports (JPEG, GIF, WebP, TIFF, …) for a format that is PNG by its own spec. `rf_enhance::hdpack` decodes **no** pixels on purpose — it takes only `ImageInfo` dimensions from the caller — so the decoder lives here, in the one crate that already owns image I/O | This crate was previously listed as dev-only ("nothing in the shipped binary touches it", W10-03 screenshots) and that sentence is no longer true — this row is the correction. Note the `egui_kittest` row's reasoning for *not* enabling its `snapshot` feature cites `image` as new licence surface; that surface is now accepted deliberately rather than acquired by accident. **Verified, not assumed:** `cargo deny check licenses` was run before and after adding it and reports the *same single* rejection either way — `libfuzzer-sys 0.4.13`'s `NCSA`, a pre-existing fuzzing dependency with nothing to do with this row. `image` and its transitive tree add no rejected licence. The dev-dependency entry is left as-is: features unify per build, so the lib build takes `png` alone |
| Docking | **egui_dock** | 0.20.1 | Mesen-style dockable viewer layout | Fallback: egui_docking (tear-off windows) |
| File dialog | **rfd** | 0.17.2 | Native "Open ROM" dialog (macOS/Windows/Linux); synchronous `FileDialog::new().pick_file() -> Option<PathBuf>` API, no async runtime needed for the native backends this project targets | On macOS pulls the `objc2`/`objc2-app-kit` binding stack (clean tree, verified via scratch-crate resolve — no wgpu/egui overlap); `cargo deny check licenses` passes clean for this dependency specifically (its own licence risk is nil; the failures found in this ticket are all `eframe`/`egui-winit` transitive, see the UI row above) |
| Archive | **zip** | 8.6.0 | Open a zipped ROM (ticket W2-13): `.zip` is how homebrew and PD titles are normally distributed — the project's own fetched `alter_ego.zip` is the motivating case. Declared `default-features = false, features = ["deflate"]`: deflate is what real ROM archives actually use, and the default feature set additionally pulls bzip2/lzma/xz/zstd/ppmd decoders plus `getrandom` and time crates that nothing here needs | **Production home: `crates/retroforge` only, deliberately NOT in `rf-cart`** — `rf-nes` depends on `rf-cart`, so putting it there would drag flate2/zlib-rs and zopfli (a *compressor*) into the NES core; the default-off-feature workaround that would hide it is the tell that the placement is wrong. See W2-13's notes and `crate::system`'s "Mapper scope" precedent for deferring an abstraction until a second consumer exists. **Second, dev-only home added by ticket W2-11: `crates/rf-harness`'s `[dev-dependencies]`**, exercised only by `tests/alter_ego_replay.rs` (unpacking the fetched `alter_ego.zip` to run the 5-minute replay regression). This does NOT reopen the reasoning above: the "keep it out of the NES core" argument is specifically about `rf-cart` (which `rf-nes` depends on, unconditionally, in every build), not about "any crate that can reach `rf-nes`" — `rf-harness` already depends on `rf-nes` directly as the test harness (`scripts/validate-arch.sh` rule 3's named exemption) and this is a `[dev-dependencies]` entry, so it never ships in any `rf-harness` binary and never touches `rf-cart` or `rf-nes`'s own dependency tree at all. Licence-cleared before adoption by scratch-crate resolve: all 16 transitive crates are MIT / Apache-2.0 / Zlib / Unlicense / 0BSD-or-MIT-or-Apache, every one already on `deny.toml`'s allowlist — **no new per-crate exception needed**, unlike the W1-06 GUI stack |
| Audio I/O | **cpal** | 0.18.1 | Cross-platform low-level streams (CoreAudio/WASAPI/ALSA) | Backend buffer-size quirks → request-but-tolerate (R-15). ⚠️ 0.18's `SampleRate`/`ChannelCount` are plain `u32`/`u16` type aliases, not 0.15's newtypes. **Behind the `device`/`audio` features** (W2-05) so `cargo test --workspace` needs no ALSA headers on the CI runner |
| Audio ring | **rtrb** | 0.3.4 | Lock-free SPSC, realtime-safe; never allocate/lock in callback | ringbuf 0.5 acceptable alternative |
| Resampler | **rubato** | 3.0.0 | On-the-fly ratio adjustment (`set_resample_ratio_relative`) = dynamic rate control per libretro reference | ⚠️ **3.0's API is not 0.x's** (W2-05, verified against vendored source): no `FastFixedIn` — it is `Async::new_poly(..., FixedAsync::Input)`, and buffers cross as `audioadapter` adapters, so **`audioadapter-buffers` 3.0 is a direct dependency too**. ⚠️ `max_resample_ratio_relative` is **reciprocal-symmetric**: passing 1.005 allows [0.995025, 1.005] and *rejects* an additively symmetric 0.995. Hand-rolled blip-buffer possible later for NES APU fidelity |
| Gamepad | **gilrs** | 0.11.2 | Maintained; SDL mapping DB; hotplug | Wrapped in `rf_input::PadBackend` as planned (W2-06) — which is also what makes hotplug testable without a physical controller. **Behind the `gilrs`/`gamepad` features**, off by default: gilrs reaches for evdev/udev on Linux and the CI runner installs only cc65 |
| Scripting | **mlua** | 0.12.0 | Lua = emulator-community lingua franca (BizHawk/Mesen); in-process, ~zero per-frame overhead; sandbox knobs | Tier 2 **is now adopted** — see the row below. This cell previously read "only when third-party demand is real"; Brad declared that condition MET on 2026-08-20 (ticket W9-03). |
| Plugins (Tier 2) | **wasmtime** (component model) | **47.0.4** | The WASM component tier of `PLUGINS.md` §1 / FR-PLUG-006, alongside the Lua tier rather than replacing it: a WIT world states what a plugin may call, and the capability sandbox refuses everything not granted. Ticket W9-03, on Brad's 2026-08-20 ruling. | **Version correction: the mlua row said "wasmtime 46" and the adopted version is 47.0.4.** 48.0.0 exists but requires Rust 1.95.0 and this workspace pins 1.94, so 47 is the newest resolvable — not a preference. ⚠️ **`default-features = false` is a LICENCE decision and is load-bearing.** With default features the tree resolves to **244 crates** and carries two NFR-011 problems: `ittapi`/`ittapi-sys` at **`GPL-2.0-only OR BSD-3-Clause`** (the Intel VTune profiling hook — dual-licensed so BSD-3 is electable under the libzstd precedent, but nothing here profiles), and the whole `icu_*`/`zerovec`/`tinystr` family at **`Unicode-3.0`**. With `runtime,cranelift,component-model,std` the tree is **114 crates** and both families are gone, so `cargo deny --all-features check licenses` passes with **no allowlist change** — which mattered because `deny.toml` is outside W9-03's write_scope. Verified 2026-08-21 that `wasmtime@47.0.4` and `cranelift-codegen@0.134.4` are genuinely in the `--all-features` graph and `ittapi` is absent; `icu_properties@2.2.0` IS in the workspace graph but was already there via the W1-06 frontend stack (confirmed by re-resolving without this dependency) and is already covered by the 2026-08-03 per-crate exceptions. ⚠️ Unlike `ort`, this is an unconditional dependency rather than a feature: it is pure Rust, needs no system library and no build-time network, so the reasons that gate `cpal`/`gilrs`/`ort` do not apply — and a tier that only exists behind an off-by-default feature is not "alongside the Lua tier". |
| Hashing | **sha2, sha1, md-5, crc32fast** | sha2/sha1/md-5 0.11, crc32fast 1.5 | Normalized-ROM identity: SHA-256 primary, SHA-1 + MD5 (RetroAchievements), CRC32 (No-Intro) | RustCrypto 0.11 breaks the 0.10 `format!("{:x}", ...)` hex idiom (`finalize()` returns `Array<u8,N>`, no `LowerHex`) — rf-cart hex-encodes manually. See docs/research/game-identity-and-re-data.md |
| Save states | **serde + bincode** | bincode **2.0.1** | Fastest; wrapped in hand-rolled versioned TLV envelope (design/SAVE_STATES.md) — the *container* is ours, codec is swappable | ⚠️ bincode ≥2 has own Encode/Decode derives; **never write bincode 1.x idioms** (R-11). ⚠️ **`bincode 3.0.0` is NOT a release** — bincode-org published it as a placeholder whose entire `lib.rs` is `compile_error!("https://xkcd.com/2347/")`; pinning `"3"` fails the build (corrected 2026-08-02, was pinned 3.0 from 2026-07-06 research). Do not "upgrade" to it. `bincode-next`/`oxicode` are third-party forks — rejected for a PUBLIC format (CONTRACTS §3). postcard 1.1 remains the sanctioned alternative if wire-stability ever outranks speed |
| Compression | **zstd** | 0.13.3 | State/canvas/cache compression | — |
| Config/profiles | **toml** | **1** (1.1.4+, spec 1.1.0) | Human-authored, diffable profiles (design/GAME_PROFILES.md); also the test-ROM manifest (tests/rom-manifest.toml, W0-03) | serde-compatible. Pinned to major `1` at W0-03 (was "current") |
| Component-WAT (dev only) | **wat** | 1 | **`[dev-dependencies]` of `rf-plugin-sdk` only.** Compiles component WAT text into real component bytes so W9-03's criterion 3 is tested against an ACTUAL component that imports an ungranted interface, rather than against the error type's `Display`. Brad's ruling calls that criterion "the one that decides whether the sandbox is real", and a unit test over the name mapping alone leaves the sandbox itself unexercised. | Published from the same `bytecodealliance/wasm-tools` repo wasmtime itself consumes, so it tracks the same component-model spec rather than a second implementation of it. Ships in nothing — but `deny.toml` sets `include-dev = true` precisely because NFR-011 says "every third-party dependency" with no dev carve-out, so it is covered: `cargo deny --all-features check licenses` passes with it present, no allowlist change. |
| Benchmarks | **criterion** | 0.8.2 | Perf gates in CI (TESTING.md §7); first real usage: `rf-nes` dev-dependency, `crates/rf-nes/benches/cpu_step.rs` (W1-01a) | `criterion::black_box` is deprecated in 0.8 in favor of `std::hint::black_box` — use the latter |
| AI upscaling | **ort** (ONNX Runtime) | **2.0.0-rc.13 — a RELEASE CANDIDATE, and behind the off-by-default `onnx` feature of `rf-ai` precisely because of that** | Local-first offline pack generation (FR-AI-001..004, ticket W8-10, on Brad's 2026-08-20 ruling). `Enhancer` jobs run the model OFF the frame path — the composer never calls it. | **There is no stable `ort` 2.x**: 2.0.0-rc.13 is the newest publish, and 1.16's API is a different shape. Depending on an RC is a real risk and the mitigation is containment, not optimism — `onnx` is off by default, so `cargo test --workspace` never builds it and CI never sees it (same posture as `cpal` behind `device`/`audio` and `gilrs` behind `gamepad`). ⚠️ **`default-features = false` is load-bearing.** The default set enables `download-binaries` + `tls-native`, which makes the *build* reach the network for prebuilt ONNX Runtime binaries and resolves **73 crates** including `webpki-root-certs` under **CDLA-Permissive-2.0** — not on `deny.toml`'s allowlist, so it would need a per-crate NFR-011 exception. With `load-dynamic` instead the tree is **20 crates** and `cargo deny --all-features check licenses` passes with **no allowlist change at all** (verified 2026-08-21: `ort`/`ort-sys` are MIT-OR-Apache-2.0, `libloading@0.9.0` is ISC, all three already allowed). That mattered doubly for W8-10, whose write_scope excludes `deny.toml`. ⚠️ `ndarray` deliberately NOT enabled — `Tensor::from_array` takes a plain `(shape, data)` tuple (verified against the vendored source, `src/value/impl_tensor/create.rs:117`), so it would buy nothing; same reasoning as the `zip` row. ⚠️ `load-dynamic` means the runtime library is **dlopen'd at run time** from a path we choose — no build-time download, no vendored blob in git, and a missing runtime is a clean runtime error rather than a build failure on a machine that will never run inference. ⚠️ **Float inference is not bit-reproducible** across runtime versions or execution providers, so `model_id` alone does not pin output pixels — see `docs/design/AI_UPSCALING.md` §4 for what "reproducible" is therefore allowed to mean. |

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
