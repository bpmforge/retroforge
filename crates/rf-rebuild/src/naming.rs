//! The understand pass, naming half (ticket W36-09): propose a name for each
//! observed function from its [`FunctionProfile`] with generic hardware
//! rules, and turn accepted names into recipe name rows.
//!
//! Every rule is a fact about the console, not about a game: "a function that
//! writes the OAM DMA port is an OAM DMA routine" holds for any NES title
//! (nesdev "PPU OAM", `$4014`). Nothing here mentions a title, an address of
//! a title's code or a RAM location of a title; game knowledge enters only
//! through the recipe's own RAM names, which the AI facts carry.
//!
//! A proposal says which rule fired, how sure the rule is (0-100) and the
//! evidence in words. Names are unique within an atlas: when several
//! functions earn the same base name each gets the entry address as a suffix.
//! Names that humans already gave win: [`name_rows`] never renames a function
//! an existing recipe row already describes.

use std::collections::BTreeMap;

use crate::atlas::{entry, Atlas};
use crate::hw::{flag, HwClass, Platform};
use crate::profile::{FunctionProfile, Understanding};
use crate::recipe::{match_recipe, HookPoints, Recipe, RecipeFunction};

/// Confidence a rule gives itself when its trigger is a documented hardware
/// protocol (a vector, a DMA trigger) versus a shape of use.
const CERTAIN: u8 = 99;

/// One rule's documentation row (used by the research note and the tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleDoc {
    pub name: &'static str,
    pub platform: &'static str,
    pub trigger: &'static str,
    pub confidence: &'static str,
}

/// The rules, in tie-break order (earlier wins at equal confidence).
pub const RULES: &[RuleDoc] = &[
    RuleDoc {
        name: "reset",
        platform: "both",
        trigger: "function the reset vector points at, and it ran",
        confidence: "99",
    },
    RuleDoc {
        name: "nmi",
        platform: "both",
        trigger: "function entered by an interrupt sequence at an NMI vector",
        confidence: "99",
    },
    RuleDoc {
        name: "irq",
        platform: "both",
        trigger: "function entered by an interrupt sequence at an IRQ/BRK vector",
        confidence: "99",
    },
    RuleDoc {
        name: "oam_dma",
        platform: "NES",
        trigger: "writes $4014",
        confidence: "97",
    },
    RuleDoc {
        name: "oam_dma",
        platform: "SNES",
        trigger: "DMA channel set to OAM data ($43x1 = $04) and $420B written; or 16+ writes to $2104",
        confidence: "85 / 70",
    },
    RuleDoc {
        name: "read_controllers",
        platform: "NES",
        trigger: "8+ reads of $4016/$4017 and a strobe write to $4016 (80 without the strobe)",
        confidence: "92 / 80",
    },
    RuleDoc {
        name: "read_controllers",
        platform: "SNES",
        trigger: "2+ reads of $4218-$421F (auto-read), or 8+ reads of $4016/$4017",
        confidence: "85 / 80",
    },
    RuleDoc {
        name: "palette_upload",
        platform: "NES",
        trigger: "$2007 writes after $2006 was set to $3Fxx, no nametable address, at most 48 data writes per call",
        confidence: "85",
    },
    RuleDoc {
        name: "palette_upload",
        platform: "SNES",
        trigger: "2+ writes to $2122, or DMA channel set to CGRAM data ($43x1 = $22) with $420B written",
        confidence: "85",
    },
    RuleDoc {
        name: "vram_upload",
        platform: "NES",
        trigger: "16+ writes to $2007 with $2006 written, unless only $3Fxx was addressed",
        confidence: "88",
    },
    RuleDoc {
        name: "vram_upload",
        platform: "SNES",
        trigger: "16+ writes to $2118/$2119, or DMA channel set to VRAM data ($43x1 = $18/$19) with $420B written",
        confidence: "85",
    },
    RuleDoc {
        name: "sound_update",
        platform: "NES",
        trigger: "writes 3+ distinct APU registers ($4000-$4013, $4015, $4017); one or two: 65",
        confidence: "90 / 65",
    },
    RuleDoc {
        name: "sound_update",
        platform: "SNES",
        trigger: "2+ writes to the sound ports $2140-$2143 (one: 60)",
        confidence: "85 / 60",
    },
    RuleDoc {
        name: "mapper_bank_switch",
        platform: "NES",
        trigger: "writes to cartridge space ($8000-$FFFF or $4020-$5FFF)",
        confidence: "85",
    },
    RuleDoc {
        name: "wait_for_vblank",
        platform: "NES",
        trigger: "8+ reads of $2002 per call and a read with bit 7 set",
        confidence: "85",
    },
    RuleDoc {
        name: "wait_for_vblank",
        platform: "SNES",
        trigger: "8+ reads of $4210/$4212 per call and a read with bit 7 set",
        confidence: "85",
    },
    RuleDoc {
        name: "wait_for_sprite_zero",
        platform: "NES",
        trigger: "8+ reads of $2002 per call, bit 6 seen, bit 7 never",
        confidence: "75",
    },
    RuleDoc {
        name: "hw_multiply_divide",
        platform: "SNES",
        trigger: "2+ writes to $4202-$4206",
        confidence: "80",
    },
    RuleDoc {
        name: "hdma_setup",
        platform: "SNES",
        trigger: "writes $420C",
        confidence: "80",
    },
    RuleDoc {
        name: "scroll_update",
        platform: "both",
        trigger: "NES: 2+ writes to $2005 and none to $2007; SNES: 4+ writes to the BG scroll registers $210D-$2114",
        confidence: "70",
    },
    RuleDoc {
        name: "frame_main_loop",
        platform: "both",
        trigger: "runs in the main context in 90%+ of frames, at most twice per frame, calls 3+ distinct functions; the busiest such function",
        confidence: "55",
    },
];

