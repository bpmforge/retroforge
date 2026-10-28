use super::*;
use rf_core_api::{PixelLayer, PpuPixel};

fn ids(rows: &[&str]) -> (u32, u32, Vec<Option<u32>>) {
    let cols = rows[0].len() as u32;
    let v = rows
        .iter()
        .flat_map(|r| r.chars())
        .map(|c| c.to_digit(10))
        .collect();
    (cols, rows.len() as u32, v)
}

fn input<'a>(cols: u32, rows: u32, ids: &'a [Option<u32>]) -> ScreenInput<'a> {
    ScreenInput {
        id: 7,
        origin: (-16, 32),
        cols,
        rows,
        cell_px: 16,
        ids,
        visited: None,
    }
}

const SOLID_1: [TileRule; 2] = [
    TileRule {
        first: 1,
        last: 1,
        class: RuleClass::Solid,
    },
    TileRule {
        first: 2,
        last: 2,
        class: RuleClass::OneWay,
    },
];

#[test]
fn facts_decide_solid_oneway_and_open() {
    let (c, r, v) = ids(&["012"]);
    let s = bake_screen(
        &input(c, r, &v),
        &Solidity {
            rules: &SOLID_1,
            painted: None,
        },
    )
    .unwrap();
    let kinds: Vec<Kind> = s.cells.iter().map(|c| c.kind).collect();
    assert_eq!(kinds, [Kind::Open, Kind::Solid, Kind::OneWay]);
    assert!(s.cells.iter().all(|c| c.source == Source::Facts));
    assert_eq!(s.cells[1].height_eighths, DEFAULT_SOLID_EIGHTHS);
    assert_eq!(s.cells[0].height_eighths, 0);
}

#[test]
fn painted_fills_in_where_there_are_no_facts() {
    let (c, r, v) = ids(&["0123"]);
    let painted = BTreeMap::from([(0, 0), (1, 16), (2, 4)]);
    let s = bake_screen(
        &input(c, r, &v),
        &Solidity {
            rules: &[],
            painted: Some(&painted),
        },
    )
    .unwrap();
    assert_eq!(s.cells[0].kind, Kind::Open);
    assert_eq!(s.cells[1].kind, Kind::Solid);
    assert_eq!(s.cells[1].height_eighths, 16);
    assert_eq!(s.cells[2].height_eighths, 4);
    // Id 3 was never painted: unknown, not open.
    assert_eq!(s.cells[3].kind, Kind::Unknown);
    assert!(s.cells[..3].iter().all(|c| c.source == Source::Painted));
}

#[test]
fn painted_height_sets_the_extrusion_of_a_fact_solid() {
    let (c, r, v) = ids(&["1"]);
    let painted = BTreeMap::from([(1, 24)]);
    let s = bake_screen(
        &input(c, r, &v),
        &Solidity {
            rules: &SOLID_1,
            painted: Some(&painted),
        },
    )
    .unwrap();
    assert_eq!(s.cells[0].height_eighths, 24);
    assert_eq!(s.cells[0].source, Source::Facts);
}

#[test]
fn unseen_and_unobserved_cells_stay_unknown() {
    let v = vec![Some(1), None, Some(1)];
    let visited = [true, true, false];
    let mut inp = input(3, 1, &v);
    inp.visited = Some(&visited);
    let s = bake_screen(
        &inp,
        &Solidity {
            rules: &SOLID_1,
            painted: None,
        },
    )
    .unwrap();
    let kinds: Vec<Kind> = s.cells.iter().map(|c| c.kind).collect();
    assert_eq!(kinds, [Kind::Solid, Kind::Unknown, Kind::Unknown]);
    // Only the one known body is extruded.
    assert_eq!(s.quads.len(), 4);
}

#[test]
fn a_lone_block_gets_four_quads() {
    let (c, r, v) = ids(&["000", "010", "000"]);
    let s = bake_screen(
        &input(c, r, &v),
        &Solidity {
            rules: &SOLID_1,
            painted: None,
        },
    )
    .unwrap();
    assert_eq!(s.quads.len(), 4);
    for q in &s.quads {
        assert_eq!((q.len, q.height_eighths), (1, 8));
    }
}

#[test]
fn a_floor_run_merges_into_one_top_quad_and_hides_inner_edges() {
    let (c, r, v) = ids(&["0000", "1111"]);
    let s = bake_screen(
        &input(c, r, &v),
        &Solidity {
            rules: &SOLID_1,
            painted: None,
        },
    )
    .unwrap();
    let top: Vec<_> = s.quads.iter().filter(|q| q.face == Face::Top).collect();
    assert_eq!(top.len(), 1);
    assert_eq!((top[0].col, top[0].row, top[0].len), (0, 1, 4));
    // Bottom row is on the grid edge: exposed. Left and right ends: one each.
    assert_eq!(s.quads.iter().filter(|q| q.face == Face::Bottom).count(), 1);
    assert_eq!(s.quads.iter().filter(|q| q.face == Face::Left).count(), 1);
    assert_eq!(s.quads.iter().filter(|q| q.face == Face::Right).count(), 1);
}

#[test]
fn a_pillar_merges_its_sides_vertically() {
    let (c, r, v) = ids(&["010", "010", "010"]);
    let s = bake_screen(
        &input(c, r, &v),
        &Solidity {
            rules: &SOLID_1,
            painted: None,
        },
    )
    .unwrap();
    let left: Vec<_> = s.quads.iter().filter(|q| q.face == Face::Left).collect();
    assert_eq!(left.len(), 1);
    assert_eq!((left[0].col, left[0].row, left[0].len), (1, 0, 3));
}

