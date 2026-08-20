//! `.rfreplay` — BK2-shaped input-log replay (ticket W1-07,
//! `docs/design/SAVE_STATES.md` §3, FR-STATE-006).
//!
//! **Format decision (conductor pre-flight, not re-litigated here):** a
//! single UTF-8 **text** file — not a binary TLV like `.rfstate`
//! (`docs/design/SAVE_STATES.md` §2) and not a literal BizHawk `.bk2` zip.
//! Text keeps a replay diffable and hand-editable for TAS/bug repro, needs
//! no new dependency, and keeps a later "import a BizHawk header" cheap
//! (`docs/research/accuracy-and-testing.md` §5 calls BK2's `Input Log.txt`
//! shape "excellent model for our input-log format").
//!
//! ## Wire shape
//!
//! ```text
//! RFREPLAY 1
//! [Header]
//! console=nes
//! rom_sha256=<64 lowercase hex>
//! emu_version=<semver>
//! core_config=accuracy
//! start_type=power-on
//! hash_kind=reachable-v1
//! hash_interval=<u64>
//! [LogKey]
//! P1:A,B,Select,Start,Up,Down,Left,Right
//! P2:A,B,Select,Start,Up,Down,Left,Right
//! [Input]
//! |........|........|
//! |A..S....|........|
//! [Hashes]
//! 59=<64 lowercase hex>
//! 119=<64 lowercase hex>
//! ```
//!
//! Normative rules (NFR-008: public from first release, so these are
//! commitments, not suggestions):
//!
//! - **LF line endings only.** A CRLF file is refused outright, not
//!   silently accepted — byte-determinism matters more than politeness.
//! - Every byte must be ASCII; non-ASCII content is refused.
//! - Sections appear in exactly this order: `[Header]`, `[LogKey]`,
//!   `[Input]`, `[Hashes]` — no reordering, none optional.
//! - Unpressed button = `.`; pressed = its BizHawk-compatible mnemonic
//!   char ([`crate::NesButton::mnemonic`]). Order within each `|...|`
//!   group is the matching `[LogKey]` port's declared order, which is the
//!   `$4016` read order for the canonical table this crate writes.
//! - Frame numbers are 0-indexed: the Nth `|...|` line in `[Input]` is
//!   frame N.
//! - `[Hashes]` lines are `frame=hex`, meant to be emitted every
//!   `hash_interval` frames **and always for the final frame** — the
//!   caller (`crates/retroforge`, which owns the state hash) is
//!   responsible for that cadence; this module just stores whatever pairs
//!   it's given.
//! - `hash_kind` names *what* was hashed (today: `reachable-v1`, see
//!   `crates/retroforge/src/stepper.rs`'s `EmuStepper::state_hash` doc for
//!   the exact field list), so a later ticket can upgrade to a
//!   full-machine hash without silently changing what the field means.
//! - Only `start_type=power-on` is accepted in this ticket; any other
//!   value is refused with a "not supported until W2-04" message rather
//!   than pretending to honor it.
//!
//! Refusals never panic: [`ReplayLog::parse`] returns [`ReplayError`] with
//! a diagnostic for unknown magic/version, an unsupported `start_type`, a
//! malformed input line, a port-count mismatch, CRLF, or non-ASCII
//! content. ROM-hash mismatch is a separate caller-driven check
//! ([`ReplayLog::verify_rom_sha256`]) because this crate never sees the
//! loaded ROM's bytes — `crates/retroforge` computes that hash (via
//! `rf-cart`, already its dependency) and passes it in.
use std::fmt;

use rf_core_api::InputFrame;

use crate::{Button, NesButton, SnesButton};

/// `.rfreplay`'s only supported `start_type` value in this ticket.
/// Savestate-anchored replay start is ticket W2-04's job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartType {
    PowerOn,
}

impl StartType {
    const fn as_str(self) -> &'static str {
        match self {
            StartType::PowerOn => "power-on",
        }
    }
}

/// `.rfreplay` `[Header]` fields (see module doc for the wire shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayHeader {
    pub console: String,
    /// Normalized ROM SHA-256, 64 lowercase hex chars (same convention
    /// `.rfstate` uses — `docs/design/SAVE_STATES.md` §2).
    pub rom_sha256: String,
    pub emu_version: String,
    pub core_config: String,
    pub start_type: StartType,
    /// Names *what* was hashed for `[Hashes]` entries — `"reachable-v1"`
    /// today (module doc).
    pub hash_kind: String,
    pub hash_interval: u64,
}

