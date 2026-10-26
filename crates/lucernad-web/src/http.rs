//! O cliente HTTP: um pedido `GET` com prazo, o corpo lido em pedaços até
//! [`BODY_MAX`], e a nova tentativa ([`fetch`]) por cima, com espera
//! crescente e o `Retry-After` respeitado.
//!
//! O [`Http`] faz uma tentativa só e devolve qualquer status como resposta;
//! quem decide o que é passageiro é o [`fetch`]. Os erros nunca carregam a
//! URL: a query pode levar a chave do Vagalume.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use reqwest::{Url, redirect};

/// Um futuro em caixa, para o [`Http`] caber num `dyn`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// O prazo total de um pedido (conexão, resposta e corpo).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// O prazo para abrir a conexão.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// O maior corpo de resposta aceito (4 MiB); o resto não é lido.
pub const BODY_MAX: usize = 4 * 1024 * 1024;

/// O `User-Agent` de todo pedido (a LRCLIB pede um que diga quem é).
pub const USER_AGENT: &str = concat!(
    "lucernad/",
    env!("CARGO_PKG_VERSION"),
    " (Lucerna; https://github.com/lucerna-desktop/lucernad)"
);

/// O máximo de redirecionamentos seguidos nas fontes de verdade.
const REDIRECTS_MAX: usize = 3;

/// Um pedido `GET`: a URL sem query e os parâmetros, codificados pelo
/// cliente.
#[derive(Clone, PartialEq, Eq)]
pub struct Request {
    pub url: String,
    pub query: Vec<(String, String)>,
}

impl Request {
    /// Um pedido para `url`, sem parâmetros.
    pub fn new(url: impl Into<String>) -> Request {
        Request {
            url: url.into(),
            query: Vec::new(),
        }
    }

    /// Acrescenta o parâmetro `k=v`.
    pub fn param(mut self, k: &str, v: impl Into<String>) -> Request {
        self.query.push((k.to_string(), v.into()));
        self
    }
}

/// O `Debug` esconde o valor da `apikey`: o pedido pode ir para o log.
impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let query: Vec<String> = self
            .query
            .iter()
            .map(|(k, v)| {
                if k == "apikey" {
                    format!("{k}=***")
                } else {
                    format!("{k}={v}")
                }
            })
            .collect();
        f.debug_struct("Request")
            .field("url", &self.url)
            .field("query", &query.join("&"))
            .finish()
    }
}

/// A resposta: o status, o `Retry-After` em segundos (se veio) e o corpo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub retry_after: Option<Duration>,
    pub body: Vec<u8>,
}

/// Por que um pedido falhou; nenhum carrega a URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HttpError {
    /// O prazo acabou.
    Timeout,
    /// Não conectou, ou a conexão caiu antes da resposta.
    Connect,
    /// O servidor respondeu com este status (429 ou 5xx, esgotadas as
    /// tentativas do [`fetch`]).
    Status(u16),
    /// O corpo passou de [`BODY_MAX`].
    TooLarge,
    /// O `Retry-After` pediu mais segundos do que a política aceita.
    WaitTooLong(u64),
    /// Outro erro, já sem a URL.
    Other(String),
}

impl HttpError {
    /// O texto do erro, para entrar no lugar de `<e>` nas mensagens.
    pub fn message(&self) -> String {
        match self {
            HttpError::Timeout => "o servidor demorou demais para responder".to_string(),
            HttpError::Connect => "não consegui conectar".to_string(),
            HttpError::Status(n) => format!("o servidor respondeu {n}"),
            HttpError::TooLarge => "a resposta passou de 4 MiB".to_string(),
            HttpError::WaitTooLong(n) => format!("o servidor pediu para esperar {n} s"),
            HttpError::Other(text) => text.clone(),
        }
    }
}

/// Uma tentativa de `GET`. Qualquer status volta como `Ok`.
pub trait Http: Send + Sync + 'static {
    fn get<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>>;
}

/// Para onde os pedidos vão: as fontes de verdade, ou o servidor local de
/// teste (`LUCERNAD_WEB_BASE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Network {
    /// Só `https`, redirecionamento só para o mesmo host.
    Internet,
    /// `http` aceito, sem redirecionamento.
    Loopback,
}

/// O [`Http`] de verdade, pelo `reqwest` com `rustls` (`ring`) e os
/// certificados do sistema.
pub struct ReqwestHttp {
    client: reqwest::Client,
}

impl ReqwestHttp {
    /// Um cliente com prazo total `timeout` (e 5 s para conectar).
    pub fn new(timeout: Duration, network: Network) -> Result<ReqwestHttp, String> {
        // Já instalado (por outro cliente) não é erro.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let builder = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(timeout)
            .connect_timeout(CONNECT_TIMEOUT.min(timeout));
        let builder = match network {
            Network::Internet => {
                builder
                    .https_only(true)
                    .redirect(redirect::Policy::custom(|attempt| {
                        let hops = attempt.previous().len().saturating_sub(1);
                        let follow = attempt.previous().last().is_some_and(|from| {
                            same_host_redirect(from.as_str(), attempt.url().as_str(), hops)
                        });
                        if follow {
                            attempt.follow()
                        } else {
                            attempt.stop()
                        }
                    }))
            }
            Network::Loopback => builder.redirect(redirect::Policy::none()),
        };
        let client = builder.build().map_err(|e| describe(e.without_url()))?;
        Ok(ReqwestHttp { client })
    }

