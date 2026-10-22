//! As telas do `power.screens`: a descoberta (a integrada pelo
//! [`backlight`](crate::backlight), os monitores pelo [`drm`](crate::drm) e o
//! EDID), o juntar de ajustes de cada uma ([`Adjuster`]), as escritas (o
//! [`BacklightWriter`] e a thread de cada monitor) e o laço guiado pelo
//! assinante, com o uevent do kernel.
//!
//! O laço é dono de todo o estado; as escritas e as leituras demoradas rodam
//! fora dele (uma tarefa do `tokio` para a integrada, a thread de cada
//! monitor) e voltam como [`Event`] por um canal. Por tela, no máximo uma
//! operação em voo: uma escrita pedida no meio fica guardada e sai quando a
//! anterior volta.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

use lucerna_service::{RpcError, Topic, WorkGuard, WorkTracker};
use lucernad_core::publish_changed;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use tokio::time::Instant;

use crate::adjust::{Adjuster, Step, percent_of};
use crate::backlight::{Backlight, read_value};
use crate::bus::{BacklightWriter, INACTIVE_SESSION, NO_SESSION, clip_error};
use crate::ddc::{DdcError, DdcTiming, Vcp};
use crate::i2c::I2cOpener;
use crate::monitor::{Cmd, Link};
use crate::roots::Roots;
use crate::uevent::{Uevent, Uevents, devpath_name};

mod discovery;

use discovery::Why;

/// O id da tela integrada.
pub const BACKLIGHT_ID: &str = "backlight";
/// O começo do id de um monitor (`ddc:HDMI-A-1`).
pub const DDC_PREFIX: &str = "ddc:";
/// O nome da tela integrada.
pub const BACKLIGHT_NAME: &str = "Tela integrada";

/// Sem o módulo `i2c-dev` carregado.
pub const NO_I2C_DEV: &str =
    "Sem o módulo i2c-dev: os monitores externos precisam dele (veja o README).";
/// Nenhum barramento com o EDID do conector.
pub const BUS_NOT_FOUND: &str = "Não achei o barramento deste monitor.";
/// Mais de um barramento com o EDID do conector.
pub const BUS_AMBIGUOUS: &str =
    "Não deu para saber qual barramento é o deste monitor (outro monitor tem o mesmo EDID).";

/// O erro de leitura da integrada.
fn read_failed(e: &std::io::Error) -> String {
    // O errno e o detalhe vão só para o log; a tela recebe a causa em texto.
    eprintln!("lucernad: não consegui ler o brilho da tela integrada: {e}");
    let cause = match e.kind() {
        std::io::ErrorKind::NotFound => "o arquivo brightness não existe",
        std::io::ErrorKind::PermissionDenied => "sem permissão no arquivo brightness",
        std::io::ErrorKind::InvalidData => "o valor não é um número",
        _ => "o sysfs não deixou ler",
    };
    format!("Não consegui ler o brilho da tela integrada: {cause}.")
}

/// A integrada sem valor lido e sem erro guardado (não deve acontecer).
const PANEL_UNKNOWN: &str = "Não consegui ler o brilho da tela integrada: o valor é desconhecido.";

/// Se o texto é um erro de leitura da integrada (que uma leitura boa apaga).
fn is_read_error(e: &str) -> bool {
    e.starts_with("Não consegui ler o brilho da tela integrada")
}

/// O que as telas precisam da configuração.
pub(crate) struct ScreensCfg {
    pub roots: Roots,
    pub writer: Arc<dyn BacklightWriter>,
    pub i2c: Option<Arc<dyn I2cOpener>>,
    pub uevents: Arc<dyn Uevents>,
    pub timing: DdcTiming,
    pub op_timeout: Duration,
    pub stale: Duration,
    pub hotplug_delay: Duration,
    pub retry_rows: Duration,
    /// Conta as descobertas (para os testes e o diagnóstico).
    pub discoveries: Arc<AtomicUsize>,
}

/// Um pedido dos métodos ao laço.
pub(crate) enum Request {
    Set {
        id: String,
        percent: u8,
        reply: oneshot::Sender<Result<(), RpcError>>,
    },
    Change {
        id: String,
        delta: i32,
        reply: oneshot::Sender<Result<(), RpcError>>,
    },
    List {
        reply: oneshot::Sender<Value>,
    },
}

/// A tela pedida não existe.
fn no_screen() -> RpcError {
    RpcError::app(1001, "Essa tela não existe.")
}

