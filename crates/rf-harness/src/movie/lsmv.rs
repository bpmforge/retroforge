//! lsnes `.lsmv` (a zip), SNES.
//!
//! The `input` member has one line per **poll**:
//!
//! ```text
//! F. 0 0|BYsSudlrAXLR|............
//! ```
//!
//! The text before the first `|` is the sync field: a leading `F` starts a
//! new frame (any other lead is an extra poll inside the current frame), the
//! next character is `R` when the console is reset on this frame, and the two
//! numbers are the reset's delay in cycles. Each controller is twelve
//! characters in the order `B Y s S u d l r A X L R`; `.` is released.
//!
//! The core latches pads once per frame, so the **first** poll of a frame is
//! used and later ones are dropped and counted in
//! [`Movie::dropped_subframes`](super::Movie::dropped_subframes). A reset with
//! a non-zero cycle delay cannot be reproduced at a frame boundary and is
//! [`MovieError::Unsupported`].

use super::{
    malformed, open_member, pressed, utf8, Console, Format, Movie, MovieError, MovieFrame, Reset,
    MAX_FRAMES,
};

/// Open a `.lsmv` archive and parse it.
///
/// # Errors
/// See [`MovieError`]; a movie for another system is
/// [`MovieError::Unsupported`].
pub fn parse_archive(archive: &[u8]) -> Result<Movie, MovieError> {
    if let Ok(gametype) = open_member(archive, "gametype") {
        let g = String::from_utf8_lossy(&gametype).to_ascii_lowercase();
        if !g.starts_with("snes") {
            return Err(MovieError::Unsupported(format!("gametype {}", g.trim())));
        }
    }
    let input = utf8(&open_member(archive, "input")?, "input")?;
    let mut movie = parse(&input)?;
    movie.rerecords = open_member(archive, "rerecords")
        .ok()
        .and_then(|b| String::from_utf8_lossy(&b).trim().parse().ok());
    movie.rom_hint = open_member(archive, "rom.sha256")
        .ok()
        .map(|b| String::from_utf8_lossy(&b).trim().to_string())
        .filter(|s| !s.is_empty());
    Ok(movie)
}

/// Parse the `input` member's text.
///
/// # Errors
/// See [`MovieError`].
pub fn parse(text: &str) -> Result<Movie, MovieError> {
    let mut frames: Vec<MovieFrame> = Vec::new();
    let mut dropped = 0usize;
    for (n, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let (sync, pads) = line
            .split_once('|')
            .ok_or_else(|| malformed(n + 1, "no controller field"))?;
        if !sync.starts_with('F') {
            dropped += 1;
            continue;
        }
        if frames.len() >= MAX_FRAMES {
            return Err(MovieError::TooLarge);
        }
        let mut s = sync.chars().skip(1);
        let reset = match s.next() {
            Some('R') => Some(Reset::Soft),
            _ => None,
        };
        if reset.is_some() {
            let delays: Vec<&str> = sync.split_whitespace().skip(1).collect();
            if delays.iter().any(|d| d.parse::<u32>() != Ok(0)) {
                return Err(MovieError::Unsupported("reset with a cycle delay".into()));
            }
        }
        let mut out = MovieFrame {
            pads: [0; 2],
            reset,
        };
        for (slot, field) in out.pads.iter_mut().zip(pads.split('|')) {
            *slot = controller(n + 1, field)?;
        }
        frames.push(out);
    }
    if frames.is_empty() {
        return Err(MovieError::Empty);
    }
    Ok(Movie {
        format: Format::Lsmv,
        console: Console::Snes,
        frames,
        rerecords: None,
        rom_hint: None,
        dropped_subframes: dropped,
    })
}

fn controller(n: usize, field: &str) -> Result<u16, MovieError> {
    let chars: Vec<char> = field.chars().collect();
    if chars.len() != 12 {
        return Err(malformed(n, "a SNES pad field is twelve characters"));
    }
    // "BYsSudlrAXLR": position i is SNES bit 15 - i.
    Ok(chars
        .iter()
        .enumerate()
        .filter(|(_, &c)| pressed(c))
        .fold(0u16, |w, (i, _)| w | (1 << (15 - i))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_buttons_resets_and_counts_subframes() {
        let text = "F. 0 0|............\nF. 0 0|B...u...A..R\nF. 0 0|.Y.S....AXL.|.........XL.\n  . 0 0|BYsSudlrAXLR\nFR 0 0|............\n";
        let m = parse(text).expect("parses");
        assert_eq!(m.frames.len(), 4);
        assert_eq!(m.dropped_subframes, 1);
        assert_eq!(m.frames[1].pads[0], 0x8000 | 0x0800 | 0x80 | 0x10);
        assert_eq!(m.frames[2].pads[0], 0x4000 | 0x1000 | 0x80 | 0x40 | 0x20);
        assert_eq!(m.frames[2].pads[1], 0x40 | 0x20);
        assert_eq!(m.frames[3].reset, Some(Reset::Soft));
    }

    #[test]
    fn refuses_a_delayed_reset_and_bad_fields() {
        assert!(matches!(
            parse("FR 0 5|............\n"),
            Err(MovieError::Unsupported(_))
        ));
        assert!(parse("F. 0 0|...\n").is_err());
        assert!(parse("F. 0 0\n").is_err());
        assert_eq!(parse(""), Err(MovieError::Empty));
        assert_eq!(parse("  . 0 0|............\n"), Err(MovieError::Empty));
    }

    #[test]
    fn truncation_and_garbage_never_panic() {
        let text = "F. 0 0|B...u...A..R|........Xl..\nFR 0 0|............\n";
        for cut in 0..=text.len() {
            let _ = parse(&text[..cut]);
        }
        for s in ["|", "F|", "FR|||", "F\u{e9}|\u{e9}", "\0"] {
            let _ = parse(s);
        }
    }
}
