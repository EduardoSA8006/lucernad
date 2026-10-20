//! Ajudantes para os testes (feature `testing`).

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::disks::{Statfs, Usage};
use crate::nvidia::{Nvidia, NvidiaReading};

/// O caminho de `rel` dentro do `testdata/` deste crate.
pub fn testdata(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join(rel)
}

/// Um `statvfs` falso: um uso fixo por caminho, com caminhos lentos (1 s) e
/// caminhos que falham; guarda as chamadas na ordem. Caminho sem valor →
/// `Err(NotFound)`.
#[derive(Default)]
pub struct FixedStatfs {
    values: HashMap<PathBuf, Usage>,
    slow: HashSet<PathBuf>,
    failing: HashSet<PathBuf>,
    calls: Mutex<Vec<PathBuf>>,
}

impl FixedStatfs {
    /// Os usos da máquina de exemplo: `/` 2000/500, `/boot` 100/10,
    /// `/mnt/dados antigos` 1000/400 e `/var` 300/30.
    pub fn machine() -> FixedStatfs {
        let values = [
            ("/", 2000, 500),
            ("/boot", 100, 10),
            ("/mnt/dados antigos", 1000, 400),
            ("/var", 300, 30),
        ]
        .into_iter()
        .map(|(p, size, used)| (PathBuf::from(p), Usage { size, used }))
        .collect();
        FixedStatfs {
            values,
            ..FixedStatfs::default()
        }
    }

    /// `path` dorme 1 s antes de responder.
    pub fn slow(mut self, path: &str) -> FixedStatfs {
        self.slow.insert(PathBuf::from(path));
        self
    }

    /// `path` responde com erro.
    pub fn failing(mut self, path: &str) -> FixedStatfs {
        self.failing.insert(PathBuf::from(path));
        self
    }

    /// Os caminhos pedidos até agora, na ordem.
    pub fn calls(&self) -> Vec<PathBuf> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Statfs for FixedStatfs {
    fn usage(&self, path: &Path) -> io::Result<Usage> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(path.to_path_buf());
        if self.slow.contains(path) {
            std::thread::sleep(Duration::from_secs(1));
        }
        if self.failing.contains(path) {
            return Err(io::Error::other("statvfs falso falhou"));
        }
        self.values
            .get(path)
            .copied()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }
}

/// Uma NVML falsa: conta as chamadas e devolve sempre `reading`.
#[derive(Clone, Debug, Default)]
pub struct CountingNvidia {
    pub calls: Arc<AtomicUsize>,
    pub reading: Option<NvidiaReading>,
}

impl Nvidia for CountingNvidia {
    fn read(&mut self, _pci: &str) -> Option<NvidiaReading> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.reading
    }
}

/// Uma NVML que entra em pânico ao ser chamada.
#[derive(Clone, Copy, Debug, Default)]
pub struct PanicNvidia;

impl Nvidia for PanicNvidia {
    fn read(&mut self, pci: &str) -> Option<NvidiaReading> {
        panic!("PanicNvidia: a NVML foi chamada para {pci}")
    }
}
