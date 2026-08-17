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
        required: true,
        current_version: 1,
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
        // No migration fn: `Container::decode_default` turns an
        // unexpected version into `CannotMigrate`, which SAVE_STATES.md
        // §2 allows as the alternative to migrating. A pre-W2-19 state
        // is refused by name rather than misparsed -- the failure worth
        // preventing is a v1 payload read as v2, which would restore a
        // wrong PPU silently. Nothing has shipped (the tagged release is
        // W5-05), so no such file exists outside a working tree.
        required: true,
        current_version: 2,
    },
    TagInfo {
        tag: *b"APU_",
        owner: "core",
        required: true,
        current_version: 1,
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
