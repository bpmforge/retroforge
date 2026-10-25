//! The understand pass's rules namer and recipe export (ticket W36-09) over
//! hand-built profiles: each rule fires on its trigger and not without it,
//! uniqueness suffixes, human rows win, and the export carries source and
//! confidence.

use rf_rebuild::hw::flag;
use rf_rebuild::naming::{merge_name_rows, name_rows, propose_names};
use rf_rebuild::profile::{FunctionProfile, RegUse, Understanding, Vectors};
use rf_rebuild::{
    entry, Atlas, Function, HookPoints, HwClass, Platform, Recipe, RecipeFunction, Stats,
};

fn func(entry_cpu: u32, kinds: u8, tag: u64) -> Function {
    Function {
        entry_rom: entry_cpu & 0x3FFF,
        entry_cpu,
        entry_mode: 0,
        entry_kinds: kinds,
        blocks: vec![],
        insn_count: 4,
        byte_len: 8,
        byte_hash: 0x1000 + tag,
        shape_hash: 0x2000 + tag,
    }
}

fn atlas(fs: Vec<Function>) -> Atlas {
    Atlas {
        prg_len: 0x4000,
        mode_count: 1,
        blocks: vec![],
        functions: fs,
        indirect_sites: vec![],
        volatile: vec![],
        stats: Stats::default(),
    }
}

fn touch(p: &mut FunctionProfile, class: HwClass, reg: u16, reads: u64, writes: u64) {
    p.regs.insert(reg, (class, RegUse { reads, writes }));
}

fn busy() -> FunctionProfile {
    FunctionProfile {
        insns: 100,
        entries: 10,
        calls: 10,
        frames_active: 10,
        main_insns: 100,
        ..FunctionProfile::default()
    }
}

fn run(platform: Platform, profiles: Vec<FunctionProfile>) -> Understanding {
    Understanding {
        platform,
        frames: 10,
        functions: profiles,
        vectors: Vectors {
            reset: vec![0x8000],
            nmi: vec![0x8100],
            irq: vec![0x8200],
        },
        unattributed_insns: 0,
    }
}

fn name_of(ps: &[rf_rebuild::naming::Proposal], f: u32) -> Option<&str> {
    ps.iter().find(|p| p.function == f).map(|p| p.name.as_str())
}

