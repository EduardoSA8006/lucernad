//! O módulo `spectrum` num servidor de verdade, com o `FakeBackend` e o
//! `FakeBus`: o laço guiado pelo assinante e pelo player, os tópicos e o
//! método.

use std::path::{Path, PathBuf};
use std::time::Duration;

use lucerna_ipc::{Client, ClientError};
use lucerna_service::ServerBuilder;
use lucernad_core::ModuleTask;
use lucernad_spectrum::backend::OutputNode;
use lucernad_spectrum::testing::{FakeBackend, FakeBus, fake_config, noise, sine};
use lucernad_spectrum::{Spectrum, SpectrumConfig, SpectrumTopics, register};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::UnixListener;
use tokio::task::JoinHandle;
use tokio::time::Instant;

const SPOTIFY: &str = "org.mpris.MediaPlayer2.spotify";
const BRAVE: &str = "org.mpris.MediaPlayer2.brave.instance119549";
const RATE: u32 = 48_000;

/// Registra o módulo num servidor e serve num socket temporário, sem rodar o
/// laço (quem chama dá o `spawn`).
fn build(cfg: SpectrumConfig) -> (TempDir, PathBuf, SpectrumTopics, Spectrum, JoinHandle<()>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lucernad.sock");
    let mut builder = ServerBuilder::new("lucernad", "0.1.0");
    let spectrum = register(&mut builder, cfg);
    let topics = spectrum.topics.clone();
    let server = builder.build();
    let listener = UnixListener::bind(&path).unwrap();
    let handle = tokio::spawn(async move {
        let _ = server.serve(listener).await;
    });
    (dir, path, topics, spectrum, handle)
}

/// O [`build`] com o laço rodando; o [`ModuleTask`] fica guardado na tarefa
/// do servidor, que vive até o fim do teste.
async fn serve(cfg: SpectrumConfig) -> (TempDir, PathBuf, SpectrumTopics, JoinHandle<()>) {
    let (dir, path, topics, spectrum, server) = build(cfg);
    let task: ModuleTask = spectrum.spawn();
    let handle = tokio::spawn(async move {
        let _task = task;
        let _ = server.await;
    });
    (dir, path, topics, handle)
}

async fn client(path: &Path) -> Client {
    Client::connect(path, "test", "lucernad").await.unwrap()
}

/// Espera `cond` valer até `within`; senão, pânico com `what`.
async fn wait_until(within: Duration, what: &str, mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + within;
    while !cond() {
        assert!(Instant::now() < end, "{what}: não aconteceu em {within:?}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Toca `signal` em laço no nó `serial`: 480 amostras a cada 10 ms (48 kHz).
fn feed(backend: &FakeBackend, serial: u64, signal: Vec<f32>) -> JoinHandle<()> {
    let backend = backend.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(10));
        let mut at = 0;
        loop {
            tick.tick().await;
            let chunk: Vec<f32> = (0..480).map(|i| signal[(at + i) % signal.len()]).collect();
            at = (at + 480) % signal.len();
            backend.play(serial, RATE, &chunk);
        }
    })
}

/// Um segundo de seno de 1 kHz com amplitude 0,5 (o ciclo fecha em 48
/// amostras, então o laço não tem emenda).
fn tone() -> Vec<f32> {
    sine(1000.0, RATE, 0.5, RATE as usize)
}

fn spotify() -> FakeBus {
    FakeBus::new().owner(SPOTIFY, 153543)
}

fn node(id: u32, serial: u64, pid: Option<u32>, binary: Option<&str>) -> OutputNode {
    OutputNode {
        id,
        serial,
        client_pid: pid,
        binary: binary.map(str::to_string),
    }
}

/// Os nós da Tarefa 3, em ordem de `id`.
fn nodes() -> Vec<OutputNode> {
    vec![
        node(79, 178, Some(152224), Some("brave")),
        node(89, 121, Some(153543), Some("spotify")),
        node(95, 190, Some(152224), Some("brave")),
        node(97, 200, Some(1145), Some("plasmashell")),
        node(99, 210, None, None),
    ]
}

/// Os [`nodes`] sem o 121 (o do Spotify).
fn nodes_without_spotify() -> Vec<OutputNode> {
    nodes().into_iter().filter(|n| n.serial != 121).collect()
}

fn backend_with_nodes() -> FakeBackend {
    let backend = FakeBackend::new();
    backend.set_nodes(nodes());
    backend
}

async fn configure(c: &Client, params: Value) -> Result<Value, ClientError> {
    c.call("spectrum.configure", Some(params)).await
}

/// O último quadro publicado, ou `None` antes do primeiro.
fn bands(t: &SpectrumTopics) -> Option<Vec<f64>> {
    let data = t.bands.current().data;
    let bands = data.get("bands")?.as_array()?;
    Some(bands.iter().map(|v| v.as_f64().unwrap()).collect())
}

