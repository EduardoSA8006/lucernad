//! Módulo `power` do `lucernad`: o brilho das telas, o histórico de consumo e
//! os perfis de energia.
//!
//! Toda leitura do sysfs e todo caminho de `/dev/i2c-N` saem de uma
//! [`roots::Roots`]: a de verdade (`/sys`, `/dev`) ou uma árvore falsa (os
//! testes, a fumaça e o `LUCERNAD_POWER_ROOT`). A escolha da tela integrada
//! fica no [`backlight`]; a descoberta dos monitores externos (os conectores,
//! os barramentos candidatos e o casamento pelo EDID), no [`drm`]; o
//! protocolo DDC/CI, no [`ddc`], sobre o barramento do [`i2c`]; o juntar de
//! ajustes de cada tela, no [`adjust`]; as mudanças de fora e o plugar e
//! tirar, pelo [`uevent`] do kernel; o logind, o histórico do UPower e os
//! perfis de energia, pelo D-Bus do [`bus`].
//!
//! O módulo se registra num servidor com [`register`] e roda com
//! [`Power::spawn`]: três tarefas juntas sob um supervisor, as telas (com o
//! uevent só enquanto houver assinante do `power.screens`; os métodos
//! funcionam sem ele), o histórico e os perfis (cada um só com assinante do
//! tópico dele).

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lucerna_service::{RpcError, ServerBuilder, Topic, WorkTracker};
use lucernad_core::{ModuleTask, publish_changed, supervise};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::bus::{
    BacklightWriter, LogindWriter, Point, PowerBus, TreeWriter, ZbusPower, clip_error,
};
use crate::ddc::DdcTiming;
use crate::i2c::{I2cOpener, LinuxI2c};
use crate::roots::{Roots, in_container, root_from_env};
use crate::screens::{Request, ScreensCfg};
use crate::uevent::{KernelUevents, Uevents};

pub mod adjust;
pub mod backlight;
pub mod bus;
pub mod ddc;
pub mod drm;
pub mod i2c;
mod monitor;
pub mod roots;
mod screens;
#[cfg(feature = "testing")]
pub mod testing;
pub mod uevent;

pub use screens::{BUS_AMBIGUOUS, BUS_NOT_FOUND, NO_I2C_DEV};

/// O prazo de um pedido a um monitor (o Set VCP, com a releitura, tem o
/// dobro).
pub const OP_TIMEOUT: Duration = Duration::from_secs(3);
/// Mais velha que isso, a leitura de um monitor é refeita antes de ajustar.
pub const STALE: Duration = Duration::from_secs(2);
/// A espera depois do último uevent do `drm` antes de descobrir de novo.
pub const HOTPLUG_DELAY: Duration = Duration::from_millis(2500);
/// De quanto em quanto tempo o histórico é relido (com assinante).
pub const HISTORY_INTERVAL: Duration = Duration::from_secs(60);
/// O período do histórico, em segundos (a última hora).
pub const HISTORY_SPAN: u32 = bus::HISTORY_TIMESPAN;
/// Quantos pontos o histórico pede.
pub const HISTORY_RESOLUTION: u32 = bus::HISTORY_RESOLUTION;
/// O maior id de tela aceito pelos métodos, em caracteres.
pub const ID_MAX: usize = 64;

/// O texto do estado quando o laço do módulo entra em pânico.
const PANIC_ERROR: &str = "O módulo de energia parou por um erro interno.";
/// O módulo está indisponível.
const UNAVAILABLE: i32 = 1002;

/// De onde o módulo lê e por onde escreve.
pub struct PowerConfig {
    pub roots: Roots,
    /// O `LUCERNAD_POWER_ROOT` inválido (ou o container sem ele): o módulo
    /// fica indisponível com este texto e nada roda.
    pub root_error: Option<String>,
    pub writer: Arc<dyn BacklightWriter>,
    /// Quem abre os barramentos i2c; `None` = sem DDC (a raiz de teste).
    pub i2c: Option<Arc<dyn I2cOpener>>,
    pub bus: Arc<dyn PowerBus>,
    pub uevents: Arc<dyn Uevents>,
    pub timing: DdcTiming,
    pub op_timeout: Duration,
    pub stale: Duration,
    pub hotplug_delay: Duration,
    pub history_interval: Duration,
}

