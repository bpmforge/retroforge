//! BizHawk `.bk2` (a zip), NES and SNES.
//!
//! `Header.txt` carries `Platform NES` or `Platform SNES`; `Input Log.txt`
//! carries a `LogKey:` line that names every column and then one `|`-framed
//! line per frame:
//!
//! ```text
//! LogKey:#Power|Reset|#P1 Up|P1 Down|...|P1 A|#P2 Up|...|
//! |.|.|UDLRsSBA|........|
//! ```
//!
//! `#` starts a group of columns; in an input line each group is one field
//! with one character per column, `.` released. Columns are matched by
//! **name**, not by position, so a different column order still maps right.

use super::{
    malformed, open_member, pressed, utf8, Console, Format, Movie, MovieError, MovieFrame, Reset,
    MAX_FRAMES,
};

/// Open a `.bk2` archive and parse it.
///
/// # Errors
/// See [`MovieError`].
pub fn parse_archive(archive: &[u8]) -> Result<Movie, MovieError> {
    let header = utf8(&open_member(archive, "Header.txt")?, "Header.txt")?;
    let log = utf8(&open_member(archive, "Input Log.txt")?, "Input Log.txt")?;
    parse(&header, &log)
}

/// Parse the two text members.
///
/// # Errors
/// [`MovieError::Unsupported`] for other platforms and savestate-start
/// movies; otherwise as [`MovieError`].
pub fn parse(header: &str, log: &str) -> Result<Movie, MovieError> {
    let mut platform = None;
    let mut rerecords = None;
    for line in header.lines() {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "Platform" => platform = Some(value.trim().to_ascii_uppercase()),
            "StartsFromSavestate" | "StartsFromSaveRam"
                if value.trim().eq_ignore_ascii_case("true") =>
            {
                return Err(MovieError::Unsupported(format!("{key} true")));
            }
            "rerecordCount" => rerecords = value.trim().parse().ok(),
            _ => {}
        }
    }
    let console = match platform.as_deref() {
        Some("NES") => Console::Nes,
        Some("SNES") => Console::Snes,
        other => return Err(MovieError::Unsupported(format!("platform {other:?}"))),
    };

    let mut key: Option<Vec<Vec<String>>> = None;
    let mut frames = Vec::new();
    for (n, line) in log.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if let Some(k) = line.strip_prefix("LogKey:") {
            key = Some(
                k.split('#')
                    .map(|g| {
                        g.split('|')
                            .filter(|c| !c.is_empty())
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .filter(|g| !g.is_empty())
                    .collect(),
            );
        } else if line.starts_with('|') {
            let key = key
                .as_ref()
                .ok_or_else(|| malformed(n + 1, "input before LogKey"))?;
            if frames.len() >= MAX_FRAMES {
                return Err(MovieError::TooLarge);
            }
            frames.push(frame(n + 1, line, key, console)?);
        }
    }
    if frames.is_empty() {
        return Err(MovieError::Empty);
    }
    Ok(Movie {
        format: Format::Bk2,
        console,
        frames,
        rerecords,
        rom_hint: None,
        dropped_subframes: 0,
    })
}

/// The bit a column name drives, if it is a pad button of `console`.
fn bit(console: Console, button: &str) -> Option<u32> {
    // NES layout: A = 0 .. Right = 7. SNES layout: B = 15 .. R = 4.
    match (console, button) {
        (Console::Nes, "A") => Some(0),
        (Console::Nes, "B") => Some(1),
        (Console::Nes, "Select") => Some(2),
        (Console::Nes, "Start") => Some(3),
        (Console::Nes, "Up") => Some(4),
        (Console::Nes, "Down") => Some(5),
        (Console::Nes, "Left") => Some(6),
        (Console::Nes, "Right") => Some(7),
        (Console::Snes, "B") => Some(15),
        (Console::Snes, "Y") => Some(14),
        (Console::Snes, "Select") => Some(13),
        (Console::Snes, "Start") => Some(12),
        (Console::Snes, "Up") => Some(11),
        (Console::Snes, "Down") => Some(10),
        (Console::Snes, "Left") => Some(9),
        (Console::Snes, "Right") => Some(8),
        (Console::Snes, "A") => Some(7),
        (Console::Snes, "X") => Some(6),
        (Console::Snes, "L") => Some(5),
        (Console::Snes, "R") => Some(4),
        _ => None,
    }
}

