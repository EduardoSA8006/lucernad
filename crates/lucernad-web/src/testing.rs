//! Ajudantes para os testes (feature `testing`): o servidor HTTP local
//! ([`LocalServer`], só em 127.0.0.1) e o [`Http`] falso ([`FakeHttp`]).
//!
//! Nada aqui sai para a internet: o [`LocalServer`] escuta só no loopback e o
//! [`FakeHttp`] nunca abre uma conexão.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::http::{BoxFuture, Http, HttpError, Request, Response};

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

    /// Acrescenta um cabeçalho.
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
    let mut head = format!(
        "HTTP/1.1 {} Lucerna\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reply.body.len()
    );
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
    held: watch::Sender<bool>,
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
                held: watch::Sender::new(false),
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
        self.state.held.send_replace(true);
    }

    /// Solta os pedidos parados (e os próximos).
    pub fn release(&self) {
        self.state.held.send_replace(false);
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
            // O `Sender` vive no estado: o `wait_for` só termina no `release`.
            let _ = held.wait_for(|held| !*held).await;
            if self.state.panicking {
                panic!("FakeHttp::panicking: pânico no pedido");
            }
            self.answer(req)
        })
    }
}
