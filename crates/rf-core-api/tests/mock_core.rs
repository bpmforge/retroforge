//! Mock-core contract tests (ticket W0-04, CONTRACTS §1 proof obligation:
//! "mock-core contract tests (W0-04)").
//!
//! Exercises `EmulatorCore` + `CoreSink` + `StateView` + `CoreEvent` +
//! `InputFrame` + `Step` + `EventMask` end to end through a small in-file
//! mock, without any real console behavior.

use rf_core_api::{
    CartImage, CoreConfig, CoreError, CoreEvent, CoreSink, EmulatorCore, EventMask, InputFrame,
    PixelLayer, PpuPixel, ResetKind, StateError, StateReader, StateView, StateWriter, Step,
    StepResult,
};

const WRAM_SIZE: usize = 8;
const VRAM_SIZE: usize = 8;
const CGRAM_SIZE: usize = 4;
const OAM_SIZE: usize = 4;
const PPU_REGS_SIZE: usize = 4;
const CPU_REGS_SIZE: usize = 6;
const MAPPER_STATE_SIZE: usize = 2;
const SCANLINES: u16 = 4;
const PIXELS_PER_LINE: usize = 4;

/// Deterministic pixel pattern for scanline `y`, touching every
/// `PixelLayer` variant plus both states of `sprite_id`/`dropped_by_limit`
/// (FR-CORE-004 proof).
fn make_scanline_pixels(y: u16) -> [PpuPixel; PIXELS_PER_LINE] {
    let base = y as u8;
    [
        PpuPixel {
            palette_index: 0,
            layer: PixelLayer::Backdrop,
            sprite_id: None,
            priority: 0,
        },
        PpuPixel {
            palette_index: base.wrapping_add(1),
            layer: PixelLayer::Background(0),
            sprite_id: None,
            priority: 1,
        },
        PpuPixel {
            palette_index: base.wrapping_add(2),
            layer: PixelLayer::Sprite,
            sprite_id: Some(3),
            priority: 2,
        },
        PpuPixel {
            palette_index: base.wrapping_add(3),
            layer: PixelLayer::Sprite,
            sprite_id: Some(9),
            priority: 3,
        },
    ]
}

/// A tiny, deterministic mock core that exercises every `EmulatorCore`
/// method without simulating any real console.
struct MockCore {
    loaded: bool,
    last_reset: Option<ResetKind>,
    config: CoreConfig,
    scanline_cursor: u16,
    /// Incremented at every `CoreEvent::X` construction site in this mock,
    /// immediately before the value is handed to `sink.event`. Proves the
    /// no-cost path independently of what the sink does with it.
    events_constructed: u32,
    cpu_regs: [u8; CPU_REGS_SIZE],
    wram: [u8; WRAM_SIZE],
    vram: [u8; VRAM_SIZE],
    cgram: [u8; CGRAM_SIZE],
    oam: [u8; OAM_SIZE],
    ppu_regs: [u8; PPU_REGS_SIZE],
    mapper_state: [u8; MAPPER_STATE_SIZE],
}

impl MockCore {
    fn new() -> Self {
        MockCore {
            loaded: false,
            last_reset: None,
            config: CoreConfig::default(),
            scanline_cursor: 0,
            events_constructed: 0,
            cpu_regs: [1, 2, 3, 4, 5, 6],
            wram: [0xAA; WRAM_SIZE],
            vram: [0xBB; VRAM_SIZE],
            cgram: [0xCC; CGRAM_SIZE],
            oam: [0xDD; OAM_SIZE],
            ppu_regs: [0xEE; PPU_REGS_SIZE],
            mapper_state: [0xFF; MAPPER_STATE_SIZE],
        }
    }

