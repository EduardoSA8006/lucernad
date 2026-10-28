//! Módulo `system` do `lucernad`: as leituras do `/proc` e do `/sys`, o
//! amostrador e o laço que só mede com assinante.
//!
//! Cada leitura é uma função pura sobre o texto do arquivo, mais um ajudante
//! que lê o arquivo de uma raiz ([`Roots`]): nos testes, a raiz é uma pasta de
//! `testdata/`. O módulo se registra num servidor com [`register`] e roda com
//! [`System::spawn`].

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lucerna_service::{RpcError, ServerBuilder, Topic};
use lucernad_core::{GameSignal, publish_changed};
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

use crate::disks::{DiskReader, RealStatfs, Statfs};
use crate::nvidia::{Nvidia, Nvml};
use crate::sampler::{Groups, Sampler};

pub mod cpu;
pub mod disks;
pub mod fdinfo;
pub mod gpu;
pub mod info;
pub mod memory;
pub mod net;
pub mod nvidia;
pub mod sampler;
pub mod temp;

#[cfg(feature = "testing")]
pub mod testing;

pub use lucernad_core::ModuleTask;

/// Onde ficam o `/proc`, o `/sys` e o `/etc` lidos pelo módulo.
#[derive(Clone, Debug)]
pub struct Roots {
    pub procfs: PathBuf,
    pub sysfs: PathBuf,
    pub etc: PathBuf,
}

impl Roots {
    /// As raízes do sistema: `/proc`, `/sys` e `/etc`.
    pub fn system() -> Roots {
        Roots::at(Path::new("/"))
    }

    /// `base/proc`, `base/sys` e `base/etc` (uma máquina de exemplo).
    pub fn at(base: &Path) -> Roots {
        Roots {
            procfs: base.join("proc"),
            sysfs: base.join("sys"),
            etc: base.join("etc"),
        }
    }
}

/// O intervalo de amostragem padrão.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(2);
/// O menor intervalo aceito pelo `system.configure`, em ms.
pub const MIN_INTERVAL_MS: u64 = 500;
/// O maior intervalo aceito pelo `system.configure`, em ms.
pub const MAX_INTERVAL_MS: u64 = 10_000;
/// O prazo de cada `statvfs`.
pub const STATFS_TIMEOUT: Duration = Duration::from_secs(1);
/// No modo jogo (o modo leve), o menor intervalo de amostragem.
pub const LIGHT_INTERVAL: Duration = Duration::from_secs(5);

/// O intervalo valendo: o configurado e, no modo leve, no mínimo `light`.
pub fn effective_interval(configured: Duration, light_on: bool, light: Duration) -> Duration {
    if light_on {
        configured.max(light)
    } else {
        configured
    }
}

/// O texto do estado quando o laço do módulo entra em pânico.
const PANIC_ERROR: &str = "O módulo de sistema parou por um erro interno.";

/// De onde o módulo lê e como.
pub struct SystemConfig {
    pub roots: Roots,
    /// O intervalo inicial (o `system.configure` troca).
    pub interval: Duration,
    pub statfs: Arc<dyn Statfs>,
    /// O prazo de cada `statvfs`.
    pub statfs_timeout: Duration,
    pub nvidia: Box<dyn Nvidia>,
    /// O sinal do modo jogo: ligado, amostra no máximo a cada
    /// `light_interval`.
    pub game: GameSignal,
    pub light_interval: Duration,
}

impl SystemConfig {
    /// A máquina de verdade: `/proc`, `/sys` e `/etc`, o `statvfs` e a NVML.
    pub fn system() -> SystemConfig {
        SystemConfig {
            roots: Roots::system(),
            interval: DEFAULT_INTERVAL,
            statfs: Arc::new(RealStatfs),
            statfs_timeout: STATFS_TIMEOUT,
            nvidia: Box::new(Nvml),
            game: GameSignal::never(),
            light_interval: LIGHT_INTERVAL,
        }
    }
}

/// Os tópicos do módulo.
#[derive(Clone)]
pub struct SystemTopics {
    pub info: Topic,
    pub stats: Topic,
    pub disks: Topic,
    pub gpus: Topic,
    pub status: Topic,
}

/// O módulo registrado, pronto para rodar.
pub struct System {
    pub topics: SystemTopics,
    /// `None` quando o módulo está indisponível (o laço não roda).
    sampler: Option<Sampler>,
    interval: watch::Receiver<Duration>,
    game: GameSignal,
    light_interval: Duration,
}

/// O `interval_ms` dos parâmetros, se for um inteiro na faixa aceita.
fn parse_interval(params: Option<&Value>) -> Option<Duration> {
    params?
        .get("interval_ms")?
        .as_u64()
        .filter(|ms| (MIN_INTERVAL_MS..=MAX_INTERVAL_MS).contains(ms))
        .map(Duration::from_millis)
}