/// A tela existe mas não tem barramento (o erro da linha, que uma nova
/// descoberta não resolveu).
fn row_failed(error: &str) -> RpcError {
    RpcError::app(1003, error)
}

/// Um ajuste pedido a uma tela, em percentual.
#[derive(Clone, Copy, Debug)]
enum Req {
    Set(u8),
    Change(i32),
}

impl Req {
    fn apply(self, adjuster: &mut Adjuster) -> Step {
        match self {
            Req::Set(p) => adjuster.set(p),
            Req::Change(d) => adjuster.change(d),
        }
    }
}

/// O fim de um pedido a um monitor, dentro do prazo ou não.
pub(crate) enum Outcome {
    Done(Result<Vcp, DdcError>),
    TimedOut,
}

/// O que volta ao laço das operações que rodam fora dele.
pub(crate) enum Event {
    /// A escrita da integrada terminou: a releitura (bruta) ou o erro.
    Panel {
        epoch: u64,
        result: Result<u32, String>,
    },
    /// Um pedido a um monitor terminou (ou passou do prazo).
    Ddc {
        output: String,
        epoch: u64,
        cmd: Cmd,
        outcome: Outcome,
    },
    /// A thread de um monitor que tinha passado do prazo voltou.
    Returned {
        output: String,
        epoch: u64,
        result: Result<Vcp, DdcError>,
    },
}

/// A tela integrada.
struct Panel {
    epoch: u64,
    device: Backlight,
    adjuster: Adjuster,
    /// O valor publicado (bruto); `None` se nunca lido.
    shown: Option<u32>,
    error: Option<String>,
    /// Uma escrita em voo.
    writing: bool,
    /// Uma escrita pedida com outra em voo.
    deferred: Option<u32>,
    work: Option<WorkGuard>,
}

/// Um monitor externo.
struct Monitor {
    epoch: u64,
    output: String,
    name: String,
    edid_hash: u64,
    bus: Option<u32>,
    /// A thread dele; `None` quando o barramento não foi achado.
    link: Option<Link>,
    adjuster: Option<Adjuster>,
    max: Option<u32>,
    /// O valor publicado (bruto) e o máximo; `None` se nunca lido.
    shown: Option<(u32, u32)>,
    error: Option<String>,
    last_read: Option<Instant>,
    /// Um pedido na thread (também o que passou do prazo e não voltou).
    busy: bool,
    /// O pedido na thread passou do prazo e ainda não voltou.
    stuck: bool,
    /// Uma leitura pedida (em voo ou esperando a thread voltar).
    reading: bool,
    /// Os ajustes que esperam a leitura.
    queued: Vec<Req>,
    /// Uma escrita pedida com a thread ocupada.
    deferred: Option<u32>,
    work: Option<WorkGuard>,
}

impl Monitor {
    fn id(&self) -> String {
        format!("{DDC_PREFIX}{}", self.output)
    }

    /// Precisa ler antes de escrever: nunca lido, com erro ou com a última
    /// leitura há mais de `stale`.
    fn needs_read(&self, stale: Duration) -> bool {
        self.adjuster.is_none()
            || self.error.is_some()
            || self.last_read.is_none_or(|t| t.elapsed() > stale)
    }

    /// Uma leitura boa: o valor, o máximo e o erro apagado.
    fn fresh(&mut self, vcp: Vcp) {
        let (current, max) = (u32::from(vcp.current), u32::from(vcp.max));
        let adjuster = self
            .adjuster
            .get_or_insert_with(|| Adjuster::new(current, max));
        adjuster.set_max(max);
        let publish = adjuster.external(current);
        self.max = Some(max);
        self.last_read = Some(Instant::now());
        self.error = None;
        if publish {
            self.shown = Some((current, max));
        }
    }
}

/// O estado das telas, dono do laço.
pub(crate) struct Screens {
    cfg: ScreensCfg,
    topic: Topic,
    work: WorkTracker,
    events: UnboundedSender<Event>,
    panel: Option<Panel>,
    monitors: BTreeMap<String, Monitor>,
    /// O barramento achado pelo EDID, por conector e hash do EDID.
    cache: HashMap<(String, u64), u32>,
    discovered: bool,
    discoveries: Arc<AtomicUsize>,
    /// A última nova tentativa das linhas com erro (no máximo uma a cada
    /// `retry_rows`, fora a do hotplug).
    last_row_retry: Option<Instant>,
    /// Os barramentos cuja leitura do EDID passou do prazo: não são relidos
    /// até o próximo hotplug.
    timed_out: HashSet<u32>,
    /// O uevent está sendo escutado (as mudanças de fora chegam sozinhas).
    watching: bool,
    next_gen: u64,
}

