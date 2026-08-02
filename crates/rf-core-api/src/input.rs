//! Per-frame latched input (ARCHITECTURE §5, §6).

/// Maximum simultaneous controller ports an [`InputFrame`] can carry.
/// Covers NES (2 ports) and SNES (2 ports, up to 4-5 with multitap) without
/// needing an allocation.
pub const MAX_INPUT_PORTS: usize = 4;

/// One frame's worth of input, latched before [`crate::EmulatorCore::run_frame`]
/// is called.
///
/// Determinism (ARCHITECTURE §6): input only takes effect at frame
/// boundaries — a core must not sample any other input source mid-frame.
/// `rf-state` logs the sequence of `InputFrame`s so a replay is exactly
/// "same ROM hash + same initial state + same input log".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputFrame {
    /// Per-port button bitmask. Bit assignment is core-defined (NES and
    /// SNES button maps differ); a port with no controller connected, or no
    /// buttons held, is `0`.
    pub ports: [u16; MAX_INPUT_PORTS],
}

impl InputFrame {
    /// All ports disconnected / no buttons held.
    #[must_use]
    pub const fn empty() -> Self {
        InputFrame {
            ports: [0; MAX_INPUT_PORTS],
        }
    }
}

impl Default for InputFrame {
    fn default() -> Self {
        Self::empty()
    }
}
