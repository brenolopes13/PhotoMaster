//! Recompressão de fotos e vídeos para ocupar menos espaço. O original
//! sempre vai para a lixeira do app e pode ser restaurado.

use crate::db::{Db, Media};
use crate::media::{command, ffmpeg_bin};
use crate::trash::{self, ActionResult};
use crate::{scan, Dirs, Progress};
use anyhow::{bail, Context, Result};
use img_parts::{ImageEXIF, ImageICC};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct OptimizeOptions {
    /// Qualidade JPEG (1–100).
    pub jpeg_quality: u8,
    /// Se definido, reduz fotos cujo maior lado passe disto (em pixels).
    pub max_dimension: Option<u32>,
    /// "hevc" (menor) ou "h264" (mais compatível).
    pub video_codec: String,
    /// Fator de qualidade do vídeo; menor = melhor e maior. Padrão por codec.
    pub crf: Option<u8>,
}

impl Default for OptimizeOptions {
    fn default() -> Self {
        OptimizeOptions { jpeg_quality: 82, max_dimension: None, video_codec: "hevc".into(), crf: None }
    }
}

/// Só vale a pena trocar o arquivo se economizar pelo menos 10%.
const MIN_GAIN: f64 = 0.90;

fn tmp_sibling(path: &Path, ext: &str) -> PathBuf {
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    // Começa com ponto: a varredura ignora arquivos ocultos.
    path.with_file_name(format!(".pm-tmp-{name}.{ext}"))
}

/// Recomprime um JPEG preservando EXIF e perfil de cor.
pub fn recompress_jpeg(bytes: &[u8], quality: u8, max_dimension: Option<u32>) -> Result<Vec<u8>> {
    let mut img = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg)?;
    if let Some(max) = max_dimension {
        if img.width().max(img.height()) > max {
            img = img.resize(max, max, image::imageops::FilterType::Lanczos3);
        }
    }
    let mut out = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100));
    img.to_rgb8().write_with_encoder(enc)?;
    copy_jpeg_metadata(bytes, out)
}

/// Copia EXIF e ICC de `from` (JPEG) para `to` (JPEG).
pub fn copy_jpeg_metadata(from: &[u8], to: Vec<u8>) -> Result<Vec<u8>> {
    let Ok(src) = img_parts::jpeg::Jpeg::from_bytes(img_parts::Bytes::copy_from_slice(from)) else {
        return Ok(to);
    };
    let mut dst = img_parts::jpeg::Jpeg::from_bytes(img_parts::Bytes::from(to))
        .map_err(|e| anyhow::anyhow!("JPEG gerado inválido: {e}"))?;
    dst.set_exif(src.exif());
    dst.set_icc_profile(src.icc_profile());
    let mut out = Vec::new();
    dst.encoder().write_to(&mut out)?;
    Ok(out)
}

fn optimize_image(m: &Media, opts: &OptimizeOptions) -> Result<Option<PathBuf>> {
    let lower = m.name.to_ascii_lowercase();
    if !(lower.ends_with(".jpg") || lower.ends_with(".jpeg")) {
        bail!("por enquanto só fotos JPEG são recomprimidas");
    }
    let path = Path::new(&m.path);
    let bytes = std::fs::read(path)?;
    let out = recompress_jpeg(&bytes, opts.jpeg_quality, opts.max_dimension)?;
    if out.len() as f64 > bytes.len() as f64 * MIN_GAIN {
        return Ok(None);
    }
    let tmp = tmp_sibling(path, "jpg");
    std::fs::write(&tmp, out)?;
    Ok(Some(tmp))
}

