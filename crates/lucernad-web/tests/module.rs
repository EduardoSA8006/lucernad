//! O módulo `web` num servidor de verdade, com os dublês do `fake_config`
//! (`FakeHttp`, `FakeLink`, cache numa pasta temporária, prazos curtos): a
//! letra, o clima, a busca de cidade, o cache, a conectividade e o modo
//! offline. Nada aqui sai para a internet nem fala com o D-Bus.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lucerna_ipc::raw::RawConn;
use lucerna_ipc::{Client, ClientError};
use lucerna_service::ServerBuilder;
use lucernad_web::cache::{CACHE_LIMIT, Cache, Kind};
use lucernad_web::endpoints::base_from_env;
use lucernad_web::http::{HttpError, Request};
use lucernad_web::lyrics::{Found, Lyrics, Source};
use lucernad_web::matching::Track;
use lucernad_web::net::Link;
use lucernad_web::testing::{
    FakeHttp, Fakes, fake_config, forecast_body_from, forecast_today, json as reply, lrclib_entry,
};
use lucernad_web::weather::{Forecast, Location, parse_forecast};
use lucernad_web::{WebConfig, WebTopics, register};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::UnixListener;
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// A base do `fake_config`: o `FakeHttp` responde pelas rotas, nada conecta.
const BASE: &str = "http://127.0.0.1:1";
/// O prazo folgado das esperas.
const LONG: Duration = Duration::from_secs(3);
const PANIC_ERROR: &str = "O módulo de rede parou por um erro interno.";
const OFFLINE_LYRICS: &str = "Modo offline: a letra não é buscada.";
const OFFLINE_WEATHER: &str = "Modo offline: o clima não é atualizado.";
const NO_LINK: &str = "Sem conexão com a internet.";

fn tempo_perdido() -> Track {
    Track {
        artist: "Legião Urbana".to_string(),
        title: "Tempo Perdido".to_string(),
        album: String::new(),
        duration: Some(201.0),
    }
}

fn missing(title: &str) -> Track {
    Track {
        artist: "Ninguém".to_string(),
        title: title.to_string(),
        album: String::new(),
        duration: Some(180.0),
    }
}

fn cacau() -> Location {
    Location {
        name: "Cacaulândia, Rondônia, BR".to_string(),
        latitude: -10.3375,
        longitude: -62.8986,
    }
}

fn ariquemes() -> Location {
    Location {
        name: "Ariquemes, Rondônia, BR".to_string(),
        latitude: -9.9133,
        longitude: -63.0408,
    }
}

fn loc_json(l: &Location) -> Value {
    json!({"name": l.name, "latitude": l.latitude, "longitude": l.longitude})
}

/// O módulo num servidor, num socket em `sock`, com o laço rodando; o
/// `ModuleTask` fica guardado na tarefa do servidor, que vive até o fim do
/// teste.
fn serve(sock: &Path, cfg: WebConfig) -> (PathBuf, WebTopics, JoinHandle<()>) {
    let path = sock.join("lucernad.sock");
    let mut builder = ServerBuilder::new("lucernad", "0.1.0");
    let web = register(&mut builder, cfg);
    let topics = web.topics.clone();
    let server = builder.build();
    let listener = UnixListener::bind(&path).unwrap();
    let task = web.spawn();
    let handle = tokio::spawn(async move {
        let _task = task;
        let _ = server.serve(listener).await;
    });
    (path, topics, handle)
}

/// Os dublês, as pastas e o servidor de um teste.
struct Rig {
    f: Fakes,
    path: PathBuf,
    t: WebTopics,
    _sock: TempDir,
    _server: JoinHandle<()>,
}

/// O módulo com os dados (o cache) em `dir`, com a configuração mexida por
/// `tweak`.
fn rig_on(dir: &Path, tweak: impl FnOnce(&mut WebConfig)) -> Rig {
    let (mut cfg, f) = fake_config(dir);
    tweak(&mut cfg);
    let sock = tempfile::tempdir().unwrap();
    let (path, t, server) = serve(sock.path(), cfg);
    Rig {
        f,
        path,
        t,
        _sock: sock,
        _server: server,
    }
}

fn cache(dir: &Path) -> Cache {
    Cache::new(dir.join("cache"), CACHE_LIMIT)
}

async fn client(path: &Path) -> Client {
    Client::connect(path, "test", "lucernad").await.unwrap()
}

async fn call(c: &Client, method: &str, params: Value) -> Result<Value, ClientError> {
    c.call(method, Some(params)).await
}

async fn ok(c: &Client, method: &str, params: Value) {
    let r = call(c, method, params.clone()).await;
    assert_eq!(r.unwrap(), json!({}), "{method} {params}");
}

/// `web.configure {offline: false}`.
async fn online(c: &Client) {
    ok(c, "web.configure", json!({"offline": false})).await;
}