impl PowerConfig {
    /// A máquina de verdade, ou a árvore falsa do `LUCERNAD_POWER_ROOT`
    /// (gravada direto no arquivo, sem logind nem DDC). Uma raiz inválida, ou
    /// um container sem ela, deixa o módulo indisponível: nunca volta à
    /// máquina de verdade com a variável definida.
    pub fn system() -> PowerConfig {
        let value = std::env::var_os("LUCERNAD_POWER_ROOT");
        // Sem DDC e gravando numa raiz que não existe até a variável dizer
        // o contrário: com a raiz inválida, nada roda.
        let nowhere = Roots::at(Path::new("/nonexistent/lucernad-power"));
        let mut cfg = PowerConfig {
            roots: nowhere.clone(),
            root_error: None,
            writer: Arc::new(TreeWriter { roots: nowhere }),
            i2c: None,
            bus: Arc::new(ZbusPower::system()),
            uevents: Arc::new(KernelUevents),
            timing: DdcTiming::standard(),
            op_timeout: OP_TIMEOUT,
            stale: STALE,
            hotplug_delay: HOTPLUG_DELAY,
            history_interval: HISTORY_INTERVAL,
        };
        match root_from_env(value.as_deref(), in_container()) {
            Ok(None) => {
                cfg.roots = Roots::system();
                cfg.writer = Arc::new(LogindWriter::system());
                cfg.i2c = Some(Arc::new(LinuxI2c {
                    dev: PathBuf::from("/dev"),
                }));
            }
            Ok(Some(roots)) => {
                let shown = value.as_deref().map(Path::new).unwrap_or(Path::new(""));
                eprintln!(
                    "lucernad: LUCERNAD_POWER_ROOT={}: o brilho é de teste (árvore falsa, sem logind nem DDC)",
                    shown.display()
                );
                cfg.writer = Arc::new(TreeWriter {
                    roots: roots.clone(),
                });
                cfg.roots = roots;
            }
            Err(e) => {
                eprintln!("lucernad: {e}");
                cfg.root_error = Some(e);
            }
        }
        cfg
    }
}

/// Os tópicos do módulo.
#[derive(Clone)]
pub struct PowerTopics {
    /// O comum `power.screens`: `{screens}`; `null` até a primeira descoberta.
    pub screens: Topic,
    /// O comum `power.history`: `{points}` (e `error`); `null` até a primeira
    /// leitura.
    pub history: Topic,
    /// O comum `power.profiles`: `{available, performance}`; `null` até a
    /// primeira leitura (o shell fica com o último valor que conhece).
    pub profiles: Topic,
    /// O comum `power.status`: `{available, error}`.
    pub status: Topic,
}

/// O módulo registrado, pronto para rodar.
pub struct Power {
    pub topics: PowerTopics,
    parts: Option<Parts>,
}

/// O que o laço leva (nada com a raiz inválida).
struct Parts {
    screens: ScreensCfg,
    bus: Arc<dyn PowerBus>,
    history_interval: Duration,
    work: WorkTracker,
    requests: mpsc::UnboundedReceiver<Request>,
}

/// O que os métodos usam.
#[derive(Clone)]
struct Ctx {
    status: Topic,
    /// `None` com a raiz inválida.
    requests: Option<mpsc::UnboundedSender<Request>>,
}

impl Ctx {
    /// O erro `1002` com o texto do estado.
    fn unavailable(&self) -> RpcError {
        let error = self.status.current().data["error"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| PANIC_ERROR.to_string());
        RpcError::app(
            UNAVAILABLE,
            format!(
                "O módulo de energia está indisponível: {}.",
                error.trim_end_matches('.')
            ),
        )
    }

    /// Manda um pedido ao laço e espera a resposta.
    async fn ask<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<T>) -> Request,
    ) -> Result<T, RpcError> {
        let requests = self.requests.as_ref().ok_or_else(|| self.unavailable())?;
        let (tx, rx) = oneshot::channel();
        requests.send(make(tx)).map_err(|_| self.unavailable())?;
        rx.await.map_err(|_| self.unavailable())
    }

    async fn set(self, p: Value) -> Result<Value, RpcError> {
        let id = id_of(&p)?;
        let percent = field(&p, "percent")
            .and_then(Value::as_i64)
            .filter(|v| (1..=100).contains(v))
            .and_then(|v| u8::try_from(v).ok())
            .ok_or_else(|| bad("percent precisa ser um inteiro entre 1 e 100"))?;
        self.ask(|reply| Request::Set { id, percent, reply })
            .await??;
        Ok(json!({}))
    }

    async fn change(self, p: Value) -> Result<Value, RpcError> {
        let id = id_of(&p)?;
        let delta = field(&p, "delta")
            .and_then(Value::as_i64)
            .filter(|v| (-100..=100).contains(v))
            .and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| bad("delta precisa ser um inteiro entre -100 e 100"))?;
        self.ask(|reply| Request::Change { id, delta, reply })
            .await??;
        Ok(json!({}))
    }

    async fn list(self, _p: Value) -> Result<Value, RpcError> {
        self.ask(|reply| Request::List { reply }).await
    }
}

