//! Ticket W38-11 (D-032): the shell half of the presentation-only margins.
//!
//! [`Spec`] reads a recipe's camera rows and object table into plain addresses; [`Tracker`]
//! turns a read-only peek of the running machine into one frame's [`Facts`] (camera, scene key,
//! object slots). The core thread builds the facts and ships them with the frame; the UI thread
//! feeds them to `rf_enhance::presentation_margins`. Nothing here writes to the machine.
//!
//! Camera rows by recipe vocabulary (observed names, not game knowledge): `camera_x` (+
//! `camera_y`), or `scroll_x` + `scroll_page`, or a lone low byte `camera_x_lo` (unwrapped by
//! accumulating its signed change). A jump of more than [`SCENE_JUMP`] dots in a frame starts a
//! new scene (a door, a level change); `level_id` / `room_state` rows also key the scene.

use rf_enhance::presentation_margins::ObjectFact;
use rf_rebuild::{ObjectTable, Recipe};

/// A camera move larger than this in one frame is a scene change, not a scroll.
pub const SCENE_JUMP: i64 = 600;

/// How the horizontal camera is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CamX {
    /// A little-endian word (or byte) at `addr`.
    Word { addr: u32, width: u8 },
    /// Low byte and page byte.
    Paged { lo: u32, page: u32 },
    /// A lone low byte: accumulated.
    Low8 { addr: u32 },
}

/// What a recipe says about the camera and objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    cam_x: CamX,
    cam_y: Option<(u32, u8)>,
    scene: Option<(u32, u8)>,
    table: Option<ObjectTable>,
}

impl Spec {
    /// `None` when the recipe has no usable horizontal camera.
    #[must_use]
    pub fn from_recipe(recipe: &Recipe) -> Option<Self> {
        let row = |n: &str| recipe.ram.iter().find(|r| r.name == n);
        let cam_x = if let Some(r) = row("camera_x") {
            CamX::Word {
                addr: r.addr,
                width: r.width.clamp(1, 2),
            }
        } else if let (Some(lo), Some(page)) = (row("scroll_x"), row("scroll_page")) {
            CamX::Paged {
                lo: lo.addr,
                page: page.addr,
            }
        } else {
            CamX::Low8 {
                addr: row("camera_x_lo")?.addr,
            }
        };
        Some(Self {
            cam_x,
            cam_y: row("camera_y").map(|r| (r.addr, r.width.clamp(1, 2))),
            scene: row("level_id")
                .or_else(|| row("room_state"))
                .map(|r| (r.addr, r.width.clamp(1, 2))),
            table: recipe
                .object_tables
                .iter()
                .find(|t| t.x_lo.is_some() && t.y.is_some() && t.slots > 0 && t.slots <= 64)
                .cloned(),
        })
    }

    /// Whether the recipe has an object table (else objects are last-seen only, and only with a
    /// vertical camera row).
    #[must_use]
    pub fn has_objects(&self) -> bool {
        self.table.is_some()
    }

    /// A 2-D camera: last-seen sprites can be placed.
    #[must_use]
    pub fn has_full_camera(&self) -> bool {
        self.cam_y.is_some()
    }

    /// One line for the UI: what the margins will show for this game.
    #[must_use]
    pub fn describe(&self) -> &'static str {
        match (self.has_objects(), self.has_full_camera()) {
            (true, _) => "level and objects, learned from play",
            (false, true) => "level and last-seen objects, learned from play",
            (false, false) => "level only (no object table in this recipe), learned from play",
        }
    }
}

/// One frame's readings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub cam: (i64, i64),
    pub scene: u64,
    /// `None` = no table in the recipe.
    pub objects: Option<Vec<ObjectFact>>,
    /// Last-seen sprites may be placed (a vertical camera row, or an object table).
    pub full_camera: bool,
}

/// Reads [`Facts`] frame after frame (keeps the unwrapped camera and the scene counter).
#[derive(Debug, Clone)]
pub struct Tracker {
    spec: Spec,
    last: Option<i64>,
    acc: i64,
    bump: u64,
}

impl Tracker {
    #[must_use]
    pub fn new(spec: Spec) -> Self {
        Self {
            spec,
            last: None,
            acc: 0,
            bump: 0,
        }
    }

