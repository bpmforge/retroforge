//! The `room_grid` decoder family (ticket W9-08; FR-ENH-005,
//! `docs/design/GAME_PROFILES.md` §2).
//!
//! ## Why this family exists, and why it is not metatile_screens again
//!
//! §2 has named `room_grid` since W4-02 and nothing implemented it, so
//! `profiles/snes/example-mode7` declared `kind = "room_grid"` and decoded
//! **nothing** — a profile that looks configured and does not work. W9-08's
//! first criterion is "each exercising a decoder family the first does
//! not", and its note is explicit that the point is *breadth of format
//! coverage, not count*: two profiles of the same shape prove less than
//! one that forces a second family into existence.
//!
//! So the two families are deliberately different shapes:
//!
//! | | `metatile_screens` | `room_grid` |
//! |---|---|---|
//! | layout | a stream, left to right | a 2-D grid |
//! | unit | a column | a fixed `w × h` room |
//! | indirection | none — data is the level | optional room-number table |
//! | reuse | none | one room can appear many times |
//!
//! That last row is the interesting one, and it is why the family is
//! worth having: room games reuse rooms because that is how they fit on
//! a cartridge, and a decoder that could not express reuse would be a
//! decoder no room game could use.
//!
//! ## Same two properties as the first family
//!
//! **A pure function of ROM bytes** — no core, no bus, no frame — and
//! **offsets are into the NORMALIZED, header-stripped image**. Handing
//! this a raw iNES file shifts everything by 16 bytes and yields a
//! plausible-looking wrong level rather than an error, which is why it is
//! said here and asserted in the tests.
//!
//! ## Family version
//!
//! **`room_grid` as implemented here is family version 1.** Schema v0 has
//! no `decode.family_version` field (it is v0.2's), so this is recorded
//! rather than enforced — the same posture `metatile_screens` takes. Any
//! change to how bytes become rooms is a version bump, not a patch.

use rf_profiles::schema::Profile;

use super::{slice_at, DecodeError};

/// `[[rom_map]]` label for the room tile data.
pub const LABEL_ROOM_DATA: &str = "room_grid_data";

/// `[[rom_map]]` label for the room-number table, when `indexed` is set.
pub const LABEL_ROOM_INDEX: &str = "room_grid_index";

/// The family name, as it appears in `decode.kind`.
pub const FAMILY: &str = "room_grid";

/// Everything this family needs, resolved from a profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub rooms_across: u32,
    pub rooms_down: u32,
    pub room_width: u32,
    pub room_height: u32,
    /// Offset of the room tile data in the normalized image.
    pub data_offset: u32,
    /// Offset of the room-number table, when the grid is indexed.
    pub index_offset: Option<u32>,
    /// Offset of the per-tile collision attribute table, when the profile
    /// declares a `[decode].collision` table (ticket W16-05, mirroring
    /// `metatile_screens::Spec::collision_table`). Unlike
    /// `metatile_screens` — which indexes by *metatile id* through its own
    /// definition table — `room_grid` tiles ARE the raw bytes stored in
    /// ROM, so this table is indexed directly by tile byte value.
    pub collision_table: Option<u32>,
}

impl Spec {
    /// Tiles in one room.
    #[must_use]
    pub fn room_tiles(&self) -> u32 {
        self.room_width * self.room_height
    }

    /// Cells in the grid.
    #[must_use]
    pub fn cells(&self) -> u32 {
        self.rooms_across * self.rooms_down
    }
}

