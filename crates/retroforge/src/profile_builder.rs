//! Making a profile in the app (Wave 24; design
//! https://claude.ai/artifact/6a89VhM41WY7Ryqaw5pt7k).
//!
//! W24-02: the player's own profiles folder, and a new profile stamped
//! with the exact dump being played so it matches the moment it is saved.

use std::path::{Path, PathBuf};

/// The player's own profiles: `<config>/retroforge/profiles`, searched
/// before the shipped ones.
#[must_use]
pub fn user_profiles_root(config_root: &Path) -> PathBuf {
    config_root
        .join(crate::bindings_store::APP_DIR)
        .join("profiles")
}

/// A folder-safe name for a title: lowercase letters and digits, runs of
/// anything else as one dash.
#[must_use]
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_owned();
    if out.is_empty() {
        "game".to_owned()
    } else {
        out
    }
}

/// A new profile for one dump: the editor's own skeleton (so it loads with
/// no warnings) plus an `[[identity]]` carrying all four hashes.
#[must_use]
pub fn new_profile_text(title: &str, snes: bool, hashes: &rf_cart::RomHashes) -> String {
    let form = crate::profile_editor::NewProfileForm {
        title: title.to_owned(),
        console: if snes {
            rf_profiles::schema::Console::Snes
        } else {
            rf_profiles::schema::Console::Nes
        },
        region: "unknown".to_owned(),
        author: "made in RetroForge".to_owned(),
        source: "found in play with the RetroForge profile builder".to_owned(),
    };
    format!(
        "{}\n[[identity]]\nsha256 = \"{}\"\nsha1 = \"{}\"\nmd5 = \"{}\"\ncrc32 = \"{}\"\nrevision = \"the copy this profile was made from\"\n",
        form.to_toml(),
        hashes.sha256,
        hashes.sha1,
        hashes.md5,
        hashes.crc32
    )
}

/// Write a new profile for this dump under `user_root`, returning its
/// path. An existing profile for the same title is left alone and its
/// path returned.
///
/// # Errors
/// Returns the OS error text.
pub fn create(
    user_root: &Path,
    title: &str,
    snes: bool,
    hashes: &rf_cart::RomHashes,
) -> Result<PathBuf, String> {
    let dir = user_root
        .join(if snes { "snes" } else { "nes" })
        .join(slug(title));
    let path = dir.join("profile.toml");
    if path.exists() {
        return Ok(path);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::write(&path, new_profile_text(title, snes, hashes))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Ticket W24-04: the builder's steps, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    CameraX,
    CameraY,
    PlayerX,
    Lives,
}

impl Step {
    pub const ALL: [Step; 4] = [Step::CameraX, Step::CameraY, Step::PlayerX, Step::Lives];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Step::CameraX => "Camera left/right",
            Step::CameraY => "Camera up/down",
            Step::PlayerX => "Player position",
            Step::Lives => "Lives",
        }
    }

    /// What to do in the game for this step.
    #[must_use]
    pub const fn instructions(self) -> &'static str {
        match self {
            Step::CameraX => {
                "Take the first look. Walk right until the screen scrolls, then press It went up. \
                 Walk left until it scrolls back, then It went down. Stand still: It didn't change. \
                 Repeat until a few are left."
            }
            Step::CameraY => {
                "Take the first look. Go somewhere the screen scrolls up or down; answer which way \
                 it went each time. Skip this in a game that only scrolls sideways."
            }
            Step::PlayerX => {
                "Take the first look. Walk right a little (It went up), left a little (It went \
                 down), stand still (It didn't change)."
            }
            Step::Lives => {
                "Take the first look. Lose a life (It went down) or gain one (It went up); while \
                 playing normally, It didn't change."
            }
        }
    }

    /// The step after this one (the last stays put).
    #[must_use]
    pub fn next(self) -> Step {
        let at = Step::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Step::ALL[(at + 1).min(Step::ALL.len() - 1)]
    }
}

