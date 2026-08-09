//! `retroforge-tool` — the developer CLI (FR-DBG-007, FR-PROF-001).
//!
//! Namespace design (ticket W4-02, resolving the W4-02/W4-07 collision
//! recorded on the board): two top-level command GROUPS, never a bare
//! subcommand at the root.
//!
//!   retroforge-tool profile validate <path>...    -- this ticket
//!   retroforge-tool profile hash <rom> <profile>   -- this ticket
//!   retroforge-tool rom ...                        -- reserved for W4-07
//!                                                      (RA-compatible ROM
//!                                                      hashing, header
//!                                                      inspect, VRAM/OAM
//!                                                      dumps, trace
//!                                                      capture)
//!
//! `profile hash` and the future `rom hash` are deliberately different
//! commands, not an overloaded shared one: this one matches a PROFILE
//! against a ROM (an identity check); W4-07's emits a ROM's own hashes
//! with no profile involved. Keeping them in separate groups is what lets
//! W4-07 land without renaming anything here.
//!
//! No CLI-argument-parsing crate is added: `clap` (or similar) is not on
//! `docs/TECH_STACK.md`'s sanctioned list, and this surface is small
//! enough that hand-rolled parsing is the documented preference for a
//! single small crate (see `rf-cart::error`'s module doc for the same
//! reasoning applied to `thiserror`).

use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(run(&args));
}

fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("profile") => run_profile(&args[1..]),
        Some("rom") => {
            eprintln!("retroforge-tool rom: reserved for ticket W4-07, not implemented here");
            2
        }
        Some(other) => {
            eprintln!("retroforge-tool: unknown command group `{other}`");
            print_usage();
            2
        }
        None => {
            print_usage();
            2
        }
    }
}

fn print_usage() {
    eprintln!("usage: retroforge-tool <group> <subcommand> [args...]");
    eprintln!();
    eprintln!("  profile validate <path>...   validate profile TOML file(s)/dir(s)");
    eprintln!("                                against schema v0");
    eprintln!("  profile hash <rom> <profile>  check a ROM's hash against a profile's");
    eprintln!("                                [[identity]] entries");
    eprintln!("  rom ...                       reserved for ticket W4-07");
}

fn run_profile(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("validate") => run_profile_validate(&args[1..]),
        Some("hash") => run_profile_hash(&args[1..]),
        Some(other) => {
            eprintln!("retroforge-tool profile: unknown subcommand `{other}`");
            print_usage();
            2
        }
        None => {
            eprintln!("retroforge-tool profile: expected a subcommand (validate|hash)");
            print_usage();
            2
        }
    }
}

fn run_profile_validate(args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("retroforge-tool profile validate: expected at least one path");
        return 2;
    }

    let mut files = Vec::new();
    for arg in args {
        collect_toml_files(Path::new(arg), &mut files);
    }
    if files.is_empty() {
        eprintln!("retroforge-tool profile validate: no .toml files found under given path(s)");
        return 1;
    }

    let mut any_error = false;
    for file in &files {
        match rf_profiles::load_file(file) {
            Ok(outcome) => {
                for warning in &outcome.warnings {
                    println!("{}: warning: unknown key `{warning}`", file.display());
                }
                println!("{}: ok", file.display());
            }
            Err(err) => {
                eprintln!("{}: error: {err}", file.display());
                any_error = true;
            }
        }
    }

    i32::from(any_error)
}

/// Recursively collect `*.toml` files under `path` (or just `path` itself
/// if it is already a file), sorted for deterministic output —
/// `docs/TESTING.md` runs this over the whole `/profiles` directory, not a
/// single file.
fn collect_toml_files(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        for entry in entries {
            collect_toml_files(&entry, out);
        }
    } else if path.extension().and_then(|ext| ext.to_str()) == Some("toml") {
        out.push(path.to_path_buf());
    }
}

fn run_profile_hash(args: &[String]) -> i32 {
    let [rom_arg, profile_arg] = args else {
        eprintln!("retroforge-tool profile hash: expected <rom-path> <profile-path>");
        return 2;
    };
    let rom_path = Path::new(rom_arg);
    let profile_path = Path::new(profile_arg);

    let rom_bytes = match std::fs::read(rom_path) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("{}: {err}", rom_path.display());
            return 1;
        }
    };
    let cart = match rf_cart::Cartridge::load(&rom_bytes) {
        Ok(cart) => cart,
        Err(err) => {
            eprintln!("{}: {err}", rom_path.display());
            return 1;
        }
    };
    let identity = match &cart {
        rf_cart::Cartridge::Nes { identity, .. } | rf_cart::Cartridge::Snes { identity, .. } => {
            identity
        }
    };

    let outcome = match rf_profiles::load_file(profile_path) {
        Ok(outcome) => outcome,
        Err(err) => {
            eprintln!("{}: {err}", profile_path.display());
            return 1;
        }
    };
    for warning in &outcome.warnings {
        println!(
            "{}: warning: unknown key `{warning}`",
            profile_path.display()
        );
    }

    println!(
        "{}: sha256={} sha1={} md5={} crc32={} (normalized)",
        rom_path.display(),
        identity.normalized.sha256,
        identity.normalized.sha1,
        identity.normalized.md5,
        identity.normalized.crc32,
    );

    let matched = outcome.profile.matches_rom(identity);
    println!(
        "{}: {}",
        profile_path.display(),
        if matched { "MATCH" } else { "NO MATCH" }
    );

    i32::from(!matched)
}
