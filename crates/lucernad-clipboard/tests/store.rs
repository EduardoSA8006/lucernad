//! O banco: inclusão, deduplicação, fixados, limite, órfãos, sem persistência
//! e busca.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use lucernad_clipboard::mime::Kind;
use lucernad_clipboard::store::{ListQuery, NewClip, Origin, Page, Store, prepare};
use lucernad_clipboard::testing::png_bytes;
use serde_json::{Value, json};
use tempfile::TempDir;

fn disk_at(dir: &Path) -> Store {
    Store::open(Origin::Disk(dir.to_path_buf()), 100).expect("abrir o banco")
}

fn disk(tmp: &TempDir) -> Store {
    disk_at(&tmp.path().join("clipboard"))
}

fn t(s: &str) -> NewClip {
    NewClip::Text(s.into())
}

fn png(side: u32) -> NewClip {
    NewClip::Image {
        mime: "image/png".into(),
        bytes: png_bytes(side, side),
    }
}

fn add(store: &mut Store, clip: NewClip, now: i64) -> (i64, bool) {
    let prepared = prepare(clip, &store.files_dir()).expect("preparar");
    store.insert(prepared, now).expect("inserir")
}

fn all(store: &Store) -> Page {
    list(store, ListQuery::default())
}

fn list(store: &Store, q: ListQuery) -> Page {
    let q = ListQuery {
        limit: if q.limit == 0 { 200 } else { q.limit },
        ..q
    };
    store.list(&q).expect("listar")
}

fn search(store: &Store, query: &str) -> Page {
    list(
        store,
        ListQuery {
            query: Some(query.into()),
            ..ListQuery::default()
        },
    )
}

fn ids(page: &Page) -> Vec<i64> {
    page.items
        .iter()
        .map(|i| i["id"].as_i64().unwrap())
        .collect()
}

fn item(page: &Page, id: i64) -> &Value {
    page.items
        .iter()
        .find(|i| i["id"] == json!(id))
        .expect("item na página")
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

fn find_named(dir: &Path, prefix: &str, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            find_named(&path, prefix, out);
        } else if entry.file_name().to_string_lossy().starts_with(prefix) {
            out.push(path);
        }
    }
}

fn db_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    find_named(dir, "history.db", &mut found);
    found
}

/// Uma pasta em outro sistema de arquivos (o tmpfs), quando houver.
fn other_fs() -> TempDir {
    if Path::new("/dev/shm").is_dir() {
        TempDir::new_in("/dev/shm").expect("pasta em /dev/shm")
    } else {
        TempDir::new().expect("pasta temporária")
    }
}

#[test]
fn adds_and_lists_newest_first() {
    let tmp = TempDir::new().unwrap();
    let mut store = disk(&tmp);
    let (a, new) = add(&mut store, t("a"), 1);
    assert!(new);
    let (b, _) = add(&mut store, t("b"), 2);
    let (c, _) = add(&mut store, t("c"), 3);
    let page = all(&store);
    assert_eq!(ids(&page), vec![c, b, a]);
    assert_eq!(page.total, 3);
    let a = item(&page, a);
    assert_eq!(a["kind"], "text");
    assert_eq!(a["preview"], "a");
    assert_eq!(a["size"], 1);
    assert_eq!(a["lines"], 1);
    assert_eq!(a["created"], 1);
    assert_eq!(a["used"], 1);
    assert_eq!(a["pinned"], false);
    assert!(a.get("matches").is_none());
}

#[test]
fn duplicate_moves_to_top_keeping_id_and_pin() {
    let tmp = TempDir::new().unwrap();
    let mut store = disk(&tmp);
    let (a, _) = add(&mut store, t("a"), 1);
    add(&mut store, t("b"), 2);
    add(&mut store, t("c"), 3);
    assert!(store.pin(a, true).unwrap());
    assert_eq!(add(&mut store, t("a"), 9), (a, false));
    let page = all(&store);
    let item = item(&page, a);
    assert_eq!(item["used"], 9);
    assert_eq!(item["created"], 1);
    assert_eq!(item["pinned"], true);
    assert_eq!(store.counts().unwrap(), (3, 1));
}

