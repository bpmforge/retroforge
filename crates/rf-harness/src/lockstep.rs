//! The lockstep checker (ticket W36-04; `docs/design/REBUILD_LAB.md` section
//! 5, step 4): two NES cores fed identical input, one with Rebuild on (an
//! atlas installed) and one plain, compared after every frame.
//!
//! Per frame: CPU registers, cycle and frame counters, WRAM, nametable VRAM
//! (plus CHR-RAM when the cartridge has it), OAM, palette RAM, PRG-RAM, and
//! hashes of the video and audio the frame produced. The first divergence
//! stops the run.
//!
//! On a divergence the run is repeated from power-on (the machine is
//! deterministic, so nothing needs a load path): through the frame before
//! the bad one, where the plain core is serialised, then through the bad
//! frame one instruction at a time on both cores, comparing registers, WRAM
//! and the cycle counter after each, which names the exact instruction. The
//! rebuilt core keeps a ring of its last executed steps (with atlas function
//! ids) and that ring goes into the repro bundle ([`write_bundle`]).
//!
//! Today the executor is identical to the interpreter by construction, so a
//! clean run is the expected outcome; the checker exists as the referee for
//! W36-05's hooks and W36-12's JIT. It cannot see PPU/APU bugs those would
//! share with the interpreter (both cores use the same devices).
//!
//! **Savestate format.** The bundle's state is the shipped `.rfstate`
//! container (`rf_state::Container`), not a second format. The region to tag
//! table below is a copy of `retroforge::save_state`'s `REGION_TAGS`: the app
//! crate cannot be a dependency of the harness, and `rf-nes` may not name
//! chunk tags (`rf-state` cannot name `rf-nes` and `rf-nes` cannot name
//! `rf-state`, so no lower crate can host the table either). The two must stay
//! in step; `crates/retroforge/tests/w36_05_bundle_state.rs` loads a bundle
//! state back through the app's own loader.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use rf_core_api::{
    CoreEvent, CoreSink, CpuRegs, EmulatorCore, InputFrame, Mos6502Regs, PpuPixel, RebuildStats,
    ResetKind, StateError, StateWriter, Step,
};
use rf_nes::core::NesCore;
use rf_nes::rebuild::{Atlas, TraceKind};
use rf_nes::{NesLoadError, StateRegion};
use rf_state::Container;

/// Instructions the fine pass may run in one frame before giving up (a
/// frame is about 30,000; this is a bound, not a target).
const FINE_STEP_LIMIT: u32 = 2_000_000;

/// Console byte of a NES container (`retroforge::save_state::CONSOLE_NES`).
const CONSOLE_NES: u8 = 0;

/// Mirror of `retroforge::save_state::REGION_TAGS`.
const REGION_TAGS: [(StateRegion, [u8; 4]); 9] = [
    (StateRegion::Cpu, *b"CPU_"),
    (StateRegion::Ppu, *b"PPU_"),
    (StateRegion::Apu, *b"APU_"),
    (StateRegion::Wram, *b"WRAM"),
    (StateRegion::Vram, *b"VRAM"),
    (StateRegion::Oam, *b"OAM_"),
    (StateRegion::Cgram, *b"CGRM"),
    (StateRegion::Mapper, *b"MAPR"),
    (StateRegion::Cart, *b"CART"),
];

/// Which of the two cores a [`BeforeFrame`] call is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Plain,
    Rebuilt,
}

/// A per-frame hook called for each core before the frame runs (plain first).
/// It exists so a test can set up state identically on both cores (a level
/// to start in) or inject a difference on one; it must be deterministic (the
/// investigation re-run calls it again for the same frames).
pub type BeforeFrame<'a> = &'a mut dyn FnMut(u32, Side, &mut NesCore);

fn apply_hook(
    hook: &mut Option<BeforeFrame<'_>>,
    frame: u32,
    plain: &mut NesCore,
    rebuilt: &mut NesCore,
) {
    if let Some(h) = hook.as_deref_mut() {
        h(frame, Side::Plain, plain);
        h(frame, Side::Rebuilt, rebuilt);
    }
}

