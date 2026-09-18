//! Gamepad navigation of the shell's UI (ticket W8-04;
//! `docs/design/FRONTEND_UI.md` §4's "Gamepad navigation of Library/Play
//! overlay is Phase 8").
//!
//! ## It drives egui's focus system rather than replacing it
//!
//! egui already has keyboard focus with a defined order, `Tab`/`Shift+Tab`
//! traversal, arrow-key movement and `Enter`/`Space` activation — and,
//! crucially, that focus is what AccessKit reports, so it is the thing a
//! screen reader and the headless harness both see.
//!
//! So this module translates pad input into `egui::Event`s and hands them
//! over. A parallel focus model would have been more code, would not have
//! appeared in the accessibility tree, and would have drifted from
//! keyboard behaviour the first time either changed.
//!
//! ## Auto-repeat is the part that needs care
//!
//! A d-pad held down must repeat, or navigating a long list means pressing
//! Down thirty times. But a naive "repeat every frame while held" makes a
//! list uncontrollable, and repeating from the very first frame makes a
//! single tap jump two entries. Hence the two-stage timing every UI uses:
//! one event on press, then nothing until [`REPEAT_DELAY`], then one per
//! [`REPEAT_INTERVAL`].
//!
//! ## Which buttons, and why South/East
//!
//! `South` activates and `East` goes back, matching the platform
//! convention `PadButton`'s own doc records (South = A on Xbox, Cross on
//! PlayStation). `Start` toggles the menu. The face buttons are
//! deliberately NOT remappable through the game bindings: §4 says hotkeys
//! are "namespaced so game bindings and UI bindings can't clash", and
//! navigation is the clearest case of that — a pad that cannot escape a
//! menu because the user bound South to something else is a trap.

use std::time::Duration;

use eframe::egui;
use rf_input::{PadButton, PadEvent};

/// How long a direction must be held before it starts repeating.
pub const REPEAT_DELAY: Duration = Duration::from_millis(400);
/// The gap between repeats once repeating has started.
pub const REPEAT_INTERVAL: Duration = Duration::from_millis(90);

/// What a pad press means to the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavAction {
    Up,
    Down,
    Left,
    Right,
    /// Activate the focused widget.
    Activate,
    /// Close/step back out of the current surface.
    Back,
    /// Move focus forward / backward, for surfaces where a linear order
    /// is more natural than a spatial one.
    Next,
    Previous,
    /// Toggle the menu.
    Menu,
}

impl NavAction {
    /// The `egui::Event` this action becomes.
    ///
    /// `Back` and `Menu` deliberately have no event: they are decisions
    /// the shell makes about which window is open, not focus movement,
    /// and forging an `Escape` keypress for them would also reach any
    /// widget that happens to handle `Escape` itself.
    #[must_use]
    pub fn to_event(self) -> Option<egui::Event> {
        let key = match self {
            NavAction::Up => egui::Key::ArrowUp,
            NavAction::Down => egui::Key::ArrowDown,
            NavAction::Left => egui::Key::ArrowLeft,
            NavAction::Right => egui::Key::ArrowRight,
            NavAction::Activate => egui::Key::Enter,
            NavAction::Next | NavAction::Previous => egui::Key::Tab,
            NavAction::Back | NavAction::Menu => return None,
        };
        Some(egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: if self == NavAction::Previous {
                egui::Modifiers::SHIFT
            } else {
                egui::Modifiers::NONE
            },
        })
    }

    /// Map a pad button to a navigation action.
    ///
    /// Both the d-pad and the left stick navigate: a stick-only pad and a
    /// d-pad-only pad are both real, and treating one as second-class
    /// would make the UI unreachable on it.
    #[must_use]
    pub fn from_button(button: PadButton) -> Option<Self> {
        Some(match button {
            PadButton::DpadUp | PadButton::LeftStickUp => NavAction::Up,
            PadButton::DpadDown | PadButton::LeftStickDown => NavAction::Down,
            PadButton::DpadLeft | PadButton::LeftStickLeft => NavAction::Left,
            PadButton::DpadRight | PadButton::LeftStickRight => NavAction::Right,
            PadButton::South => NavAction::Activate,
            PadButton::East => NavAction::Back,
            PadButton::RightShoulder => NavAction::Next,
            PadButton::LeftShoulder => NavAction::Previous,
            PadButton::Start => NavAction::Menu,
            _ => return None,
        })
    }

    /// Does this action repeat while held?
    ///
    /// Directions do; activation does not. An auto-repeating `Activate`
    /// would fire a button many times from one press — the difference
    /// between scrolling a list and launching thirty games.
    #[must_use]
    pub fn repeats(self) -> bool {
        matches!(
            self,
            NavAction::Up | NavAction::Down | NavAction::Left | NavAction::Right
        )
    }
}

