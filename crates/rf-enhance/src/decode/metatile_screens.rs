//! The `metatile_screens` decoder family, version 1 (ticket W5-02a;
//! `docs/design/GAME_PROFILES.md` §2, FR-ENH-005).
//!
//! A level stored as a grid of **metatiles** — small fixed-size blocks of
//! CHR tile ids — with the grid itself stored per column. §2's shape:
//!
//! ```toml
//! [decode]
//! kind = "metatile_screens"
//! metatile  = { table = 0x2200, size = 4 }
//! screens   = { width = 16, height = 15, order = "column_rle" }
//! collision = { table = 0x2600, bits = "solid,platform,hazard" }
//! ```
//!
//! ## What the family requires from `rom_map`
//!
//! `[decode]` says how the level is *shaped*; it does not say where the
//! level bytes are. Those come from two `[[rom_map]]` entries, found by
//! label:
//!
//! | label | what it must hold |
//! |---|---|
//! | `level_column_offset` | one **byte** per column: where that column's data starts in `level_rle_data` |
//! | `level_rle_data` | the packed run data itself |
//!
//! Looking them up by label rather than adding fields to `[decode]` is a
//! deliberate constraint of schema v0: `ScreensSpec` has `width`,
//! `height` and `order` and nothing else, and widening the schema is not
//! this ticket's to do. The cost is that the labels are part of the
//! family's contract, which is why they are documented here and why
//! [`DecodeError::MissingRomMapEntry`] names them instead of failing
//! generically.
//!
//! ## `column_rle`
//!
//! Per `fixtures/nes/rf-scroller/FORMAT.md`'s "Column-RLE encoding":
//! each column is a tightly packed sequence of `(run, metatile_id)` byte
//! pairs; column `c` starts at `level_column_offset[c]` and ends where
//! column `c+1` starts (or at the end of the data, for the last column);
//! rows are emitted top-to-bottom; **every column's runs sum to exactly
//! `height`.**
//!
//! ### This decoder enforces the invariant the ROM's own reader trusts
//!
//! FORMAT.md is explicit that the fixture's runtime reader does not check
//! it — "`decode_column()` in `main.c` has no bounds/sum check and would
//! silently mis-decode a malformed table. A shipped `metatile_screens`
//! decoder should decide for itself whether to add that defense."
//!
//! This one adds it, and refuses rather than truncating or padding. The
//! reasoning is the difference between the two readers: the ROM's runs
//! against data its own build produced, while this runs against whatever
//! a **profile author** pointed it at, and the overwhelmingly likely
//! cause of a column that does not sum to `height` is a wrong table
//! address in a profile someone is still writing. Silently padding that
//! to `height` would hand them a level view that is subtly wrong
//! everywhere and looks fine — the single worst outcome for a tool whose
//! job is showing people the truth about a ROM.
//!
//! Family version **1**; see [`super`]'s "Family versioning" section
//! before changing any of the above.

use rf_profiles::schema::Profile;

use super::{slice_at, DecodeError};

const FAMILY: &str = "metatile_screens";

/// `rom_map` label for the per-column cursor table.
pub const LABEL_COLUMN_OFFSETS: &str = "level_column_offset";
/// `rom_map` label for the packed run data.
pub const LABEL_LEVEL_DATA: &str = "level_rle_data";

/// Everything the decoder needs, resolved from a profile but usable
/// standalone — which is what keeps [`decode`] a pure function of bytes
/// plus numbers, with no `Profile` in its signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    /// Grid width in metatiles.
    pub width: u32,
    /// Grid height in metatiles.
    pub height: u32,
    /// Offset of the metatile definition table (normalized image).
    pub metatile_table: u32,
    /// CHR tile ids per metatile (4 for a 2x2).
    pub metatile_size: u32,
    /// Offset of the per-column cursor table, and how many entries.
    pub column_offsets: u32,
    /// Offset of the packed run data, and its length in bytes.
    pub level_data: u32,
    pub level_data_len: u32,
    /// Offset of the per-metatile collision attribute table, if the
    /// profile declares one.
    pub collision_table: Option<u32>,
}

