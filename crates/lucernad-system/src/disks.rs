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

use lucernad_core::{Detached, Spawn, detached_with, spawn_thread};

use crate::Roots;

/// Uma linha do `mountinfo`: o `maj:min`, a origem, o ponto de montagem e o
/// tipo.
#[derive(Clone, Debug, PartialEq)]
pub struct Mount {
    /// O `maj:min` do dispositivo (o terceiro campo), como `8:2`; anônimo
    /// (`0:N`) no btrfs e nos virtuais.
    pub dev: String,
    pub source: String,
    pub target: String,
    pub fstype: String,
}

/// As montagens do texto de um `mountinfo`.
///
/// O `maj:min` é o terceiro campo; o alvo, o quinto; o tipo e a origem são os dois campos depois do
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
        dev: fields[2].to_string(),
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
/// `zram` e `ram` seguidos de dígito, com um tipo fora de [`REMOTE_FS`] e que
/// não seja FUSE (`fuse`, `fuse.*`): sem ser root, qualquer usuário monta um
/// FUSE com a origem que quiser (`fsname=/dev/x`). O `fuseblk` (o `ntfs-3g`,
/// sobre um bloco de verdade) conta; o [`DiskReader`] ainda exige a origem
/// na [`BlockTree`].
pub fn is_candidate(m: &Mount) -> bool {
    let Some(dev) = m.source.strip_prefix("/dev/") else {
        return false;
    };
    let virtual_dev = ["loop", "zram", "ram"].iter().any(|p| {
        dev.strip_prefix(p)
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
    });
    let fuse = m.fstype.starts_with("fuse") && m.fstype != "fuseblk";
    !virtual_dev && !fuse && !REMOTE_FS.contains(&m.fstype.as_str())
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
    /// `maj:min` → nome de kernel, dos links de `<sysfs>/dev/block`.
    devs: HashMap<String, String>,
}

impl BlockTree {
    /// A árvore lida do `<sysfs>/block`; vazia se ele não puder ser lido.
    ///
    /// As entradas com a pasta `dm`, ou cujo nome começa por `loop`, `zram`,
    /// `ram`, `sr` ou `md`, são [`Kind::Other`], com o pai = o último nome de
    /// `slaves/`; as outras são [`Kind::Disk`] se a `slaves/` estiver ausente ou
    /// vazia (no sysfs real, todo disco tem a pasta, vazia), e [`Kind::Other`]
    /// se não. Cada subpasta com o arquivo `partition` é uma [`Kind::Part`]
    /// da entrada. Nomes começando com ponto são ignorados. Os links de
    /// `<sysfs>/dev/block/<maj:min>` dão o nome de kernel de cada `maj:min`
    /// (o último componente do destino), para [`BlockTree::resolve`].
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
        let by_dev = roots.sysfs.join("dev/block");
        let devs = list_names(&by_dev)
            .into_iter()
            .filter_map(|dev| {
                let target = std::fs::read_link(by_dev.join(&dev)).ok()?;
                let name = target.file_name()?.to_str()?.to_string();
                Some((dev, name))
            })
            .collect();
        BlockTree {
            devs,
            ..BlockTree::from_nodes(nodes, dm_names)
        }
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
            devs: HashMap::new(),
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

    /// Se a origem é um bloco da árvore (pelo [`BlockTree::kernel_name`]).
    pub fn contains(&self, source: &str) -> bool {
        self.nodes.contains_key(&self.kernel_name(source))
    }

    /// As montagens com a origem trocada pelo bloco do `maj:min` quando o
    /// nome não está na árvore: o `/dev/root` de uma VM vira `/dev/sda2` (e
    /// soma no disco dele). Uma origem cujo `maj:min` também não leva a um nó
    /// da árvore fica como está (e o [`DiskReader`] não a pede).
    pub fn resolve(&self, mut mounts: Vec<Mount>) -> Vec<Mount> {
        for m in &mut mounts {
            if self.contains(&m.source) {
                continue;
            }
            let node = self
                .devs
                .get(&m.dev)
                .filter(|n| self.nodes.contains_key(*n));
            if let Some(name) = node {
                m.source = format!("/dev/{name}");
            }
        }
        mounts
    }

