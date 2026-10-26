//! The code atlas: functions and a control-flow graph recovered from a
//! coverage recording plus the ROM image (ticket W36-02).
//!
//! ## What is stored
//!
//! Structure only: ROM offsets, CPU addresses, instruction lengths, flow
//! classes, edges and a per-function byte hash. **No ROM bytes** (law 5): the
//! lifter (W36-03) re-reads the image the user already has, and W36-05
//! matches recipes against [`Function::byte_hash`].
//!
//! ## Provenance
//!
//! Everything carries [`Provenance`]. `Observed` instructions were executed
//! in the recording. An edge is `Observed` when **both of its endpoints
//! are** — the recorder logs which instructions ran, not which direct edges
//! were traversed, so a branch that ran and whose target also ran may in
//! principle never have been taken. Indirect transfers are different: every
//! recorded target of an [`IndirectSite`] is an exact fact. Guarded dispatch
//! (W36-03) leans on that; the lockstep checker (W36-04) leans on the
//! observed/inferred split to know which code has never been compared.

use bincode::{Decode, Encode};

use crate::isa::Flow;

/// Bumped on any change to the encoded shape. Part of the cache name
/// ([`CACHE_SETTINGS`]); a different number is a clean cache miss.
pub const ATLAS_FORMAT_VERSION: u32 = 4;
/// `rf-cache` key `producer` component.
pub const CACHE_PRODUCER: &str = "rf-rebuild";
/// `rf-cache` key `asset_hash` component: one atlas per ROM.
pub const CACHE_ASSET: &str = "code-atlas";
/// `rf-cache` key `settings_hash` component, `-vN` like rf-cache's other
/// users (e.g. `canvas-chunk-v3`).
pub const CACHE_SETTINGS: &str = "code-atlas-v4";

const MAGIC: [u8; 4] = *b"RFAT";
/// Upper bound on what `from_bytes` will allocate, so a corrupt cache entry
/// cannot name its own size (same stance as rf-cache's capped zstd read).
const MAX_DECODE_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
pub enum Provenance {
    /// Executed in the recording (or an indirect edge that was recorded).
    Observed,
    /// Reached only by static decode.
    Inferred,
}

/// Why a static target could not be placed in the ROM image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
pub enum Why {
    /// The address is not in the PRG window (RAM, I/O): volatile code.
    NotRom,
    /// No recorded mapping covers the target page.
    NoWindow,
    /// Several banks were seen at the target page and the site's own
    /// mapping does not pick one.
    Ambiguous,
    /// Resolves past the end of the image.
    OutOfImage,
    /// Resolves into the image but the bytes there do not form an
    /// acceptable instruction (overlaps executed bytes, or truncated).
    Undecodable,
    /// The code after a width-changing instruction (65816 `XCE`, `PLP`) was
    /// never seen running, so its decode mode is unknown (appended in
    /// format v3).
    WidthUnknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
pub enum Target {
    /// `cpu` is the address as the instruction named it (24-bit on the
    /// 65816), `mode` the decode mode the target is entered in.
    Rom {
        cpu: u32,
        rom: u32,
        mode: u8,
    },
    Unresolved {
        cpu: u32,
        why: Why,
    },
}

impl Target {
    #[must_use]
    pub fn rom(&self) -> Option<u32> {
        match *self {
            Target::Rom { rom, .. } => Some(rom),
            Target::Unresolved { .. } => None,
        }
    }

