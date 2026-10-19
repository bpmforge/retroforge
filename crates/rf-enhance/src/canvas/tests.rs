use super::*;
use rf_core_api::color::bgr555_to_rgb;
use rf_core_api::{CoreEvent, LineRegisters, PpuPixel};

/// A stand-in for the renderer's NES table (`rf-enhance` knows none): any
/// injective index -> colour map will do for these tests.
fn nes_rgb(i: u8) -> [u8; 3] {
    [i, i.wrapping_mul(3), 255 - i]
}

/// Test shorthand: place a frame into a known scene by SNES registers.
trait Feed {
    fn feed(
        &mut self,
        b: &FrameBundle,
        scene: SceneId,
        scrolls: Option<&[Option<LayerScroll>; LAYERS]>,
    );
    fn feed_with_palettes(
        &mut self,
        b: &FrameBundle,
        scene: SceneId,
        scrolls: Option<&[Option<LayerScroll>; LAYERS]>,
        rows: &[Option<&Cgram>],
    );
}

impl Feed for CanvasModel {
    fn feed(
        &mut self,
        b: &FrameBundle,
        scene: SceneId,
        scrolls: Option<&[Option<LayerScroll>; LAYERS]>,
    ) {
        self.observe_frame_in(b, scene, &FrameFeed::registers(scrolls));
    }

    fn feed_with_palettes(
        &mut self,
        b: &FrameBundle,
        scene: SceneId,
        scrolls: Option<&[Option<LayerScroll>; LAYERS]>,
        rows: &[Option<&Cgram>],
    ) {
        self.observe_frame_in(b, scene, &FrameFeed::registers(scrolls).with_palettes(rows));
    }
}

const W: u16 = 64;
const H: u16 = 4;

fn px(layer: PixelLayer, v: u8) -> PpuPixel {
    PpuPixel {
        palette_index: v,
        layer,
        sprite_id: None,
        priority: 0,
    }
}

/// Left half is BG1, right half BG2; each layer's pixel value encodes
/// its OWN world column (`hofs + c`), so a misplaced layer is visible.
fn bundle(hofs: [u16; 2], mutate: impl Fn(usize, usize, &mut PpuPixel)) -> FrameBundle {
    let mut video = Vec::new();
    for y in 0..usize::from(H) {
        for c in 0..usize::from(W) {
            let n = usize::from(c >= usize::from(W) / 2);
            let v = (hofs[n] as usize + c) as u8;
            let mut p = px(PixelLayer::Background(n as u8), v);
            mutate(y, c, &mut p);
            video.push(p);
        }
    }
    FrameBundle::new(0, W, H, video, Vec::<CoreEvent>::new())
}

fn scroll(hofs: u16) -> Option<LayerScroll> {
    Some(LayerScroll {
        hofs,
        vofs: 0,
        tilemap_width: 512,
        tilemap_height: 256,
    })
}

fn scrolls(h0: u16, h1: u16) -> [Option<LayerScroll>; LAYERS] {
    [scroll(h0), scroll(h1), None, None]
}

#[test]
fn two_layers_scrolling_at_different_rates_land_at_their_own_offsets() {
    let mut acc = CanvasModel::new();
    let id = SceneId(7);
    acc.feed(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(0, 0)));
    let first = acc.frames().unwrap();
    // BG1 moves 16, BG2 only 4 (hills move slower than the ground).
    acc.feed(&bundle([16, 4], |_, _, _| {}), id, Some(&scrolls(16, 4)));
    let second = acc.frames().unwrap();
    assert_eq!(second.scene, 7);
    let (o0, o1) = (first.origins, second.origins);
    assert_eq!(o0[0].unwrap().0 + 16, o1[0].unwrap().0);
    assert_eq!(o0[1].unwrap().0 + 4, o1[1].unwrap().0);
    let set = acc.current_canvases();
    let (bg1, bg2) = (set.canvases[0].as_ref().unwrap(), set.canvases[1].as_ref());
    // BG1 owns screen columns 0..32 only: world 0..48 after two frames.
    assert_eq!(bg1.get(47, 0).unwrap().palette_index, 47);
    assert_eq!(bg1.get(48, 0), None, "BG1 never drew past its half");
    // BG2 owns screen columns 32..64: world 32..68, value = its world x.
    let bg2 = bg2.unwrap();
    assert_eq!(bg2.get(67, 1).unwrap().palette_index, 67);
    assert_eq!(bg2.get(31, 0), None);
    // And no cross-talk: BG1's canvas holds no BG2 pixel.
    assert_eq!(bg1.get(50, 0), None);
}

#[test]
fn a_pixel_covered_by_a_sprite_does_not_erase_a_known_cell() {
    let mut acc = CanvasModel::new();
    let id = SceneId(1);
    acc.feed(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(0, 0)));
    let covered = |y: usize, c: usize, p: &mut PpuPixel| {
        if y == 0 && c == 10 {
            *p = px(PixelLayer::Sprite, 99);
        }
        if y == 1 && c == 11 {
            *p = px(PixelLayer::Background(2), 98); // another layer
        }
    };
    acc.feed(&bundle([0, 0], covered), id, Some(&scrolls(0, 0)));
    let set = acc.current_canvases();
    let bg1 = set.canvases[0].as_ref().unwrap();
    assert_eq!(bg1.get(10, 0).unwrap().palette_index, 10);
    assert_eq!(bg1.get(11, 1).unwrap().palette_index, 11);
}

#[test]
fn a_hole_stays_unknown_until_a_later_frame_reveals_it() {
    let mut acc = CanvasModel::new();
    let id = SceneId(1);
    let hole = |_: usize, c: usize, p: &mut PpuPixel| {
        if c == 5 {
            *p = px(PixelLayer::Sprite, 1);
        }
    };
    acc.feed(&bundle([0, 0], hole), id, Some(&scrolls(0, 0)));
    let before = acc.current_canvases();
    assert_eq!(before.canvases[0].as_ref().unwrap().get(5, 0), None);
    acc.feed(&bundle([1, 1], |_, _, _| {}), id, Some(&scrolls(1, 1)));
    let after = acc.current_canvases();
    assert_eq!(
        after.canvases[0]
            .as_ref()
            .unwrap()
            .get(5, 0)
            .map(|p| p.palette_index),
        Some(5)
    );
}

#[test]
fn a_layer_the_policy_keeps_narrow_is_not_stitched() {
    let mut acc = CanvasModel::new();
    // BG2 (the right half) is the HUD this time.
    let s = [scroll(0), None, None, None];
    acc.feed(&bundle([0, 0], |_, _, _| {}), SceneId(1), Some(&s));
    let set = acc.current_canvases();
    assert!(set.canvases[0].is_some());
    assert!(set.canvases[1].is_none(), "narrow layer has no canvas");
    assert_eq!(acc.frames().unwrap().origins[1], None);
}

#[test]
fn no_scroll_input_records_no_positions_and_keeps_the_canvases() {
    let mut acc = CanvasModel::new();
    let id = SceneId(1);
    acc.feed(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(0, 0)));
    acc.feed(&bundle([0, 0], |_, _, _| {}), id, None);
    assert_eq!(acc.frames(), None);
    assert!(acc.current_canvases().any());
}

#[test]
fn a_scroll_register_wrap_keeps_the_world_continuous() {
    let mut acc = CanvasModel::new();
    let id = SceneId(1);
    acc.feed(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(508, 508)));
    let a = acc.frames().unwrap().origins[0].unwrap().0;
    acc.feed(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(4, 4)));
    let b = acc.frames().unwrap().origins[0].unwrap().0;
    assert_eq!(b - a, 8, "511 -> 0 is a step of 8, not a jump back");
}

#[test]
fn scenes_keep_separate_canvases() {
    let mut acc = CanvasModel::new();
    acc.feed(
        &bundle([0, 0], |_, _, _| {}),
        SceneId(1),
        Some(&scrolls(0, 0)),
    );
    acc.feed(&bundle([0, 0], |_, _, _| {}), SceneId(2), None);
    assert!(
        !acc.current_canvases().any(),
        "scene 2 has stitched nothing"
    );
    acc.feed(&bundle([0, 0], |_, _, _| {}), SceneId(1), None);
    assert!(acc.current_canvases().any());
}

fn canvas_with(cells: &[(i64, i64, u8)]) -> Canvas {
    let mut c = Canvas::new();
    for &(x, y, v) in cells {
        c.blit_row(x, y, &[Some(px(PixelLayer::Background(0), v))]);
    }
    c
}

#[test]
fn the_margin_takes_the_front_most_layer_with_a_known_cell() {
    let layers = LayerCanvases {
        canvases: [
            Some(canvas_with(&[(100, 0, 11)])),
            Some(canvas_with(&[(100, 0, 22), (50, 0, 33)])),
            None,
            None,
        ],
        ..Default::default()
    };
    // BG1's world is offset by 100, BG2's by 0: dx -> own coordinates.
    let origins = [Some((100, 0)), Some((0, 0)), None, None];
    assert_eq!(
        margin_palette_index(&layers, &origins, 0, 0),
        11,
        "BG1 wins"
    );
    assert_eq!(
        margin_palette_index(&layers, &origins, 100, 0),
        22,
        "BG1 unknown there, BG2 shows"
    );
    assert_eq!(margin_palette_index(&layers, &origins, 50, 0), 33);
}

#[test]
fn an_unknown_margin_cell_falls_back_to_the_backdrop_colour() {
    let layers = LayerCanvases {
        canvases: [Some(canvas_with(&[(0, 0, 5)])), None, None, None],
        ..Default::default()
    };
    let origins = [Some((0, 0)), None, None, None];
    assert_eq!(front_layer_pixel(&layers, &origins, 40, 3, None), None);
    let idx = margin_palette_index(&layers, &origins, 40, 3);
    assert_eq!(idx, 0);
    let mut words = [0u16; 256];
    words[0] = 0b0_00000_11111_00000; // green backdrop
    let rgb = bgr555_to_rgb(words[usize::from(idx)], 15);
    assert!(
        rgb[1] > 200 && rgb[0] < 8 && rgb[2] < 8,
        "backdrop, not black: {rgb:?}"
    );
}

fn bw(r: u16, g: u16, b: u16) -> u16 {
    r | (g << 5) | (b << 10)
}

/// Hand values from fullsnes "SNES PPU Color-Math" / "2100h - INIDISP":
/// the SMW sky is a black backdrop plus a per-row fixed colour.
#[test]
fn the_backdrop_is_cgram_zero_through_the_rows_colour_math() {
    let sky = bw(5, 12, 20);
    let base = LineRegisters {
        math_enable: 0x20, // $2131 bit 5: math for the backdrop
        fixed_color: sky,
        backdrop_color: 0,
        brightness: 15,
        ..LineRegisters::default()
    };
    let rgb = |w: u16, b: u8| bgr555_to_rgb(w, b);
    // Black + fixed colour = the fixed colour (the HDMA sky gradient).
    assert_eq!(backdrop_color(&base), rgb(sky, 15));
    // Another row, another fixed colour: no vote, no memory.
    let next = LineRegisters {
        fixed_color: bw(6, 13, 21),
        ..base
    };
    assert_eq!(backdrop_color(&next), rgb(bw(6, 13, 21), 15));
    // A lit CGRAM 0 adds per channel and clamps at 31.
    let lit = LineRegisters {
        backdrop_color: bw(30, 0, 12),
        ..base
    };
    assert_eq!(backdrop_color(&lit), rgb(bw(31, 12, 31), 15));
    // Half ($2131 bit 6) with the fixed colour as the operand.
    let half = LineRegisters { half: true, ..lit };
    assert_eq!(backdrop_color(&half), rgb(bw(17, 6, 16), 15));
    // ...but a selected, transparent sub screen is not halved.
    let sub = LineRegisters {
        use_subscreen: true,
        ..half
    };
    assert_eq!(backdrop_color(&sub), rgb(bw(31, 12, 31), 15));
    // Subtract ($2131 bit 7) clamps at 0.
    let subtract = LineRegisters {
        subtract: true,
        ..lit
    };
    assert_eq!(backdrop_color(&subtract), rgb(bw(25, 0, 0), 15));
    // Math not enabled for the backdrop, or never ($2130 bits 4-5 = 3):
    // CGRAM 0 alone.
    let off = LineRegisters {
        math_enable: 0x1F,
        ..lit
    };
    assert_eq!(backdrop_color(&off), rgb(bw(30, 0, 12), 15));
    let never = LineRegisters {
        prevent_mode: 3,
        ..lit
    };
    assert_eq!(backdrop_color(&never), rgb(bw(30, 0, 12), 15));
    // Main forced black ($2130 bits 6-7 = 3): fixed colour alone, no half.
    let black = LineRegisters {
        clip_mode: 3,
        half: true,
        ..lit
    };
    assert_eq!(backdrop_color(&black), rgb(sky, 15));
    // Brightness scales last; 0 (force blank) is black.
    let dim = LineRegisters {
        brightness: 7,
        ..base
    };
    assert_eq!(backdrop_color(&dim), rgb(sky, 7));
    let blank = LineRegisters {
        brightness: 0,
        ..base
    };
    assert_eq!(backdrop_color(&blank), [0, 0, 0]);
}

