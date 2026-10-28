//! The plugin surface: logic, fact and render plugins over recipe facts
//! (ticket W39-00; `docs/design/REBUILD_LAB.md` section 9, "Plugin kinds over
//! the recorded living decomp").
//!
//! A manifest declares one `kind`, and the kind fixes what the plugin may ask
//! for ([`SurfaceManifest::parse`] refuses a manifest whose capabilities
//! exceed its kind):
//!
//! | kind | may | refused capabilities |
//! |---|---|---|
//! | `fact` | add and refine recipe rows ([`FactPlugin`]); changes nothing at run time | everything that acts: `write_memory`, `function_hooks`, `draw_overlay`, `replace_layers`, `input_bindings`, `filesystem` |
//! | `render` | read recipe facts and memory, emit draw data ([`RenderPlugin`]) | `write_memory`, `function_hooks`, `input_bindings`, `filesystem` |
//! | `logic` | hook functions and write memory (the mod tier) | none, but see below |
//!
//! ## The Original-mode gate (law 6)
//!
//! Original mode is the reference and stays byte-identical. A logic plugin
//! therefore must do nothing while the player is in [`PlayMode::Original`]
//! (`rf.mode() == "original"`; a [`crate::ModCtx::mode`] in Rust). It is only
//! *enabled* after the shadow-lockstep checker has run the game with the
//! plugin loaded in Original mode and found zero divergence from the plain
//! interpreter: that run is an [`OriginalModeProof`], and [`enable`] refuses a
//! logic plugin without a clean proof for that plugin. The SDK cannot run the
//! checker (it names no console core); the shell or a test builds the proof
//! from `rf_harness::lockstep::run` and hands it in. A plugin that writes
//! regardless of mode diverges, and is blocked with the frame and field.

use std::collections::BTreeMap;

use rf_rebuild::{merge_facts, FactMerge, FactRefusal, Recipe};

use crate::host::{Budget, ScriptHost};
use crate::manifest::{FilesystemCap, Manifest, ManifestError};
use crate::sandbox::{Bridge, MemoryWindow, OverlayCmd};

/// Which mode the player is in. A fresh install and a fresh host are
/// [`PlayMode::Original`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PlayMode {
    /// Accuracy mode: the unmodified simulation.
    #[default]
    Original,
    /// Super Enhanced: logic plugins may act.
    Enhanced,
}

impl PlayMode {
    /// The string `rf.mode()` returns to Lua.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Enhanced => "enhanced",
        }
    }
}

/// What a plugin is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginKind {
    Logic,
    Fact,
    Render,
}

impl PluginKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Logic => "logic",
            Self::Fact => "fact",
            Self::Render => "render",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "logic" => Some(Self::Logic),
            "fact" => Some(Self::Fact),
            "render" => Some(Self::Render),
            _ => None,
        }
    }

    /// Capabilities this kind may not hold, as `(name, held)` pairs.
    fn forbidden(self, m: &Manifest) -> Vec<(&'static str, bool)> {
        let c = &m.capabilities;
        let acts = [
            ("write_memory", c.write_memory),
            ("function_hooks", c.function_hooks),
            ("input_bindings", c.input_bindings),
            ("filesystem", c.filesystem != FilesystemCap::None),
        ];
        match self {
            Self::Logic => vec![],
            Self::Render => acts.to_vec(),
            Self::Fact => {
                let mut v = acts.to_vec();
                v.push(("draw_overlay", c.draw_overlay));
                v.push(("replace_layers", c.replace_layers));
                v
            }
        }
    }
}

/// Why a manifest is not a valid surface manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceError {
    Manifest(ManifestError),
    /// No `kind = "logic" | "fact" | "render"`.
    MissingKind,
    UnknownKind(String),
    /// The kind may not hold this capability.
    KindForbids {
        kind: PluginKind,
        capability: &'static str,
    },
    /// A loader was handed a plugin of another kind.
    WrongKind {
        expected: PluginKind,
        found: PluginKind,
    },
    /// The plugin's own file would not load.
    Load(String),
}

impl std::fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Manifest(e) => write!(f, "{e}"),
            Self::MissingKind => write!(
                f,
                "plugin manifest has no `kind` — declare kind = \"logic\", \"fact\" or \"render\""
            ),
            Self::UnknownKind(k) => write!(
                f,
                "unknown plugin kind `{k}` — expected logic, fact or render"
            ),
            Self::KindForbids { kind, capability } => {
                write!(f, "a {} plugin may not hold `{capability}`", kind.as_str())
            }
            Self::WrongKind { expected, found } => write!(
                f,
                "expected a {} plugin, this one is {}",
                expected.as_str(),
                found.as_str()
            ),
            Self::Load(e) => write!(f, "plugin failed to load: {e}"),
        }
    }
}