    fn emit_one_frame(&mut self, sink: &mut dyn CoreSink) {
        if self.config.event_mask.is_subscribed(EventMask::FRAME_START) {
            self.events_constructed += 1;
            sink.event(CoreEvent::FrameStart);
        }
        for y in 0..SCANLINES {
            let pixels = make_scanline_pixels(y);
            sink.video_scanline(y, &pixels);
            if self.config.event_mask.is_subscribed(EventMask::SCANLINE) {
                self.events_constructed += 1;
                sink.event(CoreEvent::Scanline(y));
            }
        }
        sink.audio(&[0, 1, -1, 2]);
        if self.config.event_mask.is_subscribed(EventMask::MAPPER_IRQ) {
            self.events_constructed += 1;
            sink.event(CoreEvent::MapperIrq);
        }
        if self.config.event_mask.is_subscribed(EventMask::FRAME_END) {
            self.events_constructed += 1;
            sink.event(CoreEvent::FrameEnd);
        }
    }
}

impl EmulatorCore for MockCore {
    fn load(&mut self, cart: CartImage<'_>) -> Result<(), CoreError> {
        cart.validate()?;
        if cart.rom == [0xDE, 0xAD] {
            return Err(CoreError::UnsupportedMapper("mock: 0xDEAD marker".into()));
        }
        self.loaded = true;
        Ok(())
    }

    fn reset(&mut self, kind: ResetKind) {
        self.last_reset = Some(kind);
        if kind == ResetKind::Hard {
            self.wram = [0; WRAM_SIZE];
        }
    }

    fn run_frame(&mut self, _input: &InputFrame, sink: &mut dyn CoreSink) {
        self.emit_one_frame(sink);
    }

    fn step(&mut self, granularity: Step, sink: &mut dyn CoreSink) -> StepResult {
        match granularity {
            Step::Instruction => StepResult {
                cycles: 4,
                frame_complete: false,
            },
            Step::Scanline => {
                let y = self.scanline_cursor;
                let pixels = make_scanline_pixels(y);
                sink.video_scanline(y, &pixels);
                if self.config.event_mask.is_subscribed(EventMask::SCANLINE) {
                    self.events_constructed += 1;
                    sink.event(CoreEvent::Scanline(y));
                }
                self.scanline_cursor += 1;
                let frame_complete = self.scanline_cursor >= SCANLINES;
                if frame_complete {
                    self.scanline_cursor = 0;
                }
                StepResult {
                    cycles: 341,
                    frame_complete,
                }
            }
            Step::Frame => {
                self.emit_one_frame(sink);
                StepResult {
                    cycles: 341 * u64::from(SCANLINES),
                    frame_complete: true,
                }
            }
        }
    }

    fn save_state(&self, w: &mut dyn StateWriter) -> Result<(), StateError> {
        w.write_all(&self.cpu_regs)?;
        w.write_all(&self.wram)?;
        w.write_all(&self.vram)?;
        w.write_all(&self.cgram)?;
        w.write_all(&self.oam)?;
        w.write_all(&self.ppu_regs)?;
        w.write_all(&self.mapper_state)?;
        Ok(())
    }

    fn load_state(&mut self, r: &mut dyn StateReader) -> Result<(), StateError> {
        r.read_exact(&mut self.cpu_regs)?;
        r.read_exact(&mut self.wram)?;
        r.read_exact(&mut self.vram)?;
        r.read_exact(&mut self.cgram)?;
        r.read_exact(&mut self.oam)?;
        r.read_exact(&mut self.ppu_regs)?;
        r.read_exact(&mut self.mapper_state)?;
        Ok(())
    }

    fn state_view(&self) -> StateView<'_> {
        StateView {
            cpu_regs: &self.cpu_regs,
            wram: &self.wram,
            vram: &self.vram,
            cgram: &self.cgram,
            oam: &self.oam,
            ppu_regs: &self.ppu_regs,
            mapper_state: &self.mapper_state,
        }
    }

    fn config(&mut self) -> &mut CoreConfig {
        &mut self.config
    }

    fn peek(&self, addr: u32) -> u8 {
        // The mock's whole point is being predictable: a byte derived
        // from the address, so a caller can assert it got the address it
        // asked for rather than a constant.
        (addr & 0xFF) as u8
    }
}

/// Records every call made through `CoreSink`. If `forbidden` overlaps the
/// bit for an incoming event, panics — this is the "sink whose `event()`
/// panics" proof the ticket calls for: an event outside the subscribed
/// mask reaching the sink fails the test immediately.
#[derive(Default)]
struct RecordingSink {
    scanlines: Vec<(u16, Vec<PpuPixel>)>,
    audio_calls: Vec<Vec<i16>>,
    events: Vec<CoreEvent>,
    forbidden: EventMask,
}

