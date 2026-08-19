//! The script host: error containment, per-frame budget throttling, the
//! write ledger, and `cache_dir` path containment (ticket W4-04;
//! `docs/design/PLUGINS.md` §4, FR-PLUG-003/004/005, NFR-010/D-006).

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::manifest::{Capabilities, FilesystemCap, Manifest};
use crate::sandbox::{self, ScriptLog};

/// Why a path was refused (NFR-010, D-006).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathRefusal {
    /// The `filesystem` capability was not granted at all.
    NoFilesystemCapability,
    /// The path resolved outside the plugin's cache dir — the symlink
    /// escape D-006 names. Carries both paths, because "denied" without
    /// saying *where it went* is unactionable for the author.
    EscapesCacheDir { resolved: PathBuf, root: PathBuf },
    /// The path (or the root) could not be canonicalized.
    Unresolvable(String),
}

impl std::fmt::Display for PathRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathRefusal::NoFilesystemCapability => write!(
                f,
                "plugin has no `filesystem` capability — declare `filesystem = \"cache_dir\"`"
            ),
            PathRefusal::EscapesCacheDir { resolved, root } => write!(
                f,
                "path escapes the plugin cache dir: {} resolves outside {} (NFR-010/D-006 — \
                 symlink escapes are refused)",
                resolved.display(),
                root.display()
            ),
            PathRefusal::Unresolvable(msg) => write!(f, "path cannot be resolved: {msg}"),
        }
    }
}

/// Resolve `requested` inside `cache_root`, refusing anything that
/// escapes (D-006's law, applied to the plugin `cache_dir` cap).
///
/// **Canonicalize both sides, then compare** — the same shape
/// `retroforge::library`'s scan uses. Comparing un-canonicalized paths is
/// the classic hole: `cache/../../etc/passwd` is textually "inside"
/// `cache/` until you resolve it, and a symlink is invisible to any
/// amount of string manipulation.
///
/// # Errors
/// [`PathRefusal`] when the capability is absent, either side cannot be
/// canonicalized, or the resolved path is outside the root.
pub fn resolve_in_cache_dir(
    caps: &Capabilities,
    cache_root: &Path,
    requested: &Path,
) -> Result<PathBuf, PathRefusal> {
    if caps.filesystem != FilesystemCap::CacheDir {
        return Err(PathRefusal::NoFilesystemCapability);
    }
    let root = cache_root
        .canonicalize()
        .map_err(|e| PathRefusal::Unresolvable(format!("{}: {e}", cache_root.display())))?;

    let joined = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };

    // A path that does not exist yet (a file the plugin is about to
    // create) cannot be canonicalized, so resolve its PARENT and re-append
    // the final component. Refusing to create new files would make the
    // capability useless; resolving the parent still catches every
    // symlink and `..` in the path that leads there.
    let resolved = match joined.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            let parent = joined
                .parent()
                .ok_or_else(|| PathRefusal::Unresolvable(joined.display().to_string()))?;
            let file = joined
                .file_name()
                .ok_or_else(|| PathRefusal::Unresolvable(joined.display().to_string()))?;
            parent
                .canonicalize()
                .map_err(|e| PathRefusal::Unresolvable(format!("{}: {e}", parent.display())))?
                .join(file)
        }
    };

    if resolved.starts_with(&root) {
        Ok(resolved)
    } else {
        Err(PathRefusal::EscapesCacheDir { resolved, root })
    }
}

/// One recorded memory write (`PLUGINS.md` §2: "the host routes all
/// writes through a ledger surfaced in the UI").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerEntry {
    pub frame: u64,
    pub addr: u32,
    pub value: u8,
    pub plugin: String,
}

/// Every write a plugin has made this session (FR-PLUG-003).
#[derive(Debug, Clone, Default)]
pub struct WriteLedger {
    entries: Vec<LedgerEntry>,
}

impl WriteLedger {
    /// Record a write, or refuse it when `write_memory` is not granted.
    ///
    /// Returns whether the write is permitted, so the caller cannot
    /// perform one without it having been recorded: the check and the
    /// record are the same call, rather than two a caller could do in the
    /// wrong order or forget to pair.
    pub fn record(
        &mut self,
        caps: &Capabilities,
        plugin: &str,
        frame: u64,
        addr: u32,
        value: u8,
    ) -> bool {
        if !caps.write_memory {
            return false;
        }
        self.entries.push(LedgerEntry {
            frame,
            addr,
            value,
            plugin: plugin.to_string(),
        });
        true
    }

