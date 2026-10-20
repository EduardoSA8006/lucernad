//! GPUs: as placas do `/sys/class/drm`, com o uso, a temperatura e as
//! frequências de cada fabricante.
//!
//! Repete o `gpu-status.sh` do shell, sem processo externo: a NVIDIA pela
//! NVML ([`Nvidia`]) em vez do `nvidia-smi`, a AMD pelo `gpu_busy_percent` e
//! pelo hwmon, a Intel pelas frequências do sysfs e pelo uso calculado do
//! fdinfo do DRM ([`crate::fdinfo`]). Uma placa suspensa não é lida: ler a
//! NVML ou o `gpu_busy_percent` acordaria a placa.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::nvidia::{Nvidia, NvidiaReading};
use crate::{Roots, read};

/// Os fabricantes conhecidos.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vendor {
    Nvidia,
    Amd,
    Intel,
}

impl Vendor {
    /// O fabricante pelo id PCI do `device/vendor` (`0x10de`, `0x1002`, `0x8086`).
    pub fn from_id(id: &str) -> Option<Vendor> {
        match id {
            "0x10de" => Some(Vendor::Nvidia),
            "0x1002" => Some(Vendor::Amd),
            "0x8086" => Some(Vendor::Intel),
            _ => None,
        }
    }

    /// O nome no tópico: `"nvidia"`, `"amd"` ou `"intel"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Vendor::Nvidia => "nvidia",
            Vendor::Amd => "amd",
            Vendor::Intel => "intel",
        }
    }

    /// O nome mostrado quando não há um modelo.
    pub fn generic_name(self) -> &'static str {
        match self {
            Vendor::Nvidia => "NVIDIA",
            Vendor::Amd => "AMD Radeon",
            Vendor::Intel => "Intel (integrada)",
        }
    }
}

/// Uma placa do DRM.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    /// O nome no sysfs (`card0`).
    pub card: String,
    pub vendor: Vendor,
    /// O endereço PCI (`0000:00:02.0`).
    pub pci: String,
    /// Se o `device/power/runtime_status` é `suspended` (`suspending` e
    /// `resuming` contam como ativos).
    pub suspended: bool,
    /// A pasta `class/drm/<card>`.
    pub dir: PathBuf,
}

/// Se o nome é de uma placa (`card` + dígito, sem `-`): `card0-eDP-1` é um
/// conector e fica de fora.
fn is_card_name(name: &str) -> bool {
    name.strip_prefix("card")
        .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        && !name.contains('-')
}

/// As placas de `<sysfs>/class/drm`, em ordem de nome, só as com um
/// `device/vendor` legível e conhecido.
pub fn list_cards(roots: &Roots) -> Vec<Card> {
    let drm = roots.sysfs.join("class/drm");
    let Ok(entries) = std::fs::read_dir(&drm) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| is_card_name(n))
        .collect();
    names.sort();
    names
        .into_iter()
        .filter_map(|card| {
            let dir = drm.join(&card);
            let device = dir.join("device");
            let vendor = Vendor::from_id(read(&device.join("vendor"))?.trim())?;
            let pci = pci_of(&device)?;
            let suspended =
                read(&device.join("power/runtime_status")).is_some_and(|s| s.trim() == "suspended");
            Some(Card {
                card,
                vendor,
                pci,
                suspended,
                dir,
            })
        })
        .collect()
}

/// O endereço PCI: o `PCI_SLOT_NAME` do `uevent` ou, sem ele, o nome do
/// `device` resolvido (o `readlink -f` do script).
fn pci_of(device: &Path) -> Option<String> {
    let from_uevent = read(&device.join("uevent")).and_then(|text| {
        text.lines()
            .find_map(|l| l.strip_prefix("PCI_SLOT_NAME="))
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    });
    from_uevent.or_else(|| {
        let real = std::fs::canonicalize(device).ok()?;
        Some(real.file_name()?.to_string_lossy().into_owned())
    })
}

/// O modelo da NVIDIA: o texto depois de `Model:` (e dos espaços) em
/// `<procfs>/driver/nvidia/gpus/<pci>/information`. Ler esse arquivo não
/// acorda a placa.
pub fn nvidia_model(roots: &Roots, pci: &str) -> Option<String> {
    let path = roots
        .procfs
        .join("driver/nvidia/gpus")
        .join(pci)
        .join("information");
    read(&path)?
        .lines()
        .find_map(|l| l.strip_prefix("Model:"))
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
}