/// Roda as telas até os métodos fecharem o canal.
pub(crate) async fn run(
    cfg: ScreensCfg,
    topic: Topic,
    work: WorkTracker,
    mut requests: UnboundedReceiver<Request>,
) {
    let (events_tx, mut events) = unbounded_channel();
    let discoveries = cfg.discoveries.clone();
    let mut s = Screens {
        cfg,
        topic: topic.clone(),
        work,
        events: events_tx,
        panel: None,
        monitors: BTreeMap::new(),
        cache: HashMap::new(),
        discovered: false,
        discoveries,
        last_row_retry: None,
        timed_out: HashSet::new(),
        watching: false,
        next_gen: 0,
    };
    loop {
        // Sem assinante: só os métodos e o que já estava em voo.
        if topic.subscribers() == 0 {
            let next = tokio::select! {
                _ = topic.wait_subscribed() => Next::Subscribed,
                r = requests.recv() => Next::Request(r),
                Some(e) = events.recv() => Next::Event(e),
            };
            match next {
                Next::Request(None) => return,
                Next::Request(Some(r)) => s.request(r, &mut events).await,
                Next::Event(e) => s.event(e),
                _ => {}
            }
            s.publish();
            continue;
        }

        // Com assinante: uma descoberta nova e o uevent.
        s.discover(Why::Subscribed, &mut events).await;
        let mut uevents = match s.cfg.uevents.watch() {
            Ok(rx) => Some(rx),
            Err(e) => {
                eprintln!(
                    "lucernad: sem o uevent do kernel, o brilho não acompanha mudanças de fora: {e}"
                );
                None
            }
        };
        s.watching = uevents.is_some();
        let mut hotplug: Option<Instant> = None;
        loop {
            let deadline = hotplug.unwrap_or_else(Instant::now);
            let next = tokio::select! {
                u = next_uevent(&mut uevents), if uevents.is_some() => Next::Uevent(u),
                _ = tokio::time::sleep_until(deadline), if hotplug.is_some() => Next::Hotplug,
                r = requests.recv() => Next::Request(r),
                Some(e) = events.recv() => Next::Event(e),
                _ = topic.wait_unsubscribed() => Next::Unsubscribed,
            };
            match next {
                Next::Uevent(None) => {
                    eprintln!(
                        "lucernad: o uevent do kernel fechou; o brilho não acompanha mais mudanças de fora"
                    );
                    uevents = None;
                    s.watching = false;
                }
                Next::Uevent(Some(Uevent::Lost)) => {
                    s.reread_panel();
                    s.discover(Why::Hotplug, &mut events).await;
                }
                Next::Uevent(Some(Uevent::Event {
                    subsystem, devpath, ..
                })) => match subsystem.as_str() {
                    "drm" => hotplug = Some(Instant::now() + s.cfg.hotplug_delay),
                    "backlight" => {
                        let ours = s
                            .panel
                            .as_ref()
                            .is_some_and(|p| p.device.device == devpath_name(&devpath));
                        if ours {
                            s.reread_panel();
                        }
                    }
                    _ => {}
                },
                Next::Hotplug => {
                    hotplug = None;
                    s.discover(Why::Hotplug, &mut events).await;
                }
                Next::Request(None) => return,
                Next::Request(Some(r)) => s.request(r, &mut events).await,
                Next::Event(e) => s.event(e),
                Next::Unsubscribed => {
                    s.watching = false;
                    break;
                }
                Next::Subscribed => {}
            }
            s.publish();
        }
    }
}

/// O que acordou o laço.
enum Next {
    Subscribed,
    Unsubscribed,
    Request(Option<Request>),
    Event(Event),
    Uevent(Option<Uevent>),
    Hotplug,
}

async fn next_uevent(rx: &mut Option<UnboundedReceiver<Uevent>>) -> Option<Uevent> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

impl Screens {
    fn epoch(&mut self) -> u64 {
        self.next_gen += 1;
        self.next_gen
    }

    /// Publica o `power.screens` (depois da primeira descoberta), se mudou.
    fn publish(&self) {
        if self.discovered {
            publish_changed(&self.topic, self.to_json());
        }
    }

