//! The persisted binding file (ticket W2-06): keyboard and pad maps
//! together, in a text format the user can read and edit.
//!
//! ## Why a hand-rolled text format
//!
//! `.rfreplay` (`crate::replay`) already establishes the precedent and the
//! reasoning: text keeps the file diffable and hand-editable, needs no new
//! dependency, and — for a *config* file specifically — means a user who
//! wants "swap A and B" can do it in an editor without the app. This crate
//! has exactly one dependency (`rf-core-api`) and adding `serde` + `toml`
//! to it for one small table would be the larger change.
//!
//! Unlike `.rfreplay`, this format is **forgiving on load**: an
//! unrecognized key name or button name is skipped with a warning rather
//! than refusing the file. A replay that silently dropped an input would
//! produce a wrong result, so it must refuse; a config that refuses to load
//! because one line is stale leaves the user with no bindings at all, which
//! is worse than the binding they lost.
//!
//! ```text
//! RFBIND 1
//! [Keyboard]
//! ArrowUp=0:Up
//! Z=0:B
//! [Gamepad]
//! DpadUp=Up
//! South=A
//! ```
//!
//! Keyboard lines carry a port because a keyboard drives any port (two
//! players can share one); gamepad lines do not, because a pad's port comes
//! from [`crate::PadRouter`]'s hotplug assignment, not from the file.
//!
//! ## Stability status: NOT a public commitment
//!
//! `.rfstate` and `.rfreplay` are versioned public formats governed by
//! `docs/design/CONTRACTS.md` §3 and NFR-008 — other tools read them, and
//! breaking one breaks somebody else. **`bindings.rfbind` is deliberately
//! not in that class**: it is a per-user config this app writes and reads,
//! nothing else consumes it, and losing a binding costs a user thirty
//! seconds in the Controls window rather than a corrupted save.
//!
//! It carries a magic and a version anyway, because a config file with
//! neither is one that gets misidentified the first time somebody points
//! the loader at the wrong path. The version exists to let a future change
//! refuse an old file politely — not to promise anyone that the grammar
//! will not change. Stated here so the next reader does not have to infer
//! the format's status from the fact that it has a version number.

use std::fmt::Write as _;

use crate::{Key, KeyMap, NesButton, PadButton, PadMap};

/// Magic + version line. Bumping the version requires deciding what an old
/// file means, exactly as `.rfreplay`'s version does.
const MAGIC: &str = "RFBIND 1";

/// Keyboard and pad bindings as one saveable unit.
#[derive(Debug, Clone)]
pub struct Bindings {
    pub keys: KeyMap,
    pub pads: PadMap,
}

impl Default for Bindings {
    fn default() -> Self {
        Self {
            keys: KeyMap::default_nes(),
            pads: PadMap::default_nes(),
        }
    }
}

/// What a load could not make sense of. Never fatal on its own — see the
/// module doc on why loading is forgiving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingWarning {
    /// A line that is not `name=value`.
    Malformed { line: usize, text: String },
    /// A key/pad-button/NES-button name this build does not know.
    UnknownName { line: usize, name: String },
    /// A port number outside the supported range.
    BadPort { line: usize, port: String },
}

/// Why a file could not be loaded at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingError {
    /// The first line was not [`MAGIC`].
    BadMagic(String),
}

impl std::fmt::Display for BindingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BindingError::BadMagic(found) => write!(
                f,
                "not a binding file: expected first line {MAGIC:?}, found {found:?}"
            ),
        }
    }
}

impl std::error::Error for BindingError {}