#[test]
fn pinned_come_first() {
    let tmp = TempDir::new().unwrap();
    let mut store = disk(&tmp);
    add(&mut store, t("a"), 1);
    let (b, _) = add(&mut store, t("b"), 2);
    add(&mut store, t("c"), 3);
    store.pin(b, true).unwrap();
    assert_eq!(ids(&all(&store))[0], b);
}

#[test]
fn limit_counts_only_unpinned_and_deletes_files() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("clipboard");
    let mut store = Store::open(Origin::Disk(dir.clone()), 2).unwrap();
    let (a, _) = add(&mut store, t("a"), 1);
    store.pin(a, true).unwrap();
    let (old, _) = add(&mut store, png(10), 2);
    let old_image = item(&all(&store), old)["image"]
        .as_str()
        .unwrap()
        .to_owned();
    let (mid, _) = add(&mut store, png(11), 3);
    let (new, _) = add(&mut store, png(12), 4);
    let page = all(&store);
    assert_eq!(ids(&page), vec![a, new, mid]);
    let images = dir.join("images");
    assert_eq!(names(&images).len(), 4);
    assert!(!Path::new(&old_image).exists());
    let old_hash = Path::new(&old_image).file_stem().unwrap().to_string_lossy();
    assert!(!names(&images).iter().any(|n| n.starts_with(&*old_hash)));

    store.set_limit(1).unwrap();
    assert_eq!(ids(&all(&store)), vec![a, new]);
    assert_eq!(names(&images).len(), 2);
}

#[test]
fn same_image_twice_is_one_item_and_one_file() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("clipboard");
    let mut store = disk_at(&dir);
    let (first, new) = add(&mut store, png(300), 1);
    assert!(new);
    assert_eq!(add(&mut store, png(300), 2), (first, false));
    let page = all(&store);
    assert_eq!(page.total, 1);
    let files = names(&dir.join("images"));
    assert_eq!(files.len(), 2);
    let image = &page.items[0];
    let hash = image["image"]
        .as_str()
        .map(Path::new)
        .and_then(Path::file_stem)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        files,
        vec![format!("{hash}.png"), format!("{hash}.thumb.png")]
    );
    assert_eq!(image["kind"], "image");
    assert_eq!(image["mime"], "image/png");
    assert_eq!(image["width"], 300);
    assert_eq!(image["height"], 300);
    assert!(Path::new(image["image"].as_str().unwrap()).exists());
    assert!(Path::new(image["thumb"].as_str().unwrap()).exists());
    assert_eq!(image["used"], 2);
}

#[test]
fn image_without_thumbnail_still_enters() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("clipboard");
    let mut store = disk_at(&dir);
    // Um PNG quebrado: a miniatura falha, o item entra sem ela.
    let (id, _) = add(
        &mut store,
        NewClip::Image {
            mime: "image/png".into(),
            bytes: b"\x89PNG quebrado".to_vec(),
        },
        1,
    );
    let page = all(&store);
    let image = item(&page, id);
    assert!(Path::new(image["image"].as_str().unwrap()).exists());
    assert!(image.get("thumb").is_none());
    assert!(image.get("width").is_none());
    assert_eq!(names(&dir.join("images")).len(), 1);
}

#[test]
fn files_item_lists_names() {
    let tmp = TempDir::new().unwrap();
    let mut store = disk(&tmp);
    let raw = "file:///tmp/a%20b.txt\r\nfile:///tmp/c\r\n";
    let (id, _) = add(&mut store, NewClip::Files(raw.into()), 1);
    add(&mut store, t("outra"), 2);
    let page = all(&store);
    let files = item(&page, id);
    assert_eq!(files["kind"], "files");
    assert_eq!(
        files["uris"],
        json!(["file:///tmp/a%20b.txt", "file:///tmp/c"])
    );
    assert_eq!(files["preview"], "a b.txt, c");
    assert_eq!(files["size"], raw.len());
    assert_eq!(ids(&search(&store, "a b")), vec![id]);
}