#[test]
fn backdrop_rows_come_from_registers_else_from_the_pictures_backdrop_index() {
    let back = |i: u8| px(PixelLayer::Backdrop, i);
    let bg = px(PixelLayer::Background(0), 9);
    // Row 0 (registers): drawn bg only, still has an exact backdrop.
    // Row 1 (no registers, NES-like): mostly index 0x21, one 0x11.
    // Row 2: no backdrop pixel and no registers (takes row 1's).
    let video = vec![bg, bg, bg, back(0x21), back(0x21), back(0x11), bg, bg, bg];
    let mut b = FrameBundle::new(0, 3, 3, video, Vec::<CoreEvent>::new());
    let regs = LineRegisters {
        math_enable: 0x20,
        fixed_color: bw(1, 2, 3),
        brightness: 15,
        ..LineRegisters::default()
    };
    b.lines = vec![Some(regs), None, None];
    let rows = backdrop_rows(&b, &|_, i| nes_rgb(i));
    assert_eq!(rows[0], Some(bgr555_to_rgb(bw(1, 2, 3), 15)));
    assert_eq!(rows[1], Some(nes_rgb(0x21)));
    // Row 2 has nothing of its own: it takes the row above.
    assert_eq!(rows[2], rows[1]);
    // No evidence anywhere: nothing to say.
    let none = FrameBundle::new(0, 2, 1, vec![bg, bg], Vec::<CoreEvent>::new());
    assert_eq!(backdrop_rows(&none, &|_, i| nes_rgb(i)), vec![None]);
    let empty = FrameBundle::new(0, 0, 0, Vec::new(), Vec::<CoreEvent>::new());
    assert!(backdrop_rows(&empty, &|_, i| nes_rgb(i)).is_empty());
}

#[test]
fn a_row_with_too_few_backdrop_pixels_takes_its_neighbours_colour() {
    // Width 16: a row needs 2 backdrop pixels. Row 0 has 1 (thin), row 1
    // has 9 of 0x21, row 2 has 1 of 0x05 (thin), row 3 none.
    let back = |i: u8| px(PixelLayer::Backdrop, i);
    let bg = px(PixelLayer::Background(0), 9);
    let mut video = Vec::new();
    for (n, i) in [(1, 0x30), (9, 0x21), (1, 0x05), (0, 0)] {
        video.extend((0..16).map(|c| if c < n { back(i) } else { bg }));
    }
    let b = FrameBundle::new(0, 16, 4, video, Vec::<CoreEvent>::new());
    let want = Some(nes_rgb(0x21));
    assert_eq!(backdrop_rows(&b, &|_, i| nes_rgb(i)), vec![want; 4]);
}

fn pal_words(index: u8, w: u16) -> [u16; 256] {
    let mut words = [0u16; 256];
    words[usize::from(index)] = w;
    words
}

/// A cell stitched under one palette keeps that palette after CGRAM
/// changes (a per-area load, palette cycling): STITCHING_PLAN F4.
#[test]
fn a_remembered_cell_resolves_with_the_palette_it_was_seen_under() {
    let old = pal_words(9, bw(31, 0, 0));
    let new = pal_words(9, bw(0, 0, 31));
    // Frame 1: BG1 everywhere with palette index 9, CGRAM = `old`.
    let mut acc = CanvasModel::new();
    let frame = |hofs: u16| bundle([hofs, 0], |_, _, p| *p = px(PixelLayer::Background(0), 9));
    let b1 = frame(0);
    let scrolls = [scroll(0), None, None, None];
    let old_rows: Vec<Option<&Cgram>> = vec![Some(&old); usize::from(H)];
    acc.feed_with_palettes(&b1, SceneId(1), Some(&scrolls), &old_rows);
    // Frame 2: the game loaded a new palette; the camera moved on.
    let b2 = frame(0);
    let scrolls2 = [scroll(W), None, None, None];
    let new_rows: Vec<Option<&Cgram>> = vec![Some(&new); usize::from(H)];
    acc.feed_with_palettes(&b2, SceneId(1), Some(&scrolls2), &new_rows);
    let layers = acc.current_canvases();
    assert_eq!(layers.palettes.len(), 2, "two distinct CGRAM states");
    let frames = acc.frames().expect("placed");
    let regs = LineRegisters {
        tm: 0x01,
        brightness: 15,
        ..LineRegisters::default()
    };
    // The current row palette is `new`: a cell from frame 1 (world x 0,
    // before the origin of frame 2) must still come out RED.
    let cur = new;
    let (ox, _) = frames.origins[0].expect("origin");
    let dx_old = -ox + 3; // world x 3: frame 1 only
    let dx_new = 3; // frame 2's own cell
    let at = |dx: i64| margin_color(&layers, &frames, 0, dx, None, &regs, &cur);
    assert_eq!(at(dx_old), bgr555_to_rgb(bw(31, 0, 0), 15));
    assert_eq!(at(dx_new), bgr555_to_rgb(bw(0, 0, 31), 15));
}

/// A fade-in: the cell was remembered at brightness 4 and the picture is
/// at brightness 4 now, then 12: brightness is applied to both alike,
/// so the margin never lags the picture.
#[test]
fn a_fade_leaves_remembered_margin_colours_consistent_with_the_picture() {
    let words = pal_words(9, bw(20, 10, 30));
    let mut acc = CanvasModel::new();
    let b = bundle([0, 0], |_, _, p| *p = px(PixelLayer::Background(0), 9));
    let rows: Vec<Option<&Cgram>> = vec![Some(&words); usize::from(H)];
    acc.feed_with_palettes(&b, SceneId(1), Some(&[scroll(0), None, None, None]), &rows);
    let layers = acc.current_canvases();
    let frames = acc.frames().expect("placed");
    assert_eq!(
        layers.palettes.len(),
        1,
        "brightness is not part of the key"
    );
    for brightness in [4u8, 8, 15] {
        let regs = LineRegisters {
            tm: 0x01,
            brightness,
            ..LineRegisters::default()
        };
        let got = margin_color(&layers, &frames, 0, 5, None, &regs, &words);
        // The picture's own colour for that index at that brightness.
        assert_eq!(
            got,
            bgr555_to_rgb(words[9], brightness),
            "brightness {brightness}"
        );
    }
}

#[test]
fn rows_without_a_line_palette_write_cells_with_no_id() {
    let mut acc = CanvasModel::new();
    let b = bundle([0, 0], |_, _, p| *p = px(PixelLayer::Background(0), 9));
    acc.feed(&b, SceneId(1), Some(&[scroll(0), None, None, None]));
    let layers = acc.current_canvases();
    assert!(layers.palettes.is_empty());
    let cell = layers.canvases[0].as_ref().and_then(|c| c.get(5, 0));
    assert_eq!(cell.map(|c| c.sprite_id), Some(None));
}

#[test]
fn margins_compose_through_fill_margins() {
    let layers = LayerCanvases {
        canvases: [Some(canvas_with(&[(-1, 0, 7)])), None, None, None],
        ..Default::default()
    };
    let origins = [Some((0, 0)), None, None, None];
    let (h, out_w) = (1usize, 258usize);
    let native = vec![0xAAu8; 256 * h * 4];
    let world = |wx: i64, wy: i64| -> Option<[u8; 3]> {
        let i = margin_palette_index(&layers, &origins, wx, wy);
        Some(nes_rgb(i))
    };
    let out = crate::widescreen::fill_margins(&native, 256, h, out_w, (0, 0), &world, &|_| false);
    let left = nes_rgb(7);
    let backdrop = nes_rgb(0);
    assert_eq!(
        &out[0..3],
        &left[..],
        "left margin cell is BG1's known cell"
    );
    let last = (out_w - 1) * 4;
    assert_eq!(&out[last..last + 3], &backdrop[..], "right is backdrop");
}

// ---- W32-07: merging a scout's canvases into the live ones

fn row_of(c: &Canvas, y: i64, xs: std::ops::Range<i64>) -> Vec<Option<u8>> {
    xs.map(|x| c.get(x, y).map(|p| p.palette_index)).collect()
}

#[test]
fn a_merge_fills_only_unknown_cells_and_live_wins() {
    let mut live = canvas_with(&[(0, 0, 1), (1, 0, 1), (2, 0, 1), (3, 0, 1)]);
    let scout = canvas_with(&[(2, 0, 9), (3, 0, 9), (4, 0, 9), (5, 0, 9)]);
    let filled = merge_unknown(&mut live, &scout);
    assert_eq!(filled, 2, "only x = 4 and 5 were unknown");
    assert_eq!(
        row_of(&live, 0, 0..7),
        vec![Some(1), Some(1), Some(1), Some(1), Some(9), Some(9), None]
    );
    // Idempotent: a second merge adds nothing.
    assert_eq!(merge_unknown(&mut live, &scout), 0);
}

#[test]
fn a_merge_grows_the_live_canvas_to_the_scouts_cells() {
    let mut live = canvas_with(&[(0, 0, 1)]);
    let scout = canvas_with(&[(100, 3, 7)]);
    assert_eq!(merge_unknown(&mut live, &scout), 1);
    assert_eq!(live.get(100, 3).map(|p| p.palette_index), Some(7));
    assert_eq!(live.get(0, 0).map(|p| p.palette_index), Some(1));
}

fn two_scenes() -> (CanvasModel, [Canvas; LAYERS]) {
    let mut live = CanvasModel::new();
    live.feed(
        &bundle([0, 0], |_, _, _| {}),
        SceneId(1),
        Some(&scrolls(0, 0)),
    );
    let scout = [
        canvas_with(&[(300, 0, 5)]),
        Canvas::new(),
        Canvas::new(),
        Canvas::new(),
    ];
    (live, scout)
}

#[test]
fn a_scout_only_fills_the_scene_it_was_started_for() {
    let (mut live, scout) = two_scenes();
    assert_eq!(
        live.merge_unknown_from(2, &scout),
        0,
        "no canvas for scene 2"
    );
    assert_eq!(live.scene_canvases(1).unwrap()[0].get(300, 0), None);
    assert_eq!(live.scene_merges(1), 0);
    assert_eq!(live.merge_unknown_from(1, &scout), 1);
    assert_eq!(live.scene_merges(1), 1, "a merge is counted (W34-07)");
    assert_eq!(live.scene_merges(2), 0);
    let got = live.scene_canvases(1).unwrap()[0].get(300, 0);
    assert_eq!(got.map(|p| p.palette_index), Some(5));
}

#[test]
fn a_dirty_box_limits_what_a_merge_looks_at() {
    let (mut live, _) = two_scenes();
    let scout = [
        canvas_with(&[(300, 0, 5), (900, 0, 6)]),
        Canvas::new(),
        Canvas::new(),
        Canvas::new(),
    ];
    let within = [Some((250, 0, 400, 1)), None, None, None];
    assert_eq!(live.merge_unknown_within(1, &scout, &within), 1);
    let bg1 = &live.scene_canvases(1).unwrap()[0];
    assert!(bg1.get(300, 0).is_some());
    assert_eq!(
        bg1.get(900, 0),
        None,
        "outside the box: left for the next merge"
    );
}

#[test]
fn dirty_boxes_follow_what_was_stitched_and_clear_when_taken() {
    let mut acc = CanvasModel::new();
    acc.feed(
        &bundle([0, 0], |_, _, _| {}),
        SceneId(1),
        Some(&scrolls(0, 0)),
    );
    acc.feed(
        &bundle([16, 4], |_, _, _| {}),
        SceneId(1),
        Some(&scrolls(16, 4)),
    );
    let d = acc.take_dirty();
    // Each frame blits a full 64-wide row span (the cells the layer did
    // not draw are skipped inside it): world x 0..64 and 16..80.
    assert_eq!(d[0], Some((0, 0, 80, i64::from(H))));
    assert_eq!(d[2], None, "layer 3 was never stitched");
    assert_eq!(acc.take_dirty(), [None; LAYERS]);
}

#[test]
fn a_forked_accumulator_continues_the_live_world_axes() {
    let mut live = CanvasModel::new();
    let id = SceneId(1);
    live.feed(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(500, 0)));
    live.feed(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(508, 0)));
    let live_x = live.frames().unwrap().origins[0].unwrap().0;
    let mut scout = live.fork_for_scout();
    // The raw register wrapped (508 -> 4): the world goes on to 516.
    scout.feed(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(4, 0)));
    assert_eq!(scout.frames().unwrap().origins[0].unwrap().0, live_x + 8);
    assert!(
        live.current_canvases().canvases[0].is_some(),
        "live untouched"
    );
    // Rewinding puts the scout back at the live position.
    let seed = live.fork_for_scout();
    scout.rewind_to(&seed);
    scout.feed(
        &bundle([0, 0], |_, _, _| {}),
        id,
        Some(&scrolls(516 % 512, 0)),
    );
    assert_eq!(scout.frames().unwrap().origins[0].unwrap().0, live_x + 8);
}

