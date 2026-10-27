//! Compiles the vendored rcheevos v12.5.0 C sources (see
//! `vendor/rcheevos/VENDORED.md`) into a static library linked into this
//! crate, plus a tiny layout-check shim used only by the unit tests.
//!
//! API verified against cc 1.4.0 source in the local cargo registry:
//! `Build::{include, file, warnings, compile}`.

use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let rc = root.join("vendor/rcheevos");

    // Exactly the translation units rc_client needs, listed explicitly so a
    // re-vendor that adds a file never silently changes the build.
    // RC_CLIENT_SUPPORTS_HASH is deliberately NOT defined: the host hashes
    // ROMs itself (rf-cart) and calls rc_client_begin_load_game with the
    // hash, so no file I/O, disc reader or hashing code exists in C.
    let sources = [
        "src/rc_client.c",
        "src/rc_compat.c",
        "src/rc_util.c",
        "src/rc_version.c",
        "src/rcheevos/alloc.c",
        "src/rcheevos/condition.c",
        "src/rcheevos/condset.c",
        "src/rcheevos/consoleinfo.c",
        "src/rcheevos/format.c",
        "src/rcheevos/lboard.c",
        "src/rcheevos/memref.c",
        "src/rcheevos/operand.c",
        "src/rcheevos/rc_validate.c",
        "src/rcheevos/richpresence.c",
        "src/rcheevos/runtime.c",
        "src/rcheevos/runtime_progress.c",
        "src/rcheevos/trigger.c",
        "src/rcheevos/value.c",
        "src/rapi/rc_api_common.c",
        "src/rapi/rc_api_editor.c",
        "src/rapi/rc_api_info.c",
        "src/rapi/rc_api_runtime.c",
        "src/rapi/rc_api_user.c",
        // MD5 for the award-request signature (rc_api_runtime.c).
        "src/rhash/md5.c",
    ];

    let mut build = cc::Build::new();
    build
        .include(rc.join("include"))
        .include(rc.join("src"))
        // Silence third-party warnings; our own code is covered by clippy.
        .warnings(false);
    for s in sources {
        build.file(rc.join(s));
        println!("cargo:rerun-if-changed={}", rc.join(s).display());
    }
    build.compile("rcheevos");

    let shim = root.join("shim/layout_check.c");
    cc::Build::new()
        .include(rc.join("include"))
        .file(&shim)
        .warnings(false)
        .compile("rf_achievements_shim");
    println!("cargo:rerun-if-changed={}", shim.display());
    println!("cargo:rerun-if-changed=build.rs");
}
