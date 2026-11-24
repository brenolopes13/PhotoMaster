//! Fluxo Android com um `adb` falso (script que opera sobre uma pasta).
#![cfg(unix)]

mod common;

use photomaster_core::db::Db;
use photomaster_core::{adb, optimize, scan, trash, Dirs};
use std::os::unix::fs::PermissionsExt;

const FAKE_ADB: &str = r#"#!/bin/bash
# Emula o subconjunto do adb usado pelo PhotoMaster. FAKE_ROOT = "/" do aparelho.
if [ "$1" = devices ]; then
  echo "List of devices attached"
  echo "FAKE123   device product:x model:Pixel_Teste device:y"
  echo "OUTRO456  unauthorized"
  exit 0
fi
[ "$1" = -s ] && shift 2
case "$1" in
  shell) out=$(bash -c "${2//\/sdcard/$FAKE_ROOT/sdcard}"); echo "${out//$FAKE_ROOT/}" ;;
  pull)  [ "$2" = -a ] && shift; cp -p "$FAKE_ROOT$2" "$3" ;;
  push)  mkdir -p "$(dirname "$FAKE_ROOT$3")" && cp -p "$2" "$FAKE_ROOT$3" ;;
  *) echo "comando desconhecido: $1" >&2; exit 1 ;;
esac
"#;

fn noop(_: usize, _: usize, _: &str) {}

#[test]
fn android_mirror_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let phone = tmp.path().join("celular");
    let camera = phone.join("sdcard/DCIM/Camera");
    std::fs::create_dir_all(&camera).unwrap();
    std::fs::create_dir_all(phone.join("sdcard/DCIM/.thumbnails")).unwrap();
    common::save_jpeg(&common::photo(1600, 1200, 11), &camera.join("IMG 0001.jpg"), 99);
    common::save_jpeg(&common::photo(800, 600, 12), &camera.join("IMG_0002.jpg"), 85);
    std::fs::write(phone.join("sdcard/DCIM/.thumbnails/t.jpg"), b"x").unwrap();
    std::fs::write(camera.join("nota.txt"), b"x").unwrap();

    let fake = tmp.path().join("adb");
    std::fs::write(&fake, FAKE_ADB).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("PHOTOMASTER_ADB", &fake);
    std::env::set_var("FAKE_ROOT", &phone);

    let devs = adb::devices().unwrap();
    assert_eq!(devs.len(), 2);
    assert_eq!((devs[0].serial.as_str(), devs[0].state.as_str(), devs[0].model.as_deref()), ("FAKE123", "device", Some("Pixel Teste")));
    assert_eq!(devs[1].state, "unauthorized");

    let dirs = Dirs::new(&tmp.path().join("dados")).unwrap();
    let db = Db::open(&dirs.db_path()).unwrap();
    let source = adb::import(&db, &dirs, "FAKE123", "Pixel Teste", &noop).unwrap();
    let stats = scan::scan_source(&db, &dirs, &source, &noop).unwrap();
    assert_eq!((stats.found, stats.errors), (2, 0), "{stats:?}");
    let by_name = |n: &str| db.all_media().unwrap().into_iter().find(|m| m.name == n).unwrap();

    // Otimizar substitui o arquivo também no aparelho.
    let big = by_name("IMG 0001.jpg");
    let res = optimize::optimize(&db, &dirs, &[big.id], &Default::default(), &noop);
    assert!(res[0].ok && res[0].new_size < big.size / 2, "{res:?}");
    assert_eq!(std::fs::metadata(camera.join("IMG 0001.jpg")).unwrap().len(), res[0].new_size);

    // Excluir remove do aparelho, mas a cópia fica na lixeira do app...
    let small = by_name("IMG_0002.jpg");
    let res = trash::trash_media(&db, &dirs, &[small.id], "excluído");
    assert!(res[0].ok, "{res:?}");
    assert!(!camera.join("IMG_0002.jpg").exists());
    // ...e restaurar devolve ao aparelho.
    let item = db.trash_items().unwrap().into_iter().find(|t| t.name == "IMG_0002.jpg").unwrap();
    assert!(trash::restore(&db, &dirs, &[item.id])[0].ok);
    assert_eq!(std::fs::metadata(camera.join("IMG_0002.jpg")).unwrap().len(), small.size);

    // Foto apagada pelo próprio celular: na reimportação a cópia local vai
    // para a lixeira em vez de sumir.
    std::fs::remove_file(camera.join("IMG_0002.jpg")).unwrap();
    adb::import(&db, &dirs, "FAKE123", "Pixel Teste", &noop).unwrap();
    assert!(db.all_media().unwrap().iter().all(|m| m.name != "IMG_0002.jpg"));
    assert!(db.trash_items().unwrap().iter().any(|t| t.name == "IMG_0002.jpg" && t.reason == "removido do aparelho"));
}
