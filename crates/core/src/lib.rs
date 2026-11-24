//! Núcleo do PhotoMaster: indexação, análise e ações sobre fotos e vídeos.
//!
//! Este crate não tem interface: é usado pelo app desktop (servidor local +
//! UI web) e foi pensado para ser reutilizado em apps móveis.

pub mod adb;
pub mod analysis;
pub mod cloud;
pub mod db;
pub mod enhance;
pub mod hashes;
pub mod media;
pub mod optimize;
pub mod scan;
pub mod trash;

use std::path::{Path, PathBuf};

/// Callback de progresso: (feitos, total, mensagem).
pub type Progress<'a> = &'a (dyn Fn(usize, usize, &str) + Sync);

/// Diretórios de dados do aplicativo.
#[derive(Clone, Debug)]
pub struct Dirs {
    pub data: PathBuf,
    pub thumbs: PathBuf,
    pub trash: PathBuf,
    pub mirrors: PathBuf,
}

impl Dirs {
    pub fn new(data: &Path) -> anyhow::Result<Self> {
        let d = Dirs {
            data: data.to_path_buf(),
            thumbs: data.join("thumbs"),
            trash: data.join("lixeira"),
            mirrors: data.join("aparelhos"),
        };
        for p in [&d.data, &d.thumbs, &d.trash, &d.mirrors] {
            std::fs::create_dir_all(p)?;
        }
        Ok(d)
    }

    pub fn db_path(&self) -> PathBuf {
        self.data.join("photomaster.sqlite")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Video,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::Video => "video",
        }
    }
}

/// Classifica um arquivo pela extensão. `None` = não é mídia suportada.
pub fn media_kind(path: &Path) -> Option<Kind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "webp" | "heic" | "heif" | "gif" | "bmp" | "tif" | "tiff" => {
            Some(Kind::Image)
        }
        "mp4" | "mov" | "m4v" | "3gp" | "mkv" | "avi" | "webm" | "mts" => Some(Kind::Video),
        _ => None,
    }
}

/// Move um arquivo, com fallback de copiar+apagar entre sistemas de arquivos.
pub fn move_file(from: &Path, to: &Path) -> anyhow::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)?;
    if let Ok(meta) = std::fs::metadata(from) {
        let _ = filetime::set_file_mtime(to, filetime::FileTime::from_last_modification_time(&meta));
    }
    std::fs::remove_file(from)?;
    Ok(())
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
