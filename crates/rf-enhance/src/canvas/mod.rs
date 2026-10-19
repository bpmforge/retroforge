//! One canvas model for both consoles (ticket W33-03).
//!
//! The widescreen margins show what the game has drawn beyond the 4:3
//! picture. This module remembers it: per scene, up to four LAYER canvases of
//! palette-indexed cells ([`crate::stitcher::Canvas`], `None` = never seen),
//! plus the palettes the cells were seen under. Presentation only: it reads
//! finished `FrameBundle`s and never touches a core (law 4).
//!
//! ```text
//! FrameBundle ──► placement ──► SceneCanvases ──► LayerCanvases (UI snapshot)
//!   (+ feed)     (ONE fn)       per scene            │
//!                                                    ▼
//!                                          compose::margin_pixel
//! ```
//!
//! * **NES = layer 0.** The core's scroll events become bands
//!   ([`placement::band_rows`]) and the whole level picture is layer 0.
//! * **SNES = one canvas per background layer**, each placed by its own
//!   per-line scroll registers, so parallax and HDMA bands land at the world x
//!   they were drawn at.
//! * **One placement function** ([`place_rows`]) writes both; the feed
//!   ([`FrameFeed`]) only says where the rows go.
//!
//! ## Conventions
//!
//! * Bundle column `c` of row `sy` lands on world `(world_x + c, world_y +
//!   sy)` and the 256-dot picture's left edge is `world_x + pad`
//!   ([`LayerFrames`]).
//! * Positions come from the scroll registers, wrapped to the tilemap and
//!   accumulated with [`crate::scroll_tracker::WorldAxis`]. When the bundle
//!   carries the registers each line was drawn with (`FrameBundle::lines`,
//!   widescreen on), every picture row of every layer has its OWN x axis.
//!   Without line data the per-frame registers are used, through
//!   [`ScrollLag`]: the registers read after a frame draw the NEXT one.
//! * Every background layer that is on the main or sub screen gets a canvas,
//!   whatever the widescreen policy says (the policy is about what the PPU
//!   draws, the canvas is about what we remember). A HUD layer keeps its
//!   canvas but is left out of the margins ([`LayerCanvases::hud`]).
//! * Scenes: [`crate::scene_tracker::SceneTracker`] keeps scene identity;
//!   each scene has its own canvases. A scene is persisted whole (every
//!   layer, palettes, confidence, edges) through `rf-cache`
//!   ([`crate::persistence`], W33-05): restored lazily the first time the
//!   scene is stitched, written when the scene is left, on
//!   [`CanvasModel::flush`], and when the model is dropped.
//! * **The level scout (W32-07, W32-09).** A second machine's frames are
//!   stitched into a forked model ([`CanvasModel::fork_for_scout`]) whose
//!   cells fill only the UNKNOWN cells of the live canvases
//!   ([`CanvasModel::merge_unknown_from`]): live always wins.
use std::borrow::Cow;
use std::collections::HashMap;

use rf_cache::Cache;
use rf_core_api::{CoreEvent, FrameBundle, PixelLayer};

use crate::persistence::{load_scene, save_scene, Saved, SceneView, ScoutMemo};
use crate::scene_identity::SceneId;
use crate::scene_tracker::{SceneEdge, SceneHints, SceneTracker, SETTLING};
use crate::scroll_tracker::{BandWorldPos, ScanlineBand, ScrollTracker, WorldAxis};
use crate::stitcher::Canvas;

mod compose;
mod hud;
mod merge;
mod palette_table;
mod placement;
mod registration;

pub use compose::{
    backdrop_color, backdrop_rows, front_layer_pixel, front_layer_pixel_in, margin_color,
    margin_known, margin_palette_index, margin_pixel, RowFacts,
};
pub use merge::{
    crop_into, merge_unknown, merge_unknown_in, rect_union, scout_agreement,
    scout_agreement_aligned, scout_verdict, Alignment, Rect, ScoutVerdict, MAX_ALIGN_ROWS,
    MIN_SHARED_CELLS,
};
pub use palette_table::{tag, Cgram, PaletteTable, LINE_PALETTE_ENTRIES, MAX_PALETTES};
pub use placement::{
    band_rows, place_rows, scrolls_for, FrameScrolls, LayerFrames, LayerScroll, Pick, RowPos,
    ScrollLag,
};

pub use registration::{
    gate_rows, guard_rows, Check, Confidence, AGREE_PERCENT, BAD_FRAMES_PER_STEP,
    GOOD_FRAMES_PER_STEP, MAX_LEVEL, MAX_OFFSET, MIN_EVIDENCE, STALE_FRAMES,
};

use hud::{hud_layers, row_orders, Motion};
use placement::LayerAxes;

/// How many background layers the SNES has (and canvases a scene holds).
pub const LAYERS: usize = 4;