    /// Read the machine behind `peek` (bus addresses, side-effect free).
    pub fn read(&mut self, peek: &dyn Fn(u32) -> u8) -> Facts {
        let word = |addr: u32, width: u8| -> i64 {
            (0..u32::from(width)).fold(0i64, |v, i| v | i64::from(peek(addr + i)) << (8 * i))
        };
        let raw = match self.spec.cam_x {
            CamX::Word { addr, width } => word(addr, width),
            CamX::Paged { lo, page } => word(page, 1) * 256 + word(lo, 1),
            CamX::Low8 { addr } => {
                let b = i64::from(peek(addr));
                if let Some(prev) = self.last {
                    // Signed 8-bit change; progress is a bounded step, no loop.
                    self.acc += i64::from(((b - prev) as u8) as i8);
                }
                self.last = Some(b);
                self.acc
            }
        };
        if !matches!(self.spec.cam_x, CamX::Low8 { .. }) {
            if self.last.is_some_and(|p| (raw - p).abs() > SCENE_JUMP) {
                self.bump += 1;
            }
            self.last = Some(raw);
        } else if self.last.is_none() {
            self.last = Some(raw);
        }
        let cam_y = self.spec.cam_y.map_or(0, |(a, w)| word(a, w));
        let key = self.spec.scene.map_or(0, |(a, w)| word(a, w) as u64);
        let scene = key.wrapping_mul(1_000_003).wrapping_add(self.bump) + 1;
        let objects = self.spec.table.as_ref().map(|t| {
            let at = |off: Option<u32>, k: u32| off.map(|o| t.base + o + k * t.stride);
            (0..t.slots)
                .map(|k| {
                    let lo = at(t.x_lo, k).map_or(0, |a| i64::from(peek(a)));
                    let hi = at(t.x_hi, k).map_or(0, |a| i64::from(peek(a)));
                    let alive = at(t.state, k).map_or(lo | hi != 0, |a| peek(a) != 0);
                    ObjectFact {
                        slot: k as u16,
                        kind: at(t.kind, k).map_or(0, peek),
                        alive,
                        world_x: hi * 256 + lo,
                        y: at(t.y, k).map_or(0, |a| i64::from(peek(a))),
                    }
                })
                .collect()
        });
        Facts {
            cam: (raw, cam_y),
            scene,
            objects,
            full_camera: self.spec.cam_y.is_some() || self.spec.table.is_some(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe(text: &str) -> Recipe {
        rf_profiles::load_recipe_str(&format!(
            "[recipe]\nformat = 1\nid = \"t\"\nengine = \"t\"\n{text}"
        ))
        .expect("recipe")
    }

    const MM2: &str = r#"
[[ram]]
addr = 0x1f
width = 1
name = "scroll_x"
grade = "observed"
source = "t"
[[ram]]
addr = 0x20
width = 1
name = "scroll_page"
grade = "observed"
source = "t"
[[object_table]]
name = "objects"
base = 0x400
slots = 4
stride = 1
x_lo = 0x60
x_hi = 0x40
y = 0xa0
state = 0x0
grade = "observed"
source = "t"
"#;

    #[test]
    fn mm2_shaped_recipe_reads_camera_and_slots() {
        let spec = Spec::from_recipe(&recipe(MM2)).expect("spec");
        assert!(spec.has_objects() && !spec.has_full_camera());
        let mut ram = vec![0u8; 0x800];
        ram[0x1f] = 0x10;
        ram[0x20] = 2;
        ram[0x401] = 1;
        ram[0x461] = 0x34;
        ram[0x441] = 0x03;
        ram[0x4a1] = 90;
        let mut t = Tracker::new(spec);
        let f = t.read(&|a| ram[a as usize]);
        assert_eq!(f.cam, (0x210, 0));
        let o = f.objects.expect("table");
        assert_eq!(o.len(), 4);
        assert!(!o[0].alive && o[1].alive);
        assert_eq!((o[1].world_x, o[1].y), (0x334, 90));
    }

    #[test]
    fn low_byte_camera_is_unwrapped_and_jumps_start_a_scene() {
        let spec = Spec::from_recipe(&recipe(
            "[[ram]]\naddr = 0xb1\nwidth = 1\nname = \"camera_x_lo\"\ngrade = \"observed\"\nsource = \"t\"\n",
        ))
        .expect("spec");
        assert!(!spec.has_objects());
        assert!(spec.describe().contains("level only"));
        let mut t = Tracker::new(spec);
        let mut at = |b: u8| t.read(&move |_| b).cam.0;
        assert_eq!(at(250), 0);
        assert_eq!(at(4), 10, "250 -> 4 is +10 across the wrap");
        assert_eq!(at(0), 6);
        assert_eq!(at(10), 16);
    }

    #[test]
    fn a_big_camera_jump_changes_the_scene_key() {
        let mut t = Tracker::new(Spec::from_recipe(&recipe(MM2)).expect("spec"));
        let mut ram = vec![0u8; 0x800];
        let s0 = t.read(&|a| ram[a as usize]).scene;
        ram[0x20] = 1;
        assert_eq!(
            t.read(&|a| ram[a as usize]).scene,
            s0,
            "a page of scrolling"
        );
        ram[0x20] = 9;
        assert_ne!(
            t.read(&|a| ram[a as usize]).scene,
            s0,
            "a jump of eight pages"
        );
    }

    #[test]
    fn no_camera_row_means_no_spec() {
        assert!(Spec::from_recipe(&recipe("")).is_none());
    }

    #[test]
    fn shipped_recipes_resolve() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles");
        for (p, objects, full) in [
            ("snes/super-mario-world", false, true),
            ("snes/super-metroid", false, false),
            ("nes/castlevania", true, false),
            ("nes/mega-man-2", true, false),
        ] {
            let r = rf_profiles::load_recipe_file(&root.join(p).join("recipe.toml")).expect(p);
            let s = Spec::from_recipe(&r).unwrap_or_else(|| panic!("{p}: no camera"));
            assert_eq!(
                (s.has_objects(), s.has_full_camera()),
                (objects, full),
                "{p}"
            );
        }
    }
}
