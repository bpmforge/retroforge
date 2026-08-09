//! DataCrystal TSV import (GAME_PROFILES.md §3 step 1, CONSTRAINTS §2's
//! **facts-only transcription policy**, ticket brief: "DataCrystal is
//! GFDL 1.2 ... a TSV import must ingest FACTS — address, size, value, a
//! short fresh label — and must not slurp and store a whole wiki table's
//! prose").
//!
//! ## The structural enforcement (not a comment nobody reads)
//!
//! DataCrystal's real RAM/ROM map wiki tables
//! ([datacrystal.tcrf.net/wiki/Metroid/RAM_map](https://datacrystal.tcrf.net/wiki/Metroid/RAM_map))
//! are `Address | Size | Description` (sometimes more columns) — that
//! `Description` column is exactly the free prose CONSTRAINTS §2 says must
//! be "written fresh", never transcribed. This parser's column schema has
//! **no slot for it at all**: the five columns it accepts
//! (`address`/`size`/`type`/`label`/`source`) map onto
//! [`crate::annotation::Annotation`]'s FACT fields only — `addr`, `len`,
//! `ty`, `label`, `source` — never `notes`. A TSV row transcribed from a
//! DataCrystal table by pasting its `Description` cell into a sixth column
//! is rejected outright ([`ImportError::WrongColumnCount`]), not silently
//! dropped into some free-text field — there is no free-text field for it
//! to land in. [`crate::annotation::Annotation::notes`] stays `None` on
//! every row this module produces; a human adds fresh notes afterward, in
//! the debugger UI, if they want any (`crate::annotation`'s own doc).
//!
//! `label` is additionally capped in length and word count
//! ([`MAX_LABEL_LEN`]/[`MAX_LABEL_WORDS`]) — a second, independent guard
//! against the same failure mode: a `label` cell that IS a full sentence
//! (someone pasting a `Description` cell into the `label` column instead
//! of a sixth one) is rejected too, not merely the six-column case.
//!
//! Pure data, like every other module in this crate: no `egui`, no
//! `rf-nes`, headlessly testable.

use std::fmt;

use crate::annotation::{AddressSpace, Annotation};

/// Required literal header row — see module doc for why the column set is
/// exactly this and no more.
const EXPECTED_HEADER: &str = "address\tsize\ttype\tlabel\tsource";
const EXPECTED_COLUMNS: usize = 5;
/// A DataCrystal RAM/ROM map `label` cell is a short identifier
/// (`player_health`, `screen_scroll_x`, ...) in every real table this
/// project's research doc cites — not a sentence. 48 bytes comfortably
/// fits any such identifier while still catching a pasted description.
const MAX_LABEL_LEN: usize = 48;
/// A second, wording-based guard alongside the length cap (module doc) —
/// short identifiers are essentially never more than a couple of
/// underscore/space-separated words.
const MAX_LABEL_WORDS: usize = 6;