#[test]
fn search_ignores_accents_and_marks() {
    let tmp = TempDir::new().unwrap();
    let mut store = disk(&tmp);
    let (first, _) = add(&mut store, t("Reunião de ação"), 1);
    add(&mut store, t("outra coisa"), 2);

    let page = search(&store, "ACAO");
    assert_eq!(ids(&page), vec![first]);
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0]["matches"], json!([[11, 15]]));

    let page = search(&store, "ão");
    assert_eq!(ids(&page), vec![first]);
    assert_eq!(page.items[0]["matches"], json!([[5, 7], [13, 15]]));

    let page = search(&store, "x");
    assert!(page.items.is_empty());
    assert_eq!(page.total, 0);
}

#[test]
fn fts_syntax_is_literal() {
    let tmp = TempDir::new().unwrap();
    let mut store = disk(&tmp);
    let (id, _) = add(&mut store, t("NOT here"), 1);
    add(&mut store, t("outra coisa"), 2);
    assert_eq!(search(&store, "a\"b OR c*").total, 0);
    assert_eq!(ids(&search(&store, "NOT")), vec![id]);
    assert_eq!(ids(&search(&store, "not")), vec![id]);
}

#[test]
fn pagination_and_kind_filter() {
    let tmp = TempDir::new().unwrap();
    let mut store = Store::open(Origin::Disk(tmp.path().join("clipboard")), 1000).unwrap();
    for n in 0..120 {
        add(&mut store, t(&format!("texto {n}")), n);
    }
    add(&mut store, png(10), 200);
    add(&mut store, png(11), 201);
    let page = list(
        &store,
        ListQuery {
            offset: 100,
            limit: 50,
            ..ListQuery::default()
        },
    );
    assert_eq!(page.items.len(), 22);
    assert_eq!(page.total, 122);

    let images = list(
        &store,
        ListQuery {
            kind: Some(Kind::Image),
            ..ListQuery::default()
        },
    );
    assert_eq!(images.total, 2);
    assert!(images.items.iter().all(|i| i["kind"] == "image"));

    for query in ["png", "te", "texto"] {
        let found = search(&store, query);
        assert!(found.items.iter().all(|i| i["kind"] != "image"), "{query}");
    }
    let images_by_query = list(
        &store,
        ListQuery {
            kind: Some(Kind::Image),
            query: Some("image".into()),
            ..ListQuery::default()
        },
    );
    assert_eq!(images_by_query.total, 0);
}

#[test]
fn orphans_are_removed_on_open() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("clipboard");
    let mut store = disk_at(&dir);
    add(&mut store, png(20), 1);
    drop(store);
    let images = dir.join("images");
    let kept = names(&images);
    assert_eq!(kept.len(), 2);
    fs::write(images.join("zzz.png"), b"sobra").unwrap();
    let store = disk_at(&dir);
    assert_eq!(names(&images), kept);
    assert_eq!(all(&store).total, 1);
}

#[test]
fn memory_store_never_touches_disk() {
    let tmp = TempDir::new().unwrap();
    let rt = tmp.path().join("runtime").join("clipboard");
    let d = tmp.path().join("data").join("clipboard");
    let mut store = Store::open(
        Origin::Memory {
            files: rt.clone(),
            disk: d.clone(),
        },
        100,
    )
    .unwrap();
    add(&mut store, png(30), 1);
    add(&mut store, t("texto"), 2);
    assert_eq!(names(&rt.join("images")).len(), 2);
    assert!(names(&d).is_empty());
    assert_eq!(all(&store).total, 2);
    drop(store);
    assert!(names(&rt.join("images")).is_empty());
    assert!(db_files(tmp.path()).is_empty());
}

#[test]
fn memory_open_wipes_the_disk() {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path().join("data").join("clipboard");
    let mut store = disk_at(&d);
    add(&mut store, png(40), 1);
    add(&mut store, t("texto"), 2);
    drop(store);
    assert!(!db_files(&d).is_empty());
    assert_eq!(names(&d.join("images")).len(), 2);

    let rt = tmp.path().join("runtime").join("clipboard");
    let store = Store::open(
        Origin::Memory {
            files: rt,
            disk: d.clone(),
        },
        100,
    )
    .unwrap();
    assert!(db_files(&d).is_empty());
    assert!(names(&d.join("images")).is_empty());
    assert_eq!(all(&store).total, 0);
}

