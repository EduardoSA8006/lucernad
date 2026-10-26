//! Ajudantes para os testes (feature `testing`): o servidor HTTP local
//! ([`LocalServer`], só em 127.0.0.1), o [`Http`] falso ([`FakeHttp`]), a
//! conectividade falsa ([`FakeLink`]) e a trava da sonda ([`isolated`]).
//!
//! Nada aqui sai para a internet: o [`LocalServer`] escuta só no loopback e o
//! [`FakeHttp`] nunca abre uma conexão; o [`FakeLink`] nunca fala com o D-Bus.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::http::{BoxFuture, Http, HttpError, Request, Response};
use crate::net::{Connectivity, Link};

/// O maior cabeçalho de pedido que o [`LocalServer`] lê.
const HEAD_MAX: usize = 64 * 1024;

/// Um pedido que o [`LocalServer`] recebeu.
#[derive(Clone, Debug)]
pub struct Seen {
    /// O caminho, sem a query.
    pub path: String,
    /// A query, decodificada.
    pub query: Vec<(String, String)>,
    /// O `User-Agent` (vazio se não veio).
    pub user_agent: String,
}

/// O que o [`LocalServer`] responde a um pedido.
#[derive(Clone, Debug)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Quanto esperar antes de responder (ou de fechar).
    pub delay: Duration,
    /// Fecha a conexão sem responder.
    pub drop: bool,
}

impl Reply {
    /// Um JSON com `Content-Type: application/json`.
    pub fn json(status: u16, value: &Value) -> Reply {
        Reply::bytes(status, value.to_string().into_bytes())
            .header("Content-Type", "application/json")
    }

    /// Um texto com `Content-Type: text/plain; charset=utf-8`.
    pub fn text(status: u16, text: &str) -> Reply {
        Reply::bytes(status, text.as_bytes().to_vec())
            .header("Content-Type", "text/plain; charset=utf-8")
    }

    /// Bytes, sem `Content-Type`.
    pub fn bytes(status: u16, body: Vec<u8>) -> Reply {
        Reply {
            status,
            headers: Vec::new(),
            body,
            delay: Duration::ZERO,
            drop: false,
        }
    }

    /// Responde depois de `delay`.
    pub fn after(mut self, delay: Duration) -> Reply {
        self.delay = delay;
        self
    }

    /// Acrescenta um cabeçalho; um `Content-Length` substitui o calculado.
    pub fn header(mut self, k: &str, v: &str) -> Reply {
        self.headers.push((k.to_string(), v.to_string()));
        self
    }

    /// Lê o pedido e fecha a conexão sem responder.
    pub fn dropped() -> Reply {
        Reply {
            drop: true,
            ..Reply::bytes(0, Vec::new())
        }
    }
}

type Handler = Arc<dyn Fn(&Seen) -> Reply + Send + Sync + 'static>;

/// Um servidor HTTP/1.1 mínimo em 127.0.0.1 (porta livre), numa tarefa do
/// runtime do teste; uma conexão por pedido (`Connection: close`). Para
/// quando é solto.
pub struct LocalServer {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    task: JoinHandle<()>,
}

impl LocalServer {
    /// Sobe o servidor; `handler` decide a resposta de cada pedido.
    pub async fn start(handler: impl Fn(&Seen) -> Reply + Send + Sync + 'static) -> LocalServer {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .expect("escutar em 127.0.0.1");
        let port = listener.local_addr().expect("porta local").port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler: Handler = Arc::new(handler);
        let log = seen.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, handler.clone(), log.clone()));
            }
        });
        LocalServer {
            base: format!("http://127.0.0.1:{port}"),
            seen,
            task,
        }
    }

    /// A base, `http://127.0.0.1:<porta>`, sem barra no fim.
    pub fn base(&self) -> String {
        self.base.clone()
    }

    /// Os pedidos recebidos, em ordem de chegada.
    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Atende uma conexão: lê o pedido, anota, espera e responde (ou fecha).
