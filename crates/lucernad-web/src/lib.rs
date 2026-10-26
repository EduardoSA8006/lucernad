//! Módulo `web` do `lucernad`: a letra das músicas e a previsão do tempo, com
//! cache em disco.
//!
//! As peças: o [`lrc`] lê a letra (sincronizada ou de texto), o
//! [`matching`] decide se a faixa que uma fonte devolveu é a que foi pedida,
//! o [`http`] faz os pedidos (com prazo, nova tentativa e `Retry-After`) e o
//! [`endpoints`] diz para onde eles vão; o [`cache`] guarda as respostas em
//! disco. As fontes: o [`lyrics`] (a LRCLIB e o Vagalume) e o [`weather`]
//! (o Open-Meteo). O [`net`] diz se há internet (pelo NetworkManager).
//!
//! O módulo se registra num servidor com [`register`] e roda com
//! [`Web::spawn`]: um laço só, que guarda a configuração, a faixa e a
//! conectividade e dispara as buscas (cada uma numa tarefa própria,
//! abortável). Até o primeiro `web.configure`, o módulo fica offline: o
//! daemon sobe antes de o shell dizer se o modo offline está ligado. O
//! `web.searchCity` roda fora do laço.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use lucerna_paths::{AppPaths, Env};
use lucerna_service::{RpcError, ServerBuilder, Topic};
use lucernad_core::{ModuleTask, supervise};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};

use crate::cache::{CACHE_LIMIT, Cache};
use crate::endpoints::{Endpoints, base_from_env};
use crate::http::{
    BoxFuture, Http, HttpError, Network, REQUEST_TIMEOUT, Request, ReqwestHttp, Response,
    RetryPolicy,
};
use crate::matching::Track;
use crate::net::{Connectivity, NmConnectivity};
use crate::run::{Reach, Settings};
use crate::weather::{HttpOrFormat, Location, search_cities};

pub mod cache;
pub mod endpoints;
pub mod http;
pub mod lrc;
pub mod lyrics;
pub mod matching;
pub mod net;
mod run;
pub mod weather;

#[cfg(feature = "testing")]
pub mod testing;

/// Quanto a faixa precisa ficar parada antes do primeiro pedido da letra.
pub const TRACK_SETTLE: Duration = Duration::from_millis(500);
/// A espera antes de tentar o clima de novo depois de um erro (ou o
/// `refresh`, se menor).
pub const WEATHER_RETRY: Duration = Duration::from_secs(5 * 60);
/// O intervalo padrão do clima, em minutos.
pub const DEFAULT_REFRESH: u32 = 30;
/// O maior artista, título ou álbum, em caracteres.
pub const TEXT_MAX: usize = 500;
/// A maior busca de cidade, em caracteres.
pub const QUERY_MAX: usize = 100;
/// O maior nome de cidade, em caracteres.
pub const NAME_MAX: usize = 200;
/// A maior chave do Vagalume, em caracteres.
pub const KEY_MAX: usize = 128;

/// O texto do estado quando o laço do módulo entra em pânico.
const PANIC_ERROR: &str = "O módulo de rede parou por um erro interno.";
/// Modo offline (ou antes do primeiro `configure`): a busca de cidade não sai.
const OFFLINE: i32 = 1001;
/// O NetworkManager diz que não há conexão.
const NO_LINK: i32 = 1002;
/// O Open-Meteo falhou na busca de cidade.
const SEARCH_FAILED: i32 = 1003;
/// O módulo está indisponível.
const UNAVAILABLE: i32 = 1004;

/// Com o que o módulo fala e quanto espera.
pub struct WebConfig {
    pub http: Arc<dyn Http>,
    pub endpoints: Endpoints,
    /// O `LUCERNAD_WEB_BASE` inválido (ou o cliente HTTP que não subiu): o
    /// módulo fica indisponível com este texto e nada roda.
    pub base_error: Option<String>,
    /// `None`: sem pasta de cache, tudo vai à rede (com assinante).
    pub cache: Option<Cache>,
    pub link: Arc<dyn Connectivity>,
    /// A nova tentativa das buscas de fundo (a letra e o clima).
    pub policy: RetryPolicy,
    /// A nova tentativa da busca de cidade (interativa).
    pub search_policy: RetryPolicy,
    pub settle: Duration,
    pub weather_retry: Duration,
    /// Quanto vale um minuto do `refresh` (60 s; encurtado nos testes).
    pub minute: Duration,
}

