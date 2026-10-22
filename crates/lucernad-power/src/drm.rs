//! Os monitores externos pelo `class/drm` do sysfs: os conectores ligados, os
//! barramentos i2c candidatos de cada placa e o casamento pelo EDID.
//!
//! Só os barramentos de placa de vídeo (um ancestral com `class` `0x03*`)
//! são candidatos: o SMBus, o DesignWare do touchpad e os sensores nunca
//! entram, mesmo com permissão dada por outra regra.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::backlight::connector_name;
use crate::roots::{EDID_MAX, Roots, read_small};

/// Um conector ligado a um monitor externo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connector {
    /// O nome sem o `cardN-` (`HDMI-A-1`).
    pub name: String,
    /// A placa (`card1`).
    pub card: String,
    /// O dispositivo PCI da placa (canônico).
    pub pci: PathBuf,
    /// O barramento do link `ddc`, se o driver o expõe.
    pub ddc: Option<u32>,
    /// O EDID do sysfs (até [`EDID_MAX`]); vazio se ilegível.
    pub edid: Vec<u8>,
}

/// O resultado de casar o EDID de um conector com os lidos nos barramentos.
#[derive(Debug, PartialEq, Eq)]
pub enum BusMatch {
    Found(u32),
    NotFound,
    Ambiguous,
}

/// Os conectores que não são monitores externos (a tela integrada e os
/// de escrita).
const INTERNAL: [&str; 4] = ["eDP", "LVDS", "DSI", "Writeback"];

/// O tamanho do bloco base do EDID, o que se compara.
const EDID_BLOCK: usize = 128;

/// Os conectores com `status` `connected`, fora os internos, ordenados por
/// nome. Um conector cuja placa não leva a um dispositivo fica de fora.
pub fn connected(roots: &Roots) -> Vec<Connector> {
    let class = roots.sysfs.join("class/drm");
    let Ok(entries) = std::fs::read_dir(&class) else {
        return Vec::new();
    };
    let mut list: Vec<Connector> = entries
        .flatten()
        .filter_map(|entry| {
            let entry = entry.file_name().into_string().ok()?;
            let name = connector_name(&entry)?;
            if INTERNAL.iter().any(|prefix| name.starts_with(prefix)) {
                return None;
            }
            let dir = class.join(&entry);
            if read_small(&dir.join("status")).ok()? != "connected" {
                return None;
            }
            let card = entry[..entry.len() - name.len() - 1].to_string();
            let pci = class.join(&card).join("device").canonicalize().ok()?;
            Some(Connector {
                name: name.to_string(),
                card,
                pci,
                ddc: bus_of_link(&dir.join("ddc")),
                edid: read_edid(&dir.join("edid")),
            })
        })
        .collect();
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list
}

/// Os barramentos `i2c-dev` sob o dispositivo `pci` e de placa de vídeo,
/// fora os de `taken` (os `ddc` dos conectores e os já casados), ordenados.
pub fn candidate_buses(roots: &Roots, pci: &Path, taken: &[u32]) -> Vec<u32> {
    let pci = pci.canonicalize().unwrap_or_else(|_| pci.to_path_buf());
    let mut buses: Vec<u32> = i2c_dev_buses(roots)
        .into_iter()
        .filter(|bus| !taken.contains(bus))
        .filter(|bus| adapter(roots, *bus).is_some_and(|dir| dir.starts_with(&pci)))
        .filter(|bus| is_gpu_bus(roots, *bus))
        .collect();
    buses.sort_unstable();
    buses
}

/// Se o barramento tem um ancestral com `class` `0x03*` (placa de vídeo).
pub fn is_gpu_bus(roots: &Roots, bus: u32) -> bool {
    let Some(adapter) = adapter(roots, bus) else {
        return false;
    };
    let Ok(top) = roots.sysfs.canonicalize() else {
        return false;
    };
    adapter
        .ancestors()
        .take_while(|dir| dir.starts_with(&top) && *dir != top)
        .any(|dir| read_small(&dir.join("class")).is_ok_and(|class| class.starts_with("0x03")))
}

/// O nome do monitor no descritor 0xFC do EDID: até 13 bytes, até o `0x0A`,
/// só ASCII imprimível, sem espaços nas pontas. Vazio ou ausente → `None`.
pub fn edid_name(edid: &[u8]) -> Option<String> {
    if edid.len() < EDID_BLOCK {
        return None;
    }
    let descriptor = [54, 72, 90, 108].into_iter().find_map(|offset| {
        let d = &edid[offset..offset + 18];
        (d[..3] == [0, 0, 0] && d[3] == 0xFC).then_some(&d[5..18])
    })?;
    let text: String = descriptor
        .iter()
        .take_while(|b| **b != 0x0A)
        .filter(|b| (0x20..=0x7E).contains(*b))
        .map(|b| char::from(*b))
        .collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Se o módulo `i2c-dev` está carregado (`<sysfs>/class/i2c-dev` existe).
pub fn i2c_dev_loaded(roots: &Roots) -> bool {
    roots.sysfs.join("class/i2c-dev").is_dir()
}

/// Casa os 128 primeiros bytes do EDID do sysfs com os lidos em cada
/// barramento (`None` = não respondeu). Só um casamento vale: nenhum é
/// [`BusMatch::NotFound`] e mais de um, [`BusMatch::Ambiguous`] (nunca chutar).
pub fn match_edid(sysfs_edid: &[u8], read: &[(u32, Option<[u8; 128]>)]) -> BusMatch {
    let Some(base) = sysfs_edid.get(..EDID_BLOCK) else {
        return BusMatch::NotFound;
    };
    let mut found = read
        .iter()
        .filter(|(_, edid)| edid.as_ref().is_some_and(|edid| edid[..] == *base))
        .map(|(bus, _)| *bus);
    match (found.next(), found.next()) {
        (Some(bus), None) => BusMatch::Found(bus),
        (Some(_), Some(_)) => BusMatch::Ambiguous,
        (None, _) => BusMatch::NotFound,
    }
}

/// Os números dos barramentos em `<sysfs>/class/i2c-dev`.
fn i2c_dev_buses(roots: &Roots) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(roots.sysfs.join("class/i2c-dev")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| bus_number(entry.file_name().to_str()?))
        .collect()
}

/// A pasta (canônica) do adaptador do barramento.
fn adapter(roots: &Roots, bus: u32) -> Option<PathBuf> {
    roots
        .sysfs
        .join("class/i2c-dev")
        .join(format!("i2c-{bus}"))
        .join("device")
        .canonicalize()
        .ok()
}

/// O barramento para onde aponta um link (`ddc` → `i2c-14` → 14).
fn bus_of_link(link: &Path) -> Option<u32> {
    let target = std::fs::read_link(link).ok()?;
    bus_number(target.file_name()?.to_str()?)
}

/// `i2c-14` → 14.
fn bus_number(name: &str) -> Option<u32> {
    let digits = name.strip_prefix("i2c-")?;
    digits
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| digits.parse().ok())
        .flatten()
}

/// O EDID do sysfs, até [`EDID_MAX`]; vazio se ilegível.
fn read_edid(path: &Path) -> Vec<u8> {
    let mut edid = Vec::new();
    match File::open(path) {
        Ok(file) => match file.take(EDID_MAX).read_to_end(&mut edid) {
            Ok(_) => edid,
            Err(_) => Vec::new(),
        },
        Err(_) => Vec::new(),
    }
}