/// Which input device most recently produced UI activity (ticket W15-08,
/// `docs/design/UX_WAVE_15.md` §9). `app.rs` consults this in two places:
/// `apply_theme`'s larger type scale, and the thicker, stronger-accent
/// focus ring `library_cards`/`library_rows` draw on the selected item.
///
/// Live state only — never persisted (`plan.json` W15-08 acceptance 3
/// says so explicitly): this is "what is the player's hand on right
/// now", not a preference, so there is nothing here for a settings file
/// to remember across launches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputDevice {
    #[default]
    Mouse,
    Keyboard,
    Gamepad,
}

impl InputDevice {
    /// Resolve one frame's active device from three independent
    /// booleans: did the keyboard, the pointer, or a pad produce
    /// activity this frame.
    ///
    /// **Precedence when more than one fires the same frame: Gamepad >
    /// Keyboard > Mouse.** The pointer is the noisiest of the three
    /// signals (any pixel of movement counts) so it must never be able to
    /// mask a real key or pad press that landed the same frame; a pad
    /// button pressed while a hand still rests on the mouse is a
    /// deliberate device switch, not a coincidence to average away.
    ///
    /// When none of the three fired, `previous` stands — this is a
    /// *tracker*, not a per-frame snapshot that decays to some default
    /// the instant nothing happens. A quiet frame between two key
    /// presses must not flicker the type scale back down and up again.
    #[must_use]
    pub fn resolve(previous: Self, keyboard: bool, mouse: bool, gamepad: bool) -> Self {
        if gamepad {
            Self::Gamepad
        } else if keyboard {
            Self::Keyboard
        } else if mouse {
            Self::Mouse
        } else {
            previous
        }
    }
}

/// Tracks held directions and produces repeat events.
#[derive(Debug, Default)]
pub struct GamepadNav {
    /// The direction currently held, and how long since its last event.
    held: Option<(NavAction, Duration)>,
    /// Whether the initial delay has elapsed for the held direction.
    repeating: bool,
    /// Set while the UI is being driven by a pad, so the shell can show
    /// focus rings it would otherwise hide for mouse users.
    pub active: bool,
}

impl GamepadNav {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed pad events; returns the actions to apply this frame.
    pub fn on_events(&mut self, events: &[PadEvent]) -> Vec<NavAction> {
        let mut out = Vec::new();
        for event in events {
            match event {
                PadEvent::ButtonDown(_, button) => {
                    let Some(action) = NavAction::from_button(*button) else {
                        continue;
                    };
                    self.active = true;
                    out.push(action);
                    if action.repeats() {
                        self.held = Some((action, Duration::ZERO));
                        self.repeating = false;
                    }
                }
                PadEvent::ButtonUp(_, button) => {
                    if let Some(action) = NavAction::from_button(*button) {
                        if self.held.map(|(a, _)| a) == Some(action) {
                            self.held = None;
                            self.repeating = false;
                        }
                    }
                }
                // A pad vanishing mid-hold must not leave a direction
                // stuck repeating forever.
                PadEvent::Disconnected(_) => {
                    self.held = None;
                    self.repeating = false;
                }
                PadEvent::Connected(_) => {}
            }
        }
        out
    }

    /// Advance the repeat timer; returns any repeat actions due.
    pub fn tick(&mut self, dt: Duration) -> Vec<NavAction> {
        let Some((action, elapsed)) = self.held.as_mut() else {
            return Vec::new();
        };
        *elapsed += dt;
        let threshold = if self.repeating {
            REPEAT_INTERVAL
        } else {
            REPEAT_DELAY
        };
        let mut out = Vec::new();
        while *elapsed >= threshold {
            *elapsed -= threshold;
            self.repeating = true;
            out.push(*action);
            // After the first repeat the interval is the shorter one, so
            // stop draining against the delay.
            if threshold == REPEAT_DELAY {
                break;
            }
        }
        out
    }

