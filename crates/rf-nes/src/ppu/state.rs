//! PPU serialization: the `PPU_`, `VRAM`, `OAM_` and `CGRM` chunk payloads
//! (ticket W2-04). See `crate::state`'s module doc for the encoding rules,
//! the compiler-enforced completeness trick, and the frame-boundary rule
//! this module leans on twice.

use rf_cart::Mirroring;
use rf_core_api::StateError;

use crate::ppu::{EvaluatedSprite, Ppu, SpriteUnit};
use crate::state::{StateIn, StateOut};

/// Upper bound for the CHR-RAM block, so a corrupt length cannot make the
/// loader allocate wildly: 8 KiB is the standard CHR-RAM size and 256 KiB is
/// far above anything a mapper this crate emulates exposes.
const MAX_CHR_BYTES: usize = 256 * 1024;
/// Overlay sprite lists are per-scanline and bounded by OAM (64 sprites);
/// the cap is deliberately loose but finite.
const MAX_OVERLAY_SPRITES: usize = 64;

impl Ppu {
    /// The `PPU_` chunk: every field that is neither one of the four
    /// memories (which get their own chunks) nor per-scanline scratch.
    pub(crate) fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        // Exhaustive, no `..` — see `crate::state`'s module doc.
        let Ppu {
            ctrl,
            mask,
            status,
            oam_addr,
            oam: _, // OAM_ chunk
            v,
            t,
            x,
            w,
            read_buffer,
            chr: _,        // VRAM chunk (only when it is CHR RAM)
            chr_is_ram: _, // fixed by the cartridge, not machine state
            vram: _,       // VRAM chunk
            palette: _,    // CGRM chunk
            mirroring,
            bg_pattern_shift_lo,
            bg_pattern_shift_hi,
            bg_attr_shift_lo,
            bg_attr_shift_hi,
            nt_latch,
            at_latch,
            pt_lo_latch,
            pt_hi_latch,
            scanline,
            dot,
            frame_is_odd,
            render_enable_pipe,
            frame_count,
            suppress_vblank_this_frame,
            secondary_oam,
            secondary_oam_count,
            active_sprites,
            active_sprite_count,
            sprite_pattern_lo_latch,
            sprite_overlay_enabled,
            overlay_sprites,
            overlay_active_sprites,
            // Per-scanline scratch, not state: every element of both line
            // buffers is written before the row that reads them is pushed
            // (`sprites.rs`'s `output_pixel` writes `line_buffer[x]` for
            // every visible dot), so restoring them would restore values
            // that are overwritten before they can be observed.
            overlay_line_buffer: _,
            line_buffer: _,
            // Output, not state — refused below rather than dropped.
            completed,
            dot_clock,
            a12_low_since,
            pending_a12_edges,
            // Caller configuration, not machine state: the event mask is
            // what the *app* subscribed to (`CoreConfig`), so restoring it
            // from a file would silently change which events a running
            // session emits. `crate::ppu`'s own doc calls it a
            // subscription, and FR-CORE-006 makes it the caller's choice.
            event_mask: _,
            events,
        } = self;

        if !completed.is_empty() || !events.is_empty() {
            return Err(StateError::Corrupt(format!(
                "save_state is frame-boundary only (docs/design/SAVE_STATES.md §2): {} \
                 undrained scanline(s) and {} undrained event(s) would be lost -- drain the \
                 sink before saving",
                completed.len(),
                events.len()
            )));
        }

