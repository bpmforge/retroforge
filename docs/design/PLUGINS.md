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

### 1.1 The Tier-2 component world (ticket W9-03)

The WASM tier landed in `crates/rf-plugin-sdk/`: `wit/plugin.wit` is the
contract and `src/component.rs` is the host. Note what §1's table above
does **not** say — it names the tier and its isolation story but specifies
no world, so the world's shape is a W9-03 design decision recorded here
rather than a pre-existing requirement being implemented.

**One interface per capability, not one `host` interface.** That split is
what makes the sandbox mechanical instead of advisory: the host links an
interface *if and only if* the manifest grants the matching §2 capability,
so a plugin that imports `write-memory` without being granted it is
missing an import and cannot instantiate. Capability names match
`manifest::Capabilities` field-for-field.

**Nothing is stubbed.** There are deliberately no no-op host functions for
ungranted capabilities, because a stub is exactly the failure FR-PLUG-006's
sandbox exists to prevent: the plugin would run, do nothing, and report
success, and the user would be told a mod was active while it was inert.

Three refusals, each with its own diagnostic, and the distinction matters
to whoever has to act on it:

| Situation | Refusal | Whose problem |
|---|---|---|
| imports a capability the manifest does not grant | `CapabilitiesNotGranted`, listing **every** one at once | the plugin author's manifest |
| imports a capability granted and declared, but with no host side in this build | `CapabilityNotImplemented`, naming it | **ours** — the manifest is correct |
| imports an unknown interface, or one from another package | `UnknownImport` | neither; deny-by-default covers the unknown case |

Implemented host-side today: `read-memory`, `write-memory` (ledgered — §2's
mod boundary), `frame-events`, `scanline-events`. Declared in the world but
not yet implemented: `read-ppu`, `draw-overlay`, `replace-layers`,
`input-bindings`, `filesystem` — each needs renderer, input or cache
plumbing outside `rf-plugin-sdk`, and each is refused **by name** rather
than linked as an empty instance.

`world plugin`'s single export, `on-frame`, is likewise a W9-03 choice and
not something §1 dictated: it is the minimum a plugin needs to do anything
per-frame, and more exports are cheaper to add later than to remove.

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
input_bindings = false   # register hotkeys/virtual buttons; shown in the remap UI, conflict-checked
filesystem = "cache_dir" # none | cache_dir; exports go through the host `export` API (user-picked destination), never raw fs access
```

`write_memory` is the mod boundary: silently altering game behavior is
impossible because the host routes all writes through a ledger surfaced in
the UI and recorded in save states (PROF/ENHC chunks).

**Path containment (D-006, NFR-010):** the `filesystem = "cache_dir"` cap
is realpath-enforced — every path a plugin opens is canonicalized and must
resolve inside the plugin's cache dir; symlink escapes are refused with a
diagnostic naming the offending path. The host `export` API writes only to
a user-picked destination (native dialog), never a plugin-supplied path.
The same containment law covers library scan roots, `profiles.d`
references, and rf-cache (CONSTRAINTS §1).

**Intake (D-005, FR-PROF-007):** community-distributed plugins/packs/
profiles are deny-by-default — manifest requires SPDX `license` +
`provenance` fields; unknown/missing licenses and denylisted content (NC
assets, GFDL text, GPL-derived shader code) are rejected at install scan,
before any capability prompt. Enforcement CI lands Phase 9; the manifest
fields are schema-required from v0.

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
