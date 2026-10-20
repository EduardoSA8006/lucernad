//! Discos: as montagens do `/proc/self/mountinfo`, a árvore de blocos do
//! `/sys/block` e o `statvfs` de cada origem, somados por disco físico.
//!
//! Repete o que o `SystemStats.qml` fazia com o `df -l` e o `lsblk`: os mesmos
//! discos, a mesma soma e a mesma ordem.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde::Serialize;
use tokio::task::JoinSet;

use crate::{Detached, Roots, detached};

/// Uma linha do `mountinfo`: a origem, o ponto de montagem e o tipo.
#[derive(Clone, Debug, PartialEq)]
pub struct Mount {
    pub source: String,
    pub target: String,
    pub fstype: String,
}

/// As montagens do texto de um `mountinfo`.
///
/// O alvo é o quinto campo; o tipo e a origem são os dois campos depois do
/// separador `-` (antes dele pode haver campos opcionais, como `shared:1`).
/// Os escapes octais do kernel (`\040` é o espaço) são desfeitos. Linhas
/// incompletas são ignoradas.
pub fn parse_mountinfo(text: &str) -> Vec<Mount> {
    text.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<Mount> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    let sep = fields.iter().position(|f| *f == "-")?;
    if sep < 6 || fields.len() < sep + 3 {
        return None;
    }
    Some(Mount {
        source: unescape(fields[sep + 2]),
        target: unescape(fields[4]),
        fstype: unescape(fields[sep + 1]),
    })
}

/// Desfaz os escapes `\NNN` (octal) do `mountinfo`.
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let octal = bytes.get(i + 1..i + 4).filter(|d| {
            bytes[i] == b'\\' && d.iter().all(|b| (b'0'..=b'7').contains(b)) && d[0] <= b'3'
        });
        match octal {
            Some(d) => {
                out.push((d[0] - b'0') * 64 + (d[1] - b'0') * 8 + (d[2] - b'0'));
                i += 4;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Sistemas de arquivos de rede: ficam fora, como no `df -l`.
pub const REMOTE_FS: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "smbfs",
    "ncpfs",
    "afs",
    "ceph",
    "glusterfs",
    "fuse.glusterfs",
    "9p",
    "fuse.sshfs",
    "sshfs",
    "fuse.rclone",
    "davfs",
];

/// Se a montagem entra na conta dos discos: origem em `/dev/` (o que não é
/// dispositivo, como `tmpfs`, `overlay` e os de rede, fica fora), sem `loop`,
/// `zram` e `ram` seguidos de dígito, e com um tipo fora de [`REMOTE_FS`].
pub fn is_candidate(m: &Mount) -> bool {
    let Some(dev) = m.source.strip_prefix("/dev/") else {
        return false;
    };
    let virtual_dev = ["loop", "zram", "ram"].iter().any(|p| {
        dev.strip_prefix(p)
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
    });
    !virtual_dev && !REMOTE_FS.contains(&m.fstype.as_str())
}

/// O tipo de um dispositivo de blocos.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Um disco físico (o fim da subida).
    Disk,
    /// Uma partição; o pai é o disco.
    Part,
    /// Outro dispositivo (device mapper, RAID, loop, zram…); o pai, se houver,
    /// é o último nome de `slaves/`.
    Other,
}

/// Um dispositivo da árvore de blocos.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub kind: Kind,
    pub parent: Option<String>,
}

/// Quantos passos [`BlockTree::disk_of`] sobe pelos pais antes de desistir.
const MAX_STEPS: usize = 8;

/// A árvore de blocos: cada dispositivo pelo nome de kernel (`nvme0n1p2`,
/// `dm-0`), mais os nomes do device mapper (`root` → `dm-0`).
#[derive(Clone, Debug, Default)]
pub struct BlockTree {
    nodes: HashMap<String, Node>,
    /// Nome do mapper → nome de kernel.
    dm: HashMap<String, String>,
}

