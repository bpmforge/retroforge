//! RetroForge application shell. UI framework arrives with ticket F-01;
//! until then this binary only proves the workspace links end to end.

fn main() {
    println!(
        "RetroForge {} (core-api: {})",
        env!("CARGO_PKG_VERSION"),
        rf_core_api::CRATE_NAME
    );
}