/// Ticket W24-04: write `addr` into `text` (a profile) for `step`, and
/// prove the result still loads. Camera steps set `[camera]` (and, on an
/// SNES, decoded widescreen); the others add a `[[memory_map]]` row with
/// the builder as its source. The file is rewritten as plain TOML.
///
/// # Errors
/// Returns why the text could not be parsed or the result would not load.
pub fn apply_step(text: &str, step: Step, addr: u32) -> Result<String, String> {
    // The finder searches 16-bit words, so a camera it finds is one.
    apply_step_typed(text, step, addr, "u16")
}

/// Ticket W27-02: write the camera the in-play finder found, keeping the
/// width it found — most NES cameras are one byte (`$FD`), and reading
/// one as a word would take the next byte as its high half.
///
/// # Errors
/// As [`apply_step`].
pub fn apply_camera(text: &str, x: (u32, &str), y: Option<(u32, &str)>) -> Result<String, String> {
    let text = apply_step_typed(text, Step::CameraX, x.0, x.1)?;
    match y {
        Some((addr, ty)) => apply_step_typed(&text, Step::CameraY, addr, ty),
        None => Ok(text),
    }
}

/// [`apply_step`], with the camera axis's type given.
fn apply_step_typed(
    text: &str,
    step: Step,
    addr: u32,
    camera_type: &str,
) -> Result<String, String> {
    let mut doc: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    let snes = doc
        .get("meta")
        .and_then(|m| m.get("console"))
        .and_then(toml::Value::as_str)
        == Some("snes");
    let axis = |addr: u32| {
        let mut t = toml::Table::new();
        t.insert("addr".into(), toml::Value::Integer(i64::from(addr)));
        t.insert("type".into(), toml::Value::String(camera_type.into()));
        toml::Value::Table(t)
    };
    match step {
        Step::CameraX | Step::CameraY => {
            let camera = doc
                .entry("camera")
                .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                .as_table_mut()
                .ok_or("[camera] is not a table")?;
            camera
                .entry("mode")
                .or_insert_with(|| toml::Value::String("side_scroller".into()));
            let key = if step == Step::CameraX { "x" } else { "y" };
            camera.insert(key.into(), axis(addr));
            if snes {
                if let Some(caps) = doc
                    .get_mut("capabilities")
                    .and_then(toml::Value::as_table_mut)
                {
                    caps.insert("widescreen".into(), toml::Value::String("decoded".into()));
                }
            }
        }
        Step::PlayerX | Step::Lives => {
            let (label, len, ty) = if step == Step::PlayerX {
                ("player_x", 2, "u16")
            } else {
                ("lives", 1, "u8")
            };
            let rows = doc
                .entry("memory_map")
                .or_insert_with(|| toml::Value::Array(Vec::new()))
                .as_array_mut()
                .ok_or("[[memory_map]] is not a list")?;
            rows.retain(|r| r.get("label").and_then(toml::Value::as_str) != Some(label));
            let mut row = toml::Table::new();
            row.insert("addr".into(), toml::Value::Integer(i64::from(addr)));
            row.insert("len".into(), toml::Value::Integer(len));
            row.insert("type".into(), toml::Value::String(ty.into()));
            row.insert("label".into(), toml::Value::String(label.into()));
            row.insert(
                "source".into(),
                toml::Value::String("found in play with the RetroForge profile builder".into()),
            );
            rows.push(toml::Value::Table(row));
        }
    }
    let out = format!(
        "# Made in RetroForge with the profile builder (docs/design/GAME_PROFILES.md).\n{}",
        toml::to_string(&doc).map_err(|e| e.to_string())?
    );
    rf_profiles::load_str(&out).map_err(|e| format!("the profile would not load: {e}"))?;
    Ok(out)
}

