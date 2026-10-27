//! O laço do modo jogo: os eventos do Hyprland (com junção), as esperas da
//! [`Machine`], os pedidos dos métodos e a restauração pendente, num
//! `select!` só. Ao virar ativo, aplica os efeitos na ordem compositor,
//! perfil, GameMode e modo leve; ao virar inativo, desfaz ao contrário.
//!
//! Tópicos: `game.state` e `game.status`; métodos: `game.set`,
//! `game.toggle`, `game.configure` (o Formato do plano) e `game.get` (o
//! `game.state` atual, para a linha de comando).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use lucerna_service::{RpcError, ServerBuilder, Topic, WorkGuard, WorkTracker};
use lucernad_core::{ModuleTask, publish_changed, supervise};
use lucernad_hyprland::events::{CONNECTED, DISCONNECTED, Event};
use lucernad_hyprland::socket::HyprError;
use lucernad_hyprland::{Hypr, windows};
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::bus::{BusError, GameBus, ZbusGame, timed};
use crate::compositor::{apply, apply_after_reload, restore};
use crate::config::{GameSettings, parse};
use crate::detect::{GameCache, ProcRoot};
use crate::journal::{Journal, Store};
use crate::state::{AUTO_EXIT, FULLSCREEN_ENTER, Machine, Trigger, triggers};

/// A junção dos eventos do Hyprland antes de reler as janelas.
pub const SETTLE: Duration = Duration::from_millis(300);
/// Com diário pendente, de quanto em quanto tempo a restauração é tentada.
pub const RESTORE_RETRY: Duration = Duration::from_secs(5);
/// Quanto o `shutdown` espera o laço desfazer tudo.
pub const SHUTDOWN_WAIT: Duration = Duration::from_secs(5);

const PANIC_ERROR: &str = "O módulo do modo jogo parou por um erro interno.";
const UNAVAILABLE: i32 = 1002;

/// Os eventos que mudam as janelas ou os workspaces à mostra.
const WINDOW_EVENTS: [&str; 12] = [
    "openwindow",
    "closewindow",
    "fullscreen",
    "workspace",
    "workspacev2",
    "movewindowv2",
    "activewindowv2",
    "monitoradded",
    "monitoraddedv2",
    "monitorremoved",
    "monitorremovedv2",
    "configreloaded",
];

/// As esperas do módulo (trocáveis nos testes).
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub settle: Duration,
    pub fullscreen_enter: Duration,
    pub auto_exit: Duration,
    pub restore_retry: Duration,
    pub shutdown: Duration,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing {
            settle: SETTLE,
            fullscreen_enter: FULLSCREEN_ENTER,
            auto_exit: AUTO_EXIT,
            restore_retry: RESTORE_RETRY,
            shutdown: SHUTDOWN_WAIT,
        }
    }
}

/// De onde o modo jogo lê e por onde age.
pub struct GameConfig {
    /// O Hyprland; `None` = sem compositor (D22).
    pub hypr: Option<Arc<dyn Hypr>>,
    pub bus: Arc<dyn GameBus>,
    pub proc: ProcRoot,
    /// O diário; sem ele (sem `XDG_RUNTIME_DIR`), o compositor nunca é
    /// mexido.
    pub journal: Option<Store>,
    /// O sinal do modo leve dos outros módulos.
    pub light: Option<watch::Sender<bool>>,
    pub timing: Timing,
}

impl GameConfig {
    /// A máquina de verdade, com o Hyprland `hypr` e o sinal `light`.
    pub fn system(hypr: Option<Arc<dyn Hypr>>, light: Option<watch::Sender<bool>>) -> GameConfig {
        GameConfig {
            hypr,
            bus: Arc::new(ZbusGame::system()),
            proc: ProcRoot::system(),
            journal: Store::from_env().ok(),
            light,
            timing: Timing::default(),
        }
    }
}

/// Os tópicos do módulo.
#[derive(Clone)]
pub struct GameTopics {
    /// O `game.state` (o Formato).
    pub state: Topic,
    /// O `game.status`: `{available, error}`.
    pub status: Topic,
}

/// Um pedido dos métodos ao laço.
enum Request {
    Set(bool, oneshot::Sender<Value>),
    Toggle(oneshot::Sender<Value>),
    Configure(Box<GameSettings>, oneshot::Sender<Value>),
}

/// O módulo registrado, pronto para rodar.
pub struct Game {
    pub topics: GameTopics,
    cfg: GameConfig,
    work: WorkTracker,
    requests: mpsc::UnboundedReceiver<Request>,
}

