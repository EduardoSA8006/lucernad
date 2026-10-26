//! O laço do módulo `web`: guarda a configuração, a faixa, a conectividade e
//! a previsão da cidade, e dispara as buscas.
//!
//! Cada busca (a letra, o clima) roda numa tarefa própria, abortável, com a
//! chave de quem pediu; o laço espera cada uma e só publica se a chave ainda
//! é a da faixa (ou da cidade) atual e se o módulo ainda está online. O
//! pânico de uma busca é relançado no laço, para o `supervise` ver. Toda E/S
//! do cache vai por `spawn_blocking`.

use std::future::pending;
use std::panic::resume_unwind;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lucernad_core::publish_changed;
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinError, JoinHandle};
use tokio::time::Instant;

use crate::cache::{Cache, Kind};
use crate::endpoints::Endpoints;
use crate::http::{Http, RetryPolicy};
use crate::lrc::lines_json;
use crate::lyrics::{self, Found, Lyrics};
use crate::matching::Track;
use crate::net::{Connectivity, Link};
use crate::weather::{self, Forecast, Location};
use crate::{Ask, WebTopics};

const OFFLINE_LYRICS: &str = "Modo offline: a letra não é buscada.";
const OFFLINE_WEATHER: &str = "Modo offline: o clima não é atualizado.";
const NO_LINK: &str = "Sem conexão com a internet.";

/// O que o `web.configure` manda (substitui tudo).
pub(crate) struct Settings {
    pub offline: bool,
    pub key: Option<String>,
    pub location: Option<Location>,
    /// O intervalo do clima, em minutos.
    pub refresh: u32,
}

/// Se a busca de cidade pode sair (o laço atualiza; o método lê).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reach {
    /// Modo offline, ou antes do primeiro `configure`.
    Off,
    /// O NetworkManager diz que não há conexão.
    NoLink,
    Online,
}

/// O que o laço leva (nada com a base inválida).
pub(crate) struct Parts {
    pub http: Arc<dyn Http>,
    pub endpoints: Endpoints,
    pub cache: Option<Cache>,
    pub link: Arc<dyn Connectivity>,
    pub policy: RetryPolicy,
    pub settle: Duration,
    pub weather_retry: Duration,
    pub minute: Duration,
    pub requests: mpsc::UnboundedReceiver<Ask>,
    pub reach: watch::Sender<Reach>,
}

/// Uma busca em voo: a chave de quem pediu e a tarefa. Largar aborta a
/// tarefa: ao trocar de busca, ao sair do laço e no pânico dele, nenhuma
/// busca fica órfã.
struct Flight<T> {
    key: String,
    handle: JoinHandle<T>,
}

