//! Ticket W30-08: the core thread's side of the speed governor.
//!
//! [`rf_enhance::speed_governor::SpeedGovernor`] is pure: it is fed one
//! observation per real frame and answers with a multiplier. This module
//! owns what it cannot: one governor per overclocked chip (the S-CPU, the
//! SA-1 and the Super FX), the tick signal and the picture-changed signal it is fed from, and
//! applying the answer to the machine.
//!
//! It lives on the CORE thread because only that thread sees every real
//! frame (the UI thread sees them lossily), and it observes the REAL frame
//! only — run-ahead's shadow machine just copies the multipliers chosen here
//! (`EmuStepper::copy_picture_config_from`).
//!
//! ## The signals
//!
//! - **Tick**: when the matched profile declares `[overclock].tick_counter`
//!   (a WRAM byte the game increments once per logic tick), a tick is that
//!   byte having moved by exactly +1 (mod 256) since the previous real
//!   frame. Otherwise the S-CPU's `frame_idle` for every chip: the SA-1's
//!   own idle carries no information (it is always "idle" in a polling
//!   loop), the S-CPU's does.
//! - **Picture changed**: this frame's resolved picture (the sink's RGBA, so
//!   a palette change counts) differs from the last real frame's. A game that hangs at a raised clock stops changing
//!   it, which is how the governor's hang guard finds out.

use rf_enhance::speed_governor::{GovernorStatus, Notice, Observation, SpeedGovernor, NORMAL};

use crate::stepper::EmuStepper;

/// Which clock a policy is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockChip {
    /// The SNES main CPU (65816).
    Cpu,
    /// The SA-1 coprocessor.
    Sa1,
    /// The Super FX (GSU) coprocessor (W30-01c); up to 800%.
    Gsu,
}

/// How one chip's clock is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClockPolicy {
    /// Stock speed.
    #[default]
    Off,
    /// Always this speed, percent (the advanced option).
    Fixed(u16),
    /// The governor chooses, never above `ceiling` percent.
    Governed { ceiling: u16 },
}

/// What the profile told us about the game's tick, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClockSignal {
    /// `[overclock].tick_counter`: the WRAM address of the game's tick byte.
    pub tick_counter: Option<u32>,
    /// `[overclock].target_tick_hz`: the game's normal logic rate.
    pub target_hz: Option<u32>,
}

/// The governors' status for the card, per chip (`None` when that chip is
/// not governed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GovernorReport {
    pub cpu: Option<GovernorStatus>,
    pub sa1: Option<GovernorStatus>,
    pub gsu: Option<GovernorStatus>,
}

#[derive(Debug, Default)]
struct Chip {
    policy: ClockPolicy,
    /// Kept after the policy turns off if (and only if) the hang guard
    /// tripped, so the "rest of the session" cap survives a toggle.
    governor: Option<SpeedGovernor>,
    applied: u16,
}

impl Chip {
    fn governed(&self) -> bool {
        matches!(self.policy, ClockPolicy::Governed { .. })
    }

    fn set_policy(&mut self, policy: ClockPolicy, signal: ClockSignal) {
        self.policy = policy;
        match policy {
            ClockPolicy::Off | ClockPolicy::Fixed(_) => {
                if self
                    .governor
                    .as_ref()
                    .is_none_or(|g| g.status().notice != Notice::TurnedOff)
                {
                    self.governor = None;
                }
            }
            ClockPolicy::Governed { ceiling } => {
                let governor = self
                    .governor
                    .get_or_insert_with(|| SpeedGovernor::new(ceiling, signal.target_hz));
                governor.set_ceiling(ceiling);
                governor.set_profile_target(signal.target_hz);
            }
        }
    }

    /// The multiplier this chip should run at right now.
    fn wanted(&self) -> u16 {
        match self.policy {
            ClockPolicy::Off => NORMAL,
            ClockPolicy::Fixed(pct) => pct,
            ClockPolicy::Governed { .. } => {
                self.governor.as_ref().map_or(NORMAL, |g| g.multiplier())
            }
        }
    }
}

/// Both chips' governors plus the signal bookkeeping.
#[derive(Debug, Default)]
pub struct ClockGovernors {
    cpu: Chip,
    sa1: Chip,
    gsu: Chip,
    signal: ClockSignal,
    last_counter: Option<u8>,
    last_picture: Option<u64>,
}

impl ClockGovernors {
    /// Set one chip's policy and apply it to the machine.
    pub fn set_policy(&mut self, chip: ClockChip, policy: ClockPolicy, stepper: &mut EmuStepper) {
        let signal = self.signal;
        match chip {
            ClockChip::Cpu => self.cpu.set_policy(policy, signal),
            ClockChip::Sa1 => self.sa1.set_policy(policy, signal),
            ClockChip::Gsu => self.gsu.set_policy(policy, signal),
        }
        self.apply(stepper);
    }

