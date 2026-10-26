//! A letra: a LRCLIB (o `get` exato e, se faltar, a busca conferida pelo
//! [`same_track`]) e, só com a chave, o Vagalume.
//!
//! A ordem do [`find`]: o `get` da LRCLIB com `syncedLyrics` acaba ali; com
//! só `plainLyrics`, o texto fica como reserva e a busca tenta uma
//! sincronizada que case (achou → ela; não achou ou a busca falhou → a
//! reserva, sem ir ao Vagalume). Sem nada do `get`, a busca: uma sincronizada
//! que case, senão uma de texto que case; nada → o Vagalume (com a chave).
//! Erro de rede é erro, não "sem letra": o `Err` traz o texto do Formato.

use std::time::{Duration, SystemTime};

use serde_json::{Map, Value, json};

use crate::endpoints::Endpoints;
use crate::http::{Http, HttpError, Request, Response, RetryPolicy, fetch};
use crate::lrc::{Line, lines_json, parse_lrc, plain_lines};
use crate::matching::{Track, same_track};

/// Por quanto tempo um "sem letra" guardado vale.
pub const NONE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// A página do Vagalume: todo link da letra fica dentro dela.
const VAGALUME_SITE: &str = "https://www.vagalume.com.br/";

const LRCLIB_FORMAT: &str = "A LRCLIB respondeu fora do formato.";
const VAGALUME_FORMAT: &str = "O Vagalume respondeu fora do formato.";

/// De onde a letra veio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Lrclib,
    Vagalume,
}

impl Source {
    /// O nome no tópico: `"lrclib"` ou `"vagalume"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Lrclib => "lrclib",
            Source::Vagalume => "vagalume",
        }
    }

    fn parse(s: &str) -> Option<Source> {
        match s {
            "lrclib" => Some(Source::Lrclib),
            "vagalume" => Some(Source::Vagalume),
            _ => None,
        }
    }
}

/// Uma letra achada; `url` só no Vagalume (a página da letra).
#[derive(Clone, Debug, PartialEq)]
pub struct Lyrics {
    pub synced: bool,
    pub lines: Vec<Line>,
    pub source: Source,
    pub url: Option<String>,
}

/// O resultado de uma busca: a letra, ou nenhuma (`vagalume` diz se o
/// Vagalume foi consultado).
#[derive(Clone, Debug, PartialEq)]
pub enum Found {
    Lyrics(Lyrics),
    None { vagalume: bool },
}

impl Found {
    /// O formato do cache.
    pub fn to_json(&self) -> Value {
        match self {
            Found::Lyrics(l) => {
                let mut v = json!({
                    "state": "found",
                    "synced": l.synced,
                    "lines": lines_json(&l.lines),
                    "source": l.source.as_str(),
                });
                if let Some(url) = &l.url {
                    v["url"] = json!(url);
                }
                v
            }
            Found::None { vagalume } => json!({"state": "none", "vagalume": vagalume}),
        }
    }

    /// Lê o formato do cache; qualquer coisa fora dele → `None`.
    pub fn from_json(v: &Value) -> Option<Found> {
        match v.get("state")?.as_str()? {
            "none" => Some(Found::None {
                vagalume: v.get("vagalume")?.as_bool()?,
            }),
            "found" => {
                let lines = v
                    .get("lines")?
                    .as_array()?
                    .iter()
                    .map(|l| {
                        let time = match l.get("time")? {
                            Value::Null => None,
                            t => Some(t.as_f64().filter(|t| t.is_finite() && *t >= 0.0)?),
                        };
                        let text = l.get("text")?.as_str()?.to_string();
                        Some(Line { time, text })
                    })
                    .collect::<Option<Vec<_>>>()?;
                let url = match v.get("url") {
                    None | Some(Value::Null) => None,
                    Some(u) => Some(u.as_str()?.to_string()),
                };
                Some(Found::Lyrics(Lyrics {
                    synced: v.get("synced")?.as_bool()?,
                    lines,
                    source: Source::parse(v.get("source")?.as_str()?)?,
                    url,
                }))
            }
            _ => None,
        }
    }
}

