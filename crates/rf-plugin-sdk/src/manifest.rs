//! Plugin manifest and capability model (ticket W4-04;
//! `docs/design/PLUGINS.md` §2, FR-PLUG-001/002/003, D-005).
//!
//! Two things are schema-required from v0 and refused without, because
//! D-005 makes community intake **deny-by-default**: an SPDX `license`
//! and a `provenance` statement. PLUGINS.md §2 is explicit that
//! "unknown/missing licenses … are rejected at install scan, **before any
//! capability prompt**" — so they are validated here, before a
//! capability list is ever shown to a user, rather than as a later step
//! someone could reorder.

use std::collections::BTreeMap;

/// What a plugin is allowed to do (`PLUGINS.md` §2's `[capabilities]`).
///
/// Every field defaults to the **denying** value. That is the whole
/// design: a manifest that forgets a capability, or a build that adds a
/// new one, denies rather than grants — so the failure direction of an
/// out-of-date manifest is "less access", never "more".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Capabilities {
    pub read_memory: bool,
    pub read_ppu: bool,
    pub frame_events: bool,
    /// "costs perf; explicit" (§2).
    pub scanline_events: bool,
    pub draw_overlay: bool,
    pub replace_layers: bool,
    /// The mod boundary (§2): "silently altering game behavior is
    /// impossible because the host routes all writes through a ledger".
    pub write_memory: bool,
    pub input_bindings: bool,
    /// `none` | `cache_dir` — never raw filesystem access.
    pub filesystem: FilesystemCap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilesystemCap {
    #[default]
    None,
    CacheDir,
}

impl FilesystemCap {
    fn from_str(s: &str) -> Self {
        match s.trim() {
            "cache_dir" => FilesystemCap::CacheDir,
            // Anything unrecognised denies. A typo'd capability must not
            // grant a capability.
            _ => FilesystemCap::None,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            FilesystemCap::None => "none",
            FilesystemCap::CacheDir => "cache_dir",
        }
    }
}

impl Capabilities {
    /// The list a UI shows at enable time (FR-PLUG-003), most-dangerous
    /// first.
    ///
    /// Ordering is deliberate and is the UI's whole value: a user
    /// skimming a list stops reading after a few lines, so `write_memory`
    /// must never be the ninth bullet under `read_memory`.
    #[must_use]
    pub fn granted_summary(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.write_memory {
            out.push("WRITE MEMORY — can alter game behaviour (mod tier)");
        }
        if self.replace_layers {
            out.push("replace rendered layers");
        }
        if self.input_bindings {
            out.push("register hotkeys / virtual buttons");
        }
        if self.filesystem == FilesystemCap::CacheDir {
            out.push("read/write its own cache directory");
        }
        if self.draw_overlay {
            out.push("draw an overlay");
        }
        if self.read_memory {
            out.push("read emulated memory");
        }
        if self.read_ppu {
            out.push("read PPU memory (VRAM/OAM/palette)");
        }
        if self.scanline_events {
            out.push("receive per-scanline events (costs performance)");
        }
        if self.frame_events {
            out.push("receive per-frame events");
        }
        out
    }
}

/// A parsed plugin manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub id: String,
    pub version: String,
    pub api: String,
    /// SPDX expression — required (D-005).
    pub license: String,
    /// Where this plugin came from — required (D-005).
    pub provenance: String,
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// A required field is missing or blank. Named individually rather
    /// than as a generic "invalid manifest" so an author is told which.
    MissingField(&'static str),
    Parse(String),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::MissingField(field) => write!(
                f,
                "plugin manifest is missing required field `{field}` — D-005 makes community \
                 intake deny-by-default, so `license` (SPDX) and `provenance` are required \
                 before any capability prompt is shown"
            ),
            ManifestError::Parse(msg) => write!(f, "plugin manifest parse error: {msg}"),
        }
    }
}

impl std::error::Error for ManifestError {}

fn flag(map: &BTreeMap<String, String>, key: &str) -> bool {
    map.get(key).map(|v| v.trim()) == Some("true")
}