// ---- W32-08: shared scroll timing, parallax layers, HUD, layer order

#[test]
fn the_lag_hands_a_frame_the_read_taken_after_the_one_before_it() {
    let read = |x: u16| FrameScrolls {
        scene: Some((x, 0)),
        layers: Some(scrolls(x, x)),
    };
    let mut lag = ScrollLag::default();
    // Nothing known before any read: the frame is not placed.
    assert_eq!(lag.drawn_with(1, read(10)), FrameScrolls::default());
    // Frame 2 was drawn with what was read after frame 1.
    assert_eq!(lag.drawn_with(2, read(20)), read(10));
    // A gap (frame 5 after frame 2): unknown again, then consecutive.
    assert_eq!(lag.drawn_with(5, read(50)), FrameScrolls::default());
    assert_eq!(lag.drawn_with(6, read(60)), read(50));
    // A primed read (a scout pass start) serves the next frame.
    lag.prime(100, read(7));
    assert_eq!(lag.drawn_with(101, read(8)), read(7));
    lag.forget();
    assert_eq!(lag.drawn_with(102, read(9)), FrameScrolls::default());
}

/// Picture whose BG1 pixel at screen column `c` holds `hofs + c`, i.e.
/// its own world x, so any misplacement shows in the stitched cells.
fn world_x_bundle(hofs: u16) -> FrameBundle {
    let mut video = Vec::new();
    for _ in 0..usize::from(H) {
        for c in 0..usize::from(W) {
            video.push(px(PixelLayer::Background(0), (usize::from(hofs) + c) as u8));
        }
    }
    let events = vec![
        rf_core_api::CoreEvent::Scanline(0),
        rf_core_api::CoreEvent::Scanline(H - 1),
    ];
    FrameBundle::new(0, W, H, video, events)
}

/// The set of `world x - value` over every known cell of a canvas: one
/// element means every cell sits at the world x its content encodes.
fn placement_offsets(c: &Canvas) -> std::collections::BTreeSet<i64> {
    let (ox, oy) = c.origin();
    let mut out = std::collections::BTreeSet::new();
    for y in 0..c.height() as i64 {
        for x in 0..c.width() as i64 {
            if let Some(p) = c.get(ox + x, oy + y) {
                out.insert(ox + x - i64::from(p.palette_index));
            }
        }
    }
    out
}

/// The same content placed through both consoles' feeds. The SNES reads the
/// scroll registers (one frame late, through the one `ScrollLag`); the NES is
/// placed by the bands of the core's own scroll events. Either way the cells
/// land through the one `place_rows`, at the world x their content encodes.
#[test]
fn the_nes_and_snes_feeds_place_the_same_content_at_the_same_world_x() {
    // A camera with uneven steps, so a one-frame error cannot hide in a
    // constant offset. Frame k is drawn at DRAW[k]; the registers read
    // after frame k hold DRAW[k + 1] (they draw the next frame).
    const DRAW: [u16; 9] = [10, 13, 18, 20, 29, 31, 40, 41, 52];
    let read = |x: u16| FrameScrolls {
        scene: Some((x, 0)),
        layers: Some([scroll(x), None, None, None]),
    };
    let mut snes = CanvasModel::new();
    let mut nes = CanvasModel::new();
    let mut lag = ScrollLag::default();
    lag.prime(0, read(DRAW[1]));
    for (k, &draw) in DRAW.iter().enumerate().skip(1) {
        let bundle = world_x_bundle(draw);
        let drawn = lag.drawn_with(k as u64, read(DRAW.get(k + 1).copied().unwrap_or(0)));
        snes.feed(&bundle, SceneId(1), drawn.layers.as_ref());
        // The NES core states the scroll in its own events.
        let mut events = vec![CoreEvent::ScrollWrite {
            x: draw,
            y: 0,
            layer: PixelLayer::Background(0),
        }];
        events.extend(bundle.events.iter().cloned());
        let nes_bundle = FrameBundle::new(0, W, H, bundle.video.clone(), events);
        nes.observe_frame(&nes_bundle, &FrameFeed::bands());
    }
    let (s, n) = (snes.current_canvases(), nes.current_canvases());
    let s = s.canvases[0].as_ref().expect("SNES layer 0");
    let n = n.canvases[0].as_ref().expect("NES layer 0");
    let (a, b) = (placement_offsets(s), placement_offsets(n));
    assert_eq!(a.len(), 1, "SNES: every cell at its world x: {a:?}");
    assert_eq!(b.len(), 1, "NES: every cell at its world x: {b:?}");
    assert_eq!(a, b, "both consoles use the same world x");
}

#[test]
fn a_layer_that_never_moves_while_another_does_is_hud_only_when_it_is_a_band() {
    let mut acc = CanvasModel::new();
    let id = SceneId(1);
    // BG1 scrolls; BG2 (right half) is full height at a fixed scroll;
    // BG3 is a one-row band at a fixed scroll.
    let band = |y: usize, _c: usize, p: &mut PpuPixel| {
        if y == 0 {
            *p = px(PixelLayer::Background(2), 9);
        }
    };
    let s = |h: u16| [scroll(h), scroll(4), scroll(0), None];
    acc.feed(&bundle([0, 0], band), id, Some(&s(0)));
    assert_eq!(acc.hud_layers(1), [false; LAYERS], "nothing has moved yet");
    for h in [8u16, 16, 24] {
        acc.feed(&bundle([h, 0], band), id, Some(&s(h)));
    }
    assert_eq!(
        acc.hud_layers(1),
        [false, false, true, false],
        "the band is HUD; the full-height static layer is not"
    );
    assert!(acc.current_canvases().hud[2]);
    assert!(
        acc.current_canvases().canvases[2].is_some(),
        "the HUD layer keeps its canvas"
    );
}

#[test]
fn a_hud_layer_gives_the_margins_nothing_and_a_parallax_layer_does() {
    let layers = LayerCanvases {
        canvases: [
            None,
            Some(canvas_with(&[(5, 0, 22)])),
            Some(canvas_with(&[(5, 0, 33)])),
            None,
        ],
        hud: [false, false, true, false],
        ..Default::default()
    };
    let o = Some((0, 0));
    let origins = [None, o, o, None];
    // BG2 (hills) shows although the policy never widened it; BG3 is HUD.
    assert_eq!(
        front_layer_pixel(&layers, &origins, 5, 0, None).map(|p| p.palette_index),
        Some(22)
    );
    let hud_first = [2u8, 1, 0, 3];
    assert_eq!(
        front_layer_pixel(&layers, &origins, 5, 0, Some(&hud_first)).map(|p| p.palette_index),
        Some(22),
        "even when the order puts the HUD layer in front"
    );
}

#[test]
fn the_picture_decides_which_layer_is_in_front_row_by_row() {
    let mut acc = CanvasModel::new();
    let id = SceneId(1);
    let s = scrolls(0, 0);
    // Frame 1: BG1 left, BG2 right (each layer's cells become known).
    acc.feed(&bundle([0, 0], |_, _, _| {}), id, Some(&s));
    // Frame 2: BG2 shows EVERYWHERE, also where BG1 is known: BG2 is in
    // front of BG1 on every row.
    let all_bg2 = |_: usize, c: usize, p: &mut PpuPixel| {
        *p = px(PixelLayer::Background(1), c as u8);
    };
    let b2 = bundle([0, 0], all_bg2);
    acc.feed(&b2, id, Some(&s));
    let orders = acc.row_orders(&b2);
    assert_eq!(orders.len(), usize::from(H));
    assert!(orders.iter().all(|o| o[..2] == [1, 0]), "{orders:?}");
    // No evidence (a single known layer): the default order.
    let mut fresh = CanvasModel::new();
    let b1 = bundle([0, 0], |_, _, p| *p = px(PixelLayer::Background(0), 1));
    fresh.feed(&b1, id, Some(&s));
    assert!(fresh.row_orders(&b1).iter().all(|o| *o == [0, 1, 2, 3]));
}

#[test]
fn a_delta_carries_only_what_was_stitched_since_the_last_one() {
    let mut acc = CanvasModel::new();
    let id = SceneId(1);
    acc.feed(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(0, 0)));
    let first = acc.take_delta(1).expect("scene has canvases");
    assert!(first[0].get(0, 0).is_some());
    acc.feed(&bundle([100, 0], |_, _, _| {}), id, Some(&scrolls(100, 0)));
    let second = acc.take_delta(1).unwrap();
    assert!(second[0].get(0, 0).is_none(), "already delivered");
    assert!(second[0].get(100, 0).is_some());
    // Merging the deltas into a live accumulator rebuilds the canvases.
    let mut live = CanvasModel::new();
    live.feed(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(0, 0)));
    assert_eq!(live.merge_unknown_from(1, &first), 0, "live already knows");
    assert!(live.merge_unknown_from(1, &second) > 0);
    assert!(live.scene_canvases(1).unwrap()[0].get(100, 0).is_some());
    assert!(acc.take_delta(2).is_none(), "no such scene");
}

// ---- W33-01: per-line scroll, sub-screen pixels, margin colour math

/// BG1 left half, BG2 right half; each pixel's value is its own world x
/// (`row_hofs[y][layer] + c`), with the registers each line was drawn
/// with attached as line data.
fn line_bundle(row_hofs: [[u16; 2]; H as usize]) -> FrameBundle {
    let mut video = Vec::new();
    for hofs in &row_hofs {
        for c in 0..usize::from(W) {
            let n = usize::from(c >= usize::from(W) / 2);
            video.push(px(
                PixelLayer::Background(n as u8),
                (usize::from(hofs[n]) + c) as u8,
            ));
        }
    }
    let mut b = FrameBundle::new(0, W, H, video, Vec::new());
    b.lines = row_hofs
        .iter()
        .map(|h| {
            Some(LineRegisters {
                hofs: [h[0], h[1], 0, 0],
                ..LineRegisters::default()
            })
        })
        .collect();
    b
}

/// Per canvas row, the set of `world x - value` of its known cells.
fn row_offsets(c: &Canvas, y: i64) -> std::collections::BTreeSet<i64> {
    let (ox, _) = c.origin();
    (0..c.width() as i64)
        .filter_map(|x| {
            c.get(ox + x, y)
                .map(|p| ox + x - i64::from(p.palette_index))
        })
        .collect()
}

#[test]
fn a_layer_with_a_different_scroll_on_each_line_lands_each_row_at_its_own_world_x() {
    let mut acc = CanvasModel::new();
    let id = SceneId(1);
    // BG2 has an HDMA band: rows 1 and 3 are drawn 8 px further along.
    // The per-frame registers say 0 (the value at the start of the
    // frame); the lines say what really drew them.
    let f1 = line_bundle([[0, 0], [0, 8], [0, 0], [0, 8]]);
    acc.feed(&f1, id, Some(&scrolls(0, 0)));
    let first = acc.frames().unwrap();
    assert_eq!(
        first.origin_at(1, 1).unwrap().0 - first.origin_at(1, 0).unwrap().0,
        8,
        "row 1 of BG2 sits 8 px right of row 0"
    );
    assert_eq!(first.origin_at(0, 1), first.origin_at(0, 0), "BG1 is flat");
    // The next frame everything moved on: BG1 by 16, BG2's rows by 4.
    let f2 = line_bundle([[16, 4], [16, 12], [16, 4], [16, 12]]);
    acc.feed(&f2, id, Some(&scrolls(16, 4)));
    let set = acc.current_canvases();
    let bg2 = set.canvases[1].as_ref().unwrap();
    for y in 0..i64::from(H) {
        assert_eq!(
            row_offsets(bg2, y),
            [0].into(),
            "BG2 row {y}: every cell at the world x its content encodes"
        );
    }
    let bg1 = set.canvases[0].as_ref().unwrap();
    assert!((0..i64::from(H)).all(|y| row_offsets(bg1, y) == [0].into()));
}

#[test]
fn without_line_registers_the_per_frame_scroll_is_used_as_before() {
    let mut acc = CanvasModel::new();
    let mut b = line_bundle([[0, 0]; H as usize]);
    b.lines.clear();
    acc.feed(&b, SceneId(1), Some(&scrolls(0, 0)));
    assert!(acc.frames().unwrap().row_dx.is_empty());
    assert_eq!(acc.frames().unwrap().origin_at(1, 3).unwrap().0, 0);
}

