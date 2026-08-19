//! The Lua sandbox (ticket W4-04; `docs/design/PLUGINS.md` §2/§4).
//!
//! ## This is the load-bearing part of the whole plugin story
//!
//! Capability gating, `cache_dir` containment and the write ledger are
//! **all decorative** if a script can reach `io.open` or `require` its way
//! around them. So the sandbox is built first and tested first, and the
//! test asserts the dangerous names are *absent* rather than that some
//! wrapper refused them — a binding that does not exist cannot be
//! bypassed by a cleverer call.
//!
//! ## What was actually verified, not assumed
//!
//! `mlua::Lua::new()` loads the **full** standard library. This uses
//! [`mlua::Lua::new_with`] with a minimal [`mlua::StdLib`] set instead —
//! and that alone is not enough, which was found by probing rather than
//! by reading:
//!
//! | name | after a minimal `StdLib` |
//! |---|---|
//! | `io`, `os`, `package`, `require`, `debug`, `loadstring`, `coroutine` | absent |
//! | **`dofile`, `loadfile`, `load`** | **still present** |
//!
//! `dofile` and `loadfile` **read files**, and they survive a minimal
//! `StdLib` because Lua's *base* library is always loaded and has no
//! `StdLib` bit of its own. They are removed explicitly below. `load` is
//! removed with them: without `io` it compiles strings rather than
//! reaching the filesystem, so it is a smaller hole — but a script host
//! has no need to compile arbitrary chunks at runtime, and leaving a
//! smaller hole open is still leaving a hole.
//!
//! ## `print` is rebound rather than removed
//!
//! A script author's first debugging tool should not be the thing that
//! silently writes to a stdout nobody is watching. `print` appends to an
//! in-process log the REPL panel renders, which is both more useful and
//! keeps the host's own stdout clean.

use std::sync::{Arc, Mutex};

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::manifest::{Capabilities, FilesystemCap};

/// Names Lua's always-loaded base library provides that this host removes.
///
/// Public so the test suite — and anyone auditing the sandbox — can see
/// the list rather than infer it from a constructor call.
pub const REMOVED_BASE_GLOBALS: &[&str] = &["dofile", "loadfile", "load"];

/// Standard libraries a plugin script may use.
///
/// Note what is NOT here: `io`, `os`, `package`, `debug`, `ffi`. A script
/// that wants the filesystem asks for the `cache_dir` capability and goes
/// through the host, which is the only path that can be contained.
fn permitted_stdlib() -> mlua::StdLib {
    mlua::StdLib::TABLE | mlua::StdLib::STRING | mlua::StdLib::MATH | mlua::StdLib::UTF8
}

/// Shared, in-process script output (`print`, plus host diagnostics).
#[derive(Debug, Clone, Default)]
pub struct ScriptLog(Arc<Mutex<Vec<String>>>);

impl ScriptLog {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, line: impl Into<String>) {
        if let Ok(mut lines) = self.0.lock() {
            lines.push(line.into());
            // A runaway script must not grow this without bound; the REPL
            // shows a tail anyway.
            if lines.len() > 500 {
                let excess = lines.len() - 500;
                lines.drain(..excess);
            }
        }
    }

    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        self.0.lock().map(|l| l.clone()).unwrap_or_default()
    }
}

/// A window of live machine memory the host publishes each frame.
///
/// A snapshot rather than a live bus handle, and that is the whole
/// safety argument: a script cannot reach into a running core, cannot
/// perturb it, and cannot observe it mid-frame. The shell fills this at a
/// frame boundary from a non-perturbing peek, exactly as the debugger's
/// viewers do — `PLUGINS.md` §2's "the host routes all access".
#[derive(Debug, Default, Clone)]
pub struct MemoryWindow {
    pub base: u32,
    pub bytes: Vec<u8>,
}

impl MemoryWindow {
    /// Byte at `addr`, or 0 outside the published window.
    ///
    /// Out-of-window reads return 0 rather than erroring because a script
    /// polling an address the host did not publish is a normal thing to
    /// do while an author is finding their way, and a Lua error would
    /// pause the script (W4-04's fault handling) for what is really a
    /// miss. The window's bounds are visible to the host, which is where
    /// a "your script is reading outside the published range" diagnostic
    /// belongs.
    #[must_use]
    pub fn read_u8(&self, addr: u32) -> u8 {
        addr.checked_sub(self.base)
            .and_then(|off| self.bytes.get(off as usize))
            .copied()
            .unwrap_or(0)
    }
}

