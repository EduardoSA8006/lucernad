//! A detecção dos jogos pelo `/proc` (D8 do spec): o processo de uma janela,
//! ou um ancestral até [`ANCESTORS_MAX`] níveis, é jogo se tem `SteamAppId`
//! ou `SteamGameId` não vazio e diferente de `0`, se é o `gamescope`, se é
//! Wine (o executável `wine*-preloader`, `wine` ou `wine64`, ou o `argv[0]`
//! terminado em `.exe`) ou se é o `lutris-wrapper`.
//!
//! Só processos do próprio usuário (o `Uid` real do `status`) são lidos: a
//! subida para no primeiro processo de outro dono, sem abrir o `environ`
//! dele. Nada do que é lido é guardado além do "é jogo / não é".

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Quantos ancestrais, além do próprio processo, a detecção sobe.
pub const ANCESTORS_MAX: usize = 8;

/// De onde o `/proc` é lido e de quem são os processos que contam.
#[derive(Clone, Debug)]
pub struct ProcRoot {
    pub procfs: PathBuf,
    pub uid: u32,
}

impl ProcRoot {
    /// O `/proc` de verdade e o usuário do processo.
    pub fn system() -> ProcRoot {
        ProcRoot {
            procfs: PathBuf::from("/proc"),
            uid: rustix::process::getuid().as_raw(),
        }
    }

    /// Um `/proc` em `path` (os testes), com o usuário `uid`.
    pub fn at(path: &Path, uid: u32) -> ProcRoot {
        ProcRoot {
            procfs: path.to_path_buf(),
            uid,
        }
    }
}

/// O `Uid` real e o `PPid` do `status`.
fn status(dir: &Path) -> Option<(u32, i32)> {
    let text = std::fs::read_to_string(dir.join("status")).ok()?;
    let mut uid = None;
    let mut ppid = None;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("Uid:") {
            uid = v.split_whitespace().next().and_then(|u| u.parse().ok());
        } else if let Some(v) = line.strip_prefix("PPid:") {
            ppid = v.trim().parse().ok();
        }
    }
    Some((uid?, ppid?))
}

/// O último pedaço de um caminho (com `/` ou `\`).
fn base(s: &str) -> &str {
    s.rsplit(['/', '\\']).next().unwrap_or(s)
}

/// Se o processo da pasta `dir` (já conferido como do usuário) tem um dos
/// sinais de jogo.
fn has_signal(dir: &Path) -> bool {
    if let Ok(env) = std::fs::read(dir.join("environ")) {
        let steam = env.split(|b| *b == 0).any(|kv| {
            [&b"SteamAppId="[..], &b"SteamGameId="[..]].iter().any(|k| {
                kv.strip_prefix(*k)
                    .is_some_and(|v| !v.is_empty() && v != b"0")
            })
        });
        if steam {
            return true;
        }
    }
    let comm = std::fs::read_to_string(dir.join("comm")).unwrap_or_default();
    let comm = comm.trim_end();
    if comm == "gamescope" || comm == "lutris-wrapper" {
        return true;
    }
    if let Ok(exe) = std::fs::read_link(dir.join("exe")) {
        let name = exe.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let wine = name == "wine"
            || name == "wine64"
            || (name.starts_with("wine") && name.ends_with("-preloader"));
        if wine || name == "gamescope" {
            return true;
        }
    }
    if let Ok(cmdline) = std::fs::read(dir.join("cmdline")) {
        let argv0 = cmdline.split(|b| *b == 0).next().unwrap_or_default();
        let argv0 = String::from_utf8_lossy(argv0);
        if argv0.starts_with("lutris-wrapper") {
            return true;
        }
        let name = base(&argv0);
        if name.to_ascii_lowercase().ends_with(".exe") || name == "gamescope" {
            return true;
        }
    }
    false
}

/// Se o processo `pid` (ou um ancestral até [`ANCESTORS_MAX`]) é um jogo.
pub fn is_game(root: &ProcRoot, pid: i32) -> bool {
    let mut pid = pid;
    for _ in 0..=ANCESTORS_MAX {
        if pid <= 1 {
            return false;
        }
        let dir = root.procfs.join(pid.to_string());
        let Some((uid, ppid)) = status(&dir) else {
            return false;
        };
        if uid != root.uid {
            return false;
        }
        if has_signal(&dir) {
            return true;
        }
        pid = ppid;
    }
    false
}

/// O "é jogo" de cada pid, lido uma vez e guardado até o pid sumir.
#[derive(Default)]
pub struct GameCache {
    known: HashMap<i32, bool>,
}

impl GameCache {
    /// O guardado, ou lido agora.
    pub fn is_game(&mut self, root: &ProcRoot, pid: i32) -> bool {
        *self.known.entry(pid).or_insert_with(|| is_game(root, pid))
    }

    /// Esquece os pids fora de `alive`.
    pub fn retain(&mut self, alive: &HashSet<i32>) {
        self.known.retain(|pid, _| alive.contains(pid));
    }
}