/// A proposed name for one atlas function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    /// Index into [`Atlas::functions`].
    pub function: u32,
    /// The final, atlas-unique name.
    pub name: String,
    /// The name before uniqueness suffixes.
    pub base: String,
    /// Where the name came from: `rule:<rule>` or `ai:<model>`.
    pub source: String,
    /// 0-100.
    pub confidence: u8,
    /// What was observed, in words.
    pub evidence: Vec<String>,
    /// Other rules that also fired (lower confidence), for the report.
    pub also: Vec<String>,
}

#[derive(Debug, Clone)]
struct Candidate {
    base: &'static str,
    confidence: u8,
    evidence: String,
}

fn cand(base: &'static str, confidence: u8, evidence: String) -> Candidate {
    Candidate {
        base,
        confidence,
        evidence,
    }
}

/// How often per call, for evidence text.
fn per_call(p: &FunctionProfile, n: u64) -> f64 {
    n as f64 / p.entries.max(p.calls).max(1) as f64
}

fn rules_for(platform: Platform, p: &FunctionProfile) -> Vec<Candidate> {
    let mut out = Vec::new();
    match platform {
        Platform::Nes => nes_rules(p, &mut out),
        Platform::Snes => snes_rules(p, &mut out),
    }
    out
}

fn nes_rules(p: &FunctionProfile, out: &mut Vec<Candidate>) {
    let ppu_data = p.reg(0x2007).writes;
    let dma = p.reg(0x4014).writes;
    if dma >= 1 {
        out.push(cand(
            "oam_dma",
            97,
            format!("writes the OAM DMA port $4014 {dma} time(s)"),
        ));
    }
    let pad_reads = p.reg_range(0x4016, 0x4017).reads;
    let strobe = p.reg(0x4016).writes;
    if pad_reads >= 8 {
        let (c, extra) = if strobe >= 1 {
            (92, " and strobes $4016")
        } else {
            (80, "")
        };
        out.push(cand(
            "read_controllers",
            c,
            format!("{pad_reads} reads of the pad ports $4016/$4017{extra}"),
        ));
    }
    let addr_writes = p.reg(0x2006).writes;
    let palette_only =
        p.flags & flag::PPU_ADDR_PALETTE != 0 && p.flags & flag::PPU_ADDR_NAMETABLE == 0;
    if ppu_data >= 16 && addr_writes >= 1 && !palette_only {
        out.push(cand(
            "vram_upload",
            88,
            format!(
                "{ppu_data} writes to $2007 after {addr_writes} write(s) to $2006 \
                 ({:.1} data writes per call)",
                per_call(p, ppu_data)
            ),
        ));
    }
    if palette_only && ppu_data >= 1 && per_call(p, ppu_data) <= 48.0 {
        out.push(cand(
            "palette_upload",
            85,
            format!(
                "sets the PPU address into $3F00-$3FFF and writes $2007 \
                 {:.1} times per call",
                per_call(p, ppu_data)
            ),
        ));
    }
    let apu_regs = p
        .regs
        .iter()
        .filter(|(_, (c, u))| *c == HwClass::Apu && u.writes > 0)
        .count();
    if apu_regs >= 1 {
        let c = if apu_regs >= 3 { 90 } else { 65 };
        out.push(cand(
            "sound_update",
            c,
            format!("writes {apu_regs} distinct APU register(s)"),
        ));
    }
    let mapper = p.class_use(HwClass::Mapper).writes;
    if mapper >= 1 {
        out.push(cand(
            "mapper_bank_switch",
            85,
            format!("{mapper} write(s) to cartridge space (mapper registers)"),
        ));
    }
    let polls = p.reg(0x2002).reads;
    if per_call(p, polls) >= 8.0 {
        if p.flags & flag::STATUS_VBLANK_SEEN != 0 {
            out.push(cand(
                "wait_for_vblank",
                85,
                format!(
                    "polls $2002 {:.0} times per call and saw the vblank flag",
                    per_call(p, polls)
                ),
            ));
        } else if p.flags & flag::STATUS_SPRITE0_SEEN != 0 {
            out.push(cand(
                "wait_for_sprite_zero",
                75,
                format!(
                    "polls $2002 {:.0} times per call, saw the sprite 0 flag and never vblank",
                    per_call(p, polls)
                ),
            ));
        }
    }
    let scroll = p.reg(0x2005).writes;
    if scroll >= 2 && ppu_data == 0 {
        out.push(cand(
            "scroll_update",
            70,
            format!("{scroll} write(s) to $2005 and no $2007 data"),
        ));
    }
}

