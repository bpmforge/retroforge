//! FCEUX `.fm2` (text), NES.
//!
//! ```text
//! version 3
//! romChecksum base64:...
//! port0 1
//! |1|........|........||        <- |commands|port 0|port 1|expansion|
//! ```
//!
//! Header lines are `key value`. An input line starts with `|`; its first
//! field is the command bits (bit 0 soft reset, bit 1 power cycle; the other
//! bits are FDS disk commands, unsupported), then one field per port. A
//! gamepad port is eight characters in the order `RLDUTSBA`
//! (T = Start, S = Select); `.` or a space is released, any other character
//! is held. A port declared `0` in the header has an empty field.

use super::{
    malformed, pressed, Console, Format, Movie, MovieError, MovieFrame, Reset, MAX_FRAMES,
};

/// Parse an `.fm2` file.
///
/// # Errors
/// See [`MovieError`]; four-score, non-gamepad ports, PAL, FDS and the
/// binary input encoding are [`MovieError::Unsupported`].
pub fn parse(text: &str) -> Result<Movie, MovieError> {
    let mut ports = [1u8, 1u8];
    let mut rerecords = None;
    let mut rom_hint = None;
    let mut frames = Vec::new();
    // `lines()` consumes the input one line per step, so this terminates.
    for (n, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.starts_with('|') {
            if frames.len() >= MAX_FRAMES {
                return Err(MovieError::TooLarge);
            }
            frames.push(input_line(n + 1, line, ports)?);
            continue;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "fourscore" if value.trim() == "1" => {
                return Err(MovieError::Unsupported("four-score".into()));
            }
            "palFlag" | "FDS" | "binary" if value.trim() == "1" => {
                return Err(MovieError::Unsupported(format!("{key} 1")));
            }
            "port0" | "port1" => {
                let slot = usize::from(key == "port1");
                ports[slot] = match value.trim() {
                    "0" => 0,
                    "1" => 1,
                    other => {
                        return Err(MovieError::Unsupported(format!("{key} {other}")));
                    }
                };
            }
            "rerecordCount" => rerecords = value.trim().parse().ok(),
            "romChecksum" => rom_hint = Some(value.trim().to_string()),
            _ => {}
        }
    }
    if frames.is_empty() {
        return Err(MovieError::Empty);
    }
    Ok(Movie {
        format: Format::Fm2,
        console: Console::Nes,
        frames,
        rerecords,
        rom_hint,
        dropped_subframes: 0,
    })
}

fn input_line(n: usize, line: &str, ports: [u8; 2]) -> Result<MovieFrame, MovieError> {
    // "|c|p0|p1|exp|" splits into ["", c, p0, p1, exp, ...].
    let mut fields = line.split('|').skip(1);
    let cmd: u8 = fields
        .next()
        .and_then(|c| c.trim().parse().ok())
        .ok_or_else(|| malformed(n, "bad command field"))?;
    if cmd & !0b11 != 0 {
        return Err(MovieError::Unsupported("FDS disk command".into()));
    }
    let reset = match cmd & 0b11 {
        0 => None,
        // Power wins if both are set: it is the stronger reset.
        c if c & 0b10 != 0 => Some(Reset::Hard),
        _ => Some(Reset::Soft),
    };
    let mut pads = [0u16; 2];
    for (slot, kind) in pads.iter_mut().zip(ports) {
        let field = fields.next().ok_or_else(|| malformed(n, "missing port"))?;
        if kind == 0 {
            continue;
        }
        let chars: Vec<char> = field.chars().collect();
        if chars.len() != 8 {
            return Err(malformed(n, "a gamepad field is eight characters"));
        }
        // "RLDUTSBA": position i is NES bit 7 - i.
        *slot = chars
            .iter()
            .enumerate()
            .filter(|(_, &c)| pressed(c))
            .fold(0u16, |w, (i, _)| w | (1 << (7 - i)));
    }
    Ok(MovieFrame { pads, reset })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str =
        "version 3\nrerecordCount 7\nromChecksum base64:abc\nport0 1\nport1 1\nport2 0\n\
|1|........|........||\n|0|R.......|.......A||\n|0|..D.TS.A|........||\n|2|....T...|........||\n";

    #[test]
    fn parses_header_buttons_and_resets() {
        let m = parse(SAMPLE).expect("parses");
        assert_eq!(m.console, Console::Nes);
        assert_eq!(m.frames.len(), 4);
        assert_eq!(m.rerecords, Some(7));
        assert_eq!(m.rom_hint.as_deref(), Some("base64:abc"));
        assert_eq!(m.frames[0].reset, Some(Reset::Soft));
        assert_eq!(m.frames[1].pads, [0x80, 0x01]); // Right | (P2) A
        assert_eq!(m.frames[2].pads[0], 0x20 | 0x08 | 0x04 | 0x01); // Down Start Select A
        assert_eq!(m.frames[3].reset, Some(Reset::Hard));
        assert_eq!(m.frames[3].pads[0], 0x08);
    }

    #[test]
    fn a_port_declared_absent_has_an_empty_field() {
        let m = parse("port0 1\nport1 0\n|0|.......A||\n").expect("parses");
        assert_eq!(m.frames[0].pads, [0x01, 0]);
    }

    #[test]
    fn unsupported_kinds_are_named_not_guessed() {
        for h in ["fourscore 1", "port0 2", "palFlag 1", "FDS 1", "binary 1"] {
            let text = format!("{h}\n|0|........|........||\n");
            assert!(
                matches!(parse(&text), Err(MovieError::Unsupported(_))),
                "{h}"
            );
        }
        assert!(matches!(
            parse("|4|........|........||\n"),
            Err(MovieError::Unsupported(_))
        ));
    }

    #[test]
    fn bad_lines_and_empty_files_are_errors() {
        assert_eq!(parse(""), Err(MovieError::Empty));
        assert_eq!(parse("version 3\n"), Err(MovieError::Empty));
        assert!(parse("|x|........|........||\n").is_err());
        assert!(
            parse("|0|.......|........||\n").is_err(),
            "seven characters"
        );
        assert!(parse("|0|\n").is_err(), "missing ports");
    }

    #[test]
    fn truncation_at_every_byte_never_panics() {
        for cut in 0..=SAMPLE.len() {
            if SAMPLE.is_char_boundary(cut) {
                let _ = parse(&SAMPLE[..cut]);
            }
        }
    }

    #[test]
    fn garbage_never_panics() {
        for s in ["|||||", "\0\0\0", "|0|é|é|", "||||\n||||\n|", "\r\n\r\n"] {
            let _ = parse(s);
        }
    }
}
