//! Tier 2 (WASM component): what a refused capability actually looks like.
//!
//! Ticket W9-04. Run it:
//!
//! ```text
//! cargo run -p rf-plugin-sdk --example component_capability_refusal
//! ```
//!
//! Compiled by the workspace gate, same as the Lua example — see that
//! file's header for why that matters.
//!
//! It does exactly one thing: take **one** component and show that the
//! *only* thing deciding whether it loads is the manifest beside it.
//!
//! The component is built here from WAT rather than shipped as a `.wasm`
//! binary, deliberately: a checked-in blob is unreadable in review, and
//! NFR-006 keeps built artifacts out of git. `wat` is a dev-dependency,
//! and examples may use dev-dependencies.

use rf_plugin_sdk::component::{Capability, ComponentError, ComponentHost};
use rf_plugin_sdk::manifest::Manifest;

/// One component that imports `write-memory` and nothing else.
///
/// Note it is used unchanged for both halves below. That is what makes
/// this a demonstration of the *sandbox* rather than of two components.
const COMPONENT_WAT: &str = r#"
(component
  (import "retroforge:plugin/write-memory@0.1.0" (instance $w
    (export "write-u8" (func (param "addr" u32) (param "value" u8)))))
)
"#;

fn manifest(grants_write: bool) -> Manifest {
    let text = format!(
        "id = \"write-probe\"\n\
         version = \"0.1.0\"\n\
         api = \"0.1\"\n\
         license = \"MIT\"\n\
         provenance = \"RetroForge in-repo example (ticket W9-04)\"\n\
         write_memory = {grants_write}\n"
    );
    Manifest::parse(&text).expect("example manifest is valid")
}

fn main() {
    let wasm = wat::parse_str(COMPONENT_WAT).expect("example WAT is valid");

    // ---- Denied -----------------------------------------------------
    //
    // The capability is not granted, so the host never links the
    // interface. The component is therefore missing an import and is
    // refused BEFORE any of its code runs.
    //
    // What must NOT happen here is the thing this design exists to
    // prevent: linking a no-op stub, letting the plugin run, and having
    // it silently write nothing while reporting success.
    let denied = ComponentHost::new(manifest(false)).expect("engine builds");
    match denied.instantiate(&wasm) {
        Err(ComponentError::CapabilitiesNotGranted { requested }) => {
            println!("refused, and it names what it wanted:");
            for cap in &requested {
                println!("  - {}", cap.wit_name());
            }
            // The diagnostic a user would actually see.
            println!(
                "  message: {}",
                ComponentError::CapabilitiesNotGranted { requested }
            );
        }
        Err(other) => panic!("expected a capability refusal, got: {other}"),
        Ok(_) => panic!("an ungranted capability must never instantiate"),
    }

    // ---- Granted ----------------------------------------------------
    //
    // Same bytes, different manifest. This half is not decoration: without
    // it, a host that refused everything would satisfy the half above
    // while being useless.
    let granted = ComponentHost::new(manifest(true)).expect("engine builds");
    assert!(
        granted.granted().contains(&Capability::WriteMemory),
        "the manifest grants it"
    );
    match granted.instantiate(&wasm) {
        Ok(_) => println!("granted: the same component instantiates"),
        Err(e) => panic!("a granted, implemented capability must instantiate: {e}"),
    }

    // ---- Granted but unimplemented ----------------------------------
    //
    // A third outcome worth knowing about, because it is NOT the plugin
    // author's fault: `draw-overlay` is in the WIT world and can be
    // granted, but this build has no host side for it yet. It is refused
    // by name rather than linked as an empty instance, and the message
    // says the manifest is fine.
    println!(
        "not yet implemented host-side: {}",
        Capability::all()
            .into_iter()
            .filter(|c| !c.is_implemented())
            .map(Capability::wit_name)
            .collect::<Vec<_>>()
            .join(", ")
    );
}