/// A decoded level: the grid, and the distinct rooms it refers to.
///
/// Rooms are stored **once** and the grid holds indices into them, which
/// mirrors how the ROM stores it and means a level reusing one room forty
/// times costs one room of memory rather than forty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedRooms {
    pub rooms_across: u32,
    pub rooms_down: u32,
    pub room_width: u32,
    pub room_height: u32,
    /// `rooms_across * rooms_down` entries, in reading order; each is an
    /// index into [`DecodedRooms::rooms`].
    pub grid: Vec<u8>,
    /// Distinct room payloads, each `room_width * room_height` bytes.
    pub rooms: Vec<Vec<u8>>,
    /// One collision attribute byte per **tile value actually used**
    /// across every decoded room, indexed by that raw tile byte — sized
    /// to `max(tile value) + 1`, the same "derive the table's real length
    /// from what the level uses" discipline
    /// `metatile_screens::metatile_table_len` applies, since `room_grid`
    /// has no separate tile-definition table to size against. `None` when
    /// the profile declared no `[decode].collision` table (ticket W16-05).
    pub collision: Option<Vec<u8>>,
}

impl DecodedRooms {
    /// The room index at a grid cell, or `None` outside the grid.
    #[must_use]
    pub fn room_at(&self, col: u32, row: u32) -> Option<u8> {
        if col >= self.rooms_across || row >= self.rooms_down {
            return None;
        }
        self.grid
            .get((row * self.rooms_across + col) as usize)
            .copied()
    }

    /// The tile payload of the room at a grid cell.
    #[must_use]
    pub fn room_tiles_at(&self, col: u32, row: u32) -> Option<&[u8]> {
        let idx = self.room_at(col, row)?;
        self.rooms.get(idx as usize).map(Vec::as_slice)
    }

    /// One tile, addressed in whole-level coordinates.
    ///
    /// Provided because the natural thing a caller wants — "what is at
    /// level position (x, y)" — otherwise requires every caller to
    /// re-derive the room/offset arithmetic, and getting it subtly wrong
    /// is how a map view ends up showing the right rooms in the wrong
    /// places.
    #[must_use]
    pub fn tile_at(&self, x: u32, y: u32) -> Option<u8> {
        let (col, row) = (x / self.room_width, y / self.room_height);
        let tiles = self.room_tiles_at(col, row)?;
        let (ix, iy) = (x % self.room_width, y % self.room_height);
        tiles.get((iy * self.room_width + ix) as usize).copied()
    }

    /// Level size in tiles.
    #[must_use]
    pub fn level_size(&self) -> (u32, u32) {
        (
            self.rooms_across * self.room_width,
            self.rooms_down * self.room_height,
        )
    }
}

/// Resolve a [`Spec`] from a loaded profile.
///
/// # Errors
/// [`DecodeError`] naming the specific missing field or `[[rom_map]]`
/// entry — the reader is usually the person who just wrote the profile.
pub fn spec_from_profile(profile: &Profile) -> Result<Spec, DecodeError> {
    let decode = profile
        .decode
        .as_ref()
        .ok_or(DecodeError::NoDecodeSection)?;
    if decode.kind != FAMILY {
        return Err(DecodeError::UnknownFamily(decode.kind.clone()));
    }
    let rg = decode.room_grid.as_ref().ok_or(DecodeError::MissingSpec {
        family: FAMILY,
        field: "room_grid",
    })?;

    let find = |label: &'static str| -> Option<u32> {
        profile
            .rom_map
            .iter()
            .find(|e| e.label == label)
            .map(|e| e.offset)
    };

    let data_offset = find(LABEL_ROOM_DATA).ok_or(DecodeError::MissingRomMapEntry {
        family: FAMILY,
        label: LABEL_ROOM_DATA,
    })?;

    // An indexed grid without its table is a profile error, not a
    // fallback to unindexed: silently laying rooms out in reading order
    // would produce a level that looks decoded and is wrong everywhere a
    // room repeats.
    let index_offset = if rg.indexed {
        Some(
            find(LABEL_ROOM_INDEX).ok_or(DecodeError::MissingRomMapEntry {
                family: FAMILY,
                label: LABEL_ROOM_INDEX,
            })?,
        )
    } else {
        None
    };

    if rg.rooms_across == 0 || rg.rooms_down == 0 {
        return Err(DecodeError::MalformedLevel(
            "room_grid: rooms_across and rooms_down must both be non-zero".to_string(),
        ));
    }
    if rg.room_width == 0 || rg.room_height == 0 {
        return Err(DecodeError::MalformedLevel(
            "room_grid: room_width and room_height must both be non-zero".to_string(),
        ));
    }

    Ok(Spec {
        rooms_across: rg.rooms_across,
        rooms_down: rg.rooms_down,
        room_width: rg.room_width,
        room_height: rg.room_height,
        data_offset,
        index_offset,
        collision_table: decode.collision.as_ref().map(|c| c.table),
    })
}