    fn to_json(&self) -> Value {
        let mut list = Vec::new();
        if let Some(p) = &self.panel {
            list.push(row(
                BACKLIGHT_ID,
                "backlight",
                p.device.output.as_deref(),
                BACKLIGHT_NAME,
                p.shown.map(|v| (v, p.device.max)),
                p.error.as_deref(),
            ));
        }
        for m in self.monitors.values() {
            list.push(row(
                &m.id(),
                "ddc",
                Some(&m.output),
                &m.name,
                m.shown,
                m.error.as_deref(),
            ));
        }
        json!({ "screens": list })
    }

    /// Um pedido dos métodos. Sem descoberta feita (ou com um id que não
    /// existe, talvez plugado sem assinante), descobre antes.
    async fn request(&mut self, r: Request, events: &mut UnboundedReceiver<Event>) {
        match r {
            Request::List { reply } => {
                self.discover(Why::Explicit, events).await;
                let _ = reply.send(self.to_json());
            }
            Request::Set { id, percent, reply } => {
                let result = self.adjust_found(&id, Req::Set(percent), events).await;
                let _ = reply.send(result);
            }
            Request::Change { id, delta, reply } => {
                let result = self.adjust_found(&id, Req::Change(delta), events).await;
                let _ = reply.send(result);
            }
        }
    }

    async fn adjust_found(
        &mut self,
        id: &str,
        req: Req,
        events: &mut UnboundedReceiver<Event>,
    ) -> Result<(), RpcError> {
        // Sem a tela, ou com a linha sem barramento: uma descoberta
        // explícita (o monitor plugado sem assinante, a regra udev instalada
        // depois).
        if !self.discovered || !self.exists(id) || self.row_error(id).is_some() {
            self.discover(Why::Explicit, events).await;
        }
        if !self.exists(id) {
            return Err(no_screen());
        }
        if let Some(error) = self.row_error(id) {
            return Err(row_failed(&error));
        }
        if id == BACKLIGHT_ID {
            if !self.watching || self.panel.as_ref().is_some_and(|p| p.shown.is_none()) {
                // Sem o uevent, o valor pode ter mudado por fora.
                self.reread_panel();
            }
            if let Some(p) = self.panel.as_ref().filter(|p| p.shown.is_none())
                && matches!(req, Req::Change(_))
            {
                // O `change` soma ao valor atual: sem ele, não grava nada.
                let error = p.error.as_deref().unwrap_or(PANEL_UNKNOWN);
                return Err(row_failed(error));
            }
            self.panel_request(req);
        } else if let Some(output) = id.strip_prefix(DDC_PREFIX) {
            self.monitor_request(output, req);
        }
        Ok(())
    }

    /// O erro de uma linha de monitor sem barramento, ou presa (a thread
    /// passou do prazo e não voltou: os pedidos não fazem fila atrás dela).
    fn row_error(&self, id: &str) -> Option<String> {
        let m = self.monitors.get(id.strip_prefix(DDC_PREFIX)?)?;
        if m.stuck {
            return Some(DdcError::NoAnswer.message());
        }
        m.link
            .is_none()
            .then(|| m.error.clone().unwrap_or_default())
    }

    fn exists(&self, id: &str) -> bool {
        if id == BACKLIGHT_ID {
            return self.panel.is_some();
        }
        id.strip_prefix(DDC_PREFIX)
            .is_some_and(|output| self.monitors.contains_key(output))
    }

    /// Um resultado de fora do laço.
    fn event(&mut self, e: Event) {
        match e {
            Event::Panel { epoch, result } => self.panel_written(epoch, result),
            Event::Ddc {
                output,
                epoch,
                cmd,
                outcome,
            } => self.ddc_done(&output, epoch, cmd, outcome),
            Event::Returned {
                output,
                epoch,
                result,
            } => self.ddc_returned(&output, epoch, result),
        }
    }

    // ---- a tela integrada ----

    /// Relê a integrada (o uevent dela, ou eventos perdidos): publica se nada
    /// estiver em voo.
    fn reread_panel(&mut self) {
        let roots = self.cfg.roots.clone();
        let Some(p) = self.panel.as_mut() else {
            return;
        };
        match read_value(&roots, &p.device.device) {
            Ok(v) => {
                if p.adjuster.external(v) {
                    p.shown = Some(v);
                }
                if p.error.as_deref().is_some_and(is_read_error) {
                    p.error = None;
                }
            }
            Err(e) => {
                // Sem leitura, o valor é desconhecido (e o `change` não tem
                // base).
                p.error = Some(read_failed(&e));
                p.shown = None;
            }
        }
    }

