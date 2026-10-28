//! The function-hook host: Lua and Rust hooks on a rebuilt core's function
//! entry/exit events (ticket W36-05; `docs/design/REBUILD_LAB.md` section 5
//! step 6; `docs/design/PLUGINS.md` §2).
//!
//! ## Thread model: no `Send` Lua
//!
//! A [`HookHost`] is created **on the core thread** and stays there. It is
//! the `CoreSink` side of the push boundary: the core calls
//! [`CoreSink::function_event`] synchronously at an instruction boundary and
//! the host runs its callbacks inside that call, with the machine lent for the
//! duration through a [`HookCtx`]. The Lua state is therefore never shared and
//! never sent anywhere; mlua's `send` feature stays off. The host is a cheap
//! `Rc` handle ([`Clone`]) so the code that owns the sink and the code that
//! reads its ledger and panel lines (same thread) can both hold it; it is
//! deliberately `!Send`, so the type system refuses to move it across threads.
//! What crosses to the UI is plain data: [`HookHost::panel_lines`] and
//! [`HookHost::ledger_entries`].
//!
//! ## Names, not addresses
//!
//! A hook registers by a recipe function name (`"nmi"`) or by an atlas
//! function id. Names resolve to ids when the shell has matched a recipe
//! against the installed atlas ([`HookHost::resolve`]); a name no function
//! matched is *inactive* (reported, never an error: the game keeps running on
//! the generic rebuild). [`HookHost::function_ids`] is what the shell hands to
//! `EmulatorCore::set_function_hooks`.
//!
//! ## Capabilities and the mod boundary (NON_GOALS 13)
//!
//! Any hook needs `function_hooks`; reading registers and memory needs
//! `read_memory`; changing memory needs `write_memory`, and every applied
//! write goes through the same [`WriteLedger`] the frame scripts use, naming
//! the plugin and the frame. A denied API is absent from the Lua `rf` table,
//! not present-and-refusing. Pokes are applied by the core with no clock
//! cost; a core accepts work RAM and cartridge RAM only.
//!
//! ## Failure containment and budget
//!
//! A Lua error pauses the hook host, never the emulator (FR-PLUG-004): the
//! host logs it, goes [`ScriptState::Paused`] and ignores later events. The
//! per-callback [`Budget`] throttles a read-only host (it then sees every Nth
//! event). **A host holding `write_memory` is never throttled**, only
//! warned: skipping events would make a mod's effect depend on wall-clock
//! timing and break replay determinism.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use rf_core_api::{CoreEvent, CoreSink, CpuRegs, FunctionEvent, HookCtx, PpuPixel};

use crate::host::{Budget, LedgerEntry, ScriptState, WriteLedger};
use crate::manifest::{Capabilities, Manifest};
use crate::sandbox::{self, Bridge, ScriptLog};
use crate::surface::PlayMode;

/// How a hook names its function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookKey {
    /// A recipe function name, resolved against the installed atlas.
    Name(String),
    /// An atlas function id, used as is.
    Id(u32),
}

impl From<&str> for HookKey {
    fn from(s: &str) -> Self {
        HookKey::Name(s.to_string())
    }
}

impl From<u32> for HookKey {
    fn from(id: u32) -> Self {
        HookKey::Id(id)
    }
}

impl std::fmt::Display for HookKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HookKey::Name(n) => write!(f, "{n}"),
            HookKey::Id(i) => write!(f, "function {i}"),
        }
    }
}

/// Why a Rust hook could not be registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookRefusal {
    /// The manifest does not grant `function_hooks`.
    NoFunctionHooksCapability,
}

impl std::fmt::Display for HookRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HookRefusal::NoFunctionHooksCapability => write!(
                f,
                "plugin has no `function_hooks` capability — declare `function_hooks = true`"
            ),
        }
    }
}

impl std::error::Error for HookRefusal {}

/// The machine as a Rust hook sees it: reads are free, writes are gated by
/// `write_memory` and ledgered.
pub struct ModCtx<'a> {
    ctx: &'a mut dyn HookCtx,
    caps: &'a Capabilities,
    plugin: &'a str,
    frame: u64,
    ledger: &'a RefCell<WriteLedger>,
    mode: PlayMode,
}

impl ModCtx<'_> {
    #[must_use]
    pub fn regs(&self) -> CpuRegs {
        self.ctx.regs()
    }

    #[must_use]
    pub fn peek(&self, addr: u32) -> u8 {
        self.ctx.peek(addr)
    }

    /// Whether the player is in Original or Enhanced mode (ticket W39-00). A
    /// logic plugin must do nothing in Original mode; the lockstep gate in
    /// [`crate::surface`] is what proves it.
    #[must_use]
    pub fn mode(&self) -> PlayMode {
        self.mode
    }

    /// The frame number the host was last told ([`HookHost::set_frame`]).
    #[must_use]
    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// Write one byte. `false` when `write_memory` is not granted or the
    /// core refused the address; an applied write is in the ledger.
    pub fn poke(&mut self, addr: u32, value: u8) -> bool {
        poke_ledgered(
            self.ctx,
            self.caps,
            self.plugin,
            self.frame,
            self.ledger,
            addr,
            value,
        )
    }
}

