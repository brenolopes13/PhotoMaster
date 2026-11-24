//! Análises sobre o índice: duplicadas, vídeos descartáveis, candidatos a
//! otimização, fotos borradas e situação do backup.

use crate::db::{Db, Media};
use crate::hashes::hamming;
use anyhow::Result;
use rayon::prelude::*;
use serde::Serialize;
use std::collections::HashMap;

// ---- duplicadas ------------------------------------------------------------

#[derive(Serialize)]
pub struct DupGroup {
    /// true = todos os arquivos são byte a byte idênticos.
    pub exact: bool,
    pub kind: String,
    /// Bytes liberados se só o primeiro item for mantido.
    pub reclaimable: u64,
    /// O primeiro item é a sugestão de qual manter.
    pub items: Vec<Media>,
}

fn similar(a: &Media, b: &Media, threshold: u32) -> bool {
    if a.kind != b.kind {
        return false;
    }
    if a.sha1 == b.sha1 {
        return true;
    }
    let (Some(da), Some(db), Some(aa), Some(ab)) = (a.dhash, b.dhash, a.ahash, b.ahash) else {
        return false;
    };
    if hamming(da, db) > threshold || hamming(aa, ab) > threshold + 4 {
        return false;
    }
    // Mesma proporção e brilho parecido evitam falsos positivos em imagens
    // "lisas" (céu, parede, tela preta), em que os hashes dizem pouco.
    if let (Some(wa), Some(ha), Some(wb), Some(hb)) = (a.width, a.height, b.width, b.height) {
        let (ra, rb) = (wa as f64 / ha.max(1) as f64, wb as f64 / hb.max(1) as f64);
        if (ra - rb).abs() / ra.max(rb) > 0.05 {
            return false;
        }
    }
    if let (Some(ba), Some(bb)) = (a.brightness, b.brightness) {
        if (ba - bb).abs() > 16.0 {
            return false;
        }
    }
    if a.kind == "video" {
        match (a.duration, b.duration) {
            (Some(x), Some(y)) if (x - y).abs() <= 1.0 => {}
            _ => return false,
        }
    }
    true
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

/// Agrupa arquivos idênticos e quase idênticos. `threshold` é a distância de
/// Hamming máxima entre os dHashes (0–64); 5 é conservador, 10 é agressivo.
pub fn duplicates(db: &Db, threshold: u32) -> Result<Vec<DupGroup>> {
    let items = db.all_media()?;
    let n = items.len();
    let v = &items;
    let pairs: Vec<(usize, usize)> = (0..n)
        .into_par_iter()
        .flat_map_iter(|i| {
            (i + 1..n).filter(move |&j| similar(&v[i], &v[j], threshold)).map(move |j| (i, j))
        })
        .collect();

    let mut parent: Vec<usize> = (0..n).collect();
    for (i, j) in pairs {
        let (a, b) = (find(&mut parent, i), find(&mut parent, j));
        if a != b {
            parent[a] = b;
        }
    }
    let mut groups: HashMap<usize, Vec<Media>> = HashMap::new();
    for (i, m) in items.iter().enumerate() {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(m.clone());
    }

    let mut out: Vec<DupGroup> = groups
        .into_values()
        .filter(|g| g.len() > 1)
        .map(|mut g| {
            // Melhor primeiro: mais pixels, mais nítida, maior arquivo; entre
            // idênticas, a mais antiga e de caminho mais curto.
            g.sort_by(|a, b| {
                let px = |m: &Media| m.width.unwrap_or(0) as u64 * m.height.unwrap_or(0) as u64;
                px(b)
                    .cmp(&px(a))
                    .then(b.sharpness.unwrap_or(0.0).total_cmp(&a.sharpness.unwrap_or(0.0)))
                    .then(b.size.cmp(&a.size))
                    .then(a.mtime.cmp(&b.mtime))
                    .then(a.path.len().cmp(&b.path.len()))
            });
            DupGroup {
                exact: g.iter().all(|m| m.sha1 == g[0].sha1),
                kind: g[0].kind.clone(),
                reclaimable: g[1..].iter().map(|m| m.size).sum(),
                items: g,
            }
        })
        .collect();
    out.sort_by(|a, b| b.reclaimable.cmp(&a.reclaimable));
    Ok(out)
}

// ---- vídeos curtos ---------------------------------------------------------

#[derive(Serialize)]
pub struct ShortVideo {
    #[serde(flatten)]
    pub media: Media,
    /// Motivos adicionais para descartar: "escuro", "parado".
    pub flags: Vec<&'static str>,
}

fn video_flags(m: &Media) -> Vec<&'static str> {
    let mut f = Vec::new();
    if m.brightness.is_some_and(|b| b < 18.0) {
        f.push("escuro");
    }
    if m.motion.is_some_and(|x| x <= 2) && m.duration.unwrap_or(0.0) >= 1.0 {
        f.push("parado");
    }
    f
}

