//! Temperatura da CPU pelo hwmon.

use std::path::{Path, PathBuf};

use crate::{Roots, read};

/// Os sensores do pacote da CPU, na ordem de preferência.
pub const PREFERRED: [&str; 5] = ["coretemp", "k10temp", "zenpower", "cpu_thermal", "acpitz"];

/// O `temp1_input` do primeiro sensor de [`PREFERRED`] (na ordem da lista)
/// entre os `(pasta, nome)` dos chips.
pub fn pick_sensor(chips: &[(PathBuf, String)]) -> Option<PathBuf> {
    PREFERRED.iter().find_map(|want| {
        chips
            .iter()
            .find(|(_, name)| name == want)
            .map(|(dir, _)| dir.join("temp1_input"))
    })
}

/// Escolhe o sensor entre os `class/hwmon/hwmon*` (em ordem de nome).
pub fn find_sensor(roots: &Roots) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(roots.sysfs.join("class/hwmon"))
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("hwmon"))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    let chips: Vec<(PathBuf, String)> = dirs
        .into_iter()
        .filter_map(|dir| {
            let name = read(&dir.join("name"))?.trim().to_string();
            Some((dir, name))
        })
        .collect();
    pick_sensor(&chips)
}

/// A temperatura em °C (o arquivo traz milésimos); `None` sem leitura.
pub fn read_temp(path: &Path) -> Option<f64> {
    let milli: f64 = read(path)?.trim().parse().ok()?;
    Some(milli / 1000.0)
}
