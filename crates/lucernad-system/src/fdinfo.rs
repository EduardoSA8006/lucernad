//! O uso das GPUs pelo fdinfo do DRM (`/proc/<pid>/fdinfo/<fd>`).
//!
//! Repete o `drm-fdinfo.sh` e o `drmBusy` do `SystemStats.qml`: cada fd
//! aberto num `/dev/dri/*` traz os contadores do cliente do DRM (i915:
//! nanossegundos ocupados por classe de motor; xe: ciclos ocupados e ciclos
//! totais). O uso sai da diferença entre duas leituras. Sem root, só os
//! processos do próprio usuário aparecem; ler o fdinfo não acorda a GPU.
//!
//! O mesmo passeio pelos fds vê quem está com a NVIDIA aberta
//! ([`DrmSnapshot::nvidia_open`]): a NVML só é chamada nesse caso.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::{Roots, read};

/// Um cliente do DRM: o driver, a placa e os contadores.
#[derive(Clone, Debug, PartialEq)]
pub struct DrmClient {
    pub driver: String,
    /// O endereço PCI da placa (`drm-pdev`); vazio se o fdinfo não trouxer.
    pub pdev: String,
    /// As chaves `engine-*`, `cycles-*` e `total-cycles-*` (sem o `drm-`).
    pub values: BTreeMap<String, f64>,
}

/// Uma leitura de todos os clientes do DRM.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DrmSnapshot {
    /// O instante da leitura, em ns (a média do começo e do fim).
    pub ts_ns: u64,
    /// Os clientes por `(pdev, client-id)`; o mesmo cliente em vários fds
    /// aparece uma vez.
    pub clients: BTreeMap<(String, String), DrmClient>,
    /// Se algum outro processo tem um `/dev/nvidia<n>` aberto.
    pub nvidia_open: bool,
}

/// O id e o cliente do texto de um fdinfo; `None` se não for do DRM (sem
/// `drm-driver` ou sem `drm-client-id`).
///
/// O valor de cada contador é o primeiro número da linha (`500 ns` → 500);
/// um valor que não é número finito deixa a chave de fora.
pub fn parse_fdinfo(text: &str) -> Option<(String, DrmClient)> {
    let (mut driver, mut id, mut pdev) = (None, None, String::new());
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let value = rest.split_whitespace().next().unwrap_or("");
        match key.trim() {
            "drm-driver" if !value.is_empty() => driver = Some(value.to_string()),
            "drm-client-id" if !value.is_empty() => id = Some(value.to_string()),
            "drm-pdev" => pdev = value.to_string(),
            key => {
                let Some(name) = key.strip_prefix("drm-") else {
                    continue;
                };
                let counter = ["engine-", "cycles-", "total-cycles-"]
                    .iter()
                    .any(|p| name.starts_with(p));
                if let Some(v) = value.parse::<f64>().ok().filter(|v| v.is_finite())
                    && counter
                {
                    values.insert(name.to_string(), v);
                }
            }
        }
    }
    let client = DrmClient {
        driver: driver?,
        pdev,
        values,
    };
    Some((id?, client))
}

