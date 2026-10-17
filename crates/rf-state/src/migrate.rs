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
    /// Migrations that apply to one console's payload only (the header's
    /// `console` byte). Consulted before `fns`, so a tag whose NES and
    /// SNES layouts differ can carry a function for each.
    scoped: HashMap<(u8, [u8; 4]), MigrateFn>,
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

    /// Register (or replace) the migration for `tag` in containers whose
    /// header `console` byte is `console`. Takes precedence over a
    /// console-agnostic [`register`](Self::register)ed function.
    pub fn register_for_console(&mut self, console: u8, tag: [u8; 4], f: MigrateFn) {
        self.scoped.insert((console, tag), f);
    }

    /// Run the registered migration for `tag`, if any.
    pub(crate) fn migrate(
        &self,
        console: u8,
        tag: [u8; 4],
        found_version: u16,
        payload: &[u8],
    ) -> Option<Result<Vec<u8>, ContainerError>> {
        let f = self
            .scoped
            .get(&(console, tag))
            .or_else(|| self.fns.get(&tag));
        f.map(|f| {
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
        assert!(reg.migrate(0, *b"CPU_", 0, &[]).is_none());
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
            reg.migrate(0, *b"CPU_", 0, &[1, 2]).unwrap().unwrap(),
            vec![1, 2]
        );

        reg.register(*b"CPU_", failing_migration);
        let err = reg.migrate(0, *b"CPU_", 0, &[]).unwrap().unwrap_err();
        assert_eq!(
            err,
            ContainerError::MigrationFailed {
                tag: *b"CPU_",
                message: "cannot bridge this gap".to_string(),
            }
        );
    }

    #[test]
    fn a_console_scoped_migration_wins_over_the_agnostic_one() {
        fn agnostic(_: u16, _: &[u8]) -> Result<Vec<u8>, String> {
            Ok(vec![0])
        }
        fn snes(_: u16, _: &[u8]) -> Result<Vec<u8>, String> {
            Ok(vec![1])
        }
        let mut reg = MigrationRegistry::new();
        reg.register(*b"PPU_", agnostic);
        reg.register_for_console(1, *b"PPU_", snes);
        assert_eq!(reg.migrate(0, *b"PPU_", 4, &[]).unwrap().unwrap(), vec![0]);
        assert_eq!(reg.migrate(1, *b"PPU_", 4, &[]).unwrap().unwrap(), vec![1]);
        let mut only_snes = MigrationRegistry::new();
        only_snes.register_for_console(1, *b"PPU_", snes);
        assert!(only_snes.migrate(0, *b"PPU_", 4, &[]).is_none());
    }
}