pub struct Options<'a> {
    /// Steps the rebuilt core's trace ring keeps (rounded up to 2^n).
    pub trace_len: usize,
    /// Input frames before the diverging one that go in the bundle.
    pub input_tail: usize,
    /// Re-run the diverging frame an instruction at a time.
    pub fine: bool,
    /// Where to write the repro bundle on divergence (`None`: no bundle).
    pub bundle_dir: Option<PathBuf>,
    pub before_frame: Option<BeforeFrame<'a>>,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Self {
            trace_len: 256,
            input_tail: 120,
            fine: true,
            bundle_dir: None,
            before_frame: None,
        }
    }
}

/// One field that differs at the first divergence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDiff {
    /// `cpu.a`, `cpu.pc`, `cycles`, `wram`, `vram`, `chr_ram`, `oam`,
    /// `palette`, `prg_ram`, `video_hash`, `audio_hash`, ...
    pub field: &'static str,
    /// First differing byte offset (0 for scalar fields).
    pub first_offset: usize,
    pub plain: u64,
    pub rebuilt: u64,
}

/// Where the fine pass found the first difference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FineDivergence {
    /// 0-based index of the instruction within the frame after which the
    /// cores differ.
    pub instruction: u32,
    /// `pc` of both cores before that instruction ran (the plain core's;
    /// the cores agreed up to here).
    pub pc_before: u16,
    pub fields: Vec<FieldDiff>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    pub frame: u32,
    pub fields: Vec<FieldDiff>,
    /// `None` when the fine pass was off or did not reproduce a register,
    /// WRAM or cycle difference inside the frame (e.g. a video-only one).
    pub fine: Option<FineDivergence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleFiles {
    pub dir: PathBuf,
    pub files: Vec<String>,
}

#[derive(Debug)]
pub struct Report {
    /// Frames compared (including the diverging one, if any).
    pub frames_run: u32,
    pub divergence: Option<Divergence>,
    /// The rebuilt core's executor counters at the end of the main run.
    pub stats: RebuildStats,
    pub bundle: Option<BundleFiles>,
    /// Why a bundle part could not be written, if any.
    pub bundle_errors: Vec<String>,
}

#[derive(Debug)]
pub enum LockstepError {
    Rom(NesLoadError),
    Atlas(String),
}

impl std::fmt::Display for LockstepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rom(e) => write!(f, "ROM did not load: {e:?}"),
            Self::Atlas(e) => write!(f, "atlas rejected: {e}"),
        }
    }
}

impl std::error::Error for LockstepError {}

/// FNV-1a over what a frame put on the sinks.
struct HashSink {
    video: u64,
    audio: u64,
}

const FNV_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

fn fnv(h: &mut u64, byte: u8) {
    *h ^= u64::from(byte);
    *h = h.wrapping_mul(0x0100_0000_01b3);
}

impl HashSink {
    fn new() -> Self {
        Self {
            video: FNV_BASIS,
            audio: FNV_BASIS,
        }
    }
}

impl CoreSink for HashSink {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        for b in y.to_le_bytes() {
            fnv(&mut self.video, b);
        }
        pixels
            .iter()
            .for_each(|p| fnv(&mut self.video, p.palette_index));
    }
    fn audio(&mut self, samples: &[i16]) {
        samples
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .for_each(|b| fnv(&mut self.audio, b));
    }
    fn event(&mut self, _event: CoreEvent) {}
}

/// Drops everything (the fine pass compares machine state, not output).
struct NullSink;

impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _event: CoreEvent) {}
}

fn regs(core: &NesCore) -> Mos6502Regs {
    match core.state_view().cpu_regs {
        CpuRegs::Mos6502(r) => r,
        _ => Mos6502Regs::default(),
    }
}

fn power_on(raw: &[u8]) -> Result<NesCore, LockstepError> {
    let mut core = NesCore::from_ines_bytes(raw).map_err(LockstepError::Rom)?;
    core.reset(ResetKind::Hard);
    Ok(core)
}