/// Pede ao laço para desfazer tudo antes de o processo sair.
#[derive(Clone)]
pub struct GameHandle {
    tx: mpsc::UnboundedSender<oneshot::Sender<()>>,
    wait: Duration,
}

impl GameHandle {
    /// Desfaz tudo (compositor, perfil, GameMode, modo leve) e espera, até
    /// o prazo do shutdown.
    pub async fn shutdown(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(tx).is_ok() {
            let _ = tokio::time::timeout(self.wait, rx).await;
        }
    }
}

/// O estado de antes do primeiro cálculo.
fn initial_state() -> Value {
    json!({"active": false, "manual": null, "reasons": [], "windows": [], "since": null,
           "effects": {"compositor": "off", "vrr": "off", "tearing": "off",
                       "profile": "off", "gamemode": "off", "light": "off"}})
}

fn unavailable() -> RpcError {
    RpcError::app(UNAVAILABLE, "O modo jogo está indisponível.")
}

/// Declara os tópicos `game.state` e `game.status` e os métodos `game.set`,
/// `game.toggle`, `game.configure` e `game.get`.
pub fn register(server: &mut ServerBuilder, cfg: GameConfig, work: WorkTracker) -> Game {
    let topics = GameTopics {
        state: server.topic("game.state", initial_state()),
        status: server.topic("game.status", json!({"available": true, "error": null})),
    };
    let (tx, rx) = mpsc::unbounded_channel::<Request>();
    let t = tx.clone();
    server.method("game.set", move |p| {
        let t = t.clone();
        async move {
            let on = p
                .as_ref()
                .and_then(|p| p.get("on"))
                .and_then(Value::as_bool)
                .ok_or_else(|| RpcError::invalid_params("on precisa ser true ou false"))?;
            ask(&t, |r| Request::Set(on, r)).await
        }
    });
    let t = tx.clone();
    server.method("game.toggle", move |_p| {
        let t = t.clone();
        async move { ask(&t, Request::Toggle).await }
    });
    let state = topics.state.clone();
    server.method("game.get", move |_p| {
        let state = state.clone();
        async move { Ok(state.current().data) }
    });
    let t = tx;
    server.method("game.configure", move |p| {
        let t = t.clone();
        async move {
            let s = parse(&p.unwrap_or(Value::Null))?;
            ask(&t, |r| Request::Configure(Box::new(s), r)).await
        }
    });
    Game {
        topics,
        cfg,
        work,
        requests: rx,
    }
}

async fn ask(
    tx: &mpsc::UnboundedSender<Request>,
    make: impl FnOnce(oneshot::Sender<Value>) -> Request,
) -> Result<Value, RpcError> {
    let (r, rx) = oneshot::channel();
    tx.send(make(r)).map_err(|_| unavailable())?;
    rx.await.map_err(|_| unavailable())
}

impl Game {
    /// Roda o laço e o supervisor (chame dentro do runtime).
    pub fn spawn(self) -> (ModuleTask, GameHandle) {
        let (stop_tx, stop_rx) = mpsc::unbounded_channel();
        let handle = GameHandle {
            tx: stop_tx,
            wait: self.cfg.timing.shutdown,
        };
        let status = self.topics.status.clone();
        let run = Run::new(self.cfg, self.topics, self.work);
        let task = tokio::spawn(run.run(self.requests, stop_rx));
        let task = supervise(
            task,
            status,
            "do modo jogo",
            json!({"available": false, "error": PANIC_ERROR}),
        );
        (task, handle)
    }
}

/// O estado do laço.
struct Run {
    hypr: Option<Arc<dyn Hypr>>,
    bus: Arc<dyn GameBus>,
    proc: ProcRoot,
    store: Option<Store>,
    light: Option<watch::Sender<bool>>,
    timing: Timing,
    topics: GameTopics,
    work: WorkTracker,

    settings: GameSettings,
    configured: bool,
    machine: Machine,
    cache: GameCache,
    stopping: bool,

    /// O que está no diário (espelho do arquivo).
    journal: Option<Journal>,
    /// O que o compositor recebeu por último: (efeitos, vrr, tearing, janelas).
    comp_key: Option<(bool, bool, bool, Vec<String>)>,
    comp_reload: bool,
    comp_error: Option<HyprError>,
    restore_error: Option<HyprError>,
    retry_at: Option<Instant>,
    refresh_at: Option<Instant>,

    cookie: Option<u32>,
    profile_tried: bool,
    profile: Result<(), BusError>,

    gamemode_checked: bool,
    gamemode: Result<(), BusError>,
    registered: BTreeSet<i32>,
    gamemode_failed: BTreeSet<i32>,

