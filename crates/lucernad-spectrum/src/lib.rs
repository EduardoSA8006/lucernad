//! Módulo `spectrum` do `lucernad`: o espectro do som do player exibido.
//!
//! A captura fica atrás do trait [`backend::Backend`]: o de verdade é o
//! [`pipewire::PipeWire`], que liga um fluxo de entrada a cada nó de saída do
//! player sem mexer no roteamento; nos testes, o dublê é o
//! `testing::FakeBackend` (feature `testing`). O casamento do player com os
//! nós fica no [`matching`], sobre o `/proc` e o D-Bus ([`dbus::Bus`], com o
//! dublê `testing::FakeBus`).
//!
//! O módulo se registra num servidor com [`register`] e roda com
//! [`Spectrum::spawn`]. A sessão com o PipeWire só existe enquanto houver
//! assinante de `spectrum.bands` **e** um player configurado pelo
//! `spectrum.configure`.

use std::panic::resume_unwind;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lucerna_service::{RpcError, ServerBuilder, Topic};
use lucernad_core::{Detached, ModuleTask, detached, publish_changed};
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::time::{Instant, MissedTickBehavior};

use crate::analysis::{Analyzer, BANDS, FPS};
use crate::backend::{Backend, Control, Event, OutputNode, Session};
use crate::dbus::{Bus, DBUS_TIMEOUT, Identity, SessionBus};
use crate::matching::{ProcFs, match_nodes};
use crate::mix::Rings;
use crate::pipewire::{DISCONNECTED, PipeWire};

pub mod analysis;
pub mod backend;
pub mod dbus;
pub mod matching;
pub mod mix;
pub mod pipewire;
#[cfg(feature = "testing")]
pub mod testing;

/// O prazo de cada leitura do `/proc` (o casamento dos nós).
pub const PROC_TIMEOUT: Duration = Duration::from_secs(1);
/// O prazo da sonda e da abertura de uma sessão.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(3);
/// Quanto esperar para tentar de novo depois de uma falha do PipeWire.
pub const RETRY: Duration = Duration::from_secs(5);
/// O intervalo da nova identificação do player enquanto nenhum nó casa.
pub const REIDENTIFY: Duration = Duration::from_secs(1);
/// O intervalo entre dois quadros.
pub const FRAME: Duration = Duration::from_nanos(1_000_000_000 / FPS as u64);

/// O prefixo dos nomes de player aceitos pelo `spectrum.configure`.
const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
/// O texto do estado quando o laço do módulo entra em pânico.
const PANIC_ERROR: &str = "O módulo de espectro parou por um erro interno.";

/// De onde o módulo captura e como identifica o player.
pub struct SpectrumConfig {
    pub backend: Arc<dyn Backend>,
    pub bus: Arc<dyn Bus>,
    /// O `/proc` lido pelo casamento.
    pub procfs: PathBuf,
    /// O prazo de cada identificação no D-Bus.
    pub dbus_timeout: Duration,
    /// Quanto esperar para tentar de novo depois de uma falha do PipeWire.
    pub retry: Duration,
}

impl SpectrumConfig {
    /// A máquina de verdade: o PipeWire e o D-Bus do usuário e o `/proc`.
    pub fn system() -> SpectrumConfig {
        SpectrumConfig {
            backend: Arc::new(PipeWire::system()),
            bus: Arc::new(SessionBus::new(DBUS_TIMEOUT)),
            procfs: PathBuf::from("/proc"),
            dbus_timeout: DBUS_TIMEOUT,
            retry: RETRY,
        }
    }
}

/// Os tópicos do módulo.
#[derive(Clone)]
pub struct SpectrumTopics {
    /// O contínuo `spectrum.bands`: `{"bands": [48 números de 0 a 1]}`.
    pub bands: Topic,
    /// O comum `spectrum.status`.
    pub status: Topic,
}

