//! A tela integrada pelo `class/backlight` do sysfs.

use std::io;
use std::path::Path;

use crate::roots::{Roots, read_small};

/// O dispositivo de backlight escolhido.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backlight {
    /// O nome da pasta em `class/backlight` (`intel_backlight`).
    pub device: String,
    /// O `max_brightness`, maior que zero.
    pub max: u32,
    /// O conector do link `device` (`card2-eDP-1` → `eDP-1`), se houver.
    pub output: Option<String>,
}

/// A ordem de preferência dos tipos, como a do `brightnessctl`.
const KINDS: [&str; 3] = ["firmware", "platform", "raw"];

/// O backlight da máquina: `firmware`, depois `platform`, depois `raw`; no
/// mesmo tipo, o primeiro em ordem de nome. Um dispositivo de tipo
/// desconhecido ou com `max_brightness` 0 ou ilegível não vale.
pub fn choose(roots: &Roots) -> Option<Backlight> {
    let class = roots.sysfs.join("class/backlight");
    let mut found: Vec<(usize, String, u32)> = std::fs::read_dir(&class)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let device = entry.file_name().into_string().ok()?;
            let dir = class.join(&device);
            let kind = read_small(&dir.join("type")).ok()?;
            let rank = KINDS.iter().position(|k| *k == kind)?;
            let max = read_small(&dir.join("max_brightness"))
                .ok()?
                .parse::<u32>()
                .ok()
                .filter(|max| *max > 0)?;
            Some((rank, device, max))
        })
        .collect();
    found.sort();
    let (_, device, max) = found.into_iter().next()?;
    let output = output_of(&class.join(&device).join("device"));
    Some(Backlight {
        device,
        max,
        output,
    })
}

/// O valor pedido (`brightness`), não o `actual_brightness`: com o painel
/// desligado (a tampa fechada) o `actual_brightness` dá 0.
pub fn read_value(roots: &Roots, device: &str) -> io::Result<u32> {
    let path = roots
        .sysfs
        .join("class/backlight")
        .join(device)
        .join("brightness");
    read_small(&path)?
        .parse()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// O conector para onde aponta o link `device`, sem o `cardN-`.
fn output_of(link: &Path) -> Option<String> {
    let target = std::fs::read_link(link).ok()?;
    let name = target.file_name()?.to_str()?;
    connector_name(name).map(str::to_string)
}

/// `card2-eDP-1` → `eDP-1`; o que não tem essa forma → `None`.
pub(crate) fn connector_name(entry: &str) -> Option<&str> {
    let rest = entry.strip_prefix("card")?;
    let (number, name) = rest.split_once('-')?;
    let valid = !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit());
    (valid && !name.is_empty()).then_some(name)
}