fn sub_px(layer: PixelLayer, index: u8) -> rf_core_api::SubPixel {
    rf_core_api::SubPixel {
        palette_index: index,
        layer,
        op: rf_core_api::ColorMathOp::AddHalf,
        fixed: false,
        main_black: false,
    }
}

#[test]
fn a_sub_screen_layer_fills_its_own_canvas_where_the_main_screen_showed_another() {
    let mut acc = CanvasModel::new();
    // The whole picture is BG1 (value = column); BG2 is on the sub
    // screen only. Row 1's sub line: BG2 (palette 77) on columns 10..20,
    // BG1 (palette 55) on column 30, nothing (backdrop) elsewhere.
    let mut b = bundle([0, 0], |_, _, p| {
        *p = px(PixelLayer::Background(0), 5);
    });
    b.video[usize::from(W) + 30].palette_index = 30;
    b.lines = vec![
        Some(LineRegisters {
            hofs: [0, 100, 0, 0],
            ..LineRegisters::default()
        });
        usize::from(H)
    ];
    let line: Vec<_> = (0..usize::from(W))
        .map(|c| match c {
            10..=19 => sub_px(PixelLayer::Background(1), 77),
            30 => sub_px(PixelLayer::Background(0), 55),
            _ => sub_px(PixelLayer::Backdrop, 0),
        })
        .collect();
    b.sub = vec![
        None,
        Some(rf_core_api::SubLine {
            pixels: line,
            fixed_color: 0,
        }),
        None,
        None,
    ];
    acc.feed(&b, SceneId(1), Some(&scrolls(0, 100)));
    let set = acc.current_canvases();
    let bg2 = set.canvases[1].as_ref().expect("BG2 got cells");
    for c in 0..usize::from(W) {
        let got = bg2.get(100 + c as i64, 1).map(|p| p.palette_index);
        assert_eq!(got, (10..20).contains(&c).then_some(77), "column {c}");
    }
    assert_eq!(bg2.get(110, 0), None, "row 0 had no sub-screen line");
    // The main winner is written first and never overwritten by the
    // sub screen's pixel of the SAME layer, which is the same dot.
    let bg1 = set.canvases[0].as_ref().unwrap();
    assert_eq!(bg1.get(30, 1).map(|p| p.palette_index), Some(30));
    assert!(
        (0..i64::from(W)).all(|x| bg1.get(x, 1).is_none_or(|p| p.palette_index != 77)),
        "BG1 holds no BG2 pixel"
    );
}

fn word(r: u16, g: u16, b: u16) -> u16 {
    r | (g << 5) | (b << 10)
}

fn compose_fixture() -> (LayerCanvases, LayerFrames, Cgram) {
    let mut words = [0u16; 256];
    words[9] = word(20, 0, 0); // a hill colour
    words[3] = word(1, 2, 3); // a ground colour
    let layers = LayerCanvases {
        canvases: [
            Some(canvas_with(&[(7, 0, 3)])),
            Some(canvas_with(&[(5, 0, 9)])),
            None,
            None,
        ],
        ..Default::default()
    };
    let frames = LayerFrames {
        scene: 1,
        origins: [Some((0, 0)), Some((0, 0)), None, None],
        row_dx: Vec::new(),
        confidence: Default::default(),
    };
    (layers, frames, words)
}

fn regs() -> LineRegisters {
    LineRegisters {
        tm: 0x01,
        ts: 0x02,
        math_enable: 0x20,
        half: true,
        use_subscreen: true,
        fixed_color: word(4, 4, 4),
        backdrop_color: word(10, 10, 10),
        brightness: 15,
        ..LineRegisters::default()
    }
}

fn rgb(w: u16) -> [u8; 3] {
    bgr555_to_rgb(w, 15)
}

#[test]
fn the_margin_shows_a_sub_screen_layer_behind_the_backdrop_through_colour_math() {
    let (layers, frames, pal) = compose_fixture();
    let at = |dx: i64, r: &LineRegisters| margin_color(&layers, &frames, 0, dx, None, r, &pal);
    // Nothing on the main screen at x = 5, BG2's hill on the sub screen:
    // (backdrop + hill) / 2, per channel (fullsnes SNES PPU Color-Math).
    assert_eq!(at(5, &regs()), rgb(word(15, 5, 5)));
    // Hill unknown at x = 6: the sub source is the fixed colour, and the
    // half is NOT applied to a transparent sub screen: backdrop + fixed.
    assert_eq!(at(6, &regs()), rgb(word(14, 14, 14)));
    // The sub screen not selected ($2130 bit 1 clear): fixed colour, and
    // now the half applies.
    let fixed_only = LineRegisters {
        use_subscreen: false,
        ..regs()
    };
    assert_eq!(at(5, &fixed_only), rgb(word(7, 7, 7)));
    // Subtract: (backdrop - hill), clamped at 0, halved.
    let sub = LineRegisters {
        subtract: true,
        ..regs()
    };
    assert_eq!(at(5, &sub), rgb(word(0, 5, 5)));
    // Math not enabled for the backdrop: the backdrop alone.
    let off = LineRegisters {
        math_enable: 0,
        ..regs()
    };
    assert_eq!(at(5, &off), rgb(word(10, 10, 10)));
    // $2130 bits 4-5 = 3: math never.
    let never = LineRegisters {
        prevent_mode: 3,
        ..regs()
    };
    assert_eq!(at(5, &never), rgb(word(10, 10, 10)));
    // Force main black: the hill alone (0 + 20) / 1, no half on a black
    // main screen.
    let black = LineRegisters {
        clip_mode: 3,
        ..regs()
    };
    assert_eq!(at(5, &black), rgb(word(20, 0, 0)));
    // Brightness scales the result last.
    let dim = LineRegisters {
        brightness: 7,
        ..regs()
    };
    assert_eq!(at(5, &dim), bgr555_to_rgb(word(15, 5, 5), 7));
}

#[test]
fn a_margin_dot_says_which_layer_it_belongs_to() {
    let (layers, frames, pal) = compose_fixture();
    let layer = |dx: i64, r: &LineRegisters| {
        let (rgb, layer) = margin_color_layer(&layers, &frames, 0, dx, None, r, &pal);
        assert_eq!(rgb, margin_color(&layers, &frames, 0, dx, None, r, &pal));
        layer
    };
    // BG1 known on the main screen at x = 7: the main layer.
    assert_eq!(layer(7, &regs()), Some(0));
    // Nothing on the main screen at x = 5, BG2's hill on the sub screen the
    // row blends in (Super Mario World): the hill's layer, not the backdrop.
    assert_eq!(layer(5, &regs()), Some(1));
    // The sub screen not selected: the hill is not shown, so the dot is the
    // backdrop's.
    let fixed_only = LineRegisters {
        use_subscreen: false,
        ..regs()
    };
    assert_eq!(layer(5, &fixed_only), None);
    // Nothing known at x = 6: the backdrop.
    assert_eq!(layer(6, &regs()), None);
}

#[test]
fn math_applies_to_the_main_layer_only_when_its_own_enable_bit_is_set() {
    let (layers, frames, pal) = compose_fixture();
    // BG1 known at x = 7 on the main screen.
    let at = |r: &LineRegisters| margin_color(&layers, &frames, 0, 7, None, r, &pal);
    // Enable bit for the backdrop only: BG1 is shown as it is.
    assert_eq!(at(&regs()), rgb(word(1, 2, 3)));
    // Enable bit 0 (BG1): the ground blends with the fixed colour
    // ((1+4)/2 per channel, BG2 unknown at x = 7 so a transparent sub).
    let on = LineRegisters {
        math_enable: 0x01,
        half: false,
        ..regs()
    };
    assert_eq!(at(&on), rgb(word(5, 6, 7)));
    // A layer only on the sub screen is never the main source.
    let ts_only = LineRegisters {
        tm: 0x00,
        ts: 0x01,
        ..regs()
    };
    assert_eq!(
        at(&ts_only),
        rgb(word(5, 6, 6)),
        "backdrop + BG1 on the sub screen, halved"
    );
}

#[test]
fn the_margin_reads_each_rows_own_world_x() {
    let (mut layers, mut frames, pal) = compose_fixture();
    layers.canvases[1] = Some(canvas_with(&[(5, 0, 9), (5, 1, 9)]));
    // Row 1 of BG2 was drawn 8 px further along than row 0: its hill
    // (world x 5) is 8 columns further left of the origin.
    frames.row_dx = vec![[0; LAYERS], [0, 8, 0, 0]];
    let r = regs();
    let hill = rgb(word(15, 5, 5));
    assert_eq!(margin_color(&layers, &frames, 0, 5, None, &r, &pal), hill);
    assert_ne!(margin_color(&layers, &frames, 1, 5, None, &r, &pal), hill);
    assert_eq!(margin_color(&layers, &frames, 1, -3, None, &r, &pal), hill);
}

// ---- the NES as layer 0 (moved from the composite accumulator, W4-03e, W30-03)

mod nes_layer0 {
    use super::*;

    const WIDTH: u16 = 256;
    const HEIGHT: u16 = 240;

    fn bg(v: u8) -> PpuPixel {
        px(PixelLayer::Background(0), v)
    }

    /// A TEXTURED frame (world-shifted, XOR-modulo texture), with a
    /// `ScrollWrite` moving the primary band to world x `shift`: content and
    /// scroll register move together, as in a real scrolling frame. A SOLID
    /// fill is a vacuity trap here: `scene_identity`'s edge-based signature is
    /// "all flat" for ANY solid colour, so it cannot tell "different scene"
    /// from "same scene, different fill".
    fn frame_bundle(frame_count: u64, shift: i64, seed: u8) -> FrameBundle {
        frame_bundle_with_register(frame_count, shift, shift, seed)
    }