impl<T> Drop for Flight<T> {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

type LyricsOutcome = Result<Found, String>;
type WeatherOutcome = Result<(Forecast, SystemTime), String>;

/// O que acordou o laço.
enum Event {
    Ask(Option<Ask>),
    Link(bool),
    LyricsSubscribers,
    WeatherSubscribers,
    LyricsDone(String, Result<LyricsOutcome, JoinError>),
    WeatherDone(String, Result<WeatherOutcome, JoinError>),
    WeatherDue,
}

struct Loop {
    http: Arc<dyn Http>,
    endpoints: Endpoints,
    cache: Option<Cache>,
    policy: RetryPolicy,
    settle: Duration,
    weather_retry: Duration,
    minute: Duration,
    topics: WebTopics,
    requests: mpsc::UnboundedReceiver<Ask>,
    reach: watch::Sender<Reach>,
    link_rx: watch::Receiver<Link>,
    link: Link,
    /// `None` até o primeiro `configure` (offline).
    settings: Option<Settings>,
    track: Option<Track>,
    lyrics_sub: bool,
    weather_sub: bool,
    lyrics: Option<Flight<LyricsOutcome>>,
    weather: Option<Flight<WeatherOutcome>>,
    /// A previsão guardada da cidade atual, com quando foi buscada.
    saved: Option<(SystemTime, Forecast)>,
    /// A próxima tentativa do clima depois de um erro.
    retry_at: Option<Instant>,
    /// Quando buscar o clima de novo (com assinante).
    due: Option<Instant>,
}

/// O laço do módulo; termina quando o servidor larga os métodos.
pub(crate) async fn run(parts: Parts, topics: WebTopics) {
    let mut link_rx = parts.link.watch();
    let link = *link_rx.borrow_and_update();
    let mut l = Loop {
        http: parts.http,
        endpoints: parts.endpoints,
        cache: parts.cache,
        policy: parts.policy,
        settle: parts.settle,
        weather_retry: parts.weather_retry,
        minute: parts.minute,
        topics,
        requests: parts.requests,
        reach: parts.reach,
        link_rx,
        link,
        settings: None,
        track: None,
        lyrics_sub: false,
        weather_sub: false,
        lyrics: None,
        weather: None,
        saved: None,
        retry_at: None,
        due: None,
    };
    l.publish_status();
    let mut link_alive = true;
    loop {
        let event = tokio::select! {
            ask = l.requests.recv() => Event::Ask(ask),
            changed = l.link_rx.changed(), if link_alive => Event::Link(changed.is_ok()),
            () = flip(&l.topics.lyrics, l.lyrics_sub) => Event::LyricsSubscribers,
            () = flip(&l.topics.weather, l.weather_sub) => Event::WeatherSubscribers,
            (key, r) = joined(&mut l.lyrics) => Event::LyricsDone(key, r),
            (key, r) = joined(&mut l.weather) => Event::WeatherDone(key, r),
            () = until(l.due) => Event::WeatherDue,
        };
        match event {
            Event::Ask(None) => return,
            Event::Ask(Some(ask)) => {
                // Uma troca da conectividade que chegou antes do pedido vale
                // antes dele.
                if l.link_rx.has_changed().unwrap_or(false) {
                    l.on_link().await;
                }
                l.on_ask(ask).await;
            }
            Event::Link(true) => l.on_link().await,
            Event::Link(false) => link_alive = false,
            Event::LyricsSubscribers => l.on_lyrics_subscribers().await,
            Event::WeatherSubscribers => l.on_weather_subscribers().await,
            Event::LyricsDone(key, r) => l.on_lyrics_done(key, r),
            Event::WeatherDone(key, r) => l.on_weather_done(key, r).await,
            Event::WeatherDue => {
                l.due = None;
                l.weather_check(false).await;
            }
        }
    }
}

/// Volta quando o tópico ganha o primeiro assinante (`subscribed` falso) ou
/// perde o último (`subscribed` verdadeiro).
async fn flip(topic: &lucerna_service::Topic, subscribed: bool) {
    if subscribed {
        topic.wait_unsubscribed().await;
    } else {
        topic.wait_subscribed().await;
    }
}

/// Espera a busca em voo terminar e a tira do lugar; sem busca, nunca volta.
async fn joined<T>(slot: &mut Option<Flight<T>>) -> (String, Result<T, JoinError>) {
    let Some(flight) = slot.as_mut() else {
        return pending().await;
    };
    let r = (&mut flight.handle).await;
    let mut flight = slot.take().expect("a busca em voo");
    (std::mem::take(&mut flight.key), r)
}

async fn until(due: Option<Instant>) {
    match due {
        Some(at) => tokio::time::sleep_until(at).await,
        None => pending().await,
    }
}

/// Relança o pânico de uma tarefa; o cancelamento (o abort) é ignorado.
fn rethrow(e: JoinError) {
    if e.is_panic() {
        resume_unwind(e.into_panic());
    }
}

/// Lê o cache (fora do runtime); sem cache, `None`.
async fn cache_get(cache: &Option<Cache>, kind: Kind, key: String) -> Option<(SystemTime, Value)> {
    let cache = cache.clone()?;
    match tokio::task::spawn_blocking(move || cache.get(kind, &key)).await {
        Ok(entry) => entry,
        Err(e) => {
            rethrow(e);
            None
        }
    }
}

/// Grava no cache (fora do runtime); a falha só vai para o log.
async fn cache_put(cache: Option<Cache>, kind: Kind, key: String, value: Value, now: SystemTime) {
    let Some(cache) = cache else { return };
    match tokio::task::spawn_blocking(move || cache.put(kind, &key, &value, now)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => eprintln!("lucernad: não consegui gravar o cache da rede: {e}"),
        Err(e) => rethrow(e),
    }
}

fn unix(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// O `web.lyrics` sem letra: `searching`, `none` ou `error`.
fn lyrics_empty(key: &str, state: &str, error: Option<&str>) -> Value {
    let mut v = json!({"key": key, "state": state, "synced": false, "lines": [], "source": ""});
    if let Some(e) = error {
        v["error"] = json!(e);
    }
    v
}

/// O `web.lyrics` com o resultado de uma busca (ou do cache).
fn lyrics_found(key: &str, found: &Found) -> Value {
    match found {
        Found::None { .. } => lyrics_empty(key, "none", None),
        Found::Lyrics(Lyrics {
            synced,
            lines,
            source,
            url,
        }) => {
            let mut v = json!({
                "key": key,
                "state": "found",
                "synced": synced,
                "lines": lines_json(lines),
                "source": source.as_str(),
            });
            if let Some(url) = url {
                v["url"] = json!(url);
            }
            v
        }
    }
}

fn location_json(l: &Location) -> Value {
    json!({"name": l.name, "latitude": l.latitude, "longitude": l.longitude})
}

impl Loop {
    /// Com configuração, fora do modo offline e com conexão (ou sem saber).
    fn online(&self) -> bool {
        self.settings.as_ref().is_some_and(|s| !s.offline) && self.link != Link::Offline
    }

    /// Por que não está online: o modo offline (ou sem configuração) vem
    /// antes da falta de conexão.
    fn offline_text(&self, offline: &'static str) -> &'static str {
        if self.settings.as_ref().is_none_or(|s| s.offline) {
            offline
        } else {
            NO_LINK
        }
    }

    fn location(&self) -> Option<&Location> {
        self.settings.as_ref().and_then(|s| s.location.as_ref())
    }

    /// O intervalo do clima, no relógio do módulo.
    fn period(&self) -> Duration {
        let refresh = self
            .settings
            .as_ref()
            .map_or(crate::DEFAULT_REFRESH, |s| s.refresh);
        self.minute.saturating_mul(refresh)
    }

    fn publish_status(&self) {
        let online = self.online();
        publish_changed(
            &self.topics.status,
            json!({"available": true, "error": null, "online": online}),
        );
        let reach = match &self.settings {
            None => Reach::Off,
            Some(s) if s.offline => Reach::Off,
            Some(_) if self.link == Link::Offline => Reach::NoLink,
            Some(_) => Reach::Online,
        };
        self.reach.send_replace(reach);
    }

    async fn on_ask(&mut self, ask: Ask) {
        match ask {
            Ask::Track(t, reply) => {
                self.on_track(t).await;
                let _ = reply.send(());
            }
            Ask::Configure(settings, reply) => {
                self.on_configure(settings).await;
                let _ = reply.send(());
            }
            Ask::Refresh(reply) => {
                if self.lyrics_sub && self.lyrics_state().as_deref() == Some("error") {
                    self.lyrics_routine().await;
                }
                self.weather_check(true).await;
                let _ = reply.send(());
            }
        }
    }

    async fn on_link(&mut self) {
        let was = self.online();
        self.link = *self.link_rx.borrow_and_update();
        self.transition(was).await;
        self.weather_check(false).await;
    }

    /// Depois de mudar a configuração ou a conectividade: o estado, e as
    /// buscas quando o online mudou (sair aborta a letra em voo; voltar
    /// refaz a letra com erro e zera a espera do clima).
    async fn transition(&mut self, was: bool) {
        self.publish_status();
        let now = self.online();
        if was == now {
            return;
        }
        if now {
            self.retry_at = None;
            if self.lyrics_sub && self.lyrics_state().as_deref() == Some("error") {
                self.lyrics_routine().await;
            }
        } else if self.lyrics.is_some() {
            self.lyrics_routine().await;
        }
    }

    async fn on_configure(&mut self, new: Settings) {
        let was = self.online();
        self.link = *self.link_rx.borrow_and_update();
        let old = self.settings.replace(new);
        let new = self.settings.as_ref().expect("acabou de entrar");
        let key_changed = old.as_ref().and_then(|s| s.key.as_deref()) != new.key.as_deref();
        let new_location = new.location.clone();
        match (old.and_then(|s| s.location), new_location) {
            (_, None) => {
                self.abort_weather();
                self.saved = None;
                self.retry_at = None;
                self.due = None;
                publish_changed(
                    &self.topics.weather,
                    json!({"location": null, "state": "none", "updatedAt": null,
                           "current": null, "daily": [], "sunrise": null, "sunset": null}),
                );
            }
            (Some(old), Some(new)) if old.place_key() == new.place_key() => {
                if old.name != new.name {
                    let mut v = self.topics.weather.current().data;
                    if v.is_object() {
                        v["location"] = location_json(&new);
                        publish_changed(&self.topics.weather, v);
                    }
                }
            }
            (_, Some(new)) => {
                self.abort_weather();
                self.retry_at = None;
                self.due = None;
                self.saved = cache_get(&self.cache, Kind::Weather, new.place_key())
                    .await
                    .and_then(|(t, v)| Some((t, Forecast::from_json(&v)?)));
                let state = if self.saved.is_some() {
                    "ready"
                } else {
                    "loading"
                };
                self.publish_weather(state, None);
            }
        }
        self.transition(was).await;
        if self.lyrics_sub && self.track.is_some() {
            let state = self.lyrics_state();
            let state = state.as_deref();
            // Com a chave nova, o "sem letra" (e a busca em voo, que ainda
            // usa a velha) é refeito.
            let stale = matches!(state, Some("none" | "searching"));
            if state == Some("error") || (key_changed && stale) {
                self.lyrics_routine().await;
            }
        }
        self.weather_check(false).await;
    }

    // ----- a letra -----

    /// O `state` do `web.lyrics`, se ele é da faixa atual.
    fn lyrics_state(&self) -> Option<String> {
        let key = self.track.as_ref()?.key();
        let v = self.topics.lyrics.current().data;
        (v["key"] == key.as_str())
            .then(|| v["state"].as_str().map(str::to_string))
            .flatten()
    }

    async fn on_track(&mut self, t: Track) {
        let same = self.track.as_ref() == Some(&t);
        let key = t.key();
        self.track = Some(t);
        if !self.lyrics_sub {
            return;
        }
        let shown = self.topics.lyrics.current().data["key"] == key.as_str();
        if same && (self.lyrics.is_some() || shown) {
            return;
        }
        self.lyrics_routine().await;
    }

    async fn on_lyrics_subscribers(&mut self) {
        self.lyrics_sub = self.topics.lyrics.subscribers() > 0;
        if self.lyrics_sub {
            self.lyrics_routine().await;
        } else {
            self.abort_lyrics();
        }
    }

    fn abort_lyrics(&mut self) {
        // O `Drop` do `Flight` aborta.
        self.lyrics = None;
    }

    /// A letra da faixa atual: aborta a busca em voo; o cache responde na
    /// hora (nunca passa por `searching`); offline, o erro; senão
    /// `searching` e a busca depois do `settle`.
    async fn lyrics_routine(&mut self) {
        self.abort_lyrics();
        let Some(t) = self.track.clone() else { return };
        let key = t.key();
        if t.title.trim().is_empty() || t.artist.trim().is_empty() {
            publish_changed(&self.topics.lyrics, lyrics_empty(&key, "none", None));
            return;
        }
        let vagalume = self.settings.as_ref().and_then(|s| s.key.clone());
        if let Some((saved, v)) = cache_get(&self.cache, Kind::Lyrics, key.clone()).await
            && let Some(found) = Found::from_json(&v)
            && lyrics::fresh(&found, saved, SystemTime::now(), vagalume.is_some())
        {
            publish_changed(&self.topics.lyrics, lyrics_found(&key, &found));
            return;
        }
        if !self.online() {
            let error = self.offline_text(OFFLINE_LYRICS);
            publish_changed(
                &self.topics.lyrics,
                lyrics_empty(&key, "error", Some(error)),
            );
            return;
        }
        publish_changed(&self.topics.lyrics, lyrics_empty(&key, "searching", None));
        let (http, ep, policy, settle) = (
            self.http.clone(),
            self.endpoints.clone(),
            self.policy,
            self.settle,
        );
        let (cache, cache_key) = (self.cache.clone(), key.clone());
        let handle = tokio::spawn(async move {
            tokio::time::sleep(settle).await;
            let found = lyrics::find(&*http, &ep, &policy, &t, vagalume.as_deref()).await?;
            cache_put(
                cache,
                Kind::Lyrics,
                cache_key,
                found.to_json(),
                SystemTime::now(),
            )
            .await;
            Ok(found)
        });
        self.lyrics = Some(Flight { key, handle });
    }

    fn on_lyrics_done(&mut self, key: String, r: Result<LyricsOutcome, JoinError>) {
        let r = match r {
            Ok(r) => r,
            Err(e) => return rethrow(e),
        };
        let current = self.track.as_ref().map(Track::key);
        if current.as_deref() != Some(key.as_str()) || !self.online() || !self.lyrics_sub {
            return;
        }
        let v = match r {
            Ok(found) => lyrics_found(&key, &found),
            Err(e) => lyrics_empty(&key, "error", Some(&e)),
        };
        publish_changed(&self.topics.lyrics, v);
    }

    // ----- o clima -----

    /// O `web.weather` da cidade atual com a previsão guardada (ou sem
    /// dados).
    fn publish_weather(&self, state: &str, error: Option<&str>) {
        let Some(loc) = self.location() else { return };
        let mut v = match &self.saved {
            Some((t, f)) => {
                let mut v = f.view(SystemTime::now());
                v["updatedAt"] = json!(unix(*t));
                v
            }
            None => json!({"updatedAt": null, "current": null, "daily": [],
                           "sunrise": null, "sunset": null}),
        };
        v["location"] = location_json(loc);
        v["state"] = json!(state);
        if let Some(e) = error {
            v["error"] = json!(e);
        }
        publish_changed(&self.topics.weather, v);
    }

    fn abort_weather(&mut self) {
        // O `Drop` do `Flight` aborta.
        self.weather = None;
    }

    async fn on_weather_subscribers(&mut self) {
        self.weather_sub = self.topics.weather.subscribers() > 0;
        if self.weather_sub {
            self.weather_check(false).await;
            return;
        }
        self.due = None;
        if self.weather.is_some() {
            self.abort_weather();
            // O `loading` da busca abortada volta ao guardado.
            if self.saved.is_some() && self.online() {
                self.publish_weather("ready", None);
            }
        }
    }

    /// Decide o clima: offline, aborta e mostra o guardado (ou o erro);
    /// online, busca já (`force`, ou com assinante e sem guardado ou com ele
    /// velho) ou marca a próxima busca.
    async fn weather_check(&mut self, force: bool) {
        let Some(loc) = self.location().cloned() else {
            return;
        };
        if !self.online() {
            self.abort_weather();
            self.due = None;
            if self.saved.is_some() {
                self.publish_weather("ready", None);
            } else {
                let error = self.offline_text(OFFLINE_WEATHER);
                self.publish_weather("error", Some(error));
            }
            return;
        }
        if self.weather.is_some() {
            return;
        }
        self.due = None;
        if !force && !self.weather_sub {
            return;
        }
        let now = Instant::now();
        let due = if force {
            now
        } else if let Some(at) = self.retry_at {
            at
        } else {
            match &self.saved {
                None => now,
                Some((t, _)) => {
                    let age = SystemTime::now().duration_since(*t).unwrap_or_default();
                    now + self.period().saturating_sub(age)
                }
            }
        };
        if due <= now {
            self.start_weather(loc);
        } else {
            self.due = Some(due);
        }
    }

    /// Publica `loading` (mantendo os dados) e busca a previsão.
    fn start_weather(&mut self, loc: Location) {
        self.publish_weather("loading", None);
        let key = loc.place_key();
        let (http, ep, policy) = (self.http.clone(), self.endpoints.clone(), self.policy);
        let (cache, cache_key) = (self.cache.clone(), key.clone());
        let handle = tokio::spawn(async move {
            let f = weather::forecast(&*http, &ep, &policy, &loc).await?;
            let now = SystemTime::now();
            cache_put(cache, Kind::Weather, cache_key, f.to_json(), now).await;
            Ok((f, now))
        });
        self.weather = Some(Flight { key, handle });
    }

    async fn on_weather_done(&mut self, key: String, r: Result<WeatherOutcome, JoinError>) {
        let r = match r {
            Ok(r) => r,
            Err(e) => return rethrow(e),
        };
        let current = self.location().map(Location::place_key);
        if current.as_deref() != Some(key.as_str()) || !self.online() {
            return;
        }
        match r {
            Ok((f, t)) => {
                self.saved = Some((t, f));
                self.retry_at = None;
                self.publish_weather("ready", None);
            }
            Err(e) => {
                self.retry_at = Some(Instant::now() + self.weather_retry.min(self.period()));
                self.publish_weather("error", Some(&e));
            }
        }
        self.weather_check(false).await;
    }
}
