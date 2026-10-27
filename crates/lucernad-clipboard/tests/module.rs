//! O módulo `clipboard` num servidor de verdade, com o `FakeBackend`: o laço
//! guiado pelo assinante, pelo `configure` e pela seleção nossa, os tópicos e
//! os métodos.

use std::path::{Path, PathBuf};
use std::time::Duration;

use lucerna_ipc::{Client, ClientError};
use lucerna_service::{ServerBuilder, WorkTracker};
use lucernad_clipboard::mime::{OWN_MIME, READ_TIMEOUT};
use lucernad_clipboard::pipe::NO_DISPLAY;
use lucernad_clipboard::store::{NewClip, Origin, Store, prepare};
use lucernad_clipboard::testing::{FakeBackend, fake_config, png_bytes};
use lucernad_clipboard::wayland::disconnected;
use lucernad_clipboard::{Clipboard, ClipboardConfig, ClipboardTopics, register};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::UnixListener;
use tokio::task::JoinHandle;
use tokio::time::Instant;

const TEXT: &str = "text/plain;charset=utf-8";
const PERSIST: &str = r#"{"limit":100,"persist":true}"#;
const NO_PERSIST: &str = r#"{"limit":100,"persist":false}"#;
/// O teto de um pedido que não pode esperar o trabalho pesado.
const FAST: Duration = Duration::from_millis(300);

/// Registra o módulo num servidor e serve num socket em `dir`, sem rodar o
/// laço (quem chama dá o `spawn`).
fn build(
    dir: &Path,
    cfg: ClipboardConfig,
    work: WorkTracker,
) -> (PathBuf, ClipboardTopics, Clipboard, JoinHandle<()>) {
    let path = dir.join("lucernad.sock");
    let mut builder = ServerBuilder::new("lucernad", "0.1.0");
    let clipboard = register(&mut builder, cfg, work);
    let topics = clipboard.topics.clone();
    let server = builder.build();
    let listener = UnixListener::bind(&path).unwrap();
    let handle = tokio::spawn(async move {
        let _ = server.serve(listener).await;
    });
    (path, topics, clipboard, handle)
}

/// O [`build`] com o laço rodando; o `ModuleTask` fica guardado na tarefa do
/// servidor, que vive até o fim do teste.
fn serve(
    dir: &Path,
    cfg: ClipboardConfig,
    work: WorkTracker,
) -> (PathBuf, ClipboardTopics, JoinHandle<()>) {
    let (path, topics, clipboard, server) = build(dir, cfg, work);
    let task = clipboard.spawn();
    let handle = tokio::spawn(async move {
        let _task = task;
        let _ = server.await;
    });
    (path, topics, handle)
}

/// O dublê, o diretório, o servidor e o contador de trabalho de um teste.
struct Rig {
    dir: TempDir,
    fake: FakeBackend,
    work: WorkTracker,
    path: PathBuf,
    t: ClipboardTopics,
    _server: JoinHandle<()>,
}

