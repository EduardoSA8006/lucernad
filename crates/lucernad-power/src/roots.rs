//! A raiz das leituras: o `/sys` e o `/dev` de verdade ou os de uma árvore
//! falsa, e a variável `LUCERNAD_POWER_ROOT` que escolhe entre os dois.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// O maior EDID lido do sysfs (o `stat` dá tamanho 0: só lendo se sabe).
pub const EDID_MAX: u64 = 32 * 1024;

/// O maior arquivo pequeno do sysfs lido por [`read_small`].
const SMALL_MAX: u64 = 4 * 1024;

/// O texto do `power.status.error` com o `LUCERNAD_POWER_ROOT` inválido.
pub const ROOT_INVALID: &str = "LUCERNAD_POWER_ROOT inválido: precisa ser uma pasta de teste fora do /sys, do /dev e do /proc.";

/// O texto do `power.status.error` em container sem o `LUCERNAD_POWER_ROOT`.
pub const CONTAINER_REFUSED: &str = "Em container, o módulo de energia só roda com LUCERNAD_POWER_ROOT (uma árvore falsa): sem ela, o brilho do host mudaria.";

/// As pastas que uma raiz de teste nunca pode ser nem estar dentro.
const FORBIDDEN: [&str; 3] = ["/sys", "/dev", "/proc"];

/// Onde ficam o sysfs e o `/dev`.
#[derive(Clone, Debug)]
pub struct Roots {
    pub sysfs: PathBuf,
    pub dev: PathBuf,
}

impl Roots {
    /// O `/sys` e o `/dev` da máquina.
    pub fn system() -> Self {
        Self {
            sysfs: PathBuf::from("/sys"),
            dev: PathBuf::from("/dev"),
        }
    }

    /// Uma árvore falsa: `base/sys` e `base/dev`.
    pub fn at(base: &Path) -> Self {
        Self {
            sysfs: base.join("sys"),
            dev: base.join("dev"),
        }
    }
}

/// A raiz pedida pelo `LUCERNAD_POWER_ROOT`.
///
/// Ausente ou vazia: `Ok(None)` (a máquina de verdade), salvo em container,
/// onde é recusada ([`CONTAINER_REFUSED`]). Definida, precisa ser uma pasta
/// que existe, por caminho absoluto, e que, depois do `canonicalize`, não é o
/// `/` nem fica dentro do `/sys`, do `/dev` ou do `/proc` (por componente);
/// senão, [`ROOT_INVALID`]. Nunca volta à máquina de verdade com a variável
/// definida.
pub fn root_from_env(value: Option<&OsStr>, in_container: bool) -> Result<Option<Roots>, String> {
    let value = value.filter(|v| !v.is_empty());
    let Some(value) = value else {
        return if in_container {
            Err(CONTAINER_REFUSED.to_string())
        } else {
            Ok(None)
        };
    };
    let path = Path::new(value);
    if !path.is_absolute() {
        return Err(ROOT_INVALID.to_string());
    }
    let base = path.canonicalize().map_err(|_| ROOT_INVALID.to_string())?;
    let forbidden = base == Path::new("/") || FORBIDDEN.iter().any(|f| base.starts_with(f));
    if forbidden || !base.is_dir() {
        return Err(ROOT_INVALID.to_string());
    }
    Ok(Some(Roots::at(&base)))
}

/// Se o daemon roda num container (Docker ou Podman).
pub fn in_container() -> bool {
    Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists()
}

/// Um arquivo pequeno do sysfs (até 4 KiB), sem espaços nas pontas.
pub fn read_small(path: &Path) -> io::Result<String> {
    let mut text = String::new();
    File::open(path)?
        .take(SMALL_MAX)
        .read_to_string(&mut text)?;
    Ok(text.trim().to_string())
}