    /// O disco físico de uma origem (`/dev/nvme0n1p2` → `nvme0n1`), subindo
    /// pelos pais (LUKS, LVM, RAID) até um [`Kind::Disk`], em até 8 passos.
    /// Fora da árvore (a origem ou um pai), ou sem pai antes de um disco,
    /// nenhum: o `/dev/root` chega aqui já resolvido pelo maj:min
    /// ([`BlockTree::resolve`]).
    pub fn disk_of(&self, source: &str) -> Option<String> {
        let mut dev = self.kernel_name(source);
        for _ in 0..MAX_STEPS {
            let node = self.nodes.get(&dev)?;
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

/// Quantas chamadas do `statvfs` podem estar presas ao mesmo tempo. Acima
/// disso, uma montagem nova conta como prazo estourado, sem thread nova.
pub const MAX_PENDING: usize = 16;

/// Uma chamada do `statvfs`: a origem e o alvo em que ela é pedida.
type Key = (String, PathBuf);

/// Lê o uso das montagens sem deixar uma montagem travada segurar a leitura.
///
/// Cada `statvfs` roda numa thread solta, com prazo; uma montagem (origem e
/// alvo) cuja chamada anterior ainda não voltou não é pedida de novo, e no
/// máximo [`MAX_PENDING`] ficam presas ao mesmo tempo, para não acumular
/// threads.
pub struct DiskReader {
    statfs: Arc<dyn Statfs>,
    timeout: Duration,
    spawn: Spawn,
    /// As chamadas em andamento; a thread tira a sua ao terminar.
    pending: Arc<Mutex<HashSet<Key>>>,
    /// As montagens travadas já avisadas no log, até responderem de novo.
    warned: Mutex<HashSet<Key>>,
}

/// Tira a chamada de `pending` ao sair (inclusive num pânico do `statvfs`
/// ou quando a thread não chega a ser criada).
struct PendingGuard {
    pending: Arc<Mutex<HashSet<Key>>>,
    key: Key,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        lock(&self.pending).remove(&self.key);
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
            spawn: spawn_thread,
            pending: Arc::default(),
            warned: Mutex::default(),
        }
    }

    /// O uso de cada origem candidata ([`is_candidate`]) que é um bloco de
    /// `tree` ([`BlockTree::contains`]), pela origem. Uma origem que o kernel
    /// não conhece como bloco (um FUSE com `fsname=/dev/…`) não gasta uma
    /// vaga do teto de pendentes.
    ///
    /// Um `statvfs` por origem, no primeiro alvo dela, todos ao mesmo tempo,
    /// cada um com o prazo. A origem que dá erro, estoura o prazo, ainda tem
    /// a chamada anterior em andamento ou passa do teto de pendentes fica
    /// fora do mapa. Uma montagem travada é avisada uma vez no log.
    pub async fn usages(&self, mounts: &[Mount], tree: &BlockTree) -> HashMap<String, Usage> {
        let mut sources = HashSet::new();
        let mut calls = JoinSet::new();
        for m in mounts
            .iter()
            .filter(|m| is_candidate(m) && tree.contains(&m.source))
        {
            if !sources.insert(m.source.as_str()) {
                continue;
            }
            let key: Key = (m.source.clone(), PathBuf::from(&m.target));
            {
                let mut pending = lock(&self.pending);
                if pending.contains(&key) {
                    continue;
                }
                if pending.len() >= MAX_PENDING {
                    drop(pending);
                    self.warn(
                        &key,
                        &format!("não foi pedido: já há {MAX_PENDING} montagens sem responder"),
                    );
                    continue;
                }
                pending.insert(key.clone());
            }
            let guard = PendingGuard {
                pending: self.pending.clone(),
                key: key.clone(),
            };
            let statfs = self.statfs.clone();
            let (spawn, timeout) = (self.spawn, self.timeout);
            calls.spawn(async move {
                let path = key.1.clone();
                let call = move || {
                    let _guard = guard;
                    statfs.usage(&path)
                };
                (key, detached_with(spawn, call, timeout).await)
            });
        }
        let mut out = HashMap::new();
        while let Some(joined) = calls.join_next().await {
            let Ok((key, outcome)) = joined else {
                continue;
            };
            match outcome {
                Detached::Done(result) => {
                    lock(&self.warned).remove(&key);
                    if let Ok(usage) = result {
                        out.insert(key.0, usage);
                    }
                }
                Detached::Panicked(_) => {
                    lock(&self.warned).remove(&key);
                }
                Detached::TimedOut => {
                    let msg = format!("não respondeu em {:?}", self.timeout);
                    self.warn(&key, &msg);
                }
                Detached::SpawnFailed(e) => {
                    self.warn(
                        &key,
                        &format!("não foi pedido: a thread não pôde ser criada ({e})"),
                    );
                }
            }
        }
        // Esquece os avisos de montagens que não existem mais.
        let current: HashSet<(&str, &Path)> = mounts
            .iter()
            .map(|m| (m.source.as_str(), Path::new(&m.target)))
            .collect();
        lock(&self.warned)
            .retain(|(source, target)| current.contains(&(source.as_str(), target.as_path())));
        out
    }

    /// Avisa no log, uma vez por montagem, que ela ficou fora dos discos.
    fn warn(&self, key: &Key, why: &str) {
        if lock(&self.warned).insert(key.clone()) {
            eprintln!(
                "lucernad: o statvfs de {:?} ({:?}) {why}; ela fica fora dos discos até responder",
                key.1, key.0
            );
        }
    }
}

/// Os discos de agora: o `mountinfo` do processo (com as origens resolvidas
/// por [`BlockTree::resolve`]), a árvore de blocos e o uso de cada origem.
pub async fn read_disks(roots: &Roots, reader: &DiskReader) -> Vec<Disk> {
    let text = crate::read(&roots.procfs.join("self/mountinfo")).unwrap_or_default();
    let tree = BlockTree::read(roots);
    let mounts = tree.resolve(parse_mountinfo(&text));
    let usages = reader.usages(&mounts, &tree).await;
    aggregate(&mounts, &tree, &usages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FixedStatfs;

    fn disk() -> Node {
        Node {
            kind: Kind::Disk,
            parent: None,
        }
    }

    #[tokio::test]
    async fn warned_forgets_mounts_that_are_gone() {
        let fs = FixedStatfs::machine().slow("/var");
        let r = DiskReader::new(Arc::new(fs), Duration::from_millis(100));
        let t = BlockTree::from_nodes([("sdb1".to_string(), disk())], []);
        let stuck = [Mount {
            dev: "8:17".into(),
            source: "/dev/sdb1".into(),
            target: "/var".into(),
            fstype: "ext4".into(),
        }];
        r.usages(&stuck, &t).await;
        assert_eq!(lock(&r.warned).len(), 1, "a travada foi avisada");
        r.usages(&stuck, &t).await;
        assert_eq!(lock(&r.warned).len(), 1, "continua lá enquanto montada");
        r.usages(&[], &t).await;
        assert!(lock(&r.warned).is_empty(), "desmontada, sai do conjunto");
    }

    #[tokio::test]
    async fn spawn_failure_counts_as_timed_out() {
        let mut r = DiskReader::new(Arc::new(FixedStatfs::machine()), Duration::from_millis(100));
        r.spawn = |_job| Err(io::Error::other("sem threads"));
        let mounts = [Mount {
            dev: "8:2".into(),
            source: "/dev/sda2".into(),
            target: "/boot".into(),
            fstype: "ext4".into(),
        }];
        let t = BlockTree::from_nodes([("sda2".to_string(), disk())], []);
        assert!(r.usages(&mounts, &t).await.is_empty(), "a origem fica fora");
        assert!(
            lock(&r.pending).is_empty(),
            "o alvo não fica preso como pendente"
        );
        r.spawn = spawn_thread;
        assert_eq!(
            r.usages(&mounts, &t).await.len(),
            1,
            "e é pedida de novo depois"
        );
    }
}