fn bit_for(ev: &CoreEvent) -> EventMask {
    match ev {
        CoreEvent::FrameStart => EventMask::FRAME_START,
        CoreEvent::FrameEnd => EventMask::FRAME_END,
        CoreEvent::VblankStart => EventMask::VBLANK_START,
        CoreEvent::Scanline(_) => EventMask::SCANLINE,
        CoreEvent::OamRewrite => EventMask::OAM_REWRITE,
        CoreEvent::ScrollWrite { .. } => EventMask::SCROLL_WRITE,
        CoreEvent::MapperIrq => EventMask::MAPPER_IRQ,
        CoreEvent::DmaStart { .. } => EventMask::DMA_START,
        CoreEvent::MemWatch { .. } => EventMask::MEM_WATCH,
    }
}

impl CoreSink for RecordingSink {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        self.scanlines.push((y, pixels.to_vec()));
    }

    fn audio(&mut self, samples: &[i16]) {
        self.audio_calls.push(samples.to_vec());
    }

    fn event(&mut self, ev: CoreEvent) {
        let bit = bit_for(&ev);
        assert!(
            !self.forbidden.contains(bit),
            "event {ev:?} reached the sink despite being outside the subscribed mask"
        );
        self.events.push(ev);
    }
}

#[derive(Default)]
struct VecWriter {
    buf: Vec<u8>,
}

impl StateWriter for VecWriter {
    fn write_all(&mut self, buf: &[u8]) -> Result<(), StateError> {
        self.buf.extend_from_slice(buf);
        Ok(())
    }
}

struct SliceReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> SliceReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        SliceReader { data, pos: 0 }
    }
}

impl<'a> StateReader for SliceReader<'a> {
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), StateError> {
        let end = self.pos + buf.len();
        let Some(chunk) = self.data.get(self.pos..end) else {
            return Err(StateError::Io("unexpected end of state stream".into()));
        };
        buf.copy_from_slice(chunk);
        self.pos = end;
        Ok(())
    }
}

// ---- FR-CORE-001 / FR-CORE-005: EmulatorCore + Step ----------------------

#[test]
fn load_accepts_valid_rom_and_rejects_empty() {
    let mut core = MockCore::new();
    assert!(core.load(CartImage::from_rom(&[1, 2, 3])).is_ok());
    assert!(core.loaded);

    let mut fresh = MockCore::new();
    let err = fresh.load(CartImage::from_rom(&[])).unwrap_err();
    assert!(matches!(err, CoreError::InvalidImage(_)));
}

#[test]
fn load_surfaces_core_defined_mapper_error() {
    let mut core = MockCore::new();
    let err = core.load(CartImage::from_rom(&[0xDE, 0xAD])).unwrap_err();
    assert!(matches!(err, CoreError::UnsupportedMapper(_)));
}

#[test]
fn reset_soft_preserves_wram_hard_clears_it() {
    let mut core = MockCore::new();
    core.reset(ResetKind::Soft);
    assert_eq!(core.last_reset, Some(ResetKind::Soft));
    assert_eq!(core.state_view().wram, [0xAA; WRAM_SIZE]);

    core.reset(ResetKind::Hard);
    assert_eq!(core.last_reset, Some(ResetKind::Hard));
    assert_eq!(core.state_view().wram, [0; WRAM_SIZE]);
}

