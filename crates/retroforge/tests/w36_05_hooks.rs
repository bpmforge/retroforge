//! Ticket W36-05: function hooks against the lockstep checker.
//!
//! The same three checks run twice, once on a small synthetic cartridge
//! (always) and once on real Super Mario Bros. play (`#[ignore]`, ROM-gated,
//! law 5):
//!
//! - **(a)** a Rust hook on the NMI entry pokes a profile-documented byte at
//!   frame K on the rebuilt core only: the checker reports the divergence at
//!   frame K naming `wram`, the poke is in the hook host's ledger (mod tier),
//!   and the instruction-level pass localises it;
//! - **(b)** a read-only Lua hook on the same entry only counts calls: zero
//!   divergence, and the count equals the number of NMIs the core took;
//! - **(c)** hooks loaded but matching nothing: zero divergence, zero events.
//!
//! The NMI function is found by observation (the atlas function the NMI
//! vector's target starts), its hashes go into the recipe, and no bytes or
//! disassembly are stored (D-021).
//!
//! ```text
//! cargo test --release -p retroforge --test w36_05_hooks -- --ignored --nocapture
//! ```

use std::cell::{Cell, RefCell};
use std::io::Read as _;
use std::path::PathBuf;
use std::rc::Rc;

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_harness::atlas::build_nes_atlas;
use rf_harness::lockstep::{run, Options, Report, Side};
use rf_nes::core::NesCore;
use rf_nes::rebuild::{entry, Atlas};
use rf_plugin_sdk::{Budget, Capabilities, HookHost, Manifest};
use rf_profiles::{load_recipe_file, load_recipe_str, recipe_to_toml};
use rf_rebuild::{match_recipe, HookPoints, RamMeaning, Recipe, RecipeFunction};

// ---- shared machinery ------------------------------------------------------

struct Quiet;
impl CoreSink for Quiet {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn manifest(read: bool, write: bool) -> Manifest {
    Manifest {
        id: "w36-05-test".into(),
        version: "0.1.0".into(),
        api: "0.1".into(),
        license: "MIT".into(),
        provenance: "RetroForge W36-05 test".into(),
        capabilities: Capabilities {
            function_hooks: true,
            read_memory: read,
            write_memory: write,
            ..Capabilities::default()
        },
    }
}

/// The atlas function the NMI vector's target starts: found by observation.
/// Returns its recipe row (hashes only) and the CPU address of the vector.
fn observe_nmi_row(raw: &[u8], atlas: &Atlas) -> RecipeFunction {
    let core = NesCore::from_ines_bytes(raw).expect("loads");
    let vector = u16::from_le_bytes([core.peek(0xFFFA), core.peek(0xFFFB)]);
    let f = atlas
        .functions
        .iter()
        .find(|f| f.entry_cpu == u32::from(vector) && f.entry_kinds & entry::INTERRUPT != 0)
        .expect("the NMI vector's target is an interrupt entry of the atlas");
    RecipeFunction {
        name: "nmi".to_string(),
        shape_hash: f.shape_hash,
        exact_hash: Some(f.byte_hash),
        address_hint: Some(f.entry_cpu),
        hooks: HookPoints {
            entry: true,
            exit: true,
        },
        source: "observed: the atlas function the NMI vector targets in a recording".to_string(),
    }
}

/// Run the checker with a hook host per pass built by `make`. Returns the
/// report and every host built (the first is the main run's).
fn lockstep_with_hosts(
    raw: &[u8],
    atlas: &Atlas,
    inputs: Vec<InputFrame>,
    recipe: &Recipe,
    make: impl Fn() -> HookHost + 'static,
) -> (Report, Vec<HookHost>) {
    let report = match_recipe(atlas, recipe);
    let hosts: Rc<RefCell<Vec<HookHost>>> = Rc::default();
    let build = {
        let (hosts, report) = (hosts.clone(), report.clone());
        move || {
            let h = make();
            h.resolve(&|n| report.function_of(n));
            hosts.borrow_mut().push(h.clone());
            h
        }
    };
    let ids = build().function_ids();
    hosts.borrow_mut().clear();
    let tick = hosts.clone();
    let mut before = move |frame: u32, side: Side, _core: &mut NesCore| {
        if side == Side::Rebuilt {
            if let Some(h) = tick.borrow().last() {
                h.set_frame(u64::from(frame));
            }
        }
    };
    let opts = Options {
        function_hooks: ids,
        hook_sink: Some(Box::new(move || Box::new(build()) as Box<dyn CoreSink>)),
        before_frame: Some(&mut before),
        ..Options::default()
    };
    let r = run(raw, atlas, inputs, opts).expect("lockstep runs");
    let all = hosts.borrow().clone();
    (r, all)
}

/// A Rust hook that pokes `value` at `addr` once, at the first NMI of
/// frame `at_frame`.
fn poke_host(at_frame: u64, addr: u32, value: u8) -> HookHost {
    let h = HookHost::new(manifest(true, true), Budget::default());
    let done = Cell::new(false);
    h.on_enter("nmi", move |_ev, m| {
        if m.frame() == at_frame && !done.get() {
            done.set(true);
            assert!(m.poke(addr, value), "the core applies a RAM poke");
        }
    })
    .expect("function_hooks granted");
    h
}

const COUNT_NMIS: &str = "count = 0\nrf.on_enter('nmi', function() count = count + 1 end)";

fn lua_count_host() -> HookHost {
    HookHost::load(
        manifest(false, false),
        COUNT_NMIS,
        Budget::default(),
        &rf_plugin_sdk::sandbox::Bridge::default(),
    )
    .expect("script loads")
}

/// A recipe row that matches nothing in any atlas.
fn bogus_row() -> RecipeFunction {
    RecipeFunction {
        name: "nothing".to_string(),
        shape_hash: 0xDEAD_BEEF_DEAD_BEEF,
        exact_hash: None,
        address_hint: None,
        hooks: HookPoints {
            entry: true,
            exit: true,
        },
        source: "test: matches no function".to_string(),
    }
}

/// NMI sequences that end at `entry`, counted on a plain core stepped one
/// instruction at a time (independent of the hook machinery).
fn count_nmis_into(raw: &[u8], inputs: &[InputFrame], entry: u16) -> u64 {
    let mut core = NesCore::from_ines_bytes(raw).expect("loads");
    core.reset(ResetKind::Hard);
    let mut n = 0u64;
    for input in inputs {
        core.bus_mut()
            .set_controller_buttons(0, input.ports[0] as u8);
        core.bus_mut()
            .set_controller_buttons(1, input.ports[1] as u8);
        // Law 8: bounded; a frame is about 30,000 instructions.
        for _ in 0..2_000_000 {
            let (nmi, _) = core.cpu().pending_interrupt();
            let r = core.step(rf_core_api::Step::Instruction, &mut Quiet);
            if nmi && core.cpu().pc == entry {
                n += 1;
            }
            if r.frame_complete {
                break;
            }
        }
    }
    n
}

/// The three acceptance checks, shared by the synthetic and the SMB run.
struct Case<'a> {
    raw: &'a [u8],
    atlas: &'a Atlas,
    inputs: Vec<InputFrame>,
    recipe: Recipe,
    /// Frame at which the Rust hook pokes, the byte and the value.
    poke_frame: u32,
    poke_addr: u32,
    poke_value: u8,
}