fn pair(raw: &[u8], atlas: &Atlas, trace_len: usize) -> Result<(NesCore, NesCore), LockstepError> {
    let plain = power_on(raw)?;
    let mut rebuilt = power_on(raw)?;
    rebuilt
        .set_rebuild(Some(atlas.clone()))
        .map_err(|e| LockstepError::Atlas(format!("{e:?}")))?;
    rebuilt.set_rebuild_trace(trace_len);
    Ok((plain, rebuilt))
}

fn scalar(
    out: &mut Vec<FieldDiff>,
    field: &'static str,
    plain: impl Into<u64>,
    rebuilt: impl Into<u64>,
) {
    let (p, r) = (plain.into(), rebuilt.into());
    if p != r {
        out.push(FieldDiff {
            field,
            first_offset: 0,
            plain: p,
            rebuilt: r,
        });
    }
}

fn bytes(out: &mut Vec<FieldDiff>, field: &'static str, plain: &[u8], rebuilt: &[u8]) {
    if let Some((i, (&p, &r))) = plain
        .iter()
        .zip(rebuilt)
        .enumerate()
        .find(|(_, (p, r))| p != r)
    {
        out.push(FieldDiff {
            field,
            first_offset: i,
            plain: u64::from(p),
            rebuilt: u64::from(r),
        });
    } else if plain.len() != rebuilt.len() {
        out.push(FieldDiff {
            field,
            first_offset: plain.len().min(rebuilt.len()),
            plain: plain.len() as u64,
            rebuilt: rebuilt.len() as u64,
        });
    }
}

/// CPU registers, counters and WRAM: the part the fine pass checks after
/// every instruction.
fn compare_cpu(plain: &NesCore, rebuilt: &NesCore) -> Vec<FieldDiff> {
    let (a, b) = (regs(plain), regs(rebuilt));
    let mut out = Vec::new();
    scalar(&mut out, "cpu.a", a.a, b.a);
    scalar(&mut out, "cpu.x", a.x, b.x);
    scalar(&mut out, "cpu.y", a.y, b.y);
    scalar(&mut out, "cpu.s", a.s, b.s);
    scalar(&mut out, "cpu.p", a.p, b.p);
    scalar(&mut out, "cpu.pc", a.pc, b.pc);
    scalar(
        &mut out,
        "cycles",
        plain.bus().master_cycle(),
        rebuilt.bus().master_cycle(),
    );
    bytes(&mut out, "wram", plain.bus().ram(), rebuilt.bus().ram());
    out
}

/// Everything compared once a frame ends.
fn compare_frame(
    plain: &NesCore,
    rebuilt: &NesCore,
    hp: &HashSink,
    hr: &HashSink,
) -> Vec<FieldDiff> {
    let mut out = compare_cpu(plain, rebuilt);
    scalar(
        &mut out,
        "frame_count",
        plain.bus().frame_count(),
        rebuilt.bus().frame_count(),
    );
    let (pb, rb) = (plain.bus(), rebuilt.bus());
    bytes(&mut out, "vram", pb.vram(), rb.vram());
    if pb.chr_is_ram() || rb.chr_is_ram() {
        bytes(&mut out, "chr_ram", pb.chr(), rb.chr());
    }
    bytes(&mut out, "oam", pb.oam(), rb.oam());
    bytes(&mut out, "palette", pb.palette(), rb.palette());
    bytes(&mut out, "prg_ram", pb.prg_ram(), rb.prg_ram());
    scalar(&mut out, "video_hash", hp.video, hr.video);
    scalar(&mut out, "audio_hash", hp.audio, hr.audio);
    out
}

fn run_pair_frame(
    frame: u32,
    input: &InputFrame,
    plain: &mut NesCore,
    rebuilt: &mut NesCore,
    hook: &mut Option<BeforeFrame<'_>>,
) -> (HashSink, HashSink) {
    apply_hook(hook, frame, plain, rebuilt);
    let (mut hp, mut hr) = (HashSink::new(), HashSink::new());
    plain.run_frame(input, &mut hp);
    rebuilt.run_frame(input, &mut hr);
    (hp, hr)
}

