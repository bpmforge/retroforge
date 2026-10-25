//! The `PROF` save-state chunk: which profile and which mods were active
//! (ticket W7-12; FR-ENH-009's second and third clauses).
//!
//! `SAVE_STATES.md` §2 gives `PROF` to rf-profiles and describes it as
//! "active profile id + revision + enabled mods list". It is an
//! **optional** chunk, and that is the whole design: a state with no
//! `PROF` was taken by an unmodified machine, which is the overwhelmingly
//! common case and costs nothing to record.
//!
//! ## Why a state must carry its mods, and why a mismatch is refused
//!
//! A game-logic patch changes the instructions the machine executes. Every
//! byte of RAM in a save state was written by those instructions. Load
//! that state into a session running the *unpatched* code and the machine
//! is coherent in the same way a sentence with two words swapped is still
//! a sentence: nothing crashes, and everything downstream is subtly wrong.
//! `state_slots` already surfaces "contains mods" from this chunk's mere
//! presence; this module is what makes the contents mean something.
//!
//! Refusal is the default because the failure is silent otherwise. The
//! explicit alternative — adopt the state's set and carry on — is offered
//! as its own call so that it appears in the ledger as a decision someone
//! made rather than as something that quietly happened.

use rf_enhance::mods::{reconcile, ModEngine, ModError, Reconciliation};
use rf_state::Container;

/// One patch row a recipe mod applied (ticket W36-06b): which function of the recipe, by the exact
/// hash the row was resolved against, which byte of its instruction stream, and the new value. The
/// old byte is deliberately not stored (D-021).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchStamp {
    pub mod_name: String,
    pub name: String,
    pub function: String,
    pub exact_hash: u64,
    pub offset: u32,
    pub value: u8,
}

/// One memory write a hook script made (`rf_plugin_sdk::LedgerEntry`, serialised).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteStamp {
    pub frame: u64,
    pub addr: u32,
    pub value: u8,
    pub plugin: String,
}

/// The `PROF` payload, decoded.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProfChunk {
    pub profile_id: String,
    pub revision: String,
    /// Enabled mod ids, sorted — [`ModEngine::enabled_ids`] guarantees it,
    /// and the decoder does not re-sort so that a hand-written chunk with
    /// them out of order is still compared correctly by set semantics.
    pub mods: Vec<String>,
    /// The patch rows the mods applied (v2).
    pub patches: Vec<PatchStamp>,
    /// The hook host's write ledger when the state was taken (v2).
    pub ledger: Vec<WriteStamp>,
}

/// Chunk version, matching `rf-state`'s registry entry for `PROF`.
///
/// Ticket W36-06b added the patch rows and the hook ledger as an optional TAIL after the v1 fields,
/// introduced by [`TAIL_MARK`], rather than a new payload version: a v1 payload (no tail) stays a
/// valid payload of this version, so no state ever needs a migration to be read, and the registry
/// entry (read by every state decoder) is untouched.
pub const PROF_VERSION: u16 = 1;

/// Introduces the optional tail of a `PROF` payload (patch rows, then the write ledger).
const TAIL_MARK: &[u8; 4] = b"W36B";

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn take_n<'a>(bytes: &'a [u8], pos: &mut usize, n: usize) -> Result<&'a [u8], ProfError> {
    let end = pos.checked_add(n).filter(|e| *e <= bytes.len());
    let end = end.ok_or(ProfError::Truncated)?;
    let out = &bytes[*pos..end];
    *pos = end;
    Ok(out)
}

fn take_u32(bytes: &[u8], pos: &mut usize) -> Result<u32, ProfError> {
    let b = take_n(bytes, pos, 4)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn take_u64(bytes: &[u8], pos: &mut usize) -> Result<u64, ProfError> {
    let b = take_n(bytes, pos, 8)?;
    Ok(u64::from_le_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
    ]))
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn take_str(bytes: &[u8], pos: &mut usize) -> Result<String, ProfError> {
    if *pos + 4 > bytes.len() {
        return Err(ProfError::Truncated);
    }
    let len = u32::from_le_bytes(bytes[*pos..*pos + 4].try_into().unwrap()) as usize;
    *pos += 4;
    if *pos + len > bytes.len() {
        return Err(ProfError::Truncated);
    }
    let s = std::str::from_utf8(&bytes[*pos..*pos + len])
        .map_err(|_| ProfError::NotUtf8)?
        .to_string();
    *pos += len;
    Ok(s)
}