/// O guardado ainda vale? Uma letra, sempre; "nenhuma", até [`NONE_TTL`] e
/// desde que não haja chave nova do Vagalume que ele ainda não consultou.
pub fn fresh(found: &Found, saved: SystemTime, now: SystemTime, has_key: bool) -> bool {
    match found {
        Found::Lyrics(_) => true,
        Found::None { vagalume } => {
            // O relógio que voltou conta como agora.
            let age = now.duration_since(saved).unwrap_or(Duration::ZERO);
            age < NONE_TTL && (!has_key || *vagalume)
        }
    }
}

/// O `get` da LRCLIB: título e artista; álbum e duração (arredondada) só se
/// a faixa tiver.
pub fn lrclib_get(ep: &Endpoints, t: &Track) -> Request {
    let mut req = Request::new(format!("{}/api/get", ep.lrclib))
        .param("track_name", t.title.as_str())
        .param("artist_name", t.artist.as_str());
    if !t.album.is_empty() {
        req = req.param("album_name", t.album.as_str());
    }
    if let Some(d) = t.duration.filter(|d| d.is_finite() && *d >= 0.0) {
        req = req.param("duration", format!("{}", d.round() as u64));
    }
    req
}

/// A busca da LRCLIB: título e artista.
pub fn lrclib_search(ep: &Endpoints, t: &Track) -> Request {
    Request::new(format!("{}/api/search", ep.lrclib))
        .param("track_name", t.title.as_str())
        .param("artist_name", t.artist.as_str())
}

/// A busca do Vagalume, com a chave (o `Debug` do pedido a esconde).
pub fn vagalume(ep: &Endpoints, t: &Track, key: &str) -> Request {
    Request::new(format!("{}/search.php", ep.vagalume))
        .param("art", t.artist.as_str())
        .param("mus", t.title.as_str())
        .param("apikey", key)
}

/// Busca a letra na ordem do módulo (ver o topo).
pub async fn find(
    http: &dyn Http,
    ep: &Endpoints,
    policy: &RetryPolicy,
    t: &Track,
    key: Option<&str>,
) -> Result<Found, String> {
    let lrclib_err = |e: HttpError| format!("Não consegui falar com a LRCLIB: {}.", e.message());

    let resp = fetch(http, &lrclib_get(ep, t), policy)
        .await
        .map_err(lrclib_err)?;
    let mut reserve = None;
    if resp.status == 200 {
        let entry = json_body(&resp).ok_or(LRCLIB_FORMAT)?;
        let entry = entry.as_object().ok_or(LRCLIB_FORMAT)?;
        let (synced, plain) = entry_lyrics(entry);
        if let Some(lines) = synced {
            return Ok(lrclib(true, lines));
        }
        reserve = plain;
    }

    let search = match fetch(http, &lrclib_search(ep, t), policy).await {
        Ok(resp) if resp.status == 200 => match json_body(&resp) {
            Some(Value::Array(entries)) => Ok(entries),
            _ => Err(LRCLIB_FORMAT.to_string()),
        },
        // Um 4xx da busca é "nada achado".
        Ok(_) => Ok(Vec::new()),
        Err(e) => Err(lrclib_err(e)),
    };
    match (search, reserve) {
        (Ok(entries), reserve) => {
            let matching: Vec<_> = entries
                .iter()
                .filter_map(Value::as_object)
                .filter(|e| entry_matches(t, e))
                .map(entry_lyrics)
                .collect();
            if let Some(lines) = matching.iter().find_map(|(s, _)| s.clone()) {
                return Ok(lrclib(true, lines));
            }
            if let Some(lines) = reserve {
                return Ok(lrclib(false, lines));
            }
            if let Some(lines) = matching.into_iter().find_map(|(_, p)| p) {
                return Ok(lrclib(false, lines));
            }
        }
        // A busca falhou, mas o `get` já tinha a letra de texto.
        (Err(_), Some(lines)) => return Ok(lrclib(false, lines)),
        (Err(e), None) => return Err(e),
    }

    match key {
        Some(key) => from_vagalume(http, ep, policy, t, key).await,
        None => Ok(Found::None { vagalume: false }),
    }
}

