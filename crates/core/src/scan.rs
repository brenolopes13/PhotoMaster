//! Varredura de uma fonte: encontra fotos e vídeos e grava no índice os
//! metadados, hashes e a miniatura de cada um.

use crate::db::{Db, Media, Source};
use crate::{hashes, media, media_kind, Dirs, Kind, Progress};
use anyhow::Result;
use image::DynamicImage;
use rayon::prelude::*;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Serialize, Default, Debug, Clone)]
pub struct ScanStats {
    pub found: usize,
    pub indexed: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub errors: usize,
}

fn mtime_secs(meta: &std::fs::Metadata) -> i64 {
    filetime::FileTime::from_last_modification_time(meta).unix_seconds()
}

pub fn thumb_path(dirs: &Dirs, sha1: &str) -> PathBuf {
    dirs.thumbs.join(format!("{sha1}.jpg"))
}

fn save_thumb(dirs: &Dirs, sha1: &str, small: &DynamicImage) {
    let p = thumb_path(dirs, sha1);
    if p.exists() {
        return;
    }
    let t = small.thumbnail(320, 320).to_rgb8();
    let tmp = p.with_extension("tmp");
    if let Ok(mut f) = std::fs::File::create(&tmp) {
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut f, 78);
        if t.write_with_encoder(enc).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
            return;
        }
    }
    let _ = std::fs::remove_file(&tmp);
}

/// Analisa um arquivo. Erros de decodificação não abortam: o arquivo entra no
/// índice com o campo `error`, pois os hashes de conteúdo continuam úteis.
pub fn analyze_file(dirs: &Dirs, source_id: i64, path: &Path) -> Result<Media> {
    let kind = media_kind(path).ok_or_else(|| anyhow::anyhow!("extensão não suportada"))?;
    let meta = std::fs::metadata(path)?;
    let h = hashes::content_hashes(path)?;
    let mut m = Media {
        source_id,
        path: path.to_string_lossy().into_owned(),
        name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        size: meta.len(),
        mtime: mtime_secs(&meta),
        kind: kind.as_str().into(),
        sha1: h.sha1,
        md5: h.md5,
        sha256: h.sha256,
        quickxor: h.quickxor,
        ..Default::default()
    };

    match kind {
        Kind::Image => {
            m.taken_at = media::exif_taken_at(path);
            let small = match media::load_image_oriented(path) {
                Ok(img) => {
                    m.width = Some(img.width());
                    m.height = Some(img.height());
                    Ok(img.thumbnail(512, 512))
                }
                // HEIC e afins: tenta via ffmpeg.
                Err(e) => match media::ffmpeg_frame(path, 0.0) {
                    Ok(img) => {
                        if let Ok(p) = media::ffprobe(path) {
                            m.width = p.width;
                            m.height = p.height;
                            m.codec = p.codec;
                        }
                        Ok(img)
                    }
                    Err(_) => Err(e),
                },
            };
            match small {
                Ok(small) => {
                    m.dhash = Some(hashes::dhash(&small));
                    m.ahash = Some(hashes::ahash(&small));
                    m.sharpness = Some(hashes::sharpness(&small));
                    m.brightness = Some(hashes::brightness(&small));
                    save_thumb(dirs, &m.sha1, &small);
                }
                Err(e) => m.error = Some(format!("não foi possível decodificar: {e}")),
            }
        }
        Kind::Video => match media::ffprobe(path) {
            Ok(p) => {
                m.width = p.width;
                m.height = p.height;
                m.duration = p.duration;
                m.codec = p.codec;
                m.bitrate = p.bitrate;
                // Três quadros (10%, 50%, 90%) medem brilho e movimento.
                let dur = p.duration.unwrap_or(0.0);
                let frames: Vec<DynamicImage> = [0.1, 0.5, 0.9]
                    .iter()
                    .filter_map(|f| media::ffmpeg_frame(path, dur * f).ok())
                    .collect();
                if let Some(first) = frames.first() {
                    let hs: Vec<u64> = frames.iter().map(hashes::dhash).collect();
                    m.dhash = Some(hs[0]);
                    m.ahash = Some(hashes::ahash(first));
                    m.sharpness = Some(hashes::sharpness(first));
                    m.brightness =
                        frames.iter().map(hashes::brightness).fold(None, |a: Option<f64>, b| {
                            Some(a.map_or(b, |a| a.max(b)))
                        });
                    m.motion = hs.iter().map(|h| hashes::hamming(hs[0], *h)).max();
                    save_thumb(dirs, &m.sha1, first);
                } else {
                    m.error = Some("não foi possível ler quadros do vídeo".into());
                }
            }
            Err(e) => m.error = Some(e.to_string()),
        },
    }
    Ok(m)
}

/// (Re)indexa um único arquivo.
pub fn index_file(db: &Db, dirs: &Dirs, source_id: i64, path: &Path) -> Result<i64> {
    let m = analyze_file(dirs, source_id, path)?;
    db.upsert_media(&m)
}

fn walk(root: &Path, dirs: &Dirs) -> Vec<PathBuf> {
    walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            // Pula a lixeira do app, miniaturas do Android e pastas ocultas.
            !(e.path() == dirs.trash || (e.depth() > 0 && name.starts_with('.')))
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && media_kind(e.path()).is_some())
        .map(|e| e.into_path())
        .collect()
}

/// Varre uma fonte. Arquivos já indexados com mesmo tamanho e data são
/// pulados, então varreduras repetidas são rápidas.
pub fn scan_source(db: &Db, dirs: &Dirs, source: &Source, progress: Progress) -> Result<ScanStats> {
    let root = Path::new(&source.root);
    if !root.is_dir() {
        anyhow::bail!("pasta não encontrada: {}", source.root);
    }
    progress(0, 0, "Procurando arquivos…");
    let files = walk(root, dirs);
    let mut stats = ScanStats { found: files.len(), ..Default::default() };

    let known: std::collections::HashMap<String, (u64, i64, i64)> = db
        .media_where(&format!("source_id={}", source.id))?
        .into_iter()
        .map(|m| (m.path, (m.size, m.mtime, m.id)))
        .collect();

    let on_disk: HashSet<String> = files.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    for (path, (_, _, id)) in &known {
        if !on_disk.contains(path) {
            db.delete_media(*id)?;
            stats.removed += 1;
        }
    }

    let todo: Vec<&PathBuf> = files
        .iter()
        .filter(|p| {
            let key = p.to_string_lossy();
            match (known.get(key.as_ref()), std::fs::metadata(p)) {
                (Some((size, mtime, _)), Ok(meta)) => *size != meta.len() || *mtime != mtime_secs(&meta),
                _ => true,
            }
        })
        .collect();
    stats.unchanged = files.len() - todo.len();

    let total = todo.len();
    let mut done = 0;
    for chunk in todo.chunks(16) {
        let results: Vec<(PathBuf, Result<Media>)> = chunk
            .par_iter()
            .map(|p| ((*p).clone(), analyze_file(dirs, source.id, p)))
            .collect();
        for (path, r) in results {
            match r {
                Ok(m) => {
                    if m.error.is_some() {
                        stats.errors += 1;
                    }
                    db.upsert_media(&m)?;
                    stats.indexed += 1;
                }
                Err(_) => stats.errors += 1,
            }
            done += 1;
            progress(done, total, &path.to_string_lossy());
        }
    }
    Ok(stats)
}
