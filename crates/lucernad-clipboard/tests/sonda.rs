//! A sonda do backend de verdade, num compositor isolado: rode pelo
//! `tests/isolado.sh`, que sobe um sway headless próprio num container, com
//! um `XDG_RUNTIME_DIR` temporário. Fora dele, os testes ficam ignorados, e
//! cada um começa pela trava [`isolated`]: a sessão do usuário nunca é
//! tocada.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lucerna_ipc::Client;
use lucerna_service::{ServerBuilder, WorkTracker};
use lucernad_clipboard::backend::{Backend, Event, Session, Source};
use lucernad_clipboard::mime::{IMAGE_MAX, Kind, offers_for};
use lucernad_clipboard::pipe::read_pipe;
use lucernad_clipboard::testing::{isolated, jpeg_bytes, png_bytes};
use lucernad_clipboard::wayland::{Protocol, Wayland};
use lucernad_clipboard::{ClipboardConfig, ClipboardTopics, register};
use serde_json::{Value, json};

/// O socket do sway isolado.
fn socket() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR do isolado.sh");
    PathBuf::from(dir).join("wayland-1")
}

/// O `wl-copy` do container, contra o sway isolado.
fn wl_copy(args: &[&str], stdin: &[u8]) {
    let mut child = Command::new("wl-copy")
        .args(args)
        .env("WAYLAND_DISPLAY", socket())
        .stdin(Stdio::piped())
        .spawn()
        .expect("rodar o wl-copy");
    child
        .stdin
        .take()
        .expect("o stdin do wl-copy")
        .write_all(stdin)
        .expect("escrever no wl-copy");
    assert!(child.wait().expect("esperar o wl-copy").success());
}

/// O `wl-paste` do container, contra o sway isolado.
fn wl_paste(args: &[&str]) -> Vec<u8> {
    let out = Command::new("wl-paste")
        .args(args)
        .env("WAYLAND_DISPLAY", socket())
        .output()
        .expect("rodar o wl-paste");
    assert!(out.status.success(), "wl-paste {args:?} falhou");
    out.stdout
}

/// O próximo evento que passa no `keep`, em até `within`.
fn next_event(session: &mut Session, within: Duration, keep: impl Fn(&Event) -> bool) -> Event {
    let end = Instant::now() + within;
    loop {
        match session.events.try_recv() {
            Ok(event) if keep(&event) => return event,
            Ok(_) => {}
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                assert!(Instant::now() < end, "nenhum evento esperado em {within:?}");
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("a sessão fechou: {e}"),
        }
    }
}

/// Espera uma seleção com `mime` entre os tipos e lê o conteúdo nele.
fn read_selection(session: &mut Session, mime: &str) -> Vec<u8> {
    let event = next_event(
        session,
        Duration::from_secs(2),
        |e| matches!(e, Event::Selection { mimes, .. } if mimes.iter().any(|m| m == mime)),
    );
    let Event::Selection { offer, .. } = event else {
        unreachable!()
    };
    let reader = session.control.receive(offer, mime).expect("receber");
    read_pipe(reader, IMAGE_MAX, Instant::now() + Duration::from_secs(3)).expect("ler o pipe")
}

#[test]
#[ignore = "só no compositor isolado (tests/isolado.sh)"]
fn sees_a_real_copy_and_reads_it() {
    isolated();
    for protocol in [Protocol::Ext, Protocol::Wlr] {
        let backend = Wayland {
            only: Some(protocol),
        };
        let mut session = backend.open(&socket()).expect("abrir a sessão");

        wl_copy(&[], "olá mundo".as_bytes());
        let text = read_selection(&mut session, "text/plain;charset=utf-8");
        assert_eq!(String::from_utf8_lossy(&text), "olá mundo", "{protocol:?}");

        let png = png_bytes(64, 64);
        wl_copy(&["--type", "image/png"], &png);
        assert_eq!(
            read_selection(&mut session, "image/png"),
            png,
            "{protocol:?}"
        );
    }
}

#[test]
#[ignore = "só no compositor isolado (tests/isolado.sh)"]
fn serves_what_it_offers() {
    isolated();
    let mut session = Wayland::system().open(&socket()).expect("abrir a sessão");
    let offers = offers_for(Kind::Text, None, b"colado", 5);
    let mut expected: Vec<String> = offers.iter().map(|(m, _)| m.clone()).collect();
    session.control.offer(Source { offers });

    // A nossa seleção volta como uma seleção comum.
    next_event(
        &mut session,
        Duration::from_secs(2),
        |e| matches!(e, Event::Selection { mimes, .. } if mimes.len() == expected.len()),
    );
    let listed = String::from_utf8(wl_paste(&["--list-types"])).expect("tipos em UTF-8");
    let mut listed: Vec<String> = listed.lines().map(str::to_string).collect();
    listed.sort();
    expected.sort();
    assert_eq!(listed, expected);
    assert_eq!(wl_paste(&["--no-newline"]), b"colado");
    assert_eq!(
        wl_paste(&["--no-newline", "--type", "application/x-lucernad-item"]),
        b"5"
    );

    wl_copy(&[], b"outro");
    next_event(&mut session, Duration::from_secs(2), |e| {
        *e == Event::Cancelled
    });
}