#[test]
fn different_heights_do_not_merge() {
    let (c, r, v) = ids(&["0000", "1212"]);
    let painted = BTreeMap::from([(1, 8), (2, 16)]);
    let rules = [TileRule {
        first: 1,
        last: 2,
        class: RuleClass::Solid,
    }];
    let s = bake_screen(
        &input(c, r, &v),
        &Solidity {
            rules: &rules,
            painted: Some(&painted),
        },
    )
    .unwrap();
    assert_eq!(s.quads.iter().filter(|q| q.face == Face::Top).count(), 4);
}

#[test]
fn inconsistent_inputs_are_refused() {
    let v = vec![Some(1); 5];
    let sol = Solidity::default();
    assert!(bake_screen(&input(2, 2, &v), &sol).is_none());
    let mut zero = input(5, 1, &v);
    zero.cell_px = 0;
    assert!(bake_screen(&zero, &sol).is_none());
    let short = [true; 2];
    let mut vis = input(5, 1, &v);
    vis.visited = Some(&short);
    assert!(bake_screen(&vis, &sol).is_none());
}

fn px() -> Option<PpuPixel> {
    Some(PpuPixel {
        palette_index: 1,
        layer: PixelLayer::Background(0),
        sprite_id: None,
        priority: 0,
    })
}

#[test]
fn visited_cells_follow_the_scout_canvas() {
    let mut canvas = Canvas::new();
    // One pixel at world (-1, 0) and one at (17, 17), 16 px cells.
    canvas.blit_row(-1, 0, &[px()]);
    canvas.blit_row(17, 17, &[px()]);
    let f = visited_cells(&canvas, 16).unwrap();
    let (origin, cols, rows, vis) = (f.origin, f.cols, f.rows, f.visited);
    assert_eq!(origin, (-16, 0));
    assert_eq!((cols, rows), (3, 2));
    let hits: Vec<usize> = vis
        .iter()
        .enumerate()
        .filter(|(_, v)| **v)
        .map(|(i, _)| i)
        .collect();
    // (-1,0) is cell (0,0); (17,17) is cell column 2 (world 16..32), row 1.
    assert_eq!(hits, [0, 5]);
    assert!(visited_cells(&Canvas::new(), 16).is_none());
    assert!(visited_cells(&canvas, 0).is_none());
}

fn sample_set() -> StageSet {
    let (c, r, v) = ids(&["0000", "0120", "1111"]);
    let s = bake_screen(
        &input(c, r, &v),
        &Solidity {
            rules: &SOLID_1,
            painted: None,
        },
    )
    .unwrap();
    let mut second = s.clone();
    second.id = 8;
    StageSet {
        screens: vec![s, second],
    }
}

#[test]
fn bytes_round_trip() {
    let set = sample_set();
    assert!(set.quad_count() > 0 && set.body_count() > 0);
    let back = StageSet::from_bytes(&set.to_bytes()).unwrap();
    assert_eq!(back, set);
    assert_eq!(
        StageSet::from_bytes(&StageSet::default().to_bytes()).unwrap(),
        StageSet::default()
    );
}

#[test]
fn damaged_bytes_are_refused_not_panicked_on() {
    let bytes = sample_set().to_bytes();
    for cut in [0, 3, 10, bytes.len() - 1] {
        assert!(StageSet::from_bytes(&bytes[..cut]).is_err());
    }
    let mut flipped = bytes.clone();
    flipped[bytes.len() / 2] ^= 0xFF;
    assert!(StageSet::from_bytes(&flipped).is_err());
    let mut magic = bytes;
    magic[0] = b'X';
    assert!(StageSet::from_bytes(&magic).is_err());
}

fn temp_cache(label: &str) -> (Cache, std::path::PathBuf) {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let root = std::env::temp_dir().join(format!(
        "rf-enhance-baked-{label}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    (Cache::open(&root, 64 << 20).unwrap(), root)
}

#[test]
fn cache_round_trip_is_per_rom_and_beside_a_scout_entry() {
    let (mut cache, root) = temp_cache("rt");
    let set = sample_set();
    let rom = "ab".repeat(32);
    assert!(load_stage_set(&mut cache, &rom).unwrap().is_none());
    // A scout scene entry for the same ROM lives in the same cache.
    cache
        .put(
            &crate::persistence::scene_cache_key(&rom, crate::scene_identity::SceneId(5)),
            b"scout",
        )
        .unwrap();
    let stored = save_stage_set(&mut cache, &rom, &set).unwrap();
    assert!(stored > 0);
    assert_eq!(load_stage_set(&mut cache, &rom).unwrap().unwrap(), set);
    assert!(load_stage_set(&mut cache, &"cd".repeat(32))
        .unwrap()
        .is_none());
    assert_eq!(cache.len(), 2);
    // Reopened from disk.
    drop(cache);
    let mut again = Cache::open(&root, 64 << 20).unwrap();
    assert_eq!(load_stage_set(&mut again, &rom).unwrap().unwrap(), set);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_corrupt_entry_reads_as_a_miss() {
    let (mut cache, root) = temp_cache("bad");
    let rom = "ef".repeat(32);
    cache.put(&stage_set_key(&rom), b"not a stage set").unwrap();
    assert!(load_stage_set(&mut cache, &rom).unwrap().is_none());
    let _ = std::fs::remove_dir_all(root);
}