    /// The matched profile's tick counter and target rate (both `None` for
    /// a game with no `[overclock]` signal).
    pub fn set_signal(&mut self, signal: ClockSignal) {
        if self.signal.tick_counter != signal.tick_counter {
            self.last_counter = None;
        }
        self.signal = signal;
        for chip in [&mut self.cpu, &mut self.sa1, &mut self.gsu] {
            if let Some(g) = chip.governor.as_mut() {
                g.set_profile_target(signal.target_hz);
            }
        }
    }

    /// Whether any governor needs the picture and tick of each real frame.
    #[must_use]
    pub fn wants_observation(&self) -> bool {
        self.cpu.governed() || self.sa1.governed() || self.gsu.governed()
    }

    /// Feed one real frame. `picture` is the frame's resolved RGBA.
    pub fn observe(&mut self, stepper: &mut EmuStepper, picture: &[u8]) {
        if !self.wants_observation() {
            return;
        }
        let tick = self.tick(stepper);
        let picture_changed = self.picture_changed(picture);
        let obs = Observation {
            tick,
            picture_changed,
        };
        for chip in [&mut self.cpu, &mut self.sa1, &mut self.gsu] {
            if chip.governed() {
                if let Some(g) = chip.governor.as_mut() {
                    g.observe(obs);
                }
            }
        }
        self.apply(stepper);
    }

    /// Re-baseline the signals after the machine jumped (a state load or
    /// rewind), so the jump is not read as a tick or a hang.
    pub fn resync(&mut self) {
        self.last_counter = None;
        self.last_picture = None;
    }

    /// The card's view.
    #[must_use]
    pub fn report(&self) -> GovernorReport {
        let status = |chip: &Chip| {
            chip.governed()
                .then(|| chip.governor.as_ref().map(SpeedGovernor::status))
                .flatten()
        };
        GovernorReport {
            cpu: status(&self.cpu),
            sa1: status(&self.sa1),
            gsu: status(&self.gsu),
        }
    }

    fn tick(&mut self, stepper: &EmuStepper) -> bool {
        match self.signal.tick_counter {
            Some(addr) => {
                let now = stepper.peek_bus(addr);
                let tick = self.last_counter == Some(now.wrapping_sub(1));
                self.last_counter = Some(now);
                tick
            }
            None => stepper.frame_idle(),
        }
    }

    fn picture_changed(&mut self, picture: &[u8]) -> bool {
        let hash = picture_hash(picture);
        let changed = self.last_picture != Some(hash);
        self.last_picture = Some(hash);
        changed
    }

    fn apply(&mut self, stepper: &mut EmuStepper) {
        let cpu = self.cpu.wanted();
        if cpu != self.cpu.applied || !self.cpu.governed() && cpu != stepper.cpu_overclock() {
            stepper.set_cpu_overclock(cpu);
            self.cpu.applied = cpu;
        }
        let sa1 = self.sa1.wanted();
        if sa1 != self.sa1.applied || !self.sa1.governed() && sa1 != stepper.sa1_overclock() {
            stepper.set_sa1_overclock(sa1);
            self.sa1.applied = sa1;
        }
        let gsu = self.gsu.wanted();
        if gsu != self.gsu.applied || !self.gsu.governed() && gsu != stepper.gsu_overclock() {
            stepper.set_gsu_overclock(gsu);
            self.gsu.applied = gsu;
        }
    }
}