/// Why a `PROF` chunk could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfError {
    Truncated,
    NotUtf8,
    /// Bytes remained after the declared contents — the writer and reader
    /// disagree about the shape, so the part that parsed is not
    /// trustworthy either.
    TrailingBytes(usize),
}

impl std::fmt::Display for ProfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfError::Truncated => write!(f, "PROF chunk ended mid-field"),
            ProfError::NotUtf8 => write!(f, "PROF chunk holds a non-UTF-8 string"),
            ProfError::TrailingBytes(n) => {
                write!(f, "PROF chunk has {n} bytes after its declared contents")
            }
        }
    }
}

impl std::error::Error for ProfError {}

/// Encode a `PROF` payload.
#[must_use]
pub fn encode(chunk: &ProfChunk) -> Vec<u8> {
    let mut out = Vec::new();
    put_str(&mut out, &chunk.profile_id);
    put_str(&mut out, &chunk.revision);
    out.extend_from_slice(&(chunk.mods.len() as u32).to_le_bytes());
    for m in &chunk.mods {
        put_str(&mut out, m);
    }
    if chunk.patches.is_empty() && chunk.ledger.is_empty() {
        return out;
    }
    out.extend_from_slice(TAIL_MARK);
    put_u32(&mut out, chunk.patches.len() as u32);
    for p in &chunk.patches {
        put_str(&mut out, &p.mod_name);
        put_str(&mut out, &p.name);
        put_str(&mut out, &p.function);
        out.extend_from_slice(&p.exact_hash.to_le_bytes());
        put_u32(&mut out, p.offset);
        out.push(p.value);
    }
    put_u32(&mut out, chunk.ledger.len() as u32);
    for w in &chunk.ledger {
        out.extend_from_slice(&w.frame.to_le_bytes());
        put_u32(&mut out, w.addr);
        out.push(w.value);
        put_str(&mut out, &w.plugin);
    }
    out
}

/// Decode a `PROF` payload.
///
/// # Errors
/// [`ProfError`] for a truncated, non-UTF-8 or over-long payload.
pub fn decode(bytes: &[u8]) -> Result<ProfChunk, ProfError> {
    let mut pos = 0usize;
    let profile_id = take_str(bytes, &mut pos)?;
    let revision = take_str(bytes, &mut pos)?;
    if pos + 4 > bytes.len() {
        return Err(ProfError::Truncated);
    }
    let count = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
    pos += 4;
    let mut mods = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        mods.push(take_str(bytes, &mut pos)?);
    }
    let mut patches = Vec::new();
    let mut ledger = Vec::new();
    if bytes.get(pos..pos + 4) == Some(&TAIL_MARK[..]) {
        pos += 4;
        read_tail(bytes, &mut pos, &mut patches, &mut ledger)?;
    }
    if pos != bytes.len() {
        return Err(ProfError::TrailingBytes(bytes.len() - pos));
    }
    Ok(ProfChunk {
        profile_id,
        revision,
        mods,
        patches,
        ledger,
    })
}

/// The tail's two lists. Counts come from the file: the vectors grow only as entries parse, and every
/// pass of a loop reads at least one byte or fails (law 8), so a huge count ends at the data.
fn read_tail(
    bytes: &[u8],
    pos: &mut usize,
    patches: &mut Vec<PatchStamp>,
    ledger: &mut Vec<WriteStamp>,
) -> Result<(), ProfError> {
    let pos = &mut *pos;
    let n_patches = take_u32(bytes, pos)? as usize;
    for _ in 0..n_patches {
        patches.push(PatchStamp {
            mod_name: take_str(bytes, pos)?,
            name: take_str(bytes, pos)?,
            function: take_str(bytes, pos)?,
            exact_hash: take_u64(bytes, pos)?,
            offset: take_u32(bytes, pos)?,
            value: take_n(bytes, pos, 1)?[0],
        });
    }
    let n_writes = take_u32(bytes, pos)? as usize;
    for _ in 0..n_writes {
        ledger.push(WriteStamp {
            frame: take_u64(bytes, pos)?,
            addr: take_u32(bytes, pos)?,
            value: take_n(bytes, pos, 1)?[0],
            plugin: take_str(bytes, pos)?,
        });
    }
    Ok(())
}