    guard: Option<WorkGuard>,
}

impl Run {
    fn new(cfg: GameConfig, topics: GameTopics, work: WorkTracker) -> Run {
        Run {
            hypr: cfg.hypr,
            bus: cfg.bus,
            proc: cfg.proc,
            store: cfg.journal,
            light: cfg.light,
            timing: cfg.timing,
            topics,
            work,
            settings: GameSettings::default(),
            configured: false,
            machine: Machine::with_timing(cfg.timing.fullscreen_enter, cfg.timing.auto_exit),
            cache: GameCache::default(),
            stopping: false,
            journal: None,
            comp_key: None,
            comp_reload: false,
            comp_error: None,
            restore_error: None,
            retry_at: None,
            refresh_at: None,
            cookie: None,
            profile_tried: false,
            profile: Ok(()),
            gamemode_checked: false,
            gamemode: Ok(()),
            registered: BTreeSet::new(),
            gamemode_failed: BTreeSet::new(),
            guard: None,
        }
    }

    async fn run(
        mut self,
        mut requests: mpsc::UnboundedReceiver<Request>,
        mut stop: mpsc::UnboundedReceiver<oneshot::Sender<()>>,
    ) {
        // Antes de tudo: um diário deixado por um daemon que caiu (D13).
        if let Some(store) = &self.store {
            self.journal = store.load();
        }
        if self.journal.is_some() {
            self.try_restore().await;
        }
        self.finish().await;
        let mut events = match &self.hypr {
            Some(h) => h.events(),
            None => broadcast::channel::<Event>(1).1,
        };
        let mut events_open = self.hypr.is_some();
        let mut requests_open = true;
        let mut stop_open = true;
        loop {
            let deadline = [self.machine.next_deadline(), self.refresh_at, self.retry_at]
                .into_iter()
                .flatten()
                .min();
            let mut reply: Option<(oneshot::Sender<Value>, Value)> = None;
            tokio::select! {
                e = events.recv(), if events_open => match e {
                    Ok(e) => self.on_event(&e),
                    Err(RecvError::Lagged(_)) => self.schedule_refresh(),
                    Err(RecvError::Closed) => events_open = false,
                },
                r = requests.recv(), if requests_open => match r {
                    Some(r) => reply = Some(self.on_request(r).await),
                    // O servidor parou (o serviço está saindo): o laço
                    // continua até o shutdown desfazer tudo.
                    None => requests_open = false,
                },
                s = stop.recv(), if stop_open => match s {
                    Some(s) => {
                        self.stopping = true;
                        // A última chance: uma nova tentativa agendada não
                        // adia a restauração.
                        self.retry_at = None;
                        self.comp_key = None;
                        self.finish().await;
                        let _ = s.send(());
                        return;
                    }
                    // Sem quem peça o shutdown: o laço segue.
                    None => stop_open = false,
                },
                _ = sleep_until(deadline) => {}
            }
            let now = Instant::now();
            if self.refresh_at.is_some_and(|t| t <= now) {
                self.refresh().await;
            }
            if self.retry_at.is_some_and(|t| t <= now) {
                self.retry_at = None;
                // A nova tentativa do compositor (aplicar ou desfazer).
                self.comp_key = None;
            }
            self.machine.tick(Instant::now());
            self.finish().await;
            if let Some((r, v)) = reply {
                let _ = r.send(v);
            }
        }
    }

    fn schedule_refresh(&mut self) {
        let at = Instant::now() + self.timing.settle;
        if self.refresh_at.is_none_or(|t| t > at) {
            self.refresh_at = Some(at);
        }
    }

    fn on_event(&mut self, e: &Event) {
        if e.name == "configreloaded" && self.journal.is_some() {
            self.comp_reload = true;
        }
        if e.name == CONNECTED || e.name == DISCONNECTED || WINDOW_EVENTS.contains(&e.name.as_str())
        {
            self.schedule_refresh();
        }
        // Com restauração pendente, cada evento é uma nova chance (D18).
        if self.restore_error.is_some() || self.comp_error.is_some() {
            self.retry_at = Some(Instant::now());
        }
    }

    async fn on_request(&mut self, r: Request) -> (oneshot::Sender<Value>, Value) {
        let now = Instant::now();
        match r {
            Request::Set(on, reply) => {
                self.machine.set_manual(on, now);
                (reply, json!({}))
            }
            Request::Toggle(reply) => {
                let on = !self.machine.view().active;
                self.machine.set_manual(on, now);
                (reply, json!({"active": on}))
            }
            Request::Configure(s, reply) => {
                self.settings = *s;
                self.configured = true;
                // Um configure com o modo ativo reaplica a diferença.
                self.comp_key = None;
                self.refresh().await;
                (reply, json!({}))
            }
        }
    }