fn frame(
    n: usize,
    line: &str,
    key: &[Vec<String>],
    console: Console,
) -> Result<MovieFrame, MovieError> {
    // "|a|b|c|" -> ["a", "b", "c"]: drop the leading empty piece and the
    // trailing one after the last bar.
    let body = &line[1..];
    let body = body.strip_suffix('|').unwrap_or(body);
    let fields: Vec<&str> = body.split('|').collect();
    // Groups hold several columns but one field each: re-slice per group.
    let mut out = MovieFrame::default();
    let mut port = 0usize;
    let mut at = 0usize;
    for group in key {
        // Fields for a group: a single field of `group.len()` characters
        // (BizHawk) -- or `group.len()` one-character fields if the writer
        // put a bar between every column; accept both.
        let width: usize = fields.get(at).map_or(0, |f| f.chars().count());
        let (chars, used): (Vec<char>, usize) = if width == group.len() {
            (fields[at].chars().collect(), 1)
        } else if fields.len() >= at + group.len() {
            (
                fields[at..at + group.len()]
                    .iter()
                    .map(|f| f.chars().next().unwrap_or('.'))
                    .collect(),
                group.len(),
            )
        } else {
            return Err(malformed(n, "input line is shorter than the LogKey"));
        };
        at += used;
        let is_console = group.iter().any(|c| c == "Power" || c == "Reset");
        if is_console {
            for (name, &c) in group.iter().zip(&chars) {
                if pressed(c) {
                    match name.as_str() {
                        "Power" => out.reset = Some(Reset::Hard),
                        "Reset" if out.reset.is_none() => out.reset = Some(Reset::Soft),
                        _ => {}
                    }
                }
            }
            continue;
        }
        let Some(slot) = out.pads.get_mut(port) else {
            continue; // third and later pads are not replayed
        };
        port += 1;
        for (name, &c) in group.iter().zip(&chars) {
            let button = name.split_once(' ').map_or(name.as_str(), |(_, b)| b);
            if let (true, Some(b)) = (pressed(c), bit(console, button)) {
                *slot |= 1 << b;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NES_KEY: &str = "LogKey:#Power|Reset|#P1 Up|P1 Down|P1 Left|P1 Right|P1 Select|P1 Start|P1 B|P1 A|#P2 Up|P2 Down|P2 Left|P2 Right|P2 Select|P2 Start|P2 B|P2 A|";
    const SNES_KEY: &str = "LogKey:#Power|Reset|#P1 Up|P1 Down|P1 Left|P1 Right|P1 Select|P1 Start|P1 Y|P1 B|P1 X|P1 A|P1 L|P1 R|";

    #[test]
    fn nes_columns_map_by_name() {
        let log = format!("[Input]\n{NES_KEY}\n|..|........|........|\n|.r|U...sSBA|..L.....|\n|P.|...R....|........|\n[/Input]\n");
        let m = parse("Platform NES\nrerecordCount 3\n", &log).expect("parses");
        assert_eq!(m.console, Console::Nes);
        assert_eq!(m.frames.len(), 3);
        assert_eq!(m.rerecords, Some(3));
        assert_eq!(m.frames[1].reset, Some(Reset::Soft));
        assert_eq!(m.frames[1].pads, [0x10 | 0x04 | 0x08 | 0x02 | 0x01, 0x40]);
        assert_eq!(m.frames[2].reset, Some(Reset::Hard));
        assert_eq!(m.frames[2].pads[0], 0x80);
    }

    #[test]
    fn snes_columns_map_by_name_in_any_order() {
        let log = format!("{SNES_KEY}\n|..|U.....YBXALR|\n");
        let m = parse("Platform SNES\n", &log).expect("parses");
        assert_eq!(
            m.frames[0].pads[0],
            0x8000 | 0x4000 | 0x0800 | 0x80 | 0x40 | 0x20 | 0x10
        );
        // Same buttons, columns reordered in the key: still by name.
        let key = "LogKey:#Power|Reset|#P1 R|P1 B|";
        let m = parse("Platform SNES\n", &format!("{key}\n|..|.B|\n")).expect("parses");
        assert_eq!(m.frames[0].pads[0], 0x8000);
    }

    #[test]
    fn refuses_what_it_cannot_replay() {
        let log = format!("{NES_KEY}\n|..|........|........|\n");
        assert!(matches!(
            parse("Platform GBA\n", &log),
            Err(MovieError::Unsupported(_))
        ));
        assert!(matches!(
            parse("Platform NES\nStartsFromSavestate True\n", &log),
            Err(MovieError::Unsupported(_))
        ));
        assert!(matches!(parse("", &log), Err(MovieError::Unsupported(_))));
    }

    #[test]
    fn short_lines_and_missing_key_are_errors() {
        assert!(parse("Platform NES\n", "|..|........|........|\n").is_err());
        let log = format!("{NES_KEY}\n|..|........|\n");
        assert!(parse("Platform NES\n", &log).is_err());
        assert_eq!(
            parse("Platform NES\n", &format!("{NES_KEY}\n")),
            Err(MovieError::Empty)
        );
    }

    #[test]
    fn truncation_and_garbage_never_panic() {
        let log = format!("{NES_KEY}\n|..|U...sSBA|..L.....|\n");
        for cut in 0..=log.len() {
            if log.is_char_boundary(cut) {
                let _ = parse("Platform NES\n", &log[..cut]);
            }
        }
        for s in [
            "|",
            "||",
            "LogKey:\n|",
            "LogKey:#\n|a|",
            "LogKey:#a|b|\n|é|",
        ] {
            let _ = parse("Platform NES\n", s);
        }
    }
}