impl BlockTree {
    /// A árvore lida do `<sysfs>/block`; vazia se ele não puder ser lido.
    ///
    /// As entradas com a pasta `dm`, ou cujo nome começa por `loop`, `zram`,
    /// `ram`, `sr` ou `md`, são [`Kind::Other`], com o pai = o último nome de
    /// `slaves/`; as outras são [`Kind::Disk`] se a `slaves/` estiver ausente ou
    /// vazia (no sysfs real, todo disco tem a pasta, vazia), e [`Kind::Other`]
    /// se não. Cada subpasta com o arquivo `partition` é uma [`Kind::Part`]
    /// da entrada. Nomes começando com ponto são ignorados.
    pub fn read(roots: &Roots) -> BlockTree {
        let block = roots.sysfs.join("block");
        let mut nodes = Vec::new();
        let mut dm_names = Vec::new();
        for name in list_names(&block) {
            let dir = block.join(&name);
            if !dir.is_dir() {
                continue;
            }
            let is_dm = dir.join("dm").is_dir();
            if let Some(dm_name) = crate::read(&dir.join("dm/name")) {
                dm_names.push((name.clone(), dm_name.trim().to_string()));
            }
            let parent = list_names(&dir.join("slaves")).pop();
            let by_name = ["loop", "zram", "ram", "sr", "md"]
                .iter()
                .any(|p| name.starts_with(p));
            let kind = if is_dm || by_name || parent.is_some() {
                Kind::Other
            } else {
                Kind::Disk
            };
            for part in list_names(&dir) {
                if dir.join(&part).join("partition").is_file() {
                    let node = Node {
                        kind: Kind::Part,
                        parent: Some(name.clone()),
                    };
                    nodes.push((part, node));
                }
            }
            let parent = if kind == Kind::Other { parent } else { None };
            nodes.push((name, Node { kind, parent }));
        }
        BlockTree::from_nodes(nodes, dm_names)
    }

    /// Uma árvore pronta: os dispositivos pelo nome de kernel e os pares
    /// (nome de kernel, nome do mapper), como `("dm-0", "root")`.
    pub fn from_nodes(
        nodes: impl IntoIterator<Item = (String, Node)>,
        dm_names: impl IntoIterator<Item = (String, String)>,
    ) -> BlockTree {
        BlockTree {
            nodes: nodes.into_iter().collect(),
            dm: dm_names
                .into_iter()
                .map(|(kernel, name)| (name, kernel))
                .collect(),
        }
    }

    /// O nome de kernel de uma origem: `/dev/mapper/<n>` → o `dm-N` cujo
    /// `dm/name` é `<n>` (ou `<n>`, se nenhum for); outra origem → o último
    /// componente do caminho.
    pub fn kernel_name(&self, source: &str) -> String {
        if let Some(name) = source.strip_prefix("/dev/mapper/") {
            return self.dm.get(name).cloned().unwrap_or_else(|| name.into());
        }
        source.rsplit('/').next().unwrap_or(source).to_string()
    }

    /// O disco físico de uma origem (`/dev/nvme0n1p2` → `nvme0n1`), subindo
    /// pelos pais (LUKS, LVM, RAID) até um [`Kind::Disk`], em até 8 passos.
    /// Fora da árvore no primeiro passo, o próprio nome (`/dev/root` numa VM);
    /// fora dela depois, ou sem pai antes de um disco, nenhum.
    pub fn disk_of(&self, source: &str) -> Option<String> {
        let mut dev = self.kernel_name(source);
        for step in 0..MAX_STEPS {
            let Some(node) = self.nodes.get(&dev) else {
                return (step == 0).then_some(dev);
            };
            if node.kind == Kind::Disk {
                return Some(dev);
            }
            dev = node.parent.clone()?;
        }
        None
    }
}

/// Os nomes de uma pasta, em ordem, sem os que começam com ponto; vazio se
/// ela não existir.
fn list_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();
    names
}

/// O tamanho e o usado de um sistema de arquivos, em bytes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Usage {
    pub size: u64,
    pub used: u64,
}

/// Um disco físico do tópico `system.disks`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Disk {
    pub name: String,
    pub size: u64,
    pub used: u64,
    pub root: bool,
}

/// Os discos físicos das montagens, com o uso de cada origem (`usages`, pela
/// origem).
///
/// Cada origem conta uma vez, mesmo montada em vários lugares (bind mounts,
/// subvolumes do btrfs); as de um disco são somadas; uma origem montada em
/// `/` marca o disco como o da raiz. Uma origem sem uso no mapa (o `statvfs`
/// falhou ou não respondeu) fica fora da soma e não marca a raiz. Os da raiz
/// vêm primeiro; os outros, na ordem das montagens.
pub fn aggregate(mounts: &[Mount], tree: &BlockTree, usages: &HashMap<String, Usage>) -> Vec<Disk> {
    let mut seen = HashSet::new();
    let mut list: Vec<Disk> = Vec::new();
    for m in mounts.iter().filter(|m| is_candidate(m)) {
        let Some(usage) = usages.get(&m.source) else {
            continue;
        };
        let Some(name) = tree.disk_of(&m.source) else {
            continue;
        };
        let root = m.target == "/";
        let index = match list.iter().position(|d| d.name == name) {
            Some(i) => i,
            None => {
                list.push(Disk {
                    name,
                    size: 0,
                    used: 0,
                    root: false,
                });
                list.len() - 1
            }
        };
        let disk = &mut list[index];
        disk.root |= root;
        if seen.insert(m.source.as_str()) {
            disk.size += usage.size;
            disk.used += usage.used;
        }
    }
    let (mut roots, others): (Vec<Disk>, Vec<Disk>) = list.into_iter().partition(|d| d.root);
    roots.extend(others);
    roots
}