/// What to place a frame by.
#[derive(Debug, Clone, Copy)]
pub enum Source<'a> {
    /// NES: the bundle's own `ScrollWrite`/`Scanline` events, as bands, into
    /// layer 0.
    Bands,
    /// SNES: each layer's scroll registers. `scrolls` is `None` while there
    /// is nothing to place by (widescreen off, or the previous frame's
    /// registers are unknown): the scene is tracked, no cell is written.
    /// `scene_scroll` is what the scene tracker is fed instead of the core's
    /// own scroll events ([`FrameScrolls::scene`]).
    Registers {
        scrolls: Option<&'a [Option<LayerScroll>; LAYERS]>,
        scene_scroll: Option<(u16, u16)>,
    },
}

/// One frame's inputs beyond the bundle.
#[derive(Debug, Clone, Copy)]
pub struct FrameFeed<'a> {
    pub source: Source<'a>,
    /// The palette words each picture row was drawn with (SNES); `&[]` for
    /// none. Cells are tagged with the id of their row's palette
    /// ([`PaletteTable`]).
    pub palettes: &'a [Option<&'a Cgram>],
    /// Mapper bank state for scene identity (`&[]` for NROM / no banking;
    /// `rf-nes` exposes none yet, the gap `scene_tracker` already names).
    pub mapper_state: &'a [u8],
    /// Ticket W33-09: what the profile says about the scene (the bytes of its
    /// `[[scene.level_id]]` rows, whether to cut sections by scroll axis).
    pub scene: SceneHints<'a>,
}

impl<'a> FrameFeed<'a> {
    /// A NES frame: placed by bands.
    #[must_use]
    pub fn bands() -> Self {
        Self {
            source: Source::Bands,
            palettes: &[],
            mapper_state: &[],
            scene: SceneHints {
                level: None,
                axis_sections: None,
            },
        }
    }

    /// A SNES frame placed by `scrolls` (`None` = track the scene only).
    #[must_use]
    pub fn registers(scrolls: Option<&'a [Option<LayerScroll>; LAYERS]>) -> Self {
        Self {
            source: Source::Registers {
                scrolls,
                scene_scroll: None,
            },
            palettes: &[],
            mapper_state: &[],
            scene: SceneHints {
                level: None,
                axis_sections: None,
            },
        }
    }

    /// The same feed with the profile's scene hints (ticket W33-09).
    #[must_use]
    pub fn with_scene_hints(mut self, scene: SceneHints<'a>) -> Self {
        self.scene = scene;
        self
    }

    /// The same feed with the scene tracker's scroll.
    #[must_use]
    pub fn with_scene_scroll(mut self, scroll: Option<(u16, u16)>) -> Self {
        if let Source::Registers { scene_scroll, .. } = &mut self.source {
            *scene_scroll = scroll;
        }
        self
    }

    /// The same feed with the per-row palettes.
    #[must_use]
    pub fn with_palettes(mut self, palettes: &'a [Option<&'a Cgram>]) -> Self {
        self.palettes = palettes;
        self
    }

    fn is_bands(&self) -> bool {
        matches!(self.source, Source::Bands)
    }

    fn scene_scroll(&self) -> Option<(u16, u16)> {
        match self.source {
            Source::Registers { scene_scroll, .. } => scene_scroll,
            Source::Bands => None,
        }
    }
}

/// One scene's canvases: a canvas per layer, the palettes the cells were seen
/// under, and the evidence the HUD classifier judges by. An unused layer's
/// canvas is empty (`width() == 0`).
#[derive(Default)]
pub struct SceneCanvases {
    pub layers: [Canvas; LAYERS],
    pub palettes: PaletteTable,
    /// Ticket W33-04: per layer, how far the registers' placement is trusted.
    pub confidence: [Confidence; LAYERS],
    /// What frames the canvas disagreed with entirely looked like
    /// ([`guard_rows`]).
    candidates: [Canvas; LAYERS],
    motion: [Motion; LAYERS],
    /// Ticket W33-05: a scout has run every pass on this scene (restored from
    /// the cache too): the next visit skips it.
    scouted: bool,
    /// Ticket W33-06: the ladder rung that scout finished on.
    scout_memo: Option<ScoutMemo>,
    /// Stitched or merged since the last write.
    unsaved: bool,
    /// Ticket W34-07: scout deltas merged into this scene so far (consumers
    /// that derive from the canvas look again when it changes).
    merges: u64,
    /// Restored from the cache when first stitched.
    restored: bool,
    /// CRC32 of the body on disk, so an unchanged scene is not rewritten.
    on_disk_crc: Option<u32>,
    /// What the last write stored (the size report).
    saved: Option<Saved>,
}

/// Ticket W33-06: what the scout's agreement check compares a delta with: the
/// live canvases of the scene as they stand when the scout starts (a copy, so
/// the worker never touches the live model), and which layers are worth
/// comparing with (those whose placement is fully trusted, the only ones a
/// scout merge may fill).
#[derive(Debug, Clone, Default)]
pub struct ScoutReference {
    pub layers: [Canvas; LAYERS],
    pub trusted: [bool; LAYERS],
}

