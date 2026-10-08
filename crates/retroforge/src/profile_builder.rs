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
}
