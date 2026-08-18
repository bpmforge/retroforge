//! Profile loading errors.
//!
//! `thiserror` is not a workspace dependency (see `rf-cart::error`'s module
//! doc, same reasoning applies here); hand-rolled `Display` +
//! `std::error::Error` instead of adding one for a single small crate.
//!
//! Every variant names the offending key/field where one exists, so
//! `retroforge-tool profile validate` can report it directly rather than
//! forwarding an opaque parser message (vacuity trap (c): "a `profile
//! validate` test that only checks exit code 0 ... is worthless").

use std::fmt;

/// Everything that can go wrong loading a profile document.
#[derive(Debug)]
pub enum ProfileError {
    /// Could not read the file at all.
    Io(String),
    /// Not well-formed TOML.
    Parse(String),
    /// A field schema v0 requires was absent. Dotted path, e.g.
    /// `"meta.profile_version"`.
    MissingField(String),
    /// `meta.profile_version` was present but not `MAJOR.MINOR`-shaped.
    InvalidVersion(String),
    /// `meta.profile_version`'s major exceeds `schema::SUPPORTED_PROFILE_MAJOR`
    /// (§1: "loader rejects newer majors").
    NewerMajor { found: u64, supported: u64 },
    /// The document parsed as TOML but did not match schema v0's typed
    /// shape (wrong type, missing non-optional field, ...). Message is
    /// `toml`'s own diagnostic, which names the field it choked on.
    Deserialize(String),
    /// A `[[identity]]` entry (0-based index) named no hash family at all
    /// — such an entry could never discriminate a ROM, so it is a load
    /// error rather than a silently-useless row.
    IdentityMissingHash(usize),
    /// A `[[memory_map]]` or `[[rom_map]]` entry carries no `source`
    /// citation (ticket W4-02a; FR-PROF-003: "Every `memory_map`/`rom_map`
    /// entry shall carry a `source` citation (clean-room provenance);
    /// validation shall fail without one").
    ///
    /// Names the table, the 0-based row index AND the row's `label`,
    /// because the index alone is a poor thing to hand someone editing a
    /// forty-row memory map by hand — the label is what they can search
    /// for.
    ///
    /// This is a **provenance** rule, not a tidiness one. D-005 makes
    /// community profile intake deny-by-default with provenance required,
    /// and NFR-011's core rule is clean-room-from-documentation rather
    /// than transcribed. An unenforced `source` lets a profile assert a
    /// RAM map with no stated origin and pass validation — exactly the
    /// intake hazard D-005 exists to close.
    MapEntryMissingSource {
        table: &'static str,
        index: usize,
        label: String,
    },
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProfileError::Io(msg) => write!(f, "I/O error: {msg}"),
            ProfileError::Parse(msg) => write!(f, "TOML syntax error: {msg}"),
            ProfileError::MissingField(path) => write!(f, "missing required field `{path}`"),
            ProfileError::InvalidVersion(v) => write!(
                f,
                "invalid `meta.profile_version` value `{v}` (expected MAJOR.MINOR)"
            ),
            ProfileError::NewerMajor { found, supported } => write!(
                f,
                "`meta.profile_version` major {found} is newer than this loader's supported \
                 major {supported} — refusing to load"
            ),
            ProfileError::Deserialize(msg) => write!(f, "schema error: {msg}"),
            ProfileError::MapEntryMissingSource {
                table,
                index,
                label,
            } => write!(
                f,
                "[[{table}]] entry {index} (`{label}`) has no `source` citation — FR-PROF-003 \
                 requires clean-room provenance for every {table} entry, so validation fails \
                 without one"
            ),
            ProfileError::IdentityMissingHash(idx) => write!(
                f,
                "`[[identity]]` entry {idx} specifies no hash (need at least one of \
                 sha256/sha1/md5/crc32)"
            ),
        }
    }
}

impl std::error::Error for ProfileError {}