    /// The `(ROM offset, mode)` node key, `None` when unresolved.
    #[must_use]
    pub fn node(&self) -> Option<(u32, u8)> {
        match *self {
            Target::Rom { rom, mode, .. } => Some((rom, mode)),
            Target::Unresolved { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
pub enum EdgeKind {
    /// Straight-line continuation, or a conditional branch not taken.
    Fall,
    /// Conditional branch taken.
    Taken,
    Jump,
    Call,
    /// The instruction after a call: where the callee is expected to return.
    AfterCall,
}

impl EdgeKind {
    /// Edges a function's extent follows: everything except into a callee.
    #[must_use]
    pub fn stays_in_function(self) -> bool {
        self != EdgeKind::Call
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Encode, Decode)]
pub struct Edge {
    pub kind: EdgeKind,
    pub to: Target,
    pub prov: Provenance,
}

/// One instruction of a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct InsnRec {
    pub rom: u32,
    /// A CPU address the instruction was seen or reached at. A ROM byte
    /// visible through two windows has one record; this is the first
    /// address that found it.
    pub cpu: u32,
    pub len: u8,
    pub flow: Flow,
    pub prov: Provenance,
    /// The decode mode the instruction was decoded in (0 on the 6502).
    pub mode: u8,
}

/// A straight-line run of instructions: entered only at the top (it starts
/// at an entry, a branch/call/jump target, or after a branch/call) and left
/// only at the bottom. Blocks may belong to several functions.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct Block {
    pub start_rom: u32,
    pub start_cpu: u32,
    /// Decode mode of the first instruction.
    pub start_mode: u8,
    pub insns: Vec<InsnRec>,
    /// Successor edges of the last instruction (plus the fall edge into the
    /// next block when the run was cut by a leader).
    pub edges: Vec<Edge>,
}

/// Entry-point reasons, OR-ed into [`Function::entry_kinds`].
pub mod entry {
    /// Reset / NMI / IRQ / `BRK` vector target.
    pub const INTERRUPT: u8 = 1;
    /// Recorded indirect-jump, RTS-trick or RTI target.
    pub const INDIRECT: u8 = 2;
    /// Target of a decoded absolute call.
    pub const CALL: u8 = 4;
    /// Executed code no other entry reaches (e.g. entered from RAM code);
    /// listed so every executed instruction belongs to some function.
    pub const ORPHAN: u8 = 8;
    // Which interrupt (W38-00): [`INTERRUPT`] is always set beside one of
    // these, so older filters keep working. A function can carry several
    // (a vector table whose NMI and IRQ entries point at one handler).
    /// Target of a reset entry.
    pub const RESET: u8 = 16;
    /// Target of an NMI entry.
    pub const NMI: u8 = 32;
    /// Target of an IRQ entry.
    pub const IRQ: u8 = 64;
    /// Target of a `BRK` entry.
    pub const BRK: u8 = 128;
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct Function {
    pub entry_rom: u32,
    pub entry_cpu: u32,
    /// Decode mode the function is entered in. The key of a function is
    /// `(entry_rom, entry_mode)`: the same bytes entered in two modes are
    /// two functions.
    pub entry_mode: u8,
    /// OR of [`entry`] flags.
    pub entry_kinds: u8,
    /// Indices into [`Atlas::blocks`], ascending.
    pub blocks: Vec<u32>,
    pub insn_count: u32,
    pub byte_len: u32,
    /// FNV-1a 64 over the function's instruction bytes in (cpu, rom)
    /// address order, gaps skipped. Raises a recipe match's confidence
    /// (W36-05); changes when the function is moved and re-linked.
    pub byte_hash: u64,
    /// FNV-1a 64 over the same instructions with every absolute-address
    /// operand masked out (opcode bytes and the other operands kept; the
    /// rule is [`crate::Isa::masked_operand`]). Stable across a move that
    /// re-points absolute operands, which is what recipes match on
    /// (W36-05). Masked bytes are hashed as zero so the instruction
    /// lengths still count.
    pub shape_hash: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
pub enum SiteKind {
    JmpIndirect,
    Rts,
    Rti,
    Brk,
    /// 65816 `JSR (abs,X)` (appended in format v3).
    JsrIndirect,
    /// 65816 `RTL`.
    Rtl,
    /// 65816 `COP`.
    Cop,
}

/// An indirect control transfer in ROM and every target it was recorded
/// going to. The set is exact, not a guess; anything outside it is
/// unobserved.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct IndirectSite {
    pub site_rom: u32,
    pub site_cpu: u32,
    pub kind: SiteKind,
    pub targets: Vec<Target>,
}

/// A run of the core's volatile space that executed outside PRG ROM (CPU
/// addresses on the 6502, work-RAM offsets on the SNES: `$7E:0000` is 0).
/// Never an atlas function: the bytes can change underneath the recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct VolatileRegion {
    pub start: u32,
    pub len: u32,
}

/// Counts for reports and for tests that must prove a property.
#[derive(Debug, Clone, Default, PartialEq, Eq, Encode, Decode)]
pub struct Stats {
    pub observed_insns: u32,
    pub inferred_insns: u32,
    /// Static edges (branch, jump, call, fall) whose target did not resolve.
    pub unresolved_static: u32,
    /// Of those, ambiguous between several recorded banks.
    pub ambiguous_static: u32,
    /// Executed opcode `(offset, mode)` pairs that did not decode.
    pub undecodable_observed: u32,
    /// Executed opcode `(offset, mode)` pairs that are not an instruction start in some
    /// function. Zero by construction unless a walk budget was hit.
    pub observed_uncovered: u32,
    /// Inferred decodes dropped for overlapping bytes that really executed.
    pub overlap_rejected: u32,
    /// Recorded RTI targets that became function entries. The entry rule
    /// takes every recorded indirect target that is not a plain RTS return,
    /// which includes the interrupted instruction an RTI resumes at; this
    /// counts how many entries that rule adds.
    pub rti_entries: u32,
    pub orphan_entries: u32,
    /// Edges to a ROM offset that is not a block start (pure fall cycles).
    pub dangling_edges: u32,
    /// Indirect sites executed from RAM (no ROM offset), not listed.
    pub ram_indirect_sites: u32,
    /// Never-executed `(offset, mode)` pairs at which an instruction decodes
    /// (the superset view; see [`crate::superset`]).
    pub superset_candidates: u32,
    /// Of those, pairs also instruction starts in the atlas.
    pub superset_in_atlas: u32,
    /// A hard step budget stopped a walk early.
    pub budget_hit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct Atlas {
    pub prg_len: u32,
    /// Decode modes the ISA has (1 on the 6502, 5 on the 65816).
    pub mode_count: u8,
    /// Sorted by `start_rom`.
    pub blocks: Vec<Block>,
    /// Sorted by `entry_rom`.
    pub functions: Vec<Function>,
    /// Sorted by `site_rom`.
    pub indirect_sites: Vec<IndirectSite>,
    pub volatile: Vec<VolatileRegion>,
    pub stats: Stats,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AtlasError {
    BadMagic,
    Version(u32),
    Decode(String),
}

impl std::fmt::Display for AtlasError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AtlasError::BadMagic => write!(f, "not a code atlas"),
            AtlasError::Version(v) => write!(f, "atlas format version {v} is not supported"),
            AtlasError::Decode(e) => write!(f, "corrupt atlas: {e}"),
        }
    }
}

impl std::error::Error for AtlasError {}

impl Atlas {
    /// All instructions of all blocks, in block order.
    pub fn insns(&self) -> impl Iterator<Item = &InsnRec> {
        self.blocks.iter().flat_map(|b| b.insns.iter())
    }

    /// Serialise: magic, format version, bincode body.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&ATLAS_FORMAT_VERSION.to_le_bytes());
        // Encoding to a Vec cannot fail for these types (no custom Encode,
        // no size limit set).
        let body = bincode::encode_to_vec(self, wire_config()).unwrap_or_default();
        out.extend_from_slice(&body);
        out
    }

    /// Parse bytes from [`Atlas::to_bytes`].
    ///
    /// # Errors
    /// [`AtlasError`] for a foreign file, another format version, or a
    /// truncated or corrupt body (a cache layer treats all as a miss).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, AtlasError> {
        let head = bytes.get(..8).ok_or(AtlasError::BadMagic)?;
        if head[..4] != MAGIC {
            return Err(AtlasError::BadMagic);
        }
        let ver = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
        if ver != ATLAS_FORMAT_VERSION {
            return Err(AtlasError::Version(ver));
        }
        let (atlas, _) = bincode::decode_from_slice::<Atlas, _>(&bytes[8..], wire_config())
            .map_err(|e| AtlasError::Decode(e.to_string()))?;
        Ok(atlas)
    }
}

fn wire_config() -> impl bincode::config::Config {
    bincode::config::standard().with_limit::<MAX_DECODE_BYTES>()
}