/// One `[LogKey]` port entry: a label (`"P1"`) plus the ordered button
/// list that entry's `[Input]` column group encodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortLogKey {
    pub label: String,
    pub buttons: Vec<Button>,
}

impl PortLogKey {
    /// The canonical single-port entry for a console.
    ///
    /// NES is the full `$4016` read order ([`NesButton::ALL`]); SNES is
    /// the `$4218`/`$4219` order ([`SnesButton::ALL`]).
    fn canonical(label: &str, console: Console) -> Self {
        let buttons = match console {
            Console::Nes => NesButton::ALL.iter().copied().map(Button::Nes).collect(),
            Console::Snes => SnesButton::ALL.iter().copied().map(Button::Snes).collect(),
        };
        PortLogKey {
            label: label.to_string(),
            buttons,
        }
    }
}

/// Which console's button table a log key uses.
///
/// Chosen from the `[Header]`'s `console` field. **Anything unrecognised
/// is treated as NES**, deliberately: that is what every existing
/// `.rfreplay` in the world decodes as today, and a format change must
/// not reinterpret files it did not write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Console {
    Nes,
    Snes,
}

impl Console {
    #[must_use]
    pub fn from_header(console: &str) -> Self {
        if console.eq_ignore_ascii_case("snes") {
            Console::Snes
        } else {
            Console::Nes
        }
    }
}

/// The canonical two-controller `[LogKey]` table this crate's recorder
/// writes (module doc's wire shape), for a given console.
fn canonical_log_key(console: Console) -> Vec<PortLogKey> {
    vec![
        PortLogKey::canonical("P1", console),
        PortLogKey::canonical("P2", console),
    ]
}

/// Every refusal [`ReplayLog::parse`]/[`ReplayLog::verify_rom_sha256`] can
/// report — always a diagnostic, never a panic (module doc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayError {
    /// The file contains a `\r` byte — CRLF (or a lone CR) is refused
    /// outright, not silently normalized.
    CrlfNotAllowed,
    /// The file contains a byte >= 0x80.
    NonAscii,
    /// First line isn't `RFREPLAY <version>` at all.
    BadMagic(String),
    /// First line's magic is right but the version isn't one this crate
    /// understands.
    UnsupportedVersion(String),
    /// `start_type` is a recognized *field* but not a supported *value*.
    UnsupportedStartType(String),
    /// A section/header/log-key/hash line didn't parse, with a
    /// human-readable reason.
    Malformed(String),
    /// An `[Input]` line, with its 1-based line number and a reason.
    MalformedInputLine { line: usize, reason: String },
    /// An `[Input]` line's `|...|` group count didn't match `[LogKey]`'s
    /// declared port count.
    PortCountMismatch {
        line: usize,
        expected: usize,
        found: usize,
    },
    /// [`ReplayLog::verify_rom_sha256`]: the loaded ROM's hash doesn't
    /// match the header's `rom_sha256`.
    RomShaMismatch { expected: String, actual: String },
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReplayError::CrlfNotAllowed => {
                write!(f, ".rfreplay: CRLF line endings are refused (LF only)")
            }
            ReplayError::NonAscii => write!(f, ".rfreplay: non-ASCII byte in file"),
            ReplayError::BadMagic(got) => {
                write!(
                    f,
                    ".rfreplay: bad magic (expected \"RFREPLAY <n>\"), got {got:?}"
                )
            }
            ReplayError::UnsupportedVersion(v) => {
                write!(
                    f,
                    ".rfreplay: unsupported format version {v:?} (only \"1\" is known)"
                )
            }
            ReplayError::UnsupportedStartType(v) => write!(
                f,
                ".rfreplay: start_type={v:?} not supported until W2-04 (only power-on today)"
            ),
            ReplayError::Malformed(reason) => write!(f, ".rfreplay: malformed — {reason}"),
            ReplayError::MalformedInputLine { line, reason } => {
                write!(f, ".rfreplay: malformed [Input] line {line}: {reason}")
            }
            ReplayError::PortCountMismatch {
                line,
                expected,
                found,
            } => write!(
                f,
                ".rfreplay: [Input] line {line}: expected {expected} port group(s) \
                 (per [LogKey]), found {found}"
            ),
            ReplayError::RomShaMismatch { expected, actual } => write!(
                f,
                ".rfreplay: rom_sha256 mismatch — replay expects {expected}, loaded ROM is {actual}"
            ),
        }
    }
}