/// Palette index an overlay draw uses when the script does not name one.
///
/// `0x30` is the NES palette's white, which is the one entry legible
/// against every background this project's fixtures produce — a default
/// that vanished into the backdrop would look like the overlay was
/// broken.
pub const DEFAULT_OVERLAY_COLOR: u8 = 0x30;

/// One overlay draw the script asked for.
///
/// Deliberately its own type rather than `rf_enhance::scene_graph::DrawCmd`:
/// this crate must not depend on the enhancement layer (ARCHITECTURE §3
/// has no `PLU -> ENH` edge), so the shell maps between them. The two
/// shapes are the same on purpose, so that mapping is total and obvious.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayCmd {
    Rect {
        x: i32,
        y: i32,
        width: u16,
        height: u16,
        color_index: u8,
    },
    Line {
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color_index: u8,
    },
}

/// What the shell publishes to a script, and what it collects back.
///
/// `Rc<RefCell<..>>` rather than `Arc<Mutex<..>>`: `mlua::Lua` is not
/// `Send`, a [`crate::host::ScriptHost`] lives entirely on the UI thread,
/// and a mutex here would buy nothing but the impression that this is
/// shareable across threads.
///
/// ## Which coordinate space `rf.gui` draws in
///
/// **World space**, the same space W5-03's `SceneGraph` layers use. A
/// script that reads a world-space value from a profile (RF-Scroller's
/// `player_x` is one) can pass it straight to `rf.gui.rect` and the
/// marker lands on the player in the full-level view. The bridge does
/// NOT add or subtract a camera, because doing so silently would make a
/// script's arithmetic wrong in exactly the way W5-03's module doc warns
/// about — sprites that stick to the viewport while the level scrolls.
/// A script wanting screen space subtracts `camera_x` itself, which the
/// profile also publishes.
#[derive(Debug, Default, Clone)]
pub struct Bridge {
    pub memory: Rc<RefCell<MemoryWindow>>,
    /// `[[memory_map]]` labels from the loaded profile, so a script names
    /// `player_x` instead of hardcoding `$6029` — the whole point of a
    /// profile publishing addresses.
    pub labels: Rc<RefCell<BTreeMap<String, u32>>>,
    /// Filled by `rf.gui.*`, drained by the shell each frame.
    pub overlay: Rc<RefCell<Vec<OverlayCmd>>>,
}

impl Bridge {
    /// Publish this frame's memory window and profile labels.
    pub fn publish(&self, window: MemoryWindow, labels: BTreeMap<String, u32>) {
        *self.memory.borrow_mut() = window;
        *self.labels.borrow_mut() = labels;
    }

    /// Take whatever the script drew, leaving the buffer empty.
    ///
    /// Drained rather than cleared-then-read so a frame on which the
    /// script was throttled or paused (W4-04's budget handling)
    /// contributes nothing instead of re-drawing the previous frame's
    /// overlay at a stale position.
    #[must_use]
    pub fn take_overlay(&self) -> Vec<OverlayCmd> {
        std::mem::take(&mut self.overlay.borrow_mut())
    }
}

/// Build a sandboxed Lua state for a plugin with `caps`.
///
/// # Errors
/// Returns any error mlua raises constructing the state or editing its
/// globals.
pub fn build(caps: &Capabilities, log: &ScriptLog) -> mlua::Result<mlua::Lua> {
    build_with_bridge(caps, log, &Bridge::default())
}

