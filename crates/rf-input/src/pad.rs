//! Gamepad input: a host-agnostic pad model, port routing and hotplug
//! (ticket W2-06, FR-FE-003).
//!
//! ## Why there is a trait here at all
//!
//! `docs/TECH_STACK.md`'s gamepad row says to "wrap in thin adapter trait
//! (SDL3 swap stays cheap)". [`PadBackend`] is that wrapper, and it buys two
//! things beyond a possible future backend swap:
//!
//! - **`gilrs` stays out of the default build.** It reaches for evdev/udev
//!   on Linux, which the CI runner does not install — the same constraint
//!   that put `cpal` behind a feature in W2-05. The backend lives behind
//!   `--features gilrs`; everything in this module is compiled and tested
//!   unconditionally.
//! - **Hotplug becomes mechanically testable.** "gilrs devices hotplug" is
//!   an acceptance criterion, and a criterion that can only be checked by
//!   unplugging a physical controller is a criterion nobody checks. With
//!   the trait, [`PadRouter`]'s tests script connect/disconnect/reconnect
//!   sequences directly (see this module's tests) — the same discipline
//!   `rf-audio`'s rate loop gets.
//!
//! ## Port assignment, and why it is sticky
//!
//! A pad claims the lowest free port when it connects and **keeps it until
//! it disconnects**. The alternative — renumbering ports whenever the set
//! of connected pads changes — means unplugging player 1's controller
//! silently promotes player 2 mid-game. Sticky assignment also makes
//! reconnection predictable: a pad that drops out and comes back takes the
//! lowest free port, which is its old one if nobody took it meanwhile.

use rf_core_api::{InputFrame, MAX_INPUT_PORTS};

use crate::NesButton;

/// A physical pad, as identified by the backend. Opaque on purpose: `gilrs`
/// ids, SDL instance ids and test ids are all just numbers, and nothing
/// above this module should care which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PadId(pub u64);

/// The abstract buttons a pad can report — the SDL/`gilrs` "standard
/// gamepad" set, minus everything an NES cannot use.
///
/// Analog sticks arrive here already thresholded into directions by the
/// backend: an NES d-pad is digital, so a stick has to become one somewhere,
/// and doing it at the edge keeps a single definition of "pushed far enough"
/// rather than scattering thresholds through the mapping layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PadButton {
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
    /// The bottom face button (SDL "South": A on Xbox, Cross on PlayStation).
    South,
    /// The right face button (SDL "East": B on Xbox, Circle on PlayStation).
    East,
    /// The left face button (SDL "West": X on Xbox, Square on PlayStation).
    West,
    /// The top face button (SDL "North": Y on Xbox, Triangle on PlayStation).
    North,
    LeftShoulder,
    RightShoulder,
    Start,
    Select,
    /// Left stick pushed past the digital threshold.
    LeftStickUp,
    LeftStickDown,
    LeftStickLeft,
    LeftStickRight,
}

impl PadButton {
    /// Every variant, for iteration in a remap UI and in tests.
    pub const ALL: [PadButton; 16] = [
        PadButton::DpadUp,
        PadButton::DpadDown,
        PadButton::DpadLeft,
        PadButton::DpadRight,
        PadButton::South,
        PadButton::East,
        PadButton::West,
        PadButton::North,
        PadButton::LeftShoulder,
        PadButton::RightShoulder,
        PadButton::Start,
        PadButton::Select,
        PadButton::LeftStickUp,
        PadButton::LeftStickDown,
        PadButton::LeftStickLeft,
        PadButton::LeftStickRight,
    ];