impl std::error::Error for SurfaceError {}

/// A [`Manifest`] plus its declared [`PluginKind`]. (Kept beside `Manifest`,
/// not inside it, so existing manifest literals are unaffected.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceManifest {
    pub manifest: Manifest,
    pub kind: PluginKind,
}

impl SurfaceManifest {
    /// # Errors
    /// [`SurfaceError`]: a bad base manifest, a missing or unknown `kind`, or
    /// a capability the kind may not hold.
    pub fn parse(text: &str) -> Result<Self, SurfaceError> {
        let manifest = Manifest::parse(text).map_err(SurfaceError::Manifest)?;
        let mut kind: Option<String> = None;
        for raw in text.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if let Some((k, v)) = line.split_once('=') {
                if k.trim() == "kind" {
                    kind = Some(v.trim().trim_matches('"').to_string());
                }
            }
        }
        let kind = kind.ok_or(SurfaceError::MissingKind)?;
        let kind = PluginKind::parse(&kind).ok_or(SurfaceError::UnknownKind(kind))?;
        if let Some((capability, _)) = kind
            .forbidden(&manifest)
            .into_iter()
            .find(|(_, held)| *held)
        {
            return Err(SurfaceError::KindForbids { kind, capability });
        }
        Ok(Self { manifest, kind })
    }

    fn expect(&self, expected: PluginKind) -> Result<(), SurfaceError> {
        if self.kind == expected {
            Ok(())
        } else {
            Err(SurfaceError::WrongKind {
                expected,
                found: self.kind,
            })
        }
    }
}

// ---- logic plugins: the Original-mode gate --------------------------------

/// The fewest frames a clean run may cover to count as a proof.
pub const MIN_PROOF_FRAMES: u32 = 30;

/// Where a lockstep run first differed from the plain interpreter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProofDivergence {
    pub frame: u32,
    /// The first differing field (`"wram"`, `"cpu"`, ...).
    pub field: String,
}

/// The result of running the game under shadow lockstep with a logic plugin
/// loaded in [`PlayMode::Original`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginalModeProof {
    /// The manifest id of the plugin that was loaded for the run.
    pub plugin_id: String,
    pub frames: u32,
    pub divergence: Option<ProofDivergence>,
}

/// Why a plugin was not enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceRefusal {
    /// A logic plugin with no Original-mode proof.
    NoProof { plugin: String },
    /// The proof was made for another plugin.
    ProofForOtherPlugin { plugin: String, proof_for: String },
    /// A clean run too short to mean anything.
    ProofTooShort { plugin: String, frames: u32 },
    /// The plugin changes the game in Original mode.
    BreaksOriginalMode {
        plugin: String,
        frame: u32,
        field: String,
    },
}

impl std::fmt::Display for SurfaceRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoProof { plugin } => write!(
                f,
                "logic plugin `{plugin}` is blocked: it has not been run under shadow lockstep \
                 in Original mode"
            ),
            Self::ProofForOtherPlugin { plugin, proof_for } => write!(
                f,
                "logic plugin `{plugin}` is blocked: the lockstep proof offered was made for \
                 `{proof_for}`"
            ),
            Self::ProofTooShort { plugin, frames } => write!(
                f,
                "logic plugin `{plugin}` is blocked: its lockstep run covered {frames} frames, \
                 under the {MIN_PROOF_FRAMES} needed"
            ),
            Self::BreaksOriginalMode {
                plugin,
                frame,
                field,
            } => write!(
                f,
                "logic plugin `{plugin}` is blocked: it changes `{field}` in Original mode \
                 (first at frame {frame}); Original mode must stay byte-identical, so a logic \
                 plugin has to do nothing unless rf.mode() is \"enhanced\""
            ),
        }
    }
}

impl std::error::Error for SurfaceRefusal {}

