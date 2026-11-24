//! Lixeira do aplicativo. Nada é apagado de verdade até o usuário esvaziar
//! a lixeira: "excluir" e "otimizar" guardam o original aqui.

use crate::db::{Db, Media, Source};
use crate::{adb, move_file, scan, Dirs};
use anyhow::{anyhow, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Serialize, Debug, Clone)]
pub struct ActionResult {
    pub id: i64,
    pub name: String,
    pub ok: bool,
    pub message: String,
    pub old_size: u64,
    pub new_size: u64,
}

impl ActionResult {
    pub fn fail(id: i64, name: &str, message: impl Into<String>) -> Self {
        ActionResult { id, name: name.into(), ok: false, message: message.into(), old_size: 0, new_size: 0 }
    }
}

fn unique_trash_path(dirs: &Dirs, name: &str) -> PathBuf {
    let mut n = [0u8; 6];
    let _ = getrandom::getrandom(&mut n);
    dirs.trash.join(format!("{}_{}", hex::encode(n), name))
}

/// Move o arquivo de `m` para a lixeira e registra. Não mexe na linha de
/// `media` nem no aparelho: quem chama decide.
pub(crate) fn stash(db: &Db, dirs: &Dirs, m: &Media, reason: &str, replaced: Option<&str>) -> Result<i64> {
    let dest = unique_trash_path(dirs, &m.name);
    move_file(Path::new(&m.path), &dest)?;
    db.add_trash(m.source_id, &m.path, &dest.to_string_lossy(), replaced, &m.name, m.size, &m.kind, &m.sha1, reason)
}

fn device_of(db: &Db, source_id: i64, local: &Path) -> Option<(Source, String, String)> {
    let source = db.source(source_id).ok()??;
    let (serial, remote) = adb::device_path(&source, local)?;
    Some((source, serial, remote))
}

/// Replica no aparelho (se a fonte for um Android) a troca de `old` por `new`.
pub(crate) fn sync_device(db: &Db, source_id: i64, old: Option<&Path>, new: Option<&Path>) -> Result<()> {
    if let Some(new) = new {
        if let Some((_, serial, remote)) = device_of(db, source_id, new) {
            adb::push(&serial, new, &remote)?;
        }
    }
    if let Some(old) = old {
        if Some(old) != new {
            if let Some((_, serial, remote)) = device_of(db, source_id, old) {
                adb::rm(&serial, &remote)?;
            }
        }
    }
    Ok(())
}

/// "Exclui" arquivos: vão para a lixeira e, se vierem de um Android
/// conectado, são removidos do aparelho.
pub fn trash_media(db: &Db, dirs: &Dirs, ids: &[i64], reason: &str) -> Vec<ActionResult> {
    ids.iter()
        .map(|&id| {
            let m = match db.media(id) {
                Ok(Some(m)) => m,
                _ => return ActionResult::fail(id, "", "arquivo não está mais no índice"),
            };
            let r = (|| -> Result<String> {
                stash(db, dirs, &m, reason, None)?;
                db.delete_media(m.id)?;
                match sync_device(db, m.source_id, Some(Path::new(&m.path)), None) {
                    Ok(()) => Ok("movido para a lixeira".into()),
                    Err(e) => Err(anyhow!("está na lixeira do app, mas não foi removido do aparelho: {e}")),
                }
            })();
            match r {
                Ok(msg) => ActionResult { id, name: m.name, ok: true, message: msg, old_size: m.size, new_size: 0 },
                Err(e) => ActionResult::fail(id, &m.name, e.to_string()),
            }
        })
        .collect()
}

/// Devolve itens da lixeira ao lugar de origem (e ao aparelho, se for o caso).
pub fn restore(db: &Db, dirs: &Dirs, trash_ids: &[i64]) -> Vec<ActionResult> {
    trash_ids
        .iter()
        .map(|&id| {
            let t = match db.trash_item(id) {
                Ok(Some(t)) => t,
                _ => return ActionResult::fail(id, "", "item não está na lixeira"),
            };
            let r = (|| -> Result<()> {
                let original = PathBuf::from(&t.original_path);
                // Otimização: o arquivo otimizado dá lugar ao original.
                let replaced = t.replaced_path.as_ref().map(PathBuf::from);
                if let Some(rep) = &replaced {
                    if rep.exists() {
                        std::fs::remove_file(rep)?;
                    }
                    if let Some(m) = db.media_by_path(&rep.to_string_lossy())? {
                        db.delete_media(m.id)?;
                    }
                } else if original.exists() {
                    anyhow::bail!("já existe um arquivo em {}", t.original_path);
                }
                move_file(Path::new(&t.trash_path), &original)?;
                db.delete_trash(t.id)?;
                scan::index_file(db, dirs, t.source_id, &original)?;
                sync_device(db, t.source_id, replaced.as_deref(), Some(&original))?;
                Ok(())
            })();
            match r {
                Ok(()) => ActionResult {
                    id,
                    name: t.name,
                    ok: true,
                    message: "restaurado".into(),
                    old_size: 0,
                    new_size: t.size,
                },
                Err(e) => ActionResult::fail(id, &t.name, e.to_string()),
            }
        })
        .collect()
}

/// Apaga definitivamente tudo o que está na lixeira. Retorna bytes liberados.
pub fn empty(db: &Db) -> Result<u64> {
    let mut freed = 0;
    for t in db.trash_items()? {
        match std::fs::remove_file(&t.trash_path) {
            Ok(()) => freed += t.size,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        db.delete_trash(t.id)?;
    }
    Ok(freed)
}