/// Ticket W30-06 part B: the tallest wall the table may hold, in eighths of
/// a tile (the loader's `HEIGHTS_MAX_EIGHTHS`).
pub const HEIGHT_MAX: u8 = 64;
/// Ticket W30-06 part B: how many paint actions Undo remembers.
pub const PAINT_UNDO_DEPTH: usize = 20;

/// The `[heights.tiles]` key for a tile number: `0x0A4`.
#[must_use]
pub fn tile_key(tile: u16) -> String {
    format!("0x{tile:03X}")
}

/// Ticket W30-06 part B: the height painter's working table — what the
/// author has painted so far, the brush, and the last
/// [`PAINT_UNDO_DEPTH`] paint actions. Heights are set by a person clicking
/// tiles; nothing here is inferred from the game.
///
/// A height belongs to a tile **number**, so painting one cell paints every
/// cell on screen (and in the game) that uses that number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeightPainter {
    /// The BG layer whose tilemap is read, 1..=4.
    pub layer: u8,
    /// The grid cell, 8 or 16.
    pub tile_px: u8,
    /// Eighths of a tile, `0..=`[`HEIGHT_MAX`], what a click paints.
    pub brush: u8,
    tiles: std::collections::BTreeMap<u16, u8>,
    /// `(tile, height before)`; `None` = it was not in the table.
    undo: Vec<(u16, Option<u8>)>,
}

impl HeightPainter {
    /// A painter over an existing table (empty for a new one).
    #[must_use]
    pub fn new(layer: u8, tile_px: u8, tiles: std::collections::BTreeMap<u16, u8>) -> Self {
        Self {
            layer: layer.clamp(1, 4),
            tile_px: if tile_px == 16 { 16 } else { 8 },
            brush: 16,
            tiles: tiles
                .into_iter()
                .filter(|(t, h)| *t <= 0x3FF && *h > 0)
                .map(|(t, h)| (t, h.min(HEIGHT_MAX)))
                .collect(),
            undo: Vec::new(),
        }
    }

    /// The painted height of `tile` (0 = flat).
    #[must_use]
    pub fn height_of(&self, tile: u16) -> u8 {
        self.tiles.get(&tile).copied().unwrap_or(0)
    }

    /// The painted table, tile number to eighths, sorted.
    #[must_use]
    pub fn tiles(&self) -> &std::collections::BTreeMap<u16, u8> {
        &self.tiles
    }

    /// How many tile numbers have a height.
    #[must_use]
    pub fn painted(&self) -> usize {
        self.tiles.len()
    }

    /// How many actions Undo can take back.
    #[must_use]
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// Set the brush, clamped to `0..=`[`HEIGHT_MAX`].
    pub fn set_brush(&mut self, eighths: u8) {
        self.brush = eighths.min(HEIGHT_MAX);
    }

    /// Give `tile` the brush height. `false` when nothing changed.
    pub fn paint(&mut self, tile: u16) -> bool {
        self.set(tile, self.brush)
    }

    /// Back to flat ground. `false` when it already was.
    pub fn clear(&mut self, tile: u16) -> bool {
        self.set(tile, 0)
    }

    /// Copy `tile`'s height into the brush.
    pub fn pick(&mut self, tile: u16) {
        self.brush = self.height_of(tile);
    }

    /// Take back the last paint action. `false` when there is none.
    pub fn undo(&mut self) -> bool {
        let Some((tile, before)) = self.undo.pop() else {
            return false;
        };
        match before {
            Some(h) => {
                self.tiles.insert(tile, h);
            }
            None => {
                self.tiles.remove(&tile);
            }
        }
        true
    }

    /// The table as the renderer reads it (the live preview).
    #[must_use]
    pub fn height_table(&self) -> crate::live_heights::HeightTable {
        crate::live_heights::HeightTable::new(
            self.layer,
            self.tile_px,
            self.tiles.iter().map(|(t, h)| (*t, *h)).collect(),
        )
    }