    fn panel_request(&mut self, req: Req) {
        if self.panel.as_ref().is_some_and(|p| p.shown.is_none()) {
            // Nunca lida: a base do `change` precisa do valor.
            self.reread_panel();
        }
        let Some(p) = self.panel.as_mut() else {
            return;
        };
        let step = req.apply(&mut p.adjuster);
        self.panel_step(step);
    }

    fn panel_step(&mut self, step: Step) {
        let Some(p) = self.panel.as_mut() else {
            return;
        };
        match step {
            Step::Write(v) if p.writing => p.deferred = Some(v),
            Step::Write(v) => {
                // O que estava guardado ficou para trás deste.
                p.deferred = None;
                self.panel_write(v);
            }
            Step::Publish(v) => {
                p.shown = Some(v);
                p.error = None;
            }
            Step::Failed(e) => p.error = Some(e),
            Step::Idle => {}
        }
        self.panel_work();
    }

    /// Grava `v` pelo writer e relê o `brightness`, numa tarefa própria.
    fn panel_write(&mut self, v: u32) {
        let Some(p) = self.panel.as_mut() else {
            return;
        };
        p.writing = true;
        let epoch = p.epoch;
        let device = p.device.device.clone();
        let writer = self.cfg.writer.clone();
        let roots = self.cfg.roots.clone();
        let reply = PanelReply {
            events: self.events.clone(),
            epoch,
            target: writer.target(),
            sent: false,
        };
        tokio::spawn(async move {
            let result = match writer.write(&device, v).await {
                Ok(()) => read_value(&roots, &device).map_err(|e| read_failed(&e)),
                Err(cause) => Err(write_failed(&*writer, &cause)),
            };
            reply.send(result);
        });
    }

    fn panel_written(&mut self, epoch: u64, result: Result<u32, String>) {
        let Some(p) = self.panel.as_mut().filter(|p| p.epoch == epoch) else {
            return;
        };
        p.writing = false;
        if let Err(e) = &result {
            eprintln!("lucernad: {e}");
        }
        let step = p.adjuster.written(result);
        let wrote = matches!(step, Step::Write(_));
        self.panel_step(step);
        if !wrote && let Some(v) = self.panel.as_mut().and_then(|p| p.deferred.take()) {
            self.panel_write(v);
        }
        self.panel_work();
    }

    fn panel_work(&mut self) {
        let work = self.work.clone();
        if let Some(p) = self.panel.as_mut() {
            let needed = p.writing || p.deferred.is_some() || p.adjuster.busy();
            hold(&mut p.work, needed, &work);
        }
    }

    // ---- os monitores ----

    fn monitor_request(&mut self, output: &str, req: Req) {
        let stale = self.cfg.stale;
        let Some(m) = self.monitors.get_mut(output) else {
            return;
        };
        if m.link.is_none() {
            // Sem barramento: nada a ajustar (a linha já diz por quê).
            return;
        }
        if m.reading {
            m.queued.push(req);
        } else if let Some(adjuster) = m.adjuster.as_mut().filter(|a| a.busy()) {
            let step = req.apply(adjuster);
            self.monitor_step(output, step);
        } else if m.needs_read(stale) {
            m.queued.push(req);
            self.monitor_read(output);
        } else if let Some(adjuster) = m.adjuster.as_mut() {
            let step = req.apply(adjuster);
            self.monitor_step(output, step);
        }
        self.monitor_work(output);
    }

    /// Pede uma leitura; com a thread ocupada, ela sai quando a thread
    /// voltar.
    fn monitor_read(&mut self, output: &str) {
        let Some(m) = self.monitors.get_mut(output) else {
            return;
        };
        m.reading = true;
        if !m.busy {
            self.dispatch(output, Cmd::Get);
        }
    }

    fn monitor_step(&mut self, output: &str, step: Step) {
        let Some(m) = self.monitors.get_mut(output) else {
            return;
        };
        match step {
            Step::Write(v) if m.busy => m.deferred = Some(v),
            Step::Write(v) => {
                // O que estava guardado ficou para trás deste.
                m.deferred = None;
                let value = u16::try_from(v).unwrap_or(u16::MAX);
                self.dispatch(output, Cmd::Set(value));
            }
            Step::Publish(v) => {
                m.shown = m.max.map(|max| (v, max));
                m.error = None;
            }
            Step::Failed(e) => m.error = Some(e),
            Step::Idle => {}
        }
    }

