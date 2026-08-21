# RetroForge Plugin SDK

Ticket **W9-04**. Covers both shipped tiers: **Lua** (`crates/rf-plugin-sdk/src/host.rs`,
ticket W5-07) and **WASM components** (`src/component.rs`, ticket W9-03).
Tier structure and the capability model are specified in
`docs/design/PLUGINS.md` §1 and §2; this document is how you *use* what
was built.

## 0. Every API in this document is compiled

Two example programs live beside the crate and are built by the workspace
gate — `cargo test --workspace` and
`cargo clippy --workspace --all-targets` both compile examples:

| Example | Tier | Shows |
|---|---|---|
| `crates/rf-plugin-sdk/examples/lua_player_marker.rs` | Lua | read memory by profile label, draw an overlay |
| `crates/rf-plugin-sdk/examples/component_capability_refusal.rs` | Component | one component, refused or accepted purely by its manifest |

```sh
cargo run -p rf-plugin-sdk --example lua_player_marker
cargo run -p rf-plugin-sdk --example component_capability_refusal
```

**This is the anti-rot mechanism, and it is why W9-04 is not a
markdown-only ticket.** If this document drifts from the API that ships,
the examples stop compiling and the gate goes red. Documentation that is
not compiled goes stale silently — this project has already been bitten,
by `EMULATION_CORES.md` §3.3's superseded `dropped_by_limit` line.

A test (`docs_examples_exist`) asserts the files named above are present,
so this table cannot outlive the examples either.

## 1. Which tier to write

| You want to… | Tier |
|---|---|
| poke at a game, iterate live, share a 30-line snippet | **Lua** |
| ship something others install, with a stable sandbox boundary | **Component** |

`PLUGINS.md` §1's reasoning, in one line: every successful emulator
scripting ecosystem is Lua, so Lua is the compatibility idiom; WASM is the
stability and sandbox play.

<!-- ANCHOR: capabilities -->
## 2. The manifest is the same for both tiers

```toml
id = "player-marker"
version = "0.1.0"
api = "0.1"
license = "MIT"
provenance = "where this came from"
read_memory = true
draw_overlay = true
```

Flat `key = value`; `[section]` headers are skipped by the parser.

**`license` and `provenance` are required and the parse fails without
them** — D-005 makes community intake deny-by-default, and
`ManifestError::MissingField` names which one is absent.

**Every capability defaults to denying.** A manifest that omits one, or a
build that adds a new one, denies rather than grants — so the failure
direction of an out-of-date manifest is always *less* access.

Capabilities (`manifest::Capabilities`): `read_memory`, `read_ppu`,
`frame_events`, `scanline_events`, `draw_overlay`, `replace_layers`,
`write_memory`, `input_bindings`, and `filesystem` (`none` | `cache_dir`
— never raw filesystem access; an unrecognised value denies).

`Capabilities::granted_summary()` returns the enable-time list
most-dangerous-first, because a user skimming a prompt stops after a few
lines and `write_memory` must never be the ninth bullet.

<!-- ANCHOR_END: capabilities -->

## 3. Tier 1 — Lua

The host builds a sandbox from the manifest: **an ungranted capability's
table is never created**, so `rf.mem` is `nil` rather than a function that
fails.

```rust
let bridge = Bridge::default();
bridge.publish(window, labels);          // BEFORE load — see below
let manifest = Manifest::parse(text)?;
let mut host = ScriptHost::load_with_bridge(manifest, script, Budget::default(), bridge)?;
host.on_frame(frame);
let draws = host.bridge().take_overlay();
```

**Publish before you load.** The idiomatic script resolves labels once, at
load:

```lua
local PLAYER_X = rf.profile.addr("player_x")
function on_frame(n)
    rf.gui.rect(rf.mem.read_u8(PLAYER_X) - 4, 84, 8, 8)
end
```

If the host publishes labels *after* `load`, that resolves to `nil` and
stays `nil` forever: the script runs, faults nothing, draws nothing, and
looks like a broken overlay rather than a lifecycle mistake.
`load_with_bridge` exists precisely so you can avoid that.

### The Lua surface, by capability

