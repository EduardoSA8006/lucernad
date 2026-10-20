//! Módulo `system` do `lucernad`: as leituras do `/proc` e do `/sys`.
//!
//! Cada leitura é uma função pura sobre o texto do arquivo, mais um ajudante
//! que lê o arquivo de uma raiz ([`Roots`]): nos testes, a raiz é uma pasta de
//! `testdata/`.

use std::path::{Path, PathBuf};

pub mod cpu;
pub mod info;
pub mod memory;
pub mod net;
pub mod temp;

#[cfg(feature = "testing")]
pub mod testing;

/// Onde ficam o `/proc`, o `/sys` e o `/etc` lidos pelo módulo.
#[derive(Clone, Debug)]
pub struct Roots {
    pub procfs: PathBuf,
    pub sysfs: PathBuf,
    pub etc: PathBuf,
}

impl Roots {
    /// As raízes do sistema: `/proc`, `/sys` e `/etc`.
    pub fn system() -> Roots {
        Roots::at(Path::new("/"))
    }

    /// `base/proc`, `base/sys` e `base/etc` (uma máquina de exemplo).
    pub fn at(base: &Path) -> Roots {
        Roots {
            procfs: base.join("proc"),
            sysfs: base.join("sys"),
            etc: base.join("etc"),
        }
    }
}

/// O conteúdo do arquivo, ou `None` se ele não puder ser lido.
pub(crate) fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}
