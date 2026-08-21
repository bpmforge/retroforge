# Plugin SDK

Two tiers ship. Pick by what you are doing:

| You want to… | Tier |
|---|---|
| poke at a game, iterate live, share a 30-line snippet | **Lua** |
| ship something others install, with a stable sandbox boundary | **WASM component** |

The full reference is `docs/PLUGIN_SDK.md`. This chapter quotes the two
parts a newcomer gets wrong, then shows both working examples in full.

## The capability model

{{#include ../../PLUGIN_SDK.md:capabilities}}

## The sandbox is the linker

{{#include ../../PLUGIN_SDK.md:sandbox_is_linker}}

## Example: a Lua overlay

This is `crates/rf-plugin-sdk/examples/lua_player_marker.rs` in full. It
is compiled by every `cargo test --workspace`, so it cannot drift from
the API it demonstrates.

```rust
{{#include ../../../crates/rf-plugin-sdk/examples/lua_player_marker.rs}}
```

## Example: a refused capability

`crates/rf-plugin-sdk/examples/component_capability_refusal.rs`, also
compiled by the workspace gate. One component and two manifests — the
only thing deciding whether it loads is the manifest beside it.

```rust
{{#include ../../../crates/rf-plugin-sdk/examples/component_capability_refusal.rs}}
```

## What you cannot do yet

{{#include ../../PLUGIN_SDK.md:not_yet}}