/// Run `inputs` (one entry per frame) through a plain core and a rebuilt
/// core from power-on, comparing after every frame; stop at the first
/// divergence and, if `opts.bundle_dir` is set, write a repro bundle there.
///
/// `raw` is the iNES image; `atlas` must be for its PRG ROM.
///
/// # Errors
/// The ROM does not load or the atlas is for another cartridge. A bundle
/// that cannot be written is reported in [`Report::bundle_errors`], not here.
pub fn run(
    raw: &[u8],
    atlas: &Atlas,
    inputs: impl IntoIterator<Item = InputFrame>,
    mut opts: Options<'_>,
) -> Result<Report, LockstepError> {
    let inputs: Vec<InputFrame> = inputs.into_iter().collect();
    let (mut plain, mut rebuilt) = pair(raw, atlas, opts.trace_len)?;
    let mut frames_run = 0u32;
    let mut found: Option<(u32, Vec<FieldDiff>)> = None;
    for (frame, input) in (0u32..).zip(&inputs) {
        let (hp, hr) = run_pair_frame(
            frame,
            input,
            &mut plain,
            &mut rebuilt,
            &mut opts.before_frame,
        );
        frames_run = frame + 1;
        let diffs = compare_frame(&plain, &rebuilt, &hp, &hr);
        if !diffs.is_empty() {
            found = Some((frame, diffs));
            break;
        }
    }
    let stats = rebuilt.rebuild_stats().unwrap_or_default();
    let Some((frame, fields)) = found else {
        return Ok(Report {
            frames_run,
            divergence: None,
            stats,
            bundle: None,
            bundle_errors: Vec::new(),
        });
    };
    let mut divergence = Divergence {
        frame,
        fields,
        fine: None,
    };
    let mut bundle_errors = Vec::new();
    let mut bundle = None;
    if opts.fine || opts.bundle_dir.is_some() {
        let inv = investigate(raw, atlas, &inputs, frame, &mut opts);
        divergence.fine = inv.fine.clone();
        if let Some(dir) = opts.bundle_dir.clone() {
            let (files, errs) = write_bundle(&dir, raw, &inputs, &divergence, &inv, &opts);
            bundle_errors = errs;
            bundle = Some(BundleFiles { dir, files });
        }
    }
    Ok(Report {
        frames_run,
        divergence: Some(divergence),
        stats,
        bundle,
        bundle_errors,
    })
}

/// What the re-run of the diverging frame produced.
struct Investigation {
    /// The plain core serialised at the start of the diverging frame.
    state: Result<Vec<u8>, String>,
    fine: Option<FineDivergence>,
    /// The rebuilt core's last executed steps at the point of divergence.
    trace: Vec<(u16, TraceKind)>,
    wram_plain: Vec<u8>,
    wram_rebuilt: Vec<u8>,
}

fn investigate(
    raw: &[u8],
    atlas: &Atlas,
    inputs: &[InputFrame],
    frame: u32,
    opts: &mut Options<'_>,
) -> Investigation {
    let empty = |msg: String| Investigation {
        state: Err(msg),
        fine: None,
        trace: Vec::new(),
        wram_plain: Vec::new(),
        wram_rebuilt: Vec::new(),
    };
    let Ok((mut plain, mut rebuilt)) = pair(raw, atlas, opts.trace_len) else {
        return empty("investigation re-run could not rebuild the cores".to_string());
    };
    for (f, input) in (0..frame).zip(inputs) {
        let _ = run_pair_frame(f, input, &mut plain, &mut rebuilt, &mut opts.before_frame);
    }
    let state = snapshot(&plain, raw);
    let mut fine = None;
    if let Some(input) = inputs.get(frame as usize) {
        if opts.fine {
            apply_hook(&mut opts.before_frame, frame, &mut plain, &mut rebuilt);
            fine = fine_pass(&mut plain, &mut rebuilt, input);
        } else {
            let _ = run_pair_frame(
                frame,
                input,
                &mut plain,
                &mut rebuilt,
                &mut opts.before_frame,
            );
        }
    }
    let trace = rebuilt
        .rebuild_trace()
        .into_iter()
        .map(|e| (e.cpu, e.kind))
        .collect();
    Investigation {
        state,
        fine,
        trace,
        wram_plain: plain.bus().ram().to_vec(),
        wram_rebuilt: rebuilt.bus().ram().to_vec(),
    }
}

