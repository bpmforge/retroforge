//! The chunk tag registry (SAVE_STATES.md §2). `rf-state` owns the
//! container format and, per this ticket, the tag registry itself — it
//! does *not* own or interpret the bytes inside any chunk but its own
//! (`RPLY`); every other tag's payload is opaque to this crate.
//!
//! One table drives three behaviors: which tags are required (missing one
//! is a hard load error), which tags are known at all (an unknown tag is
//! a skip-with-warning, not an error), and what version this build expects
//! for a known tag (a mismatch needs a registered migration or the load
//! fails with [`crate::error::ContainerError::CannotMigrate`]).

/// One row of the tag registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TagInfo {
    /// The 4-byte chunk tag.
    pub tag: [u8; 4],
    /// The owning crate, for documentation/diagnostics only.
    pub owner: &'static str,
    /// Whether a container is invalid without this chunk.
    pub required: bool,
    /// The chunk payload version this build expects.
    pub current_version: u16,
}

/// The full registry: the core (REQUIRED) set plus the optional
/// `INPT`/`PROF`/`ENHC`/`RPLY` chunks, exactly as enumerated in
/// SAVE_STATES.md §2.
pub const TAG_REGISTRY: &[TagInfo] = &[
    TagInfo {
        tag: *b"CPU_",
        owner: "core",
        // Version 2 as of ticket W2-01c, which added the per-port
        // last-controller-read cycle to the bus half of this chunk. It
        // decides whether the next `$4016`/`$4017` read is a new strobe
        // edge and therefore whether it shifts the pad, so a restore that
        // dropped it could clock a bit the saved machine would not have.
        required: true,
        current_version: 2,
    },
    TagInfo {
        tag: *b"PPU_",
        owner: "core",
        // Version 2 as of ticket W2-19, which added the PPU's decay
        // register (its own open-bus latch) and the eight per-bit decay
        // clocks to the payload. Real machine state -- `$2000`-`$2006`
        // reads return it -- so it had to be serialized, and a payload
        // that grew is a payload that changed.
        //
        // Version 3 as of ticket W2-01d, which added the `$2007`
        // double-read latch (`Ppu::last_2007_read_dot`/`_value`).
        //
        // No migration fn: `Container::decode_default` turns an
        // unexpected version into `CannotMigrate`, which SAVE_STATES.md
        // §2 allows as the alternative to migrating. An older state is
        // refused by name rather than misparsed -- the failure worth
        // preventing is an old payload read as the current one, which
        // would restore a wrong PPU silently. Nothing has shipped (the
        // tagged release is W5-05), so no such file exists outside a
        // working tree.
        //
        // Version 4 as of ticket W2-22, which appended the partly drawn
        // scanline (`Ppu::line_buffer`, 256 x 5 bytes): a state taken at a
        // frame boundary holds up to ~21 drawn dots of scanline 0, and
        // dropping them changed the next frame. Releases now exist, so
        // this bump ships a migration (`rf_nes::ppu::migrate_ppu_payload`,
        // registered by `retroforge::save_state::nes_migrations`) instead
        // of refusing version-3 files.
        //
        // **This is the NES (console 0) version.** The SNES `PPU_`
        // payload is an unrelated layout that shares the tag, so it
        // carries its own number in [`CONSOLE_VERSION_OVERRIDES`]
        // (SNES v5 appends `$212D` TS) -- a SNES-only change must not
        // force a bump, and a migration, on every NES state.
        required: true,
        current_version: 4,
    },
    TagInfo {
        tag: *b"APU_",
        owner: "core",
        // Version 2 as of ticket W2-21, which added the APU's delayed
        // view of the IRQ line to the payload. Same reasoning as `PPU_`
        // v2: real machine state, deliberately not reconstructible from
        // the other fields (it is one cycle behind them by design), and
        // read by the CPU before the first tick after a restore.
        required: true,
        current_version: 2,
    },
    TagInfo {
        tag: *b"WRAM",
        owner: "core",
        required: true,
        current_version: 1,
    },
    TagInfo {
        tag: *b"VRAM",
        owner: "core",
        required: true,
        current_version: 1,
    },
    TagInfo {
        tag: *b"OAM_",
        owner: "core",
        required: true,
        current_version: 1,
    },
    TagInfo {
        tag: *b"CGRM",
        owner: "core",
        required: true,
        current_version: 1,
    },
    TagInfo {
        tag: *b"MAPR",
        owner: "core",
        required: true,
        current_version: 1,
    },
    TagInfo {
        tag: *b"CART",
        owner: "core",
        required: true,
        current_version: 1,
    },
    TagInfo {
        tag: *b"INPT",
        owner: "rf-input",
        required: false,
        current_version: 1,
    },
    TagInfo {
        tag: *b"PROF",
        owner: "rf-profiles",
        required: false,
        current_version: 1,
    },
    TagInfo {
        tag: *b"ENHC",
        owner: "rf-enhance",
        required: false,
        current_version: 1,
    },
    TagInfo {
        tag: *b"RPLY",
        owner: "rf-state",
        required: false,
        current_version: 1,
    },
];

