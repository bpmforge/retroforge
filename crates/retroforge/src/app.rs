//! `eframe::App` implementation (ticket W1-06 acceptance criteria 1-4).
//!
//! This is the **only** module in this crate allowed to depend on
//! `egui`/`eframe` (crate-level doc) — it is a thin presentation layer over
//! [`crate::core_thread`] (FM-01 containment, panic-guarded core thread)
//! and [`crate::rom_open`] (the file dialog). It owns no NES/6502 state
//! directly; [`crate::stepper::EmuStepper`] lives entirely on the core
//! thread.
//!
//! `egui`/`eframe` 0.35 API notes (verified against the pinned version's
//! source in `~/.cargo/registry`, not training data — both changed
//! recently):
//! - [`eframe::App::ui`] takes `&mut egui::Ui` directly (no
//!   `CentralPanel`/margin), not the older `update(&mut self, ctx:
//!   &Context, ...)` shape.
//! - `egui::TopBottomPanel`/`SidePanel` no longer exist; panels are all
//!   `egui::Panel::top/bottom/left/right(id).show(ui, |ui| ...)`, taking a
//!   `&mut Ui` like every other panel now.
//! - `ui.close_menu()` was renamed `ui.close()`.
//!
//! ## Keyboard input (ticket W1-07)
//!
//! `poll_input` samples `egui::InputState::key_down` once per repaint for
//! the fixed handful of keys [`crate::input_map::map_key`] knows, pushes
//! each into an [`rf_input::InputLatch`] (translated via that map — the
//! only place `egui::Key` and `rf_input::Key` ever meet), and stores the
//! resulting `InputFrame` into the core thread's `SharedInputFrame`
//! (`crate::core_thread`'s module doc: one atomic, no mutex in the frame
//! loop). This module never talks to `rf_nes` directly — the core thread
//! is the only thing that latches input into a running machine.
use eframe::egui;

use crate::core_thread::{self, CoreCommand, CoreCrashReport, CoreEvent, CoreHandle};
use crate::input_map;
use crate::rom_open;

/// Every host key the default NES keymap binds — the fixed poll list
/// `poll_input` checks each repaint (module doc).
const POLLED_KEYS: [egui::Key; 8] = [
    egui::Key::ArrowUp,
    egui::Key::ArrowDown,
    egui::Key::ArrowLeft,
    egui::Key::ArrowRight,
    egui::Key::Z,
    egui::Key::X,
    egui::Key::Enter,
    egui::Key::ShiftRight,
];

/// The whole application's UI-thread-owned state.
pub struct RetroForgeApp {
    core: Option<CoreHandle>,
    texture: Option<egui::TextureHandle>,
    status: String,
    crash: Option<CoreCrashReport>,
    /// Mirrors the core thread's run state for button labels/enablement;
    /// the core thread itself (`EmuStepper::state`) is the source of
    /// truth — this is only ever set right after sending a command, so it
    /// can't drift for more than one repaint.
    running: bool,
    /// Ticket W2-14: a step command's frame arrives over the channel a few
    /// milliseconds *after* the click's own repaint has already finished.
    /// While paused nothing else schedules a repaint, so without this the
    /// frame would sit unconsumed until some unrelated event (a mouse
    /// move) happened to wake the UI — a "works if you jiggle the mouse"
    /// bug. Set when a step is requested, cleared when a frame lands.
    awaiting_stepped_frame: bool,
    /// Latest machine position from the core thread, rendered in the
    /// status bar (ticket W2-15). `None` until the first frame lands.
    /// This is what makes Step Frame and Step Scanline observable: a
    /// scanline step changes 1/240th of the picture over a framebuffer
    /// that deliberately persists, so without a readout a correct step is
    /// indistinguishable from a dead button — which is exactly how it was
    /// first reported.
    position: Option<(u64, Option<u16>)>,
    /// Host-agnostic per-frame input latch (`rf_input`, FR-FE-003) — the
    /// UI thread's write side; `poll_input` samples it every repaint into
    /// the core thread's `SharedInputFrame` (module doc).
    input_latch: rf_input::InputLatch,
    keymap: rf_input::KeyMap,
    /// UI-thread mirror of the core thread's overlay setting (ticket
    /// W3-05a) — same "set right after sending a command" pattern
    /// `running` above uses, for the same reason (the checkbox needs
    /// something to read/write; the core thread's `EmuStepper` is the real
    /// source of truth). `false` by default: a freshly opened ROM boots in
    /// Accuracy Mode (law 6).
    sprite_overlay: bool,
}