fn snes_rules(p: &FunctionProfile, out: &mut Vec<Candidate>) {
    let dma_go = p.reg(0x420B).writes;
    let oam_data = p.reg(0x2104).writes;
    if dma_go >= 1 && p.flags & flag::DMA_TO_OAM != 0 {
        out.push(cand(
            "oam_dma",
            85,
            format!("points a DMA channel at OAM data and writes $420B {dma_go} time(s)"),
        ));
    } else if oam_data >= 16 {
        out.push(cand(
            "oam_dma",
            70,
            format!("{oam_data} writes to the OAM data port $2104"),
        ));
    }
    let auto = p.reg_range(0x4218, 0x421F).reads;
    let manual = p.reg_range(0x4016, 0x4017).reads;
    if auto >= 2 {
        out.push(cand(
            "read_controllers",
            85,
            format!("{auto} read(s) of the auto-joypad results $4218-$421F"),
        ));
    } else if manual >= 8 {
        out.push(cand(
            "read_controllers",
            80,
            format!("{manual} reads of the manual pad ports $4016/$4017"),
        ));
    }
    let vram_data = p.reg_range(0x2118, 0x2119).writes;
    if vram_data >= 16 || (dma_go >= 1 && p.flags & flag::DMA_TO_VRAM != 0) {
        out.push(cand(
            "vram_upload",
            85,
            format!(
                "{vram_data} write(s) to VRAM data $2118/$2119{}",
                if p.flags & flag::DMA_TO_VRAM != 0 {
                    ", with a DMA channel pointed at VRAM"
                } else {
                    ""
                }
            ),
        ));
    }
    let cg = p.reg(0x2122).writes;
    if cg >= 2 || (dma_go >= 1 && p.flags & flag::DMA_TO_CGRAM != 0) {
        out.push(cand(
            "palette_upload",
            85,
            format!(
                "{cg} write(s) to CGRAM data $2122{}",
                if p.flags & flag::DMA_TO_CGRAM != 0 {
                    ", with a DMA channel pointed at CGRAM"
                } else {
                    ""
                }
            ),
        ));
    }
    let ports = p.reg_range(0x2140, 0x2143).writes;
    if ports >= 1 {
        let c = if ports >= 2 { 85 } else { 60 };
        out.push(cand(
            "sound_update",
            c,
            format!("{ports} write(s) to the sound CPU ports $2140-$2143"),
        ));
    }
    let math = p.reg_range(0x4202, 0x4206).writes;
    if math >= 2 {
        out.push(cand(
            "hw_multiply_divide",
            80,
            format!("{math} write(s) to the multiply/divide unit $4202-$4206"),
        ));
    }
    if p.reg(0x420C).writes >= 1 {
        out.push(cand(
            "hdma_setup",
            80,
            "writes the HDMA enable $420C".to_string(),
        ));
    }
    let polls = p.reg(0x4212).reads + p.reg(0x4210).reads;
    if per_call(p, polls) >= 8.0 && p.flags & flag::STATUS_VBLANK_SEEN != 0 {
        out.push(cand(
            "wait_for_vblank",
            85,
            format!(
                "polls $4210/$4212 {:.0} times per call and saw the vblank flag",
                per_call(p, polls)
            ),
        ));
    }
    let scroll = p.reg_range(0x210D, 0x2114).writes;
    if scroll >= 4 {
        out.push(cand(
            "scroll_update",
            70,
            format!("{scroll} write(s) to the BG scroll registers"),
        ));
    }
}