    #[must_use]
    pub fn entries(&self) -> &[LedgerEntry] {
        &self.entries
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Per-frame budget policy (`PLUGINS.md` §4: "host measures callback
/// time; over-budget plugins get throttled to every-N-frames with a UI
/// warning (protects frame pacing)").
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    /// How long one callback may take before it counts as over-budget.
    pub per_frame: Duration,
    /// How many frames to skip between calls once throttled.
    pub throttle_to_every_n: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            // W2-18's frame budget is 16.64 ms for EVERYTHING; a script
            // getting an eighth of it is already generous, and the number
            // is stated here rather than buried so it can be argued with.
            per_frame: Duration::from_millis(2),
            throttle_to_every_n: 8,
        }
    }
}

/// Why a script is not running right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptState {
    Running,
    /// Throttled for exceeding its per-frame budget — still live, just
    /// called less often.
    Throttled {
        last_cost: Duration,
    },
    /// Faulted and auto-paused (FR-PLUG-004). Carries the error so the UI
    /// can show it rather than only that "something went wrong".
    Paused {
        error: String,
    },
}

/// One loaded script plus everything the host tracks about it.
pub struct ScriptHost {
    pub manifest: Manifest,
    pub log: ScriptLog,
    lua: mlua::Lua,
    state: ScriptState,
    budget: Budget,
    ledger: WriteLedger,
    frames_since_call: u64,
    /// Ticket W5-07: what the shell publishes to the script and collects
    /// back. Cloned (it is `Rc`-backed), so the host and the shell see
    /// the same buffers.
    bridge: crate::sandbox::Bridge,
}

impl ScriptHost {
    /// Load `source` into a fresh sandbox for `manifest`.
    ///
    /// # Errors
    /// Returns the Lua error if the sandbox cannot be built or the script
    /// fails to compile. A script that will not even load is a load-time
    /// failure, not a paused script — there is nothing to pause.
    pub fn load(manifest: Manifest, source: &str, budget: Budget) -> mlua::Result<Self> {
        Self::load_with_bridge(manifest, source, budget, crate::sandbox::Bridge::default())
    }

    /// [`ScriptHost::load`], but against a bridge the caller has already
    /// populated (ticket W5-07).
    ///
    /// **This exists because of how scripts are actually written.** The
    /// natural way to use a published memory map is to resolve it once,
    /// at load:
    ///
    /// ```lua
    /// local PLAYER_X = rf.profile.addr("player_x")
    /// function on_frame(n) ... end
    /// ```
    ///
    /// which is what the shipped example does. If the host publishes the
    /// labels only after `load`, that resolves to `nil` and stays `nil`
    /// forever — the script runs, faults nothing, draws nothing, and
    /// looks like a broken overlay rather than a lifecycle mistake. The
    /// profile is known when the ROM is opened, which is before any
    /// script runs, so there is no reason to publish late.
    ///
    /// # Errors
    /// As [`ScriptHost::load`].
    pub fn load_with_bridge(
        manifest: Manifest,
        source: &str,
        budget: Budget,
        bridge: crate::sandbox::Bridge,
    ) -> mlua::Result<Self> {
        let log = ScriptLog::new();
        let lua = sandbox::build_with_bridge(&manifest.capabilities, &log, &bridge)?;
        lua.load(source).exec()?;
        Ok(ScriptHost {
            manifest,
            log,
            lua,
            state: ScriptState::Running,
            budget,
            ledger: WriteLedger::default(),
            frames_since_call: 0,
            bridge,
        })
    }

    /// The live-data bridge (ticket W5-07): publish this frame's memory
    /// window and profile labels here, and drain the overlay after
    /// [`ScriptHost::on_frame`].
    #[must_use]
    pub fn bridge(&self) -> &crate::sandbox::Bridge {
        &self.bridge
    }

    #[must_use]
    pub fn state(&self) -> &ScriptState {
        &self.state
    }

    #[must_use]
    pub fn ledger(&self) -> &WriteLedger {
        &self.ledger
    }

    /// Record a write on this script's behalf — see [`WriteLedger::record`].
    pub fn record_write(&mut self, frame: u64, addr: u32, value: u8) -> bool {
        let id = self.manifest.id.clone();
        self.ledger
            .record(&self.manifest.capabilities, &id, frame, addr, value)
    }

