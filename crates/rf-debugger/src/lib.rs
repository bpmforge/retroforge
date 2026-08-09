//! Debug/inspection: viewers' data providers, event-timeline builder, and
//! the persisted dock-panel layout format (ticket W4-06a: pattern/
//! nametable/palette/OAM viewers, event viewer, `egui_dock` layout
//! persistence — FR-DBG-001, DEBUGGER.md §3, FR-FE-004).
//!
//! Ticket W4-06b adds the annotation store + profile-skeleton export +
//! DataCrystal TSV import + memory-viewer data layer (FR-DBG-005,
//! FR-DBG-002, GAME_PROFILES.md §3 steps 1-2, CONSTRAINTS §2's
//! facts-only transcription policy): [`annotation`], [`profile_export`],
//! [`datacrystal`], [`memory_view`].
//!
//! See /docs/MODULE_DESIGN.md and /docs/design/ for the contract this crate
//! must implement. Do not add public API here without a ticket in plan.json.
//!
//! ## UI-free by design (DEBUGGER.md §3: "Providers live in `rf-debugger`
//! (UI-free, unit-testable); panels in the frontend crate")
//!
//! Nothing in this crate depends on `egui`/`eframe`/`egui_dock`, and
//! nothing in it depends on `rf-nes` (`scripts/validate-arch.sh` rule 3 —
//! only `retroforge`/`rf-harness` may touch a console core directly). Every
//! public function here takes plain, already-extracted data (`&[u8]`,
//! `&[rf_core_api::CoreEvent]`) and returns plain decoded structs — no
//! `rf_core_api::StateView` parameter, deliberately: nothing in this
//! workspace produces a live one yet (`rf_nes` implements no
//! `EmulatorCore::state_view`, verified against source at this ticket's
//! pre-flight — see each viewer module's own doc for the specifics), and
//! `crate::layout`/`crate::event_timeline`/`crate::oam`'s viewers already
//! have real data without it. This mirrors the same "plain slice, not a
//! live `StateView`" choice `rf_enhance::sprite_historian`/
//! `rf_enhance::scene_identity` made for the identical gap.
//!
//! `crates/retroforge` is the shell that has real bytes to hand these
//! functions (it already depends on `rf-nes`, exempt from rule 3 as the
//! app shell itself) and the crate that owns every `egui`/`egui_dock`
//! call — see `crates/retroforge/src/debug_dock.rs`.
//!
//! | Module | Viewer | Live data today? |
//! |---|---|---|
//! | [`pattern`] | Pattern/CHR | Yes — static, from the loaded ROM file's CHR bytes |
//! | [`nametable`] | Nametable/Tilemap | No — decode-only, see module doc |
//! | [`palette`] | Palette | No — decode-only, see module doc |
//! | [`oam`] | OAM/Sprite | Yes — live, every frame |
//! | [`event_timeline`] | Event viewer | Yes — live, `FrameBundle::events` |
//! | [`layout`] | (docking, not a viewer) | N/A — pure persisted-format data |
//! | [`memory_view`] | Memory hex | Yes — live, `EmuStepper::peek`/`prg_ram` (W4-06b) |
//! | [`annotation`] | (annotation store, not a viewer) | N/A — pure data (W4-06b) |
//! | [`profile_export`] | (skeleton export, not a viewer) | N/A — pure data (W4-06b) |
//! | [`datacrystal`] | (TSV import, not a viewer) | N/A — pure data (W4-06b) |

pub mod annotation;
pub mod datacrystal;
pub mod event_timeline;
pub mod layout;
pub mod memory_view;
pub mod nametable;
pub mod oam;
pub mod palette;
pub mod pattern;
pub mod profile_export;

/// Crate marker used by the test harness to confirm workspace wiring.
pub const CRATE_NAME: &str = "rf-debugger";

#[cfg(test)]
mod tests {
    #[test]
    fn crate_is_wired() {
        assert_eq!(super::CRATE_NAME, "rf-debugger");
    }
}
