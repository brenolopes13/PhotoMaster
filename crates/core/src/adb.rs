//! Acesso a celulares Android pelo cabo USB, via `adb`.
//!
//! O app mantém um espelho local das pastas de mídia do aparelho. Toda a
//! análise roda no espelho; apagar/otimizar/restaurar é replicado no aparelho.

use crate::db::{Db, Source};
use crate::media::command;
use crate::{media_kind, Dirs, Progress};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Pastas do Android onde ficam fotos e vídeos.
pub const DEFAULT_ROOTS: &[&str] = &[
    "/sdcard/DCIM",
    "/sdcard/Pictures",
    "/sdcard/Movies",
    "/sdcard/Android/media/com.whatsapp/WhatsApp/Media",
];

pub fn adb_bin() -> String {
    std::env::var("PHOTOMASTER_ADB").unwrap_or_else(|_| "adb".into())
}

#[derive(Serialize, Debug, Clone)]
pub struct Device {
    pub serial: String,
    /// "device" = pronto; "unauthorized" = falta aceitar o aviso no celular.
    pub state: String,
    pub model: Option<String>,
}

fn run(args: &[&str]) -> Result<std::process::Output> {
    command(&adb_bin())
        .args(args)
        .output()
        .context("adb não encontrado. Instale o Android Platform Tools e deixe o `adb` no PATH.")
}

pub fn devices() -> Result<Vec<Device>> {
    let out = run(&["devices", "-l"])?;
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text
        .lines()
        .skip(1)
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let serial = it.next()?.to_string();
            let state = it.next()?.to_string();
            let model = it.find_map(|t| t.strip_prefix("model:")).map(|m| m.replace('_', " "));
            Some(Device { serial, state, model })
        })
        .collect())
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[derive(Debug, Clone)]
pub struct RemoteEntry {
    pub path: String,
    pub size: u64,
}

pub fn list_media(serial: &str, roots: &[&str]) -> Result<Vec<RemoteEntry>> {
    let quoted: Vec<String> = roots.iter().map(|r| sh_quote(r)).collect();
    let script = format!(
        "find {} -type f -exec stat -c '%s|%n' {{}} + 2>/dev/null",
        quoted.join(" ")
    );
    let out = run(&["-s", serial, "shell", &script])?;
    let text = String::from_utf8_lossy(&out.stdout);
    if text.trim().is_empty() && !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if !err.trim().is_empty() {
            bail!("adb: {}", err.trim());
        }
    }
    Ok(text
        .lines()
        .filter_map(|l| {
            let (size, path) = l.trim_end_matches('\r').split_once('|')?;
            let hidden = path.split('/').any(|seg| seg.starts_with('.'));
            (media_kind(Path::new(path)).is_some() && !hidden)
                .then(|| RemoteEntry { path: path.to_string(), size: size.parse().unwrap_or(0) })
        })
        .collect())
}

fn check(out: std::process::Output, what: &str) -> Result<()> {
    if out.status.success() {
        Ok(())
    } else {
        bail!("adb {what} falhou: {}", String::from_utf8_lossy(&out.stderr).trim())
    }
}

pub fn pull(serial: &str, remote: &str, local: &Path) -> Result<()> {
    if let Some(p) = local.parent() {
        std::fs::create_dir_all(p)?;
    }
    check(run(&["-s", serial, "pull", "-a", remote, &local.to_string_lossy()])?, "pull")
}

pub fn push(serial: &str, local: &Path, remote: &str) -> Result<()> {
    check(run(&["-s", serial, "push", &local.to_string_lossy(), remote])?, "push")
}

pub fn rm(serial: &str, remote: &str) -> Result<()> {
    check(run(&["-s", serial, "shell", &format!("rm -f {}", sh_quote(remote))])?, "rm")
}

pub fn mirror_root(dirs: &Dirs, serial: &str) -> PathBuf {
    let safe: String =
        serial.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    dirs.mirrors.join(safe)
}

/// Caminho no aparelho correspondente a um arquivo do espelho.
/// `None` se a fonte não for um aparelho Android.
pub fn device_path(source: &Source, local: &Path) -> Option<(String, String)> {
    let serial = source.device_serial.clone()?;
    let rel = local.strip_prefix(&source.root).ok()?;
    let remote = format!("/{}", rel.to_string_lossy().replace('\\', "/"));
    Some((serial, remote))
}

/// Copia para o espelho local o que ainda não foi copiado e registra o
/// aparelho como fonte. Não altera nada no aparelho.
pub fn import(db: &Db, dirs: &Dirs, serial: &str, label: &str, progress: Progress) -> Result<Source> {
    progress(0, 0, "Listando arquivos do aparelho…");
    let entries = list_media(serial, DEFAULT_ROOTS)?;
    let root = mirror_root(dirs, serial);
    std::fs::create_dir_all(&root)?;
    let source = db.add_source("adb", label, &root.to_string_lossy(), Some(serial))?;

    let mut expected = HashSet::new();
    let total = entries.len();
    for (i, e) in entries.iter().enumerate() {
        let local = root.join(e.path.trim_start_matches('/'));
        expected.insert(local.clone());
        let up_to_date = std::fs::metadata(&local).map(|m| m.len() == e.size).unwrap_or(false);
        if !up_to_date {
            progress(i, total, &format!("Copiando {}", e.path));
            pull(serial, &e.path, &local)?;
        }
    }
    progress(total, total, "Cópia concluída");

    // Arquivos que sumiram do aparelho desde a última importação vão para a
    // lixeira do app (sem tocar no aparelho), nunca são apagados direto.
    for m in db.media_where(&format!("source_id={}", source.id))? {
        if !expected.contains(Path::new(&m.path)) && Path::new(&m.path).exists() {
            crate::trash::stash(db, dirs, &m, "removido do aparelho", None)?;
            db.delete_media(m.id)?;
        }
    }
    Ok(source)
}