    /// Call the script's `on_frame` for `frame`, applying containment and
    /// throttling.
    ///
    /// **Never returns `Err`.** FR-PLUG-004 is "script fault pauses script
    /// not emulator", so a fault is a state change here rather than an
    /// error the caller might propagate into the frame loop. That is
    /// encoded in the signature rather than left to callers to remember.
    pub fn on_frame(&mut self, frame: u64) {
        match &self.state {
            ScriptState::Paused { .. } => return,
            ScriptState::Throttled { .. } => {
                self.frames_since_call += 1;
                if self.frames_since_call < self.budget.throttle_to_every_n {
                    return;
                }
            }
            ScriptState::Running => {}
        }
        self.frames_since_call = 0;

        let Ok(func) = self.lua.globals().get::<mlua::Function>("on_frame") else {
            // No callback registered is not an error — plenty of scripts
            // are load-time only.
            return;
        };

        let started = std::time::Instant::now();
        let result = func.call::<()>(frame);
        let cost = started.elapsed();

        match result {
            Err(e) => {
                // FR-PLUG-004. The error goes to the log too, so the REPL
                // shows it without the UI having to reach into state.
                self.log.push(format!("script fault: {e}"));
                self.state = ScriptState::Paused {
                    error: e.to_string(),
                };
            }
            Ok(()) => {
                if cost > self.budget.per_frame {
                    if !matches!(self.state, ScriptState::Throttled { .. }) {
                        self.log.push(format!(
                            "over budget ({cost:?} > {:?}) — throttling to every {} frames",
                            self.budget.per_frame, self.budget.throttle_to_every_n
                        ));
                    }
                    self.state = ScriptState::Throttled { last_cost: cost };
                } else if matches!(self.state, ScriptState::Throttled { .. }) {
                    // Released: a script that got slow because of one
                    // heavy frame must be able to come back, or the
                    // throttle is a one-way punishment.
                    self.log.push("back within budget — resuming".to_string());
                    self.state = ScriptState::Running;
                }
            }
        }
    }

