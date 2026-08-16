//! Host-agnostic key identifiers (ticket W1-07, widened by W2-06).
//!
//! Deliberately **not** a full keyboard enumeration, and still host-agnostic
//! — this crate has no `egui`/`eframe` dependency (`scripts/validate-arch.sh`
//! rule 3; `crates/retroforge/src/input_map.rs` owns the translation from
//! `egui::Key`). W1-07 needed exactly the eight keys
//! [`crate::KeyMap::default_nes`] binds and said so; W2-06's remap UI needs
//! whatever a user might reasonably bind, which is the set below: letters,
//! digits, arrows, the common modifiers and the punctuation keys that exist
//! on every layout.
//!
//! ## One list, three uses
//!
//! The enum, [`Key::name`], [`Key::from_name`] and [`Key::ALL`] are
//! generated from a single macro invocation. They cannot drift apart, which
//! matters because the names are a **persisted format**: they appear in
//! every saved binding file (`crate::bindings`), so a name that disagreed
//! with its variant would silently drop that binding on load.

macro_rules! keys {
    ($($variant:ident => $name:literal),* $(,)?) => {
        /// A host key, identified by name rather than by scancode: bindings
        /// are saved as text and must survive a keyboard-layout change.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Key {
            $($variant),*
        }

        impl Key {
            /// Every key this build knows, in declaration order — the order
            /// a remap UI lists them in.
            pub const ALL: &'static [Key] = &[$(Key::$variant),*];

            /// The stable name used in saved bindings and in the UI.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Key::$variant => $name),*
                }
            }

            /// Inverse of [`Key::name`]; `None` for a name this build does
            /// not know (a config written by a newer build, say — which
            /// `crate::bindings` skips with a warning rather than refusing).
            #[must_use]
            pub fn from_name(name: &str) -> Option<Self> {
                match name {
                    $($name => Some(Key::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

keys! {
    ArrowUp => "ArrowUp",
    ArrowDown => "ArrowDown",
    ArrowLeft => "ArrowLeft",
    ArrowRight => "ArrowRight",
    A => "A",
    B => "B",
    C => "C",
    D => "D",
    E => "E",
    F => "F",
    G => "G",
    H => "H",
    I => "I",
    J => "J",
    K => "K",
    L => "L",
    M => "M",
    N => "N",
    O => "O",
    P => "P",
    Q => "Q",
    R => "R",
    S => "S",
    T => "T",
    U => "U",
    V => "V",
    W => "W",
    X => "X",
    Y => "Y",
    Z => "Z",
    Num0 => "0",
    Num1 => "1",
    Num2 => "2",
    Num3 => "3",
    Num4 => "4",
    Num5 => "5",
    Num6 => "6",
    Num7 => "7",
    Num8 => "8",
    Num9 => "9",
    Space => "Space",
    Enter => "Enter",
    Tab => "Tab",
    Backspace => "Backspace",
    Escape => "Escape",
    LeftShift => "LeftShift",
    RightShift => "RightShift",
    LeftCtrl => "LeftCtrl",
    RightCtrl => "RightCtrl",
    LeftAlt => "LeftAlt",
    RightAlt => "RightAlt",
    Comma => "Comma",
    Period => "Period",
    Slash => "Slash",
    Semicolon => "Semicolon",
    Quote => "Quote",
    LeftBracket => "LeftBracket",
    RightBracket => "RightBracket",
    Minus => "Minus",
    Equals => "Equals",
    Backslash => "Backslash",
    Backtick => "Backtick",
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The names are a persisted format, so every one must survive the
    /// round trip — a variant whose name did not parse back would drop that
    /// binding from every saved config, silently.
    #[test]
    fn every_key_name_round_trips() {
        for &key in Key::ALL {
            assert_eq!(Key::from_name(key.name()), Some(key), "{key:?}");
        }
    }

    #[test]
    fn names_are_unique() {
        let mut names: Vec<&str> = Key::ALL.iter().map(|k| k.name()).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "two keys share a name");
    }

    #[test]
    fn an_unknown_name_is_none_rather_than_a_wrong_key() {
        assert_eq!(Key::from_name("HyperKey"), None);
        assert_eq!(Key::from_name(""), None);
        assert_eq!(Key::from_name("arrowup"), None, "names are case-sensitive");
    }

    /// The eight keys W1-07's default binding uses must still exist, since
    /// widening the enum is exactly the change that could have renamed one.
    #[test]
    fn the_original_eight_keys_are_unchanged() {
        for name in [
            "ArrowUp",
            "ArrowDown",
            "ArrowLeft",
            "ArrowRight",
            "Z",
            "X",
            "Enter",
            "RightShift",
        ] {
            assert!(Key::from_name(name).is_some(), "{name} disappeared");
        }
    }
}
