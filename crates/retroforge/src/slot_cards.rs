//! Save-state slots drawn as cards with their screenshot (ticket W20-06;
//! `docs/design/UX_WAVE_20.md` §4).
//!
//! Every saved slot has carried a thumbnail sidecar since W4-11
//! (`crate::state_slots`), and until W20-06 the States window printed the
//! word "thumbnail" next to it instead of drawing it. This module holds
//! the two pieces that are not layout: decoding a slot's PNG into a
//! texture **once** (not per frame), and saying when it was saved the way
//! a person would.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use eframe::egui;

/// Decoded slot thumbnails, keyed by sidecar path and invalidated by its
/// modification time — so overwriting a slot shows the new picture, and
/// an unchanged one is decoded exactly once.
#[derive(Default)]
pub struct SlotTextures {
    cache: HashMap<PathBuf, (Option<SystemTime>, Option<egui::TextureHandle>)>,
}

impl SlotTextures {
    /// The texture for the thumbnail at `path`, decoding it if it is new
    /// or changed. `None` when the file is missing or not a PNG the
    /// decoder accepts — a broken sidecar costs the picture, never the
    /// window.
    pub fn get(&mut self, ctx: &egui::Context, path: &Path) -> Option<egui::TextureHandle> {
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if let Some((seen, tex)) = self.cache.get(path) {
            if *seen == mtime {
                return tex.clone();
            }
        }
        let tex = image::open(path).ok().map(|img| {
            let rgba = img.to_rgba8();
            let size = [rgba.width() as usize, rgba.height() as usize];
            let image = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
            ctx.load_texture(
                format!("slot-thumb:{}", path.display()),
                image,
                egui::TextureOptions::NEAREST,
            )
        });
        self.cache.insert(path.to_path_buf(), (mtime, tex.clone()));
        tex
    }

    /// Drop every cached texture (a different game's slots are showing).
    pub fn clear(&mut self) {
        self.cache.clear();
    }
}

/// "just now", "5 min ago", "3 h ago", "yesterday", "4 days ago", or the
/// date for anything older than a week. `now` before `saved` (a clock
/// change) reads as "just now" rather than a negative age.
#[must_use]
pub fn relative_age(saved_secs: u64, now_secs: u64, absolute: impl FnOnce() -> String) -> String {
    let age = now_secs.saturating_sub(saved_secs);
    match age {
        0..=59 => "just now".to_string(),
        60..=3_599 => format!("{} min ago", age / 60),
        3_600..=86_399 => format!("{} h ago", age / 3_600),
        86_400..=172_799 => "yesterday".to_string(),
        172_800..=604_799 => format!("{} days ago", age / 86_400),
        _ => absolute(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_read_like_a_person_says_them() {
        let abs = || "2026-01-01".to_string();
        assert_eq!(relative_age(1000, 1010, abs), "just now");
        assert_eq!(relative_age(1000, 1000 + 5 * 60, abs), "5 min ago");
        assert_eq!(relative_age(1000, 1000 + 3 * 3600, abs), "3 h ago");
        assert_eq!(relative_age(1000, 1000 + 86_400 + 5, abs), "yesterday");
        assert_eq!(relative_age(1000, 1000 + 4 * 86_400, abs), "4 days ago");
        assert_eq!(relative_age(1000, 1000 + 30 * 86_400, abs), "2026-01-01");
        // Clock moved backwards: not a negative age.
        assert_eq!(relative_age(5000, 1000, abs), "just now");
    }
}
