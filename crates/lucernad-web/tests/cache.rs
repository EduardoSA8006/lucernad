//! O cache em disco: ida e volta, nomes por hash, arquivo estragado, limites.

use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lucernad_web::cache::{Cache, ENTRY_MAX, Kind};
use serde_json::json;
use tempfile::TempDir;

fn t(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

#[test]
fn round_trip_with_saved_time() {
    let tmp = TempDir::new().unwrap();
    let cache = Cache::new(tmp.path().join("web"), 1 << 20);
    let t0 = t(1_792_847_400) + Duration::from_millis(750);
    cache.put(Kind::Lyrics, "k", &json!({"a": 1}), t0).unwrap();
    assert_eq!(
        cache.get(Kind::Lyrics, "k"),
        Some((t(1_792_847_400), json!({"a": 1})))
    );
    assert_eq!(cache.get(Kind::Weather, "k"), None);
}

#[test]
fn keys_become_hashed_file_names() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("web");
    let cache = Cache::new(dir.clone(), 1 << 20);
    cache
        .put(Kind::Lyrics, "Artista/../x", &json!(1), t(100))
        .unwrap();
    let path = cache.path(Kind::Lyrics, "Artista/../x");
    assert_eq!(path.parent().unwrap(), dir.join("lyrics"));
    let name = path.file_name().unwrap().to_str().unwrap();
    let stem = name.strip_suffix(".json").expect("termina em .json");
    assert_eq!(stem.len(), 64);
    assert!(stem.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(path.exists());
    let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
}

#[test]
fn corrupt_entry_is_ignored_and_removed() {
    let tmp = TempDir::new().unwrap();
    let cache = Cache::new(tmp.path().join("web"), 1 << 20);
    cache.put(Kind::Lyrics, "k", &json!(1), t(100)).unwrap();
    let path = cache.path(Kind::Lyrics, "k");
    fs::write(&path, b"\xff lixo {").unwrap();
    assert_eq!(cache.get(Kind::Lyrics, "k"), None);
    assert!(!path.exists());

    fs::write(&path, br#"{"value": 1}"#).unwrap();
    assert_eq!(cache.get(Kind::Lyrics, "k"), None);
    assert!(!path.exists());
}

#[test]
fn oversized_file_is_removed_on_read() {
    let tmp = TempDir::new().unwrap();
    let cache = Cache::new(tmp.path().join("web"), 1 << 20);
    cache.put(Kind::Lyrics, "k", &json!(1), t(100)).unwrap();
    let path = cache.path(Kind::Lyrics, "k");
    fs::write(&path, vec![b' '; ENTRY_MAX + 1]).unwrap();
    assert_eq!(cache.get(Kind::Lyrics, "k"), None);
    assert!(!path.exists());
}

#[test]
fn big_entry_is_not_written() {
    let tmp = TempDir::new().unwrap();
    let cache = Cache::new(tmp.path().join("web"), 1 << 30);
    let big = json!("x".repeat(600 * 1024));
    cache.put(Kind::Lyrics, "k", &big, t(100)).unwrap();
    assert_eq!(cache.get(Kind::Lyrics, "k"), None);
}

#[test]
fn stays_under_the_limit() {
    let tmp = TempDir::new().unwrap();
    let cache = Cache::new(tmp.path().join("web"), 10 * 1024);
    let base = SystemTime::now() - Duration::from_secs(10_000);
    let now = SystemTime::now();
    for i in 0..20u64 {
        let kind = if i % 2 == 0 {
            Kind::Lyrics
        } else {
            Kind::Weather
        };
        let key = format!("k{i}");
        cache
            .put(kind, &key, &json!("y".repeat(1000)), now)
            .unwrap();
        // Envelhece a entrada recém-gravada: a próxima é sempre a mais nova.
        let f = File::options()
            .write(true)
            .open(cache.path(kind, &key))
            .unwrap();
        f.set_modified(base + Duration::from_secs(i)).unwrap();
    }
    cache
        .put(Kind::Lyrics, "ultima", &json!("z".repeat(1000)), now)
        .unwrap();
    let mut total = 0;
    for sub in ["lyrics", "weather"] {
        for e in fs::read_dir(tmp.path().join("web").join(sub)).unwrap() {
            total += e.unwrap().metadata().unwrap().len();
        }
    }
    assert!(total <= 10 * 1024, "total {total}");
    assert!(cache.get(Kind::Lyrics, "ultima").is_some());
    assert!(cache.get(Kind::Lyrics, "k0").is_none());
}

#[test]
fn stale_tmp_files_are_pruned() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("web");
    let cache = Cache::new(dir.clone(), 1 << 20);
    cache
        .put(Kind::Lyrics, "k", &json!(1), SystemTime::now())
        .unwrap();
    let old = dir.join("lyrics/.tmp-velho");
    let fresh = dir.join("lyrics/.tmp-novo");
    fs::write(&old, b"x").unwrap();
    fs::write(&fresh, b"x").unwrap();
    File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(2 * 3600))
        .unwrap();
    cache
        .put(Kind::Lyrics, "k2", &json!(2), SystemTime::now())
        .unwrap();
    assert!(!old.exists());
    assert!(fresh.exists());
}

#[test]
fn remove_deletes_the_entry() {
    let tmp = TempDir::new().unwrap();
    let cache = Cache::new(tmp.path().join("web"), 1 << 20);
    cache.put(Kind::Weather, "k", &json!(1), t(100)).unwrap();
    cache.remove(Kind::Weather, "k");
    assert_eq!(cache.get(Kind::Weather, "k"), None);
    cache.remove(Kind::Weather, "k");
}

#[test]
fn unwritable_dir_is_not_fatal() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("web");
    fs::write(&file, b"nao sou pasta").unwrap();
    let cache = Cache::new(file, 1 << 20);
    assert!(cache.put(Kind::Lyrics, "k", &json!(1), t(100)).is_err());
    assert_eq!(cache.get(Kind::Lyrics, "k"), None);
}