    /// As [`frame_bundle`], with the scroll register saying `register` while
    /// the picture really is at `shift` (a mis-placed feed).
    fn frame_bundle_with_register(
        frame_count: u64,
        shift: i64,
        register: i64,
        seed: u8,
    ) -> FrameBundle {
        let (width, height) = (usize::from(WIDTH), usize::from(HEIGHT));
        let mut video = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                let world_x = x as i64 + shift;
                let v = (world_x.rem_euclid(97) as u8) ^ ((y as i64).rem_euclid(61) as u8) ^ seed;
                video.push(bg(v));
            }
        }
        let events = vec![
            CoreEvent::ScrollWrite {
                x: register.rem_euclid(512) as u16,
                y: 0,
                layer: PixelLayer::Background(0),
            },
            CoreEvent::Scanline(0),
            CoreEvent::Scanline(HEIGHT - 1),
        ];
        FrameBundle::new(frame_count, WIDTH, HEIGHT, video, events)
    }

    fn observe(m: &mut CanvasModel, b: &FrameBundle) -> SceneId {
        m.observe_frame(b, &FrameFeed::bands())
    }

    fn layer0(m: &CanvasModel) -> Option<Canvas> {
        m.current_canvases().canvases[0].clone()
    }

    #[test]
    fn a_fresh_model_has_no_scene_or_canvas() {
        let m = CanvasModel::new();
        assert_eq!(m.current_scene(), None);
        assert!(!m.current_canvases().any());
        assert_eq!(m.frames(), None);
    }

    #[test]
    fn the_frame_position_follows_the_scroll_and_the_scene() {
        let mut m = CanvasModel::new();
        let id = observe(&mut m, &frame_bundle(0, 0, 3));
        let first = m.frames().expect("a frame with a band");
        assert_eq!(first.scene, id.0);
        let id2 = observe(&mut m, &frame_bundle(1, 4, 3));
        let second = m.frames().unwrap();
        assert_eq!(second.scene, id2.0);
        if id == id2 {
            let (a, b) = (first.origins[0].unwrap().0, second.origins[0].unwrap().0);
            assert_eq!(b - a, 4, "scrolled 4 dots right");
        }
        // A cut is unsettled at first (W33-09): no scene, nothing placed.
        let first_cut_frame = observe(&mut m, &frame_bundle(2, 300, 250));
        assert_eq!(first_cut_frame, crate::scene_tracker::SETTLING);
        assert_eq!(m.frames(), None);
        let mut cut = first_cut_frame;
        for f in 3..40u64 {
            cut = observe(&mut m, &frame_bundle(f, 300, 250));
        }
        assert_ne!(cut, id, "a cut is a different scene");
        assert_ne!(cut, crate::scene_tracker::SETTLING, "and it settles");
        assert_eq!(m.frames().unwrap().scene, cut.0);
    }

    /// W33-07: a player standing still leaves every band unchanged, and the
    /// scroll tracker's zero-delta rule flags the level band as HUD too. The
    /// primary band is never HUD for not having moved: later frames of a
    /// standing scene still reach the canvas (live wins over what was there).
    #[test]
    fn a_standing_player_still_stitches_the_primary_band() {
        let mut m = CanvasModel::new();
        observe(&mut m, &frame_bundle(0, 0, 3));
        observe(&mut m, &frame_bundle(1, 4, 3));
        // Nothing moves from here on, but a tile animates (a 16 px square
        // with another seed; the rest of the picture is unchanged, so the
        // registration guard has no quarrel with it).
        for frame in 2..6 {
            let mut b = frame_bundle(frame, 4, 3);
            let still = frame_bundle(frame, 4, 9);
            for y in 0..16usize {
                for x in 0..16usize {
                    b.video[y * usize::from(WIDTH) + x] = still.video[y * usize::from(WIDTH) + x];
                }
            }
            observe(&mut m, &b);
        }
        let c = layer0(&m).expect("a canvas");
        let at = |x: i64, y: i64| c.get(x, y).map(|p| p.palette_index);
        let want = |x: i64, y: i64, seed: u8| (x.rem_euclid(97) as u8) ^ ((y % 61) as u8) ^ seed;
        // Screen column 6 at shift 4 is world 10.
        assert_eq!(at(10, 7), Some(want(10, 7, 9)), "the standing frames");
        assert_eq!(at(104, 7), Some(want(104, 7, 3)), "the rest is untouched");
    }

    /// ...and the HUD rule survives: a band that stays fixed while the level
    /// stands still too is still not stitched into the world.
    #[test]
    fn a_fixed_status_band_is_not_stitched_while_the_level_stands_still() {
        let (width, height) = (usize::from(WIDTH), usize::from(HEIGHT));
        let status_rows = 32usize;
        let frame = |n: u64| {
            let mut video = Vec::with_capacity(width * height);
            for y in 0..height {
                for x in 0..width {
                    let v = if y < status_rows {
                        250
                    } else {
                        ((x as i64 + 4).rem_euclid(97) as u8) ^ ((y as i64).rem_euclid(61) as u8)
                    };
                    video.push(bg(v));
                }
            }
            let sw = |x: u16| CoreEvent::ScrollWrite {
                x,
                y: 0,
                layer: PixelLayer::Background(0),
            };
            let mut events = vec![sw(0)];
            events.extend((0..status_rows as u16).map(CoreEvent::Scanline));
            events.push(sw(4));
            events.extend((status_rows as u16..HEIGHT).map(CoreEvent::Scanline));
            FrameBundle::new(n, WIDTH, HEIGHT, video, events)
        };
        let mut m = CanvasModel::new();
        for n in 0..8 {
            observe(&mut m, &frame(n));
        }
        let c = layer0(&m).expect("a canvas");
        let (ox, oy) = c.origin();
        let status_cells = (0..c.height() as i64)
            .flat_map(|y| (0..c.width() as i64).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                c.get(ox + x, oy + y)
                    .is_some_and(|p| p.palette_index == 250)
            })
            .count();
        assert_eq!(status_cells, 0, "the status bar stays out of the world");
        assert!(
            c.get(ox + 100, oy + 200).is_some() || c.width() > 0,
            "while the level is stitched"
        );
    }

    /// A camera that stops (the scroll register unchanged, which the scroll
    /// tracker calls a HUD band) keeps its frame position.
    #[test]
    fn a_still_camera_keeps_its_frame_position() {
        let mut m = CanvasModel::new();
        observe(&mut m, &frame_bundle(0, 0, 3));
        observe(&mut m, &frame_bundle(1, 4, 3));
        let moving = m.frames().expect("moving");
        for frame in 2..6 {
            observe(&mut m, &frame_bundle(frame, 4, 3));
            let now = m.frames().expect("frame");
            assert_eq!(now.origins, moving.origins, "frame {frame}");
            assert_eq!(now.scene, moving.scene, "frame {frame}");
        }
    }

    /// Ticket W30-03: a SNES core's own scroll events are not the level
    /// layer's; the scene tracker is fed the scrolling layer's registers
    /// instead, and the layer canvas is placed by them.
    #[test]
    fn the_scene_tracker_is_fed_the_layer_scroll_when_the_core_emits_none() {
        let mut m = CanvasModel::new();
        let no_scroll = |frame: u64, shift: i64| {
            let b = frame_bundle(frame, shift, 5);
            // The SNES core's run: rows 1.., then a stray row 0 from the
            // neighbouring frame, which would close the band empty.
            let events = (1..HEIGHT)
                .chain(std::iter::once(0))
                .map(CoreEvent::Scanline)
                .collect();
            FrameBundle::new(frame, WIDTH, HEIGHT, b.video, events)
        };
        let mut scenes = std::collections::BTreeSet::new();
        for frame in 0..12u64 {
            let shift = frame as i64 * 3;
            let s = [
                Some(LayerScroll {
                    hofs: shift as u16,
                    vofs: 0,
                    tilemap_width: 512,
                    tilemap_height: 256,
                }),
                None,
                None,
                None,
            ];
            let feed = FrameFeed::registers(Some(&s)).with_scene_scroll(Some((shift as u16, 0)));
            scenes.insert(m.observe_frame(&no_scroll(frame, shift), &feed).0);
        }
        assert_eq!(scenes.len(), 1, "steady scroll is one scene: {scenes:?}");
        let pos = m.frames().expect("the scrolling layer places the frame");
        assert!(pos.origins[0].unwrap().0 >= 30, "{pos:?}");
        let canvas = layer0(&m).unwrap();
        assert!(canvas.width() > usize::from(WIDTH), "{}", canvas.width());
    }

    #[test]
    fn observing_a_frame_produces_a_scene_and_a_non_empty_canvas() {
        let mut m = CanvasModel::new();
        let id = observe(&mut m, &frame_bundle(0, 0, 5));
        assert_eq!(m.current_scene(), Some(id));
        let canvas = layer0(&m).expect("a canvas must exist after one frame");
        assert!(canvas.width() > 0 && canvas.height() > 0);
    }

    /// Proves the placement uses the REAL bands, not always `(0, 0)`: a
    /// continuous scroll across several frames accumulates a canvas WIDER
    /// than one frame, the "whole level" property.
    #[test]
    fn a_continuous_scrolling_session_grows_the_canvas_past_one_frames_width() {
        let mut m = CanvasModel::new();
        for frame in 0..20u64 {
            observe(&mut m, &frame_bundle(frame, (frame as i64) * 4, 7));
        }
        let canvas = layer0(&m).expect("canvas must exist");
        assert!(
            canvas.width() > usize::from(WIDTH),
            "20 frames of steady scroll (up to 76px) must accumulate more than one frame's \
             {WIDTH}px, got {}",
            canvas.width()
        );
    }

    /// A genuine scene cut (world position jump far past
    /// `MAX_CONTINUOUS_STEP_PX`) must route later frames into a SEPARATE
    /// canvas, not corrupt the original's world-coordinate space.
    /// Ticket W33-04: the NES bands are checked like the SNES layers: a
    /// picture that is not where its scroll register says is not written.
    #[test]
    fn a_nes_band_that_disagrees_with_the_canvas_is_not_written() {
        let mut m = CanvasModel::new();
        for f in 0..12 {
            observe(&mut m, &frame_bundle(f, 3 * f as i64, 3));
        }
        let level = |m: &CanvasModel| m.frames().unwrap().confidence[0].level();
        assert_eq!(level(&m), 3, "a correct feed keeps full confidence");
        let before = layer0(&m).expect("canvas");
        // From here the register runs 5 dots ahead of the picture.
        for f in 12..16 {
            observe(
                &mut m,
                &frame_bundle_with_register(f, 3 * f as i64, 3 * f as i64 + 5, 3),
            );
        }
        assert!(m.frames().unwrap().confidence[0].last.disagrees());
        assert!(level(&m) < 3, "confidence fell");
        assert_eq!(layer0(&m).as_ref(), Some(&before), "no cell was written");
        // The fault clears.
        for f in 16..16 + 3 * GOOD_FRAMES_PER_STEP as u64 {
            observe(&mut m, &frame_bundle(f, 3 * f as i64, 3));
        }
        assert_eq!(level(&m), 3, "and confidence recovers");
    }

    #[test]
    fn a_scene_cut_starts_a_separate_canvas_not_corrupting_the_first() {
        let mut m = CanvasModel::new();
        let before = observe(&mut m, &frame_bundle(0, 0, 3));
        let canvas_before = layer0(&m).unwrap();
        let mut after = observe(&mut m, &frame_bundle(1, 300, 250));
        assert_eq!(after, crate::scene_tracker::SETTLING, "a cut settles first");
        for f in 2..40u64 {
            after = observe(&mut m, &frame_bundle(f, 300 + 3 * f as i64, 250));
        }
        assert_ne!(before, after, "a scroll jump this large is a scene cut");
        assert_ne!(canvas_before, layer0(&m).unwrap());
        assert_eq!(
            m.scene_canvases(before.0).unwrap()[0],
            canvas_before,
            "the first scene's canvas is untouched"
        );
    }

    #[test]
    fn flush_with_no_cache_configured_is_a_harmless_no_op() {
        let mut m = CanvasModel::new();
        observe(&mut m, &frame_bundle(0, 0, 1));
        m.flush();
    }

    fn unique_cache_dir(label: &str) -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "retroforge-canvas-model-test-{label}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    /// Persistence round trip through a REAL `rf_cache::Cache`: a fresh cache
    /// over the same root, not the live in-memory object.
    #[test]
    fn flush_then_a_fresh_model_restores_layer_0_from_disk() {
        let root = unique_cache_dir("roundtrip");
        let cache = Cache::open(&root, 10_000_000).unwrap();
        let mut m = CanvasModel::with_cache("rom-xyz".to_string(), Some(cache));
        let id = observe(&mut m, &frame_bundle(0, 0, 9));
        m.flush();
        let saved = layer0(&m).unwrap();
        drop(m); // simulate process exit: nothing live carries over

        let fresh_cache = Cache::open(&root, 10_000_000).unwrap();
        let mut fresh = CanvasModel::with_cache("rom-xyz".to_string(), Some(fresh_cache));
        // The same first frame again: the stitch is idempotent, so the
        // comparison is meaningful.
        let restored_id = observe(&mut fresh, &frame_bundle(0, 0, 9));
        assert_eq!(restored_id, id, "same content, same scene id");
        assert_eq!(layer0(&fresh).unwrap(), saved);
        std::fs::remove_dir_all(&root).ok();
    }

    /// Ticket W34-07: other producers keep blobs beside the scenes through the
    /// model's one cache: queued, written by the flush, read by a fresh model
    /// over the same root; the newest bytes win; a miss and a model with no
    /// cache read as nothing.
    #[test]
    fn extra_blobs_ride_the_models_cache() {
        let key = |what: &str| rf_cache::CacheKey {
            rom_sha256: "rom-extra".to_string(),
            asset_hash: what.to_string(),
            producer: "test".to_string(),
            settings_hash: "v1".to_string(),
        };
        let root = unique_cache_dir("extra");
        let mut m = CanvasModel::with_cache(
            "rom-extra".to_string(),
            Some(Cache::open(&root, 10_000_000).unwrap()),
        );
        assert!(m.fetch_extra(&key("a")).is_none(), "nothing before a write");
        m.queue_extra(key("a"), vec![1, 2, 3]);
        m.queue_extra(key("a"), vec![4, 5]);
        m.queue_extra(key("b"), vec![9]);
        assert!(
            m.fetch_extra(&key("a")).is_none(),
            "queued is not written until the flush"
        );
        m.flush();
        drop(m);
        let mut fresh = CanvasModel::with_cache(
            "rom-extra".to_string(),
            Some(Cache::open(&root, 10_000_000).unwrap()),
        );
        assert_eq!(fresh.fetch_extra(&key("a")), Some(vec![4, 5]));
        assert_eq!(fresh.fetch_extra(&key("b")), Some(vec![9]));
        assert!(fresh.fetch_extra(&key("c")).is_none());
        let mut none = CanvasModel::with_cache(String::new(), None);
        none.queue_extra(key("a"), vec![1]);
        none.flush();
        assert!(none.fetch_extra(&key("a")).is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    /// Ticket W33-05: the SNES persists again, every layer with its palettes
    /// and confidence, and the restored scene is complete on the FIRST frame
    /// of the next session (no scout, no walking).
    #[test]
    fn a_register_fed_scene_persists_every_layer_and_restores_on_the_first_frame() {
        let root = unique_cache_dir("snes");
        let cache = Cache::open(&root, 10_000_000).unwrap();
        let mut m = CanvasModel::with_cache("rom-snes".to_string(), Some(cache));
        let id = SceneId(11);
        let mut words = [0u16; 256];
        words[3] = 0x1234;
        let rows: Vec<Option<&Cgram>> = vec![Some(&words); usize::from(H)];
        // The camera walks right; both layers fill a strip.
        for step in 0..6u16 {
            let (h0, h1) = (step * 16, step * 4);
            m.feed_with_palettes(
                &bundle([h0, h1], |_, _, _| {}),
                id,
                Some(&scrolls(h0, h1)),
                &rows,
            );
        }
        m.mark_scouted(id.0);
        let saved = m.current_canvases();
        let saved_conf = m.frames().unwrap().confidence;
        assert!(saved.canvases[0].is_some() && saved.canvases[1].is_some());
        assert!(!saved.palettes.is_empty());
        drop(m); // the process ends

        let cache = Cache::open(&root, 10_000_000).unwrap();
        let mut fresh = CanvasModel::with_cache("rom-snes".to_string(), Some(cache));
        // ONE frame, at the position the last session left the camera at.
        let (h0, h1) = (5 * 16, 5 * 4);
        fresh.feed_with_palettes(
            &bundle([h0, h1], |_, _, _| {}),
            id,
            Some(&scrolls(h0, h1)),
            &rows,
        );
        assert!(fresh.scene_restored(id.0));
        assert!(fresh.scene_scouted(id.0), "a scouted scene stays scouted");
        let back = fresh.current_canvases();
        assert_eq!(back.canvases, saved.canvases, "every layer, cell for cell");
        assert_eq!(back.palettes, saved.palettes);
        let conf = fresh.frames().unwrap().confidence;
        for n in 0..2 {
            assert_eq!(conf[n].level(), saved_conf[n].level());
            assert_eq!(
                conf[n].total_agreed,
                saved_conf[n].total_agreed + conf[n].last.agreed as u64
            );
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// Scene edges come back with the scene, so a revisit knows its
    /// neighbours at once.
    #[test]
    fn a_scenes_edges_are_restored_with_it() {
        let root = unique_cache_dir("edges");
        let cache = Cache::open(&root, 10_000_000).unwrap();
        let mut m = CanvasModel::with_cache("rom-nes".to_string(), Some(cache));
        let a = observe(&mut m, &frame_bundle(0, 0, 3));
        let mut b = a;
        for f in 1..40u64 {
            b = observe(&mut m, &frame_bundle(f, 300 + 3 * f as i64, 250));
        }
        assert_ne!(a, b);
        let edges = m.scene_edges().to_vec();
        assert!(edges.iter().any(|e| e.from == a.0 && e.to == b.0));
        drop(m);

        let cache = Cache::open(&root, 10_000_000).unwrap();
        let mut fresh = CanvasModel::with_cache("rom-nes".to_string(), Some(cache));
        assert!(fresh.scene_edges().is_empty());
        let again = observe(&mut fresh, &frame_bundle(0, 0, 3));
        assert_eq!(again, a);
        assert_eq!(
            fresh.scene_edges(),
            edges.as_slice(),
            "latest per pair, plain data"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// An entry from another build (here: garbage under the right key) is
    /// ignored; the scene starts empty and nothing panics.
    #[test]
    fn a_corrupt_cache_entry_is_ignored_and_the_scene_starts_empty() {
        let root = unique_cache_dir("corrupt");
        let mut cache = Cache::open(&root, 10_000_000).unwrap();
        let mut m = CanvasModel::with_cache("rom-nes".to_string(), None);
        let id = observe(&mut m, &frame_bundle(0, 0, 9));
        cache
            .put(
                &crate::persistence::scene_cache_key("rom-nes", id),
                b"RFSC\x02\x00truncated",
            )
            .unwrap();
        let mut fresh = CanvasModel::with_cache("rom-nes".to_string(), Some(cache));
        let again = observe(&mut fresh, &frame_bundle(0, 0, 9));
        assert_eq!(again, id);
        assert!(!fresh.scene_restored(id.0));
        std::fs::remove_dir_all(&root).ok();
    }

    /// A quit loses nothing: the scene is written when the model drops, and a
    /// scene left behind is written the moment the game cuts away from it.
    #[test]
    fn a_scene_is_written_when_left_and_an_idle_flush_writes_nothing_again() {
        let root = unique_cache_dir("left");
        let cache = Cache::open(&root, 10_000_000).unwrap();
        let mut m = CanvasModel::with_cache("rom-nes".to_string(), Some(cache));
        let a = observe(&mut m, &frame_bundle(0, 0, 3));
        // The cut: SETTLING frames write the scene that was left.
        observe(&mut m, &frame_bundle(1, 300, 250));
        let on_disk = Cache::open(&root, 10_000_000).unwrap();
        assert!(
            on_disk.contains(&crate::persistence::scene_cache_key("rom-nes", a)),
            "written when left, not only at exit"
        );
        assert!(m.scene_saved(a.0).is_some());
        assert_eq!(m.flush(), 0, "nothing changed since: nothing is written");
        std::fs::remove_dir_all(&root).ok();
    }

    /// The map view's adapter reads layer 0 on the NES and the level's own
    /// layer on the SNES; a HUD layer is never it.
    #[test]
    fn the_map_canvas_is_layer_0_or_the_first_level_layer() {
        let mut layers = LayerCanvases::default();
        assert!(layers.map_canvas().is_none());
        layers.canvases[1] = Some(canvas_with(&[(5, 0, 22)]));
        assert_eq!(
            layers
                .map_canvas()
                .unwrap()
                .get(5, 0)
                .unwrap()
                .palette_index,
            22
        );
        layers.canvases[0] = Some(canvas_with(&[(5, 0, 11)]));
        assert_eq!(
            layers
                .map_canvas()
                .unwrap()
                .get(5, 0)
                .unwrap()
                .palette_index,
            11
        );
        layers.hud[0] = true;
        assert_eq!(
            layers
                .map_canvas()
                .unwrap()
                .get(5, 0)
                .unwrap()
                .palette_index,
            22,
            "layer 0 is HUD here"
        );
    }

    /// The NES's margin: the front-most known cell resolved by the fixed
    /// table, else the row's backdrop, else nothing (the PPU's own pixel).
    #[test]
    fn margin_pixel_without_registers_uses_the_fixed_table_and_the_backdrop() {
        let layers = LayerCanvases {
            canvases: [Some(canvas_with(&[(5, 0, 7)])), None, None, None],
            ..Default::default()
        };
        let frames = LayerFrames {
            scene: 1,
            origins: [Some((0, 0)), None, None, None],
            row_dx: Vec::new(),
            confidence: Default::default(),
        };
        let facts = RowFacts {
            backdrop: Some([9, 9, 9]),
            ..RowFacts::default()
        };
        assert_eq!(
            margin_pixel(&layers, &frames, 0, 5, &facts, &nes_rgb),
            Some(nes_rgb(7))
        );
        assert_eq!(
            margin_pixel(&layers, &frames, 0, 6, &facts, &nes_rgb),
            Some([9, 9, 9])
        );
        assert_eq!(
            margin_pixel(&layers, &frames, 0, 6, &RowFacts::default(), &nes_rgb),
            None
        );
    }

    /// Ticket W34-09: the NES's one layer is layer 0; the backdrop has none.
    #[test]
    fn margin_pixel_layer_names_the_layer_of_a_known_cell_and_none_for_the_backdrop() {
        let layers = LayerCanvases {
            canvases: [Some(canvas_with(&[(5, 0, 7)])), None, None, None],
            ..Default::default()
        };
        let frames = LayerFrames {
            scene: 1,
            origins: [Some((0, 0)), None, None, None],
            row_dx: Vec::new(),
            confidence: Default::default(),
        };
        let facts = RowFacts {
            backdrop: Some([9, 9, 9]),
            ..RowFacts::default()
        };
        assert_eq!(
            margin_pixel_layer(&layers, &frames, 0, 5, &facts, &nes_rgb),
            Some((nes_rgb(7), Some(0)))
        );
        assert_eq!(
            margin_pixel_layer(&layers, &frames, 0, 6, &facts, &nes_rgb),
            Some(([9, 9, 9], None))
        );
    }
}

mod registration_checks {
    use super::*;

    const RW: u16 = 160;
    const RH: u16 = 32;

    /// One layer (BG1) whose pixel encodes its own world column and row, so a
    /// picture 3 px off the registers disagrees everywhere.
    fn ramp(hofs: u16) -> FrameBundle {
        let mut video = Vec::new();
        for y in 0..usize::from(RH) {
            for c in 0..usize::from(RW) {
                let v = ((usize::from(hofs) + c) * 7 + y * 13) as u8;
                video.push(px(PixelLayer::Background(0), v));
            }
        }
        FrameBundle::new(0, RW, RH, video, Vec::<CoreEvent>::new())
    }

    fn one_layer(hofs: u16) -> [Option<LayerScroll>; LAYERS] {
        [scroll(hofs), None, None, None]
    }

    const ID: SceneId = SceneId(1);
    const SPEED: u16 = 2;

    /// Frame `f`: the picture is at `f * SPEED`, the registers say `claimed`.
    fn feed_frame(m: &mut CanvasModel, f: u16, claimed: u16) {
        m.feed(&ramp(f * SPEED), ID, Some(&one_layer(claimed)));
    }

    fn conf(m: &CanvasModel) -> Confidence {
        m.frames().unwrap().confidence[0]
    }

    fn warmed_up(frames: u16) -> CanvasModel {
        let mut m = CanvasModel::new();
        for f in 0..frames {
            feed_frame(&mut m, f, f * SPEED);
        }
        m
    }

    #[test]
    fn a_correct_placement_keeps_full_confidence_and_grows_the_canvas() {
        let m = warmed_up(60);
        let c = conf(&m);
        assert_eq!(c.level(), 3);
        assert!(c.last.agrees(), "{c:?}");
        assert_eq!(c.agreement_percent(), Some(100.0));
        let canvas = m.current_canvases().canvases[0].clone().unwrap();
        assert!(canvas.width() >= usize::from(RW) + 59 * usize::from(SPEED));
    }

    #[test]
    fn the_first_frame_of_a_scene_has_no_evidence_and_is_still_written() {
        let m = warmed_up(1);
        let c = conf(&m);
        assert_eq!((c.level(), c.last.judged_bands), (3, 0));
        assert!(m.current_canvases().canvases[0].is_some());
    }

    /// A register read 3 px off the picture: detected within 10 frames, no
    /// cell written, confidence drops, and a clean feed recovers.
    #[test]
    fn a_mis_placed_feed_is_detected_withheld_and_recovers() {
        let mut m = warmed_up(20);
        let before = m.current_canvases();
        let mut detected_at = None;
        for (i, f) in (20..30).enumerate() {
            feed_frame(&mut m, f, f * SPEED + 3);
            if detected_at.is_none() && conf(&m).last.disagrees() {
                detected_at = Some(i);
            }
        }
        assert_eq!(detected_at, Some(0), "detected on the first bad frame");
        assert!(conf(&m).level() < 3, "{:?}", conf(&m));
        assert_eq!(
            m.current_canvases().canvases,
            before.canvases,
            "nothing written"
        );
        // The residual says where the picture really sits.
        assert_eq!(conf(&m).last.residual, Some(-3));
        // Fault clears: the next frames agree, and confidence climbs back.
        for f in 30..30 + 3 * GOOD_FRAMES_PER_STEP as u16 + 4 {
            feed_frame(&mut m, f, f * SPEED);
        }
        assert!(conf(&m).last.agrees());
        assert_eq!(conf(&m).level(), 3);
        let canvas = m.current_canvases().canvases[0].clone().unwrap();
        let probe = 10 + i64::from(30 * SPEED);
        let want = (probe as usize * 7) as u8;
        assert_eq!(canvas.get(probe, 0).map(|p| p.palette_index), Some(want));
    }

    /// The scroll registers read one frame late (the old lag bug).
    #[test]
    fn a_one_frame_lag_is_detected() {
        let mut m = warmed_up(20);
        let before = m.current_canvases();
        for f in 20..26 {
            feed_frame(&mut m, f, (f - 1) * SPEED);
        }
        assert!(conf(&m).last.disagrees());
        assert!(conf(&m).level() < 3);
        assert_eq!(m.current_canvases().canvases, before.canvases);
    }

    /// A gross error (the old widened-line bundle bug was 128 px): outside
    /// the +-8 search, so no residual is offered, but the rows are withheld
    /// all the same. Only caught where the canvas already knows the ground
    /// the picture lands on; a fresh stretch is no evidence.
    #[test]
    fn a_gross_error_over_known_ground_is_detected() {
        let mut m = warmed_up(70);
        let before = m.current_canvases();
        feed_frame(&mut m, 70, 70 * SPEED + 100);
        assert!(conf(&m).last.disagrees());
        assert_eq!(conf(&m).last.residual, None, "out of search range");
        assert_eq!(m.current_canvases().canvases, before.canvases);
    }

    #[test]
    fn a_distrusted_layer_gives_the_margins_nothing_and_takes_no_scout_cells() {
        let mut m = warmed_up(20);
        for f in 20..26 {
            feed_frame(&mut m, f, f * SPEED + 3);
        }
        assert!(conf(&m).level() < 3);
        let mut scout: [Canvas; LAYERS] = std::array::from_fn(|_| Canvas::new());
        scout[0] = canvas_with(&[(5000, 0, 9)]);
        assert_eq!(m.merge_unknown_from(ID.0, &scout), 0, "no scout cell joins");
        // Two more disagreeing frames take it below 2: no margin cells.
        for f in 26..30 {
            feed_frame(&mut m, f, f * SPEED + 3);
        }
        let set = m.current_canvases();
        assert_eq!(set.untrusted[0], conf(&m).level() < 2);
        assert!(set.untrusted[0], "{:?}", conf(&m));
        assert!(!set.usable(0));
    }

    /// A screen the scene tracker did not cut: the canvas is stale, not the
    /// placement wrong. Writes are withheld for a while, then the canvas is
    /// dropped and the new picture is learned (no permanent freeze).
    #[test]
    fn a_canvas_the_picture_never_agrees_with_again_is_dropped_and_relearned() {
        let mut m = warmed_up(20);
        // A different picture (offset ramp) with consistent registers.
        let other = |f: u16| {
            let mut b = ramp(f * SPEED);
            for p in &mut b.video {
                p.palette_index = p.palette_index.wrapping_add(101);
            }
            b
        };
        for f in 20..20 + STALE_FRAMES as u16 + 1 {
            m.feed(&other(f), ID, Some(&one_layer(f * SPEED)));
        }
        assert_eq!(conf(&m).resets, 1, "{:?}", conf(&m));
        assert_eq!(conf(&m).level(), 3, "starts over");
        for f in 66..80 {
            m.feed(&other(f), ID, Some(&one_layer(f * SPEED)));
        }
        let canvas = m.current_canvases().canvases[0].clone().unwrap();
        let x = i64::from(70 * SPEED) + 5;
        assert_eq!(
            canvas.get(x, 0).map(|p| p.palette_index),
            Some(((x as usize * 7) as u8).wrapping_add(101)),
            "the new picture was written"
        );
        assert!(conf(&m).last.agrees());
    }

    #[test]
    fn confidence_steps_follow_the_streaks() {
        let mut c = Confidence::default();
        let bad = Check {
            withheld_rows: 4,
            judged_rows: 4,
            picture_rows: 4,
            judged_bands: 1,
            ..Check::default()
        };
        let good = Check {
            judged_bands: 1,
            agreed: 40,
            compared: 40,
            ..Check::default()
        };
        for _ in 0..BAD_FRAMES_PER_STEP {
            c.observe(bad);
        }
        assert_eq!(c.level(), 2);
        // A frame with no verdict counts toward recovery (nothing was
        // contradicted), and an isolated bad frame never steps down.
        c.observe(Check::default());
        for _ in 0..GOOD_FRAMES_PER_STEP - 2 {
            c.observe(good);
        }
        assert_eq!(c.level(), 2);
        c.observe(good);
        assert_eq!(c.level(), 3);
        c.observe(good);
        assert_eq!(c.level(), 3, "capped");
    }

    /// A scout cell is not evidence about a live frame: wrong scout cells
    /// under the live game's own frames must not make the placement check
    /// withhold them (the garbage would stay on the canvas).
    #[test]
    fn wrong_scout_cells_under_live_frames_are_not_a_placement_fault() {
        let mut m = CanvasModel::new();
        for f in 0..60u16 {
            feed_frame(&mut m, f, f * SPEED);
        }
        // The live game knows world x 0..278. The scout filled everything
        // from 280 to 1000, every row, with garbage.
        let mut delta = Canvas::new();
        for y in 0..i64::from(RH) {
            let row: Vec<_> = (280..1000)
                .map(|_| Some(px(PixelLayer::Background(0), 250)))
                .collect();
            delta.blit_row(280, y, &row);
        }
        let delta = [delta, Canvas::new(), Canvas::new(), Canvas::new()];
        assert!(m.merge_unknown_from(ID.0, &delta) > 0);
        // The live game gets there in big steps: most of every frame lies on
        // scout cells, a strip of it on live ones.
        for hofs in [180u16, 260, 340, 420, 500] {
            m.feed(&ramp(hofs), ID, Some(&one_layer(hofs)));
            let last = m.frames().unwrap().confidence[0].last;
            assert_eq!(
                last.withheld_rows, 0,
                "hofs {hofs}: withheld over scout cells"
            );
        }
        let c = &m.scene_canvases(ID.0).unwrap()[0];
        for y in [0i64, 7, 31] {
            let want = (350 * 7 + y * 13) as u8;
            assert_eq!(c.get(350, y).map(|p| p.palette_index), Some(want));
            assert!(!c.is_scout(350, y), "the live frame made the cell live");
        }
        assert_eq!(m.frames().unwrap().confidence[0].level(), MAX_LEVEL);
    }
}

mod scout_agreement_checks {
    use super::*;

    fn filled(v: impl Fn(i64) -> u8) -> Canvas {
        let mut c = Canvas::new();
        let row: Vec<_> = (0..3000)
            .map(|x| Some(px(PixelLayer::Background(0), v(x))))
            .collect();
        c.blit_row(0, 0, &row);
        c
    }

    #[test]
    fn identical_cells_merge_and_a_shifted_scout_is_rejected() {
        let live = filled(|x| (x * 7) as u8);
        let same = scout_agreement(&live, &filled(|x| (x * 7) as u8), None);
        assert_eq!(same, (3000, 3000));
        assert_eq!(scout_verdict(same), Some(ScoutVerdict::Merge));
        let shifted = scout_agreement(&live, &filled(|x| ((x + 3) * 7) as u8), None);
        assert_eq!(scout_verdict(shifted), Some(ScoutVerdict::Reject));
    }

    #[test]
    fn the_thresholds_are_99_and_95_percent_over_2000_shared_cells() {
        assert_eq!(scout_verdict((2970, 3000)), Some(ScoutVerdict::Merge));
        assert_eq!(scout_verdict((2969, 3000)), Some(ScoutVerdict::SlowDown));
        assert_eq!(scout_verdict((2850, 3000)), Some(ScoutVerdict::SlowDown));
        assert_eq!(scout_verdict((2849, 3000)), Some(ScoutVerdict::Reject));
        assert_eq!(
            scout_verdict((1999, 1999)),
            None,
            "too few shared cells to judge"
        );
    }
}

// ---- scenes: level ids, settling, pipe rooms (ticket W33-09)

mod scenes {
    use super::*;
    use crate::scene_tracker::{SceneHints, SETTLE_FRAMES, SETTLING};

    const WIDTH: u16 = 256;
    const HEIGHT: u16 = 240;

    fn cell_colour(cx: i64, cy: i64, base: u8) -> u8 {
        base + (cx.wrapping_mul(7919) ^ cy.wrapping_mul(104_729) ^ (cx * cy)).rem_euclid(24) as u8
    }

    /// The level at world scroll `shift`: 8x8 cells of 24 colours (`base`
    /// shifts the palette), one full-screen band scrolled to `shift`.
    fn picture(frame_count: u64, shift: i64, base: u8) -> FrameBundle {
        let (width, height) = (usize::from(WIDTH), usize::from(HEIGHT));
        let mut video = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                let (wx, wy) = (x as i64 + shift, y as i64);
                video.push(px(
                    PixelLayer::Background(0),
                    cell_colour(wx.div_euclid(8), wy.div_euclid(8), base),
                ));
            }
        }
        let events = vec![
            CoreEvent::ScrollWrite {
                x: shift.rem_euclid(512) as u16,
                y: 0,
                layer: PixelLayer::Background(0),
            },
            CoreEvent::Scanline(0),
            CoreEvent::Scanline(HEIGHT - 1),
        ];
        FrameBundle::new(frame_count, WIDTH, HEIGHT, video, events)
    }

    fn black(frame_count: u64) -> FrameBundle {
        let video =
            vec![px(PixelLayer::Background(0), 15); usize::from(WIDTH) * usize::from(HEIGHT)];
        FrameBundle::new(frame_count, WIDTH, HEIGHT, video, Vec::new())
    }

    fn feed<'a>(level: Option<&'a [u8]>) -> FrameFeed<'a> {
        FrameFeed::bands().with_scene_hints(SceneHints {
            level,
            axis_sections: None,
        })
    }

    fn cells(m: &CanvasModel, scene: SceneId) -> usize {
        m.scene_canvases(scene.0)
            .map_or(0, |c| c[0].cells().iter().flatten().count())
    }

    /// Walk the level: `n` frames from scroll `from`, 3 px a frame.
    fn walk(m: &mut CanvasModel, key: &[u8], from: i64, n: i64, frame: &mut u64) -> SceneId {
        let mut id = SETTLING;
        for i in 0..n {
            *frame += 1;
            id = m.observe_frame(&picture(*frame, from + i * 3, 1), &feed(Some(key)));
        }
        id
    }

    #[test]
    fn a_pipe_room_and_back_leaves_the_levels_canvas_intact() {
        let mut m = CanvasModel::new();
        let mut frame = 0;
        let level = walk(&mut m, &[1], 0, 60, &mut frame);
        assert_ne!(level, SETTLING);
        let before = cells(&m, level);
        let snapshot = m.scene_canvases(level.0).expect("canvas").clone();
        assert!(before > 0);

        // The pipe: the key changes under a black frame; the room is another
        // picture, scrolling slowly (a picture standing still is a HUD band and
        // is never stitched, whatever the scene).
        for _ in 0..4 {
            frame += 1;
            assert_eq!(m.observe_frame(&black(frame), &feed(Some(&[2]))), SETTLING);
        }
        let mut room = SETTLING;
        for _ in 0..SETTLE_FRAMES + 10 {
            frame += 1;
            room = m.observe_frame(&picture(frame, frame as i64, 40), &feed(Some(&[2])));
        }
        assert!(
            room != SETTLING && room != level,
            "the room is a scene of its own"
        );
        assert!(cells(&m, room) > 0, "the room has a canvas");
        assert_eq!(
            cells(&m, level),
            before,
            "the level's cell count changed in the room"
        );

        // Back out: the SAME scene id, so the same canvas, every cell as it
        // was and the walk goes on from there.
        let back = walk(&mut m, &[1], 180, 40, &mut frame);
        assert_eq!(back, level);
        let after = m.scene_canvases(level.0).expect("canvas");
        assert!(cells(&m, level) >= before);
        let (ox, oy) = snapshot[0].origin();
        for y in 0..snapshot[0].height() as i64 {
            for x in 0..snapshot[0].width() as i64 {
                if let Some(p) = snapshot[0].get(ox + x, oy + y) {
                    assert_eq!(
                        after[0].get(ox + x, oy + y),
                        Some(p),
                        "cell ({x},{y}) changed"
                    );
                }
            }
        }
    }

    #[test]
    fn frames_while_a_cut_settles_are_stitched_nowhere() {
        let mut m = CanvasModel::new();
        let mut frame = 0;
        let level = walk(&mut m, &[1], 0, 30, &mut frame);
        let before = cells(&m, level);
        // A level that is still loading: its key moves every frame.
        for i in 0..SETTLE_FRAMES - 1 {
            frame += 1;
            let key = [2, i as u8];
            let id = m.observe_frame(
                &picture(frame, 500 + i64::from(i) * 3, 60),
                &feed(Some(&key)),
            );
            assert_eq!(id, SETTLING);
            assert_eq!(m.current_scene(), Some(SETTLING));
            assert!(
                !m.current_canvases().any(),
                "no canvas to show while unsettled"
            );
            assert_eq!(m.frames(), None);
        }
        assert_eq!(cells(&m, level), before);
        assert!(
            m.scene_canvases(SETTLING.0).is_none(),
            "nothing was stitched under SETTLING"
        );
    }

    #[test]
    fn only_a_continuous_edge_offers_a_neighbour_to_the_margin() {
        let mut m = CanvasModel::new();
        let mut frame = 0;
        // A section change the picture scrolls straight across (MM2's
        // horizontal section into the climb): continuous.
        let a = walk(&mut m, &[1], 0, 40, &mut frame);
        let b = walk(&mut m, &[2], 120, 60, &mut frame);
        assert!(a != b && b != SETTLING);
        let neighbours = m.margin_neighbours(a);
        assert_eq!(neighbours.len(), 1, "{:?}", m.scene_edges());
        assert_eq!((neighbours[0].from, neighbours[0].to), (a.0, b.0));
        // A pipe (a black frame between): never.
        for _ in 0..4 {
            frame += 1;
            m.observe_frame(&black(frame), &feed(Some(&[3])));
        }
        let c = walk(&mut m, &[3], 0, 40, &mut frame);
        assert!(c != b && c != SETTLING);
        assert!(m.margin_neighbours(c).is_empty(), "{:?}", m.scene_edges());
        assert_eq!(
            m.margin_neighbours(b).len(),
            1,
            "b only has the continuous edge to a"
        );
    }

    #[test]
    fn without_a_profile_a_title_a_map_and_a_level_each_get_their_own_canvas() {
        let mut m = CanvasModel::new();
        let mut ids = Vec::new();
        let mut frame = 0;
        for base in [1u8, 30, 70] {
            let mut id = SETTLING;
            for i in 0..40 {
                frame += 1;
                id = m.observe_frame(&picture(frame, i * 2, base), &FrameFeed::bands());
            }
            ids.push(id);
        }
        assert!(ids.iter().all(|&i| i != SETTLING));
        assert!(
            ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2],
            "{ids:?}"
        );
        for id in ids {
            assert!(cells(&m, id) > 0, "{id:?} has no canvas of its own");
        }
    }
}

