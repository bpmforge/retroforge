//! Plugin API: observation-first, capability-gated, WASM/Lua hosts later
//!
//! See /docs/MODULE_DESIGN.md and /docs/design/ for the contract this crate
//! must implement. Do not add public API here without a ticket in plan.json.

/// Crate marker used by the test harness to confirm workspace wiring.
pub const CRATE_NAME: &str = "rf-plugin-sdk";

#[cfg(test)]
mod tests {
    #[test]
    fn crate_is_wired() {
        assert_eq!(super::CRATE_NAME, "rf-plugin-sdk");
    }
}

pub mod component;
pub mod host;
pub mod manifest;
pub mod sandbox;

pub use host::{Budget, LedgerEntry, PathRefusal, ScriptHost, ScriptState, WriteLedger};
pub use manifest::{Capabilities, FilesystemCap, Manifest, ManifestError};
pub use sandbox::ScriptLog;

/// The SDK documentation names example files; these assert they exist.
///
/// `docs/PLUGIN_SDK.md`'s whole anti-rot claim is that its API is
/// compiled — but that only holds while the examples it names are the
/// examples that exist. A renamed or deleted example would leave the doc
/// pointing at nothing while every other test stayed green, which is the
/// same silent-drift shape as the unread `plugin.wit` W9-03 found.
#[cfg(test)]
mod doc_example_tests {
    use std::path::Path;

    /// Every example the SDK doc names, and the doc that names them.
    const NAMED_EXAMPLES: &[&str] = &[
        "examples/lua_player_marker.rs",
        "examples/component_capability_refusal.rs",
    ];

    #[test]
    fn docs_examples_exist() {
        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        for rel in NAMED_EXAMPLES {
            assert!(
                crate_dir.join(rel).is_file(),
                "docs/PLUGIN_SDK.md names {rel}, which does not exist"
            );
        }
    }

    #[test]
    fn the_sdk_doc_names_every_example_that_exists() {
        // The anti-vacuity half: without it, ADDING an example the doc
        // never mentions still passes, and a reader would never find it.
        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let doc = std::fs::read_to_string(crate_dir.join("../../docs/PLUGIN_SDK.md"))
            .expect("docs/PLUGIN_SDK.md is readable from the crate");
        let dir = std::fs::read_dir(crate_dir.join("examples")).expect("examples/ exists");
        for entry in dir {
            let path = entry.expect("readable dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .expect("example has a name")
                .to_string();
            assert!(
                doc.contains(&name),
                "examples/{name} exists but docs/PLUGIN_SDK.md never names it"
            );
        }
    }
}
