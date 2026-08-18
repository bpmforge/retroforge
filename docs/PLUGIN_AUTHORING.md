# Writing a RetroForge plugin

A minimal, honest page (ticket W4-04, R-A4). It documents what the host
**actually does today**, not the full `docs/design/PLUGINS.md` vision —
where the two differ, this page says so.

## A plugin is two files

```
plugins/examples/player-overlay/
  plugin.toml   # manifest: identity, licence, provenance, capabilities
  main.lua      # the script
```

## The manifest

```toml
[plugin]
id = "player-overlay"
version = "0.1.0"
api = "0.1"
license = "MIT OR Apache-2.0"          # SPDX — REQUIRED
provenance = "first-party example"      # where it came from — REQUIRED

[capabilities]
read_memory = true
draw_overlay = true
```

`license` and `provenance` are **required and refused when absent**.
That is not paperwork: D-005 makes community intake deny-by-default, and
`PLUGINS.md` §2 puts the check *before any capability prompt is shown*, so
a plugin with no stated origin never gets as far as asking for access.

**Every capability defaults to denied**, and anything unrecognised denies
too — `write_memory = "yes"` grants nothing, because only the literal
`true` grants. A manifest that forgets a field, or one written against a
newer build that added a capability, fails toward *less* access.

## What a script can reach

A denied capability means the function **does not exist**. There is no
wrapper that refuses you; `rf.mem` is simply `nil`, and calling it is an
ordinary Lua nil-call error pointing at your line.

| capability | binds |
|---|---|
| `read_memory` | `rf.mem.read_u8` |
| `draw_overlay` | `rf.gui.rect`, `rf.gui.text`, `rf.gui.line` |
| `filesystem = "cache_dir"` | `rf.cache` |

Available regardless: `string`, `math`, `table`, `utf8`, `pcall`, `print`.

**Absent from every script, always:** `io`, `os`, `package`, `require`,
`debug`, `load`, `loadfile`, `dofile`. The first five are excluded by
loading a minimal standard library; the last three survive that and are
removed explicitly, because Lua's base library is always loaded and has no
opt-out of its own. If you need the filesystem, ask for the `cache_dir`
capability and go through the host — that is the only path that can be
contained.

`print` writes to the **Lua console panel**, not the host's stdout.

## The frame callback

```lua
function on_frame(frame)
  local x = rf.mem.read_u8(0x0300)
  rf.gui.rect(x, 200, 8, 8)
end
```

## What happens when your script misbehaves

- **It errors.** The script is auto-paused and the error goes to the
  console. The emulator keeps running — FR-PLUG-004 is "script fault
  pauses script not emulator", and the host's frame callback is typed so
  it cannot return an error into the frame loop at all.
- **It is slow.** Over its per-frame budget (2 ms by default, against
  W2-18's 16.64 ms whole-frame budget) it is throttled to every 8th frame
  with a console warning. Come back under budget and the throttle
  **releases** — it is not a one-way punishment.
- **It touches memory.** `write_memory` is the mod boundary. Every write
  goes through a ledger that records frame, address, value and which
  plugin did it, surfaced in the UI.

## Known gaps, stated plainly

- `rf.mem.read_u8` is **capability-gated but returns 0** in this build.
  The gate is real and tested; wiring a live `StateView` through needs the
  shell's core handle and is owed by a later ticket.
- `rf.gui.*` accepts and validates calls but does not yet reach the
  renderer's overlay path.
- `write_memory` is declarable and ledgered, but the host **refuses the
  write itself**: `PLUGINS.md` §2 requires the ledger also be recorded in
  save states (PROF/ENHC chunks), and the `ENHC` chunk does not exist yet.
  Shipping the ledger half-wired would be worse than shipping it honest.
- WASM tier, `replace_layers` and `input_bindings` are not implemented.
