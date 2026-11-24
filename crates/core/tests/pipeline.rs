//! Teste de ponta a ponta: varredura, análises, lixeira, otimização,
//! melhoria e conferência de backup contra servidores simulados.

mod common;

use photomaster_core::analysis::{self, BackupStatus};
use photomaster_core::cloud::{self, CloudConfig};
use photomaster_core::db::Db;
use photomaster_core::{enhance, hashes, optimize, scan, trash, Dirs};
use std::path::Path;

fn noop(_: usize, _: usize, _: &str) {}

fn sha1_of(p: &Path) -> String {
    hashes::content_hashes(p).unwrap().sha1
}

/// Servidor falso que responde como Nextcloud (WebDAV), Microsoft Graph,
/// Google Drive e o endpoint de token OAuth.
fn mock_cloud(a_sha1: String, a_size: u64, b_size: u64, video_qx: String, video_size: u64, c_md5: String, c_size: u64) -> String {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}", server.server_addr());
    let base2 = base.clone();
    std::thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let url = req.url().to_string();
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let authorized = req.headers().iter().any(|h| {
                h.field.equiv("Authorization")
                    && (h.value.as_str() == "Basic YW5hOnNlbmhhLWFwcA==" || h.value.as_str() == "Bearer tok-acesso")
            });
            let (code, text) = if url == "/token" {
                assert!(body.contains("grant_type=refresh_token") && body.contains("refresh_token=rt-1"));
                (200, r#"{"access_token":"tok-acesso","refresh_token":"rt-2"}"#.to_string())
            } else if !authorized {
                (401, "nao autorizado".to_string())
            } else if req.method().as_str() == "PROPFIND" && url == "/remote.php/dav/files/ana/" {
                (207, format!(r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
<d:response><d:href>/remote.php/dav/files/ana/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>
<d:response><d:href>/remote.php/dav/files/ana/Fotos%20Celular/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>
<d:response><d:href>/remote.php/dav/files/ana/notas.txt</d:href><d:propstat><d:prop><d:resourcetype/><d:getcontentlength>10</d:getcontentlength></d:prop></d:propstat></d:response>
</d:multistatus>"#))
            } else if req.method().as_str() == "PROPFIND" && url == "/remote.php/dav/files/ana/Fotos%20Celular/" {
                (207, format!(r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
<d:response><d:href>/remote.php/dav/files/ana/Fotos%20Celular/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>
<d:response><d:href>/remote.php/dav/files/ana/Fotos%20Celular/a.jpg</d:href><d:propstat><d:prop><d:resourcetype/><d:getcontentlength>{a_size}</d:getcontentlength><oc:checksums><oc:checksum>SHA1:{a_sha1}</oc:checksum></oc:checksums></d:prop></d:propstat></d:response>
<d:response><d:href>/remote.php/dav/files/ana/Fotos%20Celular/b.jpg</d:href><d:propstat><d:prop><d:resourcetype/><d:getcontentlength>{b_size}</d:getcontentlength></d:prop></d:propstat></d:response>
</d:multistatus>"#))
            } else if url.starts_with("/graph/me/drive/root/delta") {
                (200, serde_json::json!({
                    "value": [
                        {"name": "Vídeos", "folder": {}, "parentReference": {"path": "/drive/root:"}},
                        {"name": "renomeado-na-nuvem.mp4", "size": video_size, "parentReference": {"path": "/drive/root:/Vídeos"},
                         "file": {"hashes": {"quickXorHash": video_qx}}},
                        {"name": "apagado.jpg", "size": 5, "file": {}, "deleted": {"state": "deleted"}}
                    ],
                    "@odata.nextLink": format!("{base2}/graph/pagina2")
                }).to_string())
            } else if url == "/graph/pagina2" {
                (200, r#"{"value":[]}"#.to_string())
            } else if url.starts_with("/drive/files") {
                (200, serde_json::json!({"files": [
                    {"name": "c.jpg", "size": c_size.to_string(), "md5Checksum": c_md5},
                    // Mesmo nome e tamanho de b.jpg, mas conteúdo diferente: não conta como backup.
                    {"name": "b.jpg", "size": b_size.to_string(), "md5Checksum": "00000000000000000000000000000000"}
                ]}).to_string())
            } else {
                (404, format!("rota desconhecida {url}"))
            };
            let _ = req.respond(tiny_http::Response::from_string(text).with_status_code(code));
        }
    });
    base
}

#[test]
fn full_pipeline() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = Dirs::new(&tmp.path().join("dados")).unwrap();
    let db = Db::open(&dirs.db_path()).unwrap();
    let fotos = tmp.path().join("fotos");
    std::fs::create_dir_all(fotos.join("sub")).unwrap();

    // --- acervo sintético ---------------------------------------------------
    let a = common::photo(1600, 1200, 1);
    common::save_jpeg(&a, &fotos.join("a.jpg"), 99); // "gorda": candidata a otimização
    std::fs::copy(fotos.join("a.jpg"), fotos.join("sub/a_copia.jpg")).unwrap();
    let a_small = image::imageops::resize(&a, 800, 600, image::imageops::FilterType::Triangle);
    common::save_jpeg(&a_small, &fotos.join("a_menor.jpg"), 80);
    common::save_jpeg(&common::photo(1200, 900, 2), &fotos.join("b.jpg"), 85);
    let c = common::photo(1200, 900, 3);
    common::save_jpeg(&c, &fotos.join("c.jpg"), 85);
    common::save_jpeg(&image::imageops::blur(&common::photo(1200, 900, 4), 9.0), &fotos.join("borrada.jpg"), 85);
    common::video(&fotos.join("curto.mp4"), "testsrc2=size=320x240:rate=15:duration=1.5,rotate=angle=t*2", &[]);
    common::video(&fotos.join("escuro.mp4"), "color=c=black:size=320x240:rate=15:duration=2", &[]);
    common::video(
        &fotos.join("longo.mp4"),
        "testsrc2=size=640x480:rate=30:duration=6,rotate=angle=t/3",
        &["-c:v", "libx264", "-preset", "ultrafast", "-qp", "6"],
    );
    std::fs::write(fotos.join("leia-me.txt"), "não é mídia").unwrap();
    std::fs::create_dir_all(fotos.join(".thumbnails")).unwrap();
    std::fs::copy(fotos.join("b.jpg"), fotos.join(".thumbnails/x.jpg")).unwrap();

    // --- varredura ----------------------------------------------------------
    let source = db.add_source("folder", "Teste", &fotos.to_string_lossy(), None).unwrap();
    let stats = scan::scan_source(&db, &dirs, &source, &noop).unwrap();
    assert_eq!((stats.found, stats.indexed, stats.errors), (9, 9, 0), "{stats:?}");
    let again = scan::scan_source(&db, &dirs, &source, &noop).unwrap();
    assert_eq!((again.indexed, again.unchanged), (0, 9));

    let by_name = |n: &str| db.all_media().unwrap().into_iter().find(|m| m.name == n).unwrap_or_else(|| panic!("{n}"));
    let ma = by_name("a.jpg");
    assert_eq!((ma.width, ma.height), (Some(1600), Some(1200)));
    assert!(scan::thumb_path(&dirs, &ma.sha1).exists());
    let longo = by_name("longo.mp4");
    assert_eq!(longo.codec.as_deref(), Some("h264"));
    assert!((longo.duration.unwrap() - 6.0).abs() < 0.3);

    let ov = analysis::overview(&db).unwrap();
    assert_eq!((ov.images.count, ov.videos.count), (6, 3));

    // --- duplicadas ---------------------------------------------------------
    let groups = analysis::duplicates(&db, 6).unwrap();
    assert_eq!(groups.len(), 1, "grupos: {:?}", groups.iter().map(|g| g.items.iter().map(|m| &m.name).collect::<Vec<_>>()).collect::<Vec<_>>());
    let names: Vec<&str> = groups[0].items.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names.len(), 3);
    assert_eq!(*names.last().unwrap(), "a_menor.jpg", "a versão menor deve ser a última sugestão");
    assert!(!groups[0].exact);

    // --- vídeos curtos, borradas, otimização ---------------------------------
    let shorts = analysis::short_videos(&db, 3.0).unwrap();
    let short_names: Vec<&str> = shorts.iter().map(|s| s.media.name.as_str()).collect();
    assert_eq!(short_names, ["curto.mp4", "escuro.mp4"]);
    assert!(shorts[1].flags.contains(&"escuro") && shorts[1].flags.contains(&"parado"));
    assert!(shorts[0].flags.is_empty(), "{:?}", shorts[0].flags);

    let blurry: Vec<String> = analysis::blurry(&db, 40.0).unwrap().into_iter().map(|m| m.name).collect();
    assert_eq!(blurry, ["borrada.jpg"], "nitidez: {:?}", db.all_media().unwrap().iter().map(|m| (&m.name, m.sharpness)).collect::<Vec<_>>());

    let cands = analysis::optimize_candidates(&db).unwrap();
    let cand_names: Vec<&str> = cands.iter().map(|c| c.media.name.as_str()).collect();
    assert!(cand_names.contains(&"a.jpg") && cand_names.contains(&"longo.mp4"), "{cand_names:?}");
    assert!(!cand_names.contains(&"b.jpg"));

    // --- backup (antes de alterar arquivos) ----------------------------------
    let mb = by_name("b.jpg");
    let mc = by_name("c.jpg");
    let base = mock_cloud(ma.sha1.clone(), ma.size, mb.size, longo.quickxor.clone(), longo.size, mc.md5.clone(), mc.size);
    let nc = cloud::add_account(&db, "Nextcloud", &CloudConfig::Nextcloud { url: base.clone(), user: "ana".into(), password: "senha-app".into() }).unwrap();
    let od = cloud::add_account(&db, "OneDrive", &CloudConfig::Onedrive {
        client_id: "cid".into(), refresh_token: "rt-1".into(),
        api_base: Some(format!("{base}/graph")), token_url: Some(format!("{base}/token")),
    }).unwrap();
    let gd = cloud::add_account(&db, "Google Drive", &CloudConfig::Gdrive {
        client_id: "cid".into(), client_secret: "sec".into(), refresh_token: "rt-1".into(),
        api_base: Some(format!("{base}/drive")), token_url: Some(format!("{base}/token")),
    }).unwrap();
    let sync = |id| cloud::sync_account(&db, &db.account(id).unwrap().unwrap(), &noop).unwrap();
    assert_eq!(sync(nc), 2, "txt deve ser ignorado");
    assert_eq!(sync(od), 1);
    assert_eq!(sync(gd), 2);
    // O refresh token rotacionado pelo servidor deve ter sido salvo.
    assert!(db.account(od).unwrap().unwrap().config.contains("rt-2"));

    let rep = analysis::backup_report(&db).unwrap();
    let status = |n: &str| rep.items.iter().find(|i| i.media.name == n).unwrap().status;
    assert_eq!(status("a.jpg"), BackupStatus::Verified); // SHA1 no Nextcloud
    assert_eq!(status("a_copia.jpg"), BackupStatus::Verified); // mesmo conteúdo
    assert_eq!(status("b.jpg"), BackupStatus::Probable); // nome+tamanho no Nextcloud; Drive tem hash diferente
    assert_eq!(status("c.jpg"), BackupStatus::Verified); // MD5 no Google Drive
    assert_eq!(status("longo.mp4"), BackupStatus::Verified); // QuickXor no OneDrive, nome diferente
    assert_eq!(status("curto.mp4"), BackupStatus::Missing);
    assert_eq!((rep.verified.count, rep.probable.count, rep.missing.count), (4, 1, 4));

    // Senha errada dá erro claro, sem apagar a lista anterior.
    let bad = cloud::add_account(&db, "Errada", &CloudConfig::Nextcloud { url: base.clone(), user: "ana".into(), password: "x".into() }).unwrap();
    let err = cloud::sync_account(&db, &db.account(bad).unwrap().unwrap(), &noop).unwrap_err().to_string();
    assert!(err.contains("acesso negado"), "{err}");

    // --- lixeira ------------------------------------------------------------
    let res = trash::trash_media(&db, &dirs, &[mb.id], "excluído");
    assert!(res[0].ok, "{:?}", res[0]);
    assert!(!fotos.join("b.jpg").exists());
    assert!(db.media(mb.id).unwrap().is_none());
    let item = &db.trash_items().unwrap()[0];
    let res = trash::restore(&db, &dirs, &[item.id]);
    assert!(res[0].ok, "{:?}", res[0]);
    assert_eq!(sha1_of(&fotos.join("b.jpg")), mb.sha1);
    assert!(db.trash_items().unwrap().is_empty());
    assert_eq!(by_name("b.jpg").sha1, mb.sha1);

    // --- otimização ---------------------------------------------------------
    let ids = [ma.id, longo.id, by_name("b.jpg").id];
    let res = optimize::optimize(&db, &dirs, &ids, &optimize::OptimizeOptions::default(), &noop);
    assert!(res.iter().all(|r| r.ok), "{res:?}");
    assert!(res[0].new_size < ma.size / 2, "{:?}", res[0]);
    assert!(res[1].new_size < longo.size / 2, "{:?}", res[1]);
    assert_eq!(res[2].new_size, res[2].old_size, "b.jpg já estava bem comprimida");
    assert_eq!(sha1_of(&fotos.join("b.jpg")), mb.sha1);
    let a_opt = by_name("a.jpg");
    assert_eq!((a_opt.width, a_opt.height, a_opt.mtime), (Some(1600), Some(1200), ma.mtime));
    assert_eq!(by_name("longo.mp4").codec.as_deref(), Some("hevc"));
    // Depois de otimizada, a.jpg continua sendo vista como parecida com a cópia.
    assert_eq!(analysis::duplicates(&db, 6).unwrap()[0].items.len(), 3);

    // Restaurar devolve exatamente o original.
    let t = db.trash_items().unwrap();
    assert_eq!(t.len(), 2);
    let a_trash = t.iter().find(|i| i.name == "a.jpg").unwrap();
    assert!(trash::restore(&db, &dirs, &[a_trash.id])[0].ok);
    assert_eq!(sha1_of(&fotos.join("a.jpg")), ma.sha1);
    assert_eq!(by_name("a.jpg").sha1, ma.sha1);

    // --- melhoria -----------------------------------------------------------
    let res = enhance::enhance(&db, &dirs, &[mc.id, by_name("curto.mp4").id], &enhance::EnhanceOptions::default(), &noop);
    assert!(res.iter().all(|r| r.ok), "{res:?}");
    assert!(fotos.join("c_melhorada.jpg").exists() && fotos.join("curto_melhorado.mp4").exists());
    assert_eq!(sha1_of(&fotos.join("c.jpg")), mc.sha1, "o original não pode ser alterado");
    assert_eq!(by_name("c_melhorada.jpg").width, Some(1200));

    // --- esvaziar lixeira ---------------------------------------------------
    let freed = trash::empty(&db).unwrap();
    assert_eq!(freed, longo.size);
    assert_eq!(std::fs::read_dir(&dirs.trash).unwrap().count(), 0);
}

#[test]
fn jpeg_recompress_keeps_exif() {
    // JPEG mínimo com um segmento EXIF injetado.
    let img = common::photo(640, 480, 7);
    let mut plain = Vec::new();
    img.write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut plain, 98)).unwrap();
    use img_parts::ImageEXIF;
    let mut j = img_parts::jpeg::Jpeg::from_bytes(plain.into()).unwrap();
    let exif = img_parts::Bytes::from_static(b"II*\0\x08\0\0\0\0\0\0\0\0\0");
    j.set_exif(Some(exif.clone()));
    let mut with_exif = Vec::new();
    j.encoder().write_to(&mut with_exif).unwrap();

    let out = optimize::recompress_jpeg(&with_exif, 80, Some(320)).unwrap();
    assert!(out.len() < with_exif.len());
    let j2 = img_parts::jpeg::Jpeg::from_bytes(out.clone().into()).unwrap();
    assert_eq!(j2.exif(), Some(exif));
    let small = image::load_from_memory(&out).unwrap();
    assert_eq!((small.width(), small.height()), (320, 240));
}