impl RetroForgeApp {
    #[must_use]
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        RetroForgeApp {
            core: None,
            texture: None,
            status: "No ROM loaded \u{2014} File > Open ROM...".to_string(),
            crash: None,
            running: false,
            awaiting_stepped_frame: false,
            position: None,
            input_latch: rf_input::InputLatch::new(),
            keymap: rf_input::KeyMap::default_nes(),
            sprite_overlay: false,
        }
    }

    /// Sample the fixed [`POLLED_KEYS`] list once per repaint into
    /// [`Self::input_latch`], then publish the resulting `InputFrame` to
    /// the core thread (module doc). A no-op if no core is loaded — there
    /// is nothing to publish to.
    fn poll_input(&mut self, ctx: &egui::Context) {
        ctx.input(|input_state| {
            for key in POLLED_KEYS {
                let Some(mapped) = input_map::map_key(key) else {
                    continue;
                };
                if input_state.key_down(key) {
                    self.input_latch.key_down(mapped);
                } else {
                    self.input_latch.key_up(mapped);
                }
            }
        });
        if let Some(core) = &self.core {
            core.input.store(self.input_latch.sample(&self.keymap));
        }
    }

    fn open_rom(&mut self) {
        let Some(path) = rom_open::pick_rom_file() else {
            return; // user cancelled the dialog
        };
        let bytes = match rom_open::load_rom_bytes(&path) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.status = format!("Failed to open ROM: {e}");
                return;
            }
        };
        match core_thread::spawn(bytes) {
            Ok(handle) => {
                self.core = Some(handle);
                self.crash = None;
                self.texture = None;
                self.running = false;
                // A freshly loaded ROM's core boots in Accuracy Mode (law
                // 6) — mirror that in the checkbox too, rather than leaving
                // a previous ROM's overlay choice looking still-checked
                // against a core that just reset it.
                self.sprite_overlay = false;
                self.status = format!("Loaded {}", path.display());
            }
            Err(e) => {
                self.status = format!("Failed to load ROM: {e}");
            }
        }
    }

    fn send_command(&self, cmd: CoreCommand) {
        if let Some(core) = &self.core {
            // The core thread only ever disappears if it already crashed
            // (FM-01) and drained its channel; a send failing here just
            // means the crash report is already on its way/arrived.
            let _ = core.cmd_tx.send(cmd);
        }
    }

    /// Drain every pending [`CoreEvent`] this repaint, keeping only the
    /// latest frame (older ones are stale by the time we'd paint them) and
    /// latching a crash report the moment one arrives.
    fn pump_core_events(&mut self, ctx: &egui::Context) {
        let Some(core) = &self.core else { return };
        let mut latest_frame = None;
        let mut crashed = false;
        while let Ok(evt) = core.evt_rx.try_recv() {
            match evt {
                CoreEvent::Frame(msg) => latest_frame = Some(msg),
                CoreEvent::Crashed(report) => {
                    self.crash = Some(report);
                    self.running = false;
                    crashed = true;
                    break;
                }
            }
        }
        if crashed {
            // FM-01's recovery row is "Reload ROM / load last state" — the
            // core thread has already halted for good (`run_guarded_loop`
            // never retries), so drop the dead handle now rather than
            // leaving `self.core` pointing at a channel nobody reads.
            // `has_core` in `controls_bar` then goes false on its own,
            // which disables Run/Step Frame/Step Scanline until the user
            // opens a ROM again — no separate "is there a live core"
            // check needed anywhere else.
            self.core = None;
            self.status = "Core crashed (FM-01) — open a ROM to start a fresh session".to_string();
            return;
        }
        if let Some(msg) = latest_frame {
            // Ticket W2-14: a stepped frame has now been consumed.
            self.awaiting_stepped_frame = false;
            // Ticket W2-15: position travels with the frame.
            self.position = Some((msg.frame_count, msg.last_scanline));
            let image =
                egui::ColorImage::from_rgba_unmultiplied([msg.width, msg.height], &msg.rgba);
            match &mut self.texture {
                Some(tex) => tex.set(image, egui::TextureOptions::NEAREST),
                None => {
                    self.texture =
                        Some(ctx.load_texture("nes-frame", image, egui::TextureOptions::NEAREST));
                }
            }
        }
        if self.running || self.awaiting_stepped_frame {
            // Keep repainting while running so the core thread's frames
            // keep getting picked up (CPU blit, "live frames" criterion),
            // and likewise until a requested step's frame has landed
            // (ticket W2-14) — while paused nothing else would wake the
            // UI to consume it.
            ctx.request_repaint();
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("menu_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open ROM...").clicked() {
                        self.open_rom();
                        ui.close();
                    }
                });
            });
        });
    }

    fn controls_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("controls").show(ui, |ui| {
            ui.horizontal(|ui| {
                let has_core = self.core.is_some();
                let run_label = if self.running { "Pause" } else { "Run" };
                if ui
                    .add_enabled(has_core, egui::Button::new(run_label))
                    .clicked()
                {
                    self.running = !self.running;
                    self.send_command(if self.running {
                        CoreCommand::Resume
                    } else {
                        CoreCommand::Pause
                    });
                }
                if ui
                    .add_enabled(has_core, egui::Button::new("Step Frame"))
                    .clicked()
                {
                    self.running = false;
                    self.awaiting_stepped_frame = true;
                    self.send_command(CoreCommand::StepFrame);
                }
                if ui
                    .add_enabled(has_core, egui::Button::new("Step Scanline"))
                    .clicked()
                {
                    self.running = false;
                    self.awaiting_stepped_frame = true;
                    self.send_command(CoreCommand::StepScanline);
                }
                ui.separator();
                // Ticket W3-05a, FR-ENH-001: opt-in only, off by default
                // (law 6) — checking this does not touch the accuracy
                // simulation, only whether the dropped-sprite overlay gets
                // composited on top of it (`Ppu`'s module doc, "Sprite-
                // limit-bypass overlay" section).
                if ui
                    .add_enabled(
                        has_core,
                        egui::Checkbox::new(&mut self.sprite_overlay, "De-flicker overlay"),
                    )
                    .changed()
                {
                    self.send_command(CoreCommand::SetSpriteOverlay(self.sprite_overlay));
                }
                ui.separator();
                ui.label(&self.status);
                if let Some((frame, scanline)) = self.position {
                    ui.separator();
                    ui.monospace(match scanline {
                        Some(y) => format!("frame {frame} \u{b7} scanline {y}"),
                        None => format!("frame {frame} \u{b7} scanline --"),
                    });
                }
            });
        });
    }

    fn crash_dialog(&mut self, ctx: &egui::Context) {
        let Some(report) = self.crash.clone() else {
            return;
        };
        egui::Window::new("Core crashed")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("The emulator core panicked and was contained (FM-01); the core thread has halted.");
                ui.label(format!("Message: {}", report.message));
                if let Some(loc) = &report.location {
                    ui.label(format!("Location: {loc}"));
                }
                ui.separator();
                ui.label("Trace tail:");
                egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                    ui.monospace(&report.trace_tail);
                });
                ui.separator();
                if ui.button("Dismiss").clicked() {
                    self.crash = None;
                }
            });
    }

    fn video_panel(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(texture) = &self.texture {
                ui.add(egui::Image::from_texture(texture).shrink_to_fit());
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label(&self.status);
                });
            }
        });
    }
}

impl eframe::App for RetroForgeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_input(&ctx);
        self.pump_core_events(&ctx);

        self.menu_bar(ui);
        self.controls_bar(ui);
        self.video_panel(ui);
        self.crash_dialog(&ctx);
    }
}
