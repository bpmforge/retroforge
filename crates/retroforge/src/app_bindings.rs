//! App-level hotkeys (ticket W15-06; `docs/design/UX_WAVE_15.md` §6):
//! save state, load state, fast-forward, screenshot and hold-to-peek.
//!
//! ## Why this is a separate namespace, not another `rf_input::KeyMap`
//!
//! §6 requires the App section to be "namespaced apart from game
//! bindings so the two can never collide", but this ticket's write scope
//! is `crates/retroforge/**` — `rf_input::KeyMap`/`PadMap`/`Bindings`
//! (crates/rf-input) are out of reach, and `rf_input::Key` has no
//! function-key variants at all (verified against
//! `crates/rf-input/src/key.rs`'s macro table: letters, digits, arrows,
//! punctuation and modifiers, nothing past `Tab`), so F5/F9/F12 cannot be
//! named in that enum without editing a crate this ticket may not touch.
//!
//! The fix is not to fight the boundary: [`AppAction`] is bound directly
//! to `egui::Key` (this crate already depends on `egui` everywhere —
//! `crate::input_map`'s "two modules allowed to depend on egui" doc is
//! about keeping *`rf-input`* egui-free, not about restricting this
//! crate), stored in its own small map, and persisted to its own file
//! (`app_hotkeys.rfbind`, `crate::bindings_store::app_bindings_path`) —
//! never as a section inside `bindings.rfbind`, since `rf_input::Bindings
//! ::from_text` does not know an `[App]` header and would report every
//! line under it as `Malformed`.
//!
//! "Cannot collide" is therefore a runtime check both remap flows run,
//! not a structural guarantee the type system gives for free:
//! [`key_conflicts_with_game`]/[`pad_conflicts_with_game`] guard the App
//! remap flow (`crate::app::RetroForgeApp::controls_window`'s App
//! section), and [`game_key_conflicts_with_app`]/
//! [`game_pad_conflicts_with_app`] guard the reverse direction
//! (`crate::app::RetroForgeApp::poll_input`'s existing capture flow,
//! before it calls `KeyMap::rebind`/`PadMap::bind`). Both directions go
//! through `crate::input_map::map_key`/`egui_key_for` — the one seam that
//! already translates between the two crates' key types — so an App key
//! that cannot even reach the game keymap (an F-key, `Tab`, `` ` ``) can
//! never collide, which is exactly why those are safe defaults.
//!
//! No conflict UI existed anywhere in this crate before this ticket (a
//! full-crate grep for "conflict" found none) — game-to-game rebinding
//! has always silently displaced the old key
//! (`rf_input::KeyMap::rebind`'s own doc). This module does not change
//! that; it only adds the App-vs-game check §6 and the ticket brief
//! require, surfaced inline the same way this ticket introduces for the
//! App section itself.

use eframe::egui;

use crate::input_map;

/// The five hotkeys `docs/design/UX_WAVE_15.md` §6 names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AppAction {
    SaveState,
    LoadState,
    FastForward,
    Screenshot,
    HoldToPeek,
    /// Ticket W20-04: borderless fullscreen on/off.
    Fullscreen,
}

impl AppAction {
    /// Declaration order — also the order the Controls window and the
    /// overlay menu list them in, matching §6's table.
    pub const ALL: [AppAction; 6] = [
        AppAction::SaveState,
        AppAction::LoadState,
        AppAction::FastForward,
        AppAction::Screenshot,
        AppAction::HoldToPeek,
        AppAction::Fullscreen,
    ];

    /// The actions a version-1 file (before W20-04) could know about —
    /// what lets [`AppBindings::from_text`] tell "the user unbound this"
    /// from "this action did not exist when the file was written".
    const V1_ACTIONS: [AppAction; 5] = [
        AppAction::SaveState,
        AppAction::LoadState,
        AppAction::FastForward,
        AppAction::Screenshot,
        AppAction::HoldToPeek,
    ];