impl std::error::Error for ReplayError {}

/// A parsed (or freshly recorded) `.rfreplay` — round-trips byte-exactly
/// through [`ReplayLog::parse`] / the [`fmt::Display`] impl (`.to_string()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayLog {
    pub header: ReplayHeader,
    pub log_key: Vec<PortLogKey>,
    /// `frames[n]` is frame `n` (0-indexed, module doc).
    pub frames: Vec<InputFrame>,
    /// `(frame_no, hex_hash)` pairs, in file order.
    pub hashes: Vec<(u64, String)>,
}

/// A section this format recognizes, in the one order they're allowed to
/// appear (module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Header,
    LogKey,
    Input,
    Hashes,
}

fn is_hex64_lowercase(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl ReplayLog {
    /// Parse `.rfreplay` text (module doc for the wire shape). Never
    /// panics — every rejection is a [`ReplayError`] with a diagnostic.
    ///
    /// # Errors
    /// See [`ReplayError`]'s variants.
    pub fn parse(text: &str) -> Result<Self, ReplayError> {
        if text.as_bytes().contains(&b'\r') {
            return Err(ReplayError::CrlfNotAllowed);
        }
        if !text.is_ascii() {
            return Err(ReplayError::NonAscii);
        }

        let mut lines = text.lines();
        let first = lines.next().unwrap_or("");
        let mut magic_parts = first.splitn(2, ' ');
        let magic = magic_parts.next().unwrap_or("");
        if magic != "RFREPLAY" {
            return Err(ReplayError::BadMagic(first.to_string()));
        }
        let version = magic_parts.next().unwrap_or("");
        if version != "1" {
            return Err(ReplayError::UnsupportedVersion(version.to_string()));
        }

        let mut current: Option<Section> = None;
        let mut header_lines: Vec<&str> = Vec::new();
        let mut logkey_lines: Vec<&str> = Vec::new();
        let mut input_lines: Vec<(usize, &str)> = Vec::new();
        let mut hash_lines: Vec<&str> = Vec::new();

        for (offset, line) in lines.enumerate() {
            let line_no = offset + 2; // 1-based; line 1 was the magic line
            match line {
                "[Header]" => {
                    if current.is_some() {
                        return Err(ReplayError::Malformed(
                            "[Header] must be the first section".to_string(),
                        ));
                    }
                    current = Some(Section::Header);
                }
                "[LogKey]" => {
                    if current != Some(Section::Header) {
                        return Err(ReplayError::Malformed(
                            "[LogKey] must immediately follow [Header]".to_string(),
                        ));
                    }
                    current = Some(Section::LogKey);
                }
                "[Input]" => {
                    if current != Some(Section::LogKey) {
                        return Err(ReplayError::Malformed(
                            "[Input] must immediately follow [LogKey]".to_string(),
                        ));
                    }
                    current = Some(Section::Input);
                }
                "[Hashes]" => {
                    if current != Some(Section::Input) {
                        return Err(ReplayError::Malformed(
                            "[Hashes] must immediately follow [Input]".to_string(),
                        ));
                    }
                    current = Some(Section::Hashes);
                }
                _ => match current {
                    Some(Section::Header) => header_lines.push(line),
                    Some(Section::LogKey) => logkey_lines.push(line),
                    Some(Section::Input) => input_lines.push((line_no, line)),
                    Some(Section::Hashes) => hash_lines.push(line),
                    None => {
                        return Err(ReplayError::Malformed(format!(
                            "line {line_no}: content before [Header]"
                        )))
                    }
                },
            }
        }
        if current.is_none() {
            return Err(ReplayError::Malformed(
                "no [Header] section found".to_string(),
            ));
        }

        let header = parse_header(&header_lines)?;
        let log_key = parse_log_key(&logkey_lines, Console::from_header(&header.console))?;
        let frames = parse_input(&input_lines, &log_key)?;
        let hashes = parse_hashes(&hash_lines)?;

        Ok(ReplayLog {
            header,
            log_key,
            frames,
            hashes,
        })
    }

    /// Compare the header's `rom_sha256` against `actual` (the caller's
    /// already-computed normalized hash of the loaded ROM — this crate
    /// never touches ROM bytes itself, see module doc). Both sides are
    /// compared byte-for-byte; the format requires lowercase hex on both,
    /// so this deliberately does not case-fold.
    ///
    /// # Errors
    /// [`ReplayError::RomShaMismatch`] if they differ.
    pub fn verify_rom_sha256(&self, actual: &str) -> Result<(), ReplayError> {
        if self.header.rom_sha256 != actual {
            return Err(ReplayError::RomShaMismatch {
                expected: self.header.rom_sha256.clone(),
                actual: actual.to_string(),
            });
        }
        Ok(())
    }
}

impl fmt::Display for ReplayLog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "RFREPLAY 1")?;
        writeln!(f, "[Header]")?;
        writeln!(f, "console={}", self.header.console)?;
        writeln!(f, "rom_sha256={}", self.header.rom_sha256)?;
        writeln!(f, "emu_version={}", self.header.emu_version)?;
        writeln!(f, "core_config={}", self.header.core_config)?;
        writeln!(f, "start_type={}", self.header.start_type.as_str())?;
        writeln!(f, "hash_kind={}", self.header.hash_kind)?;
        writeln!(f, "hash_interval={}", self.header.hash_interval)?;
        writeln!(f, "[LogKey]")?;
        for port in &self.log_key {
            let names: Vec<&str> = port.buttons.iter().map(|b| b.name()).collect();
            writeln!(f, "{}:{}", port.label, names.join(","))?;
        }
        writeln!(f, "[Input]")?;
        for frame in &self.frames {
            write!(f, "|")?;
            for (idx, port) in self.log_key.iter().enumerate() {
                let bits = frame.ports.get(idx).copied().unwrap_or(0);
                for &button in &port.buttons {
                    let pressed = bits & (1u16 << button.bit()) != 0;
                    write!(f, "{}", if pressed { button.mnemonic() } else { '.' })?;
                }
                write!(f, "|")?;
            }
            writeln!(f)?;
        }
        writeln!(f, "[Hashes]")?;
        for (frame_no, hash) in &self.hashes {
            writeln!(f, "{frame_no}={hash}")?;
        }
        Ok(())
    }
}