/// Enable a plugin. Fact and render plugins cannot act, so they enable on
/// their manifest alone; a logic plugin needs a clean [`OriginalModeProof`]
/// made for it.
///
/// # Errors
/// [`SurfaceRefusal`] for a logic plugin without a usable clean proof.
pub fn enable(
    plugin: &SurfaceManifest,
    proof: Option<&OriginalModeProof>,
) -> Result<PluginKind, SurfaceRefusal> {
    if plugin.kind != PluginKind::Logic {
        return Ok(plugin.kind);
    }
    let id = plugin.manifest.id.clone();
    let proof = proof.ok_or_else(|| SurfaceRefusal::NoProof { plugin: id.clone() })?;
    if proof.plugin_id != id {
        return Err(SurfaceRefusal::ProofForOtherPlugin {
            plugin: id,
            proof_for: proof.plugin_id.clone(),
        });
    }
    if let Some(d) = &proof.divergence {
        return Err(SurfaceRefusal::BreaksOriginalMode {
            plugin: id,
            frame: d.frame,
            field: d.field.clone(),
        });
    }
    if proof.frames < MIN_PROOF_FRAMES {
        return Err(SurfaceRefusal::ProofTooShort {
            plugin: id,
            frames: proof.frames,
        });
    }
    Ok(PluginKind::Logic)
}

// ---- fact plugins ----------------------------------------------------------

/// A fact plugin: a recipe-format file of rows.
#[derive(Debug, Clone)]
pub struct FactPlugin {
    pub id: String,
    facts: Recipe,
}

impl FactPlugin {
    /// # Errors
    /// [`SurfaceError`] if the plugin is not a fact plugin or the file is not
    /// a valid recipe.
    pub fn load(plugin: &SurfaceManifest, facts_toml: &str) -> Result<Self, SurfaceError> {
        plugin.expect(PluginKind::Fact)?;
        let facts = rf_profiles::load_recipe_str(facts_toml)
            .map_err(|e| SurfaceError::Load(e.to_string()))?;
        Ok(Self {
            id: plugin.manifest.id.clone(),
            facts,
        })
    }

    /// Fold this plugin's rows into `base` ([`rf_rebuild::merge_facts`]).
    ///
    /// # Errors
    /// [`FactRefusal`] if the file carries patch rows.
    pub fn apply(&self, base: &Recipe) -> Result<FactMerge, FactRefusal> {
        merge_facts(base, &self.facts)
    }
}

// ---- render plugins --------------------------------------------------------

/// Labels a Lua script resolves with `rf.profile.addr(name)`, from a recipe:
/// each RAM row by name, and each object table field of each slot as
/// `"<table>.<field>.<slot>"` (fields `x_lo`, `x_hi`, `y`, `y_hi`, `kind`,
/// `state`; slot from 0).
#[must_use]
pub fn recipe_labels(recipe: &Recipe) -> BTreeMap<String, u32> {
    let mut labels = BTreeMap::new();
    for r in &recipe.ram {
        labels.insert(r.name.clone(), r.addr);
    }
    for t in &recipe.object_tables {
        let fields = [
            ("x_lo", t.x_lo),
            ("x_hi", t.x_hi),
            ("y", t.y),
            ("y_hi", t.y_hi),
            ("kind", t.kind),
            ("state", t.state),
        ];
        for (field, off) in fields {
            let Some(off) = off else { continue };
            // A bounded range, so the walk always ends (law 8).
            for slot in 0..t.slots {
                let addr = t.base + off + slot * t.stride;
                labels.insert(format!("{}.{field}.{slot}", t.name), addr);
            }
        }
    }
    labels
}

/// A bridge carrying `recipe`'s labels over `window`.
#[must_use]
pub fn recipe_bridge(recipe: &Recipe, window: MemoryWindow) -> Bridge {
    let bridge = Bridge::default();
    bridge.publish(window, recipe_labels(recipe));
    bridge
}

/// A render plugin: a Lua script that reads recipe facts and emits draw
/// commands. It holds no `write_memory`, no hooks and no input (its manifest
/// could not have asked), so its only output is [`OverlayCmd`] data.
pub struct RenderPlugin {
    pub id: String,
    host: ScriptHost,
}

impl RenderPlugin {
    /// # Errors
    /// [`SurfaceError`] if the plugin is not a render plugin or the script
    /// does not load.
    pub fn load(
        plugin: &SurfaceManifest,
        source: &str,
        recipe: &Recipe,
        window: MemoryWindow,
    ) -> Result<Self, SurfaceError> {
        plugin.expect(PluginKind::Render)?;
        let bridge = recipe_bridge(recipe, window);
        let host = ScriptHost::load_with_bridge(
            plugin.manifest.clone(),
            source,
            Budget::default(),
            bridge,
        )
        .map_err(|e| SurfaceError::Load(e.to_string()))?;
        Ok(Self {
            id: plugin.manifest.id.clone(),
            host,
        })
    }

    /// Give the script this frame's memory (labels stay as loaded).
    pub fn set_memory(&self, window: MemoryWindow) {
        *self.host.bridge().memory.borrow_mut() = window;
    }