    /// Manda `cmd` à thread do monitor e espera a resposta numa tarefa, com
    /// prazo; passado o prazo, avisa o laço e continua esperando a thread
    /// voltar (ela nunca é morta).
    fn dispatch(&mut self, output: &str, cmd: Cmd) {
        // O Set são duas transações (a escrita e a releitura) e, com o
        // barramento ocupado por outro processo, cada uma espera o `flock`:
        // o dobro do prazo.
        let limit = match cmd {
            Cmd::Get => self.cfg.op_timeout,
            Cmd::Set(_) => self.cfg.op_timeout * 2,
        };
        let events = self.events.clone();
        let Some(m) = self.monitors.get_mut(output) else {
            return;
        };
        let Some(link) = m.link.as_mut() else {
            return;
        };
        m.busy = true;
        let rx = link.request(cmd);
        let (output, epoch) = (output.to_string(), m.epoch);
        tokio::spawn(async move {
            let mut rx = rx;
            let outcome = match tokio::time::timeout(limit, &mut rx).await {
                Ok(reply) => Outcome::Done(reply.unwrap_or(Err(DdcError::NoAnswer))),
                Err(_) => Outcome::TimedOut,
            };
            let timed_out = matches!(outcome, Outcome::TimedOut);
            let _ = events.send(Event::Ddc {
                output: output.clone(),
                epoch,
                cmd,
                outcome,
            });
            if timed_out {
                let result = rx.await.unwrap_or(Err(DdcError::NoAnswer));
                let _ = events.send(Event::Returned {
                    output,
                    epoch,
                    result,
                });
            }
        });
    }

    fn ddc_done(&mut self, output: &str, epoch: u64, cmd: Cmd, outcome: Outcome) {
        let op_timeout = self.cfg.op_timeout;
        let Some(m) = self.monitors.get_mut(output).filter(|m| m.epoch == epoch) else {
            return;
        };
        let timed_out = matches!(outcome, Outcome::TimedOut);
        let result = match outcome {
            Outcome::Done(result) => {
                m.busy = false;
                result
            }
            Outcome::TimedOut => {
                // A thread segue presa (vazamento aceito): o próximo pedido
                // espera ela voltar, e o trabalho do daemon não fica preso
                // nela.
                eprintln!(
                    "lucernad: o monitor {output} não respondeu em {op_timeout:?}; \
                     a thread dele fica esperando o driver voltar"
                );
                m.stuck = true;
                Err(DdcError::NoAnswer)
            }
        };
        self.ddc_result(output, cmd, result);
        if timed_out && let Some(m) = self.monitors.get_mut(output) {
            m.error = Some(DdcError::NoAnswer.message());
            m.queued.clear();
        }
        self.monitor_next(output);
        self.monitor_work(output);
    }

    fn ddc_returned(&mut self, output: &str, epoch: u64, result: Result<Vcp, DdcError>) {
        let Some(m) = self.monitors.get_mut(output).filter(|m| m.epoch == epoch) else {
            return;
        };
        eprintln!("lucernad: o monitor {output} voltou a responder à thread");
        m.busy = false;
        m.stuck = false;
        if m.reading {
            // A leitura que esperava a thread: vale o que ela trouxe.
            self.ddc_result(output, Cmd::Get, result);
        } else if m.deferred.is_none()
            && let Ok(vcp) = result
        {
            m.fresh(vcp);
        }
        self.monitor_next(output);
        self.monitor_work(output);
    }