/// A scene's per-layer canvases, shipped to the UI thread. `None` = the
/// layer has no canvas (never drawn, or nothing stitched yet).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayerCanvases {
    pub canvases: [Option<Canvas>; LAYERS],
    /// Layers that are HUD ([`CanvasModel::hud_layers`]): they keep their
    /// canvas but give the margins nothing.
    pub hud: [bool; LAYERS],
    /// Ticket W33-02: the palettes the cells were seen under; a cell's
    /// `sprite_id` is its id here ([`PaletteTable`]).
    pub palettes: PaletteTable,
    /// Ticket W33-04: layers whose placement confidence fell below 2: they
    /// keep their canvas but the margins take nothing from them (a low score
    /// means less margin, never a wrong one).
    pub untrusted: [bool; LAYERS],
}

impl LayerCanvases {
    /// Whether any layer has content to take a margin pixel from.
    #[must_use]
    pub fn any(&self) -> bool {
        self.canvases.iter().any(Option::is_some)
    }

    /// The canvas the map view shows: the first layer that is not HUD and
    /// has content (layer 0 on the NES, whose whole picture is layer 0; on
    /// the SNES the first background layer, the level's own). The one adapter
    /// between this model and a consumer that wants a single canvas; layers
    /// are not merged, because parallax layers do not share world
    /// coordinates.
    #[must_use]
    pub fn map_canvas(&self) -> Option<&Canvas> {
        self.primary_layer().and_then(|n| self.canvases[n].as_ref())
    }

    /// The layer [`Self::map_canvas`] shows, and whose confidence decides
    /// the margin source.
    #[must_use]
    pub fn primary_layer(&self) -> Option<usize> {
        self.canvases
            .iter()
            .zip(self.hud)
            .position(|(c, hud)| c.is_some() && !hud)
    }

    /// Whether the margins may take cells from layer `n`.
    #[must_use]
    pub fn usable(&self, n: usize) -> bool {
        !self.hud.get(n).copied().unwrap_or(true) && !self.untrusted.get(n).copied().unwrap_or(true)
    }
}

/// The scene-tracker's events: the core's own, or, for a core whose scroll
/// events do not describe the level layer (the SNES, ticket W30-03: its one
/// `ScrollWrite` per frame is not the scrolling background's), the scrolling
/// layer's registers as one band over the visible rows. The core's own
/// `Scanline` run is not usable as is: it also carries a stray row from the
/// neighbouring frame, which closes the band at that row's number and leaves
/// it empty.
fn scene_events(bundle: &FrameBundle, scroll: Option<(u16, u16)>) -> Cow<'_, [CoreEvent]> {
    let Some((x, y)) = scroll else {
        return Cow::Borrowed(&bundle.events);
    };
    let mut rows = bundle.events.iter().filter_map(|e| match e {
        CoreEvent::Scanline(sy) if *sy < bundle.height => Some(*sy),
        _ => None,
    });
    let first = rows.next();
    let (lo, hi) = rows.fold((first, first), |(lo, hi), sy| {
        (lo.map(|l| l.min(sy)), hi.map(|h| h.max(sy)))
    });
    Cow::Owned(
        std::iter::once(CoreEvent::ScrollWrite {
            x,
            y,
            layer: PixelLayer::Background(0),
        })
        .chain(lo.map(CoreEvent::Scanline))
        .chain(hi.map(CoreEvent::Scanline))
        .chain(
            bundle
                .events
                .iter()
                .filter(|e| !matches!(e, CoreEvent::ScrollWrite { .. } | CoreEvent::Scanline(_)))
                .cloned(),
        )
        .collect(),
    )
}

/// Stateful per-ROM canvas model; driven on the core thread for EVERY frame
/// the core produces, in order, with no gaps: `FrameBundle::events` carries
/// only that frame's scroll events, so a skipped frame's data is gone, and a
/// dropped frame would show up as a spurious scene cut and holes in the
/// canvas. (The UI thread's latest-wins bundle reader is therefore the wrong
/// place to drive it.)
pub struct CanvasModel {
    /// NES: bands from the core's own scroll events.
    scroll: ScrollTracker,
    scene_tracker: SceneTracker,
    /// SNES: per layer world axes.
    axes: [LayerAxes; LAYERS],
    scenes: HashMap<u64, SceneCanvases>,
    last: Option<LayerFrames>,
    last_scene: Option<SceneId>,
    /// Per layer, the bounding box of the cells written since
    /// [`Self::take_dirty`] (world coordinates).
    dirty: [Option<Rect>; LAYERS],
    /// `None` when no cache root was configured or opening it failed:
    /// persistence is strictly additive, never a reason to fail.
    cache: Option<Cache>,
    rom_sha256: String,
    /// The scene whose cells were last stitched (not [`SETTLING`]): the one
    /// written when the model moves on.
    resident: Option<u64>,
    /// How many of the scene tracker's edges were seen, to find new ones.
    edges_seen: usize,
    /// Ticket W33-04: check frames against the canvas before writing them.
    /// A scout's fork does not: its machine is the scout's own, its cells are
    /// scored against the live canvas where they meet it (W33-06), and a
    /// fork starts with no canvas to check against.
    guard: bool,
    /// Ticket W34-07: blobs other producers (the height learner) asked to be
    /// stored beside the scenes, written by the next [`Self::flush`] (and by
    /// the drop that follows the model's last one). Newest wins per key.
    extras: Vec<(rf_cache::CacheKey, Vec<u8>)>,
}