/// O módulo registrado, pronto para rodar.
pub struct Spectrum {
    pub topics: SpectrumTopics,
    cfg: SpectrumConfig,
    /// O player pedido pelo `spectrum.configure`; o laço guarda uma alça para
    /// o canal nunca fechar.
    player: Arc<watch::Sender<Option<String>>>,
}

/// O `player` dos parâmetros: `Some(None)` para `null`, `Some(Some(nome))`
/// para um nome MPRIS; `None` para qualquer outra coisa.
fn parse_player(params: Option<&Value>) -> Option<Option<String>> {
    match params?.get("player")? {
        Value::Null => Some(None),
        Value::String(name)
            if name.len() > MPRIS_PREFIX.len() && name.starts_with(MPRIS_PREFIX) =>
        {
            Some(Some(name.clone()))
        }
        _ => None,
    }
}

/// Declara o contínuo `spectrum.bands`, o comum `spectrum.status` e o método
/// `spectrum.configure`.
pub fn register(server: &mut ServerBuilder, cfg: SpectrumConfig) -> Spectrum {
    let topics = SpectrumTopics {
        bands: server.stream("spectrum.bands"),
        status: server.topic("spectrum.status", Status::default().to_json()),
    };
    let player = Arc::new(watch::Sender::new(None));
    let tx = player.clone();
    server.method("spectrum.configure", move |params| {
        let tx = tx.clone();
        async move {
            let player = parse_player(params.as_ref()).ok_or_else(|| {
                RpcError::invalid_params(
                    "player precisa ser um nome org.mpris.MediaPlayer2.* ou null",
                )
            })?;
            tx.send_replace(player);
            Ok(json!({}))
        }
    });
    Spectrum {
        topics,
        cfg,
        player,
    }
}

impl Spectrum {
    /// Roda a sonda, o laço do módulo e o supervisor (chame dentro do
    /// runtime). Se o laço entrar em pânico, o `spectrum.status` fica
    /// indisponível; o servidor continua atendendo.
    pub fn spawn(self) -> ModuleTask {
        let status = self.topics.status.clone();
        let run = tokio::spawn(run(self.cfg, self.topics, self.player));
        lucernad_core::supervise(
            run,
            status,
            "de espectro",
            json!({
                "available": false,
                "capturing": false,
                "player": null,
                "error": PANIC_ERROR,
            }),
        )
    }
}

/// O conteúdo do `spectrum.status`.
#[derive(Clone, Debug, Default)]
struct Status {
    /// A última conexão (ou a sonda) deu certo.
    available: bool,
    /// Sessão aberta com pelo menos um nó casado.
    capturing: bool,
    /// O player configurado.
    player: Option<String>,
    /// O último erro; `None` depois de uma conexão boa.
    error: Option<String>,
}

impl Status {
    fn to_json(&self) -> Value {
        json!({
            "available": self.available,
            "capturing": self.capturing,
            "player": self.player,
            "error": self.error,
        })
    }
}

/// O texto do prazo da abertura vencido.
fn timed_out() -> String {
    format!("O PipeWire não respondeu em {} s.", OPEN_TIMEOUT.as_secs())
}

/// O texto de uma thread que não pôde ser criada.
fn spawn_failed(e: &std::io::Error) -> String {
    format!("Não consegui criar a thread do PipeWire: {e}.")
}

/// Por que uma sessão terminou.
enum Exit {
    /// Sem assinante ou sem player: volta a esperar os dois.
    Idle,
    /// A conexão caiu: espera o `retry` e tenta de novo.
    Dropped,
}

/// O estado do laço que atravessa as sessões.
struct Module {
    cfg: SpectrumConfig,
    t: SpectrumTopics,
    player: watch::Receiver<Option<String>>,
    status: Status,
}

impl Module {
    /// Publica o `status` se ele mudou.
    fn publish_status(&self) {
        publish_changed(&self.t.status, self.status.to_json());
    }