fn lrclib(synced: bool, lines: Vec<Line>) -> Found {
    Found::Lyrics(Lyrics {
        synced,
        lines,
        source: Source::Lrclib,
        url: None,
    })
}

/// O corpo como JSON (o HTML de um portal cativo → `None`).
fn json_body(resp: &Response) -> Option<Value> {
    serde_json::from_slice(&resp.body).ok()
}

/// O texto de um campo, se for um texto não vazio.
fn text<'a>(entry: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    entry
        .get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

/// As linhas sincronizadas e as de texto de uma entrada da LRCLIB (vazias
/// viram `None`; a instrumental não tem letra).
fn entry_lyrics(entry: &Map<String, Value>) -> (Option<Vec<Line>>, Option<Vec<Line>>) {
    if entry.get("instrumental").and_then(Value::as_bool) == Some(true) {
        return (None, None);
    }
    let synced = text(entry, "syncedLyrics")
        .map(parse_lrc)
        .filter(|l| !l.is_empty());
    let plain = text(entry, "plainLyrics")
        .map(plain_lines)
        .filter(|l| !l.is_empty());
    (synced, plain)
}

/// A entrada da busca é a faixa pedida?
fn entry_matches(t: &Track, entry: &Map<String, Value>) -> bool {
    let (Some(title), Some(artist)) = (text(entry, "trackName"), text(entry, "artistName")) else {
        return false;
    };
    let duration = entry.get("duration").and_then(Value::as_f64);
    same_track(t, title, artist, duration)
}

/// O Vagalume: `exact`/`aprox` com a primeira música que case (título e
/// artista; ele não dá a duração) e texto não vazio; `notfound` e
/// `song_notfound` → nenhuma; o resto → fora do formato.
async fn from_vagalume(
    http: &dyn Http,
    ep: &Endpoints,
    policy: &RetryPolicy,
    t: &Track,
    key: &str,
) -> Result<Found, String> {
    let vagalume_err =
        |e: HttpError| format!("Não consegui falar com o Vagalume: {}.", e.message());
    let resp = fetch(http, &vagalume(ep, t, key), policy)
        .await
        .map_err(vagalume_err)?;
    if resp.status != 200 {
        return Err(vagalume_err(HttpError::Status(resp.status)));
    }
    let body = json_body(&resp).ok_or(VAGALUME_FORMAT)?;
    let none = Found::None { vagalume: true };
    match body.get("type").and_then(Value::as_str) {
        Some("notfound" | "song_notfound") => return Ok(none),
        Some("exact" | "aprox") => {}
        _ => return Err(VAGALUME_FORMAT.to_string()),
    }
    let artist = body
        .get("art")
        .and_then(|a| a.get("name"))
        .and_then(Value::as_str)
        .ok_or(VAGALUME_FORMAT)?;
    let song = body
        .get("mus")
        .and_then(|m| m.get(0))
        .and_then(Value::as_object)
        .ok_or(VAGALUME_FORMAT)?;
    let title = song
        .get("name")
        .and_then(Value::as_str)
        .ok_or(VAGALUME_FORMAT)?;
    if !same_track(t, title, artist, None) {
        return Ok(none);
    }
    let lines = text(song, "text").map(plain_lines).unwrap_or_default();
    if lines.is_empty() {
        return Ok(none);
    }
    let url = song.get("url").and_then(Value::as_str).unwrap_or("");
    Ok(Found::Lyrics(Lyrics {
        synced: false,
        lines,
        source: Source::Vagalume,
        url: Some(vagalume_page(url)),
    }))
}

/// O link da letra, sempre dentro do Vagalume: absoluto nele vale, começando
/// por `/` ganha o prefixo, qualquer outro vira a página inicial.
fn vagalume_page(url: &str) -> String {
    let clean = !url.chars().any(|c| c.is_control() || c.is_whitespace());
    if clean && url.starts_with(VAGALUME_SITE) {
        url.to_string()
    } else if clean && url.starts_with('/') && !url.starts_with("//") {
        format!("{}{}", VAGALUME_SITE.trim_end_matches('/'), url)
    } else {
        VAGALUME_SITE.to_string()
    }
}