    /// Stable name for the persisted file. A persisted format, like every
    /// other `name()` in this crate's binding stack — renaming a variant
    /// here must not be done without a matching rename here.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            AppAction::SaveState => "SaveState",
            AppAction::LoadState => "LoadState",
            AppAction::FastForward => "FastForward",
            AppAction::Screenshot => "Screenshot",
            AppAction::HoldToPeek => "HoldToPeek",
            AppAction::Fullscreen => "Fullscreen",
        }
    }

    /// Inverse of [`AppAction::name`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        AppAction::ALL.into_iter().find(|a| a.name() == name)
    }

    /// Display label for the Controls window and the overlay menu — §6's
    /// table column, not the persisted name.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            AppAction::SaveState => "Save state",
            AppAction::LoadState => "Load state",
            AppAction::FastForward => "Fast-forward (hold)",
            AppAction::Screenshot => "Screenshot",
            AppAction::HoldToPeek => "Hold-to-peek",
            AppAction::Fullscreen => "Fullscreen",
        }
    }
}

/// Magic + version line, same shape as `rf_input::bindings::Bindings` —
/// a per-user config, not a `docs/design/CONTRACTS.md` format, so the
/// version exists to let a future change refuse an old file politely,
/// not to promise external readers anything.
const MAGIC: &str = "RFAPPBIND 2";
/// The pre-W20-04 header, still accepted on load.
const MAGIC_V1: &str = "RFAPPBIND 1";

/// App-namespaced key/pad bindings. See the module doc for why this is
/// not `rf_input::KeyMap`/`PadMap`.
#[derive(Debug, Clone)]
pub struct AppBindings {
    keys: Vec<(egui::Key, AppAction)>,
    pads: Vec<(rf_input::PadButton, AppAction)>,
}

impl Default for AppBindings {
    /// §6's defaults: F5/F9/Tab/F12/Backtick, no pad defaults (every row
    /// in §6's gamepad column reads "— (remappable)").
    fn default() -> Self {
        let mut b = AppBindings {
            keys: Vec::new(),
            pads: Vec::new(),
        };
        b.bind_key(egui::Key::F5, AppAction::SaveState);
        b.bind_key(egui::Key::F9, AppAction::LoadState);
        b.bind_key(egui::Key::Tab, AppAction::FastForward);
        b.bind_key(egui::Key::F12, AppAction::Screenshot);
        b.bind_key(egui::Key::Backtick, AppAction::HoldToPeek);
        b.bind_key(egui::Key::F11, AppAction::Fullscreen);
        b
    }
}

impl AppBindings {
    /// Bind `key` to `action` **as a remap**: any other key previously
    /// driving `action` is unbound, and any other action `key` used to
    /// drive is unbound too — the same two-way guarantee
    /// `rf_input::KeyMap::rebind` documents (one key drives at most one
    /// action, one action is driven by at most one key), applied to this
    /// namespace's own table.
    pub fn bind_key(&mut self, key: egui::Key, action: AppAction) {
        self.keys.retain(|(k, a)| *k != key && *a != action);
        self.keys.push((key, action));
    }

    /// Remove whatever key drives `action`, if any.
    pub fn unbind_key_for(&mut self, action: AppAction) {
        self.keys.retain(|(_, a)| *a != action);
    }

    #[must_use]
    pub fn key_for(&self, action: AppAction) -> Option<egui::Key> {
        self.keys
            .iter()
            .find(|(_, a)| *a == action)
            .map(|(k, _)| *k)
    }

    #[must_use]
    pub fn action_for_key(&self, key: egui::Key) -> Option<AppAction> {
        self.keys.iter().find(|(k, _)| *k == key).map(|(_, a)| *a)
    }

    /// Same remap guarantee as [`AppBindings::bind_key`], for pads.
    pub fn bind_pad(&mut self, button: rf_input::PadButton, action: AppAction) {
        self.pads.retain(|(b, a)| *b != button && *a != action);
        self.pads.push((button, action));
    }

    pub fn unbind_pad_for(&mut self, action: AppAction) {
        self.pads.retain(|(_, a)| *a != action);
    }

    #[must_use]
    pub fn pad_for(&self, action: AppAction) -> Option<rf_input::PadButton> {
        self.pads
            .iter()
            .find(|(_, a)| *a == action)
            .map(|(b, _)| *b)
    }