#[test]
fn run_frame_emits_scanlines_with_indexed_pixel_metadata() {
    let mut core = MockCore::new();
    core.config().event_mask = EventMask::ALL;
    let mut sink = RecordingSink::default();

    core.run_frame(&InputFrame::empty(), &mut sink);

    assert_eq!(sink.scanlines.len(), SCANLINES as usize);
    for (y, pixels) in &sink.scanlines {
        assert_eq!(pixels.len(), PIXELS_PER_LINE);
        assert_eq!(*pixels, make_scanline_pixels(*y));
    }
    // Metadata round-trips exactly: palette index, layer, sprite id,
    // priority, dropped_by_limit (FR-CORE-004) — spot-check scanline 0.
    let first = &sink.scanlines[0].1;
    assert_eq!(first[0].layer, PixelLayer::Backdrop);
    assert_eq!(first[1].layer, PixelLayer::Background(0));
    assert_eq!(first[2].sprite_id, Some(3));
    assert_eq!(first[3].sprite_id, Some(9));
    assert_eq!(sink.audio_calls, vec![vec![0, 1, -1, 2]]);
}

#[test]
fn step_covers_instruction_scanline_and_frame_granularities() {
    let mut core = MockCore::new();
    let mut sink = RecordingSink::default();

    let instr = core.step(Step::Instruction, &mut sink);
    assert!(!instr.frame_complete);
    assert!(instr.cycles > 0);

    for i in 0..SCANLINES {
        let r = core.step(Step::Scanline, &mut sink);
        let expect_complete = i == SCANLINES - 1;
        assert_eq!(r.frame_complete, expect_complete);
    }
    assert_eq!(sink.scanlines.len(), SCANLINES as usize);

    let mut sink2 = RecordingSink::default();
    let frame = core.step(Step::Frame, &mut sink2);
    assert!(frame.frame_complete);
    assert_eq!(sink2.scanlines.len(), SCANLINES as usize);
}

// ---- FR-CORE-006: EventMask subscription filtering ------------------------

#[test]
fn unsubscribed_events_are_never_constructed_or_sent() {
    let mut core = MockCore::new();
    core.config().event_mask = EventMask::NONE;
    // Any event reaching the sink at all is a bug: forbid everything.
    let mut sink = RecordingSink {
        forbidden: EventMask::ALL,
        ..RecordingSink::default()
    };

    core.run_frame(&InputFrame::empty(), &mut sink);

    assert!(
        sink.events.is_empty(),
        "no events should be sent when mask is NONE"
    );
    assert_eq!(
        core.events_constructed, 0,
        "no CoreEvent should be constructed when mask is NONE (FR-CORE-006 no-cost path)"
    );
    // Video/audio are unaffected by the event mask.
    assert_eq!(sink.scanlines.len(), SCANLINES as usize);
    assert_eq!(sink.audio_calls.len(), 1);
}

#[test]
fn event_mask_is_bit_precise_not_all_or_nothing() {
    let mut core = MockCore::new();
    core.config().event_mask = EventMask::FRAME_START.union(EventMask::MAPPER_IRQ);
    let mut sink = RecordingSink {
        forbidden: EventMask::SCANLINE.union(EventMask::FRAME_END),
        ..RecordingSink::default()
    };

    core.run_frame(&InputFrame::empty(), &mut sink);

    assert_eq!(
        sink.events,
        vec![CoreEvent::FrameStart, CoreEvent::MapperIrq]
    );
    assert_eq!(core.events_constructed, 2);
}

#[test]
fn full_mask_constructs_exactly_the_events_it_subscribes_to() {
    let mut core = MockCore::new();
    core.config().event_mask = EventMask::ALL;
    let mut sink = RecordingSink::default();

    core.run_frame(&InputFrame::empty(), &mut sink);

    // FrameStart + one Scanline per line + MapperIrq + FrameEnd.
    let expected = 2 + SCANLINES as u32 + 1;
    assert_eq!(core.events_constructed, expected);
    assert_eq!(sink.events.len(), expected as usize);
}

// ---- StateView / save_state / load_state -----------------------------------

#[test]
fn state_view_borrows_live_internal_state_read_only() {
    let mut core = MockCore::new();
    core.wram[0] = 0x42;
    let view = core.state_view();
    assert_eq!(view.cpu_regs, &[1, 2, 3, 4, 5, 6]);
    assert_eq!(view.wram[0], 0x42);
    assert_eq!(view.vram.len(), VRAM_SIZE);
    assert_eq!(view.cgram.len(), CGRAM_SIZE);
    assert_eq!(view.oam.len(), OAM_SIZE);
    assert_eq!(view.ppu_regs.len(), PPU_REGS_SIZE);
    assert_eq!(view.mapper_state.len(), MAPPER_STATE_SIZE);
}

