//! Writes a real APNG so an external decoder can be pointed at it —
//! FR-FE-007's "playable file" is a claim about the world, not about our
//! own encoder, so something outside this crate has to accept it.
#[test]
#[ignore = "writes a file for external inspection; set RF_APNG_OUT"]
fn write_a_sample_apng() {
    let Ok(dir) = std::env::var("RF_APNG_OUT") else {
        eprintln!("SKIP: set RF_APNG_OUT");
        return;
    };
    let (w, h) = (64u32, 64u32);
    // A moving band, so a still viewer and an animating one differ
    // visibly — a file that "works" but shows one frame is exactly the
    // failure this sample exists to expose.
    let frames: Vec<Vec<u8>> = (0..30)
        .map(|f| {
            let mut px = vec![0u8; (w * h * 4) as usize];
            for y in 0..h {
                for x in 0..w {
                    let i = ((y * w + x) * 4) as usize;
                    let on = (x + f * 2) % 64 < 16;
                    px[i] = if on { 255 } else { 20 };
                    px[i + 1] = if on { 80 } else { 20 };
                    px[i + 2] = 40;
                    px[i + 3] = 255;
                }
            }
            px
        })
        .collect();
    let bytes = rf_renderer::png::encode_apng(&frames, w, h, 30);
    let path = format!("{dir}/sample.png");
    std::fs::write(&path, &bytes).expect("write");
    eprintln!(
        "wrote {path}: {} bytes, {} frames declared",
        bytes.len(),
        rf_renderer::png::apng_frame_count(&bytes).unwrap()
    );
}