    /// Relê as janelas e recalcula os gatilhos (só depois do primeiro
    /// `configure`, D16).
    async fn refresh(&mut self) {
        self.refresh_at = None;
        let now = Instant::now();
        let Some(hypr) = self.hypr.clone().filter(|_| self.configured) else {
            self.machine.update(Vec::new(), now);
            return;
        };
        let found = match windows(&*hypr).await {
            Ok((w, ws)) => {
                let alive = w.iter().map(|w| w.pid).collect();
                self.cache.retain(&alive);
                let proc = &self.proc;
                let cache = &mut self.cache;
                triggers(&w, &ws, |pid| cache.is_game(proc, pid), &self.settings)
            }
            Err(_) => Vec::new(),
        };
        self.machine.update(found, Instant::now());
    }

    fn active(&self) -> bool {
        self.machine.view().active && !self.stopping
    }

    fn trigger_windows(&self) -> Vec<Trigger> {
        if self.active() {
            self.machine.view().windows
        } else {
            Vec::new()
        }
    }

    /// Acerta os efeitos com a decisão e publica.
    async fn finish(&mut self) {
        let active = self.active();
        // O WorkGuard: com o modo ativo ou com um diário pendente (inclusive
        // o achado na subida), o daemon não sai por ociosidade (D18).
        if (active || self.journal.is_some()) && self.guard.is_none() {
            self.guard = Some(self.work.hold());
        }
        if active {
            self.sync_compositor().await;
            self.sync_profile(active).await;
            self.sync_gamemode(active).await;
            self.sync_light(active);
        } else {
            self.sync_light(active);
            self.sync_gamemode(active).await;
            self.sync_profile(active).await;
            self.sync_compositor().await;
        }
        // O WorkGuard só cai sem diário (D18).
        if !active && self.journal.is_none() {
            self.guard = None;
        }
        self.publish();
    }

    async fn sync_compositor(&mut self) {
        let s = &self.settings;
        let want = self.active() && (s.effects || s.vrr || s.tearing);
        let (Some(hypr), Some(store)) = (self.hypr.clone(), self.store.clone()) else {
            return;
        };
        if !want {
            self.comp_key = None;
            self.comp_error = None;
            if self.journal.is_some() && (self.retry_at.is_none() || self.stopping) {
                self.try_restore().await;
            }
            return;
        }
        let tw: Vec<String> = if s.tearing {
            self.trigger_windows()
                .into_iter()
                .map(|t| t.window.address)
                .collect()
        } else {
            Vec::new()
        };
        let key = (s.effects, s.vrr, s.tearing, tw.clone());
        if self.comp_key.as_ref() == Some(&key) && !self.comp_reload {
            return;
        }
        let s = s.clone();
        let result = if self.comp_reload {
            apply_after_reload(&*hypr, &store, &s, &tw).await
        } else {
            apply(&*hypr, &store, &s, &tw).await
        };
        self.comp_key = Some(key);
        self.comp_reload = false;
        match result {
            Ok(j) => {
                self.journal = j;
                self.comp_error = None;
                self.restore_error = None;
            }
            Err(e) => {
                eprintln!("lucernad: o modo jogo não conseguiu mexer no Hyprland: {e}");
                // O que ficou no disco (o diário é gravado antes do eval).
                self.journal = store.load();
                self.comp_error = Some(e);
                self.retry_at = Some(Instant::now() + self.timing.restore_retry);
            }
        }
    }

    async fn try_restore(&mut self) {
        let (Some(hypr), Some(store), Some(j)) =
            (self.hypr.clone(), self.store.clone(), self.journal.clone())
        else {
            return;
        };
        match restore(&*hypr, &store, &j).await {
            Ok(()) => {
                self.journal = None;
                self.restore_error = None;
            }
            Err(e) => {
                eprintln!("lucernad: não consegui desfazer o modo jogo no Hyprland: {e}");
                self.restore_error = Some(e);
                self.retry_at = Some(Instant::now() + self.timing.restore_retry);
            }
        }
    }