/// The one place a hook write is gated, applied and recorded: capability
/// first, then the core's refusal, then the ledger (so the ledger holds
/// exactly the writes that changed the machine).
fn poke_ledgered(
    ctx: &mut dyn HookCtx,
    caps: &Capabilities,
    plugin: &str,
    frame: u64,
    ledger: &RefCell<WriteLedger>,
    addr: u32,
    value: u8,
) -> bool {
    if !caps.write_memory || !ctx.poke(addr, value) {
        return false;
    }
    ledger.borrow_mut().record(caps, plugin, frame, addr, value)
}

type RustHook = Box<dyn FnMut(FunctionEvent, &mut ModCtx<'_>)>;

#[derive(Clone)]
enum Target {
    Lua(mlua::Function),
    Rust(Rc<RefCell<RustHook>>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Enter,
    Exit,
}

struct Reg {
    kind: Kind,
    key: HookKey,
    resolved: Option<u32>,
    target: Target,
}

/// Outcome of matching hook names to atlas functions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolution {
    /// `(hook key, atlas function id)` for every hook that will fire.
    pub active: Vec<(String, u32)>,
    /// Hook keys no atlas function matched.
    pub inactive: Vec<String>,
}

struct Inner {
    manifest: Manifest,
    log: ScriptLog,
    budget: Budget,
    state: ScriptState,
    ledger: Rc<RefCell<WriteLedger>>,
    frame: Rc<Cell<u64>>,
    mode: Rc<Cell<PlayMode>>,
    lua: Option<mlua::Lua>,
    regs: Rc<RefCell<Vec<Reg>>>,
    resolution: Resolution,
    /// Events delivered to callbacks, and skipped by the throttle.
    delivered: u64,
    skipped: u64,
    since_call: u64,
    warned_over_budget: bool,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // The registrations hold Lua function handles and the Lua `rf.on_*`
        // closures hold the registrations: break the cycle before the state
        // goes.
        self.regs.borrow_mut().clear();
    }
}

/// See the module doc. Cloning shares one host.
#[derive(Clone)]
pub struct HookHost(Rc<RefCell<Inner>>);

impl HookHost {
    /// A host with no script: register Rust hooks with
    /// [`HookHost::on_enter`] / [`HookHost::on_exit`].
    #[must_use]
    pub fn new(manifest: Manifest, budget: Budget) -> Self {
        Self(Rc::new(RefCell::new(Inner {
            manifest,
            log: ScriptLog::new(),
            budget,
            state: ScriptState::Running,
            ledger: Rc::new(RefCell::new(WriteLedger::default())),
            frame: Rc::new(Cell::new(0)),
            mode: Rc::new(Cell::new(PlayMode::Original)),
            lua: None,
            regs: Rc::new(RefCell::new(Vec::new())),
            resolution: Resolution::default(),
            delivered: 0,
            skipped: 0,
            since_call: 0,
            warned_over_budget: false,
        })))
    }

    /// Load a Lua `source` that registers hooks with `rf.on_enter(key, fn)`
    /// and `rf.on_exit(key, fn)` (`key` is a recipe function name or an atlas
    /// function id; the callback gets the function id).
    ///
    /// Inside a callback, and only there: `rf.regs()` (a table of registers),
    /// `rf.mem.read_u8(addr)`, `rf.mem.read_u16(addr)` (need `read_memory`)
    /// and `rf.mem.write_u8(addr, value)` (needs `write_memory`, ledgered,
    /// returns whether it was applied). `rf.profile.addr` reads the labels
    /// in `bridge`, as in frame scripts.
    ///
    /// # Errors
    /// The Lua error if the sandbox cannot be built or the script fails at
    /// load (including calling `rf.on_enter` without `function_hooks`: the
    /// function is absent, so that is a nil call).
    pub fn load(
        manifest: Manifest,
        source: &str,
        budget: Budget,
        bridge: &Bridge,
    ) -> mlua::Result<Self> {
        let host = Self::new(manifest, budget);
        let (caps, log, regs, mode) = {
            let i = host.0.borrow();
            (
                i.manifest.capabilities.clone(),
                i.log.clone(),
                i.regs.clone(),
                i.mode.clone(),
            )
        };
        let lua = sandbox::build_with_bridge(&caps, &log, bridge)?;
        install_lua_api(&lua, &caps, &regs, &mode)?;
        lua.load(source).exec()?;
        host.0.borrow_mut().lua = Some(lua);
        Ok(host)
    }

