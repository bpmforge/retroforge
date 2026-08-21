//! Tier 1 (Lua): draw a marker on the player, every frame.
//!
//! Ticket W9-04. Run it:
//!
//! ```text
//! cargo run -p rf-plugin-sdk --example lua_player_marker
//! ```
//!
//! **This file is compiled by the workspace gate**, which is the point.
//! `cargo test --workspace` and `cargo clippy --workspace --all-targets`
//! both build examples, so if the API in `docs/PLUGIN_SDK.md` drifts from
//! the API that ships, this stops compiling and the gate goes red —
//! rather than the doc quietly going stale the way
//! `EMULATION_CORES.md` §3.3's superseded `dropped_by_limit` line did.
//!
//! It does exactly one thing: read `player_x`/`player_y` through the
//! bridge and ask for a rectangle there.

use std::collections::BTreeMap;

use rf_plugin_sdk::host::{Budget, ScriptHost};
use rf_plugin_sdk::manifest::Manifest;
use rf_plugin_sdk::sandbox::{Bridge, MemoryWindow, OverlayCmd};

/// `license` and `provenance` are REQUIRED, not decoration: D-005 makes
/// community intake deny-by-default and the parser refuses a manifest
/// without them.
///
/// `read_memory` and `draw_overlay` are the only capabilities granted.
/// Everything else — `write_memory` above all — stays denied, and the
/// sandbox simply never builds those tables, so `rf.mem.write` is `nil`
/// rather than a function that fails.
const MANIFEST: &str = "\
id = \"player-marker\"
version = \"0.1.0\"
api = \"0.1\"
license = \"MIT\"
provenance = \"RetroForge in-repo example (ticket W9-04)\"
read_memory = true
draw_overlay = true
";

/// `rf.profile.addr` rides the same capability as reading memory, so a
/// script names an address instead of hardcoding it.
///
/// Resolving the labels at load time — outside `on_frame` — is the
/// idiomatic shape, and it is why `load_with_bridge` exists: if the host
/// published labels only after `load`, this would be `nil` forever and
/// the overlay would look broken rather than mis-sequenced.
const SCRIPT: &str = r#"
local PLAYER_X = rf.profile.addr("player_x")
local PLAYER_Y = rf.profile.addr("player_y")

function on_frame(n)
    local x = rf.mem.read_u8(PLAYER_X)
    local y = rf.mem.read_u8(PLAYER_Y)
    -- Colour is optional; omitted means the default palette entry.
    rf.gui.rect(x - 4, y - 4, 8, 8)
end
"#;

fn main() -> mlua::Result<()> {
    // The shell owns the bridge and publishes into it BEFORE loading, so
    // the script's load-time `rf.profile.addr` calls resolve.
    let bridge = Bridge::default();
    let mut labels = BTreeMap::new();
    labels.insert("player_x".to_string(), 0x0040);
    labels.insert("player_y".to_string(), 0x0041);
    bridge.publish(
        MemoryWindow {
            base: 0x0000,
            bytes: {
                let mut ram = vec![0u8; 0x100];
                ram[0x40] = 120; // player_x
                ram[0x41] = 88; // player_y
                ram
            },
        },
        labels,
    );

    let manifest = Manifest::parse(MANIFEST).expect("example manifest is valid");
    let mut host = ScriptHost::load_with_bridge(manifest, SCRIPT, Budget::default(), bridge)?;

    host.on_frame(1);

    // The shell drains what the script asked to draw and maps it into its
    // own scene graph. `OverlayCmd` is deliberately this crate's own type
    // — ARCHITECTURE §3 has no `PLU -> ENH` edge.
    let drawn = host.bridge().take_overlay();
    for cmd in &drawn {
        match cmd {
            OverlayCmd::Rect {
                x,
                y,
                width,
                height,
                color_index,
            } => println!("rect at ({x},{y}) {width}x{height} colour {color_index:#04x}"),
            other => println!("{other:?}"),
        }
    }

    // A script that faults is PAUSED, never propagated into the frame
    // loop (FR-PLUG-004) — so the shell checks state rather than handling
    // an error.
    println!("script state: {:?}", host.state());
    // Nothing was written, because `write_memory` was never granted.
    assert!(host.ledger().is_empty(), "this example writes nothing");
    Ok(())
}