/// Uma GPU no tópico `system.gpus`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Gpu {
    pub card: String,
    pub vendor: String,
    pub pci: String,
    /// `"active"` ou `"suspended"`.
    pub state: String,
    pub name: String,
    /// Uso de 0 a 1.
    pub usage: Option<f64>,
    /// °C.
    pub temp: Option<f64>,
    /// MHz.
    pub freq: Option<u32>,
    /// MHz.
    pub max_freq: Option<u32>,
}

/// As GPUs de `cards`, na mesma ordem.
///
/// Placa suspensa: só fabricante, PCI, estado e nome. NVIDIA ativa: a
/// leitura de `nvidia` só com `nvidia_open` (outro processo com a placa
/// aberta); sem isso, só o nome. AMD ativa: `gpu_busy_percent` / 100 e o
/// primeiro `hwmon*/temp1_input` / 1000 truncado. Intel ativa: as frequências
/// do i915 (`gt_*_freq_mhz`) ou do xe (`tile0/gt0/freq0/*`) e o uso de
/// `drm_usage` pelo PCI. Síncrona: a NVML pode demorar, então quem chama a
/// roda numa thread solta, com prazo.
pub fn read_gpus(
    roots: &Roots,
    cards: &[Card],
    nvidia: &mut dyn Nvidia,
    nvidia_open: bool,
    drm_usage: &HashMap<String, f64>,
) -> Vec<Gpu> {
    cards
        .iter()
        .map(|card| {
            let name = match card.vendor {
                Vendor::Nvidia => nvidia_model(roots, &card.pci),
                _ => None,
            }
            .unwrap_or_else(|| card.vendor.generic_name().to_string());
            let reading = if card.suspended {
                NvidiaReading::default()
            } else {
                match card.vendor {
                    Vendor::Nvidia if nvidia_open => nvidia.read(&card.pci).unwrap_or_default(),
                    Vendor::Nvidia => NvidiaReading::default(),
                    Vendor::Amd => read_amd(&card.dir.join("device")),
                    Vendor::Intel => read_intel(card, drm_usage),
                }
            };
            Gpu {
                card: card.card.clone(),
                vendor: card.vendor.as_str().to_string(),
                pci: card.pci.clone(),
                state: if card.suspended {
                    "suspended"
                } else {
                    "active"
                }
                .to_string(),
                name,
                usage: reading.usage,
                temp: reading.temp,
                freq: reading.freq,
                max_freq: reading.max_freq,
            }
        })
        .collect()
}

/// O número inteiro do arquivo, ou `None`.
fn read_num<T: std::str::FromStr>(text: Option<String>) -> Option<T> {
    text?.trim().parse().ok()
}

fn read_amd(device: &Path) -> NvidiaReading {
    let usage =
        read_num::<u32>(read(&device.join("gpu_busy_percent"))).map(|p| f64::from(p) / 100.0);
    let mut hwmons: Vec<PathBuf> = std::fs::read_dir(device.join("hwmon"))
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().starts_with("hwmon"))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    hwmons.sort();
    // O primeiro `temp1_input` legível, como o `cat hwmon*/temp1_input | head -n1`.
    let temp = hwmons
        .iter()
        .find_map(|h| read(&h.join("temp1_input")))
        .and_then(|t| read_num::<i64>(Some(t)))
        .map(|milli| (milli / 1000) as f64);
    NvidiaReading {
        usage,
        temp,
        ..NvidiaReading::default()
    }
}

fn read_intel(card: &Card, drm_usage: &HashMap<String, f64>) -> NvidiaReading {
    let xe = card.dir.join("device/tile0/gt0/freq0");
    let pick = |i915: &str, xe_name: &str| {
        read_num::<u32>(read(&card.dir.join(i915)).or_else(|| read(&xe.join(xe_name))))
    };
    NvidiaReading {
        usage: drm_usage.get(&card.pci).copied(),
        temp: None,
        freq: pick("gt_act_freq_mhz", "act_freq"),
        max_freq: pick("gt_RP0_freq_mhz", "rp0_freq"),
    }
}