fn acceptance(c: &Case<'_>) {
    // (a) the poke diverges, at the hooked frame, naming wram.
    let (addr, value, at) = (c.poke_addr, c.poke_value, c.poke_frame);
    let (r, hosts) = lockstep_with_hosts(c.raw, c.atlas, c.inputs.clone(), &c.recipe, move || {
        poke_host(u64::from(at), addr, value)
    });
    let d = r.divergence.expect("the poke must diverge");
    assert_eq!(d.frame, c.poke_frame, "divergence at the hooked frame");
    assert_eq!(r.frames_run, c.poke_frame + 1, "the run stopped there");
    let wram = d.fields.iter().find(|f| f.field == "wram");
    let wram = wram.unwrap_or_else(|| panic!("a wram difference, got {:?}", d.fields));
    println!(
        "(a) poke {addr:#06x}={value:#04x} at frame {at}: divergence frame {} field {} offset {:#06x} plain {:#x} rebuilt {:#x}",
        d.frame, wram.field, wram.first_offset, wram.plain, wram.rebuilt
    );
    assert_eq!(
        (wram.first_offset, wram.rebuilt),
        (addr as usize, u64::from(value))
    );
    let fine = d.fine.expect("localised to an instruction");
    assert!(fine
        .fields
        .iter()
        .any(|f| f.field == "wram" && f.first_offset == addr as usize));
    // Mod tier: the poke is in the main run's ledger, once.
    let ledger = hosts[0].ledger_entries();
    assert_eq!(ledger.len(), 1, "{ledger:?}");
    assert_eq!(
        (ledger[0].frame, ledger[0].addr, ledger[0].value),
        (u64::from(at), addr, value)
    );
    assert_eq!(ledger[0].plugin, "w36-05-test");
    assert!(hosts[0]
        .panel_lines()
        .iter()
        .any(|l| l.contains("MOD write") && l.contains("frame")));
    println!("(a) ledger: {:?}", hosts[0].panel_lines());

    // (b) a read-only Lua hook counts NMIs and changes nothing.
    let (r, hosts) =
        lockstep_with_hosts(c.raw, c.atlas, c.inputs.clone(), &c.recipe, lua_count_host);
    assert!(r.divergence.is_none(), "read-only hook: {:?}", r.divergence);
    assert_eq!(r.frames_run as usize, c.inputs.len());
    let count = hosts[0].lua_global_i64("count").expect("count");
    // Counted independently: a plain core stepped an instruction at a time,
    // counting NMI sequences that land on the hooked function's entry. (The
    // core's `interrupt_entries` also counts NMIs that enter other code, such
    // as the multicart's own menu.)
    let entry_cpu = c.recipe.functions[0].address_hint.expect("hint");
    let nmis = count_nmis_into(c.raw, &c.inputs, entry_cpu as u16);
    println!(
        "(b) Lua count {count}, independent NMI count {nmis}, all interrupt entries {}, frames {}",
        r.stats.interrupt_entries, r.frames_run
    );
    assert_eq!(count as u64, nmis, "one call per NMI");
    assert!(nmis <= r.stats.interrupt_entries);
    assert!(
        count as usize >= c.inputs.len() * 3 / 4,
        "most frames take the hooked NMI"
    );
    assert!(hosts[0].ledger_entries().is_empty());

    // (c) hooks that match nothing are inert.
    let mut inert = c.recipe.clone();
    inert.functions = vec![bogus_row()];
    let (r, hosts) = lockstep_with_hosts(c.raw, c.atlas, c.inputs.clone(), &inert, || {
        let h = HookHost::new(manifest(true, true), Budget::default());
        h.on_enter("nothing", |_, m| {
            m.poke(0x0300, 0xFF);
        })
        .expect("granted");
        h
    });
    assert!(r.divergence.is_none(), "inert hooks: {:?}", r.divergence);
    assert!(hosts[0].function_ids().is_empty());
    assert_eq!(hosts[0].event_counts(), (0, 0));
    assert!(hosts[0].ledger_entries().is_empty());
    assert!(hosts[0]
        .panel_lines()
        .iter()
        .any(|l| l.contains("inactive nothing")));
    println!(
        "(c) inert hooks: zero divergence over {} frames",
        r.frames_run
    );
}