/// Step both cores one instruction at a time through one frame; the first
/// instruction after which registers, cycle count or WRAM differ is the
/// answer. Law 8: the loop is a bounded `for`.
fn fine_pass(
    plain: &mut NesCore,
    rebuilt: &mut NesCore,
    input: &InputFrame,
) -> Option<FineDivergence> {
    for core in [&mut *plain, &mut *rebuilt] {
        core.bus_mut()
            .set_controller_buttons(0, input.ports[0] as u8);
        core.bus_mut()
            .set_controller_buttons(1, input.ports[1] as u8);
    }
    let mut sink = NullSink;
    for n in 0..FINE_STEP_LIMIT {
        let pc_before = regs(plain).pc;
        let a = plain.step(Step::Instruction, &mut sink);
        let b = rebuilt.step(Step::Instruction, &mut sink);
        let mut fields = compare_cpu(plain, rebuilt);
        if a.frame_complete != b.frame_complete {
            scalar(
                &mut fields,
                "frame_complete",
                u64::from(a.frame_complete),
                u64::from(b.frame_complete),
            );
        }
        if !fields.is_empty() {
            return Some(FineDivergence {
                instruction: n,
                pc_before,
                fields,
            });
        }
        if a.frame_complete {
            return None;
        }
    }
    None
}

#[derive(Default)]
struct PayloadBuf(Vec<u8>);

impl StateWriter for PayloadBuf {
    fn write_all(&mut self, buf: &[u8]) -> Result<(), StateError> {
        self.0.extend_from_slice(buf);
        Ok(())
    }
}

/// The shipped `.rfstate` bytes of `core` (see the module doc). Only valid
/// at a frame boundary, which is where the investigation takes it.
fn snapshot(core: &NesCore, raw: &[u8]) -> Result<Vec<u8>, String> {
    let sha = rf_cart::hash::identity_nes(raw).normalized.sha256;
    let mut hash = [0u8; 32];
    for (i, slot) in hash.iter_mut().enumerate() {
        let pair = sha.get(i * 2..i * 2 + 2).ok_or("short rom hash")?;
        *slot = u8::from_str_radix(pair, 16).map_err(|e| e.to_string())?;
    }
    let mut container = Container::new(CONSOLE_NES, hash, env!("CARGO_PKG_VERSION"), 0);
    for (region, tag) in REGION_TAGS {
        let mut payload = PayloadBuf::default();
        core.bus()
            .save_region(core.cpu(), region, &mut payload)
            .map_err(|e| format!("{e:?}"))?;
        let version = rf_state::tag_info(tag)
            .ok_or_else(|| format!("tag {} not in the registry", String::from_utf8_lossy(&tag)))?
            .current_version;
        container
            .add_chunk(tag, version, payload.0)
            .map_err(|e| format!("{e:?}"))?;
    }
    container.encode().map_err(|e| format!("{e:?}"))
}

fn put_file(dir: &Path, name: &str, data: &[u8], acc: &mut (Vec<String>, Vec<String>)) {
    match std::fs::write(dir.join(name), data) {
        Ok(()) => acc.0.push(name.to_string()),
        Err(e) => acc.1.push(format!("{name}: {e}")),
    }
}

fn describe(fields: &[FieldDiff]) -> String {
    let mut s = String::new();
    for f in fields {
        let _ = writeln!(
            s,
            "{:<12} first offset {:#06x}  plain {:#x}  rebuilt {:#x}",
            f.field, f.first_offset, f.plain, f.rebuilt
        );
    }
    s
}

