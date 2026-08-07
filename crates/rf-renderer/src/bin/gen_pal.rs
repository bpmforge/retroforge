//! Palette asset generator (ticket W3-01, acceptance criterion 6;
//! `docs/design/RENDERER.md` §2: "The palette LUT is data (checked-in
//! `.pal` assets + generator tool), so palette research doesn't touch
//! shaders").
//!
//! Extracts the 64-entry, no-color-emphasis NES palette slice
//! (`crate::palette::NES_PALETTE`'s module doc explains why: `CoreSink`
//! doesn't carry emphasis bits yet) from an upstream `.pal` file --
//! `tetanes-core/ntscpalette.pal` convention: 512 RGB triples (64 base
//! colors x 8 PPUMASK emphasis combinations), no-emphasis slice first --
//! and writes it to `crates/rf-renderer/assets/nes.pal`, the single source
//! of truth [`rf_renderer::palette::NES_PALETTE`] decodes from at compile
//! time. Re-running this against a freshly fetched upstream `.pal` is how
//! future palette research updates the console LUT: never by hand-editing
//! the WGSL shader or a Rust array literal.
//!
//! Usage: `gen-pal <path-to-upstream.pal> [output-path]` (output defaults
//! to `crates/rf-renderer/assets/nes.pal`).

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

const ENTRY_BYTES: usize = 3; // R, G, B, one byte each.
const ENTRIES: usize = 64; // $00-$3F, no color emphasis.
const SLICE_BYTES: usize = ENTRY_BYTES * ENTRIES; // 192.

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(input) = args.next() else {
        eprintln!("usage: gen-pal <path-to-upstream.pal> [output-path]");
        return ExitCode::FAILURE;
    };
    let output = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(default_output_path);

    let upstream = match fs::read(&input) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("failed to read {input}: {e}");
            return ExitCode::FAILURE;
        }
    };
    if upstream.len() < SLICE_BYTES {
        eprintln!(
            "{input} is only {} bytes; need at least {SLICE_BYTES} (64 RGB triples) -- \
             not a plausible .pal source",
            upstream.len()
        );
        return ExitCode::FAILURE;
    }
    let slice = &upstream[..SLICE_BYTES];
    if let Err(e) = fs::write(&output, slice) {
        eprintln!("failed to write {}: {e}", output.display());
        return ExitCode::FAILURE;
    }
    println!(
        "wrote {} ({} bytes, {ENTRIES} RGB entries) from {input}",
        output.display(),
        slice.len()
    );
    ExitCode::SUCCESS
}

fn default_output_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/nes.pal")
}
