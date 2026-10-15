//! Ticket W30-02: HD Mode 7 on real cartridges, by observation.
//!
//! ROM-gated, like `superfx_overclock.rs`: `RF_HDMODE7_ROMS` is a
//! `:`-separated list of SNES dumps (bare or zipped). Unset, it skips.
//! Each ROM runs `RF_HDMODE7_FRAMES` frames (default 1800, Start tapped
//! every 2 s to leave title screens); every 300th frame where
//! `render_hd_mode7` returns a layer (scale 3, 2x2 supersample,
//! perspective on) it prints the frame number and Mode 7 line count. With
//! `RF_HDMODE7_OUT` set it also writes `<rom>_<frame>_native.png` and
//! `_hd.png` (native nearest-upscaled vs. composited HD) there.
//!
//! What it asserts: at least one listed ROM showed Mode 7 in the run.
//! Attract mode does not reach Mode 7 in every game, and no input is
//! faked to force it.

use std::io::Read;
use std::path::Path;

use rf_core_api::{EmulatorCore, Step};
use rf_renderer::frame::FrameBuffer;
use rf_renderer::hd_layer::composite_hd_layer;
use rf_snes::core::SnesCore;
use rf_snes::ppu::hd_mode7::{render_hd_mode7, HdMode7Options};

const SCALE: usize = 3;

/// The cartridge inside `path` (bare or zipped).
fn rom_bytes(path: &Path) -> Option<Vec<u8>> {
    let raw = std::fs::read(path).ok()?;
    if !raw.starts_with(b"PK\x03\x04") {
        return Some(raw);
    }
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(raw)).ok()?;
    for index in 0..archive.len() {
        let Ok(mut file) = archive.by_index(index) else {
            break;
        };
        if !file.is_file() {
            continue;
        }
        let mut buf = Vec::new();
        if (&mut file).take(64 << 20).read_to_end(&mut buf).is_err() {
            continue;
        }
        if rf_cart::Cartridge::load(&buf).is_ok() {
            return Some(buf);
        }
    }
    None
}

/// Nearest-neighbour upscale (the "native" half of the comparison pair).
fn upscale(rgba: &[u8], width: usize, height: usize, scale: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len() * scale * scale);
    for y in 0..height * scale {
        for x in 0..width * scale {
            let at = ((y / scale) * width + x / scale) * 4;
            out.extend_from_slice(&rgba[at..at + 4]);
        }
    }
    out
}

fn write_png(dir: &Path, name: &str, rgba: &[u8], width: usize, height: usize) {
    let png = rf_renderer::png::encode_rgba(rgba, width as u32, height as u32);
    std::fs::write(dir.join(name), png).expect("write png");
}

#[test]
fn hd_mode7_on_real_cartridges() {
    let Ok(list) = std::env::var("RF_HDMODE7_ROMS") else {
        eprintln!("SKIP: RF_HDMODE7_ROMS unset — set it to SNES dumps to run this check");
        return;
    };
    let frames: u32 = std::env::var("RF_HDMODE7_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1800);
    let out_dir = std::env::var("RF_HDMODE7_OUT")
        .ok()
        .map(std::path::PathBuf::from);
    if let Some(dir) = &out_dir {
        std::fs::create_dir_all(dir).expect("create RF_HDMODE7_OUT");
    }
    let opts = HdMode7Options {
        scale: SCALE as u32,
        supersample: 2,
        perspective: true,
    };
    let mut roms_with_mode7 = 0;
    for path in list.split(':').filter(|p| !p.is_empty()) {
        let name = Path::new(path)
            .file_stem()
            .map_or(path.into(), |n| n.to_string_lossy().into_owned());
        let Some(bytes) = rom_bytes(Path::new(path)) else {
            println!("{name}: no cartridge found, skipped");
            continue;
        };
        let mut core = SnesCore::load(&bytes).expect("SNES dump loads");
        let mut sink = FrameBuffer::with_size(256, 224);
        sink.set_mode7_mask_capture(true);
        let mut seen = 0u32;
        for f in 0..frames {
            core.system_mut().bus.joypads.ports[0] = if f % 120 < 4 { 1 << 12 } else { 0 };
            let _ = core.step(Step::Frame, &mut sink);
            if f % 300 != 0 {
                continue;
            }
            let ppu = &core.system().bus.ppu;
            let Some(layer) = render_hd_mode7(ppu, opts, 256) else {
                continue;
            };
            seen += 1;
            let lines = layer.mode7_lines.iter().filter(|&&m| m).count();
            println!("{name}: frame {f}: mode 7 on {lines} lines");
            if let Some(dir) = &out_dir {
                let native = sink.rgba();
                let mask = sink.mode7_mask().expect("capture on");
                let (w, h) = (sink.width(), sink.height());
                let comp = composite_hd_layer(native, w, h, mask, &layer.rgba, SCALE);
                write_png(
                    dir,
                    &format!("{name}_{f}_native.png"),
                    &upscale(native, w, h, SCALE),
                    w * SCALE,
                    h * SCALE,
                );
                write_png(
                    dir,
                    &format!("{name}_{f}_hd.png"),
                    &comp,
                    w * SCALE,
                    h * SCALE,
                );
            }
        }
        if seen == 0 {
            println!("{name}: no Mode 7 frame in {frames} frames");
        } else {
            roms_with_mode7 += 1;
        }
    }
    assert!(
        roms_with_mode7 > 0,
        "no listed ROM showed Mode 7 within {frames} frames"
    );
}
