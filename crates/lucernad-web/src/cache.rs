//! Cache em disco, genérico, de JSON: um arquivo por chave em
//! `<pasta>/{lyrics,weather}/<sha256 da chave>.json`, com `{savedAt, value}`.
//!
//! A escrita é atômica (`.tmp-*` e `rename`), a leitura é limitada a
//! [`ENTRY_MAX`] bytes, o arquivo estragado é apagado e, depois de cada
//! escrita, os mais antigos (pelo `mtime`) saem até o total caber no limite.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lucerna_paths::create_private_dir;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// O tamanho máximo de todo o cache, em bytes.
pub const CACHE_LIMIT: u64 = 50 * 1024 * 1024;
/// O tamanho máximo de uma entrada, em bytes.
pub const ENTRY_MAX: usize = 512 * 1024;

/// Depois de quanto tempo um `.tmp-*` abandonado é apagado.
const TMP_MAX_AGE: Duration = Duration::from_secs(3600);
const TMP_PREFIX: &str = ".tmp-";

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// O que a entrada guarda; cada tipo tem a sua pasta.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Lyrics,
    Weather,
}

impl Kind {
    const ALL: [Kind; 2] = [Kind::Lyrics, Kind::Weather];

    fn dir_name(self) -> &'static str {
        match self {
            Kind::Lyrics => "lyrics",
            Kind::Weather => "weather",
        }
    }
}

/// O cache em uma pasta. A pasta só é criada na primeira escrita.
#[derive(Clone, Debug)]
pub struct Cache {
    dir: PathBuf,
    limit: u64,
}

impl Cache {
    pub fn new(dir: PathBuf, limit: u64) -> Cache {
        Cache { dir, limit }
    }

    /// O arquivo da chave: o nome é o SHA-256 dela em hexadecimal.
    pub fn path(&self, kind: Kind, key: &str) -> PathBuf {
        let hash: String = Sha256::digest(key.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        self.dir.join(kind.dir_name()).join(format!("{hash}.json"))
    }

    /// Lê a entrada: quando foi guardada (com resolução de segundo) e o valor.
    /// Arquivo estragado, grande demais ou sem `savedAt` vale como ausente e
    /// é apagado.
    pub fn get(&self, kind: Kind, key: &str) -> Option<(SystemTime, Value)> {
        let path = self.path(kind, key);
        let mut file = match File::open(&path) {
            Ok(f) => f,
            Err(_) => return None,
        };
        let mut bytes = Vec::new();
        let read = (&mut file)
            .take(ENTRY_MAX as u64 + 1)
            .read_to_end(&mut bytes);
        let parsed = match read {
            Ok(_) if bytes.len() <= ENTRY_MAX => parse(&bytes),
            _ => None,
        };
        if parsed.is_none() {
            let _ = fs::remove_file(&path);
        }
        parsed
    }

    /// Guarda a entrada e poda. Maior que [`ENTRY_MAX`]: não grava e volta
    /// `Ok`.
    pub fn put(&self, kind: Kind, key: &str, value: &Value, now: SystemTime) -> io::Result<()> {
        let saved_at = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let bytes = serde_json::to_vec(&json!({"savedAt": saved_at, "value": value}))
            .map_err(io::Error::other)?;
        if bytes.len() > ENTRY_MAX {
            return Ok(());
        }
        let path = self.path(kind, key);
        let dir = path.parent().expect("o arquivo tem pasta");
        create_private_dir(&self.dir)?;
        create_private_dir(dir)?;
        let tmp = dir.join(format!(
            "{TMP_PREFIX}{}-{}",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let written = write_file(&tmp, &bytes).and_then(|()| fs::rename(&tmp, &path));
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        written?;
        self.prune();
        Ok(())
    }

    /// Apaga a entrada, se existir.
    pub fn remove(&self, kind: Kind, key: &str) {
        let _ = fs::remove_file(self.path(kind, key));
    }

    /// Apaga os `.tmp-*` com mais de 1 h e, dos `.json` das duas pastas, os
    /// mais antigos até o total caber no limite.
    fn prune(&self) {
        let now = SystemTime::now();
        let mut entries: Vec<(SystemTime, PathBuf, u64)> = Vec::new();
        for kind in Kind::ALL {
            let Ok(read) = fs::read_dir(self.dir.join(kind.dir_name())) else {
                continue;
            };
            for entry in read.flatten() {
                let Ok(md) = entry.metadata() else { continue };
                if !md.is_file() {
                    continue;
                }
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let mtime = md.modified().unwrap_or(UNIX_EPOCH);
                if name.starts_with(TMP_PREFIX) {
                    let age = now.duration_since(mtime).unwrap_or_default();
                    if age > TMP_MAX_AGE {
                        let _ = fs::remove_file(entry.path());
                    }
                } else if name.ends_with(".json") {
                    entries.push((mtime, entry.path(), md.len()));
                }
            }
        }
        let mut total: u64 = entries.iter().map(|e| e.2).sum();
        if total <= self.limit {
            return;
        }
        entries.sort();
        for (_, path, len) in entries {
            if total <= self.limit {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total -= len;
            }
        }
    }
}

fn parse(bytes: &[u8]) -> Option<(SystemTime, Value)> {
    let mut doc: Value = serde_json::from_slice(bytes).ok()?;
    let saved_at = doc.get("savedAt")?.as_u64()?;
    let value = doc.get_mut("value")?.take();
    Some((UNIX_EPOCH + Duration::from_secs(saved_at), value))
}

fn write_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}