    /// Translate actions into events for `egui::RawInput`.
    #[must_use]
    pub fn events_for(actions: &[NavAction]) -> Vec<egui::Event> {
        actions.iter().filter_map(|a| a.to_event()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_input::PadId;

    fn down(b: PadButton) -> PadEvent {
        PadEvent::ButtonDown(PadId(0), b)
    }
    fn up(b: PadButton) -> PadEvent {
        PadEvent::ButtonUp(PadId(0), b)
    }

    /// **Both the d-pad and the left stick navigate.** A stick-only pad
    /// and a d-pad-only pad are both real hardware; treating either as
    /// second-class makes the UI unreachable on it.
    #[test]
    fn the_dpad_and_the_stick_both_navigate() {
        for (dpad, stick, want) in [
            (PadButton::DpadUp, PadButton::LeftStickUp, NavAction::Up),
            (
                PadButton::DpadDown,
                PadButton::LeftStickDown,
                NavAction::Down,
            ),
            (
                PadButton::DpadLeft,
                PadButton::LeftStickLeft,
                NavAction::Left,
            ),
            (
                PadButton::DpadRight,
                PadButton::LeftStickRight,
                NavAction::Right,
            ),
        ] {
            assert_eq!(NavAction::from_button(dpad), Some(want));
            assert_eq!(NavAction::from_button(stick), Some(want));
        }
    }

    /// South activates, East goes back — the convention PadButton's own
    /// doc records.
    #[test]
    fn the_face_buttons_follow_the_platform_convention() {
        assert_eq!(
            NavAction::from_button(PadButton::South),
            Some(NavAction::Activate)
        );
        assert_eq!(
            NavAction::from_button(PadButton::East),
            Some(NavAction::Back)
        );
        assert_eq!(
            NavAction::from_button(PadButton::Start),
            Some(NavAction::Menu)
        );
        // A button with no navigation meaning must not invent one.
        assert_eq!(NavAction::from_button(PadButton::North), None);
    }

    /// **Activation must not auto-repeat.** The difference between
    /// scrolling a list and launching thirty games.
    #[test]
    fn directions_repeat_and_activation_does_not() {
        assert!(NavAction::Up.repeats());
        assert!(NavAction::Down.repeats());
        assert!(!NavAction::Activate.repeats());
        assert!(!NavAction::Back.repeats());
        assert!(!NavAction::Menu.repeats());

        let mut nav = GamepadNav::new();
        assert_eq!(
            nav.on_events(&[down(PadButton::South)]),
            vec![NavAction::Activate]
        );
        // Holding it for a long time must produce nothing more.
        assert!(nav.tick(Duration::from_secs(5)).is_empty());
    }

    /// One press yields exactly ONE event, then silence until the delay.
    ///
    /// Repeating from the first frame makes a single tap jump two
    /// entries — the classic feel bug.
    #[test]
    fn a_press_fires_once_then_waits_for_the_repeat_delay() {
        let mut nav = GamepadNav::new();
        assert_eq!(
            nav.on_events(&[down(PadButton::DpadDown)]),
            vec![NavAction::Down]
        );

        // Just short of the delay: nothing.
        assert!(nav.tick(REPEAT_DELAY - Duration::from_millis(1)).is_empty());
        // Crossing it: exactly one repeat.
        assert_eq!(nav.tick(Duration::from_millis(2)), vec![NavAction::Down]);
    }

    /// Once repeating, the shorter interval applies.
    #[test]
    fn repeats_accelerate_after_the_initial_delay() {
        let mut nav = GamepadNav::new();
        nav.on_events(&[down(PadButton::DpadDown)]);
        nav.tick(REPEAT_DELAY);
        // Three intervals should now produce three repeats.
        let out = nav.tick(REPEAT_INTERVAL * 3);
        assert_eq!(out.len(), 3, "got {out:?}");
    }

    /// Releasing stops the repeat.
    #[test]
    fn releasing_stops_the_repeat() {
        let mut nav = GamepadNav::new();
        nav.on_events(&[down(PadButton::DpadDown)]);
        nav.on_events(&[up(PadButton::DpadDown)]);
        assert!(nav.tick(Duration::from_secs(5)).is_empty());
    }

    /// **A pad unplugged mid-hold must not leave a direction stuck.**
    /// Otherwise the UI scrolls forever with no hardware attached.
    #[test]
    fn disconnecting_mid_hold_clears_the_held_direction() {
        let mut nav = GamepadNav::new();
        nav.on_events(&[down(PadButton::DpadDown)]);
        nav.on_events(&[PadEvent::Disconnected(PadId(0))]);
        assert!(
            nav.tick(Duration::from_secs(5)).is_empty(),
            "an unplugged pad must not keep scrolling the UI"
        );
    }

    /// Releasing a DIFFERENT direction must not cancel the held one.
    #[test]
    fn releasing_another_direction_does_not_cancel_the_held_one() {
        let mut nav = GamepadNav::new();
        nav.on_events(&[down(PadButton::DpadDown)]);
        nav.on_events(&[up(PadButton::DpadLeft)]);
        assert_eq!(nav.tick(REPEAT_DELAY), vec![NavAction::Down]);
    }

    /// Navigation becomes egui events, so egui's own focus system — the
    /// one AccessKit reports — does the moving.
    #[test]
    fn navigation_becomes_egui_key_events() {
        let events = GamepadNav::events_for(&[
            NavAction::Down,
            NavAction::Activate,
            NavAction::Next,
            NavAction::Previous,
        ]);
        assert_eq!(events.len(), 4);
        assert!(matches!(
            events[0],
            egui::Event::Key {
                key: egui::Key::ArrowDown,
                pressed: true,
                ..
            }
        ));
        assert!(matches!(
            events[1],
            egui::Event::Key {
                key: egui::Key::Enter,
                ..
            }
        ));
        // Previous is Shift+Tab, which is how egui walks focus backwards.
        assert!(matches!(
            events[3],
            egui::Event::Key {
                key: egui::Key::Tab,
                modifiers: egui::Modifiers { shift: true, .. },
                ..
            }
        ));
    }

    /// `Back` and `Menu` produce NO key event: they are decisions about
    /// which window is open, and forging an Escape would also reach any
    /// widget that handles Escape itself.
    #[test]
    fn back_and_menu_are_not_forged_key_presses() {
        assert!(NavAction::Back.to_event().is_none());
        assert!(NavAction::Menu.to_event().is_none());
        assert!(GamepadNav::events_for(&[NavAction::Back, NavAction::Menu]).is_empty());
    }

    /// The shell needs to know a pad is driving, so it can show focus
    /// rings it hides for mouse users.
    #[test]
    fn pad_input_marks_navigation_active() {
        let mut nav = GamepadNav::new();
        assert!(!nav.active);
        nav.on_events(&[down(PadButton::DpadUp)]);
        assert!(nav.active);
    }

    /// The device tracker defaults to Mouse — a fresh app has never seen
    /// input from anything, and Mouse is the least surprising rest state
    /// (the ordinary type scale, the thin ring).
    #[test]
    fn input_device_defaults_to_mouse() {
        assert_eq!(InputDevice::default(), InputDevice::Mouse);
    }

    /// Each signal alone moves the tracker to its own device.
    #[test]
    fn each_signal_alone_selects_its_own_device() {
        assert_eq!(
            InputDevice::resolve(InputDevice::Mouse, true, false, false),
            InputDevice::Keyboard
        );
        assert_eq!(
            InputDevice::resolve(InputDevice::Keyboard, false, true, false),
            InputDevice::Mouse
        );
        assert_eq!(
            InputDevice::resolve(InputDevice::Mouse, false, false, true),
            InputDevice::Gamepad
        );
    }

    /// **Gamepad beats keyboard and mouse when more than one fires the
    /// same frame** — a deliberate device switch must never be masked by
    /// ambient pointer movement or a stray key.
    #[test]
    fn gamepad_wins_when_everything_fires_at_once() {
        assert_eq!(
            InputDevice::resolve(InputDevice::Mouse, true, true, true),
            InputDevice::Gamepad
        );
    }

    /// Keyboard beats mouse when both fire without a pad.
    #[test]
    fn keyboard_wins_over_mouse() {
        assert_eq!(
            InputDevice::resolve(InputDevice::Mouse, true, true, false),
            InputDevice::Keyboard
        );
    }

    /// **A quiet frame keeps the previous device** — the tracker must not
    /// decay to some default the instant nothing happens, or the type
    /// scale/ring would flicker between every pair of key presses.
    #[test]
    fn no_activity_keeps_the_previous_device() {
        assert_eq!(
            InputDevice::resolve(InputDevice::Gamepad, false, false, false),
            InputDevice::Gamepad
        );
        assert_eq!(
            InputDevice::resolve(InputDevice::Keyboard, false, false, false),
            InputDevice::Keyboard
        );
    }
}