    /// Stable name, used by the on-disk binding file and by the remap UI.
    /// These strings are a persisted format: rename one and every saved
    /// config silently loses that binding.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            PadButton::DpadUp => "DpadUp",
            PadButton::DpadDown => "DpadDown",
            PadButton::DpadLeft => "DpadLeft",
            PadButton::DpadRight => "DpadRight",
            PadButton::South => "South",
            PadButton::East => "East",
            PadButton::West => "West",
            PadButton::North => "North",
            PadButton::LeftShoulder => "LeftShoulder",
            PadButton::RightShoulder => "RightShoulder",
            PadButton::Start => "Start",
            PadButton::Select => "Select",
            PadButton::LeftStickUp => "LeftStickUp",
            PadButton::LeftStickDown => "LeftStickDown",
            PadButton::LeftStickLeft => "LeftStickLeft",
            PadButton::LeftStickRight => "LeftStickRight",
        }
    }

    /// Inverse of [`PadButton::name`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        PadButton::ALL.into_iter().find(|b| b.name() == name)
    }
}

/// One thing that happened to a pad since the last poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadEvent {
    Connected(PadId),
    Disconnected(PadId),
    ButtonDown(PadId, PadButton),
    ButtonUp(PadId, PadButton),
}

/// A source of [`PadEvent`]s. Implemented by the `gilrs` backend, and by
/// test doubles.
pub trait PadBackend {
    /// Drain everything that happened since the last call. Must not block.
    fn poll(&mut self) -> Vec<PadEvent>;
}

/// `PadButton -> (port-relative) NesButton` bindings, shared by every pad.
///
/// One table for all pads rather than one per pad: the port a pad drives
/// comes from [`PadRouter`]'s assignment, so a per-pad table would encode
/// the same thing twice and let the two disagree.
#[derive(Debug, Clone, Default)]
pub struct PadMap {
    entries: Vec<(PadButton, NesButton)>,
}

impl PadMap {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind `pad_button` to `button`, replacing any prior binding for it.
    pub fn bind(&mut self, pad_button: PadButton, button: NesButton) {
        self.entries.retain(|(p, _)| *p != pad_button);
        self.entries.push((pad_button, button));
    }

    /// Remove any binding for `pad_button`.
    pub fn unbind(&mut self, pad_button: PadButton) {
        self.entries.retain(|(p, _)| *p != pad_button);
    }

    #[must_use]
    pub fn lookup(&self, pad_button: PadButton) -> Option<NesButton> {
        self.entries
            .iter()
            .find(|(p, _)| *p == pad_button)
            .map(|&(_, b)| b)
    }

    /// Every binding, in insertion order — for the remap UI and for saving.
    #[must_use]
    pub fn entries(&self) -> &[(PadButton, NesButton)] {
        &self.entries
    }

    /// The default layout: d-pad and left stick both drive the d-pad, South
    /// is A and East is B (the SDL-standard positions of the two buttons a
    /// right-handed player's thumb reaches first, matching how every
    /// mainstream emulator ships), Start is Start and Select is Select.
    ///
    /// Both shoulder buttons are deliberately left unbound: an NES pad has
    /// eight buttons and this maps exactly eight, so anything else would be
    /// a duplicate that hides a mis-binding.
    #[must_use]
    pub fn default_nes() -> Self {
        let mut map = Self::new();
        map.bind(PadButton::DpadUp, NesButton::Up);
        map.bind(PadButton::DpadDown, NesButton::Down);
        map.bind(PadButton::DpadLeft, NesButton::Left);
        map.bind(PadButton::DpadRight, NesButton::Right);
        map.bind(PadButton::LeftStickUp, NesButton::Up);
        map.bind(PadButton::LeftStickDown, NesButton::Down);
        map.bind(PadButton::LeftStickLeft, NesButton::Left);
        map.bind(PadButton::LeftStickRight, NesButton::Right);
        map.bind(PadButton::South, NesButton::A);
        map.bind(PadButton::East, NesButton::B);
        map.bind(PadButton::Start, NesButton::Start);
        map.bind(PadButton::Select, NesButton::Select);
        map
    }
}