async fn serve(mut stream: TcpStream, handler: Handler, log: Arc<Mutex<Vec<Seen>>>) {
    let Some(seen) = read_request(&mut stream).await else {
        return;
    };
    log.lock().unwrap().push(seen.clone());
    let reply = handler(&seen);
    if !reply.delay.is_zero() {
        tokio::time::sleep(reply.delay).await;
    }
    if reply.drop {
        return;
    }
    let mut head = format!("HTTP/1.1 {} Lucerna\r\nConnection: close\r\n", reply.status);
    // Um `Content-Length` dado no `Reply` substitui o calculado (para um
    // teste mandar um corpo mais curto que o anunciado).
    if !reply
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-length"))
    {
        head.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
    }
    for (k, v) in &reply.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    // O cliente pode já ter desistido (prazo, corpo grande demais).
    if stream.write_all(head.as_bytes()).await.is_ok() {
        let _ = stream.write_all(&reply.body).await;
        let _ = stream.shutdown().await;
    }
}

/// Lê a linha do pedido e os cabeçalhos (até [`HEAD_MAX`]).
async fn read_request(stream: &mut TcpStream) -> Option<Seen> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > HEAD_MAX {
            return None;
        }
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..end]);
    let mut lines = head.split("\r\n");
    let target = lines.next()?.split(' ').nth(1)?.to_string();
    let user_agent = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("user-agent"))
        .map(|(_, v)| v.trim().to_string())
        .unwrap_or_default();
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let query = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(k), decode(v))
        })
        .collect();
    Some(Seen {
        path: path.to_string(),
        query,
        user_agent,
    })
}

/// Decodifica um pedaço de query (`+` é espaço, `%XX` é um byte).
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(hi), Some(lo)) => {
                    out.push(hi << 4 | lo);
                    i += 2;
                }
                _ => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// O valor de um dígito hexadecimal.
fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