#[test]
fn save_state_then_load_state_roundtrips() {
    let mut src = MockCore::new();
    src.wram[2] = 0x77;
    src.vram[0] = 0x99;

    let mut w = VecWriter::default();
    src.save_state(&mut w).unwrap();

    let mut dst = MockCore::new();
    let mut r = SliceReader::new(&w.buf);
    dst.load_state(&mut r).unwrap();

    assert_eq!(dst.state_view().wram, src.state_view().wram);
    assert_eq!(dst.state_view().vram, src.state_view().vram);
    assert_eq!(dst.state_view().cpu_regs, src.state_view().cpu_regs);
}

#[test]
fn load_state_reports_error_on_truncated_stream() {
    let mut core = MockCore::new();
    let short = [0u8; 2];
    let mut r = SliceReader::new(&short);
    let err = core.load_state(&mut r).unwrap_err();
    assert!(matches!(err, StateError::Io(_)));
}

// ---- CoreConfig -------------------------------------------------------------

#[test]
fn fresh_core_config_defaults_to_accuracy_mode() {
    let mut core = MockCore::new();
    assert!(core.config().accuracy_mode);
    core.config().accuracy_mode = false;
    assert!(!core.config().accuracy_mode);
}

// ---- object safety -----------------------------------------------------

#[test]
fn emulator_core_is_object_safe_and_drivable_through_dyn() {
    // ARCHITECTURE §5 uses EmulatorCore as `&mut dyn` throughout; consumers
    // (harness, frontend) never see the concrete core type. If a future
    // change to this trait breaks object safety (e.g. a generic method),
    // this stops compiling here instead of surfacing in rf-nes (W1-01a).
    let mut mock = MockCore::new();
    let core: &mut dyn EmulatorCore = &mut mock;

    core.config().event_mask = EventMask::FRAME_START;
    assert!(core.load(CartImage::from_rom(&[1])).is_ok());
    core.reset(ResetKind::Soft);

    let mut forbidden = EventMask::ALL;
    forbidden.remove(EventMask::FRAME_START);
    let mut sink = RecordingSink {
        forbidden,
        ..RecordingSink::default()
    };
    core.run_frame(&InputFrame::empty(), &mut sink);
    assert_eq!(sink.events, vec![CoreEvent::FrameStart]);

    let step = core.step(Step::Frame, &mut sink);
    assert!(step.frame_complete);

    let mut w = VecWriter::default();
    assert!(core.save_state(&mut w).is_ok());
    let mut r = SliceReader::new(&w.buf);
    assert!(core.load_state(&mut r).is_ok());

    let view: StateView<'_> = core.state_view();
    assert_eq!(view.cpu_regs.len(), CPU_REGS_SIZE);
}

// ---- EventMask <-> CoreEvent bit correspondence -------------------------

#[test]
fn event_mask_bits_correspond_one_to_one_with_core_event_variants() {
    let samples = [
        CoreEvent::FrameStart,
        CoreEvent::FrameEnd,
        CoreEvent::VblankStart,
        CoreEvent::Scanline(0),
        CoreEvent::OamRewrite,
        CoreEvent::ScrollWrite {
            x: 0,
            y: 0,
            layer: PixelLayer::Background(0),
        },
        CoreEvent::MapperIrq,
        CoreEvent::DmaStart { chan: 0 },
        CoreEvent::MemWatch { id: 0 },
    ];

    let mut seen = EventMask::NONE;
    for ev in &samples {
        let bit = bit_for(ev);
        // Every variant must map to a bit that ALL actually contains...
        assert!(
            EventMask::ALL.contains(bit),
            "{ev:?} bit missing from EventMask::ALL"
        );
        // ...and no two variants may share a bit (1:1 correspondence).
        assert!(
            !seen.contains(bit),
            "{ev:?} shares a bit with an earlier variant"
        );
        seen.insert(bit);
    }
    assert_eq!(
        seen,
        EventMask::ALL,
        "every CoreEvent variant must be represented in ALL"
    );
}