/// Um [`Http`] que nunca conecta: o lugar do cliente que não subiu (com o
/// módulo indisponível, ninguém chama).
struct NoHttp;

impl Http for NoHttp {
    fn get<'a>(&'a self, _req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        Box::pin(async { Err(HttpError::Connect) })
    }
}

impl WebConfig {
    /// As fontes de verdade, ou o servidor local do `LUCERNAD_WEB_BASE` (com
    /// o cache numa subpasta própria, `teste/`). Uma base inválida deixa o
    /// módulo indisponível: nunca volta à rede de verdade com a variável
    /// definida.
    pub fn system() -> WebConfig {
        let mut base_error = None;
        let (endpoints, network, test) = match base_from_env(
            std::env::var_os("LUCERNAD_WEB_BASE").as_deref(),
        ) {
            Ok(None) => (Endpoints::real(), Network::Internet, false),
            Ok(Some(base)) => {
                eprintln!(
                    "lucernad: LUCERNAD_WEB_BASE={base}: a rede é de teste (servidor local, nada vai à internet)"
                );
                (Endpoints::under(&base), Network::Loopback, true)
            }
            Err(e) => {
                eprintln!("lucernad: {e}");
                base_error = Some(e);
                // Nada roda; nem assim aponta para a rede de verdade.
                (
                    Endpoints::under("http://127.0.0.1:1"),
                    Network::Loopback,
                    true,
                )
            }
        };
        let http: Arc<dyn Http> = match ReqwestHttp::new(REQUEST_TIMEOUT, network) {
            Ok(http) => Arc::new(http),
            Err(e) => {
                let error = format!(
                    "Não consegui iniciar o cliente HTTP: {}.",
                    e.trim_end_matches('.')
                );
                eprintln!("lucernad: {error}");
                base_error.get_or_insert(error);
                Arc::new(NoHttp)
            }
        };
        let cache = match AppPaths::new("web", &Env::from_process()).and_then(|p| p.cache_dir()) {
            Ok(dir) => {
                let dir = if test { dir.join("teste") } else { dir };
                Some(Cache::new(dir, CACHE_LIMIT))
            }
            Err(e) => {
                eprintln!("lucernad: sem pasta de cache ({e}): a letra e o clima ficam sem cache");
                None
            }
        };
        WebConfig {
            http,
            endpoints,
            base_error,
            cache,
            link: Arc::new(NmConnectivity::system()),
            policy: RetryPolicy::standard(),
            search_policy: RetryPolicy::short(),
            settle: TRACK_SETTLE,
            weather_retry: WEATHER_RETRY,
            minute: Duration::from_secs(60),
        }
    }
}

/// Os tópicos do módulo.
#[derive(Clone)]
pub struct WebTopics {
    /// O comum `web.lyrics`: a letra da faixa atual; `null` até o primeiro
    /// `web.track` com assinante.
    pub lyrics: Topic,
    /// O comum `web.weather`: a previsão da cidade; `null` até o primeiro
    /// `web.configure`.
    pub weather: Topic,
    /// O comum `web.status`: `{available, error, online}`.
    pub status: Topic,
}

/// O módulo registrado, pronto para rodar.
pub struct Web {
    pub topics: WebTopics,
    parts: Option<run::Parts>,
}

/// Um pedido dos métodos ao laço; a resposta volta pelo `oneshot`.
pub(crate) enum Ask {
    Track(Track, oneshot::Sender<()>),
    Configure(Settings, oneshot::Sender<()>),
    Refresh(oneshot::Sender<()>),
}

/// O que os métodos usam.
#[derive(Clone)]
struct Ctx {
    status: Topic,
    /// `None` com a base inválida.
    requests: Option<mpsc::UnboundedSender<Ask>>,
    /// Se a busca de cidade pode sair (o laço atualiza).
    reach: watch::Receiver<Reach>,
    http: Arc<dyn Http>,
    endpoints: Endpoints,
    search_policy: RetryPolicy,
}

impl Ctx {
    /// O erro `1004` com o texto do estado.
    fn unavailable(&self) -> RpcError {
        let error = self.status.current().data["error"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| PANIC_ERROR.to_string());
        RpcError::app(
            UNAVAILABLE,
            format!(
                "O módulo de rede está indisponível: {}.",
                error.trim_end_matches('.')
            ),
        )
    }

