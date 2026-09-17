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
            // Ticket W11-05: an OBSERVATION facility, not machine state.
            // Deliberately not serialised — whether someone is watching
            // tiles is a property of this session's UI, and a save made
            // with an HD pack open must not turn the capture on for
            // whoever loads it. The tiles themselves are one frame's
            // scratch and are rebuilt before anyone can read them.
            tile_capture: _,
            drawn_tiles: _,
            completed_tiles: _,
            drawn_sprites: _,
            completed_sprites: _,
            vram: _,    // VRAM chunk
            palette: _, // CGRM chunk
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
            decay,
            decay_ttl,
            render_enable_pipe,
            frame_count,
            suppress_vblank_this_frame,
            secondary_oam,
            secondary_oam_count,
            active_sprites,
            active_sprite_count,
            sprite_pattern_lo_latch,
            // ENHANCEMENT STATE, NOT CORE STATE -- excluded deliberately,
            // and the exclusion is load-bearing. The overlay channel
            // (ticket W3-05a: `sprite_overlay_enabled`, `overlay_sprites`,
            // `overlay_active_sprites`, `overlay_line_buffer`) exists only
            // when Enhanced mode asks for it, and `sprites.rs`'s own doc
            // establishes it is structurally incapable of perturbing
            // accuracy state. Serializing it here would put mode-dependent
            // bytes into a CORE chunk and break ARCHITECTURE §3's
            // determinism invariant outright -- "Accuracy vs Enhanced must
            // produce identical core state hashes for identical inputs" --
            // which is exactly what happened when this ticket first widened
            // `EmuStepper::state_hash` to cover the whole PPU:
            // `mode_invariant_corpus` went red on
            // `w3-05a-sprite-limit-bypass-overlay` with identical VIDEO
            // hashes and differing STATE hashes. Enhancement state belongs
            // in the `ENHC` chunk (`rf-enhance`'s, per SAVE_STATES.md §2),
            // which an Accuracy-mode session skips with a warning.
            sprite_overlay_enabled: _,
            overlay_sprites: _,
            overlay_active_sprites: _,
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
            last_2007_read_dot,
            last_2007_read_value,
            a12_low_since,
            pending_a12_edges,
            // Caller configuration, not machine state: the event mask is
            // what the *app* subscribed to (`CoreConfig`), so restoring it
            // from a file would silently change which events a running
            // session emits. `crate::ppu`'s own doc calls it a
            // subscription, and FR-CORE-006 makes it the caller's choice.
            event_mask: _,
            // Caller configuration for the same reason (ticket W13-02e):
            // watchpoints are a debugger setting, not something the
            // machine evolved into. Restoring them from a file would
            // carry whoever saved the state's watchpoints into your
            // session, and worse, would make a save state's contents
            // depend on what someone was debugging when they wrote it.
            watches: _,
            // Caller configuration too, and excluded for the same reason
            // (ticket W3-07): `accuracy_mode` is what the *app* asked for
            // via `CoreConfig`, not something the machine evolved into.
            // Restoring it from a file would let a saved state silently
            // switch a running session's mode -- and since the mode
            // changes observable reads, a state saved in Compatibility
            // would drag a session out of Accuracy, which law 6 ("a fresh
            // install boots in Accuracy Mode") exists to prevent.
            accuracy_mode: _,
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
        // The PPU's decay register and its per-bit clocks (ticket W2-19).
        // Genuinely machine state, not scratch: `$2000`-`$2006` reads
        // return `decay` directly, so a save/load that dropped it would
        // change what the very next read yields. Both go in the `PPU_`
        // chunk, which is why `PPU_`'s registry version moved to 2.
        out.u8(*decay)?;
        for ttl in decay_ttl {
            out.u8(*ttl)?;
        }
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
        out.u64(*dot_clock)?;
        // Ticket W2-01d: decides whether the next `$2007` read is the
        // second half of a double read and therefore reports a held
        // value. Dropping it would let a restored machine return a fresh
        // byte where the saved one returned a stale one.
        out.opt_u64(*last_2007_read_dot)?;
        out.u8(*last_2007_read_value)?;
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
        self.decay = inp.u8()?;
        for bit in 0..8 {
            self.decay_ttl[bit] = inp.u8()?;
        }
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
        self.dot_clock = inp.u64()?;
        self.last_2007_read_dot = inp.opt_u64()?;
        self.last_2007_read_value = inp.u8()?;
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
        // Appended (ticket W14-12): discriminant 5 in the low nibble, the
        // four page bits in the high nibble, table 0 at bit 4.
        Mirroring::PerTable(pages) => {
            5 | (pages[0] & 1) << 4
                | (pages[1] & 1) << 5
                | (pages[2] & 1) << 6
                | (pages[3] & 1) << 7
        }
    }
}

fn decode_mirroring(v: u8) -> Result<Mirroring, StateError> {
    Ok(match v {
        0 => Mirroring::Horizontal,
        1 => Mirroring::Vertical,
        2 => Mirroring::FourScreen,
        3 => Mirroring::OneScreenLower,
        4 => Mirroring::OneScreenUpper,
        v if v & 0x0F == 5 => {
            Mirroring::PerTable([(v >> 4) & 1, (v >> 5) & 1, (v >> 6) & 1, (v >> 7) & 1])
        }
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