#[test]
fn back_to_disk_over_leftover_files() {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path().join("data").join("clipboard");
    let rt = tmp.path().join("runtime").join("clipboard");
    let mut store = Store::open(
        Origin::Memory {
            files: rt,
            disk: d.clone(),
        },
        100,
    )
    .unwrap();
    add(&mut store, t("um"), 1);
    add(&mut store, t("dois"), 2);
    // Sobras de uma queda.
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("history.db"), b"lixo").unwrap();
    fs::write(d.join("history.db-wal"), b"lixo").unwrap();
    store.move_to(Origin::Disk(d.clone())).expect("ir ao disco");
    assert_eq!(all(&store).total, 2);
    drop(store);
    let store = disk_at(&d);
    assert_eq!(all(&store).total, 2);
}

#[test]
fn prepare_needs_no_store() {
    let tmp = TempDir::new().unwrap();
    // Sem nenhum banco: só a pasta.
    let alone = tmp.path().join("sozinho");
    prepare(png(2000), &alone).expect("preparar sem banco");
    assert_eq!(names(&alone.join("images")).len(), 2);
    assert!(db_files(tmp.path()).is_empty());

    // O `insert` só faz a transação: os arquivos do `prepare` ficam como estão.
    let dir = tmp.path().join("clipboard");
    let mut store = disk_at(&dir);
    let prepared = prepare(png(2000), &dir).expect("preparar");
    let images = dir.join("images");
    let files = names(&images);
    assert_eq!(files.len(), 2);
    let mtimes = |files: &[String]| -> Vec<_> {
        files
            .iter()
            .map(|n| fs::metadata(images.join(n)).unwrap().modified().unwrap())
            .collect()
    };
    let before = mtimes(&files);
    std::thread::sleep(std::time::Duration::from_millis(20));
    let (id, new) = store.insert(prepared, 1).unwrap();
    assert!(new);
    assert_eq!(names(&images), files);
    assert_eq!(mtimes(&files), before);
    assert_eq!(item(&all(&store), id)["width"], 2000);
}

#[test]
fn persist_off_then_on_moves_items_and_files() {
    let tmp = TempDir::new().unwrap();
    let shm = other_fs();
    let d = tmp.path().join("clipboard");
    let rt = shm.path().join("clipboard");
    let memory = Origin::Memory {
        files: rt.clone(),
        disk: d.clone(),
    };

    let mut store = disk_at(&d);
    add(&mut store, t("antigo"), 1);
    add(&mut store, png(50), 2);

    store.move_to(memory).expect("desligar");
    assert!(db_files(&d).is_empty());
    assert!(names(&d.join("images")).is_empty());
    assert_eq!(all(&store).total, 0);

    let (text, _) = add(&mut store, t("novo"), 3);
    let (image, _) = add(&mut store, png(60), 4);
    store.pin(text, true).unwrap();
    assert_eq!(names(&rt.join("images")).len(), 2);

    store.move_to(Origin::Disk(d.clone())).expect("religar");
    let page = all(&store);
    assert_eq!(ids(&page), vec![text, image]);
    assert_eq!(item(&page, text)["pinned"], true);
    let moved = item(&page, image);
    assert!(
        moved["image"]
            .as_str()
            .unwrap()
            .starts_with(d.to_str().unwrap())
    );
    assert!(Path::new(moved["thumb"].as_str().unwrap()).exists());
    assert_eq!(names(&d.join("images")).len(), 2);
    assert!(names(&rt.join("images")).is_empty());
    drop(store);

    let store = disk_at(&d);
    assert_eq!(ids(&all(&store)), vec![text, image]);
    assert_eq!(names(&d.join("images")).len(), 2);
}

#[test]
fn survives_reopen() {
    let tmp = TempDir::new().unwrap();
    let mut store = disk(&tmp);
    let (a, _) = add(&mut store, t("a"), 1);
    let (b, _) = add(&mut store, t("b"), 2);
    store.pin(a, true).unwrap();
    drop(store);
    let store = disk(&tmp);
    let page = all(&store);
    assert_eq!(ids(&page), vec![a, b]);
    assert_eq!(item(&page, a)["pinned"], true);
}