impl Default for CanvasModel {
    fn default() -> Self {
        Self::with_cache(String::new(), None)
    }
}

impl CanvasModel {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_cache(rom_sha256: String, cache: Option<Cache>) -> Self {
        Self {
            scroll: ScrollTracker::new(),
            scene_tracker: SceneTracker::new(),
            axes: Default::default(),
            scenes: HashMap::new(),
            last: None,
            last_scene: None,
            dirty: [None; LAYERS],
            cache,
            rom_sha256,
            resident: None,
            edges_seen: 0,
            guard: true,
            extras: Vec::new(),
        }
    }

    /// Feed one frame, tracking the scene. Returns the scene it was
    /// attributed to.
    pub fn observe_frame(&mut self, bundle: &FrameBundle, feed: &FrameFeed<'_>) -> SceneId {
        let events = scene_events(bundle, feed.scene_scroll());
        let bands = if feed.is_bands() {
            self.scroll.observe_frame(&events)
        } else {
            Vec::new()
        };
        let scene = self.scene_tracker.observe(
            &bundle.video,
            bundle.width,
            bundle.height,
            &events,
            feed.mapper_state,
            feed.scene,
        );
        self.note_new_edges();
        self.stitch(bundle, scene, feed, &bands);
        scene
    }

    /// Edges the tracker recorded since the last frame touch two scenes: both
    /// are written again so a later visit knows the neighbour.
    fn note_new_edges(&mut self) {
        let edges = self.scene_tracker.edges();
        for e in edges.iter().skip(self.edges_seen) {
            for id in [e.from, e.to] {
                if let Some(s) = self.scenes.get_mut(&id) {
                    s.unsaved = true;
                }
            }
        }
        self.edges_seen = edges.len();
    }

    /// Feed one frame into a scene the caller already knows (a scout's
    /// machine belongs to the scene it was started for, whatever its own
    /// scroll events say).
    pub fn observe_frame_in(&mut self, bundle: &FrameBundle, scene: SceneId, feed: &FrameFeed<'_>) {
        let bands = if feed.is_bands() {
            self.scroll.observe_frame(&bundle.events)
        } else {
            Vec::new()
        };
        self.stitch(bundle, scene, feed, &bands);
    }

    fn stitch(
        &mut self,
        bundle: &FrameBundle,
        scene: SceneId,
        feed: &FrameFeed<'_>,
        bands: &[(ScanlineBand, BandWorldPos)],
    ) {
        self.last = None;
        self.last_scene = Some(scene);
        if scene == SETTLING {
            // A cut that has not settled (a fade, a level still loading) is
            // no scene's picture: nothing is stitched, so the scene that was
            // left, and the one that comes, keep exactly what they had. The
            // scene that was left is written now, while the game loads.
            self.flush();
            return;
        }
        if self.resident != Some(scene.0) {
            self.flush();
            self.resident = Some(scene.0);
        }
        if !self.scenes.contains_key(&scene.0) {
            let set = self.restore(scene);
            self.scenes.insert(scene.0, set);
        }
        let width = usize::from(bundle.width);
        if width == 0 {
            return;
        }
        let Self {
            axes,
            scenes,
            dirty,
            last,
            guard,
            ..
        } = self;
        let Some(set) = scenes.get_mut(&scene.0) else {
            return;
        };
        set.unsaved = true;
        match feed.source {
            Source::Bands => {
                // The NES: the whole picture is layer 0, placed by bands.
                let mut rows = band_rows(bands, bundle.video.len() / width);
                if *guard {
                    guard_rows(
                        &mut set.layers[0],
                        &mut set.candidates[0],
                        &mut set.confidence[0],
                        &bundle.video,
                        width,
                        &mut rows,
                        &Pick::Level,
                    );
                }
                place_rows(
                    &mut set.layers[0],
                    &bundle.video,
                    width,
                    &rows,
                    &Pick::Level,
                    &mut dirty[0],
                );
                // The largest band carries the level (the scroll tracker's
                // own primary, whose world position it accumulates). Not
                // filtered by `is_hud`: the tracker marks ANY band whose
                // scroll did not change as HUD, the level band included, so
                // a camera standing still lost its position and widescreen
                // margins fell back to VRAM until it moved again (fix
                // 2026-10-16).
                let pad = i64::from(bundle.width.saturating_sub(256) / 2);
                *last = bands
                    .iter()
                    .filter(|(band, _)| band.end > band.start)
                    .max_by_key(|(band, _)| band.end - band.start)
                    .map(|(band, pos)| LayerFrames {
                        scene: scene.0,
                        origins: [
                            Some((
                                pos.world_x + pad,
                                pos.world_y_at_start - i64::from(band.start),
                            )),
                            None,
                            None,
                            None,
                        ],
                        row_dx: Vec::new(),
                        confidence: set.confidence,
                    });
            }
            Source::Registers {
                scrolls: Some(scrolls),
                ..
            } => {
                *last = Some(stitch_registers(
                    set,
                    axes,
                    dirty,
                    bundle,
                    scene,
                    scrolls,
                    feed.palettes,
                    *guard,
                ));
            }
            Source::Registers { scrolls: None, .. } => {}
        }
    }