    #[must_use]
    pub fn action_for_pad(&self, button: rf_input::PadButton) -> Option<AppAction> {
        self.pads
            .iter()
            .find(|(b, _)| *b == button)
            .map(|(_, a)| *a)
    }

    /// Every keyboard binding, in insertion order — for the Controls
    /// window and for saving.
    #[must_use]
    pub fn keys(&self) -> &[(egui::Key, AppAction)] {
        &self.keys
    }

    /// Every pad binding, in insertion order.
    #[must_use]
    pub fn pads(&self) -> &[(rf_input::PadButton, AppAction)] {
        &self.pads
    }

    /// Serialize. Deterministic — sections in a fixed order, entries in
    /// binding order, LF endings — same reasoning as
    /// `rf_input::bindings::Bindings::to_text`.
    #[must_use]
    pub fn to_text(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::from(MAGIC);
        // Ticket W20-04: which actions this file was written knowing, so a
        // later build can give an action added since then its default
        // instead of reading its absence as "the user unbound it".
        let known: Vec<&str> = AppAction::ALL.iter().map(|a| a.name()).collect();
        let _ = write!(out, "\nknown={}", known.join(","));
        out.push_str("\n[Keyboard]\n");
        for (key, action) in &self.keys {
            let _ = writeln!(out, "{}={}", key.name(), action.name());
        }
        out.push_str("[Gamepad]\n");
        for (button, action) in &self.pads {
            let _ = writeln!(out, "{}={}", button.name(), action.name());
        }
        out
    }

    /// Parse, returning the bindings plus a warning line per entry this
    /// build could not make sense of — forgiving on load, same policy as
    /// `rf_input::bindings::Bindings::from_text`: a stale line costs that
    /// one binding, not the whole file.
    ///
    /// # Errors
    /// A string describing why, if the first line is not [`MAGIC`].
    pub fn from_text(text: &str) -> Result<(Self, Vec<String>), String> {
        let mut lines = text.lines();
        let first = lines.next().unwrap_or_default().trim();
        if first != MAGIC && first != MAGIC_V1 {
            return Err(format!(
                "not an app-hotkey file: expected first line {MAGIC:?}, found {first:?}"
            ));
        }
        // A v1 file knew exactly the original five; a v2 file says.
        let mut known: Vec<AppAction> = if first == MAGIC_V1 {
            AppAction::V1_ACTIONS.to_vec()
        } else {
            Vec::new()
        };
        let mut out = AppBindings {
            keys: Vec::new(),
            pads: Vec::new(),
        };
        let mut warnings = Vec::new();
        let mut in_gamepad = false;
        for (index, raw) in lines.enumerate() {
            let line = raw.trim();
            let number = index + 2; // +1 for 1-based, +1 for the magic line already consumed
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(list) = line.strip_prefix("known=") {
                known.extend(list.split(',').filter_map(AppAction::from_name));
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
                warnings.push(format!("line {number}: malformed ({line:?})"));
                continue;
            };
            let Some(action) = AppAction::from_name(value) else {
                warnings.push(format!("line {number}: unknown action {value:?}"));
                continue;
            };
            if in_gamepad {
                match rf_input::PadButton::from_name(name) {
                    Some(button) => out.bind_pad(button, action),
                    None => warnings.push(format!("line {number}: unknown pad button {name:?}")),
                }
            } else {
                match egui::Key::from_name(name) {
                    Some(key) => out.bind_key(key, action),
                    None => warnings.push(format!("line {number}: unknown key {name:?}")),
                }
            }
        }
        // Ticket W20-04: actions this file never heard of get their
        // default key — unless the user has since put that key to another
        // use, which wins.
        let defaults = AppBindings::default();
        for action in AppAction::ALL {
            if known.contains(&action) {
                continue;
            }
            if let Some(key) = defaults.key_for(action) {
                if out.action_for_key(key).is_none() {
                    out.bind_key(key, action);
                }
            }
        }
        Ok((out, warnings))
    }
}

/// Whether binding `key` in the App namespace would collide with a game
/// binding on any port. `None` when `key` is free to bind — either no
/// game action uses it, or (an F-key, say) it cannot even be expressed as
/// an `rf_input::Key` and so structurally never collides.
#[must_use]
pub fn key_conflicts_with_game(bindings: &rf_input::Bindings, key: egui::Key) -> Option<String> {
    let game_key = input_map::map_key(key)?;
    let (port, button) = bindings.keys.lookup(game_key)?;
    Some(format!(
        "{} is already bound to {} (Player {})",
        key.name(),
        button.name(),
        port + 1
    ))
}