        out.u8(*ctrl)?;
        out.u8(*mask)?;
        out.u8(*status)?;
        out.u8(*oam_addr)?;
        out.u16(*v)?;
        out.u16(*t)?;
        out.u8(*x)?;
        out.bool(*w)?;
        out.u8(*read_buffer)?;
        out.u8(encode_mirroring(*mirroring))?;
        out.u16(*bg_pattern_shift_lo)?;
        out.u16(*bg_pattern_shift_hi)?;
        out.u16(*bg_attr_shift_lo)?;
        out.u16(*bg_attr_shift_hi)?;
        out.u8(*nt_latch)?;
        out.u8(*at_latch)?;
        out.u8(*pt_lo_latch)?;
        out.u8(*pt_hi_latch)?;
        out.u16(*scanline)?;
        out.u16(*dot)?;
        out.bool(*frame_is_odd)?;
        out.u8(*render_enable_pipe)?;
        out.u64(*frame_count)?;
        out.bool(*suppress_vblank_this_frame)?;
        for sprite in secondary_oam {
            save_evaluated_sprite(out, sprite)?;
        }
        out.u8(*secondary_oam_count)?;
        for sprite in active_sprites {
            save_sprite_unit(out, sprite)?;
        }
        out.u8(*active_sprite_count)?;
        out.u8(*sprite_pattern_lo_latch)?;
        out.bool(*sprite_overlay_enabled)?;
        save_sprite_list(out, overlay_sprites)?;
        save_sprite_list(out, overlay_active_sprites)?;
        out.u64(*dot_clock)?;
        out.opt_u64(*a12_low_since)?;
        out.u32(*pending_a12_edges)
    }

    /// Restores the `PPU_` chunk. The two scratch line buffers and the two
    /// output queues are left as they are (see [`Ppu::save_state`]); the
    /// queues are cleared, because whatever they held belongs to the
    /// session being replaced, not to the one being restored.
    pub(crate) fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.ctrl = inp.u8()?;
        self.mask = inp.u8()?;
        self.status = inp.u8()?;
        self.oam_addr = inp.u8()?;
        self.v = inp.u16()?;
        self.t = inp.u16()?;
        self.x = inp.u8()?;
        self.w = inp.bool()?;
        self.read_buffer = inp.u8()?;
        self.mirroring = decode_mirroring(inp.u8()?)?;
        self.bg_pattern_shift_lo = inp.u16()?;
        self.bg_pattern_shift_hi = inp.u16()?;
        self.bg_attr_shift_lo = inp.u16()?;
        self.bg_attr_shift_hi = inp.u16()?;
        self.nt_latch = inp.u8()?;
        self.at_latch = inp.u8()?;
        self.pt_lo_latch = inp.u8()?;
        self.pt_hi_latch = inp.u8()?;
        self.scanline = inp.u16()?;
        self.dot = inp.u16()?;
        self.frame_is_odd = inp.bool()?;
        self.render_enable_pipe = inp.u8()?;
        self.frame_count = inp.u64()?;
        self.suppress_vblank_this_frame = inp.bool()?;
        for index in 0..self.secondary_oam.len() {
            self.secondary_oam[index] = load_evaluated_sprite(inp)?;
        }
        self.secondary_oam_count = inp.u8()?;
        for index in 0..self.active_sprites.len() {
            self.active_sprites[index] = load_sprite_unit(inp)?;
        }
        self.active_sprite_count = inp.u8()?;
        self.sprite_pattern_lo_latch = inp.u8()?;
        self.sprite_overlay_enabled = inp.bool()?;
        self.overlay_sprites = load_sprite_list(inp)?;
        self.overlay_active_sprites = load_sprite_list(inp)?;
        self.dot_clock = inp.u64()?;
        self.a12_low_since = inp.opt_u64()?;
        self.pending_a12_edges = inp.u32()?;
        self.completed.clear();
        self.events.clear();
        Ok(())
    }

    /// The `VRAM` chunk: the 4 KiB of nametable RAM, followed by the CHR
    /// block — present only when the cartridge has CHR **RAM**, since CHR
    /// ROM is cartridge data that reloading the ROM restores.
    pub(crate) fn save_vram(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        out.bytes(&self.vram)?;
        out.bool(self.chr_is_ram)?;
        if self.chr_is_ram {
            out.block(&self.chr)?;
        }
        Ok(())
    }

    pub(crate) fn load_vram(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        inp.bytes(&mut self.vram)?;
        let had_chr_ram = inp.bool()?;
        if had_chr_ram {
            let chr = inp.block(MAX_CHR_BYTES)?;
            if !self.chr_is_ram {
                return Err(StateError::Corrupt(
                    "state carries CHR RAM but the loaded cartridge has CHR ROM".to_string(),
                ));
            }
            if chr.len() != self.chr.len() {
                return Err(StateError::Corrupt(format!(
                    "state CHR RAM is {} bytes, the loaded cartridge has {}",
                    chr.len(),
                    self.chr.len()
                )));
            }
            self.chr = chr;
        } else if self.chr_is_ram {
            return Err(StateError::Corrupt(
                "state carries no CHR RAM but the loaded cartridge has it".to_string(),
            ));
        }
        Ok(())
    }

    /// The `OAM_` chunk: the 256-byte sprite table.
    pub(crate) fn save_oam(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        out.bytes(&self.oam)
    }

    pub(crate) fn load_oam(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        inp.bytes(&mut self.oam)
    }

    /// The `CGRM` chunk: 32 bytes of palette RAM.
    pub(crate) fn save_palette(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        out.bytes(&self.palette)
    }

    pub(crate) fn load_palette(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        inp.bytes(&mut self.palette)
    }
}