// ---- synthetic cartridge ---------------------------------------------------

/// NMI on; `JSR slow` forever; the NMI handler is `INC $12; RTI` at $8060.
fn nmi_rom() -> Vec<u8> {
    let mut d = vec![0u8; 16];
    d[0..4].copy_from_slice(b"NES\x1a");
    d[4] = 1;
    d[5] = 1;
    let mut prg = vec![0xEAu8; 0x4000];
    let put = |prg: &mut Vec<u8>, off: usize, b: &[u8]| prg[off..off + b.len()].copy_from_slice(b);
    put(
        &mut prg,
        0x00,
        &[
            0xA9, 0x80, 0x8D, 0x00, 0x20, 0x20, 0x20, 0x80, 0x4C, 0x05, 0x80,
        ],
    );
    put(
        &mut prg,
        0x20,
        &[
            0xA0, 0x00, 0xA2, 0x00, 0xE8, 0xD0, 0xFD, 0xC8, 0xD0, 0xF8, 0x60,
        ],
    );
    put(&mut prg, 0x60, &[0xE6, 0x12, 0x40]);
    prg[0x3FFA..0x3FFC].copy_from_slice(&0x8060u16.to_le_bytes());
    prg[0x3FFC..0x3FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    d.extend(prg);
    d.extend(vec![0u8; 0x2000]);
    d
}

#[test]
fn synthetic_cartridge_poke_diverges_at_the_hooked_frame_and_read_only_hooks_do_not() {
    let raw = nmi_rom();
    let mut rec = NesCore::from_ines_bytes(&raw).expect("loads");
    rec.set_coverage_recording(true);
    rec.reset(ResetKind::Hard);
    for _ in 0..40 {
        rec.run_frame(&InputFrame::empty(), &mut Quiet);
    }
    let atlas = build_nes_atlas(&raw, &rec.take_coverage().expect("armed")).expect("atlas");
    let row = observe_nmi_row(&raw, &atlas);
    let recipe = Recipe {
        id: "synthetic".to_string(),
        engine: "synthetic".to_string(),
        functions: vec![row],
        ram: vec![RamMeaning {
            addr: 0x0012,
            width: 1,
            name: "nmi_count".to_string(),
            source: "the handler increments it".to_string(),
        }],
    };
    // The recipe survives its own file format.
    let recipe = load_recipe_str(&recipe_to_toml(&recipe)).expect("round trip");
    acceptance(&Case {
        raw: &raw,
        atlas: &atlas,
        inputs: vec![InputFrame::empty(); 60],
        recipe,
        poke_frame: 30,
        poke_addr: 0x0300,
        poke_value: 0x5A,
    });
}

// ---- Super Mario Bros. -----------------------------------------------------

const FILE: &str = "Super Mario Bros. + Duck Hunt (USA).zip";
const START: u8 = 1 << 3;
const A: u8 = 1 << 0;
const RIGHT: u8 = 1 << 7;
const LEAD_IN: u32 = 500;
const PLAY: u32 = 1200;
const FRAMES: u32 = LEAD_IN + PLAY;

fn rom_path() -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = std::path::Path::new(&root).join("nes").join(FILE);
    p.exists().then_some(p)
}