impl Bindings {
    /// Serialize. Deterministic: sections in a fixed order, entries in
    /// binding order, LF endings — so a config committed to a dotfiles repo
    /// does not churn.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = String::from(MAGIC);
        out.push_str("\n[Keyboard]\n");
        for (key, port, button) in self.keys.entries() {
            let _ = writeln!(out, "{}={port}:{}", key.name(), button.name());
        }
        out.push_str("[Gamepad]\n");
        for (pad_button, button) in self.pads.entries() {
            let _ = writeln!(out, "{}={}", pad_button.name(), button.name());
        }
        out
    }

    /// Parse, returning the bindings plus anything that had to be skipped.
    ///
    /// A file with no recognizable entries parses to *empty* bindings, not
    /// to the defaults: silently substituting defaults would make a
    /// corrupted config indistinguishable from a deliberate "unbind
    /// everything", and the caller can apply defaults itself if it wants
    /// them.
    ///
    /// # Errors
    /// Returns [`BindingError::BadMagic`] if the first line is wrong — the
    /// one case where continuing would mean interpreting an unrelated file.
    pub fn from_text(text: &str) -> Result<(Self, Vec<BindingWarning>), BindingError> {
        let mut lines = text.lines().enumerate();
        let first = lines.next().map(|(_, l)| l.trim()).unwrap_or_default();
        if first != MAGIC {
            return Err(BindingError::BadMagic(first.to_string()));
        }

        let mut keys = KeyMap::new();
        let mut pads = PadMap::new();
        let mut warnings = Vec::new();
        let mut in_gamepad = false;

        for (index, raw) in lines {
            let line = raw.trim();
            let number = index + 1;
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match line {
                "[Keyboard]" => {
                    in_gamepad = false;
                    continue;
                }
                "[Gamepad]" => {
                    in_gamepad = true;
                    continue;
                }
                _ => {}
            }

            let Some((name, value)) = line.split_once('=') else {
                warnings.push(BindingWarning::Malformed {
                    line: number,
                    text: line.to_string(),
                });
                continue;
            };

            if in_gamepad {
                match (PadButton::from_name(name), NesButton::from_name(value)) {
                    (Some(pad_button), Some(button)) => pads.bind(pad_button, button),
                    (None, _) => warnings.push(BindingWarning::UnknownName {
                        line: number,
                        name: name.to_string(),
                    }),
                    (_, None) => warnings.push(BindingWarning::UnknownName {
                        line: number,
                        name: value.to_string(),
                    }),
                }
                continue;
            }

            let Some((port_text, button_name)) = value.split_once(':') else {
                warnings.push(BindingWarning::Malformed {
                    line: number,
                    text: line.to_string(),
                });
                continue;
            };
            let Ok(port) = port_text.parse::<usize>() else {
                warnings.push(BindingWarning::BadPort {
                    line: number,
                    port: port_text.to_string(),
                });
                continue;
            };
            if port >= rf_core_api::MAX_INPUT_PORTS {
                warnings.push(BindingWarning::BadPort {
                    line: number,
                    port: port_text.to_string(),
                });
                continue;
            }
            match (Key::from_name(name), NesButton::from_name(button_name)) {
                (Some(key), Some(button)) => keys.bind(key, port, button),
                (None, _) => warnings.push(BindingWarning::UnknownName {
                    line: number,
                    name: name.to_string(),
                }),
                (_, None) => warnings.push(BindingWarning::UnknownName {
                    line: number,
                    name: button_name.to_string(),
                }),
            }
        }

        Ok((Self { keys, pads }, warnings))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_round_trip_exactly() {
        let original = Bindings::default();
        let text = original.to_text();
        let (parsed, warnings) = Bindings::from_text(&text).expect("own output parses");
        assert!(warnings.is_empty(), "own output warned: {warnings:?}");
        assert_eq!(parsed.to_text(), text, "round trip must be byte-identical");
    }

    /// The persistence criterion in its smallest honest form: a remap made
    /// in one session is the binding in the next.
    #[test]
    fn a_remap_survives_a_save_and_load() {
        let mut bindings = Bindings::default();
        bindings.keys.bind(Key::A, 1, NesButton::Start);
        bindings.pads.bind(PadButton::North, NesButton::Select);

        let text = bindings.to_text();
        let (loaded, warnings) = Bindings::from_text(&text).expect("parses");
        assert!(warnings.is_empty());
        assert_eq!(loaded.keys.lookup(Key::A), Some((1, NesButton::Start)));
        assert_eq!(
            loaded.pads.lookup(PadButton::North),
            Some(NesButton::Select)
        );
    }

    #[test]
    fn a_file_with_the_wrong_magic_is_refused() {
        let err = Bindings::from_text("# just some file\nArrowUp=0:Up\n")
            .expect_err("a non-binding file must be refused");
        assert!(matches!(err, BindingError::BadMagic(_)));
        assert!(format!("{err}").contains("RFBIND 1"));
    }

    /// Forgiving-on-load, and the reason: one stale line must not cost the
    /// user every other binding they have.
    #[test]
    fn unknown_names_are_skipped_with_a_warning_and_everything_else_still_loads() {
        let text = "RFBIND 1\n\
                    [Keyboard]\n\
                    ArrowUp=0:Up\n\
                    HyperKey=0:Up\n\
                    Z=0:Nonsense\n\
                    [Gamepad]\n\
                    South=A\n\
                    Paddle=A\n";
        let (bindings, warnings) = Bindings::from_text(text).expect("parses");

        assert_eq!(bindings.keys.lookup(Key::ArrowUp), Some((0, NesButton::Up)));
        assert_eq!(bindings.pads.lookup(PadButton::South), Some(NesButton::A));
        assert_eq!(
            warnings.len(),
            3,
            "each bad line warns exactly once: {warnings:?}"
        );
        assert!(warnings
            .iter()
            .all(|w| matches!(w, BindingWarning::UnknownName { .. })));
    }

    #[test]
    fn malformed_lines_and_bad_ports_are_reported_precisely() {
        let text = "RFBIND 1\n\
                    [Keyboard]\n\
                    this line has no equals\n\
                    ArrowUp=nocolon\n\
                    ArrowDown=9:Down\n\
                    ArrowLeft=x:Left\n";
        let (bindings, warnings) = Bindings::from_text(text).expect("parses");
        assert!(
            bindings.keys.entries().is_empty(),
            "nothing valid was present"
        );
        assert_eq!(warnings.len(), 4);
        assert!(matches!(
            warnings[0],
            BindingWarning::Malformed { line: 3, .. }
        ));
        assert!(matches!(
            warnings[1],
            BindingWarning::Malformed { line: 4, .. }
        ));
        assert!(matches!(
            warnings[2],
            BindingWarning::BadPort { line: 5, .. }
        ));
        assert!(matches!(
            warnings[3],
            BindingWarning::BadPort { line: 6, .. }
        ));
    }

    /// An empty-but-valid file means "no bindings", not "give me the
    /// defaults" — see [`Bindings::from_text`]'s doc.
    #[test]
    fn an_empty_file_parses_to_empty_bindings_not_to_the_defaults() {
        let (bindings, warnings) = Bindings::from_text("RFBIND 1\n").expect("parses");
        assert!(warnings.is_empty());
        assert!(bindings.keys.entries().is_empty());
        assert!(bindings.pads.entries().is_empty());
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let text = "RFBIND 1\n\n# player one\n[Keyboard]\n\nArrowUp=0:Up\n";
        let (bindings, warnings) = Bindings::from_text(text).expect("parses");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(bindings.keys.lookup(Key::ArrowUp), Some((0, NesButton::Up)));
    }

    #[test]
    fn the_serialized_form_uses_lf_only() {
        let text = Bindings::default().to_text();
        assert!(!text.contains('\r'), "CRLF would break diffability");
    }
}