fn parse_header(lines: &[&str]) -> Result<ReplayHeader, ReplayError> {
    let mut console = None;
    let mut rom_sha256 = None;
    let mut emu_version = None;
    let mut core_config = None;
    let mut start_type = None;
    let mut hash_kind = None;
    let mut hash_interval = None;

    for line in lines {
        let Some((key, value)) = line.split_once('=') else {
            return Err(ReplayError::Malformed(format!(
                "[Header] line {line:?} is not key=value"
            )));
        };
        match key {
            "console" => console = Some(value.to_string()),
            "rom_sha256" => {
                if !is_hex64_lowercase(value) {
                    return Err(ReplayError::Malformed(format!(
                        "rom_sha256={value:?} is not 64 lowercase hex chars"
                    )));
                }
                rom_sha256 = Some(value.to_string());
            }
            "emu_version" => emu_version = Some(value.to_string()),
            "core_config" => core_config = Some(value.to_string()),
            "start_type" => {
                start_type = Some(if value == "power-on" {
                    StartType::PowerOn
                } else {
                    return Err(ReplayError::UnsupportedStartType(value.to_string()));
                });
            }
            "hash_kind" => hash_kind = Some(value.to_string()),
            "hash_interval" => {
                let parsed: u64 = value.parse().map_err(|_| {
                    ReplayError::Malformed(format!("hash_interval={value:?} is not a u64"))
                })?;
                hash_interval = Some(parsed);
            }
            other => {
                return Err(ReplayError::Malformed(format!(
                    "unknown [Header] key {other:?}"
                )))
            }
        }
    }

    Ok(ReplayHeader {
        console: console
            .ok_or_else(|| ReplayError::Malformed("[Header] missing console".to_string()))?,
        rom_sha256: rom_sha256
            .ok_or_else(|| ReplayError::Malformed("[Header] missing rom_sha256".to_string()))?,
        emu_version: emu_version
            .ok_or_else(|| ReplayError::Malformed("[Header] missing emu_version".to_string()))?,
        core_config: core_config
            .ok_or_else(|| ReplayError::Malformed("[Header] missing core_config".to_string()))?,
        start_type: start_type
            .ok_or_else(|| ReplayError::Malformed("[Header] missing start_type".to_string()))?,
        hash_kind: hash_kind
            .ok_or_else(|| ReplayError::Malformed("[Header] missing hash_kind".to_string()))?,
        hash_interval: hash_interval
            .ok_or_else(|| ReplayError::Malformed("[Header] missing hash_interval".to_string()))?,
    })
}