fn max_band(t: &SpectrumTopics) -> f64 {
    bands(t).map_or(0.0, |b| b.into_iter().fold(0.0, f64::max))
}

fn all_zero(t: &SpectrumTopics) -> bool {
    bands(t).is_some_and(|b| b.len() == 48 && b.iter().all(|&v| v == 0.0))
}

fn status(t: &SpectrumTopics) -> Value {
    t.status.current().data
}

/// Servidor com o `backend` e o `bus`, um cliente assinando
/// `spectrum.bands` e o player `player` configurado.
async fn capturing(
    backend: &FakeBackend,
    bus: FakeBus,
    player: &str,
) -> (TempDir, SpectrumTopics, Client, JoinHandle<()>) {
    let (dir, path, topics, server) = serve(fake_config(backend.clone(), bus)).await;
    let c = client(&path).await;
    c.subscribe(&["spectrum.bands"]).await.unwrap();
    configure(&c, json!({ "player": player })).await.unwrap();
    (dir, topics, c, server)
}

#[tokio::test]
async fn handshake_lists_topics_streams_and_method() {
    let (_d, path, ..) = serve(fake_config(FakeBackend::new(), FakeBus::new())).await;
    let c = client(&path).await;
    assert_eq!(c.hello().topics, ["spectrum.status"]);
    assert_eq!(c.hello().streams, ["spectrum.bands"]);
    assert_eq!(c.hello().methods, ["spectrum.configure"]);
}

#[tokio::test]
async fn status_follows_the_probe() {
    let (_d, _p, t, _s) = serve(fake_config(FakeBackend::new(), FakeBus::new())).await;
    let ok = json!({"available": true, "capturing": false, "player": null, "error": null});
    wait_until(Duration::from_secs(1), "a sonda boa", || status(&t) == ok).await;

    let msg = "Não consegui conectar ao PipeWire: sem socket.";
    let (_d, _p, t, _s) = serve(fake_config(FakeBackend::failing(msg), FakeBus::new())).await;
    let bad = json!({"available": false, "capturing": false, "player": null, "error": msg});
    wait_until(Duration::from_secs(1), "a sonda que falha", || {
        status(&t) == bad
    })
    .await;
}

#[tokio::test]
async fn configure_validates_the_player() {
    let (_d, path, ..) = serve(fake_config(FakeBackend::new(), FakeBus::new())).await;
    let c = client(&path).await;
    for bad in [
        json!({"player": 3}),
        json!({"player": "spotify"}),
        json!({}),
    ] {
        match configure(&c, bad.clone()).await {
            Err(ClientError::Rpc(e)) => assert_eq!(
                (e.code, e.message.as_str()),
                (
                    -32602,
                    "Parâmetros inválidos: player precisa ser um nome org.mpris.MediaPlayer2.* ou null."
                ),
                "{bad}"
            ),
            other => panic!("{bad}: esperava -32602, veio {other:?}"),
        }
    }
    assert_eq!(
        configure(&c, json!({"player": null})).await.unwrap(),
        json!({})
    );
    assert_eq!(
        configure(&c, json!({"player": SPOTIFY})).await.unwrap(),
        json!({})
    );
}