/// Write the repro bundle (all UTF-8 text except the two binary files):
///
/// - `plain_state_before_frame.rfstate`: the plain core at the start of the
///   diverging frame, the app's own container format;
/// - `inputs.txt`: `frame port0 port1` (hex) for the tail and the diverging
///   frame (marked `<-- diverging`);
/// - `trace.txt`: the rebuilt core's last executed steps, oldest first, with
///   atlas function ids and entry addresses;
/// - `diff.txt`: which fields differed, first differing offsets, the fine
///   pass result;
/// - `wram_plain.bin`, `wram_rebuilt.bin`: work RAM at the end of the pass;
/// - `README.txt`.
///
/// No PRG/CHR ROM bytes. Returns the files written and the parts that
/// failed.
fn write_bundle(
    dir: &Path,
    raw: &[u8],
    inputs: &[InputFrame],
    d: &Divergence,
    inv: &Investigation,
    opts: &Options<'_>,
) -> (Vec<String>, Vec<String>) {
    let files = Vec::new();
    let errors = Vec::new();
    if let Err(e) = std::fs::create_dir_all(dir) {
        return (files, vec![format!("create {}: {e}", dir.display())]);
    }
    let mut files_errors = (files, errors);
    let mut put = |name: &str, data: &[u8]| put_file(dir, name, data, &mut files_errors);

    if let Ok(b) = &inv.state {
        put("plain_state_before_frame.rfstate", b);
    }
    let first = (d.frame as usize).saturating_sub(opts.input_tail);
    let mut text = String::from("frame port0 port1 (hex)\n");
    for (i, input) in inputs
        .iter()
        .enumerate()
        .take(d.frame as usize + 1)
        .skip(first)
    {
        let mark = if i == d.frame as usize {
            "  <-- diverging"
        } else {
            ""
        };
        let _ = writeln!(
            text,
            "{i} {:04x} {:04x}{mark}",
            input.ports[0], input.ports[1]
        );
    }
    put("inputs.txt", text.as_bytes());

    let mut text = String::from("rebuilt core, oldest first: cpu_addr kind [function entry_cpu]\n");
    for (cpu, kind) in &inv.trace {
        let _ = match kind {
            TraceKind::Interrupt => writeln!(text, "{cpu:04x} interrupt-entry"),
            TraceKind::Fallback => writeln!(text, "{cpu:04x} fallback"),
            TraceKind::Atlas { function, prov } => match function {
                Some(f) => writeln!(text, "{cpu:04x} atlas({prov:?}) function {f}"),
                None => writeln!(text, "{cpu:04x} atlas({prov:?}) no-function"),
            },
        };
    }
    put("trace.txt", text.as_bytes());

    let mut text = format!(
        "first divergence at frame {} (0-based)\n\nframe-level differences:\n{}",
        d.frame,
        describe(&d.fields)
    );
    match &d.fine {
        Some(f) => {
            let _ = write!(
                text,
                "\ninstruction-level: after instruction {} of the frame (it started at pc {:#06x}):\n{}",
                f.instruction,
                f.pc_before,
                describe(&f.fields)
            );
        }
        None if opts.fine => text.push_str(
            "\ninstruction-level: no register, cycle or WRAM difference appeared inside the frame\n(a video/audio/other-memory difference, or one made before the frame started).\n",
        ),
        None => {}
    }
    put("diff.txt", text.as_bytes());
    if !inv.wram_plain.is_empty() {
        put("wram_plain.bin", &inv.wram_plain);
        put("wram_rebuilt.bin", &inv.wram_rebuilt);
    }
    let readme = format!(
        "RetroForge lockstep repro bundle (ticket W36-04)\n\
         ROM sha256 (normalised): {}\n\
         Diverging frame: {} (0-based, counted from power-on, Hard reset)\n\n\
         plain_state_before_frame.rfstate  the plain core at the start of that frame (.rfstate)\n\
         inputs.txt                        last {} input frames and the diverging one\n\
         trace.txt                         rebuilt core's last {} executed steps with atlas function ids\n\
         diff.txt                          which fields differed, first differing offsets\n\
         wram_*.bin                        work RAM of both cores at the end of the instruction pass\n\n\
         Replay: load the state in the app against the same ROM, feed inputs.txt from the\n\
         diverging frame with an atlas installed, and compare against a plain core.\n\
         No ROM bytes are in this bundle.\n",
        rf_cart::hash::identity_nes(raw).normalized.sha256,
        d.frame,
        opts.input_tail,
        opts.trace_len.next_power_of_two(),
    );
    put("README.txt", readme.as_bytes());
    if let Err(e) = &inv.state {
        files_errors.1.push(format!("savestate: {e}"));
    }
    files_errors
}