/// [`build`], but with a live [`Bridge`] behind `rf.mem`, `rf.profile`
/// and `rf.gui` (ticket W5-07).
///
/// W4-04 built the capability GATE and left the data path stubbed —
/// `rf.mem.read_u8` returned a literal 0 and `rf.gui.*` were no-ops —
/// because wiring a live machine through is the shell's to give, not this
/// crate's to take. This is that wiring, and the gate is unchanged: a
/// denied capability still means the function is ABSENT, not
/// present-and-refusing.
///
/// # Errors
/// Returns the Lua error if the sandbox cannot be built.
pub fn build_with_bridge(
    caps: &Capabilities,
    log: &ScriptLog,
    bridge: &Bridge,
) -> mlua::Result<mlua::Lua> {
    let lua = mlua::Lua::new_with(permitted_stdlib(), mlua::LuaOptions::default())?;
    {
        let globals = lua.globals();
        for name in REMOVED_BASE_GLOBALS {
            globals.set(*name, mlua::Value::Nil)?;
        }
        let log_for_print = log.clone();
        let print = lua.create_function(move |_, args: mlua::MultiValue| {
            let line = args
                .iter()
                .map(|v| match v {
                    mlua::Value::String(s) => s.to_string_lossy().to_string(),
                    other => format!("{other:?}"),
                })
                .collect::<Vec<_>>()
                .join("\t");
            log_for_print.push(line);
            Ok(())
        })?;
        globals.set("print", print)?;

        // The `rf` table is built per-capability: a DENIED capability
        // means the function is ABSENT, not present-and-refusing. A
        // script cannot work around a name that was never bound, and an
        // author gets "attempt to call a nil value" at the exact call
        // rather than a runtime refusal they might catch and ignore.
        let rf = lua.create_table()?;
        rf.set("api", "0.1")?;

        if caps.read_memory {
            let mem = lua.create_table()?;
            let m = bridge.memory.clone();
            mem.set(
                "read_u8",
                lua.create_function(move |_, addr: u32| Ok(m.borrow().read_u8(addr)))?,
            )?;
            let m16 = bridge.memory.clone();
            // Little-endian, because every 6502-family game stores 16-bit
            // values that way and every `u16` in a profile's `memory_map`
            // means that. A script that had to assemble two bytes itself
            // would get the order wrong roughly half the time, and the
            // symptom — a value that looks right for the first 256 units
            // and then wraps — is the kind of bug that survives a demo.
            mem.set(
                "read_u16",
                lua.create_function(move |_, addr: u32| {
                    let m = m16.borrow();
                    Ok(u32::from(m.read_u8(addr)) | (u32::from(m.read_u8(addr + 1)) << 8))
                })?,
            )?;
            rf.set("mem", mem)?;

            // `rf.profile.addr(label)` rides the SAME capability as
            // reading memory, deliberately: an address the host published
            // is only useful for reading, and gating it separately would
            // let a script learn the map while being denied the map's
            // only purpose.
            let labels = bridge.labels.clone();
            let profile = lua.create_table()?;
            profile.set(
                "addr",
                lua.create_function(move |_, label: String| {
                    Ok(labels.borrow().get(&label).copied())
                })?,
            )?;
            rf.set("profile", profile)?;
        }
        if caps.draw_overlay {
            let gui = lua.create_table()?;
            let rects = bridge.overlay.clone();
            gui.set(
                "rect",
                // The colour is OPTIONAL, and not merely for
                // convenience: W4-04's stub accepted any argument list,
                // so scripts written against it call `rect(x, y, w, h)`.
                // Making the fifth argument mandatory would have turned a
                // stub into a breaking change for every script already
                // written, which is not a thing a data path should do
                // when it starts carrying data.
                lua.create_function(
                    move |_, (x, y, w, h, color): (i32, i32, u16, u16, Option<u8>)| {
                        rects.borrow_mut().push(OverlayCmd::Rect {
                            x,
                            y,
                            width: w,
                            height: h,
                            color_index: color.unwrap_or(DEFAULT_OVERLAY_COLOR),
                        });
                        Ok(())
                    },
                )?,
            )?;
            let lines = bridge.overlay.clone();
            gui.set(
                "line",
                lua.create_function(
                    move |_, (x0, y0, x1, y1, color): (i32, i32, i32, i32, Option<u8>)| {
                        lines.borrow_mut().push(OverlayCmd::Line {
                            x0,
                            y0,
                            x1,
                            y1,
                            color_index: color.unwrap_or(DEFAULT_OVERLAY_COLOR),
                        });
                        Ok(())
                    },
                )?,
            )?;
            // `text` stays a no-op and says so rather than pretending:
            // W5-03's `OverlayCmds` layer carries lines and rects only,
            // and inventing a text command here would put a variant in
            // the API that nothing downstream can draw.
            let log_for_text = log.clone();
            gui.set(
                "text",
                lua.create_function(move |_, _: mlua::MultiValue| {
                    log_for_text
                        .push("rf.gui.text is not implemented — the overlay layer draws lines and rectangles only");
                    Ok(())
                })?,
            )?;
            rf.set("gui", gui)?;
        }
        if caps.filesystem == FilesystemCap::CacheDir {
            rf.set("cache", lua.create_table()?)?;
        }
        globals.set("rf", rf)?;
    }
    Ok(lua)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present(lua: &mlua::Lua, expr: &str) -> bool {
        lua.load(format!("return ({expr}) ~= nil"))
            .eval()
            .unwrap_or(false)
    }

    /// **The test that matters.** Every escape hatch is ABSENT — not
    /// wrapped, not refused, absent. A binding that does not exist cannot
    /// be reached by a cleverer call.
    ///
    /// `dofile`/`loadfile`/`load` are in this list because probing found
    /// them still present after a minimal `StdLib` (Lua's base library is
    /// always loaded and has no `StdLib` bit) — they are removed
    /// explicitly, and this is what proves the removal happened.
    #[test]
    fn no_escape_hatch_survives_the_sandbox() {
        let lua = build(&Capabilities::default(), &ScriptLog::new()).unwrap();
        for name in [
            "io",
            "os",
            "package",
            "require",
            "debug",
            "loadstring",
            "dofile",
            "loadfile",
            "load",
            "arg",
        ] {
            assert!(
                !present(&lua, name),
                "`{name}` is reachable from a plugin script — every capability gate in this \
                 crate is decorative while it is"
            );
        }
    }

    /// Anti-vacuity for the test above: the sandbox is not simply empty.
    /// A script can still compute, or the host would be useless and
    /// "everything is absent" would be trivially true.
    #[test]
    fn a_script_can_still_do_useful_work() {
        let lua = build(&Capabilities::default(), &ScriptLog::new()).unwrap();
        for name in ["string", "math", "table", "utf8", "pcall", "print"] {
            assert!(present(&lua, name), "`{name}` should remain available");
        }
        let n: i64 = lua
            .load("local t = {3,1,2}; table.sort(t); return t[1] + math.floor(2.7)")
            .eval()
            .unwrap();
        assert_eq!(n, 3);
    }

    /// A denied capability means the function is ABSENT. Both directions
    /// asserted, so "the gate works" and "nothing is ever bound" cannot
    /// look the same.
    #[test]
    fn denied_capabilities_are_absent_and_granted_ones_are_present() {
        let denied = build(&Capabilities::default(), &ScriptLog::new()).unwrap();
        assert!(present(&denied, "rf"), "the rf table itself always exists");
        assert!(!present(&denied, "rf.mem"), "read_memory denied");
        assert!(!present(&denied, "rf.gui"), "draw_overlay denied");
        assert!(!present(&denied, "rf.cache"), "filesystem denied");

        let granted = build(
            &Capabilities {
                read_memory: true,
                draw_overlay: true,
                filesystem: FilesystemCap::CacheDir,
                ..Capabilities::default()
            },
            &ScriptLog::new(),
        )
        .unwrap();
        assert!(present(&granted, "rf.mem.read_u8"));
        assert!(present(&granted, "rf.gui.rect"));
        assert!(present(&granted, "rf.cache"));
    }

    /// Calling a denied API fails at the call site with Lua's own
    /// nil-call error — the author sees exactly which line, rather than a
    /// refusal they could catch and ignore.
    #[test]
    fn calling_a_denied_api_is_a_nil_call_error() {
        let lua = build(&Capabilities::default(), &ScriptLog::new()).unwrap();
        let err = lua
            .load("return rf.mem.read_u8(0x100)")
            .exec()
            .expect_err("a denied capability must not be callable");
        assert!(
            err.to_string().contains("nil"),
            "expected a nil-index/call error, got: {err}"
        );
    }

    #[test]
    fn print_goes_to_the_script_log_not_stdout() {
        let log = ScriptLog::new();
        let lua = build(&Capabilities::default(), &log).unwrap();
        lua.load(r#"print("hello", "world")"#).exec().unwrap();
        assert_eq!(log.lines(), vec!["hello\tworld".to_string()]);
    }
}