/// O campo `key` dos parâmetros; `null` conta como ausente.
fn field<'a>(p: &'a Value, key: &str) -> Option<&'a Value> {
    p.get(key).filter(|v| !v.is_null())
}

fn bad(text: &str) -> RpcError {
    RpcError::invalid_params(text)
}

fn id_of(p: &Value) -> Result<String, RpcError> {
    field(p, "id")
        .and_then(Value::as_str)
        .filter(|id| id.chars().count() <= ID_MAX)
        .map(str::to_string)
        .ok_or_else(|| bad("id precisa ser o texto de uma tela, com até 64 caracteres"))
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

/// Declara os tópicos `power.screens`, `power.history`, `power.profiles` e
/// `power.status` e os métodos `power.set`, `power.change` e `power.list`.
pub fn register(server: &mut ServerBuilder, cfg: PowerConfig, work: WorkTracker) -> Power {
    let status = match &cfg.root_error {
        Some(e) => json!({ "available": false, "error": e }),
        None => json!({ "available": true, "error": null }),
    };
    let topics = PowerTopics {
        screens: server.topic("power.screens", Value::Null),
        history: server.topic("power.history", Value::Null),
        profiles: server.topic("power.profiles", Value::Null),
        status: server.topic("power.status", status),
    };
    let (parts, requests) = if cfg.root_error.is_some() {
        (None, None)
    } else {
        let (tx, rx) = mpsc::unbounded_channel();
        let parts = Parts {
            screens: ScreensCfg {
                roots: cfg.roots,
                writer: cfg.writer,
                i2c: cfg.i2c,
                uevents: cfg.uevents,
                timing: cfg.timing,
                op_timeout: cfg.op_timeout,
                stale: cfg.stale,
                hotplug_delay: cfg.hotplug_delay,
            },
            bus: cfg.bus,
            history_interval: cfg.history_interval,
            work,
            requests: rx,
        };
        (Some(parts), Some(tx))
    };
    let ctx = Ctx {
        status: topics.status.clone(),
        requests,
    };
    method(server, "power.set", &ctx, Ctx::set);
    method(server, "power.change", &ctx, Ctx::change);
    method(server, "power.list", &ctx, Ctx::list);
    Power { topics, parts }
}

impl Power {
    /// Roda o laço do módulo e o supervisor (chame dentro do runtime). Se o
    /// laço entrar em pânico, o `power.status` fica indisponível; o servidor
    /// continua atendendo. Com a raiz inválida, nada roda.
    pub fn spawn(self) -> ModuleTask {
        let status = self.topics.status.clone();
        let topics = self.topics;
        let run = match self.parts {
            None => tokio::spawn(async {}),
            Some(parts) => tokio::spawn(async move {
                tokio::join!(
                    screens::run(parts.screens, topics.screens, parts.work, parts.requests),
                    history(topics.history, parts.bus.clone(), parts.history_interval),
                    profiles(topics.profiles, parts.bus),
                );
            }),
        };
        supervise(
            run,
            status,
            "de energia",
            json!({ "available": false, "error": PANIC_ERROR }),
        )
    }
}

/// O histórico: com assinante, lê já e a cada `interval`.
async fn history(topic: Topic, bus: Arc<dyn PowerBus>, interval: Duration) {
    loop {
        topic.wait_subscribed().await;
        let reading = async {
            loop {
                let data = match bus.history().await {
                    Ok(points) => json!({ "points": points_json(&points) }),
                    Err(e) => {
                        let error = format!(
                            "Não consegui ler o histórico do UPower: {}.",
                            clip_error(e.trim_end_matches('.'))
                        );
                        eprintln!("lucernad: {error}");
                        json!({ "points": [], "error": error })
                    }
                };
                publish_changed(&topic, data);
                tokio::time::sleep(interval).await;
            }
        };
        tokio::select! {
            _ = topic.wait_unsubscribed() => {}
            _ = reading => {}
        }
    }
}

fn points_json(points: &[Point]) -> Value {
    points
        .iter()
        .map(|p| json!({ "time": p.time, "rate": p.rate, "state": p.state }))
        .collect()
}

/// Os perfis: com assinante, segue o `watch_profiles`; sem, larga o
/// receptor (e a tarefa do D-Bus termina).
async fn profiles(topic: Topic, bus: Arc<dyn PowerBus>) {
    loop {
        topic.wait_subscribed().await;
        let mut rx = bus.watch_profiles();
        loop {
            tokio::select! {
                p = rx.recv() => match p {
                    Some(p) => publish_changed(
                        &topic,
                        json!({ "available": p.available, "performance": p.performance }),
                    ),
                    None => {
                        topic.wait_unsubscribed().await;
                        break;
                    }
                },
                _ = topic.wait_unsubscribed() => break,
            }
        }
    }
}