/// Vídeos com até `max_secs` segundos, do mais curto para o mais longo.
pub fn short_videos(db: &Db, max_secs: f64) -> Result<Vec<ShortVideo>> {
    let mut v: Vec<ShortVideo> = db
        .media_where("kind='video' AND duration IS NOT NULL")?
        .into_iter()
        .filter(|m| m.duration.unwrap_or(f64::MAX) <= max_secs)
        .map(|m| ShortVideo { flags: video_flags(&m), media: m })
        .collect();
    v.sort_by(|a, b| a.media.duration.unwrap_or(0.0).total_cmp(&b.media.duration.unwrap_or(0.0)));
    Ok(v)
}

// ---- otimização ------------------------------------------------------------

#[derive(Serialize)]
pub struct OptimizeCandidate {
    #[serde(flatten)]
    pub media: Media,
    pub estimated_size: u64,
    pub estimated_saving: u64,
    pub reason: String,
}

/// Bitrate-alvo (bits/s) para HEVC de boa qualidade nesta resolução.
pub fn target_video_bitrate(width: u32, height: u32) -> u64 {
    let px = width as u64 * height as u64;
    match px {
        0..=500_000 => 1_200_000,           // até ~480p
        500_001..=1_000_000 => 2_500_000,   // 720p
        1_000_001..=2_200_000 => 5_000_000, // 1080p
        2_200_001..=4_000_000 => 9_000_000, // 1440p
        _ => 16_000_000,                    // 4K+
    }
}

/// Bytes por pixel acima dos quais um JPEG está "gordo" (qualidade > ~92).
const JPEG_FAT_BPP: f64 = 0.38;
/// Bytes por pixel típicos de um JPEG de qualidade 82.
const JPEG_TARGET_BPP: f64 = 0.20;

pub fn optimize_candidates(db: &Db) -> Result<Vec<OptimizeCandidate>> {
    let mut out = Vec::new();
    for m in db.all_media()? {
        let (Some(w), Some(h)) = (m.width, m.height) else { continue };
        if m.error.is_some() {
            continue;
        }
        let lower = m.name.to_ascii_lowercase();
        if m.kind == "image" && (lower.ends_with(".jpg") || lower.ends_with(".jpeg")) {
            let px = (w as f64 * h as f64).max(1.0);
            let bpp = m.size as f64 / px;
            if bpp > JPEG_FAT_BPP && m.size > 300_000 {
                let est = (px * JPEG_TARGET_BPP) as u64;
                out.push(OptimizeCandidate {
                    estimated_size: est,
                    estimated_saving: m.size.saturating_sub(est),
                    reason: format!("JPEG com compressão muito leve ({bpp:.2} bytes/pixel)"),
                    media: m,
                });
            }
        } else if m.kind == "video" {
            let (Some(br), Some(dur)) = (m.bitrate, m.duration) else { continue };
            let target = target_video_bitrate(w, h);
            if br as f64 > target as f64 * 1.5 && dur > 1.0 && m.size > 2_000_000 {
                let est = (m.size as f64 * target as f64 / br as f64) as u64;
                let codec = m.codec.clone().unwrap_or_else(|| "?".into());
                out.push(OptimizeCandidate {
                    estimated_size: est,
                    estimated_saving: m.size.saturating_sub(est),
                    reason: format!(
                        "{codec} a {:.1} Mbps; H.265 a ~{:.1} Mbps mantém a qualidade",
                        br as f64 / 1e6,
                        target as f64 / 1e6
                    ),
                    media: m,
                });
            }
        }
    }
    out.sort_by(|a, b| b.estimated_saving.cmp(&a.estimated_saving));
    Ok(out)
}

// ---- fotos borradas --------------------------------------------------------

/// Fotos com nitidez abaixo do limite (heurística: variância do Laplaciano).
pub fn blurry(db: &Db, threshold: f64) -> Result<Vec<Media>> {
    let mut v: Vec<Media> = db
        .media_where("kind='image' AND sharpness IS NOT NULL")?
        .into_iter()
        .filter(|m| m.sharpness.unwrap_or(f64::MAX) < threshold)
        .collect();
    v.sort_by(|a, b| a.sharpness.unwrap_or(0.0).total_cmp(&b.sharpness.unwrap_or(0.0)));
    Ok(v)
}

// ---- backup ----------------------------------------------------------------

#[derive(Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "lowercase")]
pub enum BackupStatus {
    /// Não encontrado em nenhuma conta.
    Missing,
    /// Mesmo nome e tamanho, mas o serviço não informou hash para conferir.
    Probable,
    /// Mesmo tamanho e mesmo hash de conteúdo: cópia íntegra confirmada.
    Verified,
}

