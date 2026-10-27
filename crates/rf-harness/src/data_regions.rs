//! Driving the data-region finder on a real cartridge (ticket W38-01).
//!
//! The finder itself ([`rf_rebuild::dataregion`]) is a [`StepObserver`] with no
//! console knowledge; what needs the cores is (1) turning a CPU address into a
//! ROM offset (the NES bank windows are read from the mapper once per frame,
//! the SNES map is fixed) and (2) running frames under the observer. That is
//! this module. A driver runs plain frames until the caller asks for an
//! observed one, so the slow per-instruction path is paid only around the
//! stretch being studied. An observed frame is bit-identical to a plain one
//! (W36-09), so switching between them changes nothing in the game.
//!
//! Only offsets and counts leave a caller (law 5, D-021).

use rf_core_api::{
    CoreEvent, CoreSink, EmulatorCore as _, InputFrame, PpuPixel, ResetKind, StepObserver,
};
use rf_nes::core::NesCore;
use rf_rebuild::dataregion::{DataFacts, DataReader, Resolver};
use rf_rebuild::{Atlas, Isa as _, Platform};
use rf_snes::core::SnesCore;
use rf_snes::rebuild::Wdc65816;

struct Null;

impl CoreSink for Null {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

/// The NES PRG pages `$8000-$FFFF` in 4 KiB steps, as ROM offsets (the
/// granularity the coverage recorder uses).
fn nes_pages(core: &NesCore) -> [Option<u32>; 8] {
    let mut pages = [None; 8];
    for (i, slot) in pages.iter_mut().enumerate() {
        let addr = 0x8000u16 + (i as u16) * 0x1000;
        *slot = core
            .bus()
            .prg_rom_offset(addr)
            .and_then(|o| u32::try_from(o).ok());
    }
    pages
}

/// An NES cartridge under the finder.
pub struct NesDriver {
    pub core: NesCore,
    atlas: Atlas,
    reader: DataReader,
    installed: bool,
}

impl NesDriver {
    /// # Errors
    /// The image does not load.
    pub fn new(raw: &[u8], atlas: &Atlas) -> Result<Self, String> {
        let mut core = NesCore::from_ines_bytes(raw).map_err(|e| format!("{e:?}"))?;
        core.reset(ResetKind::Hard);
        Ok(Self {
            core,
            atlas: atlas.clone(),
            reader: DataReader::new(Platform::Nes),
            installed: false,
        })
    }

    /// Run one frame, observed or plain.
    ///
    /// # Errors
    /// The atlas does not fit the cartridge.
    pub fn frame(&mut self, input: &InputFrame, observe: bool) -> Result<(), String> {
        if !observe {
            self.core.run_frame(input, &mut Null);
            return Ok(());
        }
        if !self.installed {
            self.core
                .set_rebuild_atlas(Some(Box::new(self.atlas.clone())))
                .map_err(|e| format!("{e:?}"))?;
            self.installed = true;
        }
        let pages = nes_pages(&self.core);
        self.reader.set_resolver(Box::new(move |a| {
            let page = usize::try_from(a.checked_sub(0x8000)? >> 12).ok()?;
            pages
                .get(page)
                .copied()
                .flatten()
                .map(|base| base + (a & 0xFFF))
        }));
        if self
            .core
            .run_frame_observed(input, &mut Null, &mut self.reader)
        {
            Ok(())
        } else {
            Err("this core cannot be observed".to_string())
        }
    }

    /// The CPU-address to ROM-offset map for the frame about to run (the bank
    /// windows are read from the mapper now), for a second observer.
    #[must_use]
    pub fn resolver(&self) -> Resolver {
        let pages = nes_pages(&self.core);
        Box::new(move |a| {
            let page = usize::try_from(a.checked_sub(0x8000)? >> 12).ok()?;
            pages
                .get(page)
                .copied()
                .flatten()
                .map(|base| base + (a & 0xFFF))
        })
    }