/// Propose names for the observed functions of an understand run.
#[must_use]
pub fn propose_names(u: &Understanding, atlas: &Atlas) -> Vec<Proposal> {
    let n = u.functions.len().min(atlas.functions.len());
    // function -> ranked candidates (best first).
    let mut ranked: Vec<Vec<Candidate>> = vec![Vec::new(); n];

    // Vector rules first: they are the strongest evidence there is.
    for (i, p) in u.functions.iter().enumerate().take(n) {
        let f = &atlas.functions[i];
        if f.entry_kinds & entry::INTERRUPT == 0 {
            continue;
        }
        let low = f.entry_cpu & 0xFFFF;
        if p.nmi_entries > 0 {
            ranked[i].push(cand(
                "nmi",
                CERTAIN,
                format!(
                    "entered {} time(s) by an interrupt sequence at the NMI vector",
                    p.nmi_entries
                ),
            ));
        } else if p.irq_entries > 0 {
            ranked[i].push(cand(
                "irq",
                CERTAIN,
                format!(
                    "entered {} time(s) by an interrupt sequence at an IRQ/BRK vector",
                    p.irq_entries
                ),
            ));
        } else if u.vectors.reset.contains(&low) && p.insns > 0 {
            ranked[i].push(cand(
                "reset",
                CERTAIN,
                "the reset vector points here and it ran at power-on".to_string(),
            ));
        }
    }
    for (i, p) in u.functions.iter().enumerate().take(n) {
        if p.is_empty() {
            continue;
        }
        let mut c = rules_for(u.platform, p);
        ranked[i].append(&mut c);
    }
    main_loop(u, &mut ranked);

    let rule_order = |name: &str| {
        RULES
            .iter()
            .position(|r| r.name == name)
            .unwrap_or(usize::MAX)
    };
    let mut chosen: Vec<(usize, Candidate, Vec<String>)> = Vec::new();
    for (i, mut cs) in ranked.into_iter().enumerate() {
        if cs.is_empty() {
            continue;
        }
        cs.sort_by(|a, b| {
            b.confidence
                .cmp(&a.confidence)
                .then(rule_order(a.base).cmp(&rule_order(b.base)))
        });
        let also: Vec<String> = cs.iter().skip(1).map(|c| c.base.to_string()).collect();
        chosen.push((i, cs.swap_remove(0), also));
    }

    // Uniqueness. A name that only one function earned stays bare. Where
    // several earned it, the interrupt and spine names go to the one that ran
    // most (the same code entered at another decode width is a different
    // function and keeps the suffix); every other duplicate takes its entry
    // address, then its decode mode, then its index, until all differ.
    let mut uses: BTreeMap<&'static str, Vec<usize>> = BTreeMap::new();
    for (k, (_, c, _)) in chosen.iter().enumerate() {
        uses.entry(c.base).or_default().push(k);
    }
    let singleton = |base: &str| matches!(base, "reset" | "nmi" | "irq" | "frame_main_loop");
    let mut bare: Vec<bool> = vec![false; chosen.len()];
    for (base, ks) in &uses {
        if ks.len() == 1 {
            bare[ks[0]] = true;
        } else if singleton(base) {
            let best = ks.iter().copied().max_by_key(|&k| {
                let i = chosen[k].0;
                (
                    chosen[k].1.confidence,
                    u.functions[i].entries.max(u.functions[i].calls),
                    std::cmp::Reverse(i),
                )
            });
            if let Some(k) = best {
                bare[k] = true;
            }
        }
    }
    let width = if u.platform == Platform::Snes { 6 } else { 4 };
    let mut names: Vec<String> = chosen
        .iter()
        .enumerate()
        .map(|(k, (i, c, _))| {
            if bare[k] {
                c.base.to_string()
            } else {
                format!("{}_{:0width$x}", c.base, atlas.functions[*i].entry_cpu)
            }
        })
        .collect();
    for pass in 0..2 {
        let mut count: BTreeMap<String, u32> = BTreeMap::new();
        for n in &names {
            *count.entry(n.clone()).or_default() += 1;
        }
        for (k, n) in names.iter_mut().enumerate() {
            if count[n.as_str()] > 1 && !bare[k] {
                let f = &atlas.functions[chosen[k].0];
                if pass == 0 {
                    *n = format!("{n}_m{}", f.entry_mode);
                } else {
                    *n = format!("{n}_{}", chosen[k].0);
                }
            }
        }
    }
    chosen
        .into_iter()
        .zip(names)
        .map(|((i, c, also), name)| Proposal {
            function: i as u32,
            name,
            base: c.base.to_string(),
            source: format!("rule:{}", c.base),
            confidence: c.confidence,
            evidence: vec![c.evidence],
            also,
        })
        .collect()
}

