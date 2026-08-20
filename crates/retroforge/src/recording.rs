//! Video recording (ticket W8-03; FR-FE-007, `docs/design/FRONTEND_UI.md`
//! §5).
//!
//! ## Which pipeline is recorded, and why that is a choice
//!
//! FR-FE-007 says "video recording of **either** pipeline". The shell
//! already resolves both every frame it captures — `CompareBuffers` holds
//! the accuracy-exact `original` (from `video_scanline` alone) and the
//! `enhanced` frame the user is actually looking at. [`Recorder`] takes
//! whichever the caller hands it, so the choice stays with the UI rather
//! than being baked in here.
//!
//! ## Never on the core thread
//!
//! §5 is explicit: encode "from the render-thread texture queue … never
//! on the core thread". [`Recorder::push`] is a memcpy into a `Vec` and
//! nothing else; all encoding happens in [`Recorder::finish`], which the
//! caller invokes when recording stops. Nothing here can stall the
//! emulation.
//!
//! ## The format is APNG, and it is uncompressed
//!
//! See `rf_renderer::png::encode_apng` for why APNG rather than an ffmpeg
//! sidecar or gstreamer. The consequence to be honest about: frames are
//! stored uncompressed, so a recording is **large** — roughly
//! `width * height * 4` bytes per frame. [`Recorder::estimated_bytes`]
//! reports that as it accrues, and [`Recorder::with_limit`] exists so a
//! UI can stop before filling a disk rather than after.

use rf_renderer::png;

/// Accumulates frames and encodes them as an APNG when stopped.
pub struct Recorder {
    width: u32,
    height: u32,
    fps: u16,
    frames: Vec<Vec<u8>>,
    /// Stop accepting frames past this many bytes. `None` = unbounded.
    limit_bytes: Option<usize>,
    /// Frames dropped because the limit was reached — surfaced rather
    /// than silently discarded, so a UI can say the recording was cut.
    dropped: u64,
}

impl Recorder {
    #[must_use]
    pub fn new(width: u32, height: u32, fps: u16) -> Self {
        Self {
            width,
            height,
            fps,
            frames: Vec::new(),
            limit_bytes: None,
            dropped: 0,
        }
    }

    /// Stop accepting frames once the recording would exceed `bytes`.
    #[must_use]
    pub fn with_limit(mut self, bytes: usize) -> Self {
        self.limit_bytes = Some(bytes);
        self
    }

    /// Frames captured so far.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Frames refused because the size limit was reached.
    #[must_use]
    pub fn dropped_frames(&self) -> u64 {
        self.dropped
    }

    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        self.frames.len() * (self.width as usize) * (self.height as usize) * 4
    }

    /// Capture one RGBA frame.
    ///
    /// A frame whose geometry disagrees with the recording's is
    /// **refused**, not scaled or padded: a recording that silently
    /// changed size mid-file would produce a broken APNG, and guessing at
    /// a resize is a decision this does not own. Returns whether the
    /// frame was kept.
    pub fn push(&mut self, rgba: &[u8]) -> bool {
        let expected = (self.width as usize) * (self.height as usize) * 4;
        if rgba.len() != expected {
            self.dropped += 1;
            return false;
        }
        if let Some(limit) = self.limit_bytes {
            if self.estimated_bytes() + expected > limit {
                self.dropped += 1;
                return false;
            }
        }
        self.frames.push(rgba.to_vec());
        true
    }

    /// Encode everything captured. `None` if nothing was.
    #[must_use]
    pub fn finish(self) -> Option<Vec<u8>> {
        if self.frames.is_empty() {
            return None;
        }
        Some(png::encode_apng(
            &self.frames,
            self.width,
            self.height,
            self.fps,
        ))
    }

    /// A filename for a recording taken at `frame`.
    ///
    /// The `.png` extension is deliberate and not a mistake: an APNG *is*
    /// a PNG, and every non-APNG decoder shows its first frame rather
    /// than refusing the file. Naming it `.apng` would break that
    /// gracefully-degrading property with tools that dispatch on
    /// extension.
    #[must_use]
    pub fn suggested_filename(frame: u64) -> String {
        format!("retroforge-{frame}-recording.png")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32, fill: u8) -> Vec<u8> {
        vec![fill; (w * h * 4) as usize]
    }

    /// **FR-FE-007, checked against the OUTPUT.** The file's own declared
    /// frame count must match what was pushed — not the recorder's
    /// bookkeeping, which would pass on a malformed file.
    #[test]
    fn the_encoded_file_declares_the_frames_that_were_pushed() {
        let mut r = Recorder::new(8, 8, 60);
        for i in 0..12u8 {
            assert!(r.push(&frame(8, 8, i * 20)));
        }
        assert_eq!(r.frame_count(), 12);
        let bytes = r.finish().expect("frames were captured");
        assert_eq!(png::apng_frame_count(&bytes), Some(12));
    }

    /// Recording nothing produces nothing, rather than a zero-frame file
    /// no decoder accepts.
    #[test]
    fn an_empty_recording_produces_no_file() {
        assert!(Recorder::new(8, 8, 60).finish().is_none());
    }

    /// A frame of the wrong geometry is REFUSED and counted, never
    /// scaled, padded or silently dropped.
    #[test]
    fn a_mismatched_frame_is_refused_and_reported() {
        let mut r = Recorder::new(8, 8, 60);
        assert!(r.push(&frame(8, 8, 1)));
        assert!(!r.push(&frame(16, 16, 2)), "wrong geometry must be refused");
        assert_eq!(r.frame_count(), 1);
        assert_eq!(r.dropped_frames(), 1, "and the drop must be visible");

        // What it did capture is still a valid file.
        let bytes = r.finish().expect("one frame");
        assert_eq!(png::apng_frame_count(&bytes), Some(1));
    }

    /// The size limit stops the recording rather than filling a disk —
    /// and says how many frames it cost.
    #[test]
    fn the_size_limit_stops_capture_and_reports_the_shortfall() {
        let per_frame = 4 * 4 * 4;
        let mut r = Recorder::new(4, 4, 60).with_limit(per_frame * 3);
        for _ in 0..10 {
            r.push(&frame(4, 4, 7));
        }
        assert_eq!(r.frame_count(), 3, "capped at the limit");
        assert_eq!(r.dropped_frames(), 7);
        assert_eq!(png::apng_frame_count(&r.finish().unwrap()), Some(3));
    }

    #[test]
    fn estimated_bytes_tracks_what_was_captured() {
        let mut r = Recorder::new(10, 10, 60);
        assert_eq!(r.estimated_bytes(), 0);
        r.push(&frame(10, 10, 0));
        assert_eq!(r.estimated_bytes(), 10 * 10 * 4);
    }

    /// The extension is `.png` on purpose: an APNG IS a PNG, and every
    /// non-APNG decoder shows its first frame rather than refusing it.
    #[test]
    fn the_suggested_filename_keeps_the_png_extension() {
        let name = Recorder::suggested_filename(1234);
        assert!(name.ends_with(".png"), "{name}");
        assert!(name.contains("1234"));
    }
}