/// Decode a level from the **normalized** ROM image.
///
/// # Errors
/// [`DecodeError::OutOfRange`] when a table or the room data runs past the
/// end of the image, [`DecodeError::MalformedLevel`] when the index names
/// a room the data does not contain.
pub fn decode(rom: &[u8], spec: &Spec) -> Result<DecodedRooms, DecodeError> {
    let cells = spec.cells();
    let room_tiles = spec.room_tiles();

    // Unindexed: cell N is room N, so the data must hold one room per
    // cell. Indexed: the table says which rooms are used, and the data
    // holds however many distinct rooms exist.
    let (grid, room_count) = match spec.index_offset {
        Some(index_offset) => {
            let table = slice_at(rom, index_offset, cells, "room_grid_index")?;
            let grid = table.to_vec();
            // Rooms are counted from the table rather than assumed, so a
            // level using rooms 0..3 in forty cells reads four rooms.
            let highest = grid.iter().copied().max().unwrap_or(0);
            (grid, u32::from(highest) + 1)
        }
        None => {
            if cells > u32::from(u8::MAX) + 1 {
                return Err(DecodeError::MalformedLevel(format!(
                    "room_grid: an unindexed grid has {cells} cells, but a room index is one \
                     byte — such a level needs `indexed = true` and a room_grid_index table"
                )));
            }
            #[allow(clippy::cast_possible_truncation)]
            let grid: Vec<u8> = (0..cells).map(|i| i as u8).collect();
            (grid, cells)
        }
    };

    let total = room_count.checked_mul(room_tiles).ok_or_else(|| {
        DecodeError::MalformedLevel(format!(
            "room_grid: {room_count} rooms of {room_tiles} tiles overflows"
        ))
    })?;
    let data = slice_at(rom, spec.data_offset, total, "room_grid_data")?;

    let rooms: Vec<Vec<u8>> = data
        .chunks_exact(room_tiles as usize)
        .map(<[u8]>::to_vec)
        .collect();

    // The index must not name a room the data does not contain. Without
    // this a bad table produces a level with holes in it, and the caller
    // finds out by rendering nothing where a room should be.
    if let Some(bad) = grid.iter().find(|&&i| usize::from(i) >= rooms.len()) {
        return Err(DecodeError::MalformedLevel(format!(
            "room_grid: index names room {bad}, but only {} room(s) were decoded",
            rooms.len()
        )));
    }

    // Sized from what the level actually uses, not assumed to be 256 —
    // the same reasoning `metatile_table_len` documents: a room byte the
    // table does not cover is caught here as an out-of-range read rather
    // than silently reading whatever follows the table.
    let collision = match spec.collision_table {
        Some(table) => {
            let max_tile = rooms.iter().flatten().copied().max().unwrap_or(0);
            let len = u32::from(max_tile) + 1;
            Some(slice_at(rom, table, len, "collision_table")?.to_vec())
        }
        None => None,
    };

    Ok(DecodedRooms {
        rooms_across: spec.rooms_across,
        rooms_down: spec.rooms_down,
        room_width: spec.room_width,
        room_height: spec.room_height,
        grid,
        rooms,
        collision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two rooms of 2x2: room 0 is all 0xA, room 1 all 0xB.
    fn two_rooms() -> Vec<u8> {
        let mut rom = vec![0u8; 16]; // padding, so offset 0 is never the answer by accident
        rom.extend_from_slice(&[0xAA, 0xAA, 0xAA, 0xAA]); // room 0 @ 16
        rom.extend_from_slice(&[0xBB, 0xBB, 0xBB, 0xBB]); // room 1 @ 20
        rom.extend_from_slice(&[0, 1, 1, 0]); // index @ 24: 2x2 grid
        rom
    }

    fn indexed_spec() -> Spec {
        Spec {
            rooms_across: 2,
            rooms_down: 2,
            room_width: 2,
            room_height: 2,
            data_offset: 16,
            index_offset: Some(24),
            collision_table: None,
        }
    }

    #[test]
    fn an_indexed_grid_reuses_one_room_in_several_cells() {
        // The property that makes this family worth having: room reuse is
        // how room games fit on a cartridge.
        let out = decode(&two_rooms(), &indexed_spec()).unwrap();
        assert_eq!(out.grid, vec![0, 1, 1, 0]);
        assert_eq!(out.rooms.len(), 2, "two DISTINCT rooms, used four times");
        assert_eq!(out.room_at(0, 0), Some(0));
        assert_eq!(out.room_at(1, 0), Some(1));
        assert_eq!(out.room_at(0, 1), Some(1));
        assert_eq!(out.room_at(1, 1), Some(0));
    }

    #[test]
    fn tile_at_resolves_whole_level_coordinates() {
        // The arithmetic every caller would otherwise re-derive, and get
        // subtly wrong — right rooms in the wrong places.
        let out = decode(&two_rooms(), &indexed_spec()).unwrap();
        assert_eq!(out.level_size(), (4, 4));
        assert_eq!(out.tile_at(0, 0), Some(0xAA)); // room 0
        assert_eq!(out.tile_at(3, 0), Some(0xBB)); // room 1, right half
        assert_eq!(out.tile_at(0, 3), Some(0xBB)); // room 1, bottom-left
        assert_eq!(out.tile_at(3, 3), Some(0xAA)); // room 0 again
        assert_eq!(out.tile_at(4, 0), None, "outside the level");
    }

    #[test]
    fn an_unindexed_grid_lays_rooms_out_in_reading_order() {
        let mut rom = vec![0u8; 8];
        rom.extend_from_slice(&[1, 1, 2, 2, 3, 3, 4, 4]); // 4 rooms of 2 tiles
        let spec = Spec {
            rooms_across: 2,
            rooms_down: 2,
            room_width: 2,
            room_height: 1,
            data_offset: 8,
            index_offset: None,
            collision_table: None,
        };
        let out = decode(&rom, &spec).unwrap();
        assert_eq!(out.grid, vec![0, 1, 2, 3]);
        assert_eq!(out.rooms.len(), 4);
        assert_eq!(out.room_tiles_at(1, 1), Some(&[4u8, 4][..]));
    }

    #[test]
    fn an_index_naming_a_room_that_was_not_decoded_is_refused() {
        // Otherwise the level has holes and the caller finds out by
        // rendering nothing where a room should be.
        let mut rom = vec![0u8; 16];
        rom.extend_from_slice(&[0xAA; 4]); // exactly ONE room
        rom.extend_from_slice(&[0, 9, 0, 0]); // index names room 9
        let spec = Spec {
            index_offset: Some(20),
            ..indexed_spec()
        };
        let err = decode(&rom, &spec).unwrap_err();
        // Room 9 makes room_count 10, so the DATA runs out first — either
        // way it must refuse rather than produce a holed level.
        assert!(
            matches!(
                err,
                DecodeError::MalformedLevel(_) | DecodeError::OutOfRange { .. }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn data_running_past_the_end_names_the_table_and_the_rom_length() {
        let spec = Spec {
            data_offset: 1000,
            ..indexed_spec()
        };
        let err = decode(&two_rooms(), &spec).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("room_grid_data"), "{msg}");
        assert!(msg.contains("NORMALIZED"), "the trap must be named: {msg}");
    }

    #[test]
    fn an_unindexed_grid_larger_than_a_byte_is_refused_with_the_fix() {
        // 300 cells cannot be numbered in one byte. Refusing with the
        // remedy named beats silently truncating to 256.
        let spec = Spec {
            rooms_across: 20,
            rooms_down: 15,
            room_width: 1,
            room_height: 1,
            data_offset: 0,
            index_offset: None,
            collision_table: None,
        };
        let err = decode(&vec![0u8; 4096], &spec).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("indexed = true"), "must name the fix: {msg}");
    }

    // ---- profile resolution ----

    /// Build a real profile through the real loader.
    ///
    /// `[[rom_map]]` needs `offset`, `len`, `label`, `type` AND `source`
    /// — the last is FR-PROF-003's clean-room citation and the loader
    /// rejects a profile without it. Written against `RomMapEntry` rather
    /// than from memory: an earlier draft of this helper omitted `len`
    /// and every profile test failed on `missing field \`len\``.
    fn profile_with(extra: &str) -> Profile {
        let toml = format!(
            "[meta]\nprofile_version = \"0.1\"\ntitle = \"T\"\nconsole = \"nes\"\n\
             region = \"ntsc\"\n\n[decode]\nkind = \"room_grid\"\n{extra}"
        );
        rf_profiles::load_str(&toml).expect("valid profile").profile
    }

    #[test]
    fn a_profile_resolves_into_a_spec() {
        let p = profile_with(
            "\n[decode.room_grid]\nrooms_across = 2\nrooms_down = 2\n\
             room_width = 4\nroom_height = 4\nindexed = true\n\n\
             [[rom_map]]\nlabel = \"room_grid_data\"\noffset = 16\nlen = 64\n\
             type = \"room_tiles\"\nsource = \"in-repo fixture, this ticket\"\n\
             [[rom_map]]\nlabel = \"room_grid_index\"\noffset = 64\nlen = 4\n\
             type = \"room_index\"\nsource = \"in-repo fixture, this ticket\"\n",
        );
        let spec = spec_from_profile(&p).unwrap();
        assert_eq!(spec.data_offset, 16);
        assert_eq!(spec.index_offset, Some(64));
        assert_eq!(spec.room_tiles(), 16);
        assert_eq!(spec.cells(), 4);
    }

    #[test]
    fn an_indexed_profile_without_its_index_table_is_refused() {
        // NOT a silent fallback to unindexed: that would produce a level
        // that looks decoded and is wrong everywhere a room repeats.
        let p = profile_with(
            "\n[decode.room_grid]\nrooms_across = 2\nrooms_down = 2\n\
             room_width = 4\nroom_height = 4\nindexed = true\n\n\
             [[rom_map]]\nlabel = \"room_grid_data\"\noffset = 16\nlen = 64\n\
             type = \"room_tiles\"\nsource = \"in-repo fixture, this ticket\"\n",
        );
        let err = spec_from_profile(&p).unwrap_err();
        assert_eq!(
            err,
            DecodeError::MissingRomMapEntry {
                family: FAMILY,
                label: LABEL_ROOM_INDEX
            }
        );
    }

    #[test]
    fn a_profile_declaring_room_grid_without_the_table_says_which_field() {
        // This is exactly what profiles/snes/example-mode7 was: kind
        // declared, nothing configured, nothing decoded.
        let p = profile_with(
            "\n[[rom_map]]\nlabel = \"room_grid_data\"\noffset = 0\nlen = 16\n\
             type = \"room_tiles\"\nsource = \"in-repo fixture, this ticket\"\n",
        );
        assert_eq!(
            spec_from_profile(&p).unwrap_err(),
            DecodeError::MissingSpec {
                family: FAMILY,
                field: "room_grid"
            }
        );
    }

    #[test]
    fn a_zero_dimension_is_refused() {
        for bad in [
            "rooms_across = 0\nrooms_down = 2",
            "rooms_across = 2\nrooms_down = 0",
        ] {
            let p = profile_with(&format!(
                "\n[decode.room_grid]\n{bad}\nroom_width = 4\nroom_height = 4\n\n\
                 [[rom_map]]\nlabel = \"room_grid_data\"\noffset = 0\nlen = 16\n\
                 type = \"room_tiles\"\nsource = \"in-repo fixture, this ticket\"\n"
            ));
            assert!(matches!(
                spec_from_profile(&p),
                Err(DecodeError::MalformedLevel(_))
            ));
        }
    }

    #[test]
    fn the_other_family_is_not_accepted_here() {
        let p = profile_with("");
        let mut other = p.clone();
        other.decode.as_mut().unwrap().kind = "metatile_screens".into();
        assert_eq!(
            spec_from_profile(&other).unwrap_err(),
            DecodeError::UnknownFamily("metatile_screens".into())
        );
    }

    // ---- collision (ticket W16-05) ----

    #[test]
    fn a_profile_with_a_collision_table_resolves_it_onto_the_spec() {
        // `[decode].collision` already exists on the schema (it hangs off
        // `Decode`, not off `RoomGridSpec` — `metatile_screens` reads the
        // same field), so this is wiring, not a schema change.
        let p = profile_with(
            "collision = { table = 40, bits = \"solid,platform,hazard\" }\n\n\
             [decode.room_grid]\nrooms_across = 2\nrooms_down = 2\n\
             room_width = 2\nroom_height = 2\nindexed = true\n\n\
             [[rom_map]]\nlabel = \"room_grid_data\"\noffset = 16\nlen = 16\n\
             type = \"room_tiles\"\nsource = \"in-repo fixture, this ticket\"\n\
             [[rom_map]]\nlabel = \"room_grid_index\"\noffset = 32\nlen = 4\n\
             type = \"room_index\"\nsource = \"in-repo fixture, this ticket\"\n",
        );
        let spec = spec_from_profile(&p).unwrap();
        assert_eq!(spec.collision_table, Some(40));
    }

    #[test]
    fn a_profile_without_collision_leaves_the_spec_field_none() {
        let spec = spec_from_profile(&profile_with(
            "\n[decode.room_grid]\nrooms_across = 2\nrooms_down = 2\n\
             room_width = 2\nroom_height = 2\n\n\
             [[rom_map]]\nlabel = \"room_grid_data\"\noffset = 0\nlen = 16\n\
             type = \"room_tiles\"\nsource = \"in-repo fixture, this ticket\"\n",
        ))
        .unwrap();
        assert_eq!(spec.collision_table, None);
    }

    /// A synthetic room grid whose tiles double as their own collision
    /// index — two rooms of a fully solid floor (`0x01`) and fully open
    /// space (`0x00`) — proving criterion 1: "the room_grid decoder
    /// family gains a collision table and bits exactly like
    /// metatile_screens".
    #[test]
    fn decoding_a_synthetic_room_grid_with_a_collision_table_produces_it() {
        let mut rom = vec![0u8; 16]; // padding
        rom.extend_from_slice(&[0x01, 0x01, 0x01, 0x01]); // room 0 @ 16: floor
        rom.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // room 1 @ 20: open
        rom.extend_from_slice(&[0, 1, 1, 0]); // index @ 24: 2x2 grid
                                              // collision table @ 28: tile 0x00 -> not solid, tile 0x01 -> solid (bit0)
        rom.extend_from_slice(&[0b000, 0b001]);

        let spec = Spec {
            collision_table: Some(28),
            ..indexed_spec()
        };
        let out = decode(&rom, &spec).unwrap();
        let collision = out.collision.as_ref().expect("collision table declared");
        assert_eq!(collision.len(), 2, "sized to max tile value (1) + 1");
        assert_eq!(collision[0x00], 0b000, "open tile: not solid");
        assert_eq!(collision[0x01], 0b001, "floor tile: solid");
    }

    #[test]
    fn a_missing_collision_table_declares_none() {
        let out = decode(&two_rooms(), &indexed_spec()).unwrap();
        assert_eq!(out.collision, None);
    }

    #[test]
    fn a_collision_table_running_past_the_rom_names_the_table() {
        let spec = Spec {
            collision_table: Some(1_000),
            ..indexed_spec()
        };
        let err = decode(&two_rooms(), &spec).unwrap_err();
        assert!(err.to_string().contains("collision_table"), "{err}");
    }
}