/// Look up a tag's registry row, if it is known.
pub fn tag_info(tag: [u8; 4]) -> Option<&'static TagInfo> {
    TAG_REGISTRY.iter().find(|t| t.tag == tag)
}

/// Whether `tag` is in the core (required) set — used to scope
/// [`crate::Container::state_hash`] to core machine state only, so an
/// Accuracy-mode save (no `ENHC`/`PROF`) and an Enhanced-mode save of the
/// same logical machine state (FR-STATE-007) hash identically.
pub fn is_required(tag: [u8; 4]) -> bool {
    tag_info(tag).is_some_and(|t| t.required)
}

/// Header `console` byte for NES states.
pub const CONSOLE_NES: u8 = 0;
/// Header `console` byte for SNES states.
pub const CONSOLE_SNES: u8 = 1;

/// Per-console payload versions that differ from [`TagInfo::current_version`].
///
/// A tag's payload layout belongs to the console that wrote it, so the
/// `PPU_` chunk of a SNES state and of a NES state evolve independently.
/// A `(console, tag)` pair listed here overrides the registry number for
/// that console only; every other pair uses the registry's. `rf-state`
/// still interprets none of the bytes -- these are just numbers.
///
/// - `(SNES, PPU_) = 5`: v4 was the shared number the SNES inherited from
///   the NES; v5 appends `$212D` TS (the sub-screen layer enables), which
///   v4 never stored. Migrate v4 -> v5 with
///   `rf_snes::ppu::migrate_ppu_payload`.
pub const CONSOLE_VERSION_OVERRIDES: &[(u8, [u8; 4], u16)] = &[(CONSOLE_SNES, *b"PPU_", 5)];

/// The payload version this build writes and expects for `tag` in a
/// container whose header says `console`, or `None` for an unknown tag.
#[must_use]
pub fn current_version_for(console: u8, tag: [u8; 4]) -> Option<u16> {
    CONSOLE_VERSION_OVERRIDES
        .iter()
        .find(|(c, t, _)| *c == console && *t == tag)
        .map(|(_, _, v)| *v)
        .or_else(|| tag_info(tag).map(|i| i.current_version))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_no_duplicate_tags() {
        let mut seen = std::collections::HashSet::new();
        for t in TAG_REGISTRY {
            assert!(seen.insert(t.tag), "duplicate tag in registry: {:?}", t.tag);
        }
    }

    #[test]
    fn core_tags_are_required_and_others_are_not() {
        for core_tag in [
            b"CPU_", b"PPU_", b"APU_", b"WRAM", b"VRAM", b"OAM_", b"CGRM", b"MAPR", b"CART",
        ] {
            assert!(is_required(*core_tag), "{core_tag:?} should be required");
        }
        for optional_tag in [b"INPT", b"PROF", b"ENHC", b"RPLY"] {
            assert!(
                !is_required(*optional_tag),
                "{optional_tag:?} should be optional"
            );
        }
    }

    #[test]
    fn unknown_tag_is_not_in_registry() {
        assert!(tag_info(*b"ZZZZ").is_none());
    }
}