    fn set(&mut self, tile: u16, eighths: u8) -> bool {
        let eighths = eighths.min(HEIGHT_MAX);
        if tile > 0x3FF || self.height_of(tile) == eighths {
            return false;
        }
        let before = self.tiles.get(&tile).copied();
        if eighths == 0 {
            self.tiles.remove(&tile);
        } else {
            self.tiles.insert(tile, eighths);
        }
        self.undo.push((tile, before));
        if self.undo.len() > PAINT_UNDO_DEPTH {
            self.undo.remove(0);
        }
        true
    }
}

/// Ticket W30-06 part B: the open painter, as the app holds it. Plain data;
/// the app draws it (`RetroForgeApp::height_painter_window`).
#[derive(Debug, Clone)]
pub struct PainterSession {
    pub painter: HeightPainter,
    /// The next click copies that tile's height into the brush.
    pub pick_armed: bool,
    /// The diorama is shown beside the grid, which means the game runs.
    pub preview: bool,
    pub note: Option<String>,
    /// The painter paused a running game, so closing it resumes.
    pub paused_game: bool,
    /// A paused game has sent no snapshot yet: step one frame to get one.
    pub need_frame: bool,
    /// Painted since the last save.
    pub dirty: bool,
}

/// Ticket W30-06 part B: write the painted table into `text` (a profile) as
/// `[heights]` — layer, tile_px and `[heights.tiles]` keyed `"0x0A4"`,
/// sorted — replacing any table already there, and prove the result still
/// loads. An empty table removes `[heights]`. The file is rewritten as plain
/// TOML, like [`apply_step`].
///
/// # Errors
/// Returns why the text could not be parsed or the result would not load.
pub fn apply_heights(
    text: &str,
    layer: u8,
    tile_px: u8,
    tiles: &std::collections::BTreeMap<u16, u8>,
) -> Result<String, String> {
    let mut doc: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    doc.remove("heights");
    if !tiles.is_empty() {
        let mut rows = toml::Table::new();
        for (tile, h) in tiles.iter().filter(|(_, h)| **h > 0) {
            rows.insert(tile_key(*tile), toml::Value::Integer(i64::from(*h)));
        }
        let mut t = toml::Table::new();
        t.insert("layer".into(), toml::Value::Integer(i64::from(layer)));
        t.insert("tile_px".into(), toml::Value::Integer(i64::from(tile_px)));
        t.insert("tiles".into(), toml::Value::Table(rows));
        doc.insert("heights".into(), toml::Value::Table(t));
    }
    let out = format!(
        "# Made in RetroForge with the profile builder (docs/design/GAME_PROFILES.md).\n{}",
        toml::to_string(&doc).map_err(|e| e.to_string())?
    );
    rf_profiles::load_str(&out).map_err(|e| format!("the profile would not load: {e}"))?;
    Ok(out)
}

/// Ticket W30-06 part B: where "Save heights" writes. The builder only ever
/// writes the player's own copy: a matched profile that already lives under
/// `user_root` is rewritten in place; a shipped one is copied to
/// `<user_root>/snes/<slug(title)>/profile.toml`, which the loader searches
/// first, so the repo's file is never touched.
#[must_use]
pub fn heights_save_path(user_root: &Path, matched: Option<&Path>, title: &str) -> PathBuf {
    match matched {
        Some(path) if path.starts_with(user_root) => path.to_path_buf(),
        _ => user_root
            .join("snes")
            .join(slug(title))
            .join("profile.toml"),
    }
}

/// Ticket W30-06 part B: the table a profile's text already holds, as a
/// painter starts from it. `None` when it does not parse or has no
/// `[heights]`.
#[must_use]
pub fn heights_from_text(text: &str) -> Option<HeightPainter> {
    let profile = rf_profiles::load_str(text).ok()?.profile;
    let h = profile.heights?;
    let layer = u8::try_from(h.layer).unwrap_or(1);
    let tile_px = u8::try_from(h.tile_px).unwrap_or(8);
    Some(HeightPainter::new(
        layer,
        tile_px,
        h.table().into_iter().collect(),
    ))
}