fn load_rom(path: &std::path::Path) -> Vec<u8> {
    let bytes = std::fs::read(path).expect("reads the zip");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("zip");
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).expect("entry");
        if !entry.is_file() {
            continue;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).expect("reads");
        if matches!(
            rf_cart::Cartridge::load(&buf),
            Ok(rf_cart::Cartridge::Nes { .. })
        ) {
            return buf;
        }
    }
    panic!("no NES image in the zip");
}

/// Two Starts (game select, then SMB's title), then Right with A pressed 12
/// frames in every 60: the W36-04 drive script one (world 1-1 by real play).
fn script_one(frame: u32) -> u8 {
    match frame {
        0..=199 => 0,
        200..=204 | 400..=404 => START,
        205..=399 | 405..=499 => 0,
        f if (f - 500) % 60 < 12 => RIGHT | A,
        _ => RIGHT,
    }
}

fn smb_inputs() -> Vec<InputFrame> {
    (0..FRAMES)
        .map(|f| {
            let mut i = InputFrame::empty();
            i.ports[0] = u16::from(script_one(f));
            i
        })
        .collect()
}

fn recipe_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../profiles/nes/super-mario-bros/recipe.toml")
}

/// Lives, from `profiles/nes/super-mario-bros/profile.toml` `[[memory_map]]`.
const LIVES: u32 = 0x075A;

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR)"]
fn smb_nmi_hooks_poke_diverges_read_only_and_inert_hooks_do_not() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let mut rec = NesCore::from_ines_bytes(&raw).expect("loads");
    rec.set_coverage_recording(true);
    rec.reset(ResetKind::Hard);
    for input in smb_inputs() {
        rec.run_frame(&input, &mut Quiet);
    }
    let atlas = build_nes_atlas(&raw, &rec.take_coverage().expect("armed")).expect("atlas");
    println!(
        "atlas: {} functions (format v{})",
        atlas.functions.len(),
        rf_rebuild::ATLAS_FORMAT_VERSION
    );

    // The recipe's one function row is the NMI handler, found by observation.
    let observed = observe_nmi_row(&raw, &atlas);
    let committed = recipe_path();
    if !committed.exists() && std::env::var("RF_W36_05_WRITE_RECIPE").is_ok() {
        let recipe = Recipe {
            id: "super-mario-bros".to_string(),
            engine: "smb1".to_string(),
            functions: vec![observed.clone()],
            ram: vec![RamMeaning {
                addr: LIVES,
                width: 1,
                name: "lives".to_string(),
                source: "profiles/nes/super-mario-bros/profile.toml [[memory_map]] lives"
                    .to_string(),
            }],
        };
        std::fs::write(&committed, recipe_to_toml(&recipe)).expect("writes the recipe");
        println!("wrote {}", committed.display());
    }
    let recipe = load_recipe_file(&committed).expect("the committed recipe loads");
    assert_eq!(
        recipe.functions.len(),
        1,
        "the recipe's only row is the NMI"
    );
    let row = &recipe.functions[0];
    assert_eq!(row.name, "nmi");
    assert_eq!(
        (row.shape_hash, row.exact_hash),
        (observed.shape_hash, observed.exact_hash),
        "the committed recipe matches the NMI handler of this dump"
    );
    assert_eq!(recipe.ram[0].addr, LIVES);
    let m = match_recipe(&atlas, &recipe);
    assert_eq!(m.matched.len(), 1, "{m:?}");
    println!("recipe match: {:?}", m.matched[0]);

    acceptance(&Case {
        raw: &raw,
        atlas: &atlas,
        inputs: smb_inputs(),
        recipe,
        poke_frame: 700,
        poke_addr: LIVES,
        poke_value: 0x07,
    });
}