impl Rig {
    fn data(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    /// As imagens sem persistência.
    fn memory_images(&self) -> PathBuf {
        self.dir.path().join("run/lucerna/clipboard/images")
    }
}

fn rig() -> Rig {
    rig_with(FakeBackend::new(), |_| {})
}

/// O [`rig`] com o `fake` e a configuração mexida por `tweak`; com `dir`
/// pronto antes (o histórico de antes) pelo [`rig_in`].
fn rig_with(fake: FakeBackend, tweak: impl FnOnce(&mut ClipboardConfig)) -> Rig {
    rig_in(tempfile::tempdir().unwrap(), fake, tweak)
}

fn rig_in(dir: TempDir, fake: FakeBackend, tweak: impl FnOnce(&mut ClipboardConfig)) -> Rig {
    let mut cfg = fake_config(fake.clone(), dir.path());
    tweak(&mut cfg);
    let work = WorkTracker::new();
    let (path, t, server) = serve(dir.path(), cfg, work.clone());
    Rig {
        dir,
        fake,
        work,
        path,
        t,
        _server: server,
    }
}

async fn client(path: &Path) -> Client {
    Client::connect(path, "test", "lucernad").await.unwrap()
}

/// Um cliente configurado com persistência e assinando o histórico, com a
/// sessão já aberta.
async fn ready(r: &Rig) -> Client {
    let c = client(&r.path).await;
    configure(&c, PERSIST).await.unwrap();
    subscribed(&c).await;
    wait_until(Duration::from_secs(1), "a sessão aberta", || {
        r.fake.sessions() == 1
    })
    .await;
    c
}

async fn subscribed(c: &Client) {
    c.subscribe(&["clipboard.history"]).await.unwrap();
}

async fn call(c: &Client, method: &str, params: Value) -> Result<Value, ClientError> {
    c.call(method, Some(params)).await
}

async fn configure(c: &Client, params: &str) -> Result<Value, ClientError> {
    call(
        c,
        "clipboard.configure",
        serde_json::from_str(params).unwrap(),
    )
    .await
}

async fn list(c: &Client, params: Value) -> Value {
    call(c, "clipboard.list", params).await.unwrap()
}

async fn list_all(c: &Client) -> Value {
    list(c, json!({"offset": 0, "limit": 200})).await
}

/// Outro app copia o texto `s`.
fn text(b: &FakeBackend, s: &str) -> u64 {
    b.copy(&[TEXT], &[(TEXT, s.as_bytes())])
}

fn history(t: &ClipboardTopics) -> Value {
    t.history.current().data
}

fn count(t: &ClipboardTopics) -> i64 {
    history(t)["count"].as_i64().unwrap_or(-1)
}

fn revision(t: &ClipboardTopics) -> i64 {
    history(t)["revision"].as_i64().unwrap_or(-1)
}

fn status(t: &ClipboardTopics) -> Value {
    t.status.current().data
}

fn ids(page: &Value) -> Vec<i64> {
    items(page)
        .iter()
        .map(|i| i["id"].as_i64().unwrap())
        .collect()
}

fn previews(page: &Value) -> Vec<String> {
    items(page)
        .iter()
        .map(|i| i["preview"].as_str().unwrap_or("").to_string())
        .collect()
}

fn items(page: &Value) -> Vec<Value> {
    page["items"].as_array().cloned().unwrap_or_default()
}

/// Espera `cond` valer até `within`; senão, pânico com `what`.
async fn wait_until(within: Duration, what: &str, mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + within;
    while !cond() {
        assert!(Instant::now() < end, "{what}: não aconteceu em {within:?}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

async fn wait_count(t: &ClipboardTopics, n: i64) {
    wait_until(Duration::from_secs(3), &format!("count {n}"), || {
        count(t) == n
    })
    .await;
}

/// Passa um milissegundo (a unidade do `used`), para a ordem não empatar.
async fn tick() {
    tokio::time::sleep(Duration::from_millis(5)).await;
}

/// Espera o módulo pedir o conteúdo da `n`-ésima cópia: só a oferta atual
/// pode ser lida, então a cópia seguinte vem depois disso.
async fn read_started(r: &Rig, n: usize) {
    wait_until(Duration::from_secs(1), &format!("a leitura {n}"), || {
        r.fake.reads() == n
    })
    .await;
}

/// O texto `s` copiado por outro app e já no histórico, com `n` itens.
async fn copied(r: &Rig, s: &str, n: i64) {
    text(&r.fake, s);
    wait_count(&r.t, n).await;
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// O código e o texto de um erro do método.
fn rpc_error(result: Result<Value, ClientError>) -> (i32, String) {
    match result {
        Err(ClientError::Rpc(e)) => (e.code, e.message),
        other => panic!("esperava um erro do método: {other:?}"),
    }
}

/// Um histórico no disco em `dir/data`, de antes do servidor, com `clips`.
fn history_before(dir: &Path, clips: Vec<NewClip>) -> Vec<i64> {
    let data = dir.join("data");
    let mut store = Store::open(Origin::Disk(data.clone()), 100).unwrap();
    clips
        .into_iter()
        .enumerate()
        .map(|(i, clip)| {
            let p = prepare(clip, &data).unwrap();
            store.insert(p, 1_000 + i as i64).unwrap().0
        })
        .collect()
}

#[tokio::test]
async fn handshake_lists_topics_and_methods() {
    let r = rig();
    let c = client(&r.path).await;
    assert_eq!(c.hello().topics, ["clipboard.history", "clipboard.status"]);
    assert!(c.hello().streams.is_empty());
    assert_eq!(
        c.hello().methods,
        [
            "clipboard.list",
            "clipboard.get",
            "clipboard.copy",
            "clipboard.remove",
            "clipboard.pin",
            "clipboard.clear",
            "clipboard.copyImage",
            "clipboard.configure",
        ]
    );
    assert_eq!(history(&r.t), Value::Null);
    assert_eq!(status(&r.t), json!({"available": false, "error": null}));
}

#[tokio::test]
async fn params_are_validated() {
    let r = rig();
    let c = client(&r.path).await;
    let long = "x".repeat(1001);
    let cases = [
        (
            "clipboard.configure",
            json!({"limit": 0, "persist": true}),
            "limit precisa ser um inteiro entre 1 e 1000",
        ),
        (
            "clipboard.configure",
            json!({"limit": 5, "persist": "sim"}),
            "persist precisa ser verdadeiro ou falso",
        ),
        (
            "clipboard.configure",
            json!({"limit": 5, "persist": true, "waylandDisplay": 7}),
            "waylandDisplay precisa ser um texto",
        ),
        (
            "clipboard.list",
            json!({"offset": -1, "limit": 10}),
            "offset precisa ser um inteiro a partir de 0",
        ),
        (
            "clipboard.list",
            json!({"offset": 0, "limit": 201}),
            "limit precisa ser um inteiro entre 1 e 200",
        ),
        (
            "clipboard.list",
            json!({"kind": "video", "offset": 0, "limit": 10}),
            "kind precisa ser text, image ou files",
        ),
        (
            "clipboard.list",
            json!({"query": long, "offset": 0, "limit": 10}),
            "query precisa ser um texto de até 1000 caracteres",
        ),
        (
            "clipboard.pin",
            json!({"id": "1", "pinned": true}),
            "id precisa ser o número de um item",
        ),
        (
            "clipboard.pin",
            json!({"id": 1, "pinned": "sim"}),
            "pinned precisa ser verdadeiro ou falso",
        ),
        (
            "clipboard.clear",
            json!({"all": 1}),
            "all precisa ser verdadeiro ou falso",
        ),
        (
            "clipboard.copyImage",
            json!({"path": "rel.png"}),
            "path precisa ser um caminho absoluto",
        ),
        (
            "clipboard.copyImage",
            json!({"path": "/a.png", "store": 1}),
            "store precisa ser verdadeiro ou falso",
        ),
    ];
    for (method, params, text) in cases {
        assert_eq!(
            rpc_error(call(&c, method, params.clone()).await),
            (-32602, format!("Parâmetros inválidos: {text}.")),
            "{method} {params}"
        );
    }
    // Nada disso configurou o módulo.
    assert_eq!(history(&r.t), Value::Null);
}

#[tokio::test]
async fn nothing_observes_before_configure() {
    let r = rig_with(FakeBackend::new(), |cfg| {
        cfg.configure_wait = Duration::from_secs(30);
    });
    let c = client(&r.path).await;
    subscribed(&c).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(r.fake.opened(), 0, "sem configure, nada observa");
    assert_eq!(history(&r.t), Value::Null);
    configure(&c, PERSIST).await.unwrap();
    wait_until(
        Duration::from_secs(1),
        "a sessão depois do configure",
        || r.fake.opened() == 1,
    )
    .await;
}

#[tokio::test]
async fn default_configuration_after_the_wait() {
    let r = rig();
    let c = client(&r.path).await;
    subscribed(&c).await;
    // O `configure_wait` é de 200 ms.
    wait_until(Duration::from_secs(1), "a sessão com o padrão", || {
        r.fake.opened() == 1
    })
    .await;
    assert!(r.data().join("history.db").exists());
    assert_eq!(count(&r.t), 0);
}

#[tokio::test]
async fn copies_become_history() {
    let r = rig();
    let c = ready(&r).await;
    text(&r.fake, "olá mundo");
    read_started(&r, 1).await;
    let png = png_bytes(64, 64);
    r.fake.copy(&["image/png"], &[("image/png", &png)]);
    read_started(&r, 2).await;
    r.fake.copy(
        &["text/uri-list"],
        &[("text/uri-list", b"file:///tmp/relat%C3%B3rio.pdf\r\n")],
    );
    wait_count(&r.t, 3).await;

    let page = list_all(&c).await;
    let kinds: Vec<Value> = items(&page).iter().map(|i| i["kind"].clone()).collect();
    assert_eq!(kinds, ["files", "image", "text"]);
    assert_eq!(page["total"], 3);
    let image = &items(&page)[1];
    assert_eq!(image["mime"], "image/png");
    assert_eq!(
        (image["width"].clone(), image["height"].clone()),
        (json!(64), json!(64))
    );
    assert!(Path::new(image["thumb"].as_str().unwrap()).exists());
    assert!(Path::new(image["image"].as_str().unwrap()).exists());
    assert_eq!(items(&page)[0]["preview"], "relatório.pdf");

    for (kind, preview) in [("text", "olá mundo"), ("files", "relatório.pdf")] {
        let page = list(&c, json!({"kind": kind, "offset": 0, "limit": 10})).await;
        assert_eq!(previews(&page), [preview], "{kind}");
    }
    let page = list(&c, json!({"kind": "image", "offset": 0, "limit": 10})).await;
    assert_eq!(page["total"], 1);

    let id = ids(&list(&c, json!({"kind": "text", "offset": 0, "limit": 1})).await)[0];
    let item = call(&c, "clipboard.get", json!({"id": id})).await.unwrap();
    assert_eq!(item["text"], "olá mundo");
}

#[tokio::test]
async fn secret_is_never_read_nor_stored() {
    let r = rig();
    let _c = ready(&r).await;
    r.fake.copy(
        &["text/plain", "x-kde-passwordManagerHint"],
        &[("text/plain", b"segredo-123")],
    );
    // Uma cópia comum depois: quando ela chega, a sensível já passou.
    copied(&r, "visível", 1).await;
    assert_eq!(r.fake.reads(), 1, "a cópia sensível não foi lida");
    for name in ["history.db", "history.db-wal"] {
        let bytes = std::fs::read(r.data().join(name)).unwrap_or_default();
        assert!(
            !bytes.windows(11).any(|w| w == b"segredo-123"),
            "o segredo está em {name}"
        );
    }
}

#[tokio::test]
async fn own_copy_only_moves_to_top() {
    let r = rig();
    let c = ready(&r).await;
    copied(&r, "primeiro", 1).await;
    copied(&r, "segundo", 2).await;
    let page = list_all(&c).await;
    let (b, a) = (ids(&page)[0], ids(&page)[1]);

    // O `used` é em milissegundos: cada passo num milissegundo próprio.
    tick().await;
    call(&c, "clipboard.copy", json!({"id": a})).await.unwrap();
    let owned = r.fake.owned().expect("a seleção é nossa");
    assert_eq!(owned.last().map(String::as_str), Some(OWN_MIME));
    assert_eq!(r.fake.paste("text/plain").unwrap(), b"primeiro");
    let reads = r.fake.reads();

    // O compositor repete a nossa seleção, com os tipos da fonte.
    let mimes: Vec<&str> = owned.iter().map(String::as_str).collect();
    r.fake.copy(&mimes, &[]);
    tick().await;
    copied(&r, "terceiro", 3).await;
    assert_eq!(r.fake.reads(), reads + 1, "a nossa seleção não foi lida");
    let page = list_all(&c).await;
    assert_eq!(&ids(&page)[1..], [a, b]);
}

#[tokio::test]
async fn owned_selection_outlives_the_subscriber() {
    let r = rig();
    let c = ready(&r).await;
    copied(&r, "fica", 1).await;
    let id = ids(&list_all(&c).await)[0];
    call(&c, "clipboard.copy", json!({"id": id})).await.unwrap();
    assert_eq!(r.work.pending(), 1);

    c.unsubscribe(&["clipboard.history"]).await.unwrap();
    wait_until(Duration::from_secs(1), "sem assinante", || {
        r.t.history.subscribers() == 0
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(r.fake.sessions(), 1, "a seleção nossa segura a sessão");
    assert_eq!(r.work.pending(), 1);

    // Sem assinante, a sessão só segura a seleção: nada de outro app é lido.
    let reads = r.fake.reads();
    text(&r.fake, "alheio");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!((r.fake.reads(), count(&r.t)), (reads, 1));

    r.fake.cancel();
    wait_until(Duration::from_secs(1), "a sessão solta", || {
        r.work.pending() == 0 && r.fake.sessions() == 0
    })
    .await;
}

#[tokio::test]
async fn copy_without_subscriber_opens_a_session() {
    let dir = tempfile::tempdir().unwrap();
    let id = history_before(dir.path(), vec![NewClip::Text("de antes".into())])[0];
    let r = rig_in(dir, FakeBackend::new(), |_| {});
    let c = client(&r.path).await;
    configure(&c, PERSIST).await.unwrap();
    assert_eq!(r.fake.sessions(), 0);
    call(&c, "clipboard.copy", json!({"id": id})).await.unwrap();
    assert_eq!(r.fake.sessions(), 1);
    assert_eq!(r.fake.paste("text/plain").unwrap(), b"de antes");
    assert_eq!(r.work.pending(), 1);
}

#[tokio::test]
async fn fast_copies_keep_their_order() {
    let r = rig();
    let c = ready(&r).await;
    let start = Instant::now();
    r.fake.copy_stuck(&[TEXT]);
    read_started(&r, 1).await;
    // As duas chegam antes de a primeira terminar de ler.
    text(&r.fake, "x");
    read_started(&r, 2).await;
    text(&r.fake, "y");
    wait_until(
        READ_TIMEOUT + Duration::from_secs(2),
        "as duas cópias",
        || count(&r.t) == 2,
    )
    .await;
    assert!(start.elapsed() < READ_TIMEOUT + Duration::from_secs(2));
    assert_eq!(previews(&list_all(&c).await), ["y", "x"]);
}

#[tokio::test]
async fn stuck_offer_does_not_block_the_next_copy() {
    let r = rig();
    let c = ready(&r).await;
    r.fake.copy_stuck(&[TEXT]);
    read_started(&r, 1).await;
    let start = Instant::now();
    let page = list_all(&c).await;
    assert!(start.elapsed() < FAST, "{:?}", start.elapsed());
    assert_eq!(page["total"], 0);
    // O laço segue atendendo: a próxima cópia é lida já.
    text(&r.fake, "depois");
    read_started(&r, 2).await;
}

#[tokio::test]
async fn first_configure_without_persist_wipes_the_disk() {
    let dir = tempfile::tempdir().unwrap();
    history_before(
        dir.path(),
        vec![
            NewClip::Text("de antes".into()),
            NewClip::Image {
                mime: "image/png".into(),
                bytes: png_bytes(8, 8),
            },
        ],
    );
    assert_eq!(files_in(&dir.path().join("data/images")).len(), 2);
    let r = rig_in(dir, FakeBackend::new(), |_| {});
    let c = client(&r.path).await;
    configure(&c, NO_PERSIST).await.unwrap();
    assert_eq!(count(&r.t), 0);
    let left: Vec<String> = files_in(&r.data())
        .into_iter()
        .filter(|n| n.starts_with("history.db"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    assert!(files_in(&r.data().join("images")).is_empty());
}

#[tokio::test]
async fn aborting_the_module_clears_memory_files() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeBackend::new();
    let (path, t, clipboard, _server) = build(
        dir.path(),
        fake_config(fake.clone(), dir.path()),
        WorkTracker::new(),
    );
    let task = clipboard.spawn();
    let c = client(&path).await;
    configure(&c, NO_PERSIST).await.unwrap();
    subscribed(&c).await;
    wait_until(Duration::from_secs(1), "a sessão", || fake.sessions() == 1).await;
    let png = png_bytes(16, 16);
    fake.copy(&["image/png"], &[("image/png", &png)]);
    wait_count(&t, 1).await;
    let images = dir.path().join("run/lucerna/clipboard/images");
    assert_eq!(files_in(&images).len(), 2);

    drop(task);
    wait_until(Duration::from_millis(500), "os arquivos apagados", || {
        files_in(&images).is_empty()
    })
    .await;
}

#[tokio::test]
async fn list_stays_fast_while_a_big_image_is_saved() {
    let r = rig();
    let c = ready(&r).await;
    let png = png_bytes(4000, 4000);
    r.fake.copy_held(&["image/png"], &[("image/png", &png)]);
    read_started(&r, 1).await;
    // A leitura não acabou: o item não pode ter aparecido.
    let start = Instant::now();
    let page = list_all(&c).await;
    assert!(start.elapsed() < FAST, "{:?}", start.elapsed());
    assert_eq!(page["total"], 0);

    // Os bytes chegam; a miniatura e a gravação correm sem o lock.
    r.fake.release();
    let saving = Instant::now();
    let start = Instant::now();
    list_all(&c).await;
    let listed = start.elapsed();
    wait_until(Duration::from_secs(60), "a imagem gravada", || {
        count(&r.t) == 1
    })
    .await;
    let saved = saving.elapsed();
    assert!(listed < FAST, "o list levou {listed:?}");
    assert!(
        listed < saved,
        "o list ({listed:?}) esperou a gravação ({saved:?})"
    );
}

#[tokio::test]
async fn list_of_a_huge_text_is_fast() {
    let r = rig();
    let c = ready(&r).await;
    // Perto de 1 MiB, com acentos e quebras, e a agulha só no fim.
    let mut big = "Ação  do dia\n\tlinha ".repeat(45_000);
    big.push_str("agulha");
    assert!(big.len() <= 1 << 20, "{}", big.len());
    copied(&r, &big, 1).await;

    let start = Instant::now();
    let page = list_all(&c).await;
    assert!(
        start.elapsed() < FAST,
        "sem consulta: {:?}",
        start.elapsed()
    );
    assert_eq!(previews(&page)[0].chars().count(), 400);

    let start = Instant::now();
    let page = list(&c, json!({"query": "agulha", "offset": 0, "limit": 10})).await;
    assert!(
        start.elapsed() < FAST,
        "com consulta: {:?}",
        start.elapsed()
    );
    let item = &items(&page)[0];
    assert!(item["preview"].as_str().unwrap().ends_with("agulha"));
    assert_eq!(item["matches"].as_array().unwrap().len(), 1);

    let start = Instant::now();
    let page = list(&c, json!({"query": "acao", "offset": 0, "limit": 10})).await;
    assert!(
        start.elapsed() < FAST,
        "consulta comum: {:?}",
        start.elapsed()
    );
    assert_eq!(items(&page)[0]["matches"].as_array().unwrap().len(), 20);
}

#[tokio::test]
async fn persist_off_keeps_disk_clean() {
    let r = rig();
    let c = client(&r.path).await;
    configure(&c, NO_PERSIST).await.unwrap();
    subscribed(&c).await;
    wait_until(Duration::from_secs(1), "a sessão", || {
        r.fake.sessions() == 1
    })
    .await;
    copied(&r, "só na memória", 1).await;
    let png = png_bytes(16, 16);
    r.fake.copy(&["image/png"], &[("image/png", &png)]);
    wait_count(&r.t, 2).await;
    assert!(!r.data().join("history.db").exists());
    assert!(files_in(&r.data().join("images")).is_empty());
    assert_eq!(files_in(&r.memory_images()).len(), 2);

    configure(&c, PERSIST).await.unwrap();
    assert!(r.data().join("history.db").exists());
    assert_eq!(files_in(&r.data().join("images")).len(), 2);
    assert!(files_in(&r.memory_images()).is_empty());
    assert_eq!(count(&r.t), 2);
    assert_eq!(list_all(&c).await["total"], 2);
}

#[tokio::test]
async fn limit_change_trims() {
    let r = rig();
    let c = ready(&r).await;
    for (i, s) in ["a", "b", "c", "d", "e"].iter().enumerate() {
        copied(&r, s, i as i64 + 1).await;
    }
    let before = revision(&r.t);
    configure(&c, r#"{"limit":2,"persist":true}"#)
        .await
        .unwrap();
    assert_eq!(count(&r.t), 2);
    assert!(revision(&r.t) > before);
    assert_eq!(previews(&list_all(&c).await), ["e", "d"]);
}

#[tokio::test]
async fn clear_and_pin() {
    let r = rig();
    let c = ready(&r).await;
    copied(&r, "fixo", 1).await;
    copied(&r, "solto", 2).await;
    let page = list_all(&c).await;
    let fixed = ids(&page)[1];
    let pinned = call(&c, "clipboard.pin", json!({"id": fixed, "pinned": true}))
        .await
        .unwrap();
    assert_eq!(pinned, json!({}));
    assert_eq!(history(&r.t)["pinned"], 1);
    assert_eq!(ids(&list_all(&c).await)[0], fixed, "fixado primeiro");

    assert_eq!(
        call(&c, "clipboard.clear", json!({})).await.unwrap(),
        json!({})
    );
    assert_eq!(count(&r.t), 1);
    assert_eq!(ids(&list_all(&c).await), [fixed]);
    call(&c, "clipboard.clear", json!({"all": true}))
        .await
        .unwrap();
    assert_eq!(count(&r.t), 0);

    let none = json!({"id": 999});
    assert_eq!(
        call(&c, "clipboard.remove", none.clone()).await.unwrap(),
        json!({})
    );
    let pin = json!({"id": 999, "pinned": true});
    assert_eq!(call(&c, "clipboard.pin", pin).await.unwrap(), json!({}));
    let gone = (1001, "O item 999 não existe mais.".to_string());
    assert_eq!(
        rpc_error(call(&c, "clipboard.copy", none.clone()).await),
        gone
    );
    assert_eq!(rpc_error(call(&c, "clipboard.get", none).await), gone);
}

#[tokio::test]
async fn missing_display_is_state() {
    let r = rig_with(FakeBackend::new(), |cfg| cfg.display_env = None);
    let c = client(&r.path).await;
    configure(&c, PERSIST).await.unwrap();
    subscribed(&c).await;
    let missing = json!({"available": false, "error": NO_DISPLAY});
    wait_until(Duration::from_secs(1), "o erro da exibição", || {
        status(&r.t) == missing
    })
    .await;
    assert_eq!(r.fake.opened(), 0);

    configure(
        &c,
        r#"{"limit":100,"persist":true,"waylandDisplay":"wayland-7"}"#,
    )
    .await
    .unwrap();
    wait_until(Duration::from_secs(1), "a sessão na wayland-7", || {
        status(&r.t)["available"] == true
    })
    .await;
    let socket = r.fake.sockets().pop().unwrap();
    assert_eq!(socket, r.dir.path().join("run/wayland-7"));
}

#[tokio::test]
async fn compositor_drop_becomes_state_and_retries() {
    let r = rig();
    let _c = ready(&r).await;
    wait_until(Duration::from_secs(1), "disponível", || {
        status(&r.t)["available"] == true
    })
    .await;
    r.fake.disconnect(&disconnected("x"));
    let dropped = json!({"available": false, "error": "O compositor fechou a conexão: x."});
    wait_until(Duration::from_secs(1), "a queda no estado", || {
        status(&r.t) == dropped
    })
    .await;
    wait_until(Duration::from_secs(1), "a nova sessão", || {
        r.fake.opened() == 2 && status(&r.t)["available"] == true
    })
    .await;
}

#[tokio::test]
async fn copy_image_from_disk() {
    let r = rig();
    let c = ready(&r).await;
    // A extensão mente: o mime vem dos bytes.
    let photo = r.dir.path().join("foto.jpg");
    std::fs::write(&photo, png_bytes(32, 32)).unwrap();
    let result = call(&c, "clipboard.copyImage", json!({"path": photo}))
        .await
        .unwrap();
    let id = result["id"].as_i64().unwrap();
    assert!(id > 0, "{result}");
    assert_eq!(
        r.fake.owned(),
        Some(vec!["image/png".to_string(), OWN_MIME.to_string()])
    );
    wait_count(&r.t, 1).await;
    let item = call(&c, "clipboard.get", json!({"id": id})).await.unwrap();
    assert_eq!(item["mime"], "image/png");

    // Sem guardar: só a imagem na fonte, e o eco dela não entra no histórico.
    let other = r.dir.path().join("outra.png");
    let other_png = png_bytes(48, 48);
    std::fs::write(&other, &other_png).unwrap();
    let result = call(
        &c,
        "clipboard.copyImage",
        json!({"path": other, "store": false}),
    )
    .await
    .unwrap();
    assert_eq!(result, json!({"id": 0}));
    assert_eq!(r.fake.owned(), Some(vec!["image/png".to_string()]));
    assert_eq!(r.fake.paste("image/png").unwrap(), other_png);
    let reads = r.fake.reads();
    copied(&r, "sentinela", 2).await;
    assert_eq!(
        r.fake.reads(),
        reads + 1,
        "o eco da nossa imagem não foi lido"
    );
    let page = list(&c, json!({"kind": "image", "offset": 0, "limit": 10})).await;
    assert_eq!(ids(&page), [id]);

    let missing = r.dir.path().join("nada.png");
    let (code, message) =
        rpc_error(call(&c, "clipboard.copyImage", json!({"path": missing})).await);
    assert_eq!(code, 1002);
    assert!(
        message.starts_with("Não consegui ler a imagem: "),
        "{message}"
    );
    let notes = r.dir.path().join("notas.png");
    std::fs::write(&notes, "só texto").unwrap();
    assert_eq!(
        rpc_error(call(&c, "clipboard.copyImage", json!({"path": notes})).await),
        (
            1002,
            format!(
                "Não consegui ler a imagem: {} não é uma imagem.",
                notes.display()
            )
        )
    );
}

#[tokio::test]
async fn early_copy_does_not_cancel_the_default_configuration() {
    let r = rig();
    let c = client(&r.path).await;
    subscribed(&c).await;
    // Uma captura sem guardar nos primeiros 200 ms (o `configure_wait`).
    let capture = r.dir.path().join("captura.png");
    std::fs::write(&capture, png_bytes(16, 16)).unwrap();
    let copied_back = call(
        &c,
        "clipboard.copyImage",
        json!({"path": capture, "store": false}),
    )
    .await
    .unwrap();
    assert_eq!(copied_back, json!({"id": 0}));
    assert_eq!(history(&r.t), Value::Null, "ainda sem configuração");
    // O padrão vale depois da espera, e a cópia de outro app entra.
    wait_until(Duration::from_secs(1), "o padrão", || count(&r.t) == 0).await;
    copied(&r, "alheio", 1).await;
}

#[tokio::test]
async fn copy_image_refuses_what_is_not_a_regular_file() {
    let r = rig();
    let c = ready(&r).await;
    let fifo = r.dir.path().join("fila.png");
    rustix::fs::mkfifoat(
        rustix::fs::CWD,
        &fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
    )
    .unwrap();
    for path in [fifo, r.dir.path().to_path_buf()] {
        let start = Instant::now();
        let (code, message) =
            rpc_error(call(&c, "clipboard.copyImage", json!({"path": path})).await);
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "{path:?} prendeu a chamada"
        );
        assert_eq!(
            (code, message),
            (
                1002,
                format!(
                    "Não consegui ler a imagem: {} não é um arquivo comum.",
                    path.display()
                )
            )
        );
    }
}

#[tokio::test]
async fn escaped_items_fit_in_a_message() {
    let r = rig();
    let c = ready(&r).await;
    // Um texto só de controles: cada um vira \u00XX (6 bytes) no JSON.
    let controls = "\u{1}".repeat(1 << 20);
    copied(&r, &controls, 1).await;
    let id = ids(&list_all(&c).await)[0];
    let item = call(&c, "clipboard.get", json!({"id": id})).await.unwrap();
    assert_eq!(item["truncated"], true);
    let escaped = serde_json::to_string(item["text"].as_str().unwrap()).unwrap();
    assert!(escaped.len() <= (256 << 10) + 2, "{}", escaped.len());

    // Caminhos longos: as uris da página cabem em ~2 KiB por item.
    let uris: Vec<String> = (0..40)
        .map(|i| format!("file:///{}/{i}.txt", "d".repeat(400)))
        .collect();
    let list_bytes = uris.join("\r\n");
    r.fake.copy(
        &["text/uri-list"],
        &[("text/uri-list", list_bytes.as_bytes())],
    );
    wait_count(&r.t, 2).await;
    let page = list(&c, json!({"kind": "files", "offset": 0, "limit": 10})).await;
    let files = &items(&page)[0];
    let kept = files["uris"].as_array().unwrap();
    assert!(!kept.is_empty() && kept.len() < 40, "{}", kept.len());
    assert!(serde_json::to_string(kept).unwrap().len() <= 2 << 10);
    assert_eq!(files["urisTotal"], 40);
}

#[tokio::test]
async fn panic_marks_the_module_unavailable() {
    let r = rig_with(FakeBackend::panicking(), |_| {});
    let c = client(&r.path).await;
    configure(&c, PERSIST).await.unwrap();
    subscribed(&c).await;
    let panicked = json!({
        "available": false,
        "error": "O módulo da área de transferência parou por um erro interno.",
    });
    wait_until(Duration::from_secs(1), "o erro interno", || {
        status(&r.t) == panicked
    })
    .await;
    let again = client(&r.path).await;
    assert_eq!(again.hello().app, "lucernad");
}

#[tokio::test]
async fn big_items_fit_in_a_message() {
    let r = rig();
    let c = ready(&r).await;
    let big = "Ação do dia, linha ".repeat(49_000);
    assert!(big.len() <= 1 << 20, "{}", big.len());
    copied(&r, &big, 1).await;
    let uris: Vec<String> = (0..5000)
        .map(|i| format!("file:///home/u/{}/arquivo-{i:05}.txt", "pasta".repeat(20)))
        .collect();
    let list_bytes = uris.join("\r\n");
    assert!(list_bytes.len() <= 1 << 20, "{}", list_bytes.len());
    r.fake.copy(
        &["text/uri-list"],
        &[("text/uri-list", list_bytes.as_bytes())],
    );
    wait_count(&r.t, 2).await;

    let page = list_all(&c).await;
    let files = &items(&page)[0];
    let listed = files["uris"].as_array().unwrap().len();
    assert!((1..50).contains(&listed), "{listed}");
    assert_eq!(files["urisTotal"], 5000);
    let text_id = ids(&page)[1];

    // O `get` cabe no limite do SDK (senão a chamada falharia) e diz que cortou.
    let item = call(&c, "clipboard.get", json!({"id": text_id}))
        .await
        .unwrap();
    assert_eq!(item["truncated"], true);
    let text = item["text"].as_str().unwrap();
    assert!(
        text.len() <= 256 << 10 && text.len() > 250 << 10,
        "{}",
        text.len()
    );
    assert!(big.starts_with(text));
    let item = call(&c, "clipboard.get", json!({"id": files["id"]}))
        .await
        .unwrap();
    assert_eq!(item["truncated"], true);
    let kept = item["uris"].as_array().unwrap();
    assert!(kept.len() < 5000 && kept[0] == uris[0].as_str());

    // Um texto pequeno vem inteiro, sem `truncated`.
    copied(&r, "curto", 3).await;
    let id = ids(&list_all(&c).await)[0];
    let item = call(&c, "clipboard.get", json!({"id": id})).await.unwrap();
    assert_eq!(
        (item["text"].clone(), item.get("truncated")),
        (json!("curto"), None)
    );
}

#[tokio::test]
async fn echo_queue_skips_only_our_sources() {
    let r = rig();
    let c = ready(&r).await;
    r.fake.defer_echoes();
    let stored = r.dir.path().join("guardada.png");
    std::fs::write(&stored, png_bytes(24, 24)).unwrap();
    let kept = call(&c, "clipboard.copyImage", json!({"path": stored}))
        .await
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    wait_count(&r.t, 1).await;
    let capture = r.dir.path().join("captura.png");
    std::fs::write(&capture, png_bytes(40, 40)).unwrap();
    call(
        &c,
        "clipboard.copyImage",
        json!({"path": capture, "store": false}),
    )
    .await
    .unwrap();

    // Uma cópia alheia chega antes dos dois ecos e não esvazia a fila.
    copied(&r, "alheio", 2).await;
    let reads = r.fake.reads();
    r.fake.flush_echoes();
    copied(&r, "sentinela", 3).await;
    assert_eq!(r.fake.reads(), reads + 1, "os ecos não foram lidos");
    let images = json!({"kind": "image", "offset": 0, "limit": 10});
    assert_eq!(ids(&list(&c, images.clone()).await), [kept]);

    // A seleção deixou de ser nossa: os mesmos tipos, de outro app, entram.
    call(
        &c,
        "clipboard.copyImage",
        json!({"path": capture, "store": false}),
    )
    .await
    .unwrap();
    r.fake.cancel();
    let other = png_bytes(56, 56);
    tick().await;
    r.fake.copy(&["image/png"], &[("image/png", &other)]);
    wait_count(&r.t, 4).await;
    assert_eq!(list(&c, images).await["total"], 2);
}

/// O [`rig`] com o sinal do modo jogo.
fn light_rig() -> (Rig, tokio::sync::watch::Sender<bool>) {
    let (light, game) = lucernad_core::GameSignal::channel();
    let r = rig_with(FakeBackend::new(), |cfg| cfg.game = game);
    (r, light)
}

#[tokio::test]
async fn light_mode_defers_copies_in_order() {
    let (r, light) = light_rig();
    let c = ready(&r).await;
    copied(&r, "antes", 1).await;
    let rev = revision(&r.t);
    light.send(true).unwrap();
    tick().await;
    for (i, s) in ["um", "dois", "três"].iter().enumerate() {
        text(&r.fake, s);
        read_started(&r, 2 + i).await;
        tick().await;
    }
    // Lidas (a seleção de um app some se não for lida), mas nada no banco
    // nem publicado.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(count(&r.t), 1);
    assert_eq!(revision(&r.t), rev);
    assert_eq!(previews(&list_all(&c).await), ["antes"]);
    // Ao sair, as três na ordem, publicadas uma vez.
    light.send(false).unwrap();
    wait_count(&r.t, 4).await;
    assert_eq!(revision(&r.t), rev + 1);
    assert_eq!(
        previews(&list_all(&c).await),
        ["três", "dois", "um", "antes"]
    );
}

#[tokio::test]
async fn light_mode_queue_drops_oldest_after_fifty() {
    assert_eq!(lucernad_clipboard::DEFERRED_MAX, 50);
    let (r, light) = light_rig();
    let c = ready(&r).await;
    light.send(true).unwrap();
    tick().await;
    for i in 0..52 {
        text(&r.fake, &format!("c{i}"));
        read_started(&r, i + 1).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(count(&r.t), 0);
    light.send(false).unwrap();
    wait_count(&r.t, 50).await;
    let page = list_all(&c).await;
    let got = previews(&page);
    assert_eq!(got.first().map(String::as_str), Some("c51"));
    assert_eq!(got.last().map(String::as_str), Some("c2"), "c0 e c1 caíram");
}

#[tokio::test]
async fn copy_from_history_works_during_light_mode() {
    let (r, light) = light_rig();
    let c = ready(&r).await;
    copied(&r, "antigo", 1).await;
    let id = ids(&list_all(&c).await)[0];
    light.send(true).unwrap();
    tick().await;
    call(&c, "clipboard.copy", json!({"id": id})).await.unwrap();
    assert_eq!(r.fake.paste(TEXT).as_deref(), Some(&b"antigo"[..]));
    light.send(false).unwrap();
}