    async fn sync_profile(&mut self, active: bool) {
        let want = active && self.settings.profile;
        if want {
            if self.cookie.is_none() && !self.profile_tried {
                self.profile_tried = true;
                // Sem prazo de fora: as etapas têm o delas, e cortar o futuro
                // depois de o pedido sair perderia o cookie com a retenção feita.
                match self.bus.hold_performance().await {
                    Ok(c) => {
                        self.cookie = Some(c);
                        self.profile = Ok(());
                    }
                    Err(e) => {
                        if e != BusError::Absent {
                            eprintln!(
                                "lucernad: o modo jogo não segurou o perfil de desempenho: {e}"
                            );
                        }
                        self.profile = Err(e);
                    }
                }
            }
            return;
        }
        self.profile_tried = false;
        self.profile = Ok(());
        if let Some(c) = self.cookie.take()
            && let Err(e) = self.bus.release(c).await
        {
            eprintln!("lucernad: o modo jogo não soltou o perfil de desempenho: {e}");
        }
    }

    async fn sync_gamemode(&mut self, active: bool) {
        let want_on = active && self.settings.gamemode;
        let want: BTreeSet<i32> = if want_on {
            // Janela sem pid conhecido não vai ao GameMode.
            self.trigger_windows()
                .iter()
                .map(|t| t.window.pid)
                .filter(|pid| *pid > 0)
                .collect()
        } else {
            BTreeSet::new()
        };
        if want_on && !self.gamemode_checked {
            self.gamemode_checked = true;
            self.gamemode = match timed(self.bus.gamemode_present()).await {
                Ok(true) => Ok(()),
                Ok(false) => Err(BusError::Absent),
                Err(e) => Err(e),
            };
        }
        if !want_on {
            self.gamemode_checked = false;
            self.gamemode = Ok(());
        }
        let gone: Vec<i32> = self.registered.difference(&want).copied().collect();
        for pid in gone {
            self.registered.remove(&pid);
            let _ = timed(self.bus.unregister_game(pid)).await;
        }
        self.gamemode_failed.retain(|p| want.contains(p));
        if self.gamemode == Err(BusError::Absent) {
            return;
        }
        let new: Vec<i32> = want
            .difference(&self.registered)
            .filter(|p| !self.gamemode_failed.contains(p))
            .copied()
            .collect();
        for pid in new {
            match timed(self.bus.register_game(pid)).await {
                Ok(()) => {
                    self.registered.insert(pid);
                }
                Err(e) => {
                    eprintln!("lucernad: o GameMode não registrou o processo {pid}: {e}");
                    self.gamemode_failed.insert(pid);
                    self.gamemode = Err(e);
                }
            }
        }
    }

    fn sync_light(&mut self, active: bool) {
        let want = active && self.settings.light;
        if let Some(tx) = &self.light {
            tx.send_if_modified(|v| {
                let changed = *v != want;
                *v = want;
                changed
            });
        }
    }

    fn comp_effect(&self, on: bool) -> String {
        if !self.active() || !on {
            return "off".into();
        }
        if self.hypr.is_none() || self.store.is_none() {
            return "unavailable".into();
        }
        match &self.comp_error {
            None => "ok".into(),
            Some(HyprError::Unreachable(_)) => "unavailable".into(),
            Some(e) => format!("error: {e}"),
        }
    }

    fn publish(&self) {
        let v = self.machine.view();
        let active = self.active();
        let s = &self.settings;
        let mut compositor = self.comp_effect(s.effects);
        if !active && let Some(e) = &self.restore_error {
            compositor = format!("error: não consegui desfazer o modo jogo: {e}");
        }
        let bus_effect = |on: bool, r: &Result<(), BusError>| -> String {
            if !active || !on {
                "off".into()
            } else {
                match r {
                    Ok(()) => "ok".into(),
                    Err(e) => e.effect(),
                }
            }
        };
        let light = if !active || !s.light {
            "off"
        } else if self.light.is_some() {
            "ok"
        } else {
            "unavailable"
        };
        let windows: Vec<Value> = if active {
            v.windows
                .iter()
                .map(|t| {
                    json!({"address": t.window.address, "class": t.window.class,
                           "title": t.window.title, "reason": t.reason.as_str()})
                })
                .collect()
        } else {
            Vec::new()
        };
        let since = v
            .since
            .filter(|_| active)
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64);
        let data = json!({
            "active": active,
            "manual": match v.manual { Some(true) => json!("on"), Some(false) => json!("off"), None => Value::Null },
            "reasons": if active { v.reasons.clone() } else { Vec::new() },
            "windows": windows,
            "since": since,
            "effects": {
                "compositor": compositor,
                "vrr": self.comp_effect(s.vrr),
                "tearing": self.comp_effect(s.tearing),
                "profile": bus_effect(s.profile, &self.profile),
                "gamemode": bus_effect(s.gamemode, &self.gamemode),
                "light": light,
            },
        });
        publish_changed(&self.topics.state, data);
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending().await,
    }
}