    async fn request(&self, req: &Request) -> Result<Response, HttpError> {
        let mut resp = self
            .client
            .get(&req.url)
            .query(&req.query)
            .send()
            .await
            .map_err(classify)?;
        let status = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(retry_after);
        if resp.content_length().is_some_and(|n| n > BODY_MAX as u64) {
            return Err(HttpError::TooLarge);
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(classify)? {
            if body.len() + chunk.len() > BODY_MAX {
                return Err(HttpError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Response {
            status,
            retry_after,
            body,
        })
    }
}

impl Http for ReqwestHttp {
    fn get<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        Box::pin(self.request(req))
    }
}

/// O erro do `reqwest` como [`HttpError`], sempre sem a URL. A conexão que
/// cai antes da resposta conta como [`HttpError::Connect`].
fn classify(e: reqwest::Error) -> HttpError {
    let e = e.without_url();
    if e.is_timeout() {
        HttpError::Timeout
    // No `reqwest` 0.13, a conexão que cai antes da resposta só aparece como
    // `is_request()` (o `IncompleteMessage` do hyper, sem `io::Error` por
    // baixo); distinguir exigiria o `hyper` direto, que o plano não permite.
    // Por isso todo erro de envio vira `Connect`, de propósito: também a
    // resposta malformada e o erro de protocolo (ganham nova tentativa).
    } else if e.is_connect() || e.is_request() {
        HttpError::Connect
    } else {
        HttpError::Other(describe(e))
    }
}

/// O texto do erro com as causas, sem a URL.
fn describe(e: reqwest::Error) -> String {
    let mut text = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// Se um redirecionamento de `from` para `to` pode ser seguido: o mesmo host
/// (e porta), só `https`, e `hops` (os já seguidos) abaixo de 3.
pub fn same_host_redirect(from: &str, to: &str, hops: usize) -> bool {
    if hops >= REDIRECTS_MAX {
        return false;
    }
    let (Ok(from), Ok(to)) = (Url::parse(from), Url::parse(to)) else {
        return false;
    };
    from.scheme() == "https"
        && to.scheme() == "https"
        && from.host_str().is_some()
        && from.host_str() == to.host_str()
        && from.port_or_known_default() == to.port_or_known_default()
}

/// Quantas novas tentativas e quanto esperar entre elas.
#[derive(Clone, Copy, Debug)]
pub struct RetryPolicy {
    /// As novas tentativas depois da primeira.
    pub retries: usize,
    /// A primeira espera; dobra a cada tentativa.
    pub base: Duration,
    /// O maior `Retry-After` aceito; mais que isso desiste.
    pub retry_after_max: Duration,
}

impl RetryPolicy {
    /// A das buscas de fundo: 3 novas tentativas (1, 2 e 4 s), `Retry-After`
    /// até 60 s.
    pub fn standard() -> RetryPolicy {
        RetryPolicy {
            retries: 3,
            base: Duration::from_secs(1),
            retry_after_max: Duration::from_secs(60),
        }
    }

    /// A da busca de cidade, interativa: 1 nova tentativa (1 s),
    /// `Retry-After` até 5 s.
    pub fn short() -> RetryPolicy {
        RetryPolicy {
            retries: 1,
            base: Duration::from_secs(1),
            retry_after_max: Duration::from_secs(5),
        }
    }
}

/// O pedido com nova tentativa nos erros passageiros (prazo, conexão, 429 e
/// 5xx). Os outros status voltam como resposta. Esgotadas as tentativas,
/// volta o último erro (`Status(n)` para 429/5xx).
pub async fn fetch(
    http: &dyn Http,
    req: &Request,
    policy: &RetryPolicy,
) -> Result<Response, HttpError> {
    let mut attempt = 0;
    loop {
        let backoff = policy.base.saturating_mul(1 << attempt.min(16));
        let (error, wait) = match http.get(req).await {
            Ok(resp) if resp.status == 429 || (500..600).contains(&resp.status) => {
                let asked = if resp.status == 429 || resp.status == 503 {
                    resp.retry_after
                } else {
                    None
                };
                (HttpError::Status(resp.status), asked)
            }
            Ok(resp) => return Ok(resp),
            Err(e @ (HttpError::Timeout | HttpError::Connect)) => (e, None),
            Err(e) => return Err(e),
        };
        if attempt >= policy.retries {
            return Err(error);
        }
        let wait = match wait {
            Some(asked) if asked > policy.retry_after_max => {
                return Err(HttpError::WaitTooLong(asked.as_secs()));
            }
            Some(asked) => asked,
            None => backoff,
        };
        tokio::time::sleep(wait).await;
        attempt += 1;
    }
}

/// O `Retry-After` em segundos inteiros (≥ 0); a forma de data fica `None`.
pub fn retry_after(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse::<u64>().ok().map(Duration::from_secs)
}
