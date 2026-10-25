//! Resolving a CPU address to a ROM offset using only mappings the
//! recording saw (ticket W36-02).
//!
//! ## The rule
//!
//! A static `JSR`/`JMP`/branch operand is a CPU address; the bytes it names
//! depend on the bank state when it runs. For a *site* at CPU address `s`
//! sitting at ROM offset `o` (so its page maps base `b_s = o - (s & 0xFFF)`)
//! and a *target* CPU address `t` on page `p_t`:
//!
//! 1. **Co-observed.** Collect the bases the target page had in every
//!    recorded [`WindowMap`] in which the site's page also had base `b_s`
//!    (the two pages were mapped *together*, e.g. both inside one 32 KiB
//!    GxROM bank). Exactly one distinct base: use it.
//! 2. **Contiguous tie-break.** Several distinct bases: keep those that lie
//!    at the same CPU spacing in ROM as in the address space
//!    (`b_t - b_s == (p_t - p_s) * 4096`, one contiguous ROM region). Exactly
//!    one left: use it. Otherwise the edge is [`Why::Ambiguous`] — a call from
//!    a fixed bank into a switched one genuinely depends on a bank write that
//!    the atlas cannot see statically.
//! 3. **Unique fallback.** No recorded map holds the site's mapping (it was
//!    never executed, so the mapping may not be among the snapshots): look
//!    at every recorded base of the target page. Exactly one: use it; none:
//!    [`Why::NoWindow`]; several: step 2's tie-break, else ambiguous.
//!
//! Snapshots are whole simultaneous mappings (see `rf_core_api::coverage`),
//! which is why step 1 can speak of "mapped together" rather than inferring
//! it from independent observations.

use std::collections::BTreeMap;

use rf_core_api::coverage::{WindowMap, NO_ROM, WINDOW_BASE, WINDOW_PAGES, WINDOW_PAGE_SHIFT};

use crate::atlas::Why;

const PAGE: u32 = 1 << WINDOW_PAGE_SHIFT;

pub struct WindowIndex<'a> {
    maps: &'a [WindowMap],
    /// Distinct bases ever seen per page, ascending.
    page_bases: [Vec<u32>; WINDOW_PAGES],
    /// ROM page base -> lowest CPU page start that ever showed it.
    base_to_cpu: BTreeMap<u32, u32>,
    memo: BTreeMap<(usize, u32, usize), Result<u32, Why>>,
}

impl<'a> WindowIndex<'a> {
    #[must_use]
    pub fn new(maps: &'a [WindowMap]) -> Self {
        let mut page_bases: [Vec<u32>; WINDOW_PAGES] = Default::default();
        let mut base_to_cpu: BTreeMap<u32, u32> = BTreeMap::new();
        for m in maps {
            for (page, &base) in m.0.iter().enumerate() {
                if base == NO_ROM {
                    continue;
                }
                page_bases[page].push(base);
                let cpu = u32::from(WINDOW_BASE) + ((page as u32) << WINDOW_PAGE_SHIFT);
                let e = base_to_cpu.entry(base).or_insert(cpu);
                *e = (*e).min(cpu);
            }
        }
        for v in &mut page_bases {
            v.sort_unstable();
            v.dedup();
        }
        Self {
            maps,
            page_bases,
            base_to_cpu,
            memo: BTreeMap::new(),
        }
    }

    /// A CPU address ROM offset `rom` was seen at, the lowest if several
    /// windows showed it. `None` when no snapshot maps that offset.
    #[must_use]
    pub fn cpu_for_rom(&self, rom: u32) -> Option<u32> {
        let lo = rom.saturating_sub(PAGE - 1);
        self.base_to_cpu
            .range(lo..=rom)
            .filter(|(&base, _)| rom - base < PAGE)
            .map(|(&base, &cpu)| cpu + (rom - base))
            .min()
    }

    /// ROM offset of `target` for a site at (`site_cpu`, `site_rom`), by
    /// the module-level rule. `prg_len` bounds the result.
    ///
    /// # Errors
    /// [`Why`] when the target cannot be placed.
    pub fn resolve(
        &mut self,
        site_cpu: u32,
        site_rom: u32,
        target: u32,
        prg_len: u32,
    ) -> Result<u32, Why> {
        let Some(pt) = u16::try_from(target).ok().and_then(WindowMap::page_of) else {
            return Err(Why::NotRom);
        };
        let site_page = u16::try_from(site_cpu).ok().and_then(WindowMap::page_of);
        let site_base = site_rom.checked_sub(site_cpu & (PAGE - 1));
        let (key_page, key_base) = match (site_page, site_base) {
            (Some(p), Some(b)) => (p, b),
            _ => (usize::MAX, 0),
        };
        let base = match self.memo.get(&(key_page, key_base, pt)) {
            Some(r) => *r,
            None => {
                let r = self.pick_base(site_page.zip(site_base), pt);
                self.memo.insert((key_page, key_base, pt), r);
                r
            }
        }?;
        let rom = base + (target & (PAGE - 1));
        if rom >= prg_len {
            return Err(Why::OutOfImage);
        }
        Ok(rom)
    }