fn run_ffmpeg(input: &Path, output: &Path, video_args: &[&str], audio_copy: bool) -> Result<()> {
    let mut c = command(&ffmpeg_bin());
    c.args(["-y", "-v", "error", "-nostdin", "-i"]).arg(input);
    c.args(["-map", "0:v:0", "-map", "0:a?"]).args(video_args);
    if audio_copy {
        c.args(["-c:a", "copy"]);
    } else {
        c.args(["-c:a", "aac", "-b:a", "128k"]);
    }
    c.args(["-map_metadata", "0", "-movflags", "+faststart"]).arg(output);
    let out = c.output().context("ffmpeg não encontrado")?;
    if !out.status.success() {
        let _ = std::fs::remove_file(output);
        bail!("ffmpeg falhou: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Roda o ffmpeg tentando primeiro copiar o áudio e, se o contêiner não
/// aceitar o codec original, recodificando em AAC.
pub(crate) fn transcode(input: &Path, output: &Path, video_args: &[&str]) -> Result<()> {
    run_ffmpeg(input, output, video_args, true).or_else(|_| run_ffmpeg(input, output, video_args, false))
}

fn optimize_video(m: &Media, opts: &OptimizeOptions) -> Result<Option<PathBuf>> {
    let path = Path::new(&m.path);
    let (codec_args, crf): (&[&str], u8) = match opts.video_codec.as_str() {
        "h264" => (&["-c:v", "libx264", "-preset", "medium"], opts.crf.unwrap_or(23)),
        _ => (&["-c:v", "libx265", "-preset", "medium", "-tag:v", "hvc1", "-x265-params", "log-level=error"], opts.crf.unwrap_or(26)),
    };
    let crf = crf.to_string();
    let mut args: Vec<&str> = codec_args.to_vec();
    args.extend(["-crf", &crf]);
    // Extensão final já no temporário: é ela que escolhe o contêiner.
    let ext = final_path(m).extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "mp4".into());
    let tmp = tmp_sibling(path, &ext);
    transcode(path, &tmp, &args)?;
    let new_len = std::fs::metadata(&tmp)?.len();
    if new_len as f64 > m.size as f64 * MIN_GAIN {
        let _ = std::fs::remove_file(&tmp);
        return Ok(None);
    }
    Ok(Some(tmp))
}

fn final_path(m: &Media) -> PathBuf {
    let path = Path::new(&m.path);
    if m.kind != "video" {
        return path.to_path_buf();
    }
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if matches!(ext.as_str(), "mp4" | "mov" | "m4v") {
        path.to_path_buf()
    } else {
        path.with_extension("mp4")
    }
}

fn optimize_one(db: &Db, dirs: &Dirs, m: &Media, opts: &OptimizeOptions) -> Result<ActionResult> {
    let tmp = if m.kind == "video" { optimize_video(m, opts)? } else { optimize_image(m, opts)? };
    let Some(tmp) = tmp else {
        return Ok(ActionResult {
            id: m.id,
            name: m.name.clone(),
            ok: true,
            message: "já está bem comprimido; mantido como está".into(),
            old_size: m.size,
            new_size: m.size,
        });
    };
    let old = PathBuf::from(&m.path);
    let new = final_path(m);
    if new != old && new.exists() {
        let _ = std::fs::remove_file(&tmp);
        bail!("já existe {}", new.display());
    }
    // Original para a lixeira; otimizado assume o lugar, com a mesma data.
    trash::stash(db, dirs, m, "otimizado", Some(&new.to_string_lossy()))?;
    std::fs::rename(&tmp, &new)?;
    let _ = filetime::set_file_mtime(&new, filetime::FileTime::from_unix_time(m.mtime, 0));
    if new != old {
        db.delete_media(m.id)?;
    }
    let id = scan::index_file(db, dirs, m.source_id, &new)?;
    let new_size = std::fs::metadata(&new)?.len();
    let mut message = format!("reduzido em {:.0}%", 100.0 * (1.0 - new_size as f64 / m.size.max(1) as f64));
    if let Err(e) = trash::sync_device(db, m.source_id, Some(&old), Some(&new)) {
        message.push_str(&format!(" (não foi possível atualizar o aparelho: {e})"));
    }
    Ok(ActionResult { id, name: m.name.clone(), ok: true, message, old_size: m.size, new_size })
}

pub fn optimize(db: &Db, dirs: &Dirs, ids: &[i64], opts: &OptimizeOptions, progress: Progress) -> Vec<ActionResult> {
    let mut out = Vec::new();
    for (i, &id) in ids.iter().enumerate() {
        let m = match db.media(id) {
            Ok(Some(m)) => m,
            _ => {
                out.push(ActionResult::fail(id, "", "arquivo não está mais no índice"));
                continue;
            }
        };
        progress(i, ids.len(), &format!("Otimizando {}", m.name));
        out.push(optimize_one(db, dirs, &m, opts).unwrap_or_else(|e| ActionResult::fail(id, &m.name, e.to_string())));
    }
    progress(ids.len(), ids.len(), "Concluído");
    out
}