// ---- W33-06: the per-cell source (Live | Scout)

fn scout_canvas_with(cells: &[(i64, i64, u8)]) -> Canvas {
    let mut c = Canvas::new();
    for &(x, y, v) in cells {
        c.blit_row_scout(x, y, &[Some(px(PixelLayer::Background(0), v))]);
    }
    c
}

#[test]
fn a_merge_tags_the_cells_it_fills_scout_and_leaves_live_cells_live() {
    let mut live = canvas_with(&[(0, 0, 1), (1, 0, 1)]);
    let scout = canvas_with(&[(1, 0, 9), (2, 0, 9)]);
    assert_eq!(merge_unknown(&mut live, &scout), 1);
    assert!(
        !live.is_scout(0, 0) && !live.is_scout(1, 0),
        "live stays live"
    );
    assert!(live.is_scout(2, 0), "the filled cell is a scout cell");
    assert_eq!(live.scout_cell_count(), 1);
    assert_eq!(live.get_live(2, 0), None, "a scout cell is not a live read");
    assert_eq!(live.get(2, 0).map(|p| p.palette_index), Some(9));
    assert!(!live.is_scout(50, 50), "outside the canvas is not scout");
}

#[test]
fn a_scout_never_overwrites_a_live_cell_or_an_earlier_scout_cell() {
    let mut live = canvas_with(&[(0, 0, 1)]);
    assert_eq!(merge_unknown(&mut live, &canvas_with(&[(1, 0, 5)])), 1);
    // A second scout (the left pass) offers different cells at 0 and 1.
    let second = canvas_with(&[(0, 0, 7), (1, 0, 7), (2, 0, 7)]);
    assert_eq!(
        merge_unknown(&mut live, &second),
        1,
        "only x = 2 is unknown"
    );
    assert_eq!(row_of(&live, 0, 0..3), vec![Some(1), Some(5), Some(7)]);
    assert_eq!(live.scout_cell_count(), 2);
}

