//! Geração de mídia sintética para os testes.
#![allow(dead_code)]

use image::{Rgb, RgbImage};
use std::path::Path;
use std::process::Command;

/// "Foto" determinística com formas e textura; `seed` muda a cena inteira.
pub fn photo(w: u32, h: u32, seed: u32) -> RgbImage {
    let mut s = seed.wrapping_mul(2654435761).max(1);
    let mut rnd = move || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        s
    };
    let blobs: Vec<(f32, f32, f32, [f32; 3])> = (0..14)
        .map(|_| {
            (
                (rnd() % w) as f32,
                (rnd() % h) as f32,
                (w as f32) * (0.08 + (rnd() % 100) as f32 / 500.0),
                [(rnd() % 256) as f32, (rnd() % 256) as f32, (rnd() % 256) as f32],
            )
        })
        .collect();
    RgbImage::from_fn(w, h, |x, y| {
        let mut c = [40.0 + 120.0 * x as f32 / w as f32, 60.0 + 100.0 * y as f32 / h as f32, 90.0];
        for (bx, by, r, col) in &blobs {
            let d = ((x as f32 - bx).powi(2) + (y as f32 - by).powi(2)).sqrt();
            if d < *r {
                c = *col;
            }
        }
        // Textura fina, para a foto ter nitidez mensurável.
        let n = ((x * 7 + y * 13 + seed) % 23) as f32 - 11.0;
        Rgb([(c[0] + n).clamp(0.0, 255.0) as u8, (c[1] + n).clamp(0.0, 255.0) as u8, (c[2] + n).clamp(0.0, 255.0) as u8])
    })
}

pub fn save_jpeg(img: &RgbImage, path: &Path, quality: u8) {
    let mut f = std::fs::File::create(path).unwrap();
    img.write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut f, quality)).unwrap();
}

/// Gera um vídeo com o ffmpeg a partir de uma fonte `lavfi`.
pub fn video(path: &Path, lavfi: &str, extra: &[&str]) {
    let st = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i", lavfi])
        .args(extra)
        .args(["-pix_fmt", "yuv420p"])
        .arg(path)
        .status()
        .expect("ffmpeg é necessário para os testes");
    assert!(st.success(), "ffmpeg falhou gerando {}", path.display());
}