    /// The scene of the last frame observed ([`SETTLING`] while a cut has
    /// not settled: no canvases then).
    #[must_use]
    pub fn current_scene(&self) -> Option<SceneId> {
        self.last_scene
    }

    /// Ticket W33-08: whether the current section scrolls vertically
    /// ([`SceneTracker::scrolls_vertically`]); `None` when it is not known.
    #[must_use]
    pub fn scene_scrolls_vertically(&self) -> Option<bool> {
        self.scene_tracker.scrolls_vertically()
    }

    /// Ticket W33-09: every scene change seen this session, with the scroll
    /// position it happened at (`SceneTracker::edges`).
    #[must_use]
    pub fn scene_edges(&self) -> &[SceneEdge] {
        self.scene_tracker.edges()
    }

    /// Ticket W33-09: the scenes a margin of `scene` may show past the edge
    /// of its canvas: across CONTINUOUS edges only. A door, a pipe or a fade
    /// never joins two scenes. (The margins compose from the current scene's
    /// canvas alone today; this is the one rule any neighbour fill must go
    /// through.)
    #[must_use]
    pub fn margin_neighbours(&self, scene: SceneId) -> Vec<SceneEdge> {
        self.scene_tracker.continuous_neighbours(scene)
    }

    /// Where the latest frame sits on each layer's canvas.
    #[must_use]
    pub fn frames(&self) -> Option<LayerFrames> {
        self.last.clone()
    }

    /// Which layers of `scene` are HUD ([`hud`] module doc).
    #[must_use]
    pub fn hud_layers(&self, scene: u64) -> [bool; LAYERS] {
        self.scenes
            .get(&scene)
            .map_or([false; LAYERS], |s| hud_layers(&s.motion))
    }

    /// Ticket W32-08: for each picture row of the frame just observed, the
    /// layers front to back, by the picture's own evidence. Empty before a
    /// frame has been placed per layer.
    #[must_use]
    pub fn row_orders(&self, bundle: &FrameBundle) -> Vec<[u8; LAYERS]> {
        let Some(frames) = self.last.as_ref() else {
            return Vec::new();
        };
        let Some(set) = self.scenes.get(&frames.scene) else {
            return Vec::new();
        };
        row_orders(bundle, frames, &set.layers, self.hud_layers(frames.scene))
    }

    /// Ticket W33-06: a copy of `scene`'s canvases for the scout's agreement
    /// check (`None` for a scene with none yet).
    #[must_use]
    pub fn scout_reference(&self, scene: u64) -> Option<ScoutReference> {
        let set = self.scenes.get(&scene)?;
        Some(ScoutReference {
            layers: set.layers.clone(),
            trusted: set.confidence.map(|c| c.level() >= MAX_LEVEL),
        })
    }

    /// Put a scene's canvases and palettes in as if stitched (tests of the
    /// consumers of a scene, which cannot build a whole play session).
    #[doc(hidden)]
    pub fn seed_scene_for_test(
        &mut self,
        scene: u64,
        layers: [Canvas; LAYERS],
        palettes: PaletteTable,
    ) {
        let set = self.scenes.entry(scene).or_default();
        set.layers = layers;
        set.palettes = palettes;
        set.unsaved = true;
    }

    /// How many scout deltas have been merged into `scene` (W34-07).
    #[must_use]
    pub fn scene_merges(&self, scene: u64) -> u64 {
        self.scenes.get(&scene).map_or(0, |s| s.merges)
    }

    /// The palettes `scene`'s cells were seen under (W33-02), borrowed.
    #[must_use]
    pub fn scene_palettes(&self, scene: u64) -> Option<&PaletteTable> {
        self.scenes.get(&scene).map(|s| &s.palettes)
    }

    /// The per-layer canvases stitched for `scene`, borrowed.
    #[must_use]
    pub fn scene_canvases(&self, scene: u64) -> Option<&[Canvas; LAYERS]> {
        self.scenes.get(&scene).map(|s| &s.layers)
    }

    /// A clone of the current scene's non-empty layer canvases, for the UI
    /// snapshot (pull, throttled: a clone is O(canvas size), tens of MB/s at
    /// 60 Hz for a level of any real size).
    #[must_use]
    pub fn current_canvases(&self) -> LayerCanvases {
        let mut out = LayerCanvases::default();
        let Some(scene) = self.last_scene.map(|s| s.0) else {
            return out;
        };
        if let Some(set) = self.scenes.get(&scene) {
            out.hud = hud_layers(&set.motion);
            out.palettes = set.palettes.clone();
            out.untrusted = set.confidence.map(|c| c.level() < 2);
            for (slot, canvas) in out.canvases.iter_mut().zip(&set.layers) {
                if canvas.width() > 0 && canvas.height() > 0 {
                    *slot = Some(canvas.clone());
                }
            }
        }
        out
    }

