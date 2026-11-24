//! Nextcloud (e qualquer servidor WebDAV): listagem recursiva via PROPFIND.

use super::{http_err, RemoteFile};
use crate::media_kind;
use anyhow::{bail, Result};
use base64::Engine;
use percent_encoding::{percent_decode_str, utf8_percent_encode, NON_ALPHANUMERIC};
use std::collections::{HashSet, VecDeque};
use std::path::Path;

const PROPFIND_BODY: &str = r#"<?xml version="1.0"?>
<d:propfind xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
  <d:prop><d:resourcetype/><d:getcontentlength/><oc:checksums/></d:prop>
</d:propfind>"#;

/// URL da raiz WebDAV. Aceita tanto o endereço do Nextcloud
/// (`https://nuvem.exemplo.com`) quanto uma URL WebDAV completa.
pub fn dav_root(url: &str, user: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    if url.contains("/remote.php/") || url.contains("/dav") {
        format!("{url}/")
    } else {
        format!("{url}/remote.php/dav/files/{}/", utf8_percent_encode(user, NON_ALPHANUMERIC))
    }
}

/// `https://host:porta` de uma URL.
fn origin(url: &str) -> &str {
    let after = url.find("://").map(|i| i + 3).unwrap_or(0);
    match url[after..].find('/') {
        Some(i) => &url[..after + i],
        None => url,
    }
}

fn decode(href: &str) -> String {
    percent_decode_str(href).decode_utf8_lossy().into_owned()
}

#[derive(Debug, PartialEq)]
pub(crate) struct Entry {
    pub href: String,
    pub is_dir: bool,
    pub size: u64,
    pub sha1: Option<String>,
    pub md5: Option<String>,
    pub sha256: Option<String>,
}

pub(crate) fn parse_multistatus(xml: &str) -> Result<Vec<Entry>> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| anyhow::anyhow!("resposta WebDAV inválida: {e}"))?;
    let mut out = Vec::new();
    for resp in doc.descendants().filter(|n| n.has_tag_name(("DAV:", "response"))) {
        let Some(href) = resp.children().find(|n| n.has_tag_name(("DAV:", "href"))).and_then(|n| n.text()) else {
            continue;
        };
        let mut e = Entry { href: href.trim().to_string(), is_dir: false, size: 0, sha1: None, md5: None, sha256: None };
        for n in resp.descendants() {
            match n.tag_name().name() {
                "collection" => e.is_dir = true,
                "getcontentlength" => e.size = n.text().and_then(|t| t.trim().parse().ok()).unwrap_or(0),
                // Ex.: "SHA1:abc MD5:def ADLER32:123"
                "checksum" => {
                    for part in n.text().unwrap_or("").split_whitespace() {
                        if let Some((algo, value)) = part.split_once(':') {
                            let value = Some(value.to_ascii_lowercase());
                            match algo.to_ascii_uppercase().as_str() {
                                "SHA1" => e.sha1 = value,
                                "MD5" => e.md5 = value,
                                "SHA256" => e.sha256 = value,
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        out.push(e);
    }
    Ok(out)
}

fn auth_header(user: &str, password: &str) -> String {
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}")))
}

pub fn list(url: &str, user: &str, password: &str, report: &dyn Fn(usize)) -> Result<Vec<RemoteFile>> {
    let root = dav_root(url, user);
    if !(root.starts_with("https://") || root.starts_with("http://")) {
        bail!("o endereço deve começar com https://");
    }
    let host = origin(&root).to_string();
    let root_path = decode(&root[host.len()..]);
    let auth = auth_header(user, password);

    let mut files = Vec::new();
    let mut queue = VecDeque::from([root[host.len()..].to_string()]);
    let mut seen = HashSet::new();
    while let Some(dir) = queue.pop_front() {
        if !seen.insert(decode(&dir)) {
            continue;
        }
        let resp = ureq::request("PROPFIND", &format!("{host}{dir}"))
            .set("Depth", "1")
            .set("Authorization", &auth)
            .set("Content-Type", "application/xml")
            .send_string(PROPFIND_BODY)
            .map_err(http_err)?;
        let body = resp.into_string()?;
        for e in parse_multistatus(&body)? {
            // Alguns servidores devolvem href absoluto (com host).
            let href = e.href.strip_prefix(host.as_str()).unwrap_or(&e.href).to_string();
            let path = decode(&href);
            if e.is_dir {
                if path.trim_end_matches('/') != decode(&dir).trim_end_matches('/') {
                    queue.push_back(href);
                }
                continue;
            }
            if media_kind(Path::new(&path)).is_none() {
                continue;
            }
            let rel = path.strip_prefix(&root_path).unwrap_or(&path).trim_start_matches('/').to_string();
            files.push(RemoteFile {
                name: rel.rsplit('/').next().unwrap_or(&rel).to_string(),
                path: rel,
                size: e.size,
                md5: e.md5,
                sha1: e.sha1,
                sha256: e.sha256,
                quickxor: None,
            });
        }
        report(files.len());
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots() {
        assert_eq!(dav_root("https://n.ex.com/", "ana b"), "https://n.ex.com/remote.php/dav/files/ana%20b/");
        assert_eq!(dav_root("https://x.com/remote.php/dav/files/u", "u"), "https://x.com/remote.php/dav/files/u/");
        assert_eq!(origin("https://x.com:8443/a/b"), "https://x.com:8443");
    }

    #[test]
    fn parses_nextcloud_response() {
        let xml = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
 <d:response><d:href>/remote.php/dav/files/u/Fotos/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
 </d:response>
 <d:response><d:href>/remote.php/dav/files/u/Fotos/f%C3%A9rias.jpg</d:href>
  <d:propstat><d:prop><d:resourcetype/><d:getcontentlength>1234</d:getcontentlength>
   <oc:checksums><oc:checksum>SHA1:ABCDEF MD5:0123 ADLER32:9</oc:checksum></oc:checksums></d:prop>
   <d:status>HTTP/1.1 200 OK</d:status></d:propstat>
 </d:response>
</d:multistatus>"#;
        let v = parse_multistatus(xml).unwrap();
        assert_eq!(v.len(), 2);
        assert!(v[0].is_dir);
        assert_eq!(v[1].size, 1234);
        assert_eq!(v[1].sha1.as_deref(), Some("abcdef"));
        assert_eq!(v[1].md5.as_deref(), Some("0123"));
        assert_eq!(decode(&v[1].href), "/remote.php/dav/files/u/Fotos/férias.jpg");
    }
}