/// Os nomes numéricos de uma pasta, em ordem numérica.
fn numeric_entries(dir: &Path) -> Vec<(u64, String)> {
    let mut out: Vec<(u64, String)> = std::fs::read_dir(dir)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    Some((name.parse().ok()?, name))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// Se o destino de um fd é uma placa NVIDIA (`/dev/nvidia` + dígitos; o
/// `nvidiactl` e o `nvidia-modeset` não contam).
fn is_nvidia_card(target: &str) -> bool {
    target
        .strip_prefix("/dev/nvidia")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Lê os clientes do DRM de `<procfs>/<pid>/fd/*` (menos o próprio processo).
///
/// Só os fds que apontam para `/dev/dri/*` têm o fdinfo lido; o primeiro de
/// cada `(pdev, client-id)` fica. `ts_ns` é a média do `clock()` antes e
/// depois do passeio, que pode levar dezenas de milissegundos.
pub fn scan(roots: &Roots, clock: &dyn Fn() -> u64) -> DrmSnapshot {
    let t0 = clock();
    let me = u64::from(std::process::id());
    let mut snap = DrmSnapshot::default();
    for (pid, name) in numeric_entries(&roots.procfs) {
        if pid == me {
            continue;
        }
        let dir = roots.procfs.join(&name);
        for (_, fd) in numeric_entries(&dir.join("fd")) {
            let Ok(target) = std::fs::read_link(dir.join("fd").join(&fd)) else {
                continue;
            };
            let target = target.to_string_lossy();
            if is_nvidia_card(&target) {
                snap.nvidia_open = true;
            }
            if !target.starts_with("/dev/dri/") {
                continue;
            }
            let Some((id, client)) =
                read(&dir.join("fdinfo").join(&fd)).and_then(|t| parse_fdinfo(&t))
            else {
                continue;
            };
            snap.clients
                .entry((client.pdev.clone(), id))
                .or_insert(client);
        }
    }
    let t1 = clock();
    snap.ts_ns = ((u128::from(t0) + u128::from(t1)) / 2) as u64;
    snap
}

/// O uso (0 a 1) da placa `pdev` entre duas leituras: a classe de motor mais
/// ocupada, como o `intel_gpu_top`.
///
/// Só entram os clientes presentes nas duas leituras (quem nasce e morre
/// dentro da janela não é contado). i915: Δns do `engine-<classe>` somado nos
/// clientes / (janela × `engine-capacity-<classe>`, padrão 1). xe:
/// Δ`cycles-<classe>` somado / o maior Δ`total-cycles-<classe>`. Contador que
/// volta conta 0. `None` sem janela, com a janela maior que 3 intervalos (a
/// leitura anterior é velha) ou sem cliente em comum. Limitado a 1.
pub fn drm_busy(
    prev: &DrmSnapshot,
    cur: &DrmSnapshot,
    pdev: &str,
    interval_ms: u64,
) -> Option<f64> {
    if cur.ts_ns <= prev.ts_ns {
        return None;
    }
    let window = cur.ts_ns - prev.ts_ns;
    if u128::from(window) > 3 * u128::from(interval_ms) * 1_000_000 {
        return None;
    }
    let window = window as f64;
    let (mut ns, mut cap, mut cycles, mut total) = (
        HashMap::<&str, f64>::new(),
        HashMap::<&str, f64>::new(),
        HashMap::<&str, f64>::new(),
        HashMap::<&str, f64>::new(),
    );
    let mut common = 0;
    for (key, c) in &cur.clients {
        let Some(p) = prev.clients.get(key) else {
            continue;
        };
        if c.pdev != pdev {
            continue;
        }
        common += 1;
        for (name, &value) in &c.values {
            if let Some(class) = name.strip_prefix("engine-capacity-") {
                let e = cap.entry(class).or_insert(0.0);
                *e = e.max(value);
                continue;
            }
            let d = (value - p.values.get(name).copied().unwrap_or(value)).max(0.0);
            if let Some(class) = name.strip_prefix("engine-") {
                *ns.entry(class).or_insert(0.0) += d;
            } else if let Some(class) = name.strip_prefix("total-cycles-") {
                let e = total.entry(class).or_insert(0.0);
                *e = e.max(d);
            } else if let Some(class) = name.strip_prefix("cycles-") {
                *cycles.entry(class).or_insert(0.0) += d;
            }
        }
    }
    if common == 0 {
        return None;
    }
    let mut busy: f64 = 0.0;
    for (class, n) in &ns {
        let c = cap.get(class).copied().filter(|c| *c > 0.0).unwrap_or(1.0);
        busy = busy.max(n / (window * c));
    }
    for (class, n) in &cycles {
        if let Some(t) = total.get(class).filter(|t| **t > 0.0) {
            busy = busy.max(n / t);
        }
    }
    Some(busy.min(1.0))
}