#[tokio::test]
async fn nothing_runs_without_subscriber_or_player() {
    let backend = backend_with_nodes();
    let (_d, path, ..) = serve(fake_config(backend.clone(), spotify())).await;
    let c = client(&path).await;

    configure(&c, json!({ "player": SPOTIFY })).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(backend.opened(), 0, "só o player");

    configure(&c, json!({"player": null})).await.unwrap();
    c.subscribe(&["spectrum.bands"]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(backend.opened(), 0, "só o assinante");

    configure(&c, json!({ "player": SPOTIFY })).await.unwrap();
    wait_until(Duration::from_secs(1), "a sessão com os dois", || {
        backend.sessions() == 1
    })
    .await;
}

#[tokio::test]
async fn captures_the_player_and_publishes_bands() {
    let backend = backend_with_nodes();
    let (_d, t, _c, _s) = capturing(&backend, spotify(), SPOTIFY).await;
    let _feed = feed(&backend, 121, tone());
    wait_until(Duration::from_secs(1), "a captura do 121", || {
        backend.capturing() == [121]
    })
    .await;
    wait_until(Duration::from_secs(2), "um quadro acima de 0,5", || {
        max_band(&t) > 0.5
    })
    .await;
    let b = bands(&t).unwrap();
    assert_eq!(b.len(), 48);
    for v in &b {
        assert!((0.0..=1.0).contains(v), "{v} fora de [0, 1]");
        assert!(
            ((v * 1000.0).round() - v * 1000.0).abs() < 1e-9,
            "{v} com mais de 3 casas"
        );
    }
    assert_eq!(
        status(&t),
        json!({"available": true, "capturing": true, "player": SPOTIFY, "error": null})
    );
}

#[tokio::test]
async fn other_apps_do_not_move_the_bars() {
    let backend = backend_with_nodes();
    let (_d, t, _c, _s) = capturing(&backend, spotify(), SPOTIFY).await;
    wait_until(Duration::from_secs(1), "a captura do 121", || {
        backend.capturing() == [121]
    })
    .await;
    let brave = feed(&backend, 178, tone());
    let end = Instant::now() + Duration::from_millis(500);
    while Instant::now() < end {
        assert_eq!(max_band(&t), 0.0, "o Brave mexeu as barras do Spotify");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // O controle: o player casado mexe as barras, no mesmo servidor.
    let _spotify = feed(&backend, 121, tone());
    wait_until(Duration::from_secs(2), "o Spotify mexe as barras", || {
        max_band(&t) > 0.5
    })
    .await;
    brave.abort();
}

#[tokio::test]
async fn switching_player_moves_the_capture() {
    let backend = backend_with_nodes();
    let bus = spotify().owner(BRAVE, 119549);
    let (_d, t, c, _s) = capturing(&backend, bus, SPOTIFY).await;
    wait_until(Duration::from_secs(1), "a captura do 121", || {
        backend.capturing() == [121]
    })
    .await;
    configure(&c, json!({ "player": BRAVE })).await.unwrap();
    wait_until(Duration::from_secs(1), "a captura do Brave", || {
        backend.capturing() == [178, 190]
    })
    .await;
    assert_eq!(
        (backend.sessions(), backend.opened()),
        (1, 1),
        "a mesma sessão"
    );
    wait_until(Duration::from_secs(1), "o status com o Brave", || {
        status(&t)["player"] == BRAVE
    })
    .await;
}

#[tokio::test]
async fn node_that_leaves_and_returns() {
    let backend = backend_with_nodes();
    let (_d, t, _c, _s) = capturing(&backend, spotify(), SPOTIFY).await;
    let _feed = feed(&backend, 121, tone());
    wait_until(Duration::from_secs(2), "um quadro", || max_band(&t) > 0.5).await;

    backend.set_nodes(nodes_without_spotify());
    wait_until(Duration::from_secs(1), "a captura vazia", || {
        backend.capturing().is_empty()
    })
    .await;
    wait_until(Duration::from_secs(1), "o status sem captura", || {
        status(&t)["capturing"] == false
    })
    .await;
    wait_until(Duration::from_secs(1), "o quadro de zeros", || all_zero(&t)).await;

    backend.set_nodes(nodes());
    wait_until(Duration::from_secs(1), "a captura de volta", || {
        backend.capturing() == [121]
    })
    .await;
}

#[tokio::test]
async fn silent_node_falls_to_zero() {
    let backend = backend_with_nodes();
    let (_d, t, _c, _s) = capturing(&backend, spotify(), SPOTIFY).await;
    let feed = feed(&backend, 121, tone());
    wait_until(Duration::from_secs(2), "um quadro", || max_band(&t) > 0.5).await;
    feed.abort();
    wait_until(Duration::from_secs(1), "o quadro de zeros", || all_zero(&t)).await;
    assert_eq!(backend.capturing(), [121], "o nó continua no grafo");
}

#[tokio::test]
async fn client_info_arriving_later_starts_capture() {
    let backend = FakeBackend::new();
    let mut early = nodes();
    early[1] = node(89, 121, None, None);
    backend.set_nodes(early);
    let (_d, _t, _c, _s) = capturing(&backend, spotify(), SPOTIFY).await;
    wait_until(Duration::from_secs(1), "a sessão", || {
        backend.sessions() == 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        backend.capturing(),
        Vec::<u64>::new(),
        "sem o info do client"
    );

    backend.set_nodes(nodes());
    wait_until(Duration::from_secs(1), "a captura com o info", || {
        backend.capturing() == [121]
    })
    .await;
}

#[tokio::test]
async fn restarted_player_is_found_again() {
    let backend = backend_with_nodes();
    let bus = spotify();
    let (_d, _t, _c, _s) = capturing(&backend, bus.clone(), SPOTIFY).await;
    wait_until(Duration::from_secs(1), "a captura do 121", || {
        backend.capturing() == [121]
    })
    .await;

    // O Spotify reinicia: nó novo, PID novo, sem binário (a reserva por nome
    // não casa; só o PID novo casa).
    let mut restarted = nodes_without_spotify();
    restarted.push(node(120, 300, Some(153600), None));
    restarted.sort_by_key(|n| n.id);
    backend.set_nodes(restarted);
    wait_until(Duration::from_secs(1), "a captura vazia", || {
        backend.capturing().is_empty()
    })
    .await;

    // O dono novo, sem `Graph` novo: só o timer de nova identificação acha.
    bus.set_owner(SPOTIFY, 153600);
    wait_until(Duration::from_secs(2), "a captura do player novo", || {
        backend.capturing() == [300]
    })
    .await;
}

#[tokio::test]
async fn stops_when_the_last_subscriber_leaves() {
    let backend = backend_with_nodes();
    let (_d, t, c, _s) = capturing(&backend, spotify(), SPOTIFY).await;
    let _feed = feed(&backend, 121, tone());
    wait_until(Duration::from_secs(2), "um quadro", || max_band(&t) > 0.5).await;

    c.unsubscribe(&["spectrum.bands"]).await.unwrap();
    wait_until(Duration::from_millis(200), "a sessão fechada", || {
        backend.sessions() == 0
    })
    .await;
    wait_until(Duration::from_millis(200), "o quadro de zeros", || {
        all_zero(&t)
    })
    .await;
    wait_until(Duration::from_millis(200), "o status sem captura", || {
        status(&t)["capturing"] == false
    })
    .await;
}

#[tokio::test]
async fn null_player_closes_the_session() {
    let backend = backend_with_nodes();
    let (_d, t, c, _s) = capturing(&backend, spotify(), SPOTIFY).await;
    wait_until(Duration::from_secs(1), "a sessão", || {
        backend.sessions() == 1
    })
    .await;
    configure(&c, json!({"player": null})).await.unwrap();
    wait_until(Duration::from_secs(1), "a sessão fechada", || {
        backend.sessions() == 0
    })
    .await;
    wait_until(Duration::from_secs(1), "o status sem player", || {
        status(&t) == json!({"available": true, "capturing": false, "player": null, "error": null})
    })
    .await;
}

#[tokio::test]
async fn pipewire_drop_becomes_state_and_retries() {
    let backend = backend_with_nodes();
    let (_d, t, _c, _s) = capturing(&backend, spotify(), SPOTIFY).await;
    wait_until(Duration::from_secs(1), "a captura do 121", || {
        backend.capturing() == [121]
    })
    .await;

    backend.disconnect("x");
    let dropped = json!({
        "available": false,
        "capturing": false,
        "player": SPOTIFY,
        "error": "O PipeWire fechou a conexão."
    });
    wait_until(Duration::from_secs(1), "a queda vira estado", || {
        status(&t) == dropped
    })
    .await;

    wait_until(Duration::from_secs(1), "a nova sessão", || {
        backend.opened() == 2 && backend.capturing() == [121]
    })
    .await;
    wait_until(Duration::from_secs(1), "o status de volta", || {
        status(&t)
            == json!({"available": true, "capturing": true, "player": SPOTIFY, "error": null})
    })
    .await;
}

#[tokio::test]
async fn hanging_dbus_falls_back_to_the_name() {
    let backend = backend_with_nodes();
    let (_d, _t, _c, _s) = capturing(&backend, FakeBus::hanging(), SPOTIFY).await;
    wait_until(
        Duration::from_secs(1),
        "a captura pela reserva por nome",
        || backend.capturing() == [121],
    )
    .await;
}

#[tokio::test]
async fn frames_at_about_60_per_second() {
    let backend = backend_with_nodes();
    let (_d, t, _c, _s) = capturing(&backend, spotify(), SPOTIFY).await;
    let _feed = feed(&backend, 121, noise(7, RATE as usize));
    wait_until(Duration::from_secs(2), "um quadro", || max_band(&t) > 0.0).await;
    let first = t.bands.current().seq;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let frames = t.bands.current().seq - first;
    assert!((40..=70).contains(&frames), "{frames} quadros em 1 s");
}

#[tokio::test]
async fn panic_marks_the_module_unavailable() {
    let backend = FakeBackend::panicking();
    let (_d, path, t, _s) = serve(fake_config(backend, spotify())).await;
    let c = client(&path).await; // vivo até o fim
    c.subscribe(&["spectrum.bands"]).await.unwrap();
    configure(&c, json!({ "player": SPOTIFY })).await.unwrap();
    wait_until(Duration::from_secs(1), "o status do pânico", || {
        status(&t)["error"] == "O módulo de espectro parou por um erro interno."
    })
    .await;
    assert_eq!(
        status(&t),
        json!({
            "available": false,
            "capturing": false,
            "player": null,
            "error": "O módulo de espectro parou por um erro interno."
        })
    );
    let other = client(&path).await;
    assert_eq!(
        other.hello().streams,
        ["spectrum.bands"],
        "o servidor continua atendendo"
    );
    drop(c);
}