/// Ticket W24-03: which way a value moved between two looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Up,
    Down,
    Same,
}

/// Ticket W24-03: the RAM search — candidates are 16-bit little-endian
/// words at every offset of the work RAM, narrowed each time the player
/// says which way the thing they are watching moved.
#[derive(Debug, Clone, Default)]
pub struct Finder {
    last: Vec<u8>,
    /// Offsets still in the running, ascending.
    candidates: Vec<usize>,
    /// How many looks have narrowed it.
    pub rounds: u32,
}

/// Read the 16-bit little-endian word at `at` (the byte past the end reads
/// as zero, so the last offset is still a candidate).
fn word(ram: &[u8], at: usize) -> u16 {
    let lo = ram.get(at).copied().unwrap_or(0);
    let hi = ram.get(at + 1).copied().unwrap_or(0);
    u16::from_le_bytes([lo, hi])
}

impl Finder {
    /// Start from a first look at `ram`: every offset is a candidate.
    #[must_use]
    pub fn start(ram: Vec<u8>) -> Self {
        Self {
            candidates: (0..ram.len()).collect(),
            last: ram,
            rounds: 0,
        }
    }

    /// Keep the candidates whose word moved as `change` says since the
    /// last look, then make `ram` the new last look.
    pub fn narrow(&mut self, ram: Vec<u8>, change: Change) {
        if ram.len() != self.last.len() {
            *self = Self::start(ram);
            return;
        }
        let last = &self.last;
        self.candidates.retain(|&at| {
            let (old, new) = (word(last, at), word(&ram, at));
            match change {
                Change::Up => new > old,
                Change::Down => new < old,
                Change::Same => new == old,
            }
        });
        self.last = ram;
        self.rounds += 1;
    }

    /// How many offsets still match.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.candidates.len()
    }

    /// The first `n` candidates with their current values.
    #[must_use]
    pub fn top(&self, n: usize) -> Vec<(usize, u16)> {
        self.candidates
            .iter()
            .take(n)
            .map(|&at| (at, word(&self.last, at)))
            .collect()
    }
}

/// How the builder shows an address: `$021E` on a NES, `$7E001A` on an
/// SNES — the widths the consoles' own documentation uses.
#[must_use]
pub fn show_address(snes: bool, addr: u32) -> String {
    if snes {
        format!("${addr:06X}")
    } else {
        format!("${addr:04X}")
    }
}