    fn available(&self) -> bool {
        self.status.current().data["available"] != false
    }

    /// Manda um pedido ao laço e espera a resposta.
    async fn ask(&self, make: impl FnOnce(oneshot::Sender<()>) -> Ask) -> Result<(), RpcError> {
        let requests = self.requests.as_ref().ok_or_else(|| self.unavailable())?;
        if !self.available() {
            return Err(self.unavailable());
        }
        let (tx, rx) = oneshot::channel();
        requests.send(make(tx)).map_err(|_| self.unavailable())?;
        rx.await.map_err(|_| self.unavailable())
    }

    async fn track(self, p: Value) -> Result<Value, RpcError> {
        let artist = text_field(&p, "artist")
            .ok_or_else(|| bad("artist precisa ser um texto com até 500 caracteres"))?;
        let title = text_field(&p, "title")
            .ok_or_else(|| bad("title precisa ser um texto com até 500 caracteres"))?;
        let album = match field(&p, "album") {
            None => String::new(),
            Some(_) => text_field(&p, "album")
                .ok_or_else(|| bad("album precisa ser um texto com até 500 caracteres, ou null"))?,
        };
        let duration = match field(&p, "duration") {
            None => None,
            Some(v) => Some(
                v.as_f64()
                    .filter(|d| (0.0..=86_400.0).contains(d))
                    .ok_or_else(|| {
                        bad("duration precisa ser um número entre 0 e 86400 (segundos), ou null")
                    })?,
            ),
        };
        let t = Track {
            artist,
            title,
            album,
            duration,
        };
        self.ask(|reply| Ask::Track(t, reply)).await?;
        Ok(json!({}))
    }

    async fn configure(self, p: Value) -> Result<Value, RpcError> {
        let offline = field(&p, "offline")
            .and_then(Value::as_bool)
            .ok_or_else(|| bad("offline precisa ser verdadeiro ou falso"))?;
        let key = match field(&p, "vagalumeKey") {
            None => None,
            Some(v) => Some(v.as_str().filter(|k| valid_key(k)).map(str::to_string).ok_or_else(
                || bad("vagalumeKey precisa ser um texto de 1 a 128 letras, números, - ou _, ou null"),
            )?),
        };
        let location = match field(&p, "location") {
            None => None,
            Some(v) => Some(location(v).ok_or_else(|| {
                bad("location precisa ser {name, latitude, longitude} (nome com até 200 caracteres, latitude entre -90 e 90, longitude entre -180 e 180), ou null")
            })?),
        };
        let refresh = match field(&p, "refresh") {
            None => DEFAULT_REFRESH,
            Some(v) => v
                .as_u64()
                .filter(|r| (5..=1440).contains(r))
                .and_then(|r| u32::try_from(r).ok())
                .ok_or_else(|| {
                    bad("refresh precisa ser um inteiro de 5 a 1440 (minutos), ou null")
                })?,
        };
        let settings = Settings {
            offline,
            key,
            location,
            refresh,
        };
        self.ask(|reply| Ask::Configure(settings, reply)).await?;
        Ok(json!({}))
    }

    async fn refresh(self, _p: Value) -> Result<Value, RpcError> {
        self.ask(Ask::Refresh).await?;
        Ok(json!({}))
    }

    async fn search_city(self, p: Value) -> Result<Value, RpcError> {
        let query = field(&p, "query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|q| !q.is_empty() && q.chars().count() <= QUERY_MAX)
            .map(str::to_string)
            .ok_or_else(|| bad("query precisa ser um texto de 1 a 100 caracteres"))?;
        if self.requests.is_none() || !self.available() {
            return Err(self.unavailable());
        }
        let reach = *self.reach.borrow();
        match reach {
            Reach::Off => {
                return Err(RpcError::app(
                    OFFLINE,
                    "Modo offline ligado: a busca de cidade usa a internet.",
                ));
            }
            Reach::NoLink => {
                return Err(RpcError::app(NO_LINK, "Sem conexão com a internet."));
            }
            Reach::Online => {}
        }
        let failed = |e: String| {
            RpcError::app(
                SEARCH_FAILED,
                format!("Não consegui falar com o Open-Meteo: {e}."),
            )
        };
        match search_cities(&*self.http, &self.endpoints, &self.search_policy, &query).await {
            Ok(cities) => {
                let results: Vec<Value> = cities.iter().map(|c| c.to_json()).collect();
                Ok(json!({ "results": results }))
            }
            Err(HttpOrFormat::Http(e)) => Err(failed(e.message())),
            Err(HttpOrFormat::Format) => Err(failed("a resposta veio fora do formato".into())),
        }
    }
}

/// O campo `key` dos parâmetros; `null` conta como ausente.
fn field<'a>(p: &'a Value, key: &str) -> Option<&'a Value> {
    p.get(key).filter(|v| !v.is_null())
}

