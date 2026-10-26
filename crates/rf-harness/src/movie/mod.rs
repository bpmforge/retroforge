//! Input-movie import (ticket W37-00; `docs/design/REBUILD_LAB.md` section 9).
//!
//! Community "tool-assisted" input movies are used here as **input scripts
//! only**: they are parsed into per-frame pad words, replayed on a core with
//! the coverage recorder armed (see [`crate::movie_replay`]), and nothing from
//! them is stored beyond counts. A movie says nothing about a game's code; it
//! only drives the game through more of it than a hand-written script does.
//!
//! Four formats, one [`Movie`]:
//!
//! | format | module | console | shape |
//! |---|---|---|---|
//! | FCEUX `.fm2` | [`fm2`] | NES | text, `\|cmd\|RLDUTSBA\|...\|` |
//! | BizHawk `.bk2` | [`bk2`] | NES, SNES | zip of `Header.txt` + `Input Log.txt` |
//! | lsnes `.lsmv` | [`lsmv`] | SNES | zip with an `input` member |
//! | Snes9x 1.43 `.smv` | [`smv`] | SNES | binary, two bytes per pad per frame |
//!
//! ## Pad words
//!
//! A frame's pad words are in the **core's own layout**, so they can be put
//! straight into an [`rf_core_api::InputFrame`] (NES) or the SNES joypad port
//! (see `understand::run_snes`): NES bit 0 = A ... bit 7 = Right
//! ([nesdev: standard controller](https://www.nesdev.org/wiki/Standard_controller),
//! `rf_input::NesButton::bit`), SNES the `$4218/$4219` word, B = bit 15 ...
//! R = bit 4 (fullsnes "Controllers", `rf_input::SnesButton::bit`). Every
//! format above lists the buttons in an order that is one of these layouts
//! written backwards, so each parser derives a bit from the character's
//! position and a unit test pins the result against the other crate's table.
//!
//! ## Hostile input (law 8)
//!
//! A movie is a file someone else made. Every parser returns an error rather
//! than panicking; every walk is an iterator over the input (`lines()`,
//! `chunks_exact`) so it advances on every path; memory is bounded by the
//! bytes actually present (never by a count in a header) and by
//! [`MAX_FRAMES`]; archive members are read through `take`
//! ([`open_member`]), the same posture as `crates/retroforge/src/rom_open.rs`.

pub mod bk2;
pub mod fm2;
pub mod lsmv;
pub mod smv;

use std::fmt;
use std::io::Read as _;

/// Most frames one movie may hold. The longest movies in circulation are a
/// few hundred thousand frames; this is two orders of magnitude over that and
/// keeps a hostile file's allocation bounded (4M frames x 6 bytes = 24 MB).
pub const MAX_FRAMES: usize = 4_000_000;

/// Most bytes read from one archive member (law 8: the cap is on bytes read,
/// not on what the archive claims).
pub const MAX_MEMBER_BYTES: u64 = 64 * 1024 * 1024;

/// Which console the pad words are for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Console {
    Nes,
    Snes,
}

/// A console reset a movie asks for on one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reset {
    /// The console's reset button.
    Soft,
    /// A power cycle.
    Hard,
}

/// One frame of a movie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MovieFrame {
    /// Pad words for ports 0 and 1, in the core's layout (module doc).
    pub pads: [u16; 2],
    /// A reset requested before this frame runs.
    pub reset: Option<Reset>,
}

/// Which file format a [`Movie`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Fm2,
    Bk2,
    Lsmv,
    Smv,
}

/// A parsed movie: the inputs and just enough metadata to report on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Movie {
    pub format: Format,
    pub console: Console,
    pub frames: Vec<MovieFrame>,
    /// The author's re-record count, when the file says.
    pub rerecords: Option<u64>,
    /// The ROM identity the file claims (a checksum or hash string as
    /// written), for the report; never used to gate a replay.
    pub rom_hint: Option<String>,
    /// Input lines that were extra polls inside one frame and were dropped
    /// (lsnes sub-frames): the core latches input once per frame.
    pub dropped_subframes: usize,
}

/// Why a movie did not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MovieError {
    /// The file is not shaped like the format; `at` is a 1-based line number
    /// for text formats and a byte offset for binary ones.
    Malformed { at: usize, what: String },
    /// A valid movie of a kind this importer does not replay (savestate
    /// start, four-score, PAL, ...).
    Unsupported(String),
    /// The file ends before the data its header promises.
    Truncated,
    /// More than [`MAX_FRAMES`] frames, or an archive member over the cap.
    TooLarge,
    /// No input frames.
    Empty,
    /// A required archive member is missing or the archive is damaged.
    Archive(String),
}

impl fmt::Display for MovieError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MovieError::Malformed { at, what } => write!(f, "malformed movie at {at}: {what}"),
            MovieError::Unsupported(w) => write!(f, "unsupported movie: {w}"),
            MovieError::Truncated => write!(f, "movie ends early"),
            MovieError::TooLarge => write!(f, "movie too large"),
            MovieError::Empty => write!(f, "movie has no frames"),
            MovieError::Archive(w) => write!(f, "movie archive: {w}"),
        }
    }
}

impl std::error::Error for MovieError {}

pub(crate) fn malformed(at: usize, what: impl Into<String>) -> MovieError {
    MovieError::Malformed {
        at,
        what: what.into(),
    }
}

