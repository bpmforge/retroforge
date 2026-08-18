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

/// Build a sandboxed Lua state for a plugin with `caps`.
///
/// # Errors
/// Returns any error mlua raises constructing the state or editing its
/// globals.
pub fn build(caps: &Capabilities, log: &ScriptLog) -> mlua::Result<mlua::Lua> {
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
            // Bound to a stub in this ticket: wiring a live StateView
            // through requires the shell's core handle, which is
            // `crates/retroforge`'s to give (see this crate's lib doc).
            // The CAPABILITY GATE is what is being built and tested here.
            let mem = lua.create_table()?;
            mem.set("read_u8", lua.create_function(|_, _addr: u32| Ok(0u8))?)?;
            rf.set("mem", mem)?;
        }
        if caps.draw_overlay {
            let gui = lua.create_table()?;
            for name in ["rect", "text", "line"] {
                gui.set(name, lua.create_function(|_, _: mlua::MultiValue| Ok(()))?)?;
            }
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