fn parse_log_key(lines: &[&str], console: Console) -> Result<Vec<PortLogKey>, ReplayError> {
    if lines.is_empty() {
        return Err(ReplayError::Malformed(
            "[LogKey] has no port entries".to_string(),
        ));
    }
    if lines.len() > rf_core_api::MAX_INPUT_PORTS {
        return Err(ReplayError::Malformed(format!(
            "[LogKey] declares {} ports, more than MAX_INPUT_PORTS={}",
            lines.len(),
            rf_core_api::MAX_INPUT_PORTS
        )));
    }
    let mut ports = Vec::with_capacity(lines.len());
    for line in lines {
        let Some((label, rest)) = line.split_once(':') else {
            return Err(ReplayError::Malformed(format!(
                "[LogKey] line {line:?} is not \"label:buttons\""
            )));
        };
        if label.is_empty() {
            return Err(ReplayError::Malformed(format!(
                "[LogKey] line {line:?} has an empty label"
            )));
        }
        let mut buttons = Vec::new();
        for name in rest.split(',') {
            // Console-directed on purpose: `A`, `B`, `Select`, `Start` and
            // the four directions exist on both consoles at DIFFERENT
            // bits, so a name-only lookup across both tables would
            // silently mis-decode one of them.
            let button = match console {
                Console::Nes => NesButton::from_name(name).map(Button::Nes),
                Console::Snes => SnesButton::from_name(name).map(Button::Snes),
            }
            .ok_or_else(|| {
                ReplayError::Malformed(format!("[LogKey] unknown {console:?} button name {name:?}"))
            })?;
            buttons.push(button);
        }
        ports.push(PortLogKey {
            label: label.to_string(),
            buttons,
        });
    }
    Ok(ports)
}

fn parse_input(
    lines: &[(usize, &str)],
    log_key: &[PortLogKey],
) -> Result<Vec<InputFrame>, ReplayError> {
    let mut frames = Vec::with_capacity(lines.len());
    for &(line_no, line) in lines {
        if !(line.starts_with('|') && line.ends_with('|') && line.len() >= 2) {
            return Err(ReplayError::MalformedInputLine {
                line: line_no,
                reason: "must start and end with '|'".to_string(),
            });
        }
        let inner = &line[1..line.len() - 1];
        let groups: Vec<&str> = inner.split('|').collect();
        if groups.len() != log_key.len() {
            return Err(ReplayError::PortCountMismatch {
                line: line_no,
                expected: log_key.len(),
                found: groups.len(),
            });
        }

        let mut frame = InputFrame::empty();
        for (port_idx, (group, port_key)) in groups.iter().zip(log_key).enumerate() {
            let chars: Vec<char> = group.chars().collect();
            if chars.len() != port_key.buttons.len() {
                return Err(ReplayError::MalformedInputLine {
                    line: line_no,
                    reason: format!(
                        "port {} ({}) expected {} chars, got {}",
                        port_idx,
                        port_key.label,
                        port_key.buttons.len(),
                        chars.len()
                    ),
                });
            }
            let Some(bits) = frame.ports.get_mut(port_idx) else {
                return Err(ReplayError::MalformedInputLine {
                    line: line_no,
                    reason: format!("port index {port_idx} exceeds MAX_INPUT_PORTS"),
                });
            };
            for (button, ch) in port_key.buttons.iter().zip(chars) {
                if ch == '.' {
                    continue;
                }
                if ch == button.mnemonic() {
                    *bits |= 1u16 << button.bit();
                } else {
                    return Err(ReplayError::MalformedInputLine {
                        line: line_no,
                        reason: format!(
                            "port {port_idx}: expected '.' or '{}' for {:?}, got '{ch}'",
                            button.mnemonic(),
                            button
                        ),
                    });
                }
            }
        }
        frames.push(frame);
    }
    Ok(frames)
}

fn parse_hashes(lines: &[&str]) -> Result<Vec<(u64, String)>, ReplayError> {
    let mut hashes = Vec::with_capacity(lines.len());
    for line in lines {
        let Some((frame_str, hash_str)) = line.split_once('=') else {
            return Err(ReplayError::Malformed(format!(
                "[Hashes] line {line:?} is not frame=hex"
            )));
        };
        let frame_no: u64 = frame_str.parse().map_err(|_| {
            ReplayError::Malformed(format!("[Hashes] frame {frame_str:?} is not a u64"))
        })?;
        if !is_hex64_lowercase(hash_str) {
            return Err(ReplayError::Malformed(format!(
                "[Hashes] hash {hash_str:?} is not 64 lowercase hex chars"
            )));
        }
        hashes.push((frame_no, hash_str.to_string()));
    }
    Ok(hashes)
}