    /// Acompanha o player configurado no `status`; devolve o atual.
    fn current_player(&mut self) -> Option<String> {
        let player = self.player.borrow_and_update().clone();
        self.status.player = player.clone();
        self.publish_status();
        player
    }

    /// Registra uma falha da conexão no `status`.
    fn failed(&mut self, error: String) {
        self.status.available = false;
        self.status.capturing = false;
        self.status.error = Some(error);
        self.publish_status();
    }

    /// Espera haver assinante de `spectrum.bands` e um player; devolve o
    /// player.
    async fn wait_for_work(&mut self) -> String {
        loop {
            let player = self.current_player();
            let subscribed = self.t.bands.subscribers() > 0;
            if let Some(name) = player
                && subscribed
            {
                return name;
            }
            tokio::select! {
                _ = self.t.bands.wait_subscribed(), if !subscribed => {}
                _ = self.player.changed() => {}
            }
        }
    }

    /// Espera o `retry`, uma troca de player ou a saída do assinante.
    async fn wait_retry(&mut self) {
        tokio::select! {
            _ = tokio::time::sleep(self.cfg.retry) => {}
            _ = self.player.changed() => {}
            _ = self.t.bands.wait_unsubscribed() => {}
        }
    }

    /// A sonda: conecta e desconecta, para o `available` nascer certo.
    async fn probe(&mut self) {
        let backend = self.cfg.backend.clone();
        match detached(move || backend.probe(), OPEN_TIMEOUT).await {
            Detached::Done(Ok(())) => {
                self.status.available = true;
                self.status.error = None;
                self.publish_status();
            }
            Detached::Done(Err(e)) => self.failed(e),
            Detached::TimedOut => self.failed(timed_out()),
            Detached::SpawnFailed(e) => self.failed(spawn_failed(&e)),
            Detached::Panicked(p) => resume_unwind(p),
        }
    }

    /// Abre uma sessão; `None` (com o erro no `status`) se não deu.
    async fn open(&mut self) -> Option<Session> {
        let backend = self.cfg.backend.clone();
        match detached(move || backend.open(), OPEN_TIMEOUT).await {
            Detached::Done(Ok(session)) => {
                self.status.available = true;
                self.status.error = None;
                self.publish_status();
                return Some(session);
            }
            Detached::Done(Err(e)) => self.failed(e),
            Detached::TimedOut => self.failed(timed_out()),
            Detached::SpawnFailed(e) => self.failed(spawn_failed(&e)),
            Detached::Panicked(p) => resume_unwind(p),
        }
        None
    }

    /// O que o barramento diz de `name`, no prazo do `dbus_timeout`; sem
    /// resposta, só o nome (a reserva por nome ainda pode casar).
    async fn identify(&self, name: &str) -> Identity {
        tokio::time::timeout(self.cfg.dbus_timeout, self.cfg.bus.identify(name))
            .await
            .unwrap_or_else(|_| Identity {
                name: name.to_string(),
                ..Identity::default()
            })
    }

    /// Publica um quadro de zeros se o último publicado não era de zeros.
    fn publish_silence(&self) {
        let zeros = json!({ "bands": vec![0.0; BANDS] });
        let last = self.t.bands.current().data;
        if !last.is_null() && last != zeros {
            self.t.bands.publish(zeros);
        }
    }