#[test]
fn nes_rules_fire_on_their_triggers() {
    let mut reset = busy();
    reset.calls = 0;
    reset.entries = 1;
    let mut nmi = busy();
    nmi.nmi_entries = 10;
    let mut dma = busy();
    touch(&mut dma, HwClass::OamDma, 0x4014, 0, 10);
    let mut pad = busy();
    touch(&mut pad, HwClass::Controller, 0x4016, 80, 10);
    touch(&mut pad, HwClass::Controller, 0x4017, 80, 0);
    let mut vram = busy();
    touch(&mut vram, HwClass::Ppu, 0x2006, 0, 20);
    touch(&mut vram, HwClass::Ppu, 0x2007, 0, 320);
    vram.flags = flag::PPU_ADDR_NAMETABLE;
    let mut pal = busy();
    touch(&mut pal, HwClass::Ppu, 0x2006, 0, 20);
    touch(&mut pal, HwClass::Ppu, 0x2007, 0, 160);
    pal.flags = flag::PPU_ADDR_PALETTE;
    let mut snd = busy();
    for r in [0x4000, 0x4001, 0x4002, 0x4015] {
        touch(&mut snd, HwClass::Apu, r, 0, 10);
    }
    let mut map = busy();
    touch(&mut map, HwClass::Mapper, 0x8000, 0, 10);
    let mut vbl = busy();
    touch(&mut vbl, HwClass::Ppu, 0x2002, 500, 0);
    vbl.flags = flag::STATUS_VBLANK_SEEN;
    let mut spr0 = busy();
    touch(&mut spr0, HwClass::Ppu, 0x2002, 500, 0);
    spr0.flags = flag::STATUS_SPRITE0_SEEN;
    let mut plain = busy();
    touch(&mut plain, HwClass::Ppu, 0x2000, 0, 10);
    let mut once = busy();
    touch(&mut once, HwClass::Ppu, 0x2002, 10, 0);
    once.flags = flag::STATUS_VBLANK_SEEN;

    let fs = vec![
        func(0x8000, entry::INTERRUPT, 0),
        func(0x8100, entry::INTERRUPT, 1),
        func(0x8300, entry::CALL, 2),
        func(0x8400, entry::CALL, 3),
        func(0x8500, entry::CALL, 4),
        func(0x8600, entry::CALL, 5),
        func(0x8700, entry::CALL, 6),
        func(0x8800, entry::CALL, 7),
        func(0x8900, entry::CALL, 8),
        func(0x8A00, entry::CALL, 9),
        func(0x8B00, entry::CALL, 10),
    ];
    let a = atlas(fs);
    let u = run(
        Platform::Nes,
        vec![
            reset,
            nmi,
            dma,
            pad,
            vram,
            pal,
            snd,
            map,
            vbl,
            spr0,
            plain.clone(),
        ],
    );
    // `once` is in a separate run below (it would add an eleventh function).
    let ps = propose_names(&u, &a);
    let expect = [
        (0, "reset"),
        (1, "nmi"),
        (2, "oam_dma"),
        (3, "read_controllers"),
        (4, "vram_upload"),
        (5, "palette_upload"),
        (6, "sound_update"),
        (7, "mapper_bank_switch"),
        (8, "wait_for_vblank"),
        (9, "wait_for_sprite_zero"),
    ];
    for (f, n) in expect {
        assert_eq!(name_of(&ps, f), Some(n), "function {f}");
    }
    assert_eq!(name_of(&ps, 10), None, "PPU control writes name nothing");
    assert!(ps
        .iter()
        .all(|p| !p.evidence.is_empty() && p.source.starts_with("rule:")));
    let pad_p = ps.iter().find(|p| p.function == 3).unwrap();
    assert_eq!(pad_p.confidence, 92, "strobe raises it");

    // A single status read is not a polling loop.
    let a2 = atlas(vec![func(0x8300, entry::CALL, 2)]);
    let u2 = run(Platform::Nes, vec![once]);
    assert!(propose_names(&u2, &a2).is_empty());
}

#[test]
fn snes_rules_fire_on_their_triggers() {
    let mut dma_oam = busy();
    touch(&mut dma_oam, HwClass::Dma, 0x420B, 0, 10);
    dma_oam.flags = flag::DMA_TO_OAM;
    let mut dma_vram = busy();
    touch(&mut dma_vram, HwClass::Dma, 0x420B, 0, 10);
    dma_vram.flags = flag::DMA_TO_VRAM;
    let mut dma_cg = busy();
    touch(&mut dma_cg, HwClass::Dma, 0x420B, 0, 10);
    dma_cg.flags = flag::DMA_TO_CGRAM;
    let mut pad = busy();
    touch(&mut pad, HwClass::Controller, 0x4218, 20, 0);
    let mut snd = busy();
    touch(&mut snd, HwClass::Apu, 0x2140, 10, 30);
    let mut mul = busy();
    touch(&mut mul, HwClass::Math, 0x4202, 0, 10);
    touch(&mut mul, HwClass::Math, 0x4203, 0, 10);
    let mut hdma = busy();
    touch(&mut hdma, HwClass::Dma, 0x420C, 0, 10);
    let mut wait = busy();
    touch(&mut wait, HwClass::CpuStatus, 0x4212, 900, 0);
    wait.flags = flag::STATUS_VBLANK_SEEN;
    let fs: Vec<Function> = (0..8)
        .map(|i| func(0x8300 + i * 0x100, entry::CALL, u64::from(i)))
        .collect();
    let u = run(
        Platform::Snes,
        vec![dma_oam, dma_vram, dma_cg, pad, snd, mul, hdma, wait],
    );
    let ps = propose_names(&u, &atlas(fs));
    let names: Vec<_> = (0..8).map(|i| name_of(&ps, i)).collect();
    assert_eq!(
        names,
        [
            Some("oam_dma"),
            Some("vram_upload"),
            Some("palette_upload"),
            Some("read_controllers"),
            Some("sound_update"),
            Some("hw_multiply_divide"),
            Some("hdma_setup"),
            Some("wait_for_vblank")
        ]
    );
}