/// Accumulates a `.rfreplay` while a recording session drives an emulator
/// forward. This type only accumulates data — the actual "latch input,
/// then advance one frame" call happens in `crates/retroforge` (which owns
/// the emulator), through a single shared function
/// (`EmuStepper::latch_and_advance_frame`) that both recording and
/// playback go through, so the two can never diverge in *how* input is
/// applied (module doc / ticket W1-07).
#[derive(Debug, Clone)]
pub struct ReplayRecorder {
    header: ReplayHeader,
    log_key: Vec<PortLogKey>,
    frames: Vec<InputFrame>,
    hashes: Vec<(u64, String)>,
}

impl ReplayRecorder {
    #[must_use]
    pub fn new(header: ReplayHeader) -> Self {
        let console = Console::from_header(&header.console);
        ReplayRecorder {
            header,
            log_key: canonical_log_key(console),
            frames: Vec::new(),
            hashes: Vec::new(),
        }
    }

    /// How often (in frames) the caller is expected to also call
    /// [`ReplayRecorder::record_hash`] — mirrors `header.hash_interval`.
    #[must_use]
    pub fn hash_interval(&self) -> u64 {
        self.header.hash_interval
    }

    /// Append one frame's input. Frame index = `self.frames.len()` before
    /// this call (module doc: "Nth `|` line is frame N").
    pub fn record_frame(&mut self, frame: InputFrame) {
        self.frames.push(frame);
    }

    /// Record a state hash for `frame_no` (0-indexed, the frame just
    /// completed).
    pub fn record_hash(&mut self, frame_no: u64, hash: String) {
        self.hashes.push((frame_no, hash));
    }

    /// Total frames recorded so far.
    #[must_use]
    pub fn frame_count(&self) -> u64 {
        self.frames.len() as u64
    }

    /// Consume the recorder into a finished [`ReplayLog`].
    #[must_use]
    pub fn finish(self) -> ReplayLog {
        ReplayLog {
            header: self.header,
            log_key: self.log_key,
            frames: self.frames,
            hashes: self.hashes,
        }
    }
}

/// Plays a parsed [`ReplayLog`] back frame-by-frame.
#[derive(Debug, Clone)]
pub struct ReplayPlayer<'a> {
    log: &'a ReplayLog,
    cursor: usize,
}

impl<'a> ReplayPlayer<'a> {
    #[must_use]
    pub fn new(log: &'a ReplayLog) -> Self {
        ReplayPlayer { log, cursor: 0 }
    }

    /// The next logged frame's input, advancing the cursor — `None` once
    /// the log is exhausted.
    pub fn next_frame(&mut self) -> Option<InputFrame> {
        let frame = self.log.frames.get(self.cursor).copied();
        if frame.is_some() {
            self.cursor += 1;
        }
        frame
    }

    /// Total frames in the log being played.
    #[must_use]
    pub fn total_frames(&self) -> usize {
        self.log.frames.len()
    }