/// O `statvfs`, atrás de um trait para os testes.
pub trait Statfs: Send + Sync + 'static {
    /// O uso do sistema de arquivos montado em `path`.
    fn usage(&self, path: &Path) -> io::Result<Usage>;
}

/// O `statvfs` de verdade (`rustix`): `size = f_blocks × f_frsize` e
/// `used = (f_blocks − f_bfree) × f_frsize`, o "Usado" do `df`.
pub struct RealStatfs;

impl Statfs for RealStatfs {
    fn usage(&self, path: &Path) -> io::Result<Usage> {
        let st = rustix::fs::statvfs(path)?;
        Ok(Usage {
            size: st.f_blocks.saturating_mul(st.f_frsize),
            used: st
                .f_blocks
                .saturating_sub(st.f_bfree)
                .saturating_mul(st.f_frsize),
        })
    }
}

/// Lê o uso das montagens sem deixar uma montagem travada segurar a leitura.
///
/// Cada `statvfs` roda numa thread solta, com prazo; um alvo cuja chamada
/// anterior ainda não voltou não é pedido de novo, para não acumular threads
/// presas na mesma montagem.
pub struct DiskReader {
    statfs: Arc<dyn Statfs>,
    timeout: Duration,
    /// Os alvos com uma chamada em andamento; a thread tira o seu ao terminar.
    pending: Arc<Mutex<HashSet<PathBuf>>>,
}

/// Tira o alvo de `pending` ao sair (inclusive num pânico do `statvfs`).
struct PendingGuard {
    pending: Arc<Mutex<HashSet<PathBuf>>>,
    path: PathBuf,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        lock(&self.pending).remove(&self.path);
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl DiskReader {
    /// Um leitor com o `statvfs` dado e o prazo de cada chamada.
    pub fn new(statfs: Arc<dyn Statfs>, timeout: Duration) -> DiskReader {
        DiskReader {
            statfs,
            timeout,
            pending: Arc::default(),
        }
    }

    /// O uso de cada origem candidata ([`is_candidate`]), pela origem.
    ///
    /// Um `statvfs` por origem, no primeiro alvo dela, todos ao mesmo tempo,
    /// cada um com o prazo. A origem que dá erro, estoura o prazo ou ainda
    /// tem a chamada anterior em andamento fica fora do mapa.
    pub async fn usages(&self, mounts: &[Mount]) -> HashMap<String, Usage> {
        let mut sources = HashSet::new();
        let mut calls = JoinSet::new();
        for m in mounts.iter().filter(|m| is_candidate(m)) {
            if !sources.insert(m.source.as_str()) {
                continue;
            }
            let path = PathBuf::from(&m.target);
            if !lock(&self.pending).insert(path.clone()) {
                continue;
            }
            let guard = PendingGuard {
                pending: self.pending.clone(),
                path: path.clone(),
            };
            let statfs = self.statfs.clone();
            let source = m.source.clone();
            let timeout = self.timeout;
            calls.spawn(async move {
                let call = move || {
                    let _guard = guard;
                    statfs.usage(&path)
                };
                (source, detached(call, timeout).await)
            });
        }
        let mut out = HashMap::new();
        while let Some(joined) = calls.join_next().await {
            if let Ok((source, Detached::Done(Ok(usage)))) = joined {
                out.insert(source, usage);
            }
        }
        out
    }
}

/// Os discos de agora: o `mountinfo` do processo, a árvore de blocos e o uso
/// de cada origem.
pub async fn read_disks(roots: &Roots, reader: &DiskReader) -> Vec<Disk> {
    let text = crate::read(&roots.procfs.join("self/mountinfo")).unwrap_or_default();
    let mounts = parse_mountinfo(&text);
    let tree = BlockTree::read(roots);
    let usages = reader.usages(&mounts).await;
    aggregate(&mounts, &tree, &usages)
}