/// Espera `cond` valer até `within`; senão, pânico com `what`.
async fn wait_until(within: Duration, what: &str, mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + within;
    while !cond() {
        assert!(Instant::now() < end, "{what}: não aconteceu em {within:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn count(t: &ClipboardTopics) -> i64 {
    t.history.current().data["count"].as_i64().unwrap_or(-1)
}

/// O `wl-copy` (fora do runtime: ele espera o filho se apresentar) e o
/// item no histórico, com `n` itens.
async fn copied(t: &ClipboardTopics, args: &'static [&'static str], data: Vec<u8>, n: i64) {
    tokio::task::spawn_blocking(move || wl_copy(args, &data))
        .await
        .unwrap();
    wait_until(Duration::from_secs(5), &format!("a cópia {n}"), || {
        count(t) == n
    })
    .await;
}

/// O id do primeiro item da página pedida com `params` que tem `key` igual
/// a `value`.
async fn find_id(c: &Client, params: Value, key: &str, value: &str) -> i64 {
    let page = c.call("clipboard.list", Some(params)).await.unwrap();
    let items = page["items"].as_array().cloned().unwrap_or_default();
    let item = items
        .iter()
        .find(|i| i[key] == value)
        .unwrap_or_else(|| panic!("nenhum item com {key} = {value}: {page}"));
    item["id"].as_i64().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "só no compositor isolado (tests/isolado.sh)"]
async fn module_keeps_real_copies() {
    isolated();
    let dir = tempfile::tempdir().unwrap();
    let work = WorkTracker::new();
    let cfg = ClipboardConfig {
        backend: Arc::new(Wayland::system()),
        data_dir: Some(dir.path().join("data")),
        runtime_dir: socket().parent().map(PathBuf::from),
        display_env: Some("wayland-1".to_string()),
        configure_wait: Duration::from_secs(2),
        retry: Duration::from_millis(200),
    };
    let mut builder = ServerBuilder::new("lucernad", "0.1.0");
    let clipboard = register(&mut builder, cfg, work.clone());
    let t = clipboard.topics.clone();
    let server = builder.build();
    let path = dir.path().join("lucernad.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    tokio::spawn(async move {
        let _ = server.serve(listener).await;
    });
    let _task = clipboard.spawn();
    let c = Client::connect(&path, "sonda", "lucernad").await.unwrap();
    c.call(
        "clipboard.configure",
        Some(json!({"limit": 100, "persist": true})),
    )
    .await
    .unwrap();
    c.subscribe(&["clipboard.history"]).await.unwrap();
    wait_until(Duration::from_secs(3), "a sessão", || {
        t.status.current().data["available"] == true
    })
    .await;

    let png = png_bytes(64, 64);
    copied(&t, &[], "Ação real".as_bytes().to_vec(), 1).await;
    copied(&t, &["--type", "image/png"], png.clone(), 2).await;
    copied(&t, &["--type", "image/jpeg"], jpeg_bytes(32, 32), 3).await;
    copied(
        &t,
        &["--type", "text/uri-list"],
        b"file:///tmp/a.txt".to_vec(),
        4,
    )
    .await;

    // A senha do gerenciador: nem lida nem guardada.
    let selecao = std::env::var("LUCERNA_SELECAO").expect("LUCERNA_SELECAO do isolado.sh");
    let mut secret = Command::new(selecao)
        .args(["oferecer", "text/plain", "x-kde-passwordManagerHint"])
        .env("WAYLAND_DISPLAY", socket())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("rodar o lucernad-selecao");
    secret.stdin.take().unwrap().write_all(b"segredo").unwrap();
    let mut line = String::new();
    BufReader::new(secret.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line.trim(), "pronto");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(count(&t), 4);
    let found = c
        .call(
            "clipboard.list",
            Some(json!({"query": "segredo", "offset": 0, "limit": 10})),
        )
        .await
        .unwrap();
    assert_eq!(found["total"], 0);

    // O PNG de volta, pelo próprio lucernad.
    let images = json!({"kind": "image", "offset": 0, "limit": 10});
    let id = find_id(&c, images, "mime", "image/png").await;
    c.call("clipboard.copy", Some(json!({"id": id})))
        .await
        .unwrap();
    assert_eq!(wl_paste(&["--type", "image/png"]), png);
    assert!(secret.wait().unwrap().success(), "a senha foi substituída");
    assert_eq!(work.pending(), 1);

    // O texto de volta; outro app copia por cima: a seleção deixa de ser nossa.
    let texts = json!({"kind": "text", "offset": 0, "limit": 10});
    let id = find_id(&c, texts, "preview", "Ação real").await;
    c.call("clipboard.copy", Some(json!({"id": id})))
        .await
        .unwrap();
    assert_eq!(wl_paste(&["--no-newline"]), "Ação real".as_bytes());
    tokio::task::spawn_blocking(|| wl_copy(&[], b"outro"))
        .await
        .unwrap();
    wait_until(Duration::from_secs(2), "a seleção solta", || {
        work.pending() == 0
    })
    .await;

    // Os outros testes da sonda começam com a seleção vazia.
    let cleared = Command::new("wl-copy")
        .arg("--clear")
        .env("WAYLAND_DISPLAY", socket())
        .status()
        .expect("rodar o wl-copy --clear");
    assert!(cleared.success());
}

#[test]
#[ignore = "só no compositor isolado (tests/isolado.sh)"]
fn compositor_drop_is_an_event() {
    isolated();
    let mut session = Wayland::system().open(&socket()).expect("abrir a sessão");
    let pid = std::env::var("LUCERNA_SWAY_PID").expect("LUCERNA_SWAY_PID do isolado.sh");
    let status = Command::new("kill")
        .arg(&pid)
        .status()
        .expect("rodar o kill");
    assert!(status.success(), "matar o sway {pid}");
    let event = next_event(&mut session, Duration::from_secs(2), |e| {
        matches!(e, Event::Disconnected(_))
    });
    let Event::Disconnected(why) = event else {
        unreachable!()
    };
    assert!(why.starts_with("O compositor fechou a conexão: "), "{why}");
}