/// A decoded level. Plain data, no lifetimes — the caller usually keeps
/// it for the whole session while the ROM slice it came from does not
/// outlive load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedLevel {
    pub width: u32,
    pub height: u32,
    /// `width * height` metatile ids, **column-major**: index
    /// `col * height + row`. Column-major because the source encoding is
    /// per column and every consumer so far (a side-scroller's level view
    /// and its camera) walks columns; storing it row-major here would
    /// mean transposing twice for no reader's benefit.
    pub metatiles: Vec<u8>,
    /// `metatile_size` CHR tile ids per metatile id, in table order —
    /// what turns a metatile grid into something drawable.
    pub metatile_tiles: Vec<u8>,
    /// How many metatile definitions the table held.
    pub metatile_count: u32,
    /// One collision attribute byte per metatile id, when the profile
    /// declared a collision table.
    pub collision: Option<Vec<u8>>,
}

impl DecodedLevel {
    /// Metatile id at `(col, row)`, or `None` when out of bounds.
    #[must_use]
    pub fn at(&self, col: u32, row: u32) -> Option<u8> {
        if col >= self.width || row >= self.height {
            return None;
        }
        self.metatiles
            .get((col * self.height + row) as usize)
            .copied()
    }

    /// One column, top to bottom.
    #[must_use]
    pub fn column(&self, col: u32) -> Option<&[u8]> {
        if col >= self.width {
            return None;
        }
        let start = (col * self.height) as usize;
        self.metatiles.get(start..start + self.height as usize)
    }

    /// The CHR tile ids making up metatile `id`.
    #[must_use]
    pub fn tiles_for(&self, id: u8) -> Option<&[u8]> {
        if u32::from(id) >= self.metatile_count {
            return None;
        }
        let size = (self.metatile_tiles.len() / self.metatile_count as usize).max(1);
        let start = usize::from(id) * size;
        self.metatile_tiles.get(start..start + size)
    }
}

/// Resolve a [`Spec`] from a loaded profile.
///
/// # Errors
/// Names the missing `[decode]` field or `[[rom_map]]` label rather than
/// failing generically — see [`DecodeError`].
pub fn spec_from_profile(profile: &Profile) -> Result<Spec, DecodeError> {
    let decode = profile
        .decode
        .as_ref()
        .ok_or(DecodeError::NoDecodeSection)?;
    if decode.kind != FAMILY {
        return Err(DecodeError::UnknownFamily(decode.kind.clone()));
    }
    let screens = decode.screens.as_ref().ok_or(DecodeError::MissingSpec {
        family: FAMILY,
        field: "screens",
    })?;
    if screens.order != "column_rle" {
        return Err(DecodeError::UnsupportedOrder(screens.order.clone()));
    }
    let metatile = decode.metatile.as_ref().ok_or(DecodeError::MissingSpec {
        family: FAMILY,
        field: "metatile",
    })?;

    let find =
        |label: &'static str| {
            profile.rom_map.iter().find(|e| e.label == label).ok_or(
                DecodeError::MissingRomMapEntry {
                    family: FAMILY,
                    label,
                },
            )
        };
    let columns = find(LABEL_COLUMN_OFFSETS)?;
    let data = find(LABEL_LEVEL_DATA)?;

    Ok(Spec {
        width: screens.width,
        height: screens.height,
        metatile_table: metatile.table,
        metatile_size: metatile.size,
        column_offsets: columns.offset,
        level_data: data.offset,
        level_data_len: data.len,
        collision_table: decode.collision.as_ref().map(|c| c.table),
    })
}