#[test]
fn a_live_write_over_a_scout_cell_replaces_it_and_makes_it_live() {
    let mut c = scout_canvas_with(&[(0, 0, 9), (1, 0, 9)]);
    c.blit_row(1, 0, &[Some(px(PixelLayer::Background(0), 4))]);
    assert_eq!(row_of(&c, 0, 0..2), vec![Some(9), Some(4)]);
    assert!(c.is_scout(0, 0) && !c.is_scout(1, 0));
    // A live write that skips a cell (None: a sprite was in front) leaves
    // the scout cell and its tag alone.
    c.blit_row(0, 0, &[None, Some(px(PixelLayer::Background(0), 6))]);
    assert!(c.is_scout(0, 0));
}

#[test]
fn growing_a_canvas_keeps_every_cells_source() {
    let mut c = scout_canvas_with(&[(5, 5, 9)]);
    c.blit_row(6, 5, &[Some(px(PixelLayer::Background(0), 1))]);
    // Grow in every direction.
    c.blit_row(-20, -20, &[Some(px(PixelLayer::Background(0), 2))]);
    c.blit_row(200, 40, &[Some(px(PixelLayer::Background(0), 3))]);
    assert!(c.is_scout(5, 5) && !c.is_scout(6, 5));
    assert!(!c.is_scout(-20, -20) && !c.is_scout(200, 40));
    assert_eq!(c.scout_cell_count(), 1);
}