/// Attach a `PROF` chunk describing the session's mods.
///
/// **Written whenever a profile is active, even with no mods enabled.**
/// Recording "this ran under profile X with nothing on" is a stronger
/// statement than recording nothing at all, and it is what lets a later
/// load tell "unmodified" apart from "saved before this feature existed".
///
/// # Errors
/// Propagates `rf-state`'s chunk error (duplicate tag, oversized payload).
pub fn attach(
    container: &mut Container,
    profile_id: &str,
    revision: &str,
    engine: &ModEngine,
) -> Result<(), rf_state::ContainerError> {
    let payload = encode(&ProfChunk {
        profile_id: profile_id.to_string(),
        revision: revision.to_string(),
        mods: engine.enabled_ids(),
        ..ProfChunk::default()
    });
    container.add_chunk(*b"PROF", PROF_VERSION, payload)
}

/// The recipe mods a session runs (ticket W36-06b), what the live save path stamps into `PROF`.
#[derive(Debug, Clone, Default)]
pub struct ModSession {
    /// Names of the recipe mods applied to the image (`SoftPatchSession::mods`).
    pub mods: Vec<String>,
    /// The recipe they came from: its patch rows and function hashes are what the state records.
    pub recipe: rf_rebuild::Recipe,
}

impl ModSession {
    /// The patch rows of the applied mods, with the exact hash of the function each edits.
    #[must_use]
    pub fn stamps(&self) -> Vec<PatchStamp> {
        self.recipe
            .patches
            .iter()
            .filter(|p| self.mods.contains(&p.mod_name))
            .map(|p| PatchStamp {
                mod_name: p.mod_name.clone(),
                name: p.name.clone(),
                function: p.function.clone(),
                exact_hash: self
                    .recipe
                    .functions
                    .iter()
                    .find(|f| f.name == p.function)
                    .and_then(|f| f.exact_hash)
                    .unwrap_or(0),
                offset: p.offset,
                value: p.value,
            })
            .collect()
    }
}

/// The `PROF` contents for the live save path: the session's mods and patch rows plus the hook
/// host's ledger. `None` for a session with neither (an unmodified machine writes no `PROF`, which
/// is what lets a load tell it from a modded one).
#[must_use]
pub fn session_chunk(
    session: Option<&ModSession>,
    ledger: &[rf_plugin_sdk::LedgerEntry],
) -> Option<ProfChunk> {
    let mods = session.map(|s| s.mods.clone()).unwrap_or_default();
    if mods.is_empty() && ledger.is_empty() {
        return None;
    }
    let mut mods = mods;
    mods.sort();
    Some(ProfChunk {
        profile_id: session.map(|s| s.recipe.id.clone()).unwrap_or_default(),
        revision: session.map(|s| s.recipe.engine.clone()).unwrap_or_default(),
        mods,
        patches: session.map(ModSession::stamps).unwrap_or_default(),
        ledger: ledger
            .iter()
            .map(|e| WriteStamp {
                frame: e.frame,
                addr: e.addr,
                value: e.value,
                plugin: e.plugin.clone(),
            })
            .collect(),
    })
}

/// Add `chunk` to `container` as its `PROF`.
///
/// # Errors
/// Propagates `rf-state`'s chunk error.
pub fn attach_chunk(
    container: &mut Container,
    chunk: &ProfChunk,
) -> Result<(), rf_state::ContainerError> {
    container.add_chunk(*b"PROF", PROF_VERSION, encode(chunk))
}

/// The ledger a `PROF` carries, as the hook host's own entries.
#[must_use]
pub fn ledger_of(chunk: &ProfChunk) -> Vec<rf_plugin_sdk::LedgerEntry> {
    chunk
        .ledger
        .iter()
        .map(|w| rf_plugin_sdk::LedgerEntry {
            frame: w.frame,
            addr: w.addr,
            value: w.value,
            plugin: w.plugin.clone(),
        })
        .collect()
}