    /// The expected state hash at `frame_no`, if the log records one.
    #[must_use]
    pub fn expected_hash(&self, frame_no: u64) -> Option<&str> {
        self.log
            .hashes
            .iter()
            .find(|(f, _)| *f == frame_no)
            .map(|(_, h)| h.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_header() -> ReplayHeader {
        ReplayHeader {
            console: "nes".to_string(),
            rom_sha256: "a".repeat(64),
            emu_version: "0.1.0".to_string(),
            core_config: "accuracy".to_string(),
            start_type: StartType::PowerOn,
            hash_kind: "reachable-v1".to_string(),
            hash_interval: 60,
        }
    }

    fn sample_log() -> ReplayLog {
        let mut rec = ReplayRecorder::new(sample_header());
        let mut f0 = InputFrame::empty();
        f0.ports[0] = 1 << NesButton::A.bit();
        rec.record_frame(f0);
        rec.record_frame(InputFrame::empty());
        rec.record_hash(1, "b".repeat(64));
        rec.finish()
    }

    #[test]
    fn round_trip_is_byte_exact() {
        let log = sample_log();
        let text = log.to_string();
        let parsed = ReplayLog::parse(&text).expect("valid replay text must parse");
        let text2 = parsed.to_string();
        assert_eq!(text, text2, "round trip must be byte-exact");
        assert_eq!(log, parsed);
    }

    #[test]
    fn to_string_uses_lf_only() {
        let text = sample_log().to_string();
        assert!(!text.contains('\r'));
    }

    #[test]
    fn canonical_shape_matches_the_documented_example() {
        let text = sample_log().to_string();
        assert!(text.starts_with("RFREPLAY 1\n[Header]\n"));
        assert!(text.contains("[LogKey]\nP1:A,B,Select,Start,Up,Down,Left,Right\n"));
        assert!(text.contains("P2:A,B,Select,Start,Up,Down,Left,Right\n"));
        assert!(text.contains("[Input]\n|A.......|........|\n|........|........|\n"));
        assert!(text.contains("[Hashes]\n1="));
    }

    #[test]
    fn parse_rejects_crlf() {
        let text = sample_log().to_string().replace('\n', "\r\n");
        assert_eq!(ReplayLog::parse(&text), Err(ReplayError::CrlfNotAllowed));
    }

    #[test]
    fn parse_rejects_non_ascii() {
        let mut text = sample_log().to_string();
        text.push('é');
        assert_eq!(ReplayLog::parse(&text), Err(ReplayError::NonAscii));
    }

    #[test]
    fn parse_rejects_bad_magic() {
        let text = "NOT-A-REPLAY\n";
        assert!(matches!(
            ReplayLog::parse(text),
            Err(ReplayError::BadMagic(_))
        ));
    }

    #[test]
    fn parse_rejects_unsupported_version() {
        let text = "RFREPLAY 2\n";
        assert!(matches!(
            ReplayLog::parse(text),
            Err(ReplayError::UnsupportedVersion(_))
        ));
    }

    #[test]
    fn parse_rejects_unsupported_start_type() {
        let text = sample_log()
            .to_string()
            .replace("start_type=power-on", "start_type=savestate");
        assert!(matches!(
            ReplayLog::parse(&text),
            Err(ReplayError::UnsupportedStartType(_))
        ));
    }

    #[test]
    fn parse_rejects_malformed_input_line() {
        let text = sample_log().to_string().replace(
            "|A.......|........|\n",
            "|A.......|.......|\n", // one port group one char short
        );
        assert!(matches!(
            ReplayLog::parse(&text),
            Err(ReplayError::MalformedInputLine { .. })
        ));
    }

    #[test]
    fn parse_rejects_port_count_mismatch() {
        let text = sample_log()
            .to_string()
            .replace("|A.......|........|\n", "|A.......|\n");
        assert!(matches!(
            ReplayLog::parse(&text),
            Err(ReplayError::PortCountMismatch { .. })
        ));
    }

    #[test]
    fn verify_rom_sha256_ok_on_match() {
        let log = sample_log();
        assert!(log.verify_rom_sha256(&"a".repeat(64)).is_ok());
    }

    #[test]
    fn verify_rom_sha256_reports_mismatch() {
        let log = sample_log();
        let err = log.verify_rom_sha256(&"c".repeat(64)).unwrap_err();
        assert!(matches!(err, ReplayError::RomShaMismatch { .. }));
    }

    #[test]
    fn player_replays_frames_in_order_and_reports_exhaustion() {
        let log = sample_log();
        let mut player = ReplayPlayer::new(&log);
        let mut f0 = InputFrame::empty();
        f0.ports[0] = 1 << NesButton::A.bit();
        assert_eq!(player.next_frame(), Some(f0));
        assert_eq!(player.next_frame(), Some(InputFrame::empty()));
        assert_eq!(player.next_frame(), None);
    }

    #[test]
    fn player_reports_expected_hash_by_frame_number() {
        let log = sample_log();
        let player = ReplayPlayer::new(&log);
        assert_eq!(player.expected_hash(1), Some("b".repeat(64).as_str()));
        assert_eq!(player.expected_hash(0), None);
    }
}

#[cfg(test)]
mod snes_tests {
    use super::*;

    fn header(console: &str) -> ReplayHeader {
        ReplayHeader {
            console: console.to_string(),
            rom_sha256: "0".repeat(64),
            emu_version: "test".to_string(),
            core_config: "accuracy".to_string(),
            start_type: StartType::PowerOn,
            hash_kind: "reachable-v1".to_string(),
            hash_interval: 600,
        }
    }

    /// **The bug W7-02 exists to fix.** Every SNES button — including the
    /// eight above bit 7, which had no log-key entry at all before —
    /// survives a full record -> serialize -> parse -> replay round trip.
    #[test]
    fn every_snes_button_survives_a_round_trip() {
        let mut recorder = ReplayRecorder::new(header("snes"));
        for button in SnesButton::ALL {
            let mut frame = InputFrame::empty();
            frame.ports[0] = 1u16 << button.bit();
            recorder.record_frame(frame);
        }
        let log = recorder.finish();
        let parsed = ReplayLog::parse(&log.to_string()).expect("parses");
        let mut player = ReplayPlayer::new(&parsed);

        for button in SnesButton::ALL {
            let got = player.next_frame().expect("a frame per button").ports[0];
            assert_eq!(
                got,
                1u16 << button.bit(),
                "{button:?} (bit {}) did not survive the round trip",
                button.bit()
            );
        }
    }

    /// The specific case that broke RF-Scroller-S: Right is bit 8, which
    /// is outside the NES eight-bit range entirely.
    #[test]
    fn snes_right_at_bit_eight_survives() {
        let mut recorder = ReplayRecorder::new(header("snes"));
        let mut frame = InputFrame::empty();
        frame.ports[0] = 0x0100;
        recorder.record_frame(frame);
        let parsed = ReplayLog::parse(&recorder.finish().to_string()).expect("parses");
        assert_eq!(
            ReplayPlayer::new(&parsed)
                .next_frame()
                .expect("frame")
                .ports[0],
            0x0100,
            "a replay recorded holding Right must play back holding Right"
        );
    }

    /// **Backward compatibility.** An NES recorder must still produce the
    /// exact `[LogKey]` and `[Input]` bytes it always did.
    ///
    /// This is the risk in widening a documented wire format, so it is
    /// pinned literally rather than merely round-tripped: a round trip
    /// would pass even if both sides changed together.
    #[test]
    fn nes_logs_serialise_to_the_same_bytes_as_before() {
        let mut recorder = ReplayRecorder::new(header("nes"));
        let mut frame = InputFrame::empty();
        frame.ports[0] = (1 << NesButton::A.bit()) | (1 << NesButton::Right.bit());
        recorder.record_frame(frame);
        let text = recorder.finish().to_string();

        assert!(
            text.contains("P1:A,B,Select,Start,Up,Down,Left,Right\n"),
            "the NES log key must be unchanged:\n{text}"
        );
        assert!(
            text.contains("|A......R|........|\n"),
            "the NES input line must be unchanged:\n{text}"
        );
    }

    /// A console this crate does not know decodes as NES — which is what
    /// every existing file already does.
    #[test]
    fn an_unknown_console_falls_back_to_nes() {
        assert_eq!(Console::from_header("nes"), Console::Nes);
        assert_eq!(Console::from_header("SNES"), Console::Snes);
        assert_eq!(Console::from_header("gameboy"), Console::Nes);
        assert_eq!(Console::from_header(""), Console::Nes);
    }

    /// Names collide across consoles and mean DIFFERENT bits, so parsing
    /// must be console-directed.
    ///
    /// `Right` is bit 7 on NES and bit 8 on SNES. If parsing ever became
    /// name-only, one of these two assertions would break — which is
    /// exactly the silent mis-decode this test exists to prevent.
    #[test]
    fn the_same_button_name_means_a_different_bit_per_console() {
        assert_eq!(NesButton::from_name("Right").unwrap().bit(), 7);
        assert_eq!(SnesButton::from_name("Right").unwrap().bit(), 8);
        assert_eq!(NesButton::from_name("A").unwrap().bit(), 0);
        assert_eq!(SnesButton::from_name("A").unwrap().bit(), 7);
        // And an SNES-only name is not an NES button at all.
        assert!(NesButton::from_name("X").is_none());
        assert!(SnesButton::from_name("X").is_some());
    }

    /// An SNES log declaring SNES names must not be parsed as NES.
    #[test]
    fn a_snes_log_is_not_decoded_with_the_nes_table() {
        let mut recorder = ReplayRecorder::new(header("snes"));
        let mut frame = InputFrame::empty();
        frame.ports[0] = 1u16 << SnesButton::X.bit();
        recorder.record_frame(frame);
        let text = recorder.finish().to_string();
        assert!(text.contains("P1:B,Y,Select,Start,Up,Down,Left,Right,A,X,L,R\n"));
        // Re-parsing with the header intact keeps the SNES meaning.
        let parsed = ReplayLog::parse(&text).expect("parses");
        assert_eq!(
            ReplayPlayer::new(&parsed)
                .next_frame()
                .expect("frame")
                .ports[0],
            1u16 << SnesButton::X.bit()
        );
    }
}
