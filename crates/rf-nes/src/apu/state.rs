//! APU serialization: the `APU_` chunk payload (ticket W2-04). See
//! `crate::state`'s module doc for the encoding rules and the
//! compiler-enforced completeness trick.
//!
//! Every channel and shared unit writes its own fields, so a new field
//! anywhere in `crate::apu` breaks this build until it is handled — which
//! matters more here than anywhere else in the crate: the APU's phase state
//! (the frame counter's cycle, the CPU/2 parity, each timer's countdown) is
//! invisible in any frame's output, so dropping one from a save state would
//! surface only as a determinism divergence thousands of frames later.

use rf_core_api::StateError;

use crate::apu::dmc::Dmc;
use crate::apu::frame_counter::FrameCounter;
use crate::apu::noise::Noise;
use crate::apu::pulse::Pulse;
use crate::apu::triangle::Triangle;
use crate::apu::units::{Envelope, LengthCounter, Sweep};
use crate::apu::Apu;
use crate::state::{StateIn, StateOut};

impl Apu {
    pub(crate) fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        let Apu {
            pulse1,
            pulse2,
            triangle,
            noise,
            dmc,
            frame_counter,
            // AUDIO OUTPUT, NOT MACHINE STATE (ticket W2-05) — the same
            // ruling `crate::ppu`'s completed-scanline queue gets, and for
            // the same reason: these three carry the decimator's phase and
            // the samples produced since the last drain, none of which any
            // future *machine* behaviour reads. Restoring them would make a
            // save state's bytes depend on how recently the host drained
            // audio, which is a property of the app, not of the console.
            // `load_state` resets the decimator instead, so a restored
            // machine starts a clean output period.
            sample_accumulator: _,
            sample_phase: _,
            samples: _,
            // Ticket W4-10b: the per-channel scope streams and their
            // accumulators fall under the identical ruling — output, not
            // state. `channel_capture` is caller CONFIGURATION (the same
            // class as `accuracy_mode` and `event_mask`), and restoring
            // it from a file would let a state saved with the scopes open
            // turn them on in a session that never asked for them.
            channel_samples: _,
            channel_accumulators: _,
            channel_capture: _,
            on_apu_cycle,
            irq_line_delayed,
        } = self;
        pulse1.save_state(out)?;
        pulse2.save_state(out)?;
        triangle.save_state(out)?;
        noise.save_state(out)?;
        dmc.save_state(out)?;
        frame_counter.save_state(out)?;
        out.bool(*on_apu_cycle)?;
        // Ticket W2-21. Derived from the two IRQ flags, but NOT
        // reconstructible at load time: it is deliberately one cycle
        // behind them, and `Cpu`'s interrupt sampler reads
        // `CpuBus::irq_line` before the first `tick` after a restore.
        // Recomputing it from the current flags would make a restored
        // machine see the line a cycle earlier than the saved one did,
        // which is precisely the timing this field exists to model.
        out.bool(*irq_line_delayed)
    }

    pub(crate) fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.pulse1.load_state(inp)?;
        self.pulse2.load_state(inp)?;
        self.triangle.load_state(inp)?;
        self.noise.load_state(inp)?;
        self.dmc.load_state(inp)?;
        self.frame_counter.load_state(inp)?;
        self.on_apu_cycle = inp.bool()?;
        self.irq_line_delayed = inp.bool()?;
        self.reset_audio_output();
        Ok(())
    }
}

impl Envelope {
    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        let Envelope {
            volume,
            constant_volume,
            loop_flag,
            start,
            divider,
            decay_level,
        } = self;
        out.u8(*volume)?;
        out.bool(*constant_volume)?;
        out.bool(*loop_flag)?;
        out.bool(*start)?;
        out.u8(*divider)?;
        out.u8(*decay_level)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.volume = inp.u8()?;
        self.constant_volume = inp.bool()?;
        self.loop_flag = inp.bool()?;
        self.start = inp.bool()?;
        self.divider = inp.u8()?;
        self.decay_level = inp.u8()?;
        Ok(())
    }
}

impl LengthCounter {
    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        let LengthCounter {
            enabled,
            halt,
            counter,
        } = self;
        out.bool(*enabled)?;
        out.bool(*halt)?;
        out.u8(*counter)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.enabled = inp.bool()?;
        self.halt = inp.bool()?;
        self.counter = inp.u8()?;
        Ok(())
    }
}

impl Sweep {
    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        let Sweep {
            enabled,
            period,
            negate,
            shift,
            // Wiring, not state: which pulse channel this unit belongs to
            // is fixed at construction (`Sweep::new`).
            ones_complement: _,
            reload,
            divider,
        } = self;
        out.bool(*enabled)?;
        out.u8(*period)?;
        out.bool(*negate)?;
        out.u8(*shift)?;
        out.bool(*reload)?;
        out.u8(*divider)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.enabled = inp.bool()?;
        self.period = inp.u8()?;
        self.negate = inp.bool()?;
        self.shift = inp.u8()?;
        self.reload = inp.bool()?;
        self.divider = inp.u8()?;
        Ok(())
    }
}

