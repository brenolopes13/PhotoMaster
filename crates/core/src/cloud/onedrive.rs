//! OneDrive via Microsoft Graph. Usa a consulta `delta` da raiz, que devolve
//! todos os itens do drive em páginas, com os hashes de cada arquivo.

use super::{http_err, RemoteFile};
use crate::media_kind;
use anyhow::Result;
use std::path::Path;

const GRAPH: &str = "https://graph.microsoft.com/v1.0";

pub(crate) fn parse_page(v: &serde_json::Value, out: &mut Vec<RemoteFile>) {
    for item in v["value"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
        if item["file"].is_null() || !item["deleted"].is_null() {
            continue;
        }
        let Some(name) = item["name"].as_str() else { continue };
        if media_kind(Path::new(name)).is_none() {
            continue;
        }
        // parentReference.path = "/drive/root:/Imagens/Câmera"
        let parent = item["parentReference"]["path"].as_str().unwrap_or("");
        let parent = parent.split_once(':').map(|(_, p)| p).unwrap_or("").trim_start_matches('/');
        let hashes = &item["file"]["hashes"];
        let h = |k: &str| hashes[k].as_str().map(|s| s.to_string());
        out.push(RemoteFile {
            path: if parent.is_empty() { name.to_string() } else { format!("{parent}/{name}") },
            name: name.to_string(),
            size: item["size"].as_u64().unwrap_or(0),
            md5: None,
            sha1: h("sha1Hash").map(|s| s.to_ascii_lowercase()),
            sha256: h("sha256Hash").map(|s| s.to_ascii_lowercase()),
            quickxor: h("quickXorHash"),
        });
    }
}

pub fn list(access_token: &str, api_base: Option<&str>, report: &dyn Fn(usize)) -> Result<Vec<RemoteFile>> {
    let base = api_base.unwrap_or(GRAPH);
    let mut url = format!("{base}/me/drive/root/delta?$select=name,size,file,deleted,parentReference&$top=1000");
    let mut out = Vec::new();
    loop {
        let v: serde_json::Value = ureq::get(&url)
            .set("Authorization", &format!("Bearer {access_token}"))
            .call()
            .map_err(http_err)?
            .into_json()?;
        parse_page(&v, &mut out);
        report(out.len());
        match v["@odata.nextLink"].as_str() {
            Some(next) => url = next.to_string(),
            None => break,
        }
    }
    Ok(out)
}
