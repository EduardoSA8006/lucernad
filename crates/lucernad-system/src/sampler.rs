//! O amostrador: junta as leituras num ciclo de amostragem.
//!
//! Guarda o que precisa de duas leituras (CPU, rede por interface e o fdinfo
//! do DRM) e o histórico da rede. No começo de um ciclo (sem leitura
//! anterior), repete o último uso da CPU e as últimas taxas, sem pôr ponto no
//! histórico: o delta de minutos desde o ciclo anterior não diria nada.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::cpu::{CpuTimes, cpu_usage, parse_stat};
use crate::disks::{Disk, DiskReader, read_disks};
use crate::fdinfo::{DrmSnapshot, drm_busy, scan};
use crate::gpu::{Card, Gpu, Vendor, list_cards, read_gpus};
use crate::memory::parse_meminfo;
use crate::net::{NetTotals, parse_interfaces, push_history, rates};
use crate::nvidia::{NoNvidia, Nvidia};
use crate::temp::{find_sensor, read_temp};
use crate::{Detached, Roots, detached, read};

/// O prazo de uma leitura das GPUs com a NVML.
pub const NVIDIA_TIMEOUT: Duration = Duration::from_secs(2);

/// Os grupos de leitura, um por tópico.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Groups {
    pub stats: bool,
    pub disks: bool,
    pub gpus: bool,
}

impl std::ops::Not for Groups {
    type Output = Groups;

    fn not(self) -> Groups {
        Groups {
            stats: !self.stats,
            disks: !self.disks,
            gpus: !self.gpus,
        }
    }
}

/// O tópico `system.stats`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub cpu_usage: f64,
    pub cpu_temp: Option<f64>,
    pub mem_total: u64,
    pub mem_used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub rx_rate: f64,
    pub tx_rate: f64,
    pub rx_total: u64,
    pub tx_total: u64,
    pub rx_history: Vec<f64>,
    pub tx_history: Vec<f64>,
    pub uptime: f64,
}

/// As leituras de um ciclo, com a linha de base de cada grupo.
pub struct Sampler {
    roots: Roots,
    nvidia: Arc<Mutex<Box<dyn Nvidia>>>,
    nvidia_timeout: Duration,
    disks: DiskReader,
    interval: Duration,
    /// O relógio do fdinfo (ns desde a criação).
    start: Instant,
    cpu_prev: Option<CpuTimes>,
    net_prev: Option<(Instant, BTreeMap<String, NetTotals>)>,
    /// O sensor escolhido neste ciclo (`None`: ainda não escolhido).
    sensor: Option<Option<PathBuf>>,
    /// A leitura anterior do fdinfo, só com alguma Intel ativa.
    drm_prev: Option<DrmSnapshot>,
    /// Se a NVML presa já foi avisada no log (até ela responder de novo).
    nvidia_warned: bool,
    last: Stats,
}

impl Sampler {
    /// Um amostrador sem linha de base, com `interval` entre as amostras.
    pub fn new(
        roots: Roots,
        nvidia: Box<dyn Nvidia>,
        disks: DiskReader,
        interval: Duration,
    ) -> Sampler {
        Sampler {
            roots,
            nvidia: Arc::new(Mutex::new(nvidia)),
            nvidia_timeout: NVIDIA_TIMEOUT,
            disks,
            interval,
            start: Instant::now(),
            cpu_prev: None,
            net_prev: None,
            sensor: None,
            drm_prev: None,
            nvidia_warned: false,
            last: Stats::default(),
        }
    }

    /// Troca o prazo da leitura com a NVML (padrão [`NVIDIA_TIMEOUT`]).
    pub fn with_nvidia_timeout(mut self, timeout: Duration) -> Sampler {
        self.nvidia_timeout = timeout;
        self
    }

    /// O intervalo entre as amostras (vale para a janela do fdinfo).
    pub fn set_interval(&mut self, interval: Duration) {
        self.interval = interval;
    }

    /// Esquece a linha de base dos grupos em `stopped`: com `stats`, a CPU, a
    /// rede e o sensor; com `gpus`, o fdinfo. O histórico e os últimos
    /// valores ficam.
    pub fn reset(&mut self, stopped: Groups) {
        if stopped.stats {
            self.cpu_prev = None;
            self.net_prev = None;
            self.sensor = None;
        }
        if stopped.gpus {
            self.drm_prev = None;
        }
    }

