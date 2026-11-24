//! Melhoria de qualidade: contraste automático e nitidez em fotos; redução
//! de ruído, nitidez e (opcional) estabilização em vídeos. Sempre gera um
//! arquivo novo ao lado do original, que não é alterado.

use crate::db::{Db, Media};
use crate::optimize::{copy_jpeg_metadata, transcode};
use crate::trash::{self, ActionResult};
use crate::{scan, Dirs, Progress};
use anyhow::{bail, Result};
use image::RgbImage;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct EnhanceOptions {
    /// Vídeos: estabilizar tremidos (mais lento, corta um pouco as bordas).
    pub stabilize: bool,
}

/// Estica o histograma de luminância entre os percentis 0,5% e 99,5%, com
/// limites para não exagerar em fotos propositalmente escuras ou claras.
pub fn auto_contrast(img: &mut RgbImage) {
    let mut hist = [0u64; 256];
    for p in img.pixels() {
        let l = (p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000;
        hist[l as usize] += 1;
    }
    let total: u64 = hist.iter().sum();
    let cut = total / 200;
    let (mut lo, mut acc) = (0usize, 0u64);
    while lo < 255 && acc + hist[lo] <= cut {
        acc += hist[lo];
        lo += 1;
    }
    let (mut hi, mut acc) = (255usize, 0u64);
    while hi > 0 && acc + hist[hi] <= cut {
        acc += hist[hi];
        hi -= 1;
    }
    let lo = lo.min(48) as f32;
    let hi = hi.max(200) as f32;
    if hi - lo >= 254.0 {
        return;
    }
    let scale = 255.0 / (hi - lo);
    let lut: Vec<u8> = (0..256).map(|v| ((v as f32 - lo) * scale).clamp(0.0, 255.0) as u8).collect();
    for p in img.pixels_mut() {
        p[0] = lut[p[0] as usize];
        p[1] = lut[p[1] as usize];
        p[2] = lut[p[2] as usize];
    }
}

fn output_path(m: &Media, suffix: &str, ext: &str) -> PathBuf {
    let p = Path::new(&m.path);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    p.with_file_name(format!("{stem}_{suffix}.{ext}"))
}

fn enhance_image(m: &Media, out: &Path) -> Result<()> {
    let bytes = std::fs::read(&m.path)?;
    // Sem aplicar a orientação EXIF: ela é copiada junto com os metadados.
    let img = image::load_from_memory(&bytes)
        .map_err(|e| anyhow::anyhow!("formato não suportado para melhoria ({e})"))?;
    let mut rgb = img.to_rgb8();
    auto_contrast(&mut rgb);
    let rgb = image::imageops::unsharpen(&rgb, 1.2, 3);
    let mut jpeg = Vec::new();
    rgb.write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 92))?;
    let jpeg = copy_jpeg_metadata(&bytes, jpeg)?;
    std::fs::write(out, jpeg)?;
    Ok(())
}

fn enhance_video(m: &Media, out: &Path, opts: &EnhanceOptions) -> Result<()> {
    let mut filters = Vec::new();
    if opts.stabilize {
        filters.push("deshake=rx=32:ry=32");
    }
    filters.extend(["hqdn3d=1.5:1.5:6:6", "unsharp=5:5:0.5:5:5:0.0", "eq=contrast=1.03:saturation=1.06"]);
    let vf = filters.join(",");
    transcode(
        Path::new(&m.path),
        out,
        &["-vf", &vf, "-c:v", "libx264", "-preset", "medium", "-crf", "18", "-pix_fmt", "yuv420p"],
    )
}

fn enhance_one(db: &Db, dirs: &Dirs, m: &Media, opts: &EnhanceOptions) -> Result<ActionResult> {
    let out = if m.kind == "video" {
        output_path(m, "melhorado", "mp4")
    } else {
        output_path(m, "melhorada", "jpg")
    };
    if out.exists() {
        bail!("já existe {}", out.display());
    }
    if m.kind == "video" {
        enhance_video(m, &out, opts)?;
    } else {
        enhance_image(m, &out)?;
    }
    let _ = filetime::set_file_mtime(&out, filetime::FileTime::from_unix_time(m.mtime, 0));
    let id = scan::index_file(db, dirs, m.source_id, &out)?;
    let new_size = std::fs::metadata(&out)?.len();
    let name = out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut message = format!("criado {name}");
    if let Err(e) = trash::sync_device(db, m.source_id, None, Some(&out)) {
        message.push_str(&format!(" (não foi possível enviar ao aparelho: {e})"));
    }
    Ok(ActionResult { id, name: m.name.clone(), ok: true, message, old_size: m.size, new_size })
}

pub fn enhance(db: &Db, dirs: &Dirs, ids: &[i64], opts: &EnhanceOptions, progress: Progress) -> Vec<ActionResult> {
    let mut out = Vec::new();
    for (i, &id) in ids.iter().enumerate() {
        let m = match db.media(id) {
            Ok(Some(m)) => m,
            _ => {
                out.push(ActionResult::fail(id, "", "arquivo não está mais no índice"));
                continue;
            }
        };
        progress(i, ids.len(), &format!("Melhorando {}", m.name));
        out.push(enhance_one(db, dirs, &m, opts).unwrap_or_else(|e| ActionResult::fail(id, &m.name, e.to_string())));
    }
    progress(ids.len(), ids.len(), "Concluído");
    out
}