async fn track(c: &Client, t: &Track) {
    let params = json!({"artist": t.artist, "title": t.title, "duration": t.duration});
    ok(c, "web.track", params).await;
}

/// O código e o texto de um erro do método.
fn rpc_error(result: Result<Value, ClientError>) -> (i32, String) {
    match result {
        Err(ClientError::Rpc(e)) => (e.code, e.message),
        other => panic!("esperava um erro do método: {other:?}"),
    }
}

/// Espera `cond` valer até `within`; senão, pânico com `what`.
async fn wait_until(within: Duration, what: &str, mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + within;
    while !cond() {
        assert!(Instant::now() < end, "{what}: não aconteceu em {within:?}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Guarda cada estado de um tópico, na ordem: uma conexão crua inscrita
/// nele lendo cada linha `lucerna.state` (o retrato da inscrição primeiro).
/// Sem amostragem e sem o mapa do `Client` (que funde os estados que chegam
/// juntos): toda publicação separada da seguinte por um `.await` do laço
/// chega aqui (o runtime do teste roda o escritor da conexão antes de
/// voltar ao laço). A inscrição conta como assinante do tópico.
struct Seq {
    states: Arc<Mutex<Vec<Value>>>,
    task: JoinHandle<()>,
}

impl Drop for Seq {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Seq {
    fn all(&self) -> Vec<Value> {
        self.states.lock().unwrap().clone()
    }

    fn len(&self) -> usize {
        self.states.lock().unwrap().len()
    }
}

/// Guarda um `lucerna.state` (ou o retrato da inscrição) com `seq` novo.
fn keep(log: &Mutex<Vec<Value>>, last: &mut u64, state: &Value) {
    let seq = state["seq"].as_u64().unwrap_or(0);
    if seq > *last || log.lock().unwrap().is_empty() {
        *last = seq;
        log.lock().unwrap().push(state["data"].clone());
    }
}

async fn record(path: &Path, topic: &str) -> Seq {
    let mut conn = RawConn::connect(path).await.unwrap();
    conn.hello("lucernad").await.unwrap();
    conn.send(
        &json!({"jsonrpc": "2.0", "id": 1, "method": "lucerna.subscribe",
                      "params": {"topics": [topic]}}),
    )
    .await
    .unwrap();
    let states = Arc::new(Mutex::new(Vec::new()));
    let mut last = 0;
    // Até a resposta da inscrição: o retrato vem nela.
    let mut early = Vec::new();
    loop {
        let msg = conn.recv().await.unwrap().expect("a conexão do gravador");
        if msg["id"] == 1 {
            for state in msg["result"]["states"].as_array().unwrap() {
                keep(&states, &mut last, state);
            }
            break;
        }
        if msg["method"] == "lucerna.state" {
            early.push(msg["params"].clone());
        }
    }
    for state in &early {
        keep(&states, &mut last, state);
    }
    let log = states.clone();
    let task = tokio::spawn(async move {
        loop {
            match conn.recv().await {
                Ok(Some(msg)) if msg["method"] == "lucerna.state" => {
                    keep(&log, &mut last, &msg["params"]);
                }
                Ok(Some(_)) => {}
                Ok(None) => return,
                // O `recv` desiste em 5 s sem mensagem; o gravador continua.
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => panic!("o gravador: {e}"),
            }
        }
    });
    Seq { states, task }
}

/// Espera um estado guardado pelo [`Seq`] (do índice `from` em diante) que
/// satisfaça `cond`; devolve o índice e o estado.
async fn wait_seq(
    seq: &Seq,
    from: usize,
    what: &str,
    cond: impl Fn(&Value) -> bool,
) -> (usize, Value) {
    let end = Instant::now() + LONG;
    loop {
        let all = seq.all();
        if let Some(i) = (from..all.len()).find(|&i| cond(&all[i])) {
            return (i, all[i].clone());
        }
        assert!(
            Instant::now() < end,
            "{what}: não apareceu em {LONG:?}: {all:?}"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

fn is(v: &Value, key: &str, state: &str) -> bool {
    v["key"] == key && v["state"] == state
}

/// Os pedidos cuja URL termina em `path`.
fn requests_to(f: &Fakes, path: &str) -> Vec<Request> {
    f.http
        .requests()
        .into_iter()
        .filter(|r| r.url.ends_with(path))
        .collect()
}

fn param<'a>(r: &'a Request, k: &str) -> Option<&'a str> {
    r.query
        .iter()
        .find(|(name, _)| name == k)
        .map(|(_, v)| v.as_str())
}

/// A previsão de Cacaulândia gerada a partir de hoje, no formato do cache.
fn cached_forecast() -> Value {
    let body = forecast_body_from(&forecast_today()).to_string();
    parse_forecast(body.as_bytes()).unwrap().to_json()
}

fn unix(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).unwrap().as_secs()
}

#[tokio::test]
async fn handshake_lists_topics_and_methods() {
    let dir = tempfile::tempdir().unwrap();
    let r = rig_on(dir.path(), |_| {});
    let c = client(&r.path).await;
    assert_eq!(
        c.hello().topics,
        ["web.lyrics", "web.weather", "web.status"]
    );
    assert!(c.hello().streams.is_empty());
    assert_eq!(
        c.hello().methods,
        [
            "web.track",
            "web.configure",
            "web.searchCity",
            "web.refresh"
        ]
    );
    assert_eq!(r.t.lyrics.current().data, Value::Null);
    assert_eq!(r.t.weather.current().data, Value::Null);
    assert_eq!(
        r.t.status.current().data,
        json!({"available": true, "error": null, "online": false})
    );
}

#[tokio::test]
async fn params_are_validated() {
    let dir = tempfile::tempdir().unwrap();
    let r = rig_on(dir.path(), |_| {});
    let c = client(&r.path).await;
    let artist = "artist precisa ser um texto com até 500 caracteres";
    let title = "title precisa ser um texto com até 500 caracteres";
    let album = "album precisa ser um texto com até 500 caracteres, ou null";
    let duration = "duration precisa ser um número entre 0 e 86400 (segundos), ou null";
    let offline = "offline precisa ser verdadeiro ou falso";
    let key = "vagalumeKey precisa ser um texto de 1 a 128 letras, números, - ou _, ou null";
    let location = "location precisa ser {name, latitude, longitude} (nome com até 200 caracteres, latitude entre -90 e 90, longitude entre -180 e 180), ou null";
    let refresh = "refresh precisa ser um inteiro de 5 a 1440 (minutos), ou null";
    let query = "query precisa ser um texto de 1 a 100 caracteres";
    let long = "x".repeat(501);
    let cases = [
        ("web.track", json!({"artist": 1, "title": "a"}), artist),
        ("web.track", json!({"title": "a"}), artist),
        ("web.track", json!({"artist": "a", "title": long}), title),
        (
            "web.track",
            json!({"artist": "a", "title": "b", "album": 5}),
            album,
        ),
        (
            "web.track",
            json!({"artist": "a", "title": "b", "duration": -1}),
            duration,
        ),
        (
            "web.track",
            json!({"artist": "a", "title": "b", "duration": "5"}),
            duration,
        ),
        ("web.configure", json!({}), offline),
        ("web.configure", json!({"offline": "não"}), offline),
        (
            "web.configure",
            json!({"offline": false, "vagalumeKey": "tem espaço"}),
            key,
        ),
        (
            "web.configure",
            json!({"offline": false, "location": {"name": "X", "latitude": 91, "longitude": 0}}),
            location,
        ),
        (
            "web.configure",
            json!({"offline": false, "location": {"latitude": 1, "longitude": 0}}),
            location,
        ),
        (
            "web.configure",
            json!({"offline": false, "refresh": 4}),
            refresh,
        ),
        (
            "web.configure",
            json!({"offline": false, "refresh": 1441}),
            refresh,
        ),
        ("web.searchCity", json!({"query": ""}), query),
        ("web.searchCity", json!({"query": "x".repeat(101)}), query),
    ];
    for (method, params, text) in cases {
        let (code, message) = rpc_error(call(&c, method, params.clone()).await);
        assert_eq!(
            (code, message.as_str()),
            (-32602, format!("Parâmetros inválidos: {text}.").as_str()),
            "{method} {params}"
        );
        assert!(!message.contains("tem espaço"), "a mensagem ecoou o valor");
    }
    assert_eq!(r.f.http.count(), 0);
}

#[tokio::test]
async fn nothing_goes_out_before_configure() {
    let dir = tempfile::tempdir().unwrap();
    let r = rig_on(dir.path(), |_| {});
    let c = client(&r.path).await;
    c.subscribe(&["web.lyrics", "web.weather"]).await.unwrap();
    let key = tempo_perdido().key();
    track(&c, &tempo_perdido()).await;
    wait_until(LONG, "a letra em erro", || {
        is(&r.t.lyrics.current().data, &key, "error")
    })
    .await;
    assert_eq!(
        r.t.lyrics.current().data,
        json!({"key": key, "state": "error", "synced": false, "lines": [], "source": "",
               "error": OFFLINE_LYRICS})
    );
    assert_eq!(
        rpc_error(call(&c, "web.searchCity", json!({"query": "Cacau"})).await),
        (
            1001,
            "Modo offline ligado: a busca de cidade usa a internet.".to_string()
        )
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(r.f.http.count(), 0);
    assert_eq!(r.t.weather.current().data, Value::Null);
    assert_eq!(
        r.t.status.current().data,
        json!({"available": true, "error": null, "online": false})
    );
}

#[tokio::test]
async fn lyrics_found_and_cached() {
    let dir = tempfile::tempdir().unwrap();
    let key = tempo_perdido().key();
    {
        let r = rig_on(dir.path(), |_| {});
        let c = client(&r.path).await;
        online(&c).await;
        c.subscribe(&["web.lyrics"]).await.unwrap();
        let seq = record(&r.path, "web.lyrics").await;
        track(&c, &tempo_perdido()).await;
        let (i, _) = wait_seq(&seq, 0, "procurando", |v| is(v, &key, "searching")).await;
        let (_, found) = wait_seq(&seq, i, "achada", |v| is(v, &key, "found")).await;
        assert_eq!(found["source"], "lrclib");
        assert_eq!(found["synced"], true);
        assert_eq!(found["lines"][0]["text"], "Todos os dias quando acordo");
        assert!(found["lines"][0]["time"].as_f64().is_some());
        assert!(found.get("url").is_none() && found.get("error").is_none());
        assert_eq!(requests_to(&r.f, "/api/get").len(), 1);
    }
    let r = rig_on(dir.path(), |_| {});
    let c = client(&r.path).await;
    online(&c).await;
    c.subscribe(&["web.lyrics"]).await.unwrap();
    track(&c, &tempo_perdido()).await;
    wait_until(LONG, "achada do cache", || {
        is(&r.t.lyrics.current().data, &key, "found")
    })
    .await;
    assert_eq!(r.f.http.count(), 0);
}

#[tokio::test]
async fn cached_track_never_passes_through_searching() {
    let dir = tempfile::tempdir().unwrap();
    let key = tempo_perdido().key();
    let found = Found::Lyrics(Lyrics {
        synced: false,
        lines: vec![lucernad_web::lrc::Line {
            time: None,
            text: "Guardada".to_string(),
        }],
        source: Source::Lrclib,
        url: None,
    });
    cache(dir.path())
        .put(Kind::Lyrics, &key, &found.to_json(), SystemTime::now())
        .unwrap();
    let r = rig_on(dir.path(), |_| {});
    let c = client(&r.path).await;
    online(&c).await;
    c.subscribe(&["web.lyrics"]).await.unwrap();
    let seq = record(&r.path, "web.lyrics").await;
    track(&c, &missing("Outra")).await;
    let (i, _) = wait_seq(&seq, 0, "a outra procurando", |v| v["state"] == "searching").await;
    track(&c, &tempo_perdido()).await;
    let (_, v) = wait_seq(&seq, i, "achada", |v| is(v, &key, "found")).await;
    assert_eq!(v["lines"], json!([{"time": null, "text": "Guardada"}]));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !seq.all().iter().any(|v| is(v, &key, "searching")),
        "a faixa guardada passou por searching: {:?}",
        seq.all()
    );
    assert!(
        r.f.http
            .requests()
            .iter()
            .all(|q| param(q, "track_name") != Some("Tempo Perdido"))
    );
}

#[tokio::test]
async fn none_is_cached_and_failure_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let r = rig_on(dir.path(), |_| {});
    let c = client(&r.path).await;
    online(&c).await;
    c.subscribe(&["web.lyrics"]).await.unwrap();
    let seq = record(&r.path, "web.lyrics").await;
    let nothing = missing("Não Existe");
    let tp = tempo_perdido().key();

    track(&c, &nothing).await;
    let (i, v) = wait_seq(&seq, 0, "sem letra", |v| is(v, &nothing.key(), "none")).await;
    assert_eq!(
        v,
        json!({"key": nothing.key(), "state": "none", "synced": false, "lines": [], "source": ""})
    );
    track(&c, &tempo_perdido()).await;
    let (i, _) = wait_seq(&seq, i, "achada", |v| is(v, &tp, "found")).await;
    let before = r.f.http.count();
    track(&c, &nothing).await;
    wait_seq(&seq, i, "sem letra de novo", |v| {
        is(v, &nothing.key(), "none")
    })
    .await;
    assert_eq!(
        r.f.http.count(),
        before,
        "o 'sem letra' guardado foi pedido de novo"
    );

    r.f.http
        .route(&format!("{BASE}/lrclib"), |_| Err(HttpError::Connect));
    let broken = missing("Sem Rede");
    track(&c, &broken).await;
    let (i, v) = wait_seq(&seq, 0, "erro", |v| is(v, &broken.key(), "error")).await;
    assert_eq!(
        v["error"],
        "Não consegui falar com a LRCLIB: não consegui conectar."
    );
    track(&c, &tempo_perdido()).await;
    let (i, _) = wait_seq(&seq, i, "achada do cache", |v| is(v, &tp, "found")).await;
    let before = r.f.http.count();
    track(&c, &broken).await;
    wait_seq(&seq, i, "erro de novo", |v| is(v, &broken.key(), "error")).await;
    assert!(r.f.http.count() > before, "a falha ficou guardada");
}

#[tokio::test]
async fn skipping_tracks_never_shows_old_lyrics() {
    let dir = tempfile::tempdir().unwrap();
    let r = rig_on(dir.path(), |_| {});
    // Toda faixa tem letra: uma busca antiga que escapasse publicaria a dela.
    r.f.http.route(&format!("{BASE}/lrclib"), |q| {
        let artist = param(q, "artist_name").unwrap_or_default();
        let title = param(q, "track_name").unwrap_or_default();
        let entry = lrclib_entry(artist, title, 180.0, None, Some("[00:01.00]Letra"));
        Ok(reply(200, entry))
    });
    let c = client(&r.path).await;
    online(&c).await;
    let seq = record(&r.path, "web.lyrics").await;
    // Cada troca chega com a busca da faixa anterior em voo (parada).
    r.f.http.hold();
    let skipped: Vec<Track> = (0..9).map(|i| missing(&format!("Faixa {i}"))).collect();
    for (i, t) in skipped.iter().enumerate() {
        track(&c, t).await;
        wait_until(LONG, &format!("a busca da faixa {i} em voo"), || {
            r.f.http.count() == i + 1
        })
        .await;
    }
    track(&c, &tempo_perdido()).await;
    let key = tempo_perdido().key();
    wait_until(LONG, "a busca da última em voo", || r.f.http.count() == 10).await;
    r.f.http.release();
    wait_seq(&seq, 0, "a última achada", |v| is(v, &key, "found")).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(is(&r.t.lyrics.current().data, &key, "found"));
    let all = seq.all();
    assert!(
        all.iter()
            .filter(|v| v["state"] == "found")
            .all(|v| v["key"] == key),
        "uma letra de outra faixa foi publicada: {all:?}"
    );
    assert_eq!(r.f.http.max_in_flight(), 1);
    assert_eq!(requests_to(&r.f, "/api/get").len(), 10);
    assert_eq!(r.f.http.count(), 10, "uma faixa pulada seguiu buscando");
}

#[tokio::test]
async fn lyrics_only_with_subscriber() {
    let dir = tempfile::tempdir().unwrap();
    let r = rig_on(dir.path(), |_| {});
    let c = client(&r.path).await;
    online(&c).await;
    track(&c, &tempo_perdido()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(r.f.http.count(), 0);
    c.subscribe(&["web.lyrics"]).await.unwrap();
    let key = tempo_perdido().key();
    wait_until(LONG, "achada", || {
        is(&r.t.lyrics.current().data, &key, "found")
    })
    .await;
    assert_eq!(requests_to(&r.f, "/api/get").len(), 1);
    assert_eq!(r.f.http.count(), 1);
}

#[tokio::test]
async fn vagalume_key_never_leaks() {
    let dir = tempfile::tempdir().unwrap();
    let r = rig_on(dir.path(), |_| {});
    r.f.http
        .route(&format!("{BASE}/vagalume"), |_| Err(HttpError::Connect));
    let c = client(&r.path).await;
    let lyrics = record(&r.path, "web.lyrics").await;
    let weather = record(&r.path, "web.weather").await;
    let status = record(&r.path, "web.status").await;
    ok(
        &c,
        "web.configure",
        json!({"offline": false, "vagalumeKey": "CHAVE-SECRETA"}),
    )
    .await;
    c.subscribe(&["web.lyrics", "web.weather"]).await.unwrap();
    let nothing = missing("Só No Vagalume");
    track(&c, &nothing).await;
    let (_, v) = wait_seq(&lyrics, 0, "erro do Vagalume", |v| {
        is(v, &nothing.key(), "error")
    })
    .await;
    assert_eq!(
        v["error"],
        "Não consegui falar com o Vagalume: não consegui conectar."
    );
    assert!(
        r.f.http
            .requests()
            .iter()
            .any(|q| param(q, "apikey") == Some("CHAVE-SECRETA"))
    );
    for seq in [&lyrics, &weather, &status] {
        for state in seq.all() {
            assert!(!state.to_string().contains("CHAVE-SECRETA"), "{state}");
        }
    }
}

#[tokio::test]
async fn weather_cached_is_published_at_once_and_refreshed_in_background() {
    let dir = tempfile::tempdir().unwrap();
    let old = SystemTime::now() - Duration::from_secs(3600);
    cache(dir.path())
        .put(Kind::Weather, &cacau().place_key(), &cached_forecast(), old)
        .unwrap();
    let r = rig_on(dir.path(), |_| {});
    let c = client(&r.path).await;
    r.f.http.hold();
    ok(
        &c,
        "web.configure",
        json!({"offline": false, "location": loc_json(&cacau())}),
    )
    .await;
    // Sem assinante, o guardado fica publicado e nada é pedido.
    assert_eq!(r.t.weather.current().data["state"], "ready");
    assert_eq!(r.f.http.count(), 0);
    // A inscrição do gravador é o assinante: o primeiro estado é o retrato.
    let seq = record(&r.path, "web.weather").await;
    let (i, first) = wait_seq(&seq, 0, "a primeira", |v| !v.is_null()).await;
    assert_eq!(i, 0);
    assert_eq!(first["state"], "ready");
    assert_eq!(first["updatedAt"], unix(old));
    assert_eq!(first["location"], loc_json(&cacau()));
    assert_eq!(first["daily"].as_array().unwrap().len(), 7);
    assert_eq!(first["sunrise"], format!("{}T05:41", forecast_today()));
    let (i, loading) = wait_seq(&seq, i, "buscando", |v| v["state"] == "loading").await;
    assert_eq!(loading["updatedAt"], unix(old));
    assert_eq!(loading["current"], first["current"]);
    wait_until(LONG, "o pedido em voo", || r.f.http.in_flight() == 1).await;
    r.f.http.release();
    let (_, ready) = wait_seq(&seq, i, "atualizada", |v| v["state"] == "ready").await;
    assert!(ready["updatedAt"].as_u64().unwrap() > unix(old));
    assert!(ready.get("error").is_none());
}

#[tokio::test]
async fn fresh_weather_is_not_refetched() {
    let dir = tempfile::tempdir().unwrap();
    cache(dir.path())
        .put(
            Kind::Weather,
            &cacau().place_key(),
            &cached_forecast(),
            SystemTime::now(),
        )
        .unwrap();
    let r = rig_on(dir.path(), |_| {});
    let c = client(&r.path).await;
    c.subscribe(&["web.weather"]).await.unwrap();
    // O cache guarda o segundo: com o `refresh` de 30 (300 ms no relógio do
    // teste), o arredondamento poderia fazer o guardado parecer velho.
    ok(
        &c,
        "web.configure",
        json!({"offline": false, "location": loc_json(&cacau()), "refresh": 600}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(r.f.http.count(), 0);
    assert_eq!(r.t.weather.current().data["state"], "ready");
}

#[tokio::test]
async fn weather_error_keeps_the_old_data() {
    let dir = tempfile::tempdir().unwrap();
    let old = SystemTime::now() - Duration::from_secs(3600);
    cache(dir.path())
        .put(Kind::Weather, &cacau().place_key(), &cached_forecast(), old)
        .unwrap();
    let r = rig_on(dir.path(), |_| {});
    r.f.http
        .route(&format!("{BASE}/forecast"), |_| Err(HttpError::Status(500)));
    let c = client(&r.path).await;
    let seq = record(&r.path, "web.weather").await;
    c.subscribe(&["web.weather"]).await.unwrap();
    ok(
        &c,
        "web.configure",
        json!({"offline": false, "location": loc_json(&cacau())}),
    )
    .await;
    let (_, v) = wait_seq(&seq, 0, "o erro", |v| v["state"] == "error").await;
    assert_eq!(
        v["error"],
        "Não consegui falar com o Open-Meteo: o servidor respondeu 500."
    );
    assert_eq!(v["updatedAt"], unix(old));
    assert_eq!(v["current"]["temperature"], 31.2);
    assert_eq!(v["daily"].as_array().unwrap().len(), 7);
    let after = r.f.http.count();
    wait_until(LONG, "outra tentativa", || r.f.http.count() > after).await;
}

#[tokio::test]
async fn offline_and_no_link_send_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let r = rig_on(dir.path(), |_| {});
    let c = client(&r.path).await;
    ok(
        &c,
        "web.configure",
        json!({"offline": true, "location": loc_json(&cacau())}),
    )
    .await;
    c.subscribe(&["web.lyrics", "web.weather"]).await.unwrap();
    let key = tempo_perdido().key();
    track(&c, &tempo_perdido()).await;
    ok(&c, "web.refresh", json!({})).await;
    assert_eq!(
        rpc_error(call(&c, "web.searchCity", json!({"query": "Cacau"})).await),
        (
            1001,
            "Modo offline ligado: a busca de cidade usa a internet.".to_string()
        )
    );
    wait_until(LONG, "a letra em erro", || {
        r.t.lyrics.current().data["error"] == OFFLINE_LYRICS
    })
    .await;
    let weather = r.t.weather.current().data;
    assert_eq!(
        weather,
        json!({"location": loc_json(&cacau()), "state": "error", "updatedAt": null,
               "current": null, "daily": [], "sunrise": null, "sunset": null,
               "error": OFFLINE_WEATHER})
    );

    r.f.link.set(Link::Offline);
    ok(
        &c,
        "web.configure",
        json!({"offline": false, "location": loc_json(&cacau())}),
    )
    .await;
    assert_eq!(
        rpc_error(call(&c, "web.searchCity", json!({"query": "Cacau"})).await),
        (1002, NO_LINK.to_string())
    );
    ok(&c, "web.refresh", json!({})).await;
    wait_until(LONG, "sem conexão", || {
        r.t.lyrics.current().data["error"] == NO_LINK
            && r.t.weather.current().data["error"] == NO_LINK
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(r.f.http.count(), 0);

    r.f.link.set(Link::Online);
    wait_until(LONG, "a letra achada", || {
        is(&r.t.lyrics.current().data, &key, "found")
    })
    .await;
    wait_until(LONG, "o clima pronto", || {
        r.t.weather.current().data["state"] == "ready"
    })
    .await;
    assert_eq!(requests_to(&r.f, "/v1/forecast").len(), 1);
}

#[tokio::test]
async fn going_offline_mid_fetch_sends_nothing_more() {
    let dir = tempfile::tempdir().unwrap();
    let r = rig_on(dir.path(), |_| {});
    let c = client(&r.path).await;
    let lyrics = record(&r.path, "web.lyrics").await;
    let weather = record(&r.path, "web.weather").await;
    r.f.http.hold();
    ok(
        &c,
        "web.configure",
        json!({"offline": false, "location": loc_json(&cacau())}),
    )
    .await;
    c.subscribe(&["web.lyrics", "web.weather"]).await.unwrap();
    track(&c, &tempo_perdido()).await;
    wait_until(LONG, "os dois em voo", || r.f.http.in_flight() == 2).await;
    ok(
        &c,
        "web.configure",
        json!({"offline": true, "location": loc_json(&cacau())}),
    )
    .await;
    let (l_from, w_from) = (lyrics.len(), weather.len());
    let sent = r.f.http.count();
    r.f.http.release();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(r.f.http.count(), sent);
    assert_eq!(r.f.http.in_flight(), 0);
    let late: Vec<Value> = weather.all()[w_from..]
        .iter()
        .chain(&lyrics.all()[l_from..])
        .filter(|v| v["state"] == "ready" || v["state"] == "found")
        .cloned()
        .collect();
    assert!(late.is_empty(), "publicado depois do offline: {late:?}");
    assert_eq!(r.t.weather.current().data["error"], OFFLINE_WEATHER);
    assert_eq!(r.t.lyrics.current().data["error"], OFFLINE_LYRICS);
}

#[tokio::test]
async fn changing_city_mid_fetch_publishes_only_the_new_one() {
    let dir = tempfile::tempdir().unwrap();
    let r = rig_on(dir.path(), |_| {});
    let a_lat = cacau().latitude.to_string();
    r.f.http.route(&format!("{BASE}/forecast"), move |q| {
        let mut body = forecast_body_from(&forecast_today());
        if param(q, "latitude") != Some(a_lat.as_str()) {
            body["current"]["temperature_2m"] = json!(10.5);
        }
        Ok(reply(200, body))
    });
    let c = client(&r.path).await;
    let seq = record(&r.path, "web.weather").await;
    r.f.http.hold();
    c.subscribe(&["web.weather"]).await.unwrap();
    ok(
        &c,
        "web.configure",
        json!({"offline": false, "location": loc_json(&cacau())}),
    )
    .await;
    wait_until(LONG, "A em voo", || r.f.http.in_flight() == 1).await;
    ok(
        &c,
        "web.configure",
        json!({"offline": false, "location": loc_json(&ariquemes())}),
    )
    .await;
    wait_until(LONG, "B em voo", || {
        requests_to(&r.f, "/v1/forecast").len() == 2 && r.f.http.in_flight() == 1
    })
    .await;
    r.f.http.release();
    let (_, v) = wait_seq(&seq, 0, "B pronta", |v| {
        v["state"] == "ready" && v["location"] == loc_json(&ariquemes())
    })
    .await;
    assert_eq!(v["current"]["temperature"], 10.5);
    tokio::time::sleep(Duration::from_millis(50)).await;
    for state in seq.all() {
        assert!(
            !(state["location"] == loc_json(&ariquemes())
                && state["current"]["temperature"] == 31.2),
            "os dados de A sob o nome de B: {state}"
        );
    }
    let (_, saved) = cache(dir.path())
        .get(Kind::Weather, &ariquemes().place_key())
        .expect("o cache de B");
    let saved = Forecast::from_json(&saved).unwrap();
    assert_eq!(saved.current.temperature, 10.5);
}

#[tokio::test]
async fn city_search_results() {
    let dir = tempfile::tempdir().unwrap();
    let r = rig_on(dir.path(), |_| {});
    let c = client(&r.path).await;
    online(&c).await;
    let found = call(&c, "web.searchCity", json!({"query": "Cacau"}))
        .await
        .unwrap();
    let results = found["results"].as_array().unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(
        results[0],
        json!({"name": "Cidade 1", "state": "Rondônia", "country": "Brasil",
               "countryCode": "BR", "latitude": -10.1, "longitude": -62.1})
    );
    let asked = requests_to(&r.f, "/v1/search");
    assert_eq!(param(&asked[0], "name"), Some("Cacau"));

    r.f.http
        .route(&format!("{BASE}/geocoding"), |_| Err(HttpError::Timeout));
    assert_eq!(
        rpc_error(call(&c, "web.searchCity", json!({"query": "Cacau"})).await),
        (
            1003,
            "Não consegui falar com o Open-Meteo: o servidor demorou demais para responder."
                .to_string()
        )
    );
    r.f.http.route(&format!("{BASE}/geocoding"), |_| {
        Ok(reply(200, json!({"results": "nada"})))
    });
    assert_eq!(
        rpc_error(call(&c, "web.searchCity", json!({"query": "Cacau"})).await),
        (
            1003,
            "Não consegui falar com o Open-Meteo: a resposta veio fora do formato.".to_string()
        )
    );
}

#[tokio::test]
async fn refresh_fetches_now_without_subscriber() {
    let dir = tempfile::tempdir().unwrap();
    let r = rig_on(dir.path(), |_| {});
    let c = client(&r.path).await;
    ok(
        &c,
        "web.configure",
        json!({"offline": false, "location": loc_json(&cacau())}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(r.f.http.count(), 0);
    ok(&c, "web.refresh", json!({})).await;
    let saved = cache(dir.path());
    wait_until(LONG, "o cache gravado", || {
        saved.get(Kind::Weather, &cacau().place_key()).is_some()
    })
    .await;
    wait_until(LONG, "pronto", || {
        r.t.weather.current().data["state"] == "ready"
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(requests_to(&r.f, "/v1/forecast").len(), 1);
    assert_eq!(r.f.http.count(), 1);
}

#[tokio::test]
async fn status_follows_configure_and_link() {
    let dir = tempfile::tempdir().unwrap();
    let r = rig_on(dir.path(), |_| {});
    let c = client(&r.path).await;
    let status = |online: bool| json!({"available": true, "error": null, "online": online});
    assert_eq!(r.t.status.current().data, status(false));
    online(&c).await;
    assert_eq!(r.t.status.current().data, status(true));
    r.f.link.set(Link::Offline);
    wait_until(LONG, "sem conexão", || {
        r.t.status.current().data == status(false)
    })
    .await;
    r.f.link.set(Link::Online);
    wait_until(LONG, "com conexão", || {
        r.t.status.current().data == status(true)
    })
    .await;
    ok(&c, "web.configure", json!({"offline": true})).await;
    assert_eq!(r.t.status.current().data, status(false));
}

#[tokio::test]
async fn bad_base_makes_the_module_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let invalid = base_from_env(Some("https://lrclib.net".as_ref())).unwrap_err();
    let error = invalid.clone();
    let r = rig_on(dir.path(), move |cfg| cfg.base_error = Some(error));
    let c = client(&r.path).await;
    assert_eq!(
        r.t.status.current().data,
        json!({"available": false, "error": invalid, "online": false})
    );
    let unavailable = format!(
        "O módulo de rede está indisponível: {}.",
        invalid.trim_end_matches('.')
    );
    for (method, params) in [
        ("web.searchCity", json!({"query": "Cacau"})),
        ("web.track", json!({"artist": "a", "title": "b"})),
        ("web.configure", json!({"offline": false})),
        ("web.refresh", json!({})),
    ] {
        assert_eq!(
            rpc_error(call(&c, method, params).await),
            (1004, unavailable.clone()),
            "{method}"
        );
    }
    c.subscribe(&["web.lyrics", "web.weather"]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(r.f.http.count(), 0);
    assert_eq!(r.t.lyrics.current().data, Value::Null);
}

#[tokio::test]
async fn panic_marks_the_module_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let r = rig_on(dir.path(), |cfg| cfg.http = Arc::new(FakeHttp::panicking()));
    let c = client(&r.path).await;
    online(&c).await;
    c.subscribe(&["web.lyrics"]).await.unwrap();
    track(&c, &tempo_perdido()).await;
    let panicked = json!({"available": false, "error": PANIC_ERROR, "online": false});
    wait_until(LONG, "o erro interno", || {
        r.t.status.current().data == panicked
    })
    .await;
    let again = client(&r.path).await;
    assert_eq!(again.hello().app, "lucernad");
    let unavailable = format!(
        "O módulo de rede está indisponível: {}.",
        PANIC_ERROR.trim_end_matches('.')
    );
    assert_eq!(
        rpc_error(call(&again, "web.track", json!({"artist": "a", "title": "b"})).await),
        (1004, unavailable.clone())
    );
    assert_eq!(
        rpc_error(call(&again, "web.searchCity", json!({"query": "Cacau"})).await),
        (1004, unavailable)
    );
}