/// Decode a level out of `rom`.
///
/// `rom` is the **normalized** (header-stripped) image — see [`super`]'s
/// module doc for why that matters and what happens if it is not.
///
/// # Errors
/// Every table is bounds-checked against `rom`, and every column is
/// checked against the format's sum-to-`height` invariant. See this
/// module's doc for why a violation is refused rather than repaired.
pub fn decode(rom: &[u8], spec: &Spec) -> Result<DecodedLevel, DecodeError> {
    if spec.width == 0 || spec.height == 0 {
        return Err(DecodeError::MalformedLevel(format!(
            "a {}x{} grid has no cells",
            spec.width, spec.height
        )));
    }

    let offsets = slice_at(rom, spec.column_offsets, spec.width, "level_column_offset")?;
    let data = slice_at(rom, spec.level_data, spec.level_data_len, "level_rle_data")?;

    let mut metatiles = Vec::with_capacity((spec.width * spec.height) as usize);
    for col in 0..spec.width as usize {
        // A column ends where the next begins; the last ends at the end
        // of the data. One-byte offsets, not pointers — the fixture's own
        // header records that the data was kept under 256 bytes precisely
        // so these fit in a `u8`.
        let start = usize::from(offsets[col]);
        let end = if col + 1 < offsets.len() {
            usize::from(offsets[col + 1])
        } else {
            data.len()
        };
        if start > end || end > data.len() {
            return Err(DecodeError::MalformedLevel(format!(
                "column {col} spans bytes {start}..{end} of a {}-byte level_rle_data",
                data.len()
            )));
        }

        let mut emitted = 0u32;
        let pairs = &data[start..end];
        for pair in pairs.chunks(2) {
            if emitted >= spec.height {
                break;
            }
            let [run, id] = *pair else {
                return Err(DecodeError::MalformedLevel(format!(
                    "column {col} ends on a half pair — run-length encoding is (run, id) byte \
                     pairs, so an odd byte count means the offsets are wrong"
                )));
            };
            if run == 0 {
                return Err(DecodeError::MalformedLevel(format!(
                    "column {col} has a zero-length run, which can never terminate"
                )));
            }
            let take = u32::from(run).min(spec.height - emitted);
            metatiles.extend(std::iter::repeat_n(id, take as usize));
            emitted += take;
        }

        if emitted != spec.height {
            return Err(DecodeError::MalformedLevel(format!(
                "column {col} decoded {emitted} rows, not {}. The format's invariant is that \
                 every column's runs sum to exactly the grid height (FORMAT.md, 'Column-RLE \
                 encoding'); the usual cause is a wrong table offset in the profile",
                spec.height
            )));
        }
    }

    // Metatile definitions. `size` tile ids each; the table's length is
    // taken from what the profile's rom_map said, so `metatile_count`
    // is derived rather than assumed to be 256.
    let table_len = metatile_table_len(rom, spec)?;
    let metatile_tiles = slice_at(rom, spec.metatile_table, table_len, "metatile_table")?.to_vec();
    let metatile_count = table_len / spec.metatile_size.max(1);

    let collision = match spec.collision_table {
        Some(table) => Some(slice_at(rom, table, metatile_count, "collision_table")?.to_vec()),
        None => None,
    };

    Ok(DecodedLevel {
        width: spec.width,
        height: spec.height,
        metatiles,
        metatile_tiles,
        metatile_count,
        collision,
    })
}

/// How many bytes the metatile table occupies.
///
/// The largest metatile id the level actually uses decides it, which is
/// stricter than trusting a `[[rom_map]]` `len` and works for profiles
/// whose `rom_map` describes the table loosely. It also means a level
/// referencing a metatile the table does not define is caught here as an
/// out-of-range read rather than silently drawing the bytes that follow
/// the table.
fn metatile_table_len(rom: &[u8], spec: &Spec) -> Result<u32, DecodeError> {
    let offsets = slice_at(rom, spec.column_offsets, spec.width, "level_column_offset")?;
    let data = slice_at(rom, spec.level_data, spec.level_data_len, "level_rle_data")?;
    let mut max_id = 0u32;
    for col in 0..offsets.len() {
        let start = usize::from(offsets[col]);
        let end = if col + 1 < offsets.len() {
            usize::from(offsets[col + 1])
        } else {
            data.len()
        };
        if start > end || end > data.len() {
            continue; // `decode` reports this properly; do not double-report.
        }
        for pair in data[start..end].chunks(2) {
            if let [_, id] = *pair {
                max_id = max_id.max(u32::from(id));
            }
        }
    }
    Ok((max_id + 1) * spec.metatile_size.max(1))
}