    fn pick_base(&self, site: Option<(usize, u32)>, pt: usize) -> Result<u32, Why> {
        if let Some((ps, bs)) = site {
            let mut co: Vec<u32> = self
                .maps
                .iter()
                .filter(|m| m.0[ps] == bs && m.0[pt] != NO_ROM)
                .map(|m| m.0[pt])
                .collect();
            co.sort_unstable();
            co.dedup();
            if !co.is_empty() {
                return Self::choose(&co, site, pt);
            }
        }
        let all = &self.page_bases[pt];
        if all.is_empty() {
            return Err(Why::NoWindow);
        }
        Self::choose(all, site, pt)
    }

    /// One candidate wins outright; several are filtered for contiguity.
    fn choose(cands: &[u32], site: Option<(usize, u32)>, pt: usize) -> Result<u32, Why> {
        if let [one] = cands {
            return Ok(*one);
        }
        if let Some((ps, bs)) = site {
            let want = i64::from(bs) + (pt as i64 - ps as i64) * i64::from(PAGE);
            let mut it = cands.iter().filter(|&&b| i64::from(b) == want);
            if let (Some(&b), None) = (it.next(), it.next()) {
                return Ok(b);
            }
        }
        Err(Why::Ambiguous)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pages: &[(usize, u32)]) -> WindowMap {
        let mut m = WindowMap::EMPTY;
        for &(p, b) in pages {
            m.0[p] = b;
        }
        m
    }

    const N: u32 = 0x10_0000;

    #[test]
    fn co_observed_pair_beats_other_banks_of_the_target_page() {
        // Site page 0 at base 0 only ever appeared with target page 2 at
        // base 0x2000, even though page 2 was elsewhere in other maps.
        let maps = [
            map(&[(0, 0), (2, 0x2000)]),
            map(&[(0, 0x8000), (2, 0xA000)]),
            map(&[(0, 0x4000), (2, 0x6000)]),
        ];
        let mut w = WindowIndex::new(&maps);
        assert_eq!(w.resolve(0x8004, 4, 0xA010, N), Ok(0x2010));
        assert_eq!(w.resolve(0x8004, 0x8004, 0xA010, N), Ok(0xA010));
    }

    #[test]
    fn several_co_observed_bases_fall_back_to_contiguity_then_ambiguity() {
        let maps = [
            map(&[(0, 0), (1, 0x1000)]), // contiguous
            map(&[(0, 0), (1, 0x5000)]),
        ];
        let mut w = WindowIndex::new(&maps);
        assert_eq!(w.resolve(0x8000, 0, 0x9000, N), Ok(0x1000));
        let maps = [map(&[(0, 0), (1, 0x3000)]), map(&[(0, 0), (1, 0x5000)])];
        let mut w = WindowIndex::new(&maps);
        assert_eq!(w.resolve(0x8000, 0, 0x9000, N), Err(Why::Ambiguous));
    }

    #[test]
    fn unexecuted_site_uses_the_unique_recorded_base_of_the_target_page() {
        let maps = [map(&[(0, 0), (3, 0x7000)])];
        let mut w = WindowIndex::new(&maps);
        // Site page 5 never mapped: no co-observation, one base for page 3.
        assert_eq!(w.resolve(0xD000, 0x9000, 0xB123, N), Ok(0x7123));
        // Nothing ever mapped page 4.
        assert_eq!(w.resolve(0xD000, 0x9000, 0xC000, N), Err(Why::NoWindow));
    }

    #[test]
    fn non_rom_and_out_of_image_targets() {
        let maps = [map(&[(0, 0), (1, 0x1000)])];
        let mut w = WindowIndex::new(&maps);
        assert_eq!(w.resolve(0x8000, 0, 0x0300, N), Err(Why::NotRom));
        assert_eq!(w.resolve(0x8000, 0, 0x1_9000, N), Err(Why::NotRom));
        assert_eq!(w.resolve(0x8000, 0, 0x9000, 0x1000), Err(Why::OutOfImage));
    }

    #[test]
    fn cpu_for_rom_picks_the_lowest_window_that_shows_it() {
        // A 16 KiB image mirrored at $8000 and $C000.
        let maps = [map(&[(0, 0), (1, 0x1000), (4, 0), (5, 0x1000)])];
        let w = WindowIndex::new(&maps);
        assert_eq!(w.cpu_for_rom(0x1234), Some(0x9234));
        assert_eq!(w.cpu_for_rom(0x3000), None);
    }
}