#[test]
fn canvases_equal_when_cells_and_sources_equal_whatever_the_flag_vector_looks_like() {
    let live_only = canvas_with(&[(0, 0, 1), (1, 0, 2)]);
    // The same cells after a scout tag was written over and cleared again.
    let mut churned = scout_canvas_with(&[(0, 0, 9), (1, 0, 9)]);
    churned.blit_row(
        0,
        0,
        &[
            Some(px(PixelLayer::Background(0), 1)),
            Some(px(PixelLayer::Background(0), 2)),
        ],
    );
    assert_eq!(live_only, churned);
    let tagged = scout_canvas_with(&[(0, 0, 1), (1, 0, 2)]);
    assert_ne!(live_only, tagged, "same pixels, different source");
}

#[test]
fn agreement_counts_only_live_cells_and_compares_palette_ids_only_when_both_carry_one() {
    let with_id = |v: u8, id: Option<u8>| PpuPixel {
        sprite_id: id,
        ..px(PixelLayer::Background(0), v)
    };
    let mut live = Canvas::new();
    live.blit_row(
        0,
        0,
        &[
            Some(with_id(1, Some(0))),
            Some(with_id(2, Some(0))),
            Some(with_id(3, Some(0))),
            Some(with_id(4, None)),
        ],
    );
    live.blit_row_scout(4, 0, &[Some(with_id(5, Some(0)))]);
    let mut scout = Canvas::new();
    scout.blit_row(
        0,
        0,
        &[
            Some(with_id(1, Some(0))), // same
            Some(with_id(2, Some(1))), // same index, other palette id
            Some(with_id(9, Some(0))), // other index
            Some(with_id(4, Some(2))), // live has no id: index decides
            Some(with_id(5, Some(0))), // over a scout cell: not counted
        ],
    );
    assert_eq!(scout_agreement(&live, &scout, None), (2, 4));
}

#[test]
fn a_scout_a_row_off_is_read_at_that_offset_and_the_offset_never_exceeds_one_row() {
    let pic = |x: i64, y: i64| ((x * 5 + y * y) % 60) as u8;
    let mut live = Canvas::new();
    let mut scout = Canvas::new();
    for y in 0..30i64 {
        let l: Vec<_> = (0..40)
            .map(|x| Some(px(PixelLayer::Background(0), pic(x, y))))
            .collect();
        live.blit_row(0, y, &l);
        // The scout's row y is the live picture's row y + 1.
        let s: Vec<_> = (0..40)
            .map(|x| Some(px(PixelLayer::Background(0), pic(x, y + 1))))
            .collect();
        scout.blit_row(0, y, &s);
    }
    let a = scout_agreement_aligned(&live, &scout, None);
    assert_eq!(a.dy, 1);
    assert_eq!(a.agreed, a.shared);
    let (plain, shared) = scout_agreement(&live, &scout, None);
    assert!(plain < shared, "unaligned they disagree");
    // Two rows off is beyond the tolerance: not read as agreement.
    let mut far = Canvas::new();
    for y in 0..30i64 {
        let s: Vec<_> = (0..40)
            .map(|x| Some(px(PixelLayer::Background(0), pic(x, y + 2))))
            .collect();
        far.blit_row(0, y, &s);
    }
    let b = scout_agreement_aligned(&live, &far, None);
    assert!(b.agreed < b.shared, "a two row error stays an error");
}

/// A live frame landing on cells the scout filled overwrites them, with its
/// own pixels and its own source; the scout's merge never undoes that.
#[test]
fn a_live_frame_overwrites_scout_cells_and_a_later_merge_does_not_take_them_back() {
    let id = SceneId(1);
    let mut live = CanvasModel::new();
    live.feed(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(0, 0)));
    // The scout filled x = 300.. with garbage the live game has not seen.
    let garbage: Vec<(i64, i64, u8)> = (0..i64::from(H))
        .flat_map(|y| (296..320).map(move |x| (x, y, 200)))
        .collect();
    let scout = [
        scout_canvas_with(&garbage),
        Canvas::new(),
        Canvas::new(),
        Canvas::new(),
    ];
    // (scout_canvas_with tags its cells; a delta is plain cells.)
    let mut delta = Canvas::new();
    for &(x, y, v) in &garbage {
        delta.blit_row(x, y, &[Some(px(PixelLayer::Background(0), v))]);
    }
    let delta = [delta, Canvas::new(), Canvas::new(), Canvas::new()];
    drop(scout);
    assert!(live.merge_unknown_from(1, &delta) > 0);
    let bg = |m: &CanvasModel| m.scene_canvases(1).unwrap()[0].clone();
    assert!(bg(&live).is_scout(300, 0));
    assert_eq!(bg(&live).get(300, 0).map(|p| p.palette_index), Some(200));
    // The live game walks there (the world axis follows steps under half
    // the tilemap): its pixels land on x = 296..328.
    for hofs in [100u16, 200, 296] {
        live.feed(
            &bundle([hofs, 0], |_, _, _| {}),
            id,
            Some(&scrolls(hofs, 0)),
        );
    }
    let after = bg(&live);
    assert!(!after.is_scout(300, 0), "a live frame makes the cell live");
    assert_eq!(
        after.get(300, 0).map(|p| p.palette_index),
        Some(300u16 as u8),
        "with its own pixel, not the scout's"
    );
    // A scout delta offered again changes nothing.
    assert_eq!(live.merge_unknown_from(1, &delta), 0);
    assert_eq!(bg(&live).get(300, 0), after.get(300, 0));
}

#[test]
fn the_scout_reference_is_a_copy_of_the_live_canvases_with_the_trusted_layers() {
    let id = SceneId(1);
    let mut live = CanvasModel::new();
    assert!(live.scout_reference(1).is_none(), "no such scene yet");
    live.feed(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(0, 0)));
    let r = live.scout_reference(1).expect("scene");
    assert_eq!(r.layers[0], live.scene_canvases(1).unwrap()[0]);
    assert!(r.trusted[0]);
    // A copy: later frames do not reach it.
    live.feed(&bundle([100, 0], |_, _, _| {}), id, Some(&scrolls(100, 0)));
    assert!(r.layers[0].get(120, 0).is_none());
}

#[test]
fn peeking_a_delta_leaves_it_to_be_taken_and_taking_empties_it() {
    let mut acc = CanvasModel::new();
    let id = SceneId(1);
    acc.feed(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(0, 0)));
    let peeked = acc.peek_delta(1).expect("scene");
    assert!(peeked[0].get(0, 0).is_some());
    // Looking again gives the same cells, and the next frame adds to them.
    acc.feed(&bundle([100, 0], |_, _, _| {}), id, Some(&scrolls(100, 0)));
    let again = acc.peek_delta(1).unwrap();
    assert!(again[0].get(0, 0).is_some() && again[0].get(100, 0).is_some());
    let taken = acc.take_delta(1).unwrap();
    assert_eq!(taken[0], again[0]);
    assert!(acc.peek_delta(1).unwrap()[0].get(0, 0).is_none(), "taken");
}

/// A canvas gone stale whose candidate was learned from the FIRST frames of a
/// new picture (a fade-in) used to stay stale for good: gating withheld every
/// row, so the candidate never saw the settled picture and nothing promoted
/// (SMB3 at a level start: layer 0 at confidence 0, scouts merged nothing).
#[test]
fn a_candidate_learned_from_a_fade_is_restarted_and_the_settled_picture_replaces_the_canvas() {
    use crate::canvas::registration::{guard_rows, Confidence, STALE_FRAMES};
    let (w, h) = (64usize, 32usize);
    let frame = |idx: u8| -> Vec<PpuPixel> {
        (0..w * h)
            .map(|i| px(PixelLayer::Background(0), idx.wrapping_add((i % 7) as u8)))
            .collect()
    };
    let rows: Vec<Option<RowPos>> = (0..h).map(|y| Some(RowPos { x: 0, y: y as i64 })).collect();
    let (mut canvas, mut cand, mut conf) = (Canvas::new(), Canvas::new(), Confidence::default());
    let step = |canvas: &mut Canvas, cand: &mut Canvas, conf: &mut Confidence, v: &[PpuPixel]| {
        let mut r = rows.clone();
        guard_rows(canvas, cand, conf, v, w, &mut r, &Pick::Level);
        place_rows(canvas, v, w, &r, &Pick::Level, &mut None);
    };
    // The old picture, then a fade frame, then the settled picture.
    let (old, fade, settled) = (frame(10), frame(100), frame(200));
    step(&mut canvas, &mut cand, &mut conf, &old);
    step(&mut canvas, &mut cand, &mut conf, &fade);
    for _ in 0..STALE_FRAMES + 8 {
        step(&mut canvas, &mut cand, &mut conf, &settled);
    }
    assert_eq!(
        conf.resets, 1,
        "the settled picture replaced the old canvas"
    );
    assert_eq!(conf.level(), MAX_LEVEL);
    assert_eq!(canvas.get(3, 0).map(|p| p.palette_index), Some(203));
}