/// Discriminants are wire format (CONTRACTS §3): append, never renumber.
fn encode_mirroring(m: Mirroring) -> u8 {
    match m {
        Mirroring::Horizontal => 0,
        Mirroring::Vertical => 1,
        Mirroring::FourScreen => 2,
        Mirroring::OneScreenLower => 3,
        Mirroring::OneScreenUpper => 4,
    }
}

fn decode_mirroring(v: u8) -> Result<Mirroring, StateError> {
    Ok(match v {
        0 => Mirroring::Horizontal,
        1 => Mirroring::Vertical,
        2 => Mirroring::FourScreen,
        3 => Mirroring::OneScreenLower,
        4 => Mirroring::OneScreenUpper,
        other => {
            return Err(StateError::Corrupt(format!(
                "unknown Mirroring discriminant {other} in PPU_ chunk"
            )))
        }
    })
}

fn save_evaluated_sprite(out: &mut StateOut<'_>, s: &EvaluatedSprite) -> Result<(), StateError> {
    let EvaluatedSprite {
        y,
        tile,
        attr,
        x,
        oam_index,
    } = s;
    out.u8(*y)?;
    out.u8(*tile)?;
    out.u8(*attr)?;
    out.u8(*x)?;
    out.u8(*oam_index)
}

fn load_evaluated_sprite(inp: &mut StateIn<'_>) -> Result<EvaluatedSprite, StateError> {
    Ok(EvaluatedSprite {
        y: inp.u8()?,
        tile: inp.u8()?,
        attr: inp.u8()?,
        x: inp.u8()?,
        oam_index: inp.u8()?,
    })
}

fn save_sprite_unit(out: &mut StateOut<'_>, s: &SpriteUnit) -> Result<(), StateError> {
    let SpriteUnit {
        pattern_lo,
        pattern_hi,
        attr,
        x,
        oam_index,
    } = s;
    out.u8(*pattern_lo)?;
    out.u8(*pattern_hi)?;
    out.u8(*attr)?;
    out.u8(*x)?;
    out.u8(*oam_index)
}

fn load_sprite_unit(inp: &mut StateIn<'_>) -> Result<SpriteUnit, StateError> {
    Ok(SpriteUnit {
        pattern_lo: inp.u8()?,
        pattern_hi: inp.u8()?,
        attr: inp.u8()?,
        x: inp.u8()?,
        oam_index: inp.u8()?,
    })
}

fn save_sprite_list(out: &mut StateOut<'_>, list: &[SpriteUnit]) -> Result<(), StateError> {
    let len = u8::try_from(list.len()).map_err(|_| {
        StateError::Corrupt(format!(
            "overlay sprite list of {} exceeds the 64-sprite OAM bound",
            list.len()
        ))
    })?;
    out.u8(len)?;
    for sprite in list {
        save_sprite_unit(out, sprite)?;
    }
    Ok(())
}

fn load_sprite_list(inp: &mut StateIn<'_>) -> Result<Vec<SpriteUnit>, StateError> {
    let len = inp.u8()? as usize;
    if len > MAX_OVERLAY_SPRITES {
        return Err(StateError::Corrupt(format!(
            "overlay sprite list length {len} exceeds the {MAX_OVERLAY_SPRITES}-sprite bound"
        )));
    }
    let mut list = Vec::with_capacity(len);
    for _ in 0..len {
        list.push(load_sprite_unit(inp)?);
    }
    Ok(list)
}