/// Pad half of [`key_conflicts_with_game`].
#[must_use]
pub fn pad_conflicts_with_game(
    bindings: &rf_input::Bindings,
    button: rf_input::PadButton,
) -> Option<String> {
    let bound = bindings.pads.lookup(button)?;
    Some(format!(
        "{} is already bound to {} on the pad",
        button.name(),
        bound.name()
    ))
}

/// The reverse direction: whether binding a GAME action to `key` would
/// collide with an App hotkey. `crate::app::RetroForgeApp::poll_input`'s
/// remap-capture flow checks this before calling `KeyMap::rebind`.
#[must_use]
pub fn game_key_conflicts_with_app(
    app_bindings: &AppBindings,
    key: rf_input::Key,
) -> Option<String> {
    let egui_key = input_map::egui_key_for(key)?;
    let action = app_bindings.action_for_key(egui_key)?;
    Some(format!(
        "{} is already bound to {} (App)",
        key.name(),
        action.label()
    ))
}

/// Pad half of [`game_key_conflicts_with_app`].
#[must_use]
pub fn game_pad_conflicts_with_app(
    app_bindings: &AppBindings,
    button: rf_input::PadButton,
) -> Option<String> {
    let action = app_bindings.action_for_pad(button)?;
    Some(format!(
        "{} is already bound to {} (App)",
        button.name(),
        action.label()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_round_trip_through_to_text_and_from_text() {
        let original = AppBindings::default();
        let text = original.to_text();
        let (parsed, warnings) = AppBindings::from_text(&text).expect("own output parses");
        assert!(warnings.is_empty(), "own output warned: {warnings:?}");
        assert_eq!(parsed.to_text(), text, "round trip must be byte-identical");
    }

    #[test]
    fn every_default_action_has_a_key() {
        for action in AppAction::ALL {
            assert!(
                AppBindings::default().key_for(action).is_some(),
                "{action:?} has no default key"
            );
        }
    }

    #[test]
    fn a_remap_replaces_the_old_key_and_the_new_keys_old_job() {
        let mut bindings = AppBindings::default();
        // F5 (Save state) is remapped onto F9's key: F9 must stop driving
        // Load state, and Save state must stop being driven by the old F5.
        bindings.bind_key(egui::Key::F9, AppAction::SaveState);
        assert_eq!(bindings.key_for(AppAction::SaveState), Some(egui::Key::F9));
        assert_eq!(bindings.key_for(AppAction::LoadState), None);
        assert_eq!(
            bindings
                .keys()
                .iter()
                .filter(|(k, _)| *k == egui::Key::F9)
                .count(),
            1
        );
    }

    // ---- the collision rule the ticket exists for ---------------------

    #[test]
    fn an_app_default_key_that_reaches_the_game_keymap_is_rejected() {
        // Tab and Backtick are the two App defaults `rf_input::Key`
        // actually has a variant for (F5/F9/F12 do not exist in that
        // enum at all, so they can never collide structurally) — bind
        // them on the game side and confirm the App-side check catches it.
        let mut bindings = rf_input::Bindings::default();
        bindings
            .keys
            .bind(rf_input::Key::Tab, 0, rf_input::NesButton::Start);
        let msg = key_conflicts_with_game(&bindings, egui::Key::Tab)
            .expect("Tab is bound to a game action");
        assert!(msg.contains("Start"));
        assert!(msg.contains("Player 1"));

        bindings
            .keys
            .bind(rf_input::Key::Backtick, 1, rf_input::NesButton::Select);
        let msg = key_conflicts_with_game(&bindings, egui::Key::Backtick)
            .expect("Backtick is bound to a game action");
        assert!(msg.contains("Select"));
        assert!(msg.contains("Player 2"));
    }

    #[test]
    fn an_unbound_key_never_conflicts_with_the_game_keymap() {
        let bindings = rf_input::Bindings::default();
        // F5 cannot even be named as an rf_input::Key, so it must be
        // structurally impossible to conflict.
        assert_eq!(key_conflicts_with_game(&bindings, egui::Key::F5), None);
    }

    #[test]
    fn a_game_key_that_the_app_default_already_uses_is_rejected() {
        // The default AppBindings binds Tab to fast-forward, so a game
        // remap capturing Tab must be refused before it ever reaches
        // `KeyMap::rebind` — this is the "vice versa" half of criterion 2.
        let app_bindings = AppBindings::default();
        let msg = game_key_conflicts_with_app(&app_bindings, rf_input::Key::Tab)
            .expect("Tab is an App default");
        assert!(msg.contains("Fast-forward"));

        let msg = game_key_conflicts_with_app(&app_bindings, rf_input::Key::Backtick)
            .expect("Backtick is an App default");
        assert!(msg.contains("Hold-to-peek"));
    }

    #[test]
    fn a_game_key_the_app_never_touches_never_conflicts() {
        let app_bindings = AppBindings::default();
        assert_eq!(
            game_key_conflicts_with_app(&app_bindings, rf_input::Key::Z),
            None
        );
    }

    #[test]
    fn pad_conflicts_are_checked_both_directions() {
        let mut bindings = rf_input::Bindings::default();
        bindings.pads.bind(
            rf_input::PadButton::LeftShoulder,
            rf_input::NesButton::Select,
        );
        let msg = pad_conflicts_with_game(&bindings, rf_input::PadButton::LeftShoulder)
            .expect("bound on the game side");
        assert!(msg.contains("Select"));

        let mut app_bindings = AppBindings::default();
        app_bindings.bind_pad(rf_input::PadButton::RightShoulder, AppAction::Screenshot);
        let msg = game_pad_conflicts_with_app(&app_bindings, rf_input::PadButton::RightShoulder)
            .expect("bound on the App side");
        assert!(msg.contains("Screenshot"));

        // And the App side must refuse the same pad button the game
        // already uses.
        let msg = pad_conflicts_with_game(&bindings, rf_input::PadButton::LeftShoulder)
            .expect("still bound");
        assert!(msg.contains("Select"));
    }

    // ---- forgiving-on-load, mirroring rf_input::bindings' own policy --

    #[test]
    fn a_file_with_the_wrong_magic_is_refused() {
        let err = AppBindings::from_text("not an app-hotkey file\n").expect_err("must be refused");
        assert!(err.contains("RFAPPBIND 2"), "{err}");
    }

    #[test]
    fn unknown_names_are_skipped_with_a_warning_and_everything_else_still_loads() {
        let text = "RFAPPBIND 1\n\
                     [Keyboard]\n\
                     F5=SaveState\n\
                     HyperKey=LoadState\n\
                     F9=Nonsense\n\
                     [Gamepad]\n\
                     South=Screenshot\n";
        let (bindings, warnings) = AppBindings::from_text(text).expect("parses");
        assert_eq!(bindings.key_for(AppAction::SaveState), Some(egui::Key::F5));
        assert_eq!(
            bindings.pad_for(AppAction::Screenshot),
            Some(rf_input::PadButton::South)
        );
        assert_eq!(warnings.len(), 2, "{warnings:?}");
    }

    /// Ticket W20-04: a file from before Fullscreen existed gains its F11
    /// default, while an action the user deliberately unbound in a v2
    /// file stays unbound.
    #[test]
    fn new_actions_get_defaults_but_deliberate_unbinds_survive() {
        let v1 = "RFAPPBIND 1\n[Keyboard]\nF5=SaveState\n[Gamepad]\n";
        let (b, warnings) = AppBindings::from_text(v1).expect("v1 parses");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(b.key_for(AppAction::Fullscreen), Some(egui::Key::F11));
        // v1 listed no LoadState key: the user unbound it — stays unbound.
        assert_eq!(b.key_for(AppAction::LoadState), None);

        let mut mine = AppBindings::default();
        mine.unbind_key_for(AppAction::Fullscreen);
        let (round, _) = AppBindings::from_text(&mine.to_text()).expect("v2 parses");
        assert_eq!(round.key_for(AppAction::Fullscreen), None);
    }
}
