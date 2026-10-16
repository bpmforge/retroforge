//! Ticket W31-02: the Super FX polygon observer's profile -> core bridge.
//!
//! `rf-snes` must not depend on `rf-profiles`, so the shell turns the
//! profile's `[superfx_polygons]` row into the core's plain
//! [`GsuPolyConfig`] here.

use rf_profiles::schema::SuperfxPolygons;
use rf_snes::gsu_observer::GsuPolyConfig;

/// The observer config for a profile row, or `None` if a value does not fit
/// (the loader has already range-checked a loaded profile, so this only
/// guards a hand-built one).
#[must_use]
pub fn observer_config(sp: &SuperfxPolygons) -> Option<GsuPolyConfig> {
    let walker = <[u32; 2]>::try_from(sp.walker_pcs.as_slice()).ok()?;
    let viewport = <[u32; 2]>::try_from(sp.viewport.as_slice()).ok()?;
    Some(GsuPolyConfig {
        pc_bank: u8::try_from(sp.pc_bank).ok()?,
        count_read_pc: u16::try_from(sp.count_read_pc).ok()?,
        count_addr: u16::try_from(sp.count_addr).ok()?,
        buffer_addr: u16::try_from(sp.buffer_addr).ok()?,
        // The end is exclusive; 0x10000 (the whole bank) clamps to 0xFFFF.
        walker_pcs: (
            u16::try_from(walker[0]).ok()?,
            u16::try_from(walker[1].min(0xFFFF)).ok()?,
        ),
        viewport: (
            u16::try_from(viewport[0]).ok()?,
            u16::try_from(viewport[1]).ok()?,
        ),
        capture_bitmap: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_star_fox_row_becomes_the_observer_config() {
        let text = include_str!("../../../profiles/snes/star-fox/profile.toml");
        let profile = rf_profiles::loader::load_str(text).expect("loads").profile;
        let cfg = observer_config(&profile.superfx_polygons.expect("row")).expect("fits");
        assert_eq!(
            cfg,
            GsuPolyConfig {
                pc_bank: 1,
                count_read_pc: 0x8DE0,
                count_addr: 0x0134,
                buffer_addr: 0x0982,
                walker_pcs: (0xA6C0, 0xA740),
                viewport: (224, 170),
                capture_bitmap: false,
            }
        );
    }

    #[test]
    fn a_value_that_does_not_fit_is_refused() {
        let mut sp = SuperfxPolygons {
            pc_bank: 1,
            count_read_pc: 0x8DE0,
            count_addr: 0x0134,
            buffer_addr: 0x0982,
            walker_pcs: vec![0xA6C0, 0xA740],
            viewport: vec![224, 170],
        };
        assert!(observer_config(&sp).is_some());
        sp.pc_bank = 0x100;
        assert!(observer_config(&sp).is_none());
        sp.pc_bank = 1;
        sp.walker_pcs = vec![1];
        assert!(observer_config(&sp).is_none());
    }
}