/// Routes pads to controller ports and tracks what is currently held.
///
/// This is the piece that makes hotplug real: it owns the pad-to-port
/// assignment, survives disconnects without disturbing other players, and
/// produces the per-port button bytes the frontend ORs into its keyboard
/// latch.
#[derive(Debug, Default)]
pub struct PadRouter {
    /// Assigned pads, in port order. `None` is a free port.
    ports: [Option<PadId>; MAX_INPUT_PORTS],
    /// Currently-held pad buttons, per assigned pad.
    held: Vec<(PadId, PadButton)>,
}

impl PadRouter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one poll's worth of events.
    pub fn apply(&mut self, events: &[PadEvent]) {
        for event in events {
            match *event {
                PadEvent::Connected(id) => self.connect(id),
                PadEvent::Disconnected(id) => self.disconnect(id),
                PadEvent::ButtonDown(id, button) => {
                    if self.port_of(id).is_some() && !self.held.contains(&(id, button)) {
                        self.held.push((id, button));
                    }
                }
                PadEvent::ButtonUp(id, button) => {
                    self.held.retain(|held| *held != (id, button));
                }
            }
        }
    }

    /// Drain `backend` and apply what it reports.
    pub fn poll(&mut self, backend: &mut dyn PadBackend) {
        let events = backend.poll();
        self.apply(&events);
    }

    /// Which port `id` drives, if it is connected and got one.
    #[must_use]
    pub fn port_of(&self, id: PadId) -> Option<usize> {
        self.ports.iter().position(|slot| *slot == Some(id))
    }

    /// The pad assigned to `port`, if any.
    #[must_use]
    pub fn pad_at(&self, port: usize) -> Option<PadId> {
        self.ports.get(port).copied().flatten()
    }

    /// How many pads are currently assigned.
    #[must_use]
    pub fn connected_count(&self) -> usize {
        self.ports.iter().filter(|slot| slot.is_some()).count()
    }

    /// The button bits for `port`, per `map` — the same `1 << bit()`
    /// layout [`crate::InputLatch::sample`] produces, so a frontend can OR
    /// the two together and let a player use keyboard and pad at once.
    #[must_use]
    pub fn buttons(&self, port: usize, map: &PadMap) -> u16 {
        let Some(id) = self.pad_at(port) else {
            return 0;
        };
        self.held
            .iter()
            .filter(|(held_id, _)| *held_id == id)
            .filter_map(|(_, pad_button)| map.lookup(*pad_button))
            .fold(0u16, |bits, button| bits | (1u16 << button.bit()))
    }

    /// Every port's pad state as one [`InputFrame`], mirroring
    /// [`crate::InputLatch::sample`]. The frontend ORs this with the
    /// keyboard's frame so both work at once — neither wins, because a
    /// player using a pad while resting a hand on the keyboard should not
    /// have one input silently cancel the other.
    #[must_use]
    pub fn sample(&self, map: &PadMap) -> InputFrame {
        let mut frame = InputFrame::empty();
        for (port, bits) in frame.ports.iter_mut().enumerate() {
            *bits = self.buttons(port, map);
        }
        frame
    }

    /// A pad takes the lowest free port. Already-assigned pads keep theirs
    /// (a duplicate `Connected` is not an error — backends re-announce
    /// devices after a device-list refresh).
    fn connect(&mut self, id: PadId) {
        if self.port_of(id).is_some() {
            return;
        }
        if let Some(slot) = self.ports.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(id);
        }
        // More pads than ports: the extra ones stay unassigned rather than
        // displacing a player who is already using one.
    }

    fn disconnect(&mut self, id: PadId) {
        for slot in &mut self.ports {
            if *slot == Some(id) {
                *slot = None;
            }
        }
        // Releasing everything the pad held is not optional: a pad yanked
        // mid-press would otherwise leave that button stuck on forever,
        // which looks exactly like a broken emulator.
        self.held.retain(|(held_id, _)| *held_id != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted backend: hotplug becomes a unit test rather than a
    /// human unplugging a controller (module doc).
    #[derive(Default)]
    struct ScriptedPads {
        script: Vec<Vec<PadEvent>>,
    }

    impl PadBackend for ScriptedPads {
        fn poll(&mut self) -> Vec<PadEvent> {
            if self.script.is_empty() {
                Vec::new()
            } else {
                self.script.remove(0)
            }
        }
    }

    const PAD_A: PadId = PadId(1);
    const PAD_B: PadId = PadId(2);

    #[test]
    fn the_first_pad_to_connect_takes_port_zero_and_the_second_takes_port_one() {
        let mut router = PadRouter::new();
        router.apply(&[PadEvent::Connected(PAD_A), PadEvent::Connected(PAD_B)]);
        assert_eq!(router.port_of(PAD_A), Some(0));
        assert_eq!(router.port_of(PAD_B), Some(1));
        assert_eq!(router.connected_count(), 2);
    }

    /// The sticky-assignment rule, and the reason for it: unplugging player
    /// 1 must not promote player 2 mid-game.
    #[test]
    fn unplugging_one_pad_leaves_the_others_port_alone() {
        let mut router = PadRouter::new();
        router.apply(&[PadEvent::Connected(PAD_A), PadEvent::Connected(PAD_B)]);
        router.apply(&[PadEvent::Disconnected(PAD_A)]);

        assert_eq!(router.port_of(PAD_A), None);
        assert_eq!(
            router.port_of(PAD_B),
            Some(1),
            "player 2 must not be promoted to port 0 by player 1 unplugging"
        );
        assert_eq!(router.pad_at(0), None, "port 0 is free, not reassigned");
    }

    #[test]
    fn a_reconnecting_pad_takes_the_lowest_free_port() {
        let mut router = PadRouter::new();
        router.apply(&[PadEvent::Connected(PAD_A), PadEvent::Connected(PAD_B)]);
        router.apply(&[PadEvent::Disconnected(PAD_A)]);
        router.apply(&[PadEvent::Connected(PAD_A)]);
        assert_eq!(
            router.port_of(PAD_A),
            Some(0),
            "its old port was still free"
        );
        assert_eq!(router.port_of(PAD_B), Some(1));
    }

    /// The failure this exists to prevent: a pad yanked mid-press leaving a
    /// button stuck on, which is indistinguishable from a broken emulator.
    #[test]
    fn disconnecting_releases_everything_that_pad_was_holding() {
        let map = PadMap::default_nes();
        let mut router = PadRouter::new();
        router.apply(&[
            PadEvent::Connected(PAD_A),
            PadEvent::ButtonDown(PAD_A, PadButton::South),
            PadEvent::ButtonDown(PAD_A, PadButton::DpadRight),
        ]);
        assert_eq!(
            router.buttons(0, &map),
            (1 << NesButton::A.bit()) | (1 << NesButton::Right.bit())
        );

        router.apply(&[PadEvent::Disconnected(PAD_A)]);
        assert_eq!(
            router.buttons(0, &map),
            0,
            "a yanked pad must release everything"
        );
    }

    #[test]
    fn buttons_are_reported_per_port_and_never_bleed_between_pads() {
        let map = PadMap::default_nes();
        let mut router = PadRouter::new();
        router.apply(&[
            PadEvent::Connected(PAD_A),
            PadEvent::Connected(PAD_B),
            PadEvent::ButtonDown(PAD_A, PadButton::South),
            PadEvent::ButtonDown(PAD_B, PadButton::Start),
        ]);
        assert_eq!(router.buttons(0, &map), 1 << NesButton::A.bit());
        assert_eq!(router.buttons(1, &map), 1 << NesButton::Start.bit());
    }

    /// Both the d-pad and the left stick drive the same directions, so a
    /// player on either gets the same result — and holding both is still
    /// just one direction, not a double-press.
    #[test]
    fn the_stick_and_the_dpad_are_the_same_directions() {
        let map = PadMap::default_nes();
        let mut stick = PadRouter::new();
        stick.apply(&[
            PadEvent::Connected(PAD_A),
            PadEvent::ButtonDown(PAD_A, PadButton::LeftStickLeft),
        ]);
        let mut dpad = PadRouter::new();
        dpad.apply(&[
            PadEvent::Connected(PAD_A),
            PadEvent::ButtonDown(PAD_A, PadButton::DpadLeft),
        ]);
        assert_eq!(stick.buttons(0, &map), dpad.buttons(0, &map));

        let mut both = PadRouter::new();
        both.apply(&[
            PadEvent::Connected(PAD_A),
            PadEvent::ButtonDown(PAD_A, PadButton::DpadLeft),
            PadEvent::ButtonDown(PAD_A, PadButton::LeftStickLeft),
        ]);
        assert_eq!(both.buttons(0, &map), 1 << NesButton::Left.bit());
    }

    /// There are [`MAX_INPUT_PORTS`] ports (4 — `rf-core-api` sizes
    /// `InputFrame` for future consoles; an NES core simply ignores ports
    /// 2-3), and a pad beyond the last one stays unassigned rather than
    /// displacing a player who already has a port.
    #[test]
    fn pads_beyond_the_last_port_stay_unassigned_rather_than_displacing_a_player() {
        let mut router = PadRouter::new();
        let pads: Vec<PadId> = (1..=(MAX_INPUT_PORTS as u64 + 1)).map(PadId).collect();
        let events: Vec<PadEvent> = pads.iter().copied().map(PadEvent::Connected).collect();
        router.apply(&events);

        for (port, pad) in pads.iter().take(MAX_INPUT_PORTS).enumerate() {
            assert_eq!(router.port_of(*pad), Some(port));
        }
        assert_eq!(
            router.port_of(*pads.last().unwrap()),
            None,
            "the pad past the last port gets nothing"
        );
        assert_eq!(router.connected_count(), MAX_INPUT_PORTS);
    }

    /// An unassigned pad's buttons must go nowhere — not to port 0 by
    /// accident.
    #[test]
    fn an_unassigned_pad_drives_nothing() {
        let map = PadMap::default_nes();
        let mut router = PadRouter::new();
        let spare = PadId(MAX_INPUT_PORTS as u64 + 1);
        let mut events: Vec<PadEvent> = (1..=(MAX_INPUT_PORTS as u64 + 1))
            .map(|id| PadEvent::Connected(PadId(id)))
            .collect();
        events.push(PadEvent::ButtonDown(spare, PadButton::South));
        router.apply(&events);

        assert_eq!(router.port_of(spare), None, "the spare pad got no port");
        for port in 0..MAX_INPUT_PORTS {
            assert_eq!(
                router.buttons(port, &map),
                0,
                "an unassigned pad's press must not leak into port {port}"
            );
        }
    }

    #[test]
    fn polling_a_backend_applies_its_events_in_order() {
        let map = PadMap::default_nes();
        let mut backend = ScriptedPads {
            script: vec![
                vec![PadEvent::Connected(PAD_A)],
                vec![PadEvent::ButtonDown(PAD_A, PadButton::Start)],
                vec![PadEvent::ButtonUp(PAD_A, PadButton::Start)],
            ],
        };
        let mut router = PadRouter::new();

        router.poll(&mut backend);
        assert_eq!(router.connected_count(), 1);
        router.poll(&mut backend);
        assert_eq!(router.buttons(0, &map), 1 << NesButton::Start.bit());
        router.poll(&mut backend);
        assert_eq!(router.buttons(0, &map), 0);
    }

    #[test]
    fn every_pad_button_name_round_trips() {
        for button in PadButton::ALL {
            assert_eq!(PadButton::from_name(button.name()), Some(button));
        }
        assert_eq!(PadButton::from_name("Nonsense"), None);
    }
}