/// Um texto de até [`TEXT_MAX`] caracteres.
fn text_field(p: &Value, key: &str) -> Option<String> {
    field(p, key)
        .and_then(Value::as_str)
        .filter(|s| s.chars().count() <= TEXT_MAX)
        .map(str::to_string)
}

fn bad(text: &str) -> RpcError {
    RpcError::invalid_params(text)
}

/// De 1 a [`KEY_MAX`] letras ASCII, números, `-` ou `_`.
fn valid_key(k: &str) -> bool {
    (1..=KEY_MAX).contains(&k.len())
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// `{name, latitude, longitude}` dentro dos limites.
fn location(v: &Value) -> Option<Location> {
    let name = v
        .get("name")?
        .as_str()
        .filter(|n| n.chars().count() <= NAME_MAX)?;
    let latitude = v
        .get("latitude")?
        .as_f64()
        .filter(|l| (-90.0..=90.0).contains(l))?;
    let longitude = v
        .get("longitude")?
        .as_f64()
        .filter(|l| (-180.0..=180.0).contains(l))?;
    Some(Location {
        name: name.to_string(),
        latitude,
        longitude,
    })
}

/// Declara um método que recebe o [`Ctx`] e os parâmetros (`null` sem eles).
fn method<F, Fut>(server: &mut ServerBuilder, name: &str, ctx: &Ctx, f: F)
where
    F: Fn(Ctx, Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    let ctx = ctx.clone();
    server.method(name, move |params| {
        f(ctx.clone(), params.unwrap_or(Value::Null))
    });
}

/// Declara os tópicos `web.lyrics`, `web.weather` e `web.status` e os
/// métodos `web.track`, `web.configure`, `web.searchCity` e `web.refresh`.
pub fn register(server: &mut ServerBuilder, cfg: WebConfig) -> Web {
    let status = match &cfg.base_error {
        Some(e) => json!({ "available": false, "error": e, "online": false }),
        None => json!({ "available": true, "error": null, "online": false }),
    };
    let topics = WebTopics {
        lyrics: server.topic("web.lyrics", Value::Null),
        weather: server.topic("web.weather", Value::Null),
        status: server.topic("web.status", status),
    };
    let (reach_tx, reach) = watch::channel(Reach::Off);
    let (parts, requests) = if cfg.base_error.is_some() {
        (None, None)
    } else {
        let (tx, rx) = mpsc::unbounded_channel();
        let parts = run::Parts {
            http: cfg.http.clone(),
            endpoints: cfg.endpoints.clone(),
            cache: cfg.cache,
            link: cfg.link,
            policy: cfg.policy,
            settle: cfg.settle,
            weather_retry: cfg.weather_retry,
            minute: cfg.minute,
            requests: rx,
            reach: reach_tx,
        };
        (Some(parts), Some(tx))
    };
    let ctx = Ctx {
        status: topics.status.clone(),
        requests,
        reach,
        http: cfg.http,
        endpoints: cfg.endpoints,
        search_policy: cfg.search_policy,
    };
    method(server, "web.track", &ctx, Ctx::track);
    method(server, "web.configure", &ctx, Ctx::configure);
    method(server, "web.searchCity", &ctx, Ctx::search_city);
    method(server, "web.refresh", &ctx, Ctx::refresh);
    Web { topics, parts }
}

impl Web {
    /// Roda o laço do módulo e o supervisor (chame dentro do runtime). Se o
    /// laço entrar em pânico (também o pânico de uma busca, relançado nele),
    /// o `web.status` fica indisponível; o servidor continua atendendo. Com a
    /// base inválida, nada roda.
    pub fn spawn(self) -> ModuleTask {
        let status = self.topics.status.clone();
        let run = match self.parts {
            None => tokio::spawn(async {}),
            Some(parts) => tokio::spawn(run::run(parts, self.topics)),
        };
        supervise(
            run,
            status,
            "de rede",
            json!({ "available": false, "error": PANIC_ERROR, "online": false }),
        )
    }
}
