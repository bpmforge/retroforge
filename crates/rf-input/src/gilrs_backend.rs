//! The `gilrs` implementation of [`PadBackend`] (ticket W2-06), behind the
//! `gilrs` feature.
//!
//! ## Why it is optional, and what that costs
//!
//! `gilrs` reaches for evdev/udev on Linux; the CI runner installs only
//! cc65, so a default-on dependency would turn CI red on a change nobody
//! could verify from a dev machine — the same call W2-05 made for `cpal`.
//! Everything above this file (the pad model, port routing, hotplug rules,
//! bindings, persistence) is compiled and tested unconditionally, because
//! it is all behind [`PadBackend`]. What the ordinary gate does **not**
//! cover is this file: the translation table below and the axis threshold
//! are exercised by running the app with `--features gilrs`, not by a test.
//!
//! ## Axis-to-direction thresholding lives here
//!
//! An NES d-pad is digital and a stick is not, so somewhere a threshold has
//! to exist. Putting it at the backend edge (rather than in the mapping
//! layer) means there is exactly one definition of "pushed far enough", and
//! the layers above never see an analog value at all — see [`PadButton`]'s
//! doc.

use gilrs::{Axis, Button, EventType, Gilrs};

use crate::pad::{PadBackend, PadButton, PadEvent, PadId};

/// How far a stick must move before it counts as a direction press, and how
/// far it must come back before it counts as a release.
///
/// Two thresholds, not one: a single threshold makes a stick resting near
/// it chatter press/release at the poll rate, which in a game reads as the
/// d-pad stuttering. The gap is deliberately wide (0.5 in, 0.35 out)
/// because precision is worthless here — the output is one bit.
const AXIS_PRESS: f32 = 0.5;
const AXIS_RELEASE: f32 = 0.35;

/// A live `gilrs` context, adapted to [`PadBackend`].
pub struct GilrsBackend {
    gilrs: Gilrs,
    /// Which stick directions are currently "pressed", so the hysteresis
    /// above has something to compare against.
    stick_held: Vec<(PadId, PadButton)>,
}

impl GilrsBackend {
    /// Open the gamepad subsystem.
    ///
    /// # Errors
    /// Returns `gilrs`'s own error string if the platform backend cannot
    /// start (no udev, no permissions, ...). A caller should carry on
    /// without pads rather than refuse to run.
    pub fn new() -> Result<Self, String> {
        let gilrs = Gilrs::new().map_err(|e| e.to_string())?;
        Ok(Self {
            gilrs,
            stick_held: Vec::new(),
        })
    }

    /// Pads `gilrs` already knows about at startup, as `Connected` events —
    /// without this, a pad plugged in *before* the app started would never
    /// be announced, and "hotplug works" would be true only for pads
    /// connected late.
    #[must_use]
    pub fn initial_connections(&self) -> Vec<PadEvent> {
        self.gilrs
            .gamepads()
            .map(|(id, _)| PadEvent::Connected(pad_id(id)))
            .collect()
    }

    /// Turn one axis value into press/release events, with hysteresis.
    fn axis_events(&mut self, id: PadId, axis: Axis, value: f32) -> Vec<PadEvent> {
        let (negative, positive) = match axis {
            Axis::LeftStickX => (PadButton::LeftStickLeft, PadButton::LeftStickRight),
            Axis::LeftStickY => (PadButton::LeftStickDown, PadButton::LeftStickUp),
            _ => return Vec::new(),
        };
        let mut events = Vec::new();
        for (button, magnitude) in [(negative, -value), (positive, value)] {
            let held = self.stick_held.contains(&(id, button));
            if !held && magnitude >= AXIS_PRESS {
                self.stick_held.push((id, button));
                events.push(PadEvent::ButtonDown(id, button));
            } else if held && magnitude < AXIS_RELEASE {
                self.stick_held.retain(|entry| *entry != (id, button));
                events.push(PadEvent::ButtonUp(id, button));
            }
        }
        events
    }
}

impl PadBackend for GilrsBackend {
    fn poll(&mut self) -> Vec<PadEvent> {
        let mut out = Vec::new();
        while let Some(event) = self.gilrs.next_event() {
            let id = pad_id(event.id);
            match event.event {
                EventType::Connected => out.push(PadEvent::Connected(id)),
                EventType::Disconnected => {
                    self.stick_held.retain(|(held, _)| *held != id);
                    out.push(PadEvent::Disconnected(id));
                }
                EventType::ButtonPressed(button, _) => {
                    if let Some(mapped) = map_button(button) {
                        out.push(PadEvent::ButtonDown(id, mapped));
                    }
                }
                EventType::ButtonReleased(button, _) => {
                    if let Some(mapped) = map_button(button) {
                        out.push(PadEvent::ButtonUp(id, mapped));
                    }
                }
                EventType::AxisChanged(axis, value, _) => {
                    out.extend(self.axis_events(id, axis, value));
                }
                _ => {}
            }
        }
        out
    }
}

fn pad_id(id: gilrs::GamepadId) -> PadId {
    // `GamepadId` is opaque and only `Display`/`Debug`; its usize form is
    // stable within a session, which is all `PadId` promises.
    PadId(usize::from(id) as u64)
}

/// `gilrs`'s standard-gamepad buttons to this crate's. Everything an NES
/// cannot use maps to `None` rather than to a near-miss button.
fn map_button(button: Button) -> Option<PadButton> {
    Some(match button {
        Button::DPadUp => PadButton::DpadUp,
        Button::DPadDown => PadButton::DpadDown,
        Button::DPadLeft => PadButton::DpadLeft,
        Button::DPadRight => PadButton::DpadRight,
        Button::South => PadButton::South,
        Button::East => PadButton::East,
        Button::West => PadButton::West,
        Button::North => PadButton::North,
        Button::LeftTrigger => PadButton::LeftShoulder,
        Button::RightTrigger => PadButton::RightShoulder,
        Button::Start => PadButton::Start,
        Button::Select => PadButton::Select,
        // gilrs `Mode` is the SDL "guide" button (gilrs-0.11.2
        // src/ev/mod.rs:133, `Mode = BTN_MODE`).
        Button::Mode => PadButton::Guide,
        _ => return None,
    })
}