#[test]
fn get_content_touch_remove_and_clear() {
    let tmp = TempDir::new().unwrap();
    let mut store = disk(&tmp);
    let long = "linha\n".repeat(500);
    let (text, _) = add(&mut store, t(&long), 1);
    let (files, _) = add(&mut store, NewClip::Files("file:///x\n".into()), 2);
    let (image, _) = add(&mut store, png(8), 3);

    let got = store.get(text).unwrap().unwrap();
    assert_eq!(got["text"], long.as_str());
    assert_eq!(got["lines"], 500);
    assert_eq!(
        store.get(files).unwrap().unwrap()["uris"],
        json!(["file:///x"])
    );
    assert!(store.get(999).unwrap().is_none());

    let (kind, mime, bytes) = store.content(image).unwrap().unwrap();
    assert_eq!((kind, mime.as_deref()), (Kind::Image, Some("image/png")));
    assert_eq!(bytes, png_bytes(8, 8));
    let (kind, _, bytes) = store.content(text).unwrap().unwrap();
    assert_eq!((kind, bytes), (Kind::Text, long.clone().into_bytes()));
    let (kind, _, bytes) = store.content(files).unwrap().unwrap();
    assert_eq!((kind, bytes), (Kind::Files, b"file:///x\n".to_vec()));
    assert!(store.content(999).unwrap().is_none());

    assert!(store.touch(text, 10).unwrap());
    assert!(!store.touch(999, 10).unwrap());
    assert_eq!(ids(&all(&store))[0], text);

    assert!(store.remove(image).unwrap());
    assert!(!store.remove(image).unwrap());
    assert_eq!(names(&store.files_dir().join("images")).len(), 0);
    assert!(!store.pin(image, true).unwrap());

    store.pin(files, true).unwrap();
    assert_eq!(store.clear(false).unwrap(), 1);
    assert_eq!(ids(&all(&store)), vec![files]);
    assert_eq!(store.clear(true).unwrap(), 1);
    assert_eq!(store.counts().unwrap(), (0, 0));
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn memory_in(tmp: &TempDir) -> (Store, PathBuf, PathBuf) {
    let rt = tmp.path().join("runtime").join("clipboard");
    let d = tmp.path().join("data").join("clipboard");
    let store = Store::open(
        Origin::Memory {
            files: rt.clone(),
            disk: d.clone(),
        },
        100,
    )
    .unwrap();
    (store, rt, d)
}

#[test]
fn prepared_before_persist_off_leaves_nothing_on_disk() {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path().join("data").join("clipboard");
    let rt = tmp.path().join("runtime").join("clipboard");
    let mut store = disk_at(&d);
    // A cópia começa com o disco ligado...
    let stale = store.files_dir();
    store
        .move_to(Origin::Memory {
            files: rt.clone(),
            disk: d.clone(),
        })
        .unwrap();
    // ...e o `prepare` termina depois do `wipe`, gravando na pasta velha.
    let prepared = prepare(png(70), &stale).unwrap();
    assert_eq!(names(&d.join("images")).len(), 2);
    let (id, _) = store.insert(prepared, 1).unwrap();
    assert!(!d.join("images").exists());
    assert!(db_files(&d).is_empty());
    assert_eq!(names(&rt.join("images")).len(), 2);
    let page = all(&store);
    assert!(Path::new(item(&page, id)["image"].as_str().unwrap()).starts_with(&rt));
    assert!(Path::new(item(&page, id)["thumb"].as_str().unwrap()).exists());
}

#[test]
fn failed_move_to_disk_keeps_the_memory() {
    let tmp = TempDir::new().unwrap();
    let (mut store, rt, _) = memory_in(&tmp);
    add(&mut store, t("um"), 1);
    let (image, _) = add(&mut store, png(80), 2);

    // O destino numa pasta sem permissão de escrita.
    let locked = tmp.path().join("trancada");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).unwrap();
    let target = locked.join("clipboard");
    assert!(store.move_to(Origin::Disk(target.clone())).is_err());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(all(&store).total, 2);
    assert!(!target.exists());

    // A falha no meio (depois do `VACUUM INTO`): uma imagem que não se lê.
    let d = tmp.path().join("outro").join("clipboard");
    let image_path = PathBuf::from(item(&all(&store), image)["image"].as_str().unwrap());
    fs::set_permissions(&image_path, fs::Permissions::from_mode(0o000)).unwrap();
    let failed = store.move_to(Origin::Disk(d.clone()));
    fs::set_permissions(&image_path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(failed.is_err());
    assert!(db_files(&d).is_empty());
    assert!(names(&d.join("images")).is_empty());
    assert_eq!(all(&store).total, 2);
    assert_eq!(names(&rt.join("images")).len(), 2);
    assert!(store.content(image).unwrap().is_some());

    // E, depois, a troca funciona.
    store.move_to(Origin::Disk(d.clone())).unwrap();
    assert_eq!(all(&store).total, 2);
}

#[test]
fn history_files_are_private() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("clipboard");
    let mut store = disk_at(&dir);
    add(&mut store, t("texto"), 1);
    add(&mut store, png(90), 2);
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&dir.join("images")), 0o700);
    assert_eq!(mode(&dir.join("history.db")), 0o600);
    assert_eq!(mode(&dir.join("history.db-wal")), 0o600);
    for name in names(&dir.join("images")) {
        assert_eq!(mode(&dir.join("images").join(&name)), 0o600, "{name}");
    }

    // Os que vêm da memória também.
    let (mut memory, _, d) = memory_in(&tmp);
    add(&mut memory, png(91), 1);
    memory.move_to(Origin::Disk(d.clone())).unwrap();
    assert_eq!(mode(&d.join("history.db")), 0o600);
    for name in names(&d.join("images")) {
        assert_eq!(mode(&d.join("images").join(&name)), 0o600, "{name}");
    }
}

