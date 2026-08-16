//! Unit tests for the APU (ticket W2-01a). The real-ROM gate
//! (`apu_test`) lives in `crate::apu::tests::blargg_rom`; everything else
//! here pins one nesdev-documented behavior each, so a regression names the
//! unit that broke rather than just "the ROM went red".

mod blargg_rom;
mod channels;
mod dmc_dma_rom;
mod frame_counter;
mod mixer;
mod status;
