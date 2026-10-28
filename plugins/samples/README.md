# Plugin samples, one per kind (ticket W39-00)

| Directory | Kind | What it shows |
|---|---|---|
| `fact-actor-names` | fact | adds RAM and object-table rows to a recipe; changes nothing at run time |
| `render-actor-boxes` | render | reads those rows and memory, emits draw data only |
| `logic-mode-flag` | logic | writes a byte on a function hook; enabled only after shadow lockstep shows Original mode untouched |

`plugin.toml` declares `kind`; the kind fixes the capabilities it may hold
(`rf_plugin_sdk::surface`). No game content: the facts describe a made-up
engine. Tests: `crates/rf-plugin-sdk/tests/w39_00_surface.rs` (load and run) and
`crates/retroforge/tests/w39_00_logic_gate.rs` (the real lockstep gate).