/// Declara os tópicos `system.info`, `system.stats`, `system.disks`,
/// `system.gpus` e `system.status` (nesta ordem) e o método
/// `system.configure`.
///
/// O `system.info` é lido aqui, uma vez. Se o `<procfs>/stat` não puder ser
/// lido, o `system.status` já nasce indisponível e o [`System::spawn`] não
/// roda o laço.
pub fn register(server: &mut ServerBuilder, cfg: SystemConfig) -> System {
    let info = serde_json::to_value(info::read_info(&cfg.roots)).unwrap_or(Value::Null);
    let stat = cfg.roots.procfs.join("stat");
    let (status, available) = match std::fs::read_to_string(&stat) {
        Ok(_) => (json!({"available": true, "error": null}), true),
        Err(e) => (
            json!({
                "available": false,
                "error": format!("Não consegui ler {}: {e}", stat.display()),
            }),
            false,
        ),
    };
    let topics = SystemTopics {
        info: server.topic("system.info", info),
        stats: server.topic("system.stats", Value::Null),
        disks: server.topic("system.disks", Value::Null),
        gpus: server.topic("system.gpus", Value::Null),
        status: server.topic("system.status", status),
    };
    let (tx, interval) = watch::channel(cfg.interval);
    let tx = Arc::new(tx);
    server.method("system.configure", move |params| {
        let tx = tx.clone();
        async move {
            let d = parse_interval(params.as_ref()).ok_or_else(|| {
                RpcError::invalid_params(&format!(
                    "interval_ms precisa ser um inteiro entre {MIN_INTERVAL_MS} e {MAX_INTERVAL_MS}"
                ))
            })?;
            tx.send_replace(d);
            Ok(json!({}))
        }
    });
    let sampler = available.then(|| {
        let disks = DiskReader::new(cfg.statfs, cfg.statfs_timeout);
        Sampler::new(cfg.roots, cfg.nvidia, disks, cfg.interval)
    });
    System {
        topics,
        sampler,
        interval,
        game: cfg.game,
        light_interval: cfg.light_interval,
    }
}

impl System {
    /// Roda o laço do módulo e o supervisor (chame dentro do runtime). Se o
    /// laço entrar em pânico, o supervisor marca o `system.status` como
    /// indisponível; o servidor continua atendendo.
    pub fn spawn(self) -> ModuleTask {
        let Some(sampler) = self.sampler else {
            return ModuleTask::new(Vec::new());
        };
        let run: JoinHandle<()> = tokio::spawn(run(
            sampler,
            self.topics.clone(),
            self.interval,
            self.game,
            self.light_interval,
        ));
        lucernad_core::supervise(
            run,
            self.topics.status.clone(),
            "de sistema",
            json!({"available": false, "error": PANIC_ERROR}),
        )
    }
}

/// Um tick a cada `period`, o primeiro em `start`; tick perdido é pulado.
fn ticker(start: Instant, period: Duration) -> tokio::time::Interval {
    let mut t = tokio::time::interval_at(start, period);
    t.set_missed_tick_behavior(MissedTickBehavior::Skip);
    t
}

/// O laço: espera um assinante, amostra no intervalo os grupos assinados e
/// volta a esperar quando todos saem. No modo jogo, o intervalo é no mínimo
/// `light`.
async fn run(
    mut sampler: Sampler,
    t: SystemTopics,
    mut interval: watch::Receiver<Duration>,
    mut game: GameSignal,
    light: Duration,
) {
    loop {
        tokio::select! {
            _ = t.stats.wait_subscribed() => {}
            _ = t.disks.wait_subscribed() => {}
            _ = t.gpus.wait_subscribed() => {}
        }
        let period = effective_interval(*interval.borrow_and_update(), game.is_on(), light);
        sampler.set_interval(period);
        // O primeiro tick do ciclo é já.
        let mut tick = ticker(Instant::now(), period);
        loop {
            let changed = tokio::select! {
                _ = tick.tick() => false,
                Ok(()) = interval.changed() => true,
                () = game.changed() => true,
            };
            if changed {
                let period = effective_interval(*interval.borrow_and_update(), game.is_on(), light);
                sampler.set_interval(period);
                // Sem amostra fora do ritmo: a próxima vem um intervalo
                // novo depois.
                tick = ticker(Instant::now() + period, period);
                continue;
            }
            let want = Groups {
                stats: t.stats.subscribers() > 0,
                disks: t.disks.subscribers() > 0,
                gpus: t.gpus.subscribers() > 0,
            };
            // Grupo sem assinante esquece a linha de base.
            sampler.reset(!want);
            if want == Groups::default() {
                break;
            }
            if want.stats {
                t.stats
                    .publish(serde_json::to_value(sampler.stats()).unwrap_or(Value::Null));
            }
            if want.disks {
                let disks = sampler.disks().await;
                publish_changed(&t.disks, json!({ "disks": disks }));
            }
            if want.gpus {
                let gpus = sampler.gpus().await;
                publish_changed(&t.gpus, json!({ "gpus": gpus }));
            }
        }
    }
}

/// O conteúdo do arquivo, ou `None` se ele não puder ser lido.
pub(crate) fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}
