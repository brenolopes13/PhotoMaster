//! Google Drive (API v3). Lista imagens e vídeos com seus checksums; só
//! precisa do escopo de metadados, sem acesso ao conteúdo dos arquivos.

use super::{http_err, RemoteFile};
use anyhow::Result;

const DRIVE: &str = "https://www.googleapis.com/drive/v3";

pub(crate) fn parse_page(v: &serde_json::Value, out: &mut Vec<RemoteFile>) {
    for f in v["files"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
        let Some(name) = f["name"].as_str() else { continue };
        // `size` vem como string na API do Drive.
        let size = f["size"].as_str().and_then(|s| s.parse().ok()).or_else(|| f["size"].as_u64()).unwrap_or(0);
        let h = |k: &str| f[k].as_str().map(|s| s.to_ascii_lowercase());
        out.push(RemoteFile {
            path: name.to_string(),
            name: name.to_string(),
            size,
            md5: h("md5Checksum"),
            sha1: h("sha1Checksum"),
            sha256: h("sha256Checksum"),
            quickxor: None,
        });
    }
}

pub fn list(access_token: &str, api_base: Option<&str>, report: &dyn Fn(usize)) -> Result<Vec<RemoteFile>> {
    let base = api_base.unwrap_or(DRIVE);
    let mut out = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let mut req = ureq::get(&format!("{base}/files"))
            .set("Authorization", &format!("Bearer {access_token}"))
            .query("q", "trashed = false and (mimeType contains 'image/' or mimeType contains 'video/')")
            .query("fields", "nextPageToken,files(name,size,md5Checksum,sha1Checksum,sha256Checksum)")
            .query("pageSize", "1000")
            .query("spaces", "drive");
        if let Some(t) = &page_token {
            req = req.query("pageToken", t);
        }
        let v: serde_json::Value = req.call().map_err(http_err)?.into_json()?;
        parse_page(&v, &mut out);
        report(out.len());
        match v["nextPageToken"].as_str() {
            Some(t) => page_token = Some(t.to_string()),
            None => break,
        }
    }
    Ok(out)
}