/// Everything [`parse_tsv`] can reject. Every variant names the 1-based
/// source-text line it came from (vacuity trap (b): "a DataCrystal import
/// test with one well-formed row cannot detect a parser that accepts
/// anything — include malformed rows and assert they are rejected with the
/// offending line named").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// The input had no header line at all.
    EmptyInput,
    /// Line 1 was not exactly [`EXPECTED_HEADER`].
    UnexpectedHeader {
        found: String,
    },
    /// A data row did not have exactly [`EXPECTED_COLUMNS`] tab-separated
    /// fields — module doc: this is the structural facts-only guard, not
    /// an incidental arity check. A 6-column row (DataCrystal's own
    /// `Address | Size | Type | Label | Source | Description` shape,
    /// transcribed with its prose column intact) is rejected here.
    WrongColumnCount {
        line: usize,
        expected: usize,
        found: usize,
    },
    InvalidAddress {
        line: usize,
        value: String,
    },
    InvalidSize {
        line: usize,
        value: String,
    },
    EmptyField {
        line: usize,
        field: &'static str,
    },
    /// `label` exceeded [`MAX_LABEL_LEN`] bytes — module doc's second
    /// facts-only guard.
    LabelTooLong {
        line: usize,
        len: usize,
    },
    /// `label` had more than [`MAX_LABEL_WORDS`] whitespace-separated
    /// words — reads as a transcribed sentence, not a short fresh label.
    LabelLooksLikeProse {
        line: usize,
    },
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::EmptyInput => write!(f, "empty input: expected a header line"),
            ImportError::UnexpectedHeader { found } => write!(
                f,
                "line 1: expected header `{EXPECTED_HEADER}`, found `{found}`"
            ),
            ImportError::WrongColumnCount {
                line,
                expected,
                found,
            } => write!(
                f,
                "line {line}: expected {expected} tab-separated columns \
                 (address/size/type/label/source — no free-text `notes`/`description` \
                 column is accepted, CONSTRAINTS §2), found {found}"
            ),
            ImportError::InvalidAddress { line, value } => {
                write!(f, "line {line}: invalid address `{value}`")
            }
            ImportError::InvalidSize { line, value } => {
                write!(f, "line {line}: invalid size `{value}`")
            }
            ImportError::EmptyField { line, field } => {
                write!(f, "line {line}: empty `{field}` field")
            }
            ImportError::LabelTooLong { line, len } => write!(
                f,
                "line {line}: label is {len} bytes, longer than the {MAX_LABEL_LEN}-byte cap \
                 (looks like transcribed prose, CONSTRAINTS §2 facts-only policy)"
            ),
            ImportError::LabelLooksLikeProse { line } => write!(
                f,
                "line {line}: label has more than {MAX_LABEL_WORDS} words \
                 (looks like transcribed prose, CONSTRAINTS §2 facts-only policy)"
            ),
        }
    }
}

impl std::error::Error for ImportError {}

/// Parse `text` (a DataCrystal RAM-map or ROM-map TSV transcription, one
/// row per fact) into [`Annotation`]s, all tagged with the given `space`
/// (a single import call handles one map — RAM or ROM — never a mix,
/// matching DataCrystal's own "RAM map" vs. "ROM map" page split).
///
/// Fails on the first invalid line (module doc's structural guard is only
/// meaningful if a bad row stops the import rather than being silently
/// skipped alongside good ones).
///
/// # Errors
/// See [`ImportError`]'s variants.
pub fn parse_tsv(text: &str, space: AddressSpace) -> Result<Vec<Annotation>, ImportError> {
    let mut lines = text.lines().enumerate();
    let (_, header) = lines.next().ok_or(ImportError::EmptyInput)?;
    if header.trim_end_matches('\r') != EXPECTED_HEADER {
        return Err(ImportError::UnexpectedHeader {
            found: header.to_string(),
        });
    }

    let mut out = Vec::new();
    for (idx, raw_line) in lines {
        let line_no = idx + 1; // 1-based, header already consumed as line 1
        let line = raw_line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue; // blank line between rows: allowed, not a fact
        }
        out.push(parse_row(line, line_no, space)?);
    }
    Ok(out)
}

