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

/// The `PROF` payload, decoded.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProfChunk {
    pub profile_id: String,
    pub revision: String,
    /// Enabled mod ids, sorted — [`ModEngine::enabled_ids`] guarantees it,
    /// and the decoder does not re-sort so that a hand-written chunk with
    /// them out of order is still compared correctly by set semantics.
    pub mods: Vec<String>,
}

/// Chunk version, matching `rf-state`'s registry entry for `PROF`.
pub const PROF_VERSION: u16 = 1;

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
    if pos != bytes.len() {
        return Err(ProfError::TrailingBytes(bytes.len() - pos));
    }
    Ok(ProfChunk {
        profile_id,
        revision,
        mods,
    })
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
    });
    container.add_chunk(*b"PROF", PROF_VERSION, payload)
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