#[test]
fn unknown_kind_in_the_database_is_an_error() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("clipboard");
    let mut store = disk_at(&dir);
    let (id, _) = add(&mut store, t("texto"), 1);
    drop(store);
    let conn = rusqlite::Connection::open(dir.join("history.db")).unwrap();
    conn.execute("UPDATE items SET kind = 'outro' WHERE id = ?1", [id])
        .unwrap();
    drop(conn);
    let store = disk_at(&dir);
    let failed = store.list(&ListQuery {
        limit: 10,
        ..ListQuery::default()
    });
    assert!(failed.unwrap_err().contains("outro"));
    assert!(store.get(id).is_err());
}

#[test]
fn image_files_are_written_before_the_commit() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("clipboard");
    let mut store = disk_at(&dir);
    // Preparada em outra pasta; no lugar da pasta do banco, um arquivo.
    let prepared = prepare(png(95), &tmp.path().join("outra")).unwrap();
    let images = dir.join("images");
    fs::remove_dir(&images).unwrap();
    fs::write(&images, b"no caminho").unwrap();
    assert!(store.insert(prepared, 1).is_err());
    assert_eq!(store.counts().unwrap(), (0, 0));
}

/// Se `needle` aparece em algum arquivo do banco em `dir`.
fn on_disk(dir: &Path, needle: &[u8]) -> Vec<String> {
    ["history.db", "history.db-wal"]
        .iter()
        .filter(|name| {
            let bytes = fs::read(dir.join(name)).unwrap_or_default();
            bytes.windows(needle.len()).any(|w| w == needle)
        })
        .map(|name| name.to_string())
        .collect()
}

#[test]
fn removed_items_leave_no_bytes_on_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("clipboard");
    let mut store = disk_at(&dir);
    let gone = "senha-apagada-123 que não pode sobrar";
    let cleared = "outra-apagada-456 que também não";
    let (id, _) = add(&mut store, t(gone), 1);
    add(&mut store, t(cleared), 2);
    add(&mut store, t("fica no histórico"), 3);
    let (kept, _) = add(&mut store, t("fixado e mantido"), 4);
    store.pin(kept, true).unwrap();
    assert!(
        !on_disk(&dir, b"senha-apagada-123").is_empty(),
        "o teste enxerga o texto"
    );

    store.remove(id).unwrap();
    assert_eq!(on_disk(&dir, b"senha-apagada-123"), Vec::<String>::new());
    store.clear(false).unwrap();
    assert_eq!(on_disk(&dir, b"outra-apagada-456"), Vec::<String>::new());
    assert_eq!(on_disk(&dir, b"fica no hist"), Vec::<String>::new());
    assert_eq!(ids(&all(&store)), [kept]);
    assert_eq!(search(&store, "senha").total, 0);
}
