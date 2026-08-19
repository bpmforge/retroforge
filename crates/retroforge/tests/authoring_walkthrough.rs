//! Ticket W5-06 criterion 4 (R-A2): the profile authoring walkthrough,
//! **executed** rather than described, so `docs/AUTHORING.md`'s
//! time-to-first-decoded-level is a measurement someone can re-run rather
//! than a number a doc once claimed.
//!
//! `#[ignore]`d like the other wall-clock measurements in this repo
//! (`breakpoint_cost`, `debugger_idle_cost`): it reports timings, and a
//! timing is not a hermetic assertion. Run with:
//!   cargo test --release -p retroforge --test authoring_walkthrough -- --ignored --nocapture
//!
//! ## What this does and does NOT measure
//!
//! It measures the **mechanical** half of the loop: exporting a skeleton
//! from annotations, parsing it, decoding a level through it, and
//! validating the result. It does **not** measure the human half —
//! finding the addresses in the debugger, and typing the `[decode]`
//! section — because that is a person's time, not a program's, and
//! quoting a machine number as though it covered the whole loop would
//! overstate the tool by orders of magnitude. `docs/AUTHORING.md` says so
//! next to the number.

use std::path::{Path, PathBuf};
use std::time::Instant;

use rf_debugger::annotation::{AddressSpace, Annotation};
use rf_debugger::profile_export::{export_skeleton, Console, ExportMeta};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/retroforge is two levels under the repo root")
        .to_path_buf()
}

fn ram(addr: u32, len: u32, ty: &str, label: &str) -> Annotation {
    Annotation {
        space: AddressSpace::Ram,
        addr,
        len,
        ty: ty.to_string(),
        label: label.to_string(),
        notes: None,
        source: "debugger watchpoint session, RF-Scroller".to_string(),
        count: None,
    }
}

fn rom(addr: u32, len: u32, ty: &str, label: &str, count: Option<u32>) -> Annotation {
    Annotation {
        space: AddressSpace::Rom,
        addr,
        len,
        ty: ty.to_string(),
        label: label.to_string(),
        notes: None,
        source: "symbol dump of the deterministic fixture build".to_string(),
        count,
    }
}

#[test]
#[ignore = "wall-clock walkthrough measurement; feeds docs/AUTHORING.md"]
fn walkthrough_annotate_export_decode_validate_golden() {
    let root = repo_root();
    let raw = match std::fs::read(root.join("fixtures/nes/rf-scroller/build/rf-scroller.nes")) {
        Ok(raw) => raw,
        Err(_) => {
            eprintln!("SKIP: fixture ROM not built — run fixtures/nes/rf-scroller/build.sh");
            return;
        }
    };
    let normalized = raw[16..].to_vec();
    let dir = std::env::temp_dir().join(format!("rf_walkthrough_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let total = Instant::now();

    // ---- 1. annotate -------------------------------------------------
    // What a debugger session produces: the addresses W5-01 derived, as
    // annotations with their provenance attached.
    let t = Instant::now();
    let annotations = vec![
        ram(0x6029, 2, "u16", "player_x"),
        ram(0x602B, 2, "u16", "camera_x"),
        ram(0x602D, 1, "u8", "columns_streamed"),
        rom(0x11CD, 16, "bytes", "metatile_table", Some(4)),
        rom(0x11DD, 4, "bytes", "collision_table", None),
        rom(0x11E9, 48, "bytes", "level_column_offset", Some(48)),
        rom(0x1219, 246, "bytes", "level_rle_data", None),
    ];
    let annotate = t.elapsed();

    // ---- 2. export ---------------------------------------------------
    let t = Instant::now();
    let skeleton = export_skeleton(
        &annotations,
        &ExportMeta {
            title: "RF-Scroller (walkthrough)".to_string(),
            console: Console::Nes,
            region: "ntsc".to_string(),
            authors: vec!["walkthrough".to_string()],
        },
    )
    .expect("annotations with sources export");
    let export = t.elapsed();

    // ---- 3. the human step, isolated ---------------------------------
    // The skeleton has `memory_map`/`rom_map` but no `[decode]`: the
    // exporter cannot invent a level format from addresses. This is the
    // one step a person does, and it is measured separately (as zero
    // machine time) so the doc cannot pretend the tool did it.
    let decode_section = r#"
[decode]
kind = "metatile_screens"

[decode.metatile]
table = 0x11CD
size = 4

[decode.screens]
width = 48
height = 14
order = "column_rle"

[decode.collision]
table = 0x11DD
bits = "solid,platform,hazard"
"#;
    let authored = format!("{skeleton}\n{decode_section}");
    let path = dir.join("profile.toml");
    std::fs::write(&path, &authored).unwrap();

    // ---- 4. decode (the first decoded level) -------------------------
    let t = Instant::now();
    let outcome = retroforge::authoring::reload(&path, Some(&normalized));
    let decode = t.elapsed();
    assert!(
        outcome.errors.is_empty(),
        "the walkthrough profile must decode: {:?}",
        outcome.errors
    );
    let level = outcome.level.as_ref().expect("a level");
    assert_eq!((level.width, level.height), (48, 14));
    let time_to_first_decoded_level = total.elapsed();

    // ---- 5. validate -------------------------------------------------
    let t = Instant::now();
    let loaded = rf_profiles::load_file(&path).expect("the authored profile validates");
    let validate = t.elapsed();

    // ---- 6. golden ---------------------------------------------------
    // The same preview text the panel shows, which is what an author
    // would paste into a golden.
    let t = Instant::now();
    let preview = retroforge::authoring::preview_text(level, 48, 14);
    let golden = t.elapsed();
    assert_eq!(preview.lines().count(), 14);

    let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
    eprintln!(
        "R-A2 walkthrough (machine time only, human editing excluded):\n\
         \x20 annotate {:.3} ms\n\
         \x20 export   {:.3} ms  ({} bytes of TOML)\n\
         \x20 decode   {:.3} ms\n\
         \x20 validate {:.3} ms  ({} warning(s))\n\
         \x20 golden   {:.3} ms\n\
         \x20 TIME TO FIRST DECODED LEVEL: {:.3} ms",
        ms(annotate),
        ms(export),
        skeleton.len(),
        ms(decode),
        ms(validate),
        loaded.warnings.len(),
        ms(golden),
        ms(time_to_first_decoded_level),
    );

    let _ = std::fs::remove_dir_all(&dir);
}