    /// Register a Rust hook for `key`'s entry.
    ///
    /// # Errors
    /// [`HookRefusal::NoFunctionHooksCapability`] without the capability.
    pub fn on_enter(
        &self,
        key: impl Into<HookKey>,
        f: impl FnMut(FunctionEvent, &mut ModCtx<'_>) + 'static,
    ) -> Result<(), HookRefusal> {
        self.register(Kind::Enter, key.into(), Box::new(f))
    }

    /// Register a Rust hook for `key`'s exit. See [`HookHost::on_enter`].
    ///
    /// # Errors
    /// [`HookRefusal::NoFunctionHooksCapability`] without the capability.
    pub fn on_exit(
        &self,
        key: impl Into<HookKey>,
        f: impl FnMut(FunctionEvent, &mut ModCtx<'_>) + 'static,
    ) -> Result<(), HookRefusal> {
        self.register(Kind::Exit, key.into(), Box::new(f))
    }

    fn register(&self, kind: Kind, key: HookKey, f: RustHook) -> Result<(), HookRefusal> {
        let i = self.0.borrow();
        if !i.manifest.capabilities.function_hooks {
            return Err(HookRefusal::NoFunctionHooksCapability);
        }
        let resolved = match &key {
            HookKey::Id(id) => Some(*id),
            HookKey::Name(_) => None,
        };
        i.regs.borrow_mut().push(Reg {
            kind,
            key,
            resolved,
            target: Target::Rust(Rc::new(RefCell::new(f))),
        });
        Ok(())
    }

    /// Match hook names to atlas function ids (`lookup` is
    /// `MatchReport::function_of` of the shell's recipe match). Ids resolve to
    /// themselves. Call again whenever the atlas or recipe changes.
    pub fn resolve(&self, lookup: &dyn Fn(&str) -> Option<u32>) -> Resolution {
        let mut i = self.0.borrow_mut();
        let mut res = Resolution::default();
        {
            let mut regs = i.regs.borrow_mut();
            for r in regs.iter_mut() {
                if let HookKey::Name(n) = &r.key {
                    r.resolved = lookup(n);
                }
                let label = r.key.to_string();
                match r.resolved {
                    Some(id) => {
                        if !res.active.contains(&(label.clone(), id)) {
                            res.active.push((label, id));
                        }
                    }
                    None => {
                        if !res.inactive.contains(&label) {
                            res.inactive.push(label);
                        }
                    }
                }
            }
        }
        i.resolution = res.clone();
        res
    }

    /// Atlas function ids with at least one active hook, sorted unique: what
    /// to pass to `EmulatorCore::set_function_hooks`.
    #[must_use]
    pub fn function_ids(&self) -> Vec<u32> {
        let i = self.0.borrow();
        let mut ids: Vec<u32> = i.regs.borrow().iter().filter_map(|r| r.resolved).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Tell the host which frame it is in, for the ledger. The shell calls
    /// this once per frame on the core thread.
    pub fn set_frame(&self, frame: u64) {
        self.0.borrow().frame.set(frame);
    }

    /// Tell the host which mode the player is in (ticket W39-00). A new host
    /// is in [`PlayMode::Original`], the safe default.
    pub fn set_mode(&self, mode: PlayMode) {
        self.0.borrow().mode.set(mode);
    }

    #[must_use]
    pub fn mode(&self) -> PlayMode {
        self.0.borrow().mode.get()
    }

    #[must_use]
    pub fn state(&self) -> ScriptState {
        self.0.borrow().state.clone()
    }

    /// Everything the hooks have written so far, oldest first.
    #[must_use]
    pub fn ledger_entries(&self) -> Vec<LedgerEntry> {
        self.0.borrow().ledger.borrow().entries().to_vec()
    }

    /// Whether the host's manifest grants `write_memory`. A machine without
    /// this host (run-ahead's shadow, the margin redraw copy) cannot
    /// reproduce a frame in which a hook wrote memory (ticket W36-06b).
    #[must_use]
    pub fn holds_write_memory(&self) -> bool {
        self.0.borrow().manifest.capabilities.write_memory
    }

    /// Put back the ledger a save state carried (ticket W36-06b): the writes
    /// that made the saved machine what it is. Replaces what is there; the
    /// panel then shows them as the session's own history.
    pub fn restore_ledger(&self, entries: Vec<LedgerEntry>) {
        self.0.borrow().ledger.borrow_mut().restore(entries);
    }

    /// `print` output and host diagnostics.
    #[must_use]
    pub fn log_lines(&self) -> Vec<String> {
        self.0.borrow().log.lines()
    }

    /// Events delivered to callbacks / skipped by the throttle.
    #[must_use]
    pub fn event_counts(&self) -> (u64, u64) {
        let i = self.0.borrow();
        (i.delivered, i.skipped)
    }

    /// Read an integer Lua global (a script's own counter), for tests and the
    /// REPL.
    #[must_use]
    pub fn lua_global_i64(&self, name: &str) -> Option<i64> {
        let i = self.0.borrow();
        i.lua.as_ref()?.globals().get::<i64>(name).ok()
    }

    /// Read a string Lua global, for tests and the REPL.
    #[must_use]
    pub fn lua_global_string(&self, name: &str) -> Option<String> {
        let i = self.0.borrow();
        i.lua.as_ref()?.globals().get::<String>(name).ok()
    }

    /// The panel's lines: which hooks are active or inactive, the host's
    /// state, and the write ledger (FR-PLUG-003: visible in the UI).
    #[must_use]
    pub fn panel_lines(&self) -> Vec<String> {
        let i = self.0.borrow();
        let mut out = vec![format!(
            "function hooks: {} active, {} inactive — {}",
            i.resolution.active.len(),
            i.resolution.inactive.len(),
            match &i.state {
                ScriptState::Running => "running".to_string(),
                ScriptState::Throttled { .. } => "throttled".to_string(),
                ScriptState::Paused { error } => format!("paused: {error}"),
            }
        )];
        for (name, id) in &i.resolution.active {
            out.push(format!("  active   {name} -> function {id}"));
        }
        for name in &i.resolution.inactive {
            out.push(format!("  inactive {name} (no matching function)"));
        }
        out.push(format!(
            "  {} events delivered, {} skipped by the throttle",
            i.delivered, i.skipped
        ));
        let ledger = i.ledger.borrow();
        if ledger.is_empty() {
            out.push("  no memory writes".to_string());
        }
        for e in ledger.entries() {
            out.push(format!(
                "  MOD write: {} wrote {:#04x} to {:#06x} at frame {}",
                e.plugin, e.value, e.addr, e.frame
            ));
        }
        out
    }

    /// Run every callback registered for `ev`. Never fails: a Lua error
    /// pauses the host.
    pub fn dispatch(&self, ev: FunctionEvent, ctx: &mut dyn HookCtx) {
        let (function, kind) = match ev {
            FunctionEvent::Enter { function } => (function, Kind::Enter),
            FunctionEvent::Exit { function } => (function, Kind::Exit),
        };
        // Collect first and drop every borrow: callbacks run Lua and Rust
        // code that reach back into the host's shared cells.
        let (calls, caps, plugin, ledger, frame, mode, lua) = {
            let mut i = self.0.borrow_mut();
            if matches!(i.state, ScriptState::Paused { .. }) {
                return;
            }
            let calls: Vec<Target> = i
                .regs
                .borrow()
                .iter()
                .filter(|r| r.kind == kind && r.resolved == Some(function))
                .map(|r| r.target.clone())
                .collect();
            if calls.is_empty() {
                return;
            }
            // Read-only hosts may be throttled; a mod host never is.
            if matches!(i.state, ScriptState::Throttled { .. })
                && !i.manifest.capabilities.write_memory
            {
                i.since_call += 1;
                if i.since_call < i.budget.throttle_to_every_n {
                    i.skipped += 1;
                    return;
                }
            }
            i.since_call = 0;
            i.delivered += 1;
            (
                calls,
                i.manifest.capabilities.clone(),
                i.manifest.id.clone(),
                i.ledger.clone(),
                i.frame.get(),
                i.mode.get(),
                i.lua.clone(),
            )
        };

        let started = Instant::now();
        let mut fault: Option<String> = None;
        for call in calls {
            let result = match call {
                Target::Rust(f) => {
                    let mut mc = ModCtx {
                        ctx: &mut *ctx,
                        caps: &caps,
                        plugin: &plugin,
                        frame,
                        ledger: &ledger,
                        mode,
                    };
                    (f.borrow_mut())(ev, &mut mc);
                    Ok(())
                }
                Target::Lua(func) => match &lua {
                    Some(lua) => call_lua(
                        lua, &func, function, &mut *ctx, &caps, &plugin, frame, &ledger,
                    ),
                    None => Ok(()),
                },
            };
            if let Err(e) = result {
                fault = Some(e.to_string());
                break;
            }
        }
        let cost = started.elapsed();

        let mut i = self.0.borrow_mut();
        if let Some(error) = fault {
            i.log.push(format!("hook fault: {error}"));
            i.state = ScriptState::Paused { error };
        } else if cost > i.budget.per_frame {
            if i.manifest.capabilities.write_memory {
                if !i.warned_over_budget {
                    i.warned_over_budget = true;
                    i.log.push(format!(
                        "hook over budget ({cost:?} > {:?}) — not throttled: it holds write_memory",
                        i.budget.per_frame
                    ));
                }
            } else {
                if !matches!(i.state, ScriptState::Throttled { .. }) {
                    i.log.push(format!(
                        "hook over budget ({cost:?} > {:?}) — seeing every {}th event",
                        i.budget.per_frame, i.budget.throttle_to_every_n
                    ));
                }
                i.state = ScriptState::Throttled { last_cost: cost };
            }
        } else if matches!(i.state, ScriptState::Throttled { .. }) {
            i.log.push("hook back within budget — resuming".to_string());
            i.state = ScriptState::Running;
        }
    }
}

impl CoreSink for HookHost {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: CoreEvent) {}
    fn function_event(&mut self, ev: FunctionEvent, ctx: &mut dyn HookCtx) {
        self.dispatch(ev, ctx);
    }
}

const OUTSIDE: &str = "only valid inside a function hook callback";

/// Bind `rf.on_enter` / `rf.on_exit` (with `function_hooks`) and replace the
/// frame scripts' snapshot-based `rf.mem.*` with callback-only stubs: in a
/// hook host the live machine exists only during a callback, and a read of
/// the empty snapshot would silently return 0.
fn install_lua_api(
    lua: &mlua::Lua,
    caps: &Capabilities,
    regs: &Rc<RefCell<Vec<Reg>>>,
    mode: &Rc<Cell<PlayMode>>,
) -> mlua::Result<()> {
    let rf: mlua::Table = lua.globals().get("rf")?;
    // `rf.mode()` is "original" or "enhanced" (ticket W39-00). It is always
    // present: it reveals nothing about the machine.
    let m = mode.clone();
    rf.set(
        "mode",
        lua.create_function(move |_, ()| Ok(m.get().as_str()))?,
    )?;
    let stub = |lua: &mlua::Lua| {
        lua.create_function(|_, _: mlua::MultiValue| -> mlua::Result<()> {
            Err(mlua::Error::runtime(OUTSIDE))
        })
    };
    if caps.read_memory {
        let mem: mlua::Table = rf.get("mem")?;
        mem.set("read_u8", stub(lua)?)?;
        mem.set("read_u16", stub(lua)?)?;
        rf.set("regs", stub(lua)?)?;
    }
    if caps.write_memory {
        let mem = match rf.get::<mlua::Value>("mem")? {
            mlua::Value::Table(t) => t,
            _ => {
                let t = lua.create_table()?;
                rf.set("mem", t.clone())?;
                t
            }
        };
        mem.set("write_u8", stub(lua)?)?;
    }
    if caps.function_hooks {
        for (name, kind) in [("on_enter", Kind::Enter), ("on_exit", Kind::Exit)] {
            let regs = regs.clone();
            rf.set(
                name,
                lua.create_function(move |_, (key, f): (mlua::Value, mlua::Function)| {
                    let key = match key {
                        mlua::Value::String(s) => HookKey::Name(s.to_string_lossy().to_string()),
                        mlua::Value::Integer(n) => HookKey::Id(
                            u32::try_from(n)
                                .map_err(|_| mlua::Error::runtime("function id out of range"))?,
                        ),
                        _ => {
                            return Err(mlua::Error::runtime(
                                "expected a function name (string) or id (integer)",
                            ))
                        }
                    };
                    let resolved = match &key {
                        HookKey::Id(id) => Some(*id),
                        HookKey::Name(_) => None,
                    };
                    regs.borrow_mut().push(Reg {
                        kind,
                        key,
                        resolved,
                        target: Target::Lua(f),
                    });
                    Ok(())
                })?,
            )?;
        }
    }
    Ok(())
}

fn regs_table(lua: &mlua::Lua, regs: CpuRegs) -> mlua::Result<mlua::Table> {
    let t = lua.create_table()?;
    match regs {
        CpuRegs::Mos6502(r) => {
            t.set("a", r.a)?;
            t.set("x", r.x)?;
            t.set("y", r.y)?;
            t.set("s", r.s)?;
            t.set("p", r.p)?;
            t.set("pc", r.pc)?;
        }
        CpuRegs::Wdc65816(r) => {
            t.set("a", r.a)?;
            t.set("x", r.x)?;
            t.set("y", r.y)?;
            t.set("s", r.sp)?;
            t.set("d", r.d)?;
            t.set("dbr", r.dbr)?;
            t.set("pbr", r.pbr)?;
            t.set("p", r.p)?;
            t.set("pc", r.pc)?;
            t.set("e", r.e)?;
        }
        CpuRegs::None => {}
    }
    Ok(t)
}

/// Run one Lua callback with the live machine bound for exactly its duration.
#[allow(clippy::too_many_arguments)]
fn call_lua(
    lua: &mlua::Lua,
    func: &mlua::Function,
    function: u32,
    ctx: &mut dyn HookCtx,
    caps: &Capabilities,
    plugin: &str,
    frame: u64,
    ledger: &RefCell<WriteLedger>,
) -> mlua::Result<()> {
    let rf: mlua::Table = lua.globals().get("rf")?;
    let mem: Option<mlua::Table> = rf.get("mem")?;
    // Saved so the stubs go back after the callback (the scoped functions
    // are dead by then and would raise a confusing "scope ended" error).
    let saved: Vec<(mlua::Table, &str, mlua::Value)> = {
        let mut v = Vec::new();
        if caps.read_memory {
            v.push((rf.clone(), "regs", rf.get("regs")?));
            if let Some(m) = &mem {
                v.push((m.clone(), "read_u8", m.get("read_u8")?));
                v.push((m.clone(), "read_u16", m.get("read_u16")?));
            }
        }
        if caps.write_memory {
            if let Some(m) = &mem {
                v.push((m.clone(), "write_u8", m.get("write_u8")?));
            }
        }
        v
    };
    let live = RefCell::new(ctx);
    let result =
        lua.scope(|scope| {
            if caps.read_memory {
                rf.set(
                    "regs",
                    scope.create_function(|lua, ()| regs_table(lua, live.borrow().regs()))?,
                )?;
                if let Some(m) = &mem {
                    m.set(
                        "read_u8",
                        scope.create_function(|_, addr: u32| Ok(live.borrow().peek(addr)))?,
                    )?;
                    m.set(
                        "read_u16",
                        scope.create_function(|_, addr: u32| {
                            let c = live.borrow();
                            Ok(u32::from(c.peek(addr))
                                | (u32::from(c.peek(addr.wrapping_add(1))) << 8))
                        })?,
                    )?;
                }
            }
            if caps.write_memory {
                if let Some(m) = &mem {
                    m.set(
                        "write_u8",
                        scope.create_function(|_, (addr, value): (u32, u8)| {
                            Ok(poke_ledgered(
                                &mut **live.borrow_mut(),
                                caps,
                                plugin,
                                frame,
                                ledger,
                                addr,
                                value,
                            ))
                        })?,
                    )?;
                }
            }
            func.call::<()>(function)
        });
    for (table, name, value) in saved {
        table.set(name, value)?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Capabilities;

    /// A fake machine: 64 KiB, registers, refuses writes at `$2000+`.
    struct Fake {
        mem: Vec<u8>,
        pc: u16,
        pokes: Vec<(u32, u8)>,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                mem: vec![0; 0x1_0000],
                pc: 0x8123,
                pokes: vec![],
            }
        }
    }

    impl HookCtx for Fake {
        fn regs(&self) -> CpuRegs {
            CpuRegs::Mos6502(rf_core_api::Mos6502Regs {
                a: 1,
                x: 2,
                y: 3,
                s: 0xFD,
                pc: self.pc,
                p: 0x24,
            })
        }
        fn peek(&self, addr: u32) -> u8 {
            self.mem[addr as usize & 0xFFFF]
        }
        fn poke(&mut self, addr: u32, value: u8) -> bool {
            if addr >= 0x2000 {
                return false;
            }
            self.mem[addr as usize] = value;
            self.pokes.push((addr, value));
            true
        }
    }

    fn manifest(caps: Capabilities) -> Manifest {
        Manifest {
            id: "hook-test".into(),
            version: "0.1.0".into(),
            api: "0.1".into(),
            license: "MIT".into(),
            provenance: "test".into(),
            capabilities: caps,
        }
    }

    fn caps(read: bool, write: bool) -> Capabilities {
        Capabilities {
            function_hooks: true,
            read_memory: read,
            write_memory: write,
            ..Capabilities::default()
        }
    }

    fn host(c: Capabilities, src: &str) -> HookHost {
        HookHost::load(manifest(c), src, Budget::default(), &Bridge::default()).expect("loads")
    }

    fn enter(function: u32) -> FunctionEvent {
        FunctionEvent::Enter { function }
    }

    #[test]
    fn a_lua_hook_reads_registers_and_memory_and_names_resolve_late() {
        let h = host(
            caps(true, false),
            r#"
            calls = 0
            seen_pc = 0
            seen_byte = 0
            seen_word = 0
            rf.on_enter("nmi", function(id)
                calls = calls + 1
                seen_pc = rf.regs().pc
                seen_byte = rf.mem.read_u8(0x10)
                seen_word = rf.mem.read_u16(0x10)
            end)
            "#,
        );
        let mut m = Fake::new();
        m.mem[0x10] = 0x34;
        m.mem[0x11] = 0x12;
        // Unresolved: reported inactive, and the event does nothing.
        let res = h.resolve(&|_| None);
        assert_eq!(res.inactive, ["nmi"]);
        assert!(h.function_ids().is_empty());
        h.dispatch(enter(7), &mut m);
        assert_eq!(h.lua_global_i64("calls"), Some(0));
        // Resolved: active, and the hooked id is what the core is told.
        let res = h.resolve(&|n| (n == "nmi").then_some(7));
        assert_eq!(res.active, [("nmi".to_string(), 7)]);
        assert_eq!(h.function_ids(), [7]);
        h.dispatch(enter(7), &mut m);
        h.dispatch(enter(8), &mut m); // another function: not ours
        h.dispatch(FunctionEvent::Exit { function: 7 }, &mut m); // no exit hook
        assert_eq!(h.lua_global_i64("calls"), Some(1));
        assert_eq!(h.lua_global_i64("seen_pc"), Some(0x8123));
        assert_eq!(h.lua_global_i64("seen_byte"), Some(0x34));
        assert_eq!(h.lua_global_i64("seen_word"), Some(0x1234));
        assert!(h.ledger_entries().is_empty());
        assert!(h.panel_lines()[0].contains("1 active, 0 inactive"));
    }

    #[test]
    fn on_exit_hooks_fire_on_exit_events_only_and_see_the_return_pc() {
        let h = host(
            caps(true, false),
            r#"
            exits = 0
            enters = 0
            exit_pc = 0
            rf.on_exit(4, function(id) exits = exits + 1; exit_pc = rf.regs().pc end)
            rf.on_enter(4, function(id) enters = enters + 1 end)
            "#,
        );
        let mut m = Fake::new();
        m.pc = 0x9003;
        h.dispatch(FunctionEvent::Exit { function: 4 }, &mut m);
        h.dispatch(FunctionEvent::Exit { function: 5 }, &mut m);
        assert_eq!(
            (
                h.lua_global_i64("exits"),
                h.lua_global_i64("enters"),
                h.lua_global_i64("exit_pc")
            ),
            (Some(1), Some(0), Some(0x9003))
        );
        h.dispatch(enter(4), &mut m);
        assert_eq!(h.lua_global_i64("enters"), Some(1));
        assert_eq!(h.lua_global_i64("exits"), Some(1));
    }

    #[test]
    fn the_machine_is_only_reachable_inside_a_callback() {
        let h = host(
            caps(true, true),
            r#"
            outside_read = tostring(select(2, pcall(rf.mem.read_u8, 0)))
            outside_write = tostring(select(2, pcall(rf.mem.write_u8, 0, 1)))
            outside_regs = tostring(select(2, pcall(rf.regs)))
            "#,
        );
        for g in ["outside_read", "outside_write", "outside_regs"] {
            let msg = h.lua_global_string(g).unwrap_or_default();
            assert!(
                msg.contains("only valid inside a function hook callback"),
                "{g}: {msg}"
            );
        }
        // A callback may read after an earlier callback returned: the stubs
        // were restored, not left pointing at a dead scope.
        let h = host(
            caps(true, false),
            r#"
            n = 0
            rf.on_enter(1, function() n = n + rf.mem.read_u8(5) end)
            "#,
        );
        let mut m = Fake::new();
        m.mem[5] = 3;
        h.dispatch(enter(1), &mut m);
        h.dispatch(enter(1), &mut m);
        assert_eq!(h.lua_global_i64("n"), Some(6));
    }

    #[test]
    fn denied_capabilities_are_absent() {
        // function_hooks only: no registers, no memory, no writes.
        let h = host(
            Capabilities {
                function_hooks: true,
                ..Capabilities::default()
            },
            r#"
            has_regs = rf.regs ~= nil
            has_mem = rf.mem ~= nil
            rf.on_enter(1, function() end)
            "#,
        );
        let i = h.0.borrow();
        let lua = i.lua.as_ref().expect("lua");
        assert!(!lua.globals().get::<bool>("has_regs").unwrap());
        assert!(!lua.globals().get::<bool>("has_mem").unwrap());
        drop(i);
        // No function_hooks: rf.on_enter does not exist, so the script that
        // tries to hook fails to load.
        let err = HookHost::load(
            manifest(Capabilities::default()),
            "rf.on_enter('x', function() end)",
            Budget::default(),
            &Bridge::default(),
        );
        assert!(err.is_err());
        // Rust hooks are refused without it too.
        let rust = HookHost::new(manifest(Capabilities::default()), Budget::default());
        assert_eq!(
            rust.on_enter("x", |_, _| {}),
            Err(HookRefusal::NoFunctionHooksCapability)
        );
    }

    #[test]
    fn a_lua_write_needs_write_memory_and_is_ledgered_with_who_and_when() {
        let src = r#"
            rf.on_enter(3, function() ok = rf.mem.write_u8(0x75A, 7) and 1 or 0 end)
        "#;
        let h = host(caps(true, true), src);
        h.set_frame(41);
        let mut m = Fake::new();
        h.dispatch(enter(3), &mut m);
        assert_eq!(m.pokes, [(0x75A, 7)]);
        let l = h.ledger_entries();
        assert_eq!(l.len(), 1);
        assert_eq!(
            (l[0].frame, l[0].addr, l[0].value, l[0].plugin.as_str()),
            (41, 0x75A, 7, "hook-test")
        );
        assert_eq!(h.lua_global_i64("ok"), Some(1));
        assert!(h.panel_lines().iter().any(|l| l.contains("MOD write")));

        // The core refusing an address leaves no ledger entry.
        let h2 = host(
            caps(true, true),
            "rf.on_enter(3, function() ok = rf.mem.write_u8(0x2000, 7) and 1 or 0 end)",
        );
        h2.dispatch(enter(3), &mut m);
        assert_eq!(h2.lua_global_i64("ok"), Some(0));
        assert!(h2.ledger_entries().is_empty());

        // Without the capability the function is absent: the callback faults
        // and the host pauses, with nothing written.
        let ro = host(caps(true, false), src);
        let mut m2 = Fake::new();
        ro.dispatch(enter(3), &mut m2);
        assert!(m2.pokes.is_empty());
        assert!(matches!(ro.state(), ScriptState::Paused { .. }));
        assert!(ro.ledger_entries().is_empty());
    }

    #[test]
    fn a_lua_error_pauses_the_host_not_the_caller() {
        let h = host(
            caps(true, false),
            "n = 0\nrf.on_enter(1, function() n = n + 1\n error('boom') end)",
        );
        let mut m = Fake::new();
        h.dispatch(enter(1), &mut m);
        assert!(matches!(h.state(), ScriptState::Paused { .. }));
        h.dispatch(enter(1), &mut m);
        assert_eq!(
            h.lua_global_i64("n"),
            Some(1),
            "paused: later events ignored"
        );
        assert!(h.log_lines().iter().any(|l| l.contains("hook fault")));
        assert!(h.panel_lines()[0].contains("paused"));
    }

    #[test]
    fn a_rust_hook_pokes_through_the_same_ledger() {
        let h = HookHost::new(manifest(caps(true, true)), Budget::default());
        h.on_enter(9u32, |_ev, m| {
            let lives = m.peek(0x75A);
            assert!(m.poke(0x75A, lives.wrapping_add(5)));
            assert!(!m.poke(0x4000, 1), "the core refuses it, so no entry");
        })
        .unwrap();
        h.set_frame(5);
        let mut m = Fake::new();
        h.dispatch(enter(9), &mut m);
        assert_eq!(m.mem[0x75A], 5);
        let l = h.ledger_entries();
        assert_eq!(l.len(), 1);
        assert_eq!((l[0].frame, l[0].addr), (5, 0x75A));

        // Same hook on a host without write_memory: nothing is written.
        let ro = HookHost::new(manifest(caps(true, false)), Budget::default());
        ro.on_enter(9u32, |_ev, m| {
            assert!(!m.poke(0x75A, 9));
        })
        .unwrap();
        let mut m2 = Fake::new();
        ro.dispatch(enter(9), &mut m2);
        assert!(m2.pokes.is_empty());
        assert!(ro.ledger_entries().is_empty());
    }

    #[test]
    fn a_read_only_host_is_throttled_over_budget_but_a_mod_host_never_is() {
        let zero = Budget {
            per_frame: std::time::Duration::ZERO,
            throttle_to_every_n: 4,
        };
        let run = |c: Capabilities| {
            let h = HookHost::load(
                manifest(c),
                "n = 0\nrf.on_enter(1, function() n = n + 1 end)",
                zero,
                &Bridge::default(),
            )
            .unwrap();
            let mut m = Fake::new();
            for _ in 0..20 {
                h.dispatch(enter(1), &mut m);
            }
            (h.lua_global_i64("n").unwrap(), h.state())
        };
        let (n, state) = run(caps(true, false));
        assert!(matches!(state, ScriptState::Throttled { .. }));
        assert!((5..20).contains(&n), "throttled: saw {n} of 20");
        let (n, state) = run(caps(true, true));
        assert_eq!((n, state), (20, ScriptState::Running));
    }
}