    /// The scene from the cache (lazily, the first time it is stitched), or an
    /// empty one. A file that does not decode was already ignored with a log
    /// line ([`load_scene`]); a cache fault reads as a miss too.
    fn restore(&mut self, scene: SceneId) -> SceneCanvases {
        let mut set = SceneCanvases::default();
        let loaded = self
            .cache
            .as_mut()
            .and_then(|cache| load_scene(cache, &self.rom_sha256, scene).unwrap_or(None));
        if let Some(data) = loaded {
            set.layers = data.layers;
            set.palettes = data.palettes;
            set.confidence = data.confidence;
            set.scouted = data.scouted;
            set.scout_memo = data.scout_memo;
            set.restored = true;
            set.on_disk_crc = Some(data.body_crc);
            self.scene_tracker.restore_edges(&data.edges);
            self.edges_seen = self.scene_tracker.edges().len();
        }
        set
    }

    /// Ticket W34-07: the stored blob for `key`, read through this model's
    /// cache (`None` for a miss, no cache, or a cache fault). The model owns
    /// the one `Cache` of the session, so other producers go through it
    /// rather than opening a second index over the same root.
    pub fn fetch_extra(&mut self, key: &rf_cache::CacheKey) -> Option<Vec<u8>> {
        self.cache.as_mut()?.get(key).ok().flatten()
    }