    /// O resultado de uma leitura ou de uma escrita (com a releitura).
    fn ddc_result(&mut self, output: &str, cmd: Cmd, result: Result<Vcp, DdcError>) {
        let Some(m) = self.monitors.get_mut(output) else {
            return;
        };
        if let Err(e) = &result {
            eprintln!("lucernad: o monitor {output}: {}", e.message());
        }
        match cmd {
            Cmd::Get => {
                m.reading = false;
                match result {
                    Ok(vcp) => {
                        m.fresh(vcp);
                        for req in std::mem::take(&mut m.queued) {
                            let Some(adjuster) = self
                                .monitors
                                .get_mut(output)
                                .and_then(|m| m.adjuster.as_mut())
                            else {
                                break;
                            };
                            let step = req.apply(adjuster);
                            self.monitor_step(output, step);
                        }
                    }
                    Err(e) => {
                        m.error = Some(e.message());
                        m.queued.clear();
                    }
                }
            }
            Cmd::Set(_) => {
                let step = match (result, m.adjuster.as_mut()) {
                    (Ok(vcp), Some(adjuster)) => {
                        let max = u32::from(vcp.max);
                        adjuster.set_max(max);
                        m.max = Some(max);
                        m.last_read = Some(Instant::now());
                        adjuster.written(Ok(u32::from(vcp.current)))
                    }
                    (Err(e), Some(adjuster)) => adjuster.written(Err(e.message())),
                    (_, None) => Step::Idle,
                };
                self.monitor_step(output, step);
            }
        }
    }

    /// Com a thread livre: a escrita guardada, ou a leitura pedida.
    fn monitor_next(&mut self, output: &str) {
        let Some(m) = self.monitors.get_mut(output).filter(|m| !m.busy) else {
            return;
        };
        if let Some(v) = m.deferred.take() {
            let value = u16::try_from(v).unwrap_or(u16::MAX);
            self.dispatch(output, Cmd::Set(value));
        } else if m.reading {
            self.dispatch(output, Cmd::Get);
        }
    }

    fn monitor_work(&mut self, output: &str) {
        let work = self.work.clone();
        if let Some(m) = self.monitors.get_mut(output) {
            // Um monitor preso não segura o daemon (a thread pode nunca
            // voltar).
            let needed = !m.stuck
                && (m.deferred.is_some()
                    || !m.queued.is_empty()
                    || m.adjuster.as_ref().is_some_and(Adjuster::busy));
            hold(&mut m.work, needed, &work);
        }
    }
}

/// A resposta da escrita da integrada ao laço: se a tarefa cair num pânico
/// do writer antes de responder, o `Drop` responde com o erro (a escrita não
/// fica em voo para sempre, nem o trabalho do daemon preso).
struct PanelReply {
    events: UnboundedSender<Event>,
    epoch: u64,
    target: &'static str,
    sent: bool,
}

impl PanelReply {
    fn send(mut self, result: Result<u32, String>) {
        self.sent = true;
        let _ = self.events.send(Event::Panel {
            epoch: self.epoch,
            result,
        });
    }
}

impl Drop for PanelReply {
    fn drop(&mut self) {
        if !self.sent {
            let _ = self.events.send(Event::Panel {
                epoch: self.epoch,
                result: Err(format!(
                    "Não consegui ajustar o brilho {}: erro interno.",
                    self.target
                )),
            });
        }
    }
}

/// Segura (ou solta) o trabalho do daemon.
fn hold(slot: &mut Option<WorkGuard>, needed: bool, work: &WorkTracker) {
    if !needed {
        *slot = None;
    } else if slot.is_none() {
        *slot = Some(work.hold());
    }
}

/// O texto de uma escrita da integrada que falhou: o `NO_SESSION` e o
/// `INACTIVE_SESSION` já são o texto inteiro; o resto ganha o começo do
/// Formato.
fn write_failed(writer: &dyn BacklightWriter, cause: &str) -> String {
    if cause == NO_SESSION || cause == INACTIVE_SESSION {
        return cause.to_string();
    }
    format!(
        "Não consegui ajustar o brilho {}: {}.",
        writer.target(),
        clip_error(cause.trim_end_matches('.'))
    )
}

/// Uma linha do `power.screens`.
fn row(
    id: &str,
    kind: &str,
    output: Option<&str>,
    name: &str,
    shown: Option<(u32, u32)>,
    error: Option<&str>,
) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), json!(id));
    o.insert("kind".into(), json!(kind));
    if let Some(output) = output {
        o.insert("output".into(), json!(output));
    }
    o.insert("name".into(), json!(name));
    match shown {
        Some((value, max)) => {
            o.insert("value".into(), json!(value));
            o.insert("max".into(), json!(max));
            o.insert("percent".into(), json!(percent_of(value, max)));
        }
        None => {
            for key in ["value", "max", "percent"] {
                o.insert(key.into(), Value::Null);
            }
        }
    }
    if let Some(error) = error {
        o.insert("error".into(), json!(error));
    }
    Value::Object(o)
}