    /// Run the script's `on_frame` and return what it drew.
    pub fn draw(&mut self, frame: u64) -> Vec<OverlayCmd> {
        self.host.on_frame(frame);
        self.host.bridge().take_overlay()
    }

    #[must_use]
    pub fn host(&self) -> &ScriptHost {
        &self.host
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str =
        "[plugin]\nid = \"p\"\nversion = \"0.1.0\"\napi = \"0.1\"\nlicense = \"MIT\"\n\
                        provenance = \"test\"\n";

    fn parse(kind: &str, extra: &str) -> Result<SurfaceManifest, SurfaceError> {
        SurfaceManifest::parse(&format!("{BASE}kind = \"{kind}\"\n{extra}"))
    }

    #[test]
    fn a_manifest_must_declare_a_known_kind() {
        assert_eq!(SurfaceManifest::parse(BASE), Err(SurfaceError::MissingKind));
        assert_eq!(
            parse("wizard", ""),
            Err(SurfaceError::UnknownKind("wizard".into()))
        );
        assert_eq!(parse("fact", "").unwrap().kind, PluginKind::Fact);
    }

    #[test]
    fn kinds_refuse_capabilities_that_exceed_them() {
        for (kind, cap) in [
            ("fact", "write_memory"),
            ("fact", "function_hooks"),
            ("fact", "draw_overlay"),
            ("render", "write_memory"),
            ("render", "function_hooks"),
            ("render", "input_bindings"),
        ] {
            let err = parse(kind, &format!("{cap} = true\n")).unwrap_err();
            assert!(
                matches!(err, SurfaceError::KindForbids { capability, .. } if capability == cap),
                "{kind} + {cap}: {err:?}"
            );
        }
        assert!(parse("render", "draw_overlay = true\nread_memory = true\n").is_ok());
        assert!(parse("logic", "function_hooks = true\nwrite_memory = true\n").is_ok());
    }

    fn logic() -> SurfaceManifest {
        parse("logic", "function_hooks = true\nwrite_memory = true\n").unwrap()
    }

    fn proof(frames: u32, divergence: Option<ProofDivergence>) -> OriginalModeProof {
        OriginalModeProof {
            plugin_id: "p".into(),
            frames,
            divergence,
        }
    }

    #[test]
    fn a_logic_plugin_needs_a_clean_proof_made_for_it() {
        let l = logic();
        assert!(matches!(
            enable(&l, None),
            Err(SurfaceRefusal::NoProof { .. })
        ));
        assert_eq!(enable(&l, Some(&proof(60, None))), Ok(PluginKind::Logic));
        assert!(matches!(
            enable(&l, Some(&proof(5, None))),
            Err(SurfaceRefusal::ProofTooShort { frames: 5, .. })
        ));
        let mut other = proof(60, None);
        other.plugin_id = "q".into();
        assert!(matches!(
            enable(&l, Some(&other)),
            Err(SurfaceRefusal::ProofForOtherPlugin { .. })
        ));
    }

    #[test]
    fn a_divergent_proof_blocks_with_the_frame_and_field() {
        let d = ProofDivergence {
            frame: 30,
            field: "wram".into(),
        };
        let err = enable(&logic(), Some(&proof(31, Some(d)))).unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("blocked") && text.contains("Original mode"),
            "{text}"
        );
        assert!(text.contains("frame 30") && text.contains("wram"), "{text}");
    }

    #[test]
    fn fact_and_render_plugins_enable_without_a_proof() {
        assert_eq!(
            enable(&parse("fact", "").unwrap(), None),
            Ok(PluginKind::Fact)
        );
        assert_eq!(
            enable(&parse("render", "draw_overlay = true\n").unwrap(), None),
            Ok(PluginKind::Render)
        );
    }

    #[test]
    fn labels_come_from_ram_rows_and_object_table_slots() {
        use rf_rebuild::{Grade, ObjectTable, RamMeaning};
        let r = Recipe {
            ram: vec![RamMeaning {
                addr: 0x75a,
                width: 1,
                name: "lives".into(),
                source: "t".into(),
                grade: Grade::Observed,
            }],
            object_tables: vec![ObjectTable {
                name: "e".into(),
                base: 0x100,
                slots: 3,
                stride: 2,
                y: Some(1),
                ..ObjectTable::default()
            }],
            ..Recipe::default()
        };
        let l = recipe_labels(&r);
        assert_eq!(l["lives"], 0x75a);
        assert_eq!(l["e.y.0"], 0x101);
        assert_eq!(l["e.y.2"], 0x105);
        assert!(!l.contains_key("e.y.3") && !l.contains_key("e.x_lo.0"));
    }
}