    /// Run one observed frame under a caller's observer instead of the
    /// data-region reader (W38-03). The caller installs [`Self::resolver`] first.
    ///
    /// # Errors
    /// The atlas does not fit the cartridge, or the core cannot be observed.
    pub fn frame_with(
        &mut self,
        input: &InputFrame,
        observer: &mut dyn StepObserver,
    ) -> Result<(), String> {
        if !self.installed {
            self.core
                .set_rebuild_atlas(Some(Box::new(self.atlas.clone())))
                .map_err(|e| format!("{e:?}"))?;
            self.installed = true;
        }
        if self.core.run_frame_observed(input, &mut Null, observer) {
            Ok(())
        } else {
            Err("this core cannot be observed".to_string())
        }
    }

    /// What has been seen since the last take.
    pub fn take_facts(&mut self) -> DataFacts {
        self.reader.take_facts()
    }
}

/// A SNES cartridge under the finder.
pub struct SnesDriver {
    pub core: SnesCore,
    isa: Wdc65816,
    atlas: Atlas,
    reader: DataReader,
    installed: bool,
}

impl SnesDriver {
    /// # Errors
    /// The image does not load or has no SNES header.
    pub fn new(raw: &[u8], atlas: &Atlas) -> Result<Self, String> {
        let rf_cart::Cartridge::Snes { header, .. } =
            rf_cart::Cartridge::load(raw).map_err(|e| e.to_string())?
        else {
            return Err("not a SNES cartridge".to_string());
        };
        let rom_len = raw.len() - if header.had_copier_header { 512 } else { 0 };
        let isa = Wdc65816::for_board(header.map_mode, rom_len, header.coprocessor);
        let mut core = SnesCore::load(raw).map_err(|e| e.to_string())?;
        core.reset(ResetKind::Hard);
        let mut reader = DataReader::new(Platform::Snes);
        let isa_for_reader = isa;
        reader.set_resolver(Box::new(move |a| isa_for_reader.rom_offset(a)));
        Ok(Self {
            core,
            isa,
            atlas: atlas.clone(),
            reader,
            installed: false,
        })
    }

    /// Run one frame (the pad words are latched into the bus first, as the
    /// app does), observed or plain.
    ///
    /// # Errors
    /// The atlas does not fit the cartridge.
    pub fn frame(&mut self, input: &InputFrame, observe: bool) -> Result<(), String> {
        let ports = &mut self.core.system_mut().bus.joypads.ports;
        for (slot, word) in ports.iter_mut().zip(input.ports) {
            *slot = word;
        }
        if !observe {
            self.core.run_frame(input, &mut Null);
            return Ok(());
        }
        if !self.installed {
            self.core
                .set_rebuild_atlas(Some(Box::new(self.atlas.clone())))
                .map_err(|e| format!("{e:?}"))?;
            self.installed = true;
        }
        if self
            .core
            .run_frame_observed(input, &mut Null, &mut self.reader)
        {
            Ok(())
        } else {
            Err("this core cannot be observed".to_string())
        }
    }

    /// The fixed CPU-address to ROM-offset map, for a second observer.
    #[must_use]
    pub fn resolver(&self) -> Resolver {
        let isa = self.isa;
        Box::new(move |a| isa.rom_offset(a))
    }

    /// Run one observed frame under a caller's observer (W38-03); the pad
    /// words are latched first, as in [`Self::frame`].
    ///
    /// # Errors
    /// The atlas does not fit the cartridge, or the core cannot be observed.
    pub fn frame_with(
        &mut self,
        input: &InputFrame,
        observer: &mut dyn StepObserver,
    ) -> Result<(), String> {
        let ports = &mut self.core.system_mut().bus.joypads.ports;
        for (slot, word) in ports.iter_mut().zip(input.ports) {
            *slot = word;
        }
        if !self.installed {
            self.core
                .set_rebuild_atlas(Some(Box::new(self.atlas.clone())))
                .map_err(|e| format!("{e:?}"))?;
            self.installed = true;
        }
        if self.core.run_frame_observed(input, &mut Null, observer) {
            Ok(())
        } else {
            Err("this core cannot be observed".to_string())
        }
    }

    pub fn take_facts(&mut self) -> DataFacts {
        self.reader.take_facts()
    }
}
