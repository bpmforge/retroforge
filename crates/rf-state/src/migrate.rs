//! Chunk-version migration hooks (SAVE_STATES.md §2: "chunk `version` bump
//! requires a migration fn or an explicit 'cannot migrate' error").
//!
//! `rf-state` owns the container envelope, not any other crate's payload
//! layout, so it cannot itself contain the migration logic for `CPU_`,
//! `PPU_`, etc. — that belongs to each chunk's owning crate. What this
//! crate provides is the *hook*: a small caller-supplied table of
//! `(tag, found_version) -> Result<new_payload, _>` functions consulted
//! during [`crate::Container::decode`]. A tag with no registered
//! migration and a version mismatch is a hard, named error — never a
//! silent best-effort guess at forward compatibility.

use std::collections::HashMap;

use crate::error::ContainerError;

/// A migration function: given the version actually found and the raw
/// payload bytes at that version, produce the payload bytes at the
/// registry's current version (`crate::tags::TagInfo::current_version`).
pub type MigrateFn = fn(found_version: u16, payload: &[u8]) -> Result<Vec<u8>, String>;

/// Caller-supplied table of chunk migrations, consulted by
/// [`crate::Container::decode`] whenever a known chunk's version does not
/// match this build's expectation.
#[derive(Default)]
pub struct MigrationRegistry {
    fns: HashMap<[u8; 4], MigrateFn>,
}

impl MigrationRegistry {
    /// An empty registry: every version mismatch will be a hard error.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or replace) the migration function for `tag`.
    pub fn register(&mut self, tag: [u8; 4], f: MigrateFn) {
        self.fns.insert(tag, f);
    }

    /// Run the registered migration for `tag`, if any.
    pub(crate) fn migrate(
        &self,
        tag: [u8; 4],
        found_version: u16,
        payload: &[u8],
    ) -> Option<Result<Vec<u8>, ContainerError>> {
        self.fns.get(&tag).map(|f| {
            f(found_version, payload)
                .map_err(|message| ContainerError::MigrationFailed { tag, message })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_has_no_migration_for_any_tag() {
        let reg = MigrationRegistry::new();
        assert!(reg.migrate(*b"CPU_", 0, &[]).is_none());
    }

    #[test]
    fn registered_migration_runs_and_can_fail() {
        fn ok_migration(_found: u16, payload: &[u8]) -> Result<Vec<u8>, String> {
            Ok(payload.to_vec())
        }
        fn failing_migration(_found: u16, _payload: &[u8]) -> Result<Vec<u8>, String> {
            Err("cannot bridge this gap".to_string())
        }

        let mut reg = MigrationRegistry::new();
        reg.register(*b"CPU_", ok_migration);
        assert_eq!(
            reg.migrate(*b"CPU_", 0, &[1, 2]).unwrap().unwrap(),
            vec![1, 2]
        );

        reg.register(*b"CPU_", failing_migration);
        let err = reg.migrate(*b"CPU_", 0, &[]).unwrap().unwrap_err();
        assert_eq!(
            err,
            ContainerError::MigrationFailed {
                tag: *b"CPU_",
                message: "cannot bridge this gap".to_string(),
            }
        );
    }
}