/// `frame_main_loop`: the one function that is the per-frame spine of the
/// main context. Only functions no other rule claimed are considered.
fn main_loop(u: &Understanding, ranked: &mut [Vec<Candidate>]) {
    let frames = u64::from(u.frames.max(1));
    let best = u
        .functions
        .iter()
        .enumerate()
        .filter(|(i, p)| {
            ranked.get(*i).is_some_and(Vec::is_empty)
                && p.frames_active as u64 * 10 >= frames * 9
                && p.main_insns * 10 >= p.insns * 9
                && p.callees.len() >= 3
                && p.entries.max(p.calls) <= 2 * frames
        })
        .max_by_key(|(i, p)| (p.callees.len(), p.insns, std::cmp::Reverse(*i)));
    if let Some((i, p)) = best {
        ranked[i].push(cand(
            "frame_main_loop",
            55,
            format!(
                "runs in the main context in {} of {} frames, {:.1} entries per frame, \
                 calls {} distinct functions",
                p.frames_active,
                u.frames,
                p.entries.max(p.calls) as f64 / frames as f64,
                p.callees.len()
            ),
        ));
    }
}

/// Counts by rule base name, for reports.
#[must_use]
pub fn count_by_rule(proposals: &[Proposal]) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for p in proposals {
        *out.entry(p.base.clone()).or_default() += 1;
    }
    out
}