/// The address a work-RAM offset has in a profile: NES WRAM is `$0000`,
/// SNES WRAM `$7E0000` (fullsnes "Memory Map").
#[must_use]
pub fn ram_address(snes: bool, offset: usize) -> u32 {
    let base = if snes { 0x7E_0000 } else { 0 };
    #[allow(clippy::cast_possible_truncation)]
    let offset = offset as u32;
    base + offset
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hashes() -> rf_cart::RomHashes {
        rf_cart::RomHashes {
            crc32: "0123abcd".into(),
            md5: "0".repeat(32),
            sha1: "1".repeat(40),
            sha256: "2".repeat(64),
        }
    }

    #[test]
    fn slugs_are_folder_safe() {
        assert_eq!(slug("Super Mario Bros. 3 (USA)"), "super-mario-bros-3-usa");
        assert_eq!(slug("!!!"), "game");
    }

    /// The new profile loads with the real loader, no warnings, and
    /// claims exactly this dump.
    #[test]
    fn a_new_profile_loads_cleanly_and_matches_its_dump() {
        let tmp = std::env::temp_dir().join(format!("rf_newprof_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let path = create(&tmp, "Test Game", true, &hashes()).unwrap();
        let outcome = rf_profiles::load_file(&path).expect("loads");
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
        assert!(outcome
            .profile
            .identity
            .iter()
            .any(|i| i.matches(&hashes())));
        let found = crate::level_view::find_matching_profile(&tmp, &hashes());
        assert!(found.is_some());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Ticket W24-03: a counter that rises and falls is found in three
    /// looks among bytes that do other things.
    #[test]
    fn the_finder_narrows_to_the_moving_word() {
        let mut ram = vec![0u8; 64];
        let set = |ram: &mut Vec<u8>, cam: u16, noise: u8| {
            ram[10..12].copy_from_slice(&cam.to_le_bytes());
            ram[30] = noise; // a byte that wanders
        };
        set(&mut ram, 100, 1);
        let mut f = Finder::start(ram.clone());
        assert_eq!(f.remaining(), 64);
        set(&mut ram, 140, 7);
        f.narrow(ram.clone(), Change::Up);
        set(&mut ram, 90, 3);
        f.narrow(ram.clone(), Change::Down);
        f.narrow(ram.clone(), Change::Same);
        let top = f.top(5);
        assert!(top.iter().any(|(at, v)| *at == 10 && *v == 90), "{top:?}");
        assert!(f.remaining() <= 4, "{}", f.remaining());
        assert_eq!(ram_address(true, 0x1A), 0x7E_001A);
        assert_eq!(ram_address(false, 0x1A), 0x1A);
        assert_eq!(show_address(false, 0x21E), "$021E");
        assert_eq!(show_address(true, 0x7E_001A), "$7E001A");
    }

    /// Ticket W24-04: each step writes what it found, and the result
    /// loads with the real loader.
    #[test]
    fn steps_write_a_loadable_profile() {
        let text = new_profile_text("Test Game", true, &hashes());
        let text = apply_step(&text, Step::CameraX, 0x7E_001A).unwrap();
        let text = apply_step(&text, Step::CameraY, 0x7E_001C).unwrap();
        let text = apply_step(&text, Step::Lives, 0x7E_0DBE).unwrap();
        let text = apply_step(&text, Step::Lives, 0x7E_0DBF).unwrap();
        let p = rf_profiles::load_str(&text).unwrap().profile;
        let cam = p.camera.clone().expect("camera");
        assert_eq!(cam.x.unwrap().addr, 0x7E_001A);
        assert_eq!(cam.y.unwrap().addr, 0x7E_001C);
        assert_eq!(
            p.memory_map.iter().filter(|m| m.label == "lives").count(),
            1,
            "replaced, not added"
        );
        assert_eq!(p.memory_map[0].addr, 0x7E_0DBF);
        assert!(crate::level_view::profile_chips(&p).contains(&"Widescreen"));
        assert_eq!(Step::CameraX.next(), Step::CameraY);
        assert_eq!(Step::Lives.next(), Step::Lives);
    }

    /// Ticket W27-02: the found camera keeps its width.
    #[test]
    fn a_found_camera_keeps_its_width() {
        let text = new_profile_text("Test Game", false, &hashes());
        let text = apply_camera(&text, (0xFD, "u8"), Some((0xFC, "u8"))).unwrap();
        let p = rf_profiles::load_str(&text).unwrap().profile;
        let cam = p.camera.clone().expect("camera");
        assert_eq!(
            (cam.x.as_ref().unwrap().addr, cam.mode.as_str()),
            (0xFD, "side_scroller")
        );
        assert!(text.contains("type = \"u8\""), "{text}");
        assert_eq!(cam.y.unwrap().addr, 0xFC);
    }
    /// Ticket W30-06 part B: painting, picking, clearing and a bounded undo.
    #[test]
    fn the_painter_paints_picks_clears_and_undoes_the_last_twenty() {
        let mut p = HeightPainter::new(1, 8, Default::default());
        p.set_brush(200);
        assert_eq!(p.brush, HEIGHT_MAX);
        p.set_brush(16);
        assert!(p.paint(0x0A4));
        assert!(!p.paint(0x0A4), "no change, no undo entry");
        assert_eq!((p.height_of(0x0A4), p.undo_len()), (16, 1));
        p.set_brush(32);
        assert!(p.paint(0x0A4), "repainting changes it");
        p.pick(0x0A4);
        assert_eq!(p.brush, 32);
        p.pick(0x777);
        assert_eq!(p.brush, 0, "picking flat ground brushes 0");
        assert!(p.clear(0x0A4));
        assert!(!p.clear(0x0A4));
        assert_eq!(p.painted(), 0);
        assert!(p.undo() && p.height_of(0x0A4) == 32, "undo the clear");
        assert!(p.undo() && p.height_of(0x0A4) == 16, "undo the repaint");
        assert!(p.undo() && p.height_of(0x0A4) == 0, "undo the first paint");
        assert!(!p.undo());
        assert!(!p.paint(0x400), "tile numbers are 10 bits");

        p.set_brush(8);
        for t in 0..30u16 {
            p.paint(t);
        }
        assert_eq!(p.undo_len(), PAINT_UNDO_DEPTH);
        for _ in 0..PAINT_UNDO_DEPTH {
            assert!(p.undo());
        }
        assert!(!p.undo());
        assert_eq!(p.painted(), 10, "the oldest ten actions stay painted");
    }

    /// Ticket W30-06 part B: the saved table is sorted, keyed in hex, loads,
    /// and reads back as painted; a second save replaces it; empty removes.
    #[test]
    fn saved_heights_round_trip_through_the_loader() {
        let base = new_profile_text("Test Game", true, &hashes());
        let mut tiles = std::collections::BTreeMap::new();
        tiles.insert(0x1FF_u16, 64_u8);
        tiles.insert(0x0A4, 16);
        tiles.insert(0x010, 8);
        let text = apply_heights(&base, 2, 16, &tiles).unwrap();
        let a = text.find("0x010").unwrap();
        let b = text.find("0x0A4").unwrap();
        let c = text.find("0x1FF").unwrap();
        assert!(a < b && b < c, "{text}");
        assert!(text.contains("layer = 2") && text.contains("tile_px = 16"));
        let back = heights_from_text(&text).expect("a table");
        assert_eq!((back.layer, back.tile_px), (2, 16));
        assert_eq!(back.tiles(), &tiles);
        // The loader itself reads the same table.
        let loaded = rf_profiles::load_str(&text).unwrap().profile;
        assert_eq!(loaded.heights.unwrap().table().get(&0x0A4), Some(&16));
        // Other sections survive and the identity still matches.
        assert!(loaded.identity.iter().any(|i| i.matches(&hashes())));

        tiles.remove(&0x1FF);
        let text2 = apply_heights(&text, 1, 8, &tiles).unwrap();
        let back = heights_from_text(&text2).unwrap();
        assert_eq!((back.layer, back.painted()), (1, 2), "replaced, not merged");
        let none = apply_heights(&text2, 1, 8, &Default::default()).unwrap();
        assert!(heights_from_text(&none).is_none() && !none.contains("[heights"));
    }

    /// Ticket W30-06 part B: heights are only ever written to the player's
    /// own copy, never to the shipped profile.
    #[test]
    fn heights_are_saved_to_the_players_copy_never_the_shipped_file() {
        let user = Path::new("/cfg/retroforge/profiles");
        let shipped = Path::new("/repo/profiles/snes/some-game/profile.toml");
        assert_eq!(
            heights_save_path(user, Some(shipped), "Some Game (USA)"),
            user.join("snes/some-game-usa/profile.toml")
        );
        assert_eq!(
            heights_save_path(user, None, "Some Game"),
            user.join("snes/some-game/profile.toml")
        );
        let mine = user.join("snes/mine/profile.toml");
        assert_eq!(heights_save_path(user, Some(&mine), "Whatever"), mine);
    }
}
