//! Índice local em SQLite.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

pub struct Db {
    conn: Mutex<Connection>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Source {
    pub id: i64,
    /// "folder" (pasta local/dispositivo montado) ou "adb" (espelho de Android).
    pub kind: String,
    pub label: String,
    pub root: String,
    pub device_serial: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct Media {
    pub id: i64,
    pub source_id: i64,
    pub path: String,
    pub name: String,
    pub size: u64,
    pub mtime: i64,
    pub kind: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration: Option<f64>,
    pub codec: Option<String>,
    pub bitrate: Option<u64>,
    pub taken_at: Option<String>,
    pub sha1: String,
    #[serde(skip)]
    pub md5: String,
    #[serde(skip)]
    pub sha256: String,
    #[serde(skip)]
    pub quickxor: String,
    #[serde(skip)]
    pub dhash: Option<u64>,
    #[serde(skip)]
    pub ahash: Option<u64>,
    pub sharpness: Option<f64>,
    pub brightness: Option<f64>,
    /// Vídeos: maior diferença perceptual entre quadros amostrados (0–64).
    pub motion: Option<u32>,
    pub error: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct TrashItem {
    pub id: i64,
    pub source_id: i64,
    pub original_path: String,
    #[serde(skip)]
    pub trash_path: String,
    /// Arquivo que substituiu o original (otimização); removido ao restaurar.
    #[serde(skip)]
    pub replaced_path: Option<String>,
    pub name: String,
    pub size: u64,
    pub kind: String,
    pub sha1: String,
    pub reason: String,
    pub created_at: i64,
}

#[derive(Serialize, Clone, Debug)]
pub struct Account {
    pub id: i64,
    pub provider: String,
    pub label: String,
    #[serde(skip)]
    pub config: String,
    pub last_sync: Option<i64>,
    pub files: u64,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sources(
  id INTEGER PRIMARY KEY, kind TEXT NOT NULL, label TEXT NOT NULL,
  root TEXT NOT NULL UNIQUE, device_serial TEXT);
CREATE TABLE IF NOT EXISTS media(
  id INTEGER PRIMARY KEY, source_id INTEGER NOT NULL, path TEXT NOT NULL UNIQUE,
  name TEXT NOT NULL, size INTEGER NOT NULL, mtime INTEGER NOT NULL, kind TEXT NOT NULL,
  width INTEGER, height INTEGER, duration REAL, codec TEXT, bitrate INTEGER, taken_at TEXT,
  sha1 TEXT NOT NULL, md5 TEXT NOT NULL, sha256 TEXT NOT NULL, quickxor TEXT NOT NULL,
  dhash INTEGER, ahash INTEGER, sharpness REAL, brightness REAL, motion INTEGER, error TEXT);
CREATE INDEX IF NOT EXISTS media_sha1 ON media(sha1);
CREATE INDEX IF NOT EXISTS media_source ON media(source_id);
CREATE TABLE IF NOT EXISTS trash(
  id INTEGER PRIMARY KEY, source_id INTEGER NOT NULL, original_path TEXT NOT NULL,
  trash_path TEXT NOT NULL, replaced_path TEXT, name TEXT NOT NULL, size INTEGER NOT NULL,
  kind TEXT NOT NULL, sha1 TEXT NOT NULL, reason TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS cloud_accounts(
  id INTEGER PRIMARY KEY, provider TEXT NOT NULL, label TEXT NOT NULL,
  config TEXT NOT NULL, last_sync INTEGER);
CREATE TABLE IF NOT EXISTS cloud_files(
  account_id INTEGER NOT NULL, path TEXT NOT NULL, name TEXT NOT NULL, size INTEGER NOT NULL,
  md5 TEXT, sha1 TEXT, sha256 TEXT, quickxor TEXT);
CREATE INDEX IF NOT EXISTS cloud_files_account ON cloud_files(account_id);
";

const MEDIA_COLS: &str = "id, source_id, path, name, size, mtime, kind, width, height, duration, \
  codec, bitrate, taken_at, sha1, md5, sha256, quickxor, dhash, ahash, sharpness, brightness, motion, error";

fn media_from_row(r: &Row) -> rusqlite::Result<Media> {
    Ok(Media {
        id: r.get(0)?,
        source_id: r.get(1)?,
        path: r.get(2)?,
        name: r.get(3)?,
        size: r.get::<_, i64>(4)? as u64,
        mtime: r.get(5)?,
        kind: r.get(6)?,
        width: r.get(7)?,
        height: r.get(8)?,
        duration: r.get(9)?,
        codec: r.get(10)?,
        bitrate: r.get::<_, Option<i64>>(11)?.map(|v| v as u64),
        taken_at: r.get(12)?,
        sha1: r.get(13)?,
        md5: r.get(14)?,
        sha256: r.get(15)?,
        quickxor: r.get(16)?,
        dhash: r.get::<_, Option<i64>>(17)?.map(|v| v as u64),
        ahash: r.get::<_, Option<i64>>(18)?.map(|v| v as u64),
        sharpness: r.get(19)?,
        brightness: r.get(20)?,
        motion: r.get(21)?,
        error: r.get(22)?,
    })
}

fn source_from_row(r: &Row) -> rusqlite::Result<Source> {
    Ok(Source {
        id: r.get(0)?,
        kind: r.get(1)?,
        label: r.get(2)?,
        root: r.get(3)?,
        device_serial: r.get(4)?,
    })
}

fn trash_from_row(r: &Row) -> rusqlite::Result<TrashItem> {
    Ok(TrashItem {
        id: r.get(0)?,
        source_id: r.get(1)?,
        original_path: r.get(2)?,
        trash_path: r.get(3)?,
        replaced_path: r.get(4)?,
        name: r.get(5)?,
        size: r.get::<_, i64>(6)? as u64,
        kind: r.get(7)?,
        sha1: r.get(8)?,
        reason: r.get(9)?,
        created_at: r.get(10)?,
    })
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Db { conn: Mutex::new(conn) })
    }

    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---- fontes -----------------------------------------------------------

    pub fn add_source(&self, kind: &str, label: &str, root: &str, serial: Option<&str>) -> Result<Source> {
        let c = self.conn();
        c.execute(
            "INSERT INTO sources(kind,label,root,device_serial) VALUES(?,?,?,?)
             ON CONFLICT(root) DO UPDATE SET label=excluded.label",
            params![kind, label, root, serial],
        )?;
        Ok(c.query_row(
            "SELECT id,kind,label,root,device_serial FROM sources WHERE root=?",
            [root],
            source_from_row,
        )?)
    }

    pub fn sources(&self) -> Result<Vec<Source>> {
        let c = self.conn();
        let mut st = c.prepare("SELECT id,kind,label,root,device_serial FROM sources ORDER BY id")?;
        let v = st.query_map([], source_from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }

    pub fn source(&self, id: i64) -> Result<Option<Source>> {
        Ok(self
            .conn()
            .query_row("SELECT id,kind,label,root,device_serial FROM sources WHERE id=?", [id], source_from_row)
            .optional()?)
    }

    /// Remove a fonte do índice. Não toca em nenhum arquivo.
    pub fn remove_source(&self, id: i64) -> Result<()> {
        let c = self.conn();
        c.execute("DELETE FROM media WHERE source_id=?", [id])?;
        c.execute("DELETE FROM sources WHERE id=?", [id])?;
        Ok(())
    }

    // ---- mídia ------------------------------------------------------------

    pub fn upsert_media(&self, m: &Media) -> Result<i64> {
        let c = self.conn();
        c.execute(
            "INSERT INTO media(source_id,path,name,size,mtime,kind,width,height,duration,codec,bitrate,
               taken_at,sha1,md5,sha256,quickxor,dhash,ahash,sharpness,brightness,motion,error)
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
             ON CONFLICT(path) DO UPDATE SET source_id=excluded.source_id, name=excluded.name,
               size=excluded.size, mtime=excluded.mtime, kind=excluded.kind, width=excluded.width,
               height=excluded.height, duration=excluded.duration, codec=excluded.codec,
               bitrate=excluded.bitrate, taken_at=excluded.taken_at, sha1=excluded.sha1,
               md5=excluded.md5, sha256=excluded.sha256, quickxor=excluded.quickxor,
               dhash=excluded.dhash, ahash=excluded.ahash, sharpness=excluded.sharpness,
               brightness=excluded.brightness, motion=excluded.motion, error=excluded.error",
            params![
                m.source_id, m.path, m.name, m.size as i64, m.mtime, m.kind, m.width, m.height,
                m.duration, m.codec, m.bitrate.map(|v| v as i64), m.taken_at, m.sha1, m.md5,
                m.sha256, m.quickxor, m.dhash.map(|v| v as i64), m.ahash.map(|v| v as i64),
                m.sharpness, m.brightness, m.motion, m.error
            ],
        )?;
        Ok(c.query_row("SELECT id FROM media WHERE path=?", [&m.path], |r| r.get(0))?)
    }

    pub fn media(&self, id: i64) -> Result<Option<Media>> {
        Ok(self
            .conn()
            .query_row(&format!("SELECT {MEDIA_COLS} FROM media WHERE id=?"), [id], media_from_row)
            .optional()?)
    }

    pub fn media_by_path(&self, path: &str) -> Result<Option<Media>> {
        Ok(self
            .conn()
            .query_row(&format!("SELECT {MEDIA_COLS} FROM media WHERE path=?"), [path], media_from_row)
            .optional()?)
    }

    /// Lista mídia com um filtro SQL fixo (nunca vindo do usuário).
    pub fn media_where(&self, clause: &str) -> Result<Vec<Media>> {
        let c = self.conn();
        let mut st = c.prepare(&format!("SELECT {MEDIA_COLS} FROM media WHERE {clause}"))?;
        let v = st.query_map([], media_from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }

    pub fn all_media(&self) -> Result<Vec<Media>> {
        self.media_where("1=1 ORDER BY path")
    }

    pub fn delete_media(&self, id: i64) -> Result<()> {
        self.conn().execute("DELETE FROM media WHERE id=?", [id])?;
        Ok(())
    }

    // ---- lixeira ----------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn add_trash(
        &self,
        source_id: i64,
        original_path: &str,
        trash_path: &str,
        replaced_path: Option<&str>,
        name: &str,
        size: u64,
        kind: &str,
        sha1: &str,
        reason: &str,
    ) -> Result<i64> {
        let c = self.conn();
        c.execute(
            "INSERT INTO trash(source_id,original_path,trash_path,replaced_path,name,size,kind,sha1,reason,created_at)
             VALUES(?,?,?,?,?,?,?,?,?,?)",
            params![source_id, original_path, trash_path, replaced_path, name, size as i64, kind, sha1, reason, crate::now_secs()],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn trash_items(&self) -> Result<Vec<TrashItem>> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT id,source_id,original_path,trash_path,replaced_path,name,size,kind,sha1,reason,created_at
             FROM trash ORDER BY id DESC",
        )?;
        let v = st.query_map([], trash_from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }

    pub fn trash_item(&self, id: i64) -> Result<Option<TrashItem>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id,source_id,original_path,trash_path,replaced_path,name,size,kind,sha1,reason,created_at
                 FROM trash WHERE id=?",
                [id],
                trash_from_row,
            )
            .optional()?)
    }

    pub fn delete_trash(&self, id: i64) -> Result<()> {
        self.conn().execute("DELETE FROM trash WHERE id=?", [id])?;
        Ok(())
    }

    // ---- nuvem ------------------------------------------------------------

    pub fn add_account(&self, provider: &str, label: &str, config: &str) -> Result<i64> {
        let c = self.conn();
        c.execute(
            "INSERT INTO cloud_accounts(provider,label,config) VALUES(?,?,?)",
            params![provider, label, config],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn accounts(&self) -> Result<Vec<Account>> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT a.id,a.provider,a.label,a.config,a.last_sync,
                    (SELECT COUNT(*) FROM cloud_files f WHERE f.account_id=a.id)
             FROM cloud_accounts a ORDER BY a.id",
        )?;
        let v = st
            .query_map([], |r| {
                Ok(Account {
                    id: r.get(0)?,
                    provider: r.get(1)?,
                    label: r.get(2)?,
                    config: r.get(3)?,
                    last_sync: r.get(4)?,
                    files: r.get::<_, i64>(5)? as u64,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }

    pub fn account(&self, id: i64) -> Result<Option<Account>> {
        Ok(self.accounts()?.into_iter().find(|a| a.id == id))
    }

    pub fn update_account_config(&self, id: i64, config: &str) -> Result<()> {
        self.conn()
            .execute("UPDATE cloud_accounts SET config=? WHERE id=?", params![config, id])?;
        Ok(())
    }

    pub fn remove_account(&self, id: i64) -> Result<()> {
        let c = self.conn();
        c.execute("DELETE FROM cloud_files WHERE account_id=?", [id])?;
        c.execute("DELETE FROM cloud_accounts WHERE id=?", [id])?;
        Ok(())
    }
}
