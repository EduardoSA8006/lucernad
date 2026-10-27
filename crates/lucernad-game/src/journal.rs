//! O diário do modo jogo (D13): a instância do Hyprland e, de cada opção
//! mudada, o valor de antes e o que o modo jogo pôs, em
//! `$XDG_RUNTIME_DIR/lucerna/game/restore.json` (pasta 0700). Gravado antes
//! de qualquer mudança no compositor e apagado depois da restauração.

use std::io;
use std::path::{Path, PathBuf};

use lucerna_paths::{AppPaths, Env, create_private_dir};
use lucernad_hyprland::options::OptionValue;
use serde::{Deserialize, Serialize};

/// O nome do arquivo do diário.
pub const FILE: &str = "restore.json";

/// Uma opção mudada.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub option: String,
    pub before: OptionValue,
    pub applied: OptionValue,
}

/// O que o modo jogo mudou numa instância do Hyprland.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Journal {
    /// A assinatura da instância.
    pub instance: String,
    pub entries: Vec<Entry>,
    /// As janelas com `immediate` ligado (o tearing), para desligar ao sair.
    #[serde(default)]
    pub tearing_windows: Vec<String>,
}

impl Journal {
    /// Se não há nada a desfazer.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.tearing_windows.is_empty()
    }
}

/// Onde o diário fica.
#[derive(Clone, Debug)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// O diário na pasta `dir`.
    pub fn new(dir: PathBuf) -> Store {
        Store { dir }
    }

    /// `$XDG_RUNTIME_DIR/lucerna/game` do ambiente do processo.
    pub fn from_env() -> Result<Store, String> {
        let env = Env::from_process();
        let paths = AppPaths::new("game", &env).map_err(|e| e.to_string())?;
        let dir = paths.runtime_dir().map_err(|e| e.to_string())?;
        Ok(Store::new(dir.join("game")))
    }

    /// O arquivo do diário.
    pub fn path(&self) -> PathBuf {
        self.dir.join(FILE)
    }

    /// O diário, se há um; o estragado é apagado (com um aviso no log).
    pub fn load(&self) -> Option<Journal> {
        let text = std::fs::read_to_string(self.path()).ok()?;
        match serde_json::from_str(&text) {
            Ok(j) => Some(j),
            Err(e) => {
                eprintln!("lucernad: o diário do modo jogo estava estragado ({e}): apagado");
                self.clear();
                None
            }
        }
    }

    /// Grava (atômico: um `.tmp` e o `rename`).
    pub fn save(&self, j: &Journal) -> io::Result<()> {
        create_private_dir(&self.dir)?;
        let tmp = self.dir.join(format!("{FILE}.tmp"));
        let text = serde_json::to_string_pretty(j).map_err(io::Error::other)?;
        write_synced(&tmp, text.as_bytes())?;
        std::fs::rename(&tmp, self.path())
    }

    /// Apaga (sem arquivo, nada acontece).
    pub fn clear(&self) {
        let _ = std::fs::remove_file(self.path());
    }
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}