/// FNV-1a over a frame, eight bytes at a time (a cheap "did the picture
/// change", run every governed frame).
fn picture_hash(picture: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut chunks = picture.chunks_exact(8);
    for c in &mut chunks {
        let word = u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
        h = (h ^ word).wrapping_mul(0x100_0000_01b3);
    }
    for &b in chunks.remainder() {
        h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_and_fixed_policies_never_build_a_governor() {
        let mut chip = Chip::default();
        chip.set_policy(ClockPolicy::Fixed(300), ClockSignal::default());
        assert!(chip.governor.is_none());
        assert_eq!(chip.wanted(), 300);
        chip.set_policy(ClockPolicy::Off, ClockSignal::default());
        assert_eq!(chip.wanted(), 100);
    }

    #[test]
    fn a_governed_chip_starts_at_normal_and_follows_its_ceiling() {
        let mut chip = Chip::default();
        chip.set_policy(
            ClockPolicy::Governed { ceiling: 400 },
            ClockSignal::default(),
        );
        assert_eq!(chip.wanted(), 100, "governed starts at stock speed");
        chip.set_policy(
            ClockPolicy::Governed { ceiling: 200 },
            ClockSignal::default(),
        );
        assert!(chip.governor.is_some());
    }

    #[test]
    fn a_tripped_hang_guard_survives_turning_the_policy_off_and_on() {
        let mut chip = Chip::default();
        chip.set_policy(
            ClockPolicy::Governed { ceiling: 400 },
            ClockSignal {
                tick_counter: None,
                target_hz: Some(60),
            },
        );
        // A game at 40/s whose picture freezes the moment the clock rises.
        let mut acc = 0;
        for _ in 0..60 * 20 {
            let g = chip.governor.as_mut().unwrap();
            let raised = g.multiplier() > 100;
            acc += if raised { 60 } else { 40 };
            let tick = acc >= 60;
            if tick {
                acc -= 60;
            }
            g.observe(Observation {
                tick,
                picture_changed: !raised,
            });
        }
        assert_eq!(
            chip.governor.as_ref().unwrap().status().notice,
            Notice::TurnedOff
        );
        chip.set_policy(ClockPolicy::Off, ClockSignal::default());
        chip.set_policy(
            ClockPolicy::Governed { ceiling: 400 },
            ClockSignal::default(),
        );
        assert_eq!(chip.wanted(), 100);
        assert_eq!(
            chip.governor.as_ref().unwrap().status().notice,
            Notice::TurnedOff,
            "still off for the session"
        );
    }

    /// A minimal LoROM image that spins (`BRA *`): enough for a real SNES
    /// stepper to take a clock command.
    fn snes_stepper() -> EmuStepper {
        let mut rom = vec![0u8; 0x8000];
        rom[0] = 0x80;
        rom[1] = 0xFE;
        rom[0x7FD5] = 0x20;
        rom[0x7FD7] = 0x08;
        rom[0x7FFD] = 0x80;
        EmuStepper::open(&rom).expect("a minimal LoROM opens")
    }

    #[test]
    fn policies_reach_the_machine_and_off_restores_stock() {
        let mut stepper = snes_stepper();
        let mut clocks = ClockGovernors::default();
        clocks.set_policy(ClockChip::Cpu, ClockPolicy::Fixed(300), &mut stepper);
        assert_eq!(stepper.cpu_overclock(), 300);
        clocks.set_policy(ClockChip::Sa1, ClockPolicy::Fixed(200), &mut stepper);
        assert_eq!(stepper.sa1_overclock(), 200);
        // The Super FX is a no-op on this cartridge, but the policy is held
        // and governed like the others (W30-01c).
        clocks.set_policy(
            ClockChip::Gsu,
            ClockPolicy::Governed { ceiling: 800 },
            &mut stepper,
        );
        assert!(clocks.report().gsu.is_some());
        clocks.set_policy(ClockChip::Gsu, ClockPolicy::Off, &mut stepper);
        assert!(clocks.report().gsu.is_none());
        // Governed starts at stock speed, whatever the ceiling.
        clocks.set_policy(
            ClockChip::Cpu,
            ClockPolicy::Governed { ceiling: 400 },
            &mut stepper,
        );
        assert_eq!(stepper.cpu_overclock(), 100);
        assert!(clocks.wants_observation());
        assert!(clocks.report().cpu.is_some() && clocks.report().sa1.is_none());
        clocks.set_policy(ClockChip::Cpu, ClockPolicy::Off, &mut stepper);
        clocks.set_policy(ClockChip::Sa1, ClockPolicy::Off, &mut stepper);
        assert_eq!(
            (stepper.cpu_overclock(), stepper.sa1_overclock()),
            (100, 100)
        );
        assert!(!clocks.wants_observation());
    }

    #[test]
    fn the_run_ahead_shadow_copies_the_chosen_multipliers() {
        let mut real = snes_stepper();
        real.set_cpu_overclock(300);
        let mut shadow = snes_stepper();
        shadow.copy_picture_config_from(&real);
        assert_eq!(shadow.cpu_overclock(), 300);
    }

    #[test]
    fn picture_hash_tells_pictures_apart() {
        let a: Vec<u8> = (0..30).collect();
        let mut b = a.clone();
        b[29] ^= 1; // in the tail
        let mut c = a.clone();
        c[3] ^= 1; // in a chunk
        assert_ne!(picture_hash(&a), picture_hash(&b));
        assert_ne!(picture_hash(&a), picture_hash(&c));
        assert_eq!(picture_hash(&a), picture_hash(&a.clone()));
    }
}