impl Pulse {
    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        let Pulse {
            envelope,
            length,
            sweep,
            duty,
            period,
            timer,
            sequence_step,
        } = self;
        envelope.save_state(out)?;
        length.save_state(out)?;
        sweep.save_state(out)?;
        out.u8(*duty)?;
        out.u16(*period)?;
        out.u16(*timer)?;
        out.u8(*sequence_step)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.envelope.load_state(inp)?;
        self.length.load_state(inp)?;
        self.sweep.load_state(inp)?;
        self.duty = inp.u8()?;
        self.period = inp.u16()?;
        self.timer = inp.u16()?;
        self.sequence_step = inp.u8()?;
        Ok(())
    }
}

impl Triangle {
    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        let Triangle {
            length,
            control,
            linear_reload_value,
            linear_counter,
            linear_reload_flag,
            period,
            timer,
            sequence_step,
        } = self;
        length.save_state(out)?;
        out.bool(*control)?;
        out.u8(*linear_reload_value)?;
        out.u8(*linear_counter)?;
        out.bool(*linear_reload_flag)?;
        out.u16(*period)?;
        out.u16(*timer)?;
        out.u8(*sequence_step)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.length.load_state(inp)?;
        self.control = inp.bool()?;
        self.linear_reload_value = inp.u8()?;
        self.linear_counter = inp.u8()?;
        self.linear_reload_flag = inp.bool()?;
        self.period = inp.u16()?;
        self.timer = inp.u16()?;
        self.sequence_step = inp.u8()?;
        Ok(())
    }
}

impl Noise {
    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        let Noise {
            envelope,
            length,
            mode,
            period,
            timer,
            shift_register,
        } = self;
        envelope.save_state(out)?;
        length.save_state(out)?;
        out.bool(*mode)?;
        out.u16(*period)?;
        out.u16(*timer)?;
        out.u16(*shift_register)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.envelope.load_state(inp)?;
        self.length.load_state(inp)?;
        self.mode = inp.bool()?;
        self.period = inp.u16()?;
        self.timer = inp.u16()?;
        self.shift_register = inp.u16()?;
        Ok(())
    }
}

impl Dmc {
    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        let Dmc {
            irq_enabled,
            loop_flag,
            irq_flag,
            period,
            timer,
            sample_address,
            sample_length,
            current_address,
            bytes_remaining,
            sample_buffer,
            fetch_pending,
            fetch_is_load,
            shift_register,
            bits_remaining,
            silence,
            output_level,
        } = self;
        out.bool(*irq_enabled)?;
        out.bool(*loop_flag)?;
        out.bool(*irq_flag)?;
        out.u16(*period)?;
        out.u16(*timer)?;
        out.u16(*sample_address)?;
        out.u16(*sample_length)?;
        out.u16(*current_address)?;
        out.u16(*bytes_remaining)?;
        out.bool(sample_buffer.is_some())?;
        out.u8(sample_buffer.unwrap_or(0))?;
        out.bool(*fetch_pending)?;
        out.bool(*fetch_is_load)?;
        out.u8(*shift_register)?;
        out.u8(*bits_remaining)?;
        out.bool(*silence)?;
        out.u8(*output_level)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.irq_enabled = inp.bool()?;
        self.loop_flag = inp.bool()?;
        self.irq_flag = inp.bool()?;
        self.period = inp.u16()?;
        self.timer = inp.u16()?;
        self.sample_address = inp.u16()?;
        self.sample_length = inp.u16()?;
        self.current_address = inp.u16()?;
        self.bytes_remaining = inp.u16()?;
        let buffered = inp.bool()?;
        let byte = inp.u8()?;
        self.sample_buffer = buffered.then_some(byte);
        self.fetch_pending = inp.bool()?;
        self.fetch_is_load = inp.bool()?;
        self.shift_register = inp.u8()?;
        self.bits_remaining = inp.u8()?;
        self.silence = inp.bool()?;
        self.output_level = inp.u8()?;
        Ok(())
    }
}

impl FrameCounter {
    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        let FrameCounter {
            cycle,
            mode_five_step,
            inhibit_irq,
            irq_flag,
            pending_reset,
            pending_mode_five_step,
        } = self;
        out.u32(*cycle)?;
        out.bool(*mode_five_step)?;
        out.bool(*inhibit_irq)?;
        out.bool(*irq_flag)?;
        out.bool(pending_reset.is_some())?;
        out.u8(pending_reset.unwrap_or(0))?;
        out.bool(*pending_mode_five_step)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.cycle = inp.u32()?;
        self.mode_five_step = inp.bool()?;
        self.inhibit_irq = inp.bool()?;
        self.irq_flag = inp.bool()?;
        let pending = inp.bool()?;
        let delay = inp.u8()?;
        self.pending_reset = pending.then_some(delay);
        self.pending_mode_five_step = inp.bool()?;
        Ok(())
    }
}