    /// O laço de uma sessão aberta, até ela terminar.
    async fn session(&mut self, session: Session, mut name: String) -> Exit {
        let Session {
            mut events,
            control,
        } = session;
        let mut capture = Capture {
            control,
            identity: self.identify(&name).await,
            nodes: Vec::new(),
            matched: Vec::new(),
            rings: Rings::new(),
            analyzer: None,
        };
        let mut tick = tokio::time::interval(FRAME);
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut reidentify = tokio::time::interval_at(Instant::now() + REIDENTIFY, REIDENTIFY);
        reidentify.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if let Some(frame) = capture.frame() {
                        publish_changed(&self.t.bands, json!({ "bands": frame }));
                    }
                }
                _ = reidentify.tick(), if capture.matched.is_empty() => {
                    capture.identity = self.identify(&name).await;
                    self.rematch(&mut capture).await;
                }
                event = events.recv() => match event {
                    Some(Event::Graph(nodes)) => {
                        capture.nodes = nodes;
                        self.rematch(&mut capture).await;
                    }
                    Some(Event::Samples { serial, rate, mono }) => {
                        capture.push(serial, rate, &mono);
                    }
                    Some(Event::Disconnected(_)) | None => return Exit::Dropped,
                },
                _ = self.player.changed() => {
                    let Some(new) = self.current_player() else {
                        return Exit::Idle;
                    };
                    name = new;
                    capture.identity = self.identify(&name).await;
                    capture.rings.clear();
                    self.rematch(&mut capture).await;
                }
                _ = self.t.bands.wait_unsubscribed() => return Exit::Idle,
            }
        }
    }

    /// Casa os nós com o player e manda a captura ao backend.
    async fn rematch(&mut self, capture: &mut Capture) {
        let identity = capture.identity.clone();
        let nodes = capture.nodes.clone();
        let procfs = self.cfg.procfs.clone();
        let job = move || match_nodes(&identity, &nodes, &ProcFs::new(procfs));
        match detached(job, PROC_TIMEOUT).await {
            Detached::Done(matched) => capture.matched = matched,
            // O `/proc` não respondeu: fica o casamento anterior.
            Detached::TimedOut | Detached::SpawnFailed(_) => {}
            Detached::Panicked(p) => resume_unwind(p),
        }
        capture.control.capture(&capture.matched);
        capture.rings.retain(&capture.matched);
        self.status.capturing = !capture.matched.is_empty();
        self.publish_status();
    }
}

/// O estado de uma sessão aberta.
struct Capture {
    control: Box<dyn Control>,
    identity: Identity,
    nodes: Vec<OutputNode>,
    /// Os `object.serial` capturados.
    matched: Vec<u64>,
    rings: Rings,
    /// A análise e a taxa para a qual ela foi criada.
    analyzer: Option<(u32, Analyzer)>,
}

impl Capture {
    /// Guarda as amostras de um nó capturado; (re)cria a análise se a taxa
    /// mudou.
    fn push(&mut self, serial: u64, rate: u32, mono: &[f32]) {
        if !self.matched.contains(&serial) {
            return;
        }
        self.rings.push(serial, mono, std::time::Instant::now());
        if self.analyzer.as_ref().is_none_or(|(r, _)| *r != rate) {
            self.analyzer = Some((rate, Analyzer::new(rate)));
        }
    }

    /// O quadro de agora, com 3 casas; `None` antes da primeira amostra.
    fn frame(&mut self) -> Option<Vec<f64>> {
        let (_, analyzer) = self.analyzer.as_mut()?;
        let window = self.rings.window(std::time::Instant::now());
        let frame = analyzer.frame(&window);
        Some(
            frame
                .iter()
                .map(|&v| (f64::from(v) * 1000.0).round() / 1000.0)
                .collect(),
        )
    }
}

/// O laço do módulo: a sonda e, enquanto houver assinante e player, uma
/// sessão com o PipeWire.
async fn run(cfg: SpectrumConfig, t: SpectrumTopics, player: Arc<watch::Sender<Option<String>>>) {
    let mut m = Module {
        cfg,
        t,
        player: player.subscribe(),
        status: Status::default(),
    };
    m.probe().await;
    loop {
        let name = m.wait_for_work().await;
        let Some(session) = m.open().await else {
            m.wait_retry().await;
            continue;
        };
        let exit = m.session(session, name).await;
        // A sessão já foi largada (fluxos e conexão fechados).
        m.publish_silence();
        match exit {
            Exit::Idle => {
                m.status.capturing = false;
                m.publish_status();
            }
            Exit::Dropped => {
                m.failed(DISCONNECTED.to_string());
                m.wait_retry().await;
            }
        }
    }
}