fn parse_row(line: &str, line_no: usize, space: AddressSpace) -> Result<Annotation, ImportError> {
    let fields: Vec<&str> = line.split('\t').collect();
    if fields.len() != EXPECTED_COLUMNS {
        return Err(ImportError::WrongColumnCount {
            line: line_no,
            expected: EXPECTED_COLUMNS,
            found: fields.len(),
        });
    }
    let (addr_s, size_s, ty, label, source) =
        (fields[0], fields[1], fields[2], fields[3], fields[4]);

    let addr = parse_address(addr_s).ok_or_else(|| ImportError::InvalidAddress {
        line: line_no,
        value: addr_s.to_string(),
    })?;
    let len = parse_address(size_s).ok_or_else(|| ImportError::InvalidSize {
        line: line_no,
        value: size_s.to_string(),
    })?;

    require_nonempty(ty, "type", line_no)?;
    require_nonempty(label, "label", line_no)?;
    require_nonempty(source, "source", line_no)?;

    if label.len() > MAX_LABEL_LEN {
        return Err(ImportError::LabelTooLong {
            line: line_no,
            len: label.len(),
        });
    }
    if label.split_whitespace().count() > MAX_LABEL_WORDS {
        return Err(ImportError::LabelLooksLikeProse { line: line_no });
    }

    Ok(Annotation {
        space,
        addr,
        len,
        ty: ty.to_string(),
        label: label.to_string(),
        // Deliberately `None` on every row this function produces — see
        // module doc's "structural enforcement" section.
        notes: None,
        source: source.to_string(),
        count: None,
    })
}

fn require_nonempty(value: &str, field: &'static str, line: usize) -> Result<(), ImportError> {
    if value.trim().is_empty() {
        Err(ImportError::EmptyField { line, field })
    } else {
        Ok(())
    }
}