/// Plain-words lines for a load: what the state's `PROF` says its machine ran under, when that is not
/// what the session runs under (`session_mods`: the applied recipe mods). Empty when they agree or the
/// state carries no `PROF`.
#[must_use]
pub fn mismatch_lines(container: &Container, session_mods: &[String]) -> Vec<String> {
    let Some(chunk) = container.chunk(*b"PROF") else {
        return if session_mods.is_empty() {
            Vec::new()
        } else {
            vec![format!(
                "This state was saved without {}; this session runs it, so the machine in the state \
                 was not made by the modded code.",
                mod_list(session_mods)
            )]
        };
    };
    let Ok(prof) = decode(&chunk.payload) else {
        return vec!["This state's mod record is unreadable.".to_string()];
    };
    let mut lines = Vec::new();
    let mut theirs = prof.mods.clone();
    let mut ours = session_mods.to_vec();
    theirs.sort();
    ours.sort();
    if theirs != ours {
        lines.push(if theirs.is_empty() {
            format!(
                "This state was saved by an unmodified machine; this session runs {}.",
                mod_list(&ours)
            )
        } else {
            format!(
                "This state was saved with {} on ({} patched byte(s), {} hook write(s) recorded); this \
                 session does not run it, so the state's memory came from code this session lacks and \
                 play from here may differ.",
                mod_list(&theirs),
                prof.patches.len(),
                prof.ledger.len()
            )
        });
    }
    lines
}

fn mod_list(mods: &[String]) -> String {
    match mods {
        [] => "no mods".to_string(),
        [one] => format!("mod {one}"),
        many => format!("mods {}", many.join(", ")),
    }
}

/// What a load found when it compared a state's mods with the session's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModCheck {
    /// No `PROF` chunk, and no mods active: an unmodified state loaded into
    /// an unmodified session.
    NoModsEitherSide,
    /// The state and the session agree.
    Match,
    /// They disagree. **The load must not proceed on this** without an
    /// explicit choice — see [`adopt_state_mods`].
    Mismatch(Reconciliation),
    /// The chunk is present but unreadable.
    Unreadable(ProfError),
}

impl std::fmt::Display for ModCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModCheck::NoModsEitherSide => write!(f, "no mods on either side"),
            ModCheck::Match => write!(f, "mods match"),
            ModCheck::Mismatch(r) => write!(f, "{r}"),
            ModCheck::Unreadable(e) => write!(f, "{e}"),
        }
    }
}

impl ModCheck {
    /// True only when loading is safe without a decision.
    #[must_use]
    pub fn is_loadable(&self) -> bool {
        matches!(self, ModCheck::NoModsEitherSide | ModCheck::Match)
    }
}

/// Compare a container's recorded mods against the live engine.
///
/// **A state with no `PROF` and a session WITH mods is a mismatch**, not a
/// pass. That asymmetry is deliberate and is the case most likely to be
/// got wrong: the missing chunk means the state was captured by an
/// unmodified machine, so loading it into a patched session is exactly the
/// silent mismatch criterion 3 forbids — in the direction where the RAM is
/// innocent and the code is not.
#[must_use]
pub fn check(container: &Container, engine: &ModEngine) -> ModCheck {
    let active = engine.enabled_ids();
    let Some(chunk) = container.chunk(*b"PROF") else {
        return if active.is_empty() {
            ModCheck::NoModsEitherSide
        } else {
            ModCheck::Mismatch(Reconciliation::Mismatch {
                only_in_state: Vec::new(),
                only_active: active,
            })
        };
    };
    match decode(&chunk.payload) {
        Err(e) => ModCheck::Unreadable(e),
        Ok(prof) => match reconcile(&prof.mods, &active) {
            Reconciliation::Match => ModCheck::Match,
            other => ModCheck::Mismatch(other),
        },
    }
}

/// Resolve a mismatch by adopting the state's set — the explicit
/// alternative to refusing.
///
/// Separate from [`check`] on purpose: the ledger then carries the
/// enable/disable transitions this caused, at the frame it happened, so
/// "the emulator changed my mods" is answerable after the fact.
///
/// # Errors
/// [`ModError::UnknownId`] if the state names a mod this profile does not
/// declare — which means the state belongs to a different profile revision,
/// and nothing is changed.
pub fn adopt_state_mods(
    container: &Container,
    engine: &mut ModEngine,
    frame: u64,
) -> Result<(), ModError> {
    let ids = container
        .chunk(*b"PROF")
        .and_then(|c| decode(&c.payload).ok())
        .map(|p| p.mods)
        .unwrap_or_default();
    engine.adopt(&ids, frame)
}
