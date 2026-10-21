//! O casamento do player (nome D-Bus do MPRIS) com os nós de saída do
//! PipeWire, pelo PID do dono do nome e pela árvore de processos do `/proc`.
//!
//! As regras, na ordem: o nó é do player se o PID do client é o do dono do
//! nome ou um descendente dele (a busca parte do pai quando o dono é uma
//! ponte conhecida); sem o dono no `/proc` (ou com o proxy do Flatpak), a reserva compara o executável do
//! client com o nome do player, só com igualdade exata e só se todos os nós
//! que casam vierem de um mesmo processo.

use std::fs;
use std::path::{Path, PathBuf};

use crate::backend::OutputNode;
use crate::dbus::Identity;

/// As pontes de MPRIS conhecidas, pelo nome do executável: quando o dono do
/// nome é uma delas, a busca parte do pai (o app). Lista fixa: subir pelo pai
/// de qualquer dono casaria quase todos os fluxos quando o pai é o
/// `systemd --user` ou o compositor.
pub const BRIDGES: &[&str] = &["plasma-browser-integration-host"];

/// Quantos passos o [`is_descendant`] sobe no máximo pelos `ppid`.
pub const MAX_DEPTH: usize = 64;

/// O executável das raízes genéricas, das quais a busca por PID não parte.
const GENERIC_ROOT: &str = "systemd";

/// O executável que é dono dos nomes D-Bus de um app Flatpak: está no
/// `/proc` do host, mas é irmão do app, não ancestral.
const FLATPAK_PROXY: &str = "xdg-dbus-proxy";

/// O prefixo dos nomes D-Bus dos players MPRIS.
const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";

/// Um processo, no que o casamento precisa.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Process {
    /// O PID do pai.
    pub ppid: u32,
    /// O nome do arquivo do executável.
    pub exe: String,
}

/// A árvore de processos.
pub trait Processes {
    /// O processo `pid`, ou `None` se ele não existe (ou não dá para ler).
    fn get(&self, pid: u32) -> Option<Process>;
}

/// A árvore de processos lida de um `/proc` (ou de uma cópia nos testes).
pub struct ProcFs {
    root: PathBuf,
}

impl ProcFs {
    /// Lê os processos de `procfs` (normalmente `/proc`).
    pub fn new(procfs: PathBuf) -> ProcFs {
        ProcFs { root: procfs }
    }
}

impl Processes for ProcFs {
    fn get(&self, pid: u32) -> Option<Process> {
        let dir = self.root.join(pid.to_string());
        let stat = fs::read_to_string(dir.join("stat")).ok()?;
        let ppid = parse_ppid(&stat)?;
        Some(Process {
            ppid,
            exe: exe_name(&dir).unwrap_or_default(),
        })
    }
}

/// O nome do executável: o do arquivo apontado por `exe` e, sem ele, o do
/// primeiro argumento do `cmdline`. O `comm` não serve: corta em 15
/// caracteres (`plasma-browser-`).
fn exe_name(dir: &Path) -> Option<String> {
    let path = match fs::read_link(dir.join("exe")) {
        Ok(path) => path,
        Err(_) => {
            let cmdline = fs::read(dir.join("cmdline")).ok()?;
            let first = cmdline.split(|&b| b == 0).next()?;
            PathBuf::from(String::from_utf8_lossy(first).into_owned())
        }
    };
    let name = path.file_name()?.to_string_lossy().into_owned();
    (!name.is_empty()).then_some(name)
}

/// O `ppid` de uma linha do `<pid>/stat`: o segundo campo depois do último
/// `)` (o `comm` pode ter espaços e parênteses).
pub fn parse_ppid(stat: &str) -> Option<u32> {
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// Se `pid` é `ancestor` ou um descendente dele, subindo pelos `ppid` no
/// máximo [`MAX_DEPTH`] passos e parando em `pid ≤ 1` ou num processo que
/// falta.
pub fn is_descendant(procs: &dyn Processes, pid: u32, ancestor: u32) -> bool {
    let mut current = pid;
    for _ in 0..=MAX_DEPTH {
        if current == ancestor {
            return true;
        }
        if current <= 1 {
            return false;
        }
        match procs.get(current) {
            Some(process) => current = process.ppid,
            None => return false,
        }
    }
    false
}

/// De onde a busca por PID parte: o dono do nome, ou o pai dele se o dono é
/// uma ponte ([`BRIDGES`]). `None` quando essa raiz é genérica: `pid ≤ 1` ou
/// um processo cujo executável é o `systemd` (o init ou o `systemd --user`,
/// que adota as pontes órfãs), porque descender dela casaria quase tudo. Um
/// dono ausente do `/proc` vale como raiz, para a regra do PID igual
/// funcionar sem o `/proc` (como no container).
pub fn search_root(procs: &dyn Processes, owner: u32) -> Option<u32> {
    let root = match procs.get(owner) {
        Some(process) if BRIDGES.contains(&process.exe.as_str()) => process.ppid,
        _ => owner,
    };
    if root <= 1 || procs.get(root).is_some_and(|p| p.exe == GENERIC_ROOT) {
        return None;
    }
    Some(root)
}

/// Os nomes do player para a reserva: o fim do nome D-Bus depois de
/// `org.mpris.MediaPlayer2.`, sem um último componente que comece com
/// `instance`, e o `DesktopEntry`; sem repetidos. Vazio se o nome não tem o
/// prefixo.
pub fn player_names(dbus_name: &str, desktop_entry: Option<&str>) -> Vec<String> {
    let Some(rest) = dbus_name.strip_prefix(MPRIS_PREFIX) else {
        return Vec::new();
    };
    let base = match rest.rsplit_once('.') {
        Some((head, last)) if last.starts_with("instance") => head,
        _ => rest,
    };
    let mut names = Vec::new();
    for name in [Some(base), desktop_entry].into_iter().flatten() {
        if !name.is_empty() && !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    }
    names
}

/// Os `object.serial` dos nós do player `id`, na ordem de `nodes`.
pub fn match_nodes(id: &Identity, nodes: &[OutputNode], procs: &dyn Processes) -> Vec<u64> {
    if let Some(root) = id.owner_pid.and_then(|owner| search_root(procs, owner)) {
        let matched: Vec<u64> = nodes
            .iter()
            .filter(|n| n.client_pid.is_some_and(|p| is_descendant(procs, p, root)))
            .map(|n| n.serial)
            .collect();
        if !matched.is_empty() {
            return matched;
        }
    }

    // A reserva só vale quando não dá para seguir o dono pelo /proc (sem PID,
    // fora dele, como no container, ou o proxy do Flatpak, irmão do app): com
    // o dono visível, ela casaria o nó de outra instância do mesmo app.
    let visible = |owner| procs.get(owner).is_some_and(|p| p.exe != FLATPAK_PROXY);
    if id.owner_pid.is_some_and(visible) {
        return Vec::new();
    }
    let names = player_names(&id.name, id.desktop_entry.as_deref());
    let matched: Vec<&OutputNode> = nodes
        .iter()
        .filter(|n| n.binary.as_ref().is_some_and(|b| names.contains(b)))
        .collect();
    let one_process = matched.first().is_some_and(|first| {
        first.client_pid.is_some() && matched.iter().all(|n| n.client_pid == first.client_pid)
    });
    if one_process {
        matched.iter().map(|n| n.serial).collect()
    } else {
        Vec::new()
    }
}