/// Accepts `$6029`/`0x6029`/`0X6029` (hex, the 6502/DataCrystal
/// convention) or a bare decimal string. Empty input is not a valid
/// address either way.
fn parse_address(raw: &str) -> Option<u32> {
    let raw = raw.trim();
    if let Some(hex) = raw.strip_prefix('$') {
        return u32::from_str_radix(hex, 16).ok();
    }
    if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        return u32::from_str_radix(hex, 16).ok();
    }
    if raw.is_empty() {
        return None;
    }
    raw.parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vacuity trap (b), well-formed half: one row parses to exactly the
    /// facts the row named, `notes` stays `None`.
    #[test]
    fn parses_a_well_formed_row() {
        let text = "address\tsize\ttype\tlabel\tsource\n\
                     0x6029\t2\tu16\tplayer_x\thttps://datacrystal.tcrf.net/wiki/Game/RAM_map";
        let rows = parse_tsv(text, AddressSpace::Ram).expect("well-formed TSV must parse");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].addr, 0x6029);
        assert_eq!(rows[0].len, 2);
        assert_eq!(rows[0].ty, "u16");
        assert_eq!(rows[0].label, "player_x");
        assert_eq!(
            rows[0].source,
            "https://datacrystal.tcrf.net/wiki/Game/RAM_map"
        );
        assert_eq!(rows[0].notes, None, "importer must never populate notes");
        assert_eq!(rows[0].space, AddressSpace::Ram);
    }

    #[test]
    fn parses_multiple_rows_in_order_and_skips_blank_lines() {
        let text = "address\tsize\ttype\tlabel\tsource\n\
                     0x0010\t1\tu8\tflags\tsrc-a\n\
                     \n\
                     0x0020\t2\tu16\thealth\tsrc-b\n";
        let rows = parse_tsv(text, AddressSpace::Ram).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].addr, 0x0010);
        assert_eq!(rows[1].addr, 0x0020);
    }

    #[test]
    fn accepts_decimal_addresses_too() {
        let text = "address\tsize\ttype\tlabel\tsource\n24617\t2\tu16\tplayer_x\tsrc";
        let rows = parse_tsv(text, AddressSpace::Ram).unwrap();
        assert_eq!(rows[0].addr, 24617);
    }

    #[test]
    fn tags_every_row_with_the_caller_supplied_space() {
        let text = "address\tsize\ttype\tlabel\tsource\n0x0000\t4\tptr_table\tmt_table\tsrc";
        let rows = parse_tsv(text, AddressSpace::Rom).unwrap();
        assert_eq!(rows[0].space, AddressSpace::Rom);
    }

    #[test]
    fn empty_input_is_rejected() {
        assert_eq!(
            parse_tsv("", AddressSpace::Ram),
            Err(ImportError::EmptyInput)
        );
    }

    #[test]
    fn wrong_header_is_rejected() {
        let text = "addr\tsize\ttype\tlabel\tsource\n0x0000\t1\tu8\tx\ty";
        let err = parse_tsv(text, AddressSpace::Ram).expect_err("must reject");
        assert!(matches!(err, ImportError::UnexpectedHeader { .. }));
    }

    /// **Vacuity trap (b) — the licensing-structural test.** A row
    /// transcribed straight from a real DataCrystal table keeps that
    /// table's `Description` column intact (six fields, not five) — this
    /// is exactly what CONSTRAINTS §2 forbids ("never transcribe a full
    /// curated table verbatim"), and the parser must reject it BECAUSE the
    /// sixth column has nowhere to go, not as an incidental arity
    /// mismatch. Mutation: loosen `WrongColumnCount`'s check to
    /// `fields.len() < EXPECTED_COLUMNS` (accept extra columns) and this
    /// fails.
    #[test]
    fn rejects_a_row_still_carrying_a_datacrystal_description_column() {
        let text = "address\tsize\ttype\tlabel\tsource\n\
                     0x6029\t2\tu16\tplayer_x\tsrc\tByte value equals the player's current world X position in pixels, ranges from 0 to 752 and wraps at the far edge of the level";
        let err = parse_tsv(text, AddressSpace::Ram).expect_err("6-column row must be rejected");
        assert_eq!(
            err,
            ImportError::WrongColumnCount {
                line: 2,
                expected: 5,
                found: 6,
            }
        );
    }

    #[test]
    fn rejects_a_row_with_too_few_columns() {
        let text = "address\tsize\ttype\tlabel\tsource\n0x6029\t2\tu16";
        let err = parse_tsv(text, AddressSpace::Ram).expect_err("must reject");
        assert_eq!(
            err,
            ImportError::WrongColumnCount {
                line: 2,
                expected: 5,
                found: 3,
            }
        );
    }

    #[test]
    fn rejects_an_unparseable_address_and_names_the_line() {
        let text = "address\tsize\ttype\tlabel\tsource\n\
                     0x0010\t1\tu8\tok\tsrc\n\
                     not-an-address\t1\tu8\tbad\tsrc";
        let err = parse_tsv(text, AddressSpace::Ram).expect_err("must reject");
        assert_eq!(
            err,
            ImportError::InvalidAddress {
                line: 3,
                value: "not-an-address".to_string(),
            }
        );
    }

    #[test]
    fn rejects_an_unparseable_size_and_names_the_line() {
        let text = "address\tsize\ttype\tlabel\tsource\n0x0010\tbig\tu8\tok\tsrc";
        let err = parse_tsv(text, AddressSpace::Ram).expect_err("must reject");
        assert_eq!(
            err,
            ImportError::InvalidSize {
                line: 2,
                value: "big".to_string(),
            }
        );
    }

    #[test]
    fn rejects_a_row_with_an_empty_source_and_names_the_line() {
        let text = "address\tsize\ttype\tlabel\tsource\n0x0010\t1\tu8\tlbl\t";
        let err = parse_tsv(text, AddressSpace::Ram).expect_err("must reject");
        assert_eq!(
            err,
            ImportError::EmptyField {
                line: 2,
                field: "source",
            }
        );
    }

    #[test]
    fn rejects_a_row_with_an_empty_label_and_names_the_line() {
        let text = "address\tsize\ttype\tlabel\tsource\n0x0010\t1\tu8\t\tsrc";
        let err = parse_tsv(text, AddressSpace::Ram).expect_err("must reject");
        assert_eq!(
            err,
            ImportError::EmptyField {
                line: 2,
                field: "label",
            }
        );
    }

    #[test]
    fn rejects_a_label_that_is_too_long() {
        let long_label = "x".repeat(MAX_LABEL_LEN + 1);
        let text = format!("address\tsize\ttype\tlabel\tsource\n0x0010\t1\tu8\t{long_label}\tsrc");
        let err = parse_tsv(&text, AddressSpace::Ram).expect_err("must reject");
        assert_eq!(
            err,
            ImportError::LabelTooLong {
                line: 2,
                len: MAX_LABEL_LEN + 1,
            }
        );
    }

    /// A prose-shaped label under the byte cap must still be rejected on
    /// word count alone — proves the two guards are independent, not one
    /// subsuming the other.
    #[test]
    fn rejects_a_short_but_multi_sentence_label_on_word_count() {
        let text = "address\tsize\ttype\tlabel\tsource\n\
                     0x0010\t1\tu8\tis the current hp of player one\tsrc";
        let err = parse_tsv(text, AddressSpace::Ram).expect_err("must reject");
        assert_eq!(err, ImportError::LabelLooksLikeProse { line: 2 });
    }

    #[test]
    fn accepts_a_label_at_exactly_the_word_and_length_caps() {
        // Exactly MAX_LABEL_WORDS words, well under MAX_LABEL_LEN bytes.
        let text = "address\tsize\ttype\tlabel\tsource\n0x0010\t1\tu8\ta b c d e f\tsrc";
        let rows = parse_tsv(text, AddressSpace::Ram).expect("boundary must be accepted");
        assert_eq!(rows[0].label, "a b c d e f");
    }

    /// **The GAME_PROFILES.md §3 step-1-to-step-2 pipeline, end to end.**
    /// Every other test here proves `parse_tsv` in isolation; this one
    /// proves its OUTPUT actually feeds `crate::profile_export::
    /// export_skeleton` and comes out the other side through the real
    /// `rf_profiles` loader with sources intact. This is the one seam
    /// `crate::profile_export`'s own module doc calls out by name ("any
    /// future caller that builds a `&[Annotation]` some other way, e.g.
    /// straight from `crate::datacrystal::parse_tsv`") and that seam had
    /// no test until this one: `parse_tsv`'s rows never pass through
    /// `AnnotationStore::add`, so this is also the first proof that a
    /// TSV-imported row — required-non-empty `source`/`label` by
    /// [`parse_tsv`]'s own field checks, but never store-validated — still
    /// satisfies the exporter's independent refusal check rather than
    /// merely happening to.
    #[test]
    fn a_parsed_tsv_feeds_the_exporter_and_survives_the_real_validator() {
        let ram_tsv = "address\tsize\ttype\tlabel\tsource\n\
                        0x6029\t2\tu16\tplayer_x\thttps://datacrystal.tcrf.net/wiki/Game/RAM_map";
        let rom_tsv = "address\tsize\ttype\tlabel\tsource\n\
                        0x0000\t4\tptr_table\tmt_table\thttps://datacrystal.tcrf.net/wiki/Game/ROM_map";

        let mut entries = parse_tsv(ram_tsv, AddressSpace::Ram).expect("RAM TSV parses");
        entries.extend(parse_tsv(rom_tsv, AddressSpace::Rom).expect("ROM TSV parses"));

        let meta = crate::profile_export::ExportMeta {
            title: "TSV pipeline test".to_string(),
            console: crate::profile_export::Console::Nes,
            region: "ntsc".to_string(),
            authors: vec![],
        };
        let text = crate::profile_export::export_skeleton(&entries, &meta)
            .expect("TSV-parsed annotations are already sourced and labelled");

        let outcome = rf_profiles::load_str(&text).expect("must pass the real validator");
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
        assert_eq!(
            outcome.profile.memory_map[0].source.as_deref(),
            Some("https://datacrystal.tcrf.net/wiki/Game/RAM_map")
        );
        assert_eq!(outcome.profile.memory_map[0].addr, 0x6029);
        assert_eq!(
            outcome.profile.rom_map[0].source.as_deref(),
            Some("https://datacrystal.tcrf.net/wiki/Game/ROM_map")
        );
        assert_eq!(outcome.profile.rom_map[0].offset, 0x0000);
    }
}