    /// Ticket W34-07: have `bytes` stored under `key` by the next
    /// [`Self::flush`]. A later call for the same key replaces this one.
    pub fn queue_extra(&mut self, key: rf_cache::CacheKey, bytes: Vec<u8>) {
        if let Some(slot) = self.extras.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = bytes;
        } else {
            self.extras.push((key, bytes));
        }
    }

    /// Persist every scene stitched or merged into since it was last written
    /// (the current one, and one just left): the whole scene through
    /// `rf-cache`, atomically. A scene whose body is unchanged writes
    /// nothing. Best-effort: a failure is logged and degrades to "not cached
    /// this time", never a crash. Returns the bytes stored by this call.
    pub fn flush(&mut self) -> usize {
        let Some(cache) = self.cache.as_mut() else {
            self.extras.clear();
            return 0;
        };
        for (key, bytes) in std::mem::take(&mut self.extras) {
            if let Err(e) = cache.put(&key, &bytes) {
                eprintln!("rf-enhance: cache: extra entry not saved: {e}");
            }
        }
        // Ascending ids: the same order every run.
        let mut ids: Vec<u64> = self
            .scenes
            .iter()
            .filter(|(_, s)| s.unsaved)
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        let mut stored = 0;
        for id in ids {
            let edges: Vec<SceneEdge> = self
                .scene_tracker
                .edges()
                .iter()
                .filter(|e| e.from == id || e.to == id)
                .copied()
                .collect();
            let Some(set) = self.scenes.get_mut(&id) else {
                continue;
            };
            set.unsaved = false;
            if set.layers.iter().all(|c| c.width() == 0 || c.height() == 0) {
                continue;
            }
            let view = SceneView {
                layers: &set.layers,
                palettes: &set.palettes,
                confidence: &set.confidence,
                edges: &edges,
                scouted: set.scouted,
                scout_memo: set.scout_memo,
            };
            match save_scene(cache, &self.rom_sha256, SceneId(id), &view, set.on_disk_crc) {
                Ok(saved) => {
                    set.on_disk_crc = Some(saved.body_crc);
                    if !saved.unchanged {
                        stored += saved.stored_bytes;
                        eprintln!(
                            "rf-enhance: canvas cache: scene {:016x}: {} bytes stored ({} raw)",
                            id, saved.stored_bytes, saved.raw_bytes
                        );
                        set.saved = Some(saved);
                    }
                }
                Err(e) => eprintln!("rf-enhance: canvas cache: scene {id:016x} not saved: {e}"),
            }
        }
        stored
    }

    /// Whether `scene` was restored from the cache when it was first stitched.
    #[must_use]
    pub fn scene_restored(&self, scene: u64) -> bool {
        self.scenes.get(&scene).is_some_and(|s| s.restored)
    }

    /// Ticket W33-05: a scout has run every pass on `scene` (this session or
    /// a restored one): the scout driver skips it.
    #[must_use]
    pub fn scene_scouted(&self, scene: u64) -> bool {
        self.scenes.get(&scene).is_some_and(|s| s.scouted)
    }

    /// Record that a scout ran every pass on `scene` and write it now, so a
    /// quit before the next flush does not lose the fact.
    pub fn mark_scouted(&mut self, scene: u64) {
        self.mark_scouted_with(scene, None);
    }

    /// [`Self::mark_scouted`], remembering the ladder rung the scout finished
    /// on (W33-06); a later scout of the scene starts there.
    pub fn mark_scouted_with(&mut self, scene: u64, memo: Option<ScoutMemo>) {
        if let Some(s) = self.scenes.get_mut(&scene) {
            s.scouted = true;
            if memo.is_some() {
                s.scout_memo = memo;
            }
            s.unsaved = true;
        }
        self.flush();
    }

    /// The rung `scene`'s last scout finished on, if one did (W33-06).
    #[must_use]
    pub fn scene_scout_memo(&self, scene: u64) -> Option<ScoutMemo> {
        self.scenes.get(&scene).and_then(|s| s.scout_memo)
    }

    /// What the last write of `scene` stored (the size report), if it wrote.
    #[must_use]
    pub fn scene_saved(&self, scene: u64) -> Option<Saved> {
        self.scenes.get(&scene).and_then(|s| s.saved)
    }

    // ---- the level scout (W32-07, W32-09)

    /// A model that CONTINUES this one's world axes and scene tracking (so a
    /// second machine restored from a snapshot of the live one is placed on
    /// the same world coordinates, and attributed to the same scene) but
    /// holds no canvases and no cache: what a scout stitches into before its
    /// cells are merged back with [`Self::merge_unknown_from`].
    #[must_use]
    pub fn fork_for_scout(&self) -> Self {
        Self {
            scroll: self.scroll.clone(),
            scene_tracker: self.scene_tracker.clone(),
            axes: self.axes.clone(),
            scenes: HashMap::new(),
            last: None,
            last_scene: self.last_scene,
            dirty: [None; LAYERS],
            cache: None,
            rom_sha256: String::new(),
            resident: None,
            edges_seen: 0,
            guard: false,
            extras: Vec::new(),
        }
    }

    /// Put the trackers and axes back to `seed`'s (a [`Self::fork_for_scout`]
    /// of the live model) and forget the last frame's positions; the canvases
    /// stitched so far are kept. A scout's next pass restarts from the
    /// snapshot, so its positions restart from the seed.
    pub fn rewind_to(&mut self, seed: &Self) {
        self.scroll = seed.scroll.clone();
        self.scene_tracker = seed.scene_tracker.clone();
        self.axes = seed.axes.clone();
        self.last_scene = seed.last_scene;
        self.last = None;
    }

    /// The bounding box of what was stitched per layer since the last call.
    pub fn take_dirty(&mut self) -> [Option<Rect>; LAYERS] {
        std::mem::take(&mut self.dirty)
    }

    /// Ticket W32-08: what was stitched since the last call, cut out of the
    /// canvases of `scene` into canvases of their own (the dirty boxes'
    /// cells only), for a worker to ship to the thread that merges them
    /// ([`Self::merge_unknown_from`]).
    pub fn take_delta(&mut self, scene: u64) -> Option<[Canvas; LAYERS]> {
        let dirty = self.take_dirty();
        self.delta_of(scene, &dirty)
    }

    /// As [`Self::take_delta`] without forgetting what was stitched: the same
    /// cells (and more, next time) come again until [`Self::take_dirty`] or
    /// `take_delta` consumes them. The scout worker holds a delta this way
    /// while the agreement check has too few shared cells to judge it
    /// (W33-06).
    #[must_use]
    pub fn peek_delta(&self, scene: u64) -> Option<[Canvas; LAYERS]> {
        self.delta_of(scene, &self.dirty)
    }

    fn delta_of(&self, scene: u64, dirty: &[Option<Rect>; LAYERS]) -> Option<[Canvas; LAYERS]> {
        let set = self.scenes.get(&scene)?;
        let mut out: [Canvas; LAYERS] = std::array::from_fn(|_| Canvas::new());
        for ((from, rect), to) in set.layers.iter().zip(dirty).zip(out.iter_mut()) {
            if let Some(rect) = *rect {
                crop_into(to, from, rect);
            }
        }
        Some(out)
    }

    /// As [`Self::merge_unknown_from`], looking only inside `within` per
    /// layer (a scout's [`Self::take_dirty`]): the cost of a merge follows
    /// what changed, not the size of the canvases.
    pub fn merge_unknown_within(
        &mut self,
        scene: u64,
        from: &[Canvas; LAYERS],
        within: &[Option<Rect>; LAYERS],
    ) -> usize {
        let Some(live) = self.scenes.get_mut(&scene) else {
            return 0;
        };
        live.unsaved = true;
        live.merges += 1;
        let SceneCanvases {
            layers, confidence, ..
        } = live;
        // Ticket W33-04: scout cells only join a layer whose placement is
        // fully trusted (confidence 3 = history AND scout).
        layers
            .iter_mut()
            .zip(confidence.iter())
            .zip(from)
            .zip(within)
            .filter(|(((_, conf), _), _)| conf.level() >= MAX_LEVEL)
            .map(|(((live, _), scout), rect)| {
                rect.map_or(0, |r| merge_unknown_in(live, scout, Some(r)))
            })
            .sum()
    }

    /// Fill the cells of `scene`'s canvases that are still UNKNOWN from
    /// `from` (a scout's canvases); a cell the live game has seen is never
    /// touched, so live always wins. A scene with no live canvas yet takes
    /// nothing (the scout belongs to a scene the live game has not been in: a
    /// stale scout). Returns how many cells were filled.
    pub fn merge_unknown_from(&mut self, scene: u64, from: &[Canvas; LAYERS]) -> usize {
        let Some(live) = self.scenes.get_mut(&scene) else {
            return 0;
        };
        live.unsaved = true;
        live.merges += 1;
        let SceneCanvases {
            layers, confidence, ..
        } = live;
        layers
            .iter_mut()
            .zip(confidence.iter())
            .zip(from)
            .filter(|((_, conf), _)| conf.level() >= MAX_LEVEL)
            .map(|((live, _), scout)| merge_unknown(live, scout))
            .sum()
    }
}