/// Read the archive member called `name` (case-insensitive, any directory
/// prefix ignored) as bytes, never more than [`MAX_MEMBER_BYTES`] of them.
///
/// # Errors
/// [`MovieError::Archive`] when the archive does not open or lacks the
/// member; [`MovieError::TooLarge`] when the member runs past the cap.
pub fn open_member(archive: &[u8], name: &str) -> Result<Vec<u8>, MovieError> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive))
        .map_err(|e| MovieError::Archive(e.to_string()))?;
    // Walk by index: `0..len()` advances unconditionally (law 8).
    for i in 0..zip.len() {
        let entry = zip
            .by_index(i)
            .map_err(|e| MovieError::Archive(e.to_string()))?;
        let leaf = entry.name().rsplit('/').next().unwrap_or("");
        if !entry.is_file() || !leaf.eq_ignore_ascii_case(name) {
            continue;
        }
        let mut out = Vec::new();
        // `take`, never `entry.size()`: the archive's claim is untrusted.
        entry
            .take(MAX_MEMBER_BYTES + 1)
            .read_to_end(&mut out)
            .map_err(|e| MovieError::Archive(e.to_string()))?;
        if out.len() as u64 > MAX_MEMBER_BYTES {
            return Err(MovieError::TooLarge);
        }
        return Ok(out);
    }
    Err(MovieError::Archive(format!("no member {name}")))
}

pub(crate) fn utf8(bytes: &[u8], what: &str) -> Result<String, MovieError> {
    String::from_utf8(bytes.to_vec()).map_err(|_| malformed(0, format!("{what} is not UTF-8")))
}

/// `true` when a text pad column is "pressed" in BizHawk/FCEUX/lsnes style:
/// anything but `.` or a space.
pub(crate) fn pressed(c: char) -> bool {
    c != '.' && c != ' '
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layouts the parsers emit are the ones `rf-input` defines.
    #[test]
    fn nes_and_snes_bit_layouts_match_rf_input() {
        use rf_input::{NesButton, SnesButton};
        // fm2 "RLDUTSBA": position i -> bit 7 - i.
        let nes = [
            NesButton::Right,
            NesButton::Left,
            NesButton::Down,
            NesButton::Up,
            NesButton::Start,
            NesButton::Select,
            NesButton::B,
            NesButton::A,
        ];
        for (i, b) in nes.iter().enumerate() {
            assert_eq!(u32::from(b.bit()), 7 - i as u32);
        }
        // lsnes "BYsSudlrAXLR": position i -> bit 15 - i.
        let snes = [
            SnesButton::B,
            SnesButton::Y,
            SnesButton::Select,
            SnesButton::Start,
            SnesButton::Up,
            SnesButton::Down,
            SnesButton::Left,
            SnesButton::Right,
            SnesButton::A,
            SnesButton::X,
            SnesButton::L,
            SnesButton::R,
        ];
        for (i, b) in snes.iter().enumerate() {
            assert_eq!(u32::from(b.bit()), 15 - i as u32);
        }
    }

    #[test]
    fn a_missing_or_broken_archive_is_an_error() {
        assert!(matches!(
            open_member(b"not a zip", "input"),
            Err(MovieError::Archive(_))
        ));
        assert!(matches!(
            open_member(&[], "input"),
            Err(MovieError::Archive(_))
        ));
    }

    fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut cursor);
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, contents) in entries {
                w.start_file(*name, opts).expect("start");
                std::io::Write::write_all(&mut w, contents).expect("write");
            }
            w.finish().expect("finish");
        }
        cursor.into_inner()
    }

    #[test]
    fn lsmv_and_bk2_archives_open_by_member_name() {
        let lsmv = zip_with(&[
            ("gametype", b"snes_ntsc"),
            ("input", b"F. 0 0|B...........\nF. 0 0|............\n"),
            ("rerecords", b"12\n"),
        ]);
        let m = lsmv::parse_archive(&lsmv).expect("parses");
        assert_eq!(m.frames.len(), 2);
        assert_eq!(m.rerecords, Some(12));
        assert_eq!(m.frames[0].pads[0], 0x8000);
        let other = zip_with(&[("gametype", b"gbc"), ("input", b"F. 0 0|............\n")]);
        assert!(matches!(
            lsmv::parse_archive(&other),
            Err(MovieError::Unsupported(_))
        ));
        assert!(matches!(
            lsmv::parse_archive(&zip_with(&[("gametype", b"snes_ntsc")])),
            Err(MovieError::Archive(_))
        ));

        let bk2 = zip_with(&[
            ("Header.txt", b"Platform NES\n"),
            (
                "Input Log.txt",
                b"LogKey:#Power|Reset|#P1 Up|P1 A|\n|..|.A|\n",
            ),
        ]);
        let m = bk2::parse_archive(&bk2).expect("parses");
        assert_eq!(m.console, Console::Nes);
        assert_eq!(m.frames[0].pads[0], 0x01);
    }

    #[test]
    fn an_oversized_member_is_cut_off_by_the_cap_not_trusted() {
        // Highly compressible, so the archive is tiny while the member is
        // one byte over the cap: it must be refused, having read only
        // cap + 1 bytes.
        let big = vec![b'x'; (MAX_MEMBER_BYTES + 1) as usize];
        let z = zip_with(&[("input", &big)]);
        assert!(z.len() < 1 << 20, "the fixture is small");
        assert_eq!(open_member(&z, "input"), Err(MovieError::TooLarge));
    }
}