/// Uma base num porto de 127.0.0.1 onde ninguém escuta.
pub fn closed_port() -> String {
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("porta livre");
    let port = listener.local_addr().expect("porta local").port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

/// Uma resposta JSON, para as rotas do [`FakeHttp`].
pub fn json(status: u16, v: Value) -> Response {
    Response {
        status,
        retry_after: None,
        body: v.to_string().into_bytes(),
    }
}

type Route = Arc<dyn Fn(&Request) -> Result<Response, HttpError> + Send + Sync + 'static>;

struct FakeState {
    routes: Mutex<HashMap<String, Route>>,
    requests: Mutex<Vec<Request>>,
    /// Se os pedidos ficam parados, e a geração do `release` (cada um soma
    /// 1): o pedido parado sai quando vê uma geração nova, mesmo que um
    /// `hold` venha logo depois.
    held: watch::Sender<(bool, u64)>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    panicking: bool,
}

/// Um [`Http`] falso: responde pelas rotas registradas, anota os pedidos e
/// pode segurá-los ([`hold`](FakeHttp::hold)) para o teste ver o que está em
/// voo. Clonar dá outra alça para o mesmo estado.
#[derive(Clone)]
pub struct FakeHttp {
    state: Arc<FakeState>,
}

impl Default for FakeHttp {
    fn default() -> FakeHttp {
        FakeHttp::new()
    }
}

impl FakeHttp {
    /// Sem rotas: todo pedido falha com `Connect`.
    pub fn new() -> FakeHttp {
        FakeHttp::build(false)
    }

    /// Um que entra em pânico em todo pedido (depois de anotá-lo).
    pub fn panicking() -> FakeHttp {
        FakeHttp::build(true)
    }

    fn build(panicking: bool) -> FakeHttp {
        FakeHttp {
            state: Arc::new(FakeState {
                routes: Mutex::new(HashMap::new()),
                requests: Mutex::new(Vec::new()),
                held: watch::Sender::new((false, 0)),
                in_flight: AtomicUsize::new(0),
                max_in_flight: AtomicUsize::new(0),
                panicking,
            }),
        }
    }

    /// Responde por `f` os pedidos cuja `url` começa por `prefix`. O mesmo
    /// prefixo de novo substitui a rota; entre prefixos diferentes, vale o
    /// mais longo.
    pub fn route(
        &self,
        prefix: &str,
        f: impl Fn(&Request) -> Result<Response, HttpError> + Send + Sync + 'static,
    ) {
        self.state
            .routes
            .lock()
            .unwrap()
            .insert(prefix.to_string(), Arc::new(f));
    }

    /// Os pedidos recebidos, em ordem.
    pub fn requests(&self) -> Vec<Request> {
        self.state.requests.lock().unwrap().clone()
    }

    /// Quantos pedidos chegaram.
    pub fn count(&self) -> usize {
        self.state.requests.lock().unwrap().len()
    }

    /// Os pedidos ficam parados até o [`release`](FakeHttp::release).
    pub fn hold(&self) {
        self.state.held.send_modify(|(held, _)| *held = true);
    }

    /// Solta os pedidos parados (mesmo que um `hold` venha logo depois) e os
    /// próximos, até um novo `hold`.
    pub fn release(&self) {
        self.state.held.send_modify(|(held, generation)| {
            *held = false;
            *generation += 1;
        });
    }

    /// Quantos pedidos estão em voo agora.
    pub fn in_flight(&self) -> usize {
        self.state.in_flight.load(Ordering::SeqCst)
    }

    /// O pico de pedidos simultâneos.
    pub fn max_in_flight(&self) -> usize {
        self.state.max_in_flight.load(Ordering::SeqCst)
    }

    fn answer(&self, req: &Request) -> Result<Response, HttpError> {
        let route = {
            let routes = self.state.routes.lock().unwrap();
            routes
                .iter()
                .filter(|(prefix, _)| req.url.starts_with(prefix.as_str()))
                .max_by_key(|(prefix, _)| prefix.len())
                .map(|(_, f)| f.clone())
        };
        match route {
            Some(f) => f(req),
            None => Err(HttpError::Connect),
        }
    }
}

/// Conta um pedido em voo; sai da conta quando o futuro termina ou é
/// abortado.
struct InFlight(Arc<FakeState>);

impl InFlight {
    fn enter(state: &Arc<FakeState>) -> InFlight {
        let now = state.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        state.max_in_flight.fetch_max(now, Ordering::SeqCst);
        InFlight(state.clone())
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Http for FakeHttp {
    fn get<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        Box::pin(async move {
            self.state.requests.lock().unwrap().push(req.clone());
            let _guard = InFlight::enter(&self.state);
            let mut held = self.state.held.subscribe();
            let start = held.borrow_and_update().1;
            // O `Sender` vive no estado: o `wait_for` só termina no `release`.
            let _ = held
                .wait_for(|&(held, generation)| !held || generation != start)
                .await;
            if self.state.panicking {
                panic!("FakeHttp::panicking: pânico no pedido");
            }
            self.answer(req)
        })
    }
}

/// Uma entrada da LRCLIB (do `get` ou da busca), com as letras dadas (ou
/// `null`).
pub fn lrclib_entry(
    artist: &str,
    title: &str,
    duration: f64,
    plain: Option<&str>,
    synced: Option<&str>,
) -> Value {
    serde_json::json!({
        "id": 1,
        "trackName": title,
        "artistName": artist,
        "albumName": "Dois",
        "duration": duration,
        "instrumental": false,
        "plainLyrics": plain,
        "syncedLyrics": synced,
    })
}

/// Uma resposta do `search.php` do Vagalume com uma música.
pub fn vagalume_body(kind: &str, artist: &str, title: &str, url: &str, text: &str) -> Value {
    serde_json::json!({
        "type": kind,
        "art": {"id": "3ade68b3g1f86eda3", "name": artist, "url": "https://www.vagalume.com.br/legiao-urbana/"},
        "mus": [{"id": "3ade68b6g9e4cfda3", "name": title, "url": url, "lang": 1, "text": text}],
    })
}

/// 2026-10-24, em dias desde 1970-01-01.
const FORECAST_START: i64 = 20_750;

/// A previsão do Open-Meteo de 7 dias, de 24 a 30 de outubro de 2026, em
/// Cacaulândia (−4 h).
pub fn forecast_body() -> Value {
    forecast_body_days(7)
}

/// A previsão com `days` dias a partir de 2026-10-24.
pub fn forecast_body_days(days: usize) -> Value {
    forecast_body_at(FORECAST_START, days)
}

/// A previsão com `days` dias a partir do dia `start` (em dias desde
/// 1970-01-01); nascer às 05:41 e pôr às 18:03 em todos.
fn forecast_body_at(start: i64, days: usize) -> Value {
    let dates: Vec<String> = (0..days)
        .map(|i| crate::weather::civil_date(start + i as i64))
        .collect();
    let each = |f: &dyn Fn(usize) -> Value| -> Value { (0..days).map(f).collect() };
    serde_json::json!({
        "latitude": -10.34,
        "longitude": -62.9,
        "utc_offset_seconds": -14400,
        "timezone": "America/Porto_Velho",
        "current": {
            "time": format!("{}T10:00", dates.first().cloned().unwrap_or_default()),
            "temperature_2m": 31.2,
            "apparent_temperature": 35.0,
            "relative_humidity_2m": 58,
            "weather_code": 2,
            "wind_speed_10m": 7.6,
            "is_day": 1,
        },
        "daily": {
            "time": dates,
            "weather_code": each(&|i| Value::from([95, 3, 61, 2, 1, 80, 0][i % 7])),
            "temperature_2m_max": each(&|i| Value::from(33.4 - i as f64 * 0.1)),
            "temperature_2m_min": each(&|i| Value::from(22.1 + i as f64 * 0.1)),
            "precipitation_probability_max": each(&|i| Value::from(80 - (i as i64 * 10) % 80)),
            "sunrise": each(&|i| Value::from(format!("{}T05:41", dates[i]))),
            "sunset": each(&|i| Value::from(format!("{}T18:03", dates[i]))),
        },
    })
}

/// Uma resposta da busca de cidade com `n` cidades (`Cidade 1`…).
pub fn cities_body(n: usize) -> Value {
    let results: Vec<Value> = (1..=n)
        .map(|i| {
            serde_json::json!({
                "id": i,
                "name": format!("Cidade {i}"),
                "latitude": -10.0 - i as f64 / 10.0,
                "longitude": -62.0 - i as f64 / 10.0,
                "country_code": "BR",
                "country": "Brasil",
                "admin1": "Rondônia",
            })
        })
        .collect();
    serde_json::json!({"results": results, "generationtime_ms": 0.5})
}

/// O dublê do [`Connectivity`]: o teste muda o estado na mão com o
/// [`FakeLink::set`]. Os clones dividem o mesmo estado.
#[derive(Clone)]
pub struct FakeLink {
    tx: Arc<watch::Sender<Link>>,
}

impl FakeLink {
    pub fn new(link: Link) -> FakeLink {
        FakeLink {
            tx: Arc::new(watch::Sender::new(link)),
        }
    }

    /// Muda o estado (quem acompanha pelo `watch` vê a troca).
    pub fn set(&self, link: Link) {
        self.tx.send_replace(link);
    }
}

impl Connectivity for FakeLink {
    fn watch(&self) -> watch::Receiver<Link> {
        self.tx.subscribe()
    }
}

/// A trava da sonda do D-Bus (`tests/sonda.rs`): só passa com
/// `LUCERNA_ISOLADO=1` **e** dentro de um container (`/.dockerenv` ou
/// `/run/.containerenv`), que é onde o `tests/isolado.sh` sobe um D-Bus do
/// sistema de mentira. Fora disso, entra em pânico: a sonda nunca fala com o
/// NetworkManager da máquina.
pub fn isolated() {
    let flag = std::env::var_os("LUCERNA_ISOLADO").is_some_and(|v| v == "1");
    let container = Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists();
    assert!(
        flag && container,
        "sonda.rs: só roda pelo tests/isolado.sh, num container"
    );
}