impl Drop for CanvasModel {
    /// The model goes with its core thread: what was stitched since the last
    /// flush is written (a model with no cache, a scout's fork, writes
    /// nothing).
    fn drop(&mut self) {
        self.flush();
    }
}

/// The SNES half of placement: each layer's rows from its own registers (one
/// world-x axis per picture row once line registers have been seen, so a
/// band keeps its world x across frames), written by [`place_rows`]. Returns
/// where each layer's picture sits.
#[allow(clippy::too_many_arguments)] // the model's disjoint fields, borrowed apart
fn stitch_registers(
    set: &mut SceneCanvases,
    axes: &mut [LayerAxes; LAYERS],
    dirty: &mut [Option<Rect>; LAYERS],
    bundle: &FrameBundle,
    scene: SceneId,
    scrolls: &[Option<LayerScroll>; LAYERS],
    line_words: &[Option<&Cgram>],
    guard: bool,
) -> LayerFrames {
    let width = usize::from(bundle.width);
    let height = usize::from(bundle.height).min(bundle.video.len() / width);
    let pad = i64::from(bundle.width.saturating_sub(256) / 2);
    // The registers each line was drawn with, when the bundle has them. Then
    // `scrolls` only says WHICH layers to stitch and at what tilemap size;
    // the positions come from the lines.
    let has_lines = bundle.lines.iter().any(Option::is_some);
    let line0 = bundle.lines.first().copied().flatten();
    let SceneCanvases {
        layers,
        palettes,
        confidence,
        candidates,
        motion,
        ..
    } = set;
    // One interned id per picture row (rows share a palette almost always;
    // `intern` is a hash lookup either way).
    let row_ids: Vec<Option<u8>> = (0..height)
        .map(|sy| {
            line_words
                .get(sy)
                .copied()
                .flatten()
                .and_then(|words| palettes.intern(words))
        })
        .collect();
    let mut origins = [None; LAYERS];
    let mut row_dx: Vec<[i32; LAYERS]> = if has_lines {
        vec![[0; LAYERS]; height]
    } else {
        Vec::new()
    };
    let mut rows: Vec<Option<RowPos>> = Vec::with_capacity(height);
    for (n, scroll) in scrolls.iter().enumerate() {
        let Some(scroll) = scroll else { continue };
        let (tw, th) = (scroll.tilemap_width.max(1), scroll.tilemap_height.max(1));
        let axes = &mut axes[n];
        // The per-frame read keeps its axis current: it is the fallback for
        // rows with no line registers.
        let wx_frame = axes.frame.0.advance(scroll.hofs, i64::from(tw));
        let vofs = line0.map_or(scroll.vofs, |l| l.vofs[n] % th);
        let wy = axes.frame.1.advance(vofs, i64::from(th));
        if has_lines && axes.rows.len() < height {
            axes.rows.resize(height, WorldAxis::new());
        }
        let mut base_x = wx_frame;
        rows.clear();
        // Bounded `for` over the frame's rows (law 8). Every row's own axis
        // advances every frame, drawn or not, so its deltas stay one frame
        // apart.
        for sy in 0..height {
            let wx = match (
                bundle.lines.get(sy).copied().flatten(),
                axes.rows.get_mut(sy),
            ) {
                (Some(l), Some(axis)) => axis.advance(l.hofs[n] % tw, i64::from(tw)),
                _ => wx_frame,
            };
            if sy == 0 {
                base_x = wx;
            }
            if let Some(dx) = row_dx.get_mut(sy) {
                dx[n] = i32::try_from(wx - base_x).unwrap_or(0);
            }
            rows.push(Some(RowPos {
                x: wx,
                y: wy + sy as i64,
            }));
        }
        let pick = Pick::Layer {
            wanted: PixelLayer::Background(n as u8),
            sub: &bundle.sub,
            ids: &row_ids,
        };
        if guard {
            guard_rows(
                &mut layers[n],
                &mut candidates[n],
                &mut confidence[n],
                &bundle.video,
                width,
                &mut rows,
                &pick,
            );
        }
        let drawn = place_rows(
            &mut layers[n],
            &bundle.video,
            width,
            &rows,
            &pick,
            &mut dirty[n],
        );
        motion[n].observe(base_x, wy, drawn, height);
        origins[n] = Some((base_x + pad, wy));
    }
    LayerFrames {
        scene: scene.0,
        origins,
        row_dx,
        confidence: *confidence,
    }
}

#[cfg(test)]
mod tests;
