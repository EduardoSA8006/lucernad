//! O diário do modo jogo (D13): a instância do Hyprland e, de cada opção
//! mudada, o valor de antes e o que o modo jogo pôs, em
//! `$XDG_RUNTIME_DIR/lucerna/game/restore.json` (pasta 0700). Gravado antes
//! de qualquer mudança no compositor e apagado depois da restauração.

use std::io;
use std::path::{Path, PathBuf};

use lucerna_paths::{AppPaths, Env, create_private_dir};
use lucernad_hyprland::options::OptionValue;
use serde::{Deserialize, Serialize};

use crate::compositor::OPTIONS;

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
        match serde_json::from_str::<Journal>(&text) {
            Ok(mut j) => {
                // Só as opções que o modo jogo toca: um nome estranho no
                // arquivo nunca vira pedido ao Hyprland.
                let before = j.entries.len();
                j.entries.retain(|e| OPTIONS.contains(&e.option.as_str()));
                if j.entries.len() != before {
                    eprintln!(
                        "lucernad: o diário do modo jogo tinha opções desconhecidas: ignoradas"
                    );
                }
                Some(j)
            }
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

/// Grava com modo 0600 (o `.tmp` sobrado de antes é trocado).
fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::remove_file(path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}
