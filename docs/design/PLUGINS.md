# Design: Plugin System (`rf-plugin-sdk`)

## 1. Three tiers (deliberately staged)

| Tier | Tech | Ships | Use case | Isolation |
|---|---|---|---|---|
| Profiles | TOML data | Phase 4 | addresses, decode rules, settings | total (no code) |
| Scripts | Lua via mlua | Phase 4-5 | BizHawk/Mesen-idiom user scripting: overlays, research, quick per-game logic | in-process, capability-gated API surface |
| Plugins | native Rust trait objects (in-tree) → WASM/wasmtime component (Phase 9) | Phase 5+ | complex decoders, custom renderers, exporters | trait-gated now; sandboxed later |

Rationale (research-backed): every successful emulator scripting ecosystem is
Lua; no shipping emulator has WASM plugins yet — so Lua is the compatibility
idiom and WASM is our stability/sandbox play once the API has settled.
Keeping native plugins in-tree until Phase 9 avoids freezing a bad ABI.

## 2. Capability model

A plugin/script declares capabilities in its manifest; the UI shows them at
install/enable time; the host enforces them:

```toml
[plugin]
id = "example-minimap"
version = "0.1.0"
api = "0.1"
[capabilities]
read_memory = true       # StateView access
read_ppu = true          # VRAM/OAM/CGRAM views
frame_events = true
scanline_events = false  # costs perf; explicit
draw_overlay = true
replace_layers = false   # render override
write_memory = false     # MOD-tier: requires per-plugin user opt-in + red badge
filesystem = "cache_dir" # none | cache_dir
```

`write_memory` is the mod boundary: silently altering game behavior is
impossible because the host routes all writes through a ledger surfaced in
the UI and recorded in save states (PROF/ENHC chunks).

## 3. Host API (v0 sketch)

```rust
pub trait Plugin {
    fn manifest(&self) -> &Manifest;
    fn on_load(&mut self, host: &mut dyn Host) -> Result<()>;
    fn on_frame(&mut self, ctx: &FrameCtx) -> PluginOutput;   // after core frame
    fn on_event(&mut self, ev: &CoreEvent, ctx: &EventCtx);   // subscribed only
    fn panel(&mut self, ui: &mut PanelUi) {}                  // optional debug panel
}

pub struct FrameCtx<'a> {
    pub frame: u64,
    pub state: StateView<'a>,          // read-only core state
    pub rom: &'a RomView,              // read-only ROM bytes
    pub profile: Option<&'a Profile>,
    pub cache: &'a CacheHandle,
}

pub enum PluginOutput {
    None,
    Overlay(Vec<DrawCmd>),                    // draw_overlay cap
    Scene(Vec<SceneLayer>),                   // replace_layers cap
    Patches(Vec<MemPatch>),                   // write_memory cap (mod tier)
}
```

Lua scripts get the same surface via bindings (`rf.mem.read_u8`,
`rf.on_frame(fn)`, `rf.gui.rect/text/line`, `rf.rom.read`, `rf.cache`),
mirroring BizHawk naming where reasonable to ease porting of community
scripts.

## 4. Failure containment

- Script errors: caught per-callback, script auto-paused with error toast;
  never aborts emulation.
- Native plugin panics: `catch_unwind` at the host boundary, plugin disabled
  for the session.
- Per-frame time budget: host measures callback time; over-budget plugins get
  throttled to every-N-frames with a UI warning (protects frame pacing).
- WASM tier (later) adds memory/fuel limits and true isolation.