/// Whether a recipe row's `source` marks it as a machine proposal (a rule or
/// a model) rather than a human fact.
#[must_use]
pub fn is_machine_source(source: &str) -> bool {
    source.starts_with("rule:") || source.starts_with("ai:")
}

/// Recipe name rows for `accepted` proposals, ready to append to `existing`.
///
/// - A function the recipe already describes (by shape/hash match) is
///   skipped: names people gave win over proposals. A previous machine row
///   for the same function is replaced by the caller (see
///   [`merge_name_rows`]).
/// - Each row resolves to exactly one function and no two rows to the same
///   one; functions a recipe cannot tell apart share the best name.
/// - A name the recipe already uses gets the entry address as a suffix.
/// - Every row carries the exact hash and the address hint, so a function
///   that shares its shape with another still resolves to itself.
#[must_use]
pub fn name_rows(atlas: &Atlas, accepted: &[Proposal], existing: &Recipe) -> Vec<RecipeFunction> {
    let report = match_recipe(atlas, &human_only(existing));
    let mut claimed: Vec<u32> = report.matched.iter().map(|m| m.function).collect();
    let mut taken: Vec<String> = existing.functions.iter().map(|f| f.name.clone()).collect();
    // Best proposals first, and a bare name before its suffixed siblings, so
    // when several functions are indistinguishable to a recipe (the same bytes
    // entered at two 65816 widths) the row carries the name that fits best.
    let mut order: Vec<&Proposal> = accepted.iter().collect();
    order.sort_by(|a, b| {
        b.confidence
            .cmp(&a.confidence)
            .then((a.name != a.base).cmp(&(b.name != b.base)))
            .then(a.function.cmp(&b.function))
    });
    let mut rows = Vec::new();
    for p in order {
        let Some(f) = atlas.functions.get(p.function as usize) else {
            continue;
        };
        let mut name = p.name.clone();
        if taken.contains(&name) {
            name = format!("{}_{:04x}", p.name, f.entry_cpu);
        }
        if taken.contains(&name) {
            continue;
        }
        let row = RecipeFunction {
            name: name.clone(),
            shape_hash: f.shape_hash,
            exact_hash: Some(f.byte_hash),
            address_hint: Some(f.entry_cpu),
            hooks: HookPoints {
                entry: true,
                exit: true,
            },
            source: p.source.clone(),
            confidence: Some(p.confidence),
        };
        // A row is only as good as what it resolves to. Same-byte functions
        // at one address resolve to a single one of them; claim that one.
        let probe = Recipe {
            functions: vec![row.clone()],
            ..Recipe::default()
        };
        let Some(resolved) = match_recipe(atlas, &probe)
            .matched
            .first()
            .map(|m| m.function)
        else {
            continue;
        };
        if claimed.contains(&resolved) {
            continue;
        }
        claimed.push(resolved);
        taken.push(name);
        rows.push(row);
    }
    rows
}

/// `r` without its machine-proposed rows: what people established.
#[must_use]
pub fn human_only(r: &Recipe) -> Recipe {
    Recipe {
        functions: r
            .functions
            .iter()
            .filter(|f| !is_machine_source(&f.source))
            .cloned()
            .collect(),
        ..r.clone()
    }
}

/// Add `accepted` to `recipe`: earlier machine name rows are dropped first
/// (a new run replaces them), human rows stay and win. Returns the number of
/// rows added.
pub fn merge_name_rows(recipe: &mut Recipe, atlas: &Atlas, accepted: &[Proposal]) -> usize {
    recipe.functions.retain(|f| !is_machine_source(&f.source));
    let rows = name_rows(atlas, accepted, recipe);
    let n = rows.len();
    recipe.functions.extend(rows);
    n
}