#[test]
fn duplicate_names_take_the_entry_address_and_stay_unique() {
    let mut a1 = busy();
    touch(&mut a1, HwClass::OamDma, 0x4014, 0, 1);
    let b1 = a1.clone();
    let mut other = busy();
    touch(&mut other, HwClass::Mapper, 0x8000, 0, 1);
    let at = atlas(vec![
        func(0x8300, entry::CALL, 1),
        func(0x8400, entry::CALL, 2),
        func(0x8500, entry::CALL, 3),
    ]);
    let ps = propose_names(&run(Platform::Nes, vec![a1, b1, other]), &at);
    assert_eq!(name_of(&ps, 0), Some("oam_dma_8300"));
    assert_eq!(name_of(&ps, 1), Some("oam_dma_8400"));
    assert_eq!(name_of(&ps, 2), Some("mapper_bank_switch"));
    let mut names: Vec<_> = ps.iter().map(|p| p.name.clone()).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 3);
}

#[test]
fn the_main_loop_is_the_busiest_per_frame_caller_nothing_else_claimed() {
    let mut spine = busy();
    for c in 2..6 {
        spine.callees.insert(c, 10);
    }
    let mut small = busy();
    small.callees.insert(2, 10);
    let mut sparse = busy();
    sparse.frames_active = 3;
    for c in 2..6 {
        sparse.callees.insert(c, 3);
    }
    let fs: Vec<Function> = (0..3)
        .map(|i| func(0x8300 + i * 0x100, entry::CALL, u64::from(i)))
        .collect();
    let ps = propose_names(&run(Platform::Nes, vec![small, spine, sparse]), &atlas(fs));
    assert_eq!(ps.len(), 1);
    assert_eq!(
        (ps[0].function, ps[0].name.as_str()),
        (1, "frame_main_loop")
    );
    assert_eq!(ps[0].confidence, 55);
}

#[test]
fn export_carries_source_and_confidence_and_human_names_win() {
    let mut dma = busy();
    touch(&mut dma, HwClass::OamDma, 0x4014, 0, 1);
    let mut map = busy();
    touch(&mut map, HwClass::Mapper, 0x8000, 0, 1);
    let at = atlas(vec![
        func(0x8300, entry::CALL, 1),
        func(0x8400, entry::CALL, 2),
    ]);
    let ps = propose_names(&run(Platform::Nes, vec![dma, map]), &at);

    // A person already named function 0 (matched by hashes); the recipe also
    // uses the name the namer would give function 1.
    let mut recipe = Recipe {
        id: "x".into(),
        functions: vec![
            RecipeFunction {
                name: "sprite_dma_by_hand".into(),
                shape_hash: 0x2001,
                exact_hash: Some(0x1001),
                address_hint: Some(0x8300),
                hooks: HookPoints::default(),
                source: "observed: by hand".into(),
                confidence: None,
            },
            RecipeFunction {
                name: "mapper_bank_switch".into(),
                shape_hash: 0xdead,
                exact_hash: None,
                address_hint: None,
                hooks: HookPoints::default(),
                source: "observed: other".into(),
                confidence: None,
            },
        ],
        ..Recipe::default()
    };
    let rows = name_rows(&at, &ps, &recipe);
    assert_eq!(rows.len(), 1, "function 0 is described by a person");
    let r = &rows[0];
    assert_eq!(r.name, "mapper_bank_switch_8400", "no name collision");
    assert_eq!(r.source, "rule:mapper_bank_switch");
    assert_eq!(r.confidence, Some(85));
    assert_eq!((r.exact_hash, r.address_hint), (Some(0x1002), Some(0x8400)));

    // Merging replaces earlier machine rows and leaves human ones.
    assert_eq!(merge_name_rows(&mut recipe, &at, &ps), 1);
    assert_eq!(merge_name_rows(&mut recipe, &at, &ps), 1);
    assert_eq!(recipe.functions.len(), 3);
    assert!(recipe.functions[0].source.starts_with("observed"));
}