    /// Evaluate one REPL line, returning what to print (DEBUGGER.md §5).
    ///
    /// Errors come back as text rather than as `Err`: a REPL that dies on
    /// a typo is not a REPL.
    pub fn eval_repl(&mut self, line: &str) -> String {
        match self.lua.load(line).eval::<mlua::Value>() {
            Ok(mlua::Value::Nil) => String::new(),
            Ok(v) => format!("{v:?}"),
            Err(e) => format!("error: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Capabilities;

    fn manifest(caps: Capabilities) -> Manifest {
        Manifest {
            id: "test-plugin".into(),
            version: "0.1.0".into(),
            api: "0.1".into(),
            license: "MIT".into(),
            provenance: "test".into(),
            capabilities: caps,
        }
    }

    /// FR-PLUG-004: a faulting script pauses ITSELF. The host survives and
    /// keeps being callable — which is the half that matters, since "the
    /// emulator did not crash" is the actual requirement.
    #[test]
    fn a_faulting_script_pauses_itself_and_never_takes_the_host_down() {
        let mut host = ScriptHost::load(
            manifest(Capabilities::default()),
            "function on_frame(f) error('boom on frame ' .. f) end",
            Budget::default(),
        )
        .expect("script loads");
        assert_eq!(host.state(), &ScriptState::Running);

        host.on_frame(1);
        match host.state() {
            ScriptState::Paused { error } => assert!(error.contains("boom on frame 1"), "{error}"),
            other => panic!("expected Paused, got {other:?}"),
        }

        // And the host keeps working: further frames are no-ops rather
        // than repeated faults or panics.
        host.on_frame(2);
        host.on_frame(3);
        assert!(matches!(host.state(), ScriptState::Paused { .. }));
        assert!(
            host.log.lines().iter().any(|l| l.contains("script fault")),
            "the fault must reach the log the REPL renders"
        );
    }

    /// FR-PLUG-005, both directions: the throttle ENGAGES on an
    /// over-budget script and RELEASES when it comes back. Testing only
    /// the engage half would pass on a one-way punishment.
    #[test]
    fn an_over_budget_script_is_throttled_and_can_recover() {
        // A budget of zero makes any callback over-budget, which is how
        // this stays a fast unit test rather than one that must burn real
        // milliseconds to prove a timing rule.
        let budget = Budget {
            per_frame: Duration::ZERO,
            throttle_to_every_n: 4,
        };
        let mut host = ScriptHost::load(
            manifest(Capabilities::default()),
            "calls = 0\nfunction on_frame(f) calls = calls + 1 end",
            budget,
        )
        .unwrap();

        host.on_frame(1);
        assert!(
            matches!(host.state(), ScriptState::Throttled { .. }),
            "any cost exceeds a zero budget, so this must throttle"
        );

        // "every 4th frame" means three skipped frames and then a call:
        // frames 2, 3 and 4 are skipped, frame 5 calls again.
        for f in 2..=4 {
            host.on_frame(f);
        }
        assert_eq!(
            host.lua.globals().get::<i64>("calls").unwrap(),
            1,
            "frames 2-4 must be skipped while throttled"
        );
        host.on_frame(5);
        assert_eq!(
            host.lua.globals().get::<i64>("calls").unwrap(),
            2,
            "the 4th frame after the last call runs again"
        );

        // Now make it cheap enough to release.
        host.budget.per_frame = Duration::from_secs(60);
        for f in 6..=9 {
            host.on_frame(f);
        }
        assert_eq!(
            host.state(),
            &ScriptState::Running,
            "a script that comes back within budget must be released, not punished forever"
        );
    }

    /// NFR-010 / D-006: a symlink out of the cache dir is refused, with a
    /// diagnostic naming where it went.
    #[test]
    fn a_symlink_escaping_the_cache_dir_is_refused_with_a_diagnostic() {
        let tmp = std::env::temp_dir().join(format!("rf-plugin-cap-{}", std::process::id()));
        let cache = tmp.join("cache");
        let outside = tmp.join("outside");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), b"nope").unwrap();

        let caps = Capabilities {
            filesystem: FilesystemCap::CacheDir,
            ..Capabilities::default()
        };

        // Baseline: an ordinary path inside the cache dir resolves. Without
        // this the test would pass on an implementation that refused
        // everything.
        std::fs::write(cache.join("ok.txt"), b"fine").unwrap();
        assert!(resolve_in_cache_dir(&caps, &cache, Path::new("ok.txt")).is_ok());

        // A symlink pointing outside must be refused.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, cache.join("escape")).unwrap();
            let err = resolve_in_cache_dir(&caps, &cache, Path::new("escape/secret.txt"))
                .expect_err("a symlink out of the cache dir must be refused");
            match &err {
                PathRefusal::EscapesCacheDir { resolved, .. } => {
                    assert!(resolved.to_string_lossy().contains("secret.txt"));
                }
                other => panic!("expected EscapesCacheDir, got {other:?}"),
            }
            assert!(
                err.to_string().contains("escapes the plugin cache dir"),
                "the refusal must say WHERE it went: {err}"
            );
        }

        // And plain `..` traversal, which needs no symlink at all.
        assert!(matches!(
            resolve_in_cache_dir(&caps, &cache, Path::new("../outside/secret.txt")),
            Err(PathRefusal::EscapesCacheDir { .. })
        ));

        // No capability means no path resolves, however innocent.
        assert_eq!(
            resolve_in_cache_dir(&Capabilities::default(), &cache, Path::new("ok.txt")),
            Err(PathRefusal::NoFilesystemCapability)
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A file the plugin is about to CREATE does not exist yet, so it
    /// cannot be canonicalized — but its parent can, which still catches
    /// every symlink and `..` on the way there.
    #[test]
    fn a_not_yet_existing_file_inside_the_cache_dir_is_allowed() {
        let tmp = std::env::temp_dir().join(format!("rf-plugin-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let caps = Capabilities {
            filesystem: FilesystemCap::CacheDir,
            ..Capabilities::default()
        };
        assert!(resolve_in_cache_dir(&caps, &tmp, Path::new("brand-new.bin")).is_ok());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The ledger is the gate, not a log kept alongside one: `record`
    /// returns whether the write may proceed, so a caller cannot perform
    /// a write without it having been recorded.
    #[test]
    fn writes_are_refused_without_the_capability_and_recorded_with_it() {
        let mut denied =
            ScriptHost::load(manifest(Capabilities::default()), "", Budget::default()).unwrap();
        assert!(!denied.record_write(1, 0x0300, 0xFF), "no write_memory cap");
        assert!(denied.ledger().is_empty());

        let mut granted = ScriptHost::load(
            manifest(Capabilities {
                write_memory: true,
                ..Capabilities::default()
            }),
            "",
            Budget::default(),
        )
        .unwrap();
        assert!(granted.record_write(7, 0x0300, 0x2A));
        assert_eq!(granted.ledger().entries().len(), 1);
        let e = &granted.ledger().entries()[0];
        assert_eq!((e.frame, e.addr, e.value), (7, 0x0300, 0x2A));
        assert_eq!(e.plugin, "test-plugin", "the ledger must say WHO wrote");
    }

    /// A REPL that dies on a typo is not a REPL (DEBUGGER.md §5).
    #[test]
    fn the_repl_returns_errors_as_text_and_keeps_working() {
        let mut host =
            ScriptHost::load(manifest(Capabilities::default()), "", Budget::default()).unwrap();
        assert!(host.eval_repl("this is not lua").starts_with("error:"));
        assert_eq!(host.eval_repl("return 6 * 7"), "Integer(42)");
    }
}