| Capability | Exposed as |
|---|---|
| `read_memory` | `rf.mem.read_u8(addr)`, `rf.mem.read_u16(addr)` (little-endian), `rf.profile.addr(label)` |
| `draw_overlay` | `rf.gui.rect(x, y, w, h [, color])`, `rf.gui.line(x0, y0, x1, y1 [, color])`, `rf.gui.text(...)` |

`rf.profile.addr` rides the **same** capability as reading memory: an
address the host published is only useful for reading, and gating it
separately would let a script learn the map while being denied the map's
only purpose.

`rf.mem.read_u16` is little-endian because every 6502-family game stores
16-bit values that way. A script assembling two bytes itself gets the
order wrong about half the time, and the symptom — correct for the first
256 units, then wrapping — survives a demo.

### Coordinates, faults, budget

**`rf.gui` draws in world space**, the same space the scene graph's layers
use, so a world-space value from a profile goes straight in. The bridge
does *not* silently subtract a camera; a script wanting screen space
subtracts `camera_x` itself, which the profile also publishes.

**A faulting script pauses itself, never the emulator** (FR-PLUG-004).
`on_frame` returns `()` — never `Err` — and the fault becomes
`ScriptState::Paused { error }`, so check `host.state()` rather than
handling an error. A script over its per-frame budget becomes
`Throttled` and is called every N frames (`Budget`, default 2 ms / every
8) rather than being killed.

**Writes are ledgered.** `write_memory` is the mod boundary: the host
routes writes through `WriteLedger`, so silently altering game behaviour
is impossible.

## 4. Tier 2 — WASM components

The contract is `crates/rf-plugin-sdk/wit/plugin.wit`: **one interface per
capability**, named to match `Capabilities` field-for-field.

```rust
let host = ComponentHost::new(manifest)?;
let (store, instance) = host.instantiate(&wasm_bytes)?;
```

<!-- ANCHOR: sandbox_is_linker -->
**The sandbox is the linker.** A host interface is linked *if and only if*
the manifest grants the matching capability, so an ungranted import is a
**missing** import and the component cannot instantiate.

**Nothing is stubbed**, and that is the design. A no-op stub would let the
plugin run, do nothing, and report success — the user would be told a mod
was active while it was inert. That is the exact failure the sandbox
exists to prevent, so it does not exist as a code path.

<!-- ANCHOR_END: sandbox_is_linker -->

### Three refusals, and whose problem each is

| Error | Meaning | Who acts |
|---|---|---|
| `CapabilitiesNotGranted` | imports capabilities the manifest does not grant; lists **every** one at once | the plugin author — fix the manifest |
| `CapabilityNotImplemented` | granted and in the WIT world, but this build has no host side | **us** — the manifest is already correct |
| `UnknownImport` | an unrecognised interface, or one from another package | nobody; deny-by-default covers the unknown case |

The middle one is a separate error on purpose: telling an author "not
granted" when their manifest is right would send them to fix the one file
that is not broken.

Refusal happens **before instantiation**, by inspecting the component's
imports — so no plugin code runs, and the diagnostic is ours rather than a
component-model error parsed after the fact.

### What is implemented today

| Interface | Host side |
|---|---|
| `read-memory`, `write-memory`, `frame-events`, `scanline-events` | implemented |
| `read-ppu`, `draw-overlay`, `replace-layers`, `input-bindings`, `filesystem` | **declared, not implemented** — refused by name |

The second row needs renderer, input and cache plumbing that lives outside
`rf-plugin-sdk`. Those capabilities are refused rather than linked as
empty instances, for the same reason nothing else is stubbed.

`world plugin` exports a single `on-frame: func(frame: u64)`. That is a
W9-03 design decision, not something `PLUGINS.md` §1 dictated — §1 is a
tier table and specifies no world. More exports are cheaper to add later
than to remove.

## 5. Building a component


There is no in-repo toolchain for compiling a Rust plugin to a component
yet — nothing in this workspace targets `wasm32-wasip2`, and the app shell
does not load components at runtime. The example builds its component from
WAT via the `wat` dev-dependency, which is enough to exercise the sandbox
and keeps a binary blob out of git (NFR-006).

<!-- ANCHOR: not_yet -->
Stated plainly so nobody goes looking: **you cannot yet write a Rust
plugin, build it, and have RetroForge run it.** The tier's contract,
sandbox and refusals are shipped and tested; the toolchain and the shell
wiring are not.
<!-- ANCHOR_END: not_yet -->