    /// CPU, temperatura, memória, swap, rede e tempo ligado.
    pub fn stats(&mut self) -> Stats {
        let procfs = &self.roots.procfs;
        let mut st = self.last.clone();

        let cpu = read(&procfs.join("stat")).and_then(|t| parse_stat(&t));
        if let (Some(prev), Some(cur)) = (self.cpu_prev, cpu)
            && let Some(usage) = cpu_usage(prev, cur)
        {
            st.cpu_usage = usage;
        }
        self.cpu_prev = cpu;

        let sensor = self.sensor.get_or_insert_with(|| find_sensor(&self.roots));
        st.cpu_temp = sensor.as_deref().and_then(read_temp);

        let mem = parse_meminfo(&read(&procfs.join("meminfo")).unwrap_or_default());
        st.mem_total = mem.mem_total;
        st.mem_used = mem.mem_used;
        st.swap_total = mem.swap_total;
        st.swap_used = mem.swap_used;

        let now = Instant::now();
        let ifaces = parse_interfaces(&read(&procfs.join("net/dev")).unwrap_or_default());
        st.rx_total = ifaces.values().fold(0, |a, t| a.saturating_add(t.rx));
        st.tx_total = ifaces.values().fold(0, |a, t| a.saturating_add(t.tx));
        if let Some((then, prev)) = &self.net_prev {
            let (rx, tx) = rates(prev, &ifaces, now.duration_since(*then).as_secs_f64());
            st.rx_rate = rx;
            st.tx_rate = tx;
            push_history(&mut st.rx_history, rx);
            push_history(&mut st.tx_history, tx);
        }
        self.net_prev = Some((now, ifaces));

        st.uptime = read(&procfs.join("uptime"))
            .and_then(|t| t.split_whitespace().next()?.parse().ok())
            .unwrap_or(0.0);

        self.last = st.clone();
        st
    }

    /// Os discos de agora (cada `statvfs` com prazo).
    pub async fn disks(&self) -> Vec<Disk> {
        read_disks(&self.roots, &self.disks).await
    }

    /// As GPUs de agora.
    ///
    /// O fdinfo só é percorrido com alguma Intel ou NVIDIA ativa (ele dá o uso
    /// da Intel e diz se alguém está com a NVIDIA aberta). A leitura roda numa
    /// thread solta, com prazo: uma NVML presa (ou ainda presa de uma leitura
    /// anterior) faz aquela leitura sair sem ela. Um pânico na leitura é
    /// repassado, para o supervisor do módulo ver.
    pub async fn gpus(&mut self) -> Vec<Gpu> {
        let cards = list_cards(&self.roots);
        let active = |v: Vendor| cards.iter().any(|c| c.vendor == v && !c.suspended);
        let mut usage = HashMap::new();
        let mut nvidia_open = false;
        if active(Vendor::Intel) || active(Vendor::Nvidia) {
            let start = self.start;
            let clock = move || u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
            let snap = scan(&self.roots, &clock);
            nvidia_open = snap.nvidia_open;
            if active(Vendor::Intel) {
                let prev = self.drm_prev.take().unwrap_or_default();
                let interval_ms = u64::try_from(self.interval.as_millis()).unwrap_or(u64::MAX);
                for c in cards
                    .iter()
                    .filter(|c| c.vendor == Vendor::Intel && !c.suspended)
                {
                    if let Some(u) = drm_busy(&prev, &snap, &c.pci, interval_ms) {
                        usage.insert(c.pci.clone(), u);
                    }
                }
                self.drm_prev = Some(snap);
            } else {
                self.drm_prev = None;
            }
        } else {
            self.drm_prev = None;
        }

        let without_nvidia = |cards: &[Card], usage: &HashMap<String, f64>| {
            read_gpus(&self.roots, cards, &mut NoNvidia, false, usage)
        };
        if !nvidia_open {
            return without_nvidia(&cards, &usage);
        }
        let (roots, nvidia, job_cards, job_usage) = (
            self.roots.clone(),
            self.nvidia.clone(),
            cards.clone(),
            usage.clone(),
        );
        let job = move || match nvidia.try_lock() {
            Ok(mut nv) => Some(read_gpus(&roots, &job_cards, &mut **nv, true, &job_usage)),
            // Envenenado só depois de um pânico, que já derrubou o laço.
            Err(TryLockError::Poisoned(e)) => Some(read_gpus(
                &roots,
                &job_cards,
                &mut **e.into_inner(),
                true,
                &job_usage,
            )),
            // A leitura anterior ainda está presa na NVML.
            Err(TryLockError::WouldBlock) => None,
        };
        let outcome = detached(job, self.nvidia_timeout).await;
        match outcome {
            Detached::Done(Some(gpus)) => {
                self.nvidia_warned = false;
                gpus
            }
            Detached::Panicked(p) => std::panic::resume_unwind(p),
            other => {
                if !self.nvidia_warned {
                    self.nvidia_warned = true;
                    match other {
                        Detached::SpawnFailed(e) => {
                            eprintln!("lucernad: não consegui criar a thread da NVML: {e}")
                        }
                        _ => eprintln!(
                            "lucernad: a NVML não respondeu em {:?}; as GPUs NVIDIA saem sem uso até ela voltar",
                            self.nvidia_timeout
                        ),
                    }
                }
                without_nvidia(&cards, &usage)
            }
        }
    }
}