impl Manifest {
    /// Parse the minimal `key = value` manifest shape PLUGINS.md §2
    /// sketches.
    ///
    /// Deliberately hand-parsed rather than reaching for a TOML
    /// dependency in this crate: the format is flat, the workspace's
    /// sanctioned serialization story is already narrow, and a manifest
    /// parser that cannot fail in interesting ways is worth more here
    /// than one that supports nesting nobody uses.
    ///
    /// # Errors
    /// [`ManifestError::MissingField`] when `id`, `version`, `api`,
    /// `license` or `provenance` is absent or blank.
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let mut fields: BTreeMap<String, String> = BTreeMap::new();
        for raw in text.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() || line.starts_with('[') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            fields.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
        }

        let required = |key: &'static str| -> Result<String, ManifestError> {
            match fields.get(key) {
                Some(v) if !v.trim().is_empty() => Ok(v.clone()),
                _ => Err(ManifestError::MissingField(key)),
            }
        };

        Ok(Manifest {
            id: required("id")?,
            version: required("version")?,
            api: required("api")?,
            license: required("license")?,
            provenance: required("provenance")?,
            capabilities: Capabilities {
                read_memory: flag(&fields, "read_memory"),
                read_ppu: flag(&fields, "read_ppu"),
                frame_events: flag(&fields, "frame_events"),
                scanline_events: flag(&fields, "scanline_events"),
                draw_overlay: flag(&fields, "draw_overlay"),
                replace_layers: flag(&fields, "replace_layers"),
                write_memory: flag(&fields, "write_memory"),
                input_bindings: flag(&fields, "input_bindings"),
                filesystem: fields
                    .get("filesystem")
                    .map_or(FilesystemCap::None, |v| FilesystemCap::from_str(v)),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        [plugin]
        id = "example-minimap"
        version = "0.1.0"
        api = "0.1"
        license = "MIT"
        provenance = "first-party, RetroForge examples"
    "#;

    #[test]
    fn a_minimal_manifest_parses_and_grants_nothing() {
        let m = Manifest::parse(MINIMAL).expect("minimal manifest parses");
        assert_eq!(m.id, "example-minimap");
        assert_eq!(m.capabilities, Capabilities::default());
        assert!(
            m.capabilities.granted_summary().is_empty(),
            "a manifest that declares no capabilities must grant none"
        );
    }

    /// D-005: intake is deny-by-default and these are refused BEFORE any
    /// capability prompt.
    #[test]
    fn license_and_provenance_are_required() {
        for missing in ["license", "provenance", "id", "version", "api"] {
            let text: String = MINIMAL
                .lines()
                .filter(|l| !l.trim().starts_with(missing))
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(
                Manifest::parse(&text),
                Err(ManifestError::MissingField(missing)),
                "a manifest without `{missing}` must be refused"
            );
        }
        // Blank is not present.
        let blank = MINIMAL.replace(r#"license = "MIT""#, r#"license = "  ""#);
        assert_eq!(
            Manifest::parse(&blank),
            Err(ManifestError::MissingField("license"))
        );
    }

    /// The failure direction of an unrecognised value must be LESS
    /// access, never more.
    #[test]
    fn unrecognised_capability_values_deny() {
        let text = format!("{MINIMAL}\nfilesystem = \"/etc\"\nwrite_memory = \"yes\"\n");
        let m = Manifest::parse(&text).unwrap();
        assert_eq!(
            m.capabilities.filesystem,
            FilesystemCap::None,
            "an unrecognised filesystem value must not grant access"
        );
        assert!(
            !m.capabilities.write_memory,
            "only the literal `true` grants — `yes` must not"
        );
    }

    /// FR-PLUG-003's list is ordered most-dangerous-first, because a user
    /// skimming it stops reading after a few lines.
    #[test]
    fn the_enable_time_summary_leads_with_the_dangerous_capabilities() {
        let text =
            format!("{MINIMAL}\nread_memory = true\nframe_events = true\nwrite_memory = true\n");
        let summary = Manifest::parse(&text)
            .unwrap()
            .capabilities
            .granted_summary();
        assert!(
            summary[0].contains("WRITE MEMORY"),
            "write_memory must lead the list, got {summary:?}"
        );
        assert_eq!(summary.len(), 3);
    }
}
