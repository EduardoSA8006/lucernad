//! Dados fixos da máquina: CPU, hostname, sistema e kernel.

use serde::Serialize;

use crate::{Roots, read};

/// O tópico `system.info`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    pub cpu_model: String,
    pub cpu_threads: u32,
    pub hostname: String,
    pub os_name: String,
    pub kernel: String,
}

/// O modelo (o primeiro `model name`, com os espaços colapsados) e o número
/// de threads (linhas `processor`) do `/proc/cpuinfo`.
pub fn parse_cpuinfo(text: &str) -> (String, u32) {
    let key = |line: &str, name: &str| -> Option<String> {
        let rest = line.strip_prefix(name)?.trim_start();
        Some(rest.strip_prefix(':')?.to_string())
    };
    let model = text
        .lines()
        .find_map(|l| key(l, "model name").filter(|v| !v.trim().is_empty()))
        .map(|v| v.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    let threads = text
        .lines()
        .filter(|l| key(l, "processor").is_some())
        .count();
    (model, u32::try_from(threads).unwrap_or(u32::MAX))
}

/// O `PRETTY_NAME` do `os-release`, com ou sem aspas; `""` sem ele.
pub fn parse_os_release(text: &str) -> String {
    text.lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| {
            let v = v.strip_prefix('"').unwrap_or(v);
            v.split('"').next().unwrap_or("").to_string()
        })
        .unwrap_or_default()
}

/// Lê os dados da máquina; o arquivo que falta dá `""` ou `0`.
pub fn read_info(roots: &Roots) -> Info {
    let text = |path: std::path::PathBuf| read(&path).unwrap_or_default();
    let (cpu_model, cpu_threads) = parse_cpuinfo(&text(roots.procfs.join("cpuinfo")));
    let kernel_dir = roots.procfs.join("sys/kernel");
    Info {
        cpu_model,
        cpu_threads,
        hostname: text(kernel_dir.join("hostname")).trim().to_string(),
        os_name: parse_os_release(&text(roots.etc.join("os-release"))),
        kernel: text(kernel_dir.join("osrelease")).trim().to_string(),
    }
}