#[derive(Serialize)]
pub struct BackupItem {
    #[serde(flatten)]
    pub media: Media,
    pub status: BackupStatus,
    /// Onde foi encontrado: "Conta: caminho".
    pub locations: Vec<String>,
}

#[derive(Serialize, Default)]
pub struct Tally {
    pub count: u64,
    pub bytes: u64,
}

#[derive(Serialize, Default)]
pub struct BackupReport {
    pub accounts: usize,
    pub cloud_files: usize,
    pub verified: Tally,
    pub probable: Tally,
    pub missing: Tally,
    pub items: Vec<BackupItem>,
}

struct CloudFile {
    account: String,
    path: String,
    name: String,
    md5: Option<String>,
    sha1: Option<String>,
    sha256: Option<String>,
    quickxor: Option<String>,
}

fn match_status(m: &Media, f: &CloudFile) -> Option<BackupStatus> {
    let eq = |remote: &Option<String>, local: &str| {
        remote.as_deref().map(|r| r.eq_ignore_ascii_case(local))
    };
    // QuickXor é base64: comparação exata.
    let qx = f.quickxor.as_deref().map(|r| r == m.quickxor);
    let checks = [eq(&f.sha256, &m.sha256), eq(&f.sha1, &m.sha1), eq(&f.md5, &m.md5), qx];
    if checks.contains(&Some(true)) {
        Some(BackupStatus::Verified)
    } else if checks.contains(&Some(false)) {
        None // mesmo tamanho, conteúdo diferente
    } else if f.name.eq_ignore_ascii_case(&m.name) {
        Some(BackupStatus::Probable)
    } else {
        None
    }
}

/// Cruza o índice local com os arquivos listados nas contas de nuvem.
pub fn backup_report(db: &Db) -> Result<BackupReport> {
    let accounts = db.accounts()?;
    let labels: HashMap<i64, String> = accounts.iter().map(|a| (a.id, a.label.clone())).collect();
    let mut by_size: HashMap<u64, Vec<CloudFile>> = HashMap::new();
    let mut cloud_files = 0;
    {
        let c = db.conn();
        let mut st =
            c.prepare("SELECT account_id,path,name,size,md5,sha1,sha256,quickxor FROM cloud_files")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(3)? as u64,
                CloudFile {
                    account: labels.get(&r.get::<_, i64>(0)?).cloned().unwrap_or_default(),
                    path: r.get(1)?,
                    name: r.get(2)?,
                    md5: r.get(4)?,
                    sha1: r.get(5)?,
                    sha256: r.get(6)?,
                    quickxor: r.get(7)?,
                },
            ))
        })?;
        for row in rows {
            let (size, f) = row?;
            by_size.entry(size).or_default().push(f);
            cloud_files += 1;
        }
    }

    let mut rep = BackupReport { accounts: accounts.len(), cloud_files, ..Default::default() };
    for m in db.all_media()? {
        let mut status = BackupStatus::Missing;
        let mut locations = Vec::new();
        for f in by_size.get(&m.size).map(|v| v.as_slice()).unwrap_or(&[]) {
            if let Some(s) = match_status(&m, f) {
                status = status.max(s);
                locations.push(format!("{}: {}", f.account, f.path));
            }
        }
        let t = match status {
            BackupStatus::Verified => &mut rep.verified,
            BackupStatus::Probable => &mut rep.probable,
            BackupStatus::Missing => &mut rep.missing,
        };
        t.count += 1;
        t.bytes += m.size;
        rep.items.push(BackupItem { media: m, status, locations });
    }
    Ok(rep)
}

// ---- resumo ----------------------------------------------------------------

#[derive(Serialize, Default)]
pub struct Overview {
    pub images: Tally,
    pub videos: Tally,
    pub errors: u64,
    pub trash: Tally,
}

pub fn overview(db: &Db) -> Result<Overview> {
    let c = db.conn();
    let tally = |sql: &str| -> rusqlite::Result<Tally> {
        c.query_row(sql, [], |r| {
            Ok(Tally { count: r.get::<_, i64>(0)? as u64, bytes: r.get::<_, i64>(1)? as u64 })
        })
    };
    Ok(Overview {
        images: tally("SELECT COUNT(*), COALESCE(SUM(size),0) FROM media WHERE kind='image'")?,
        videos: tally("SELECT COUNT(*), COALESCE(SUM(size),0) FROM media WHERE kind='video'")?,
        errors: c.query_row("SELECT COUNT(*) FROM media WHERE error IS NOT NULL", [], |r| {
            r.get::<_, i64>(0)
        })? as u64,
        trash: tally("SELECT COUNT(*), COALESCE(SUM(size),0) FROM trash")?,
    })
}
