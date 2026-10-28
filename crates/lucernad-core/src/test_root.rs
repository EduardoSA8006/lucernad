//! A raiz de teste do `LUCERNAD_POWER_ROOT`, comum aos módulos que mexem na
//! energia da máquina: o `power` (o brilho e os perfis lidos) e o `game` (o
//! perfil segurado e o GameMode).
//!
//! Com a variável definida, nenhum deles fala com a máquina: o container de
//! teste do shell monta o D-Bus do sistema do host, e um `HoldProfile` ali
//! mudaria o perfil de energia de verdade. Os falsos ficam sobre arquivos
//! nesta raiz: o serviço de perfis existe se a pasta [`PROFILES_DIR`]
//! existe, e o GameMode, se a [`GAMEMODE_DIR`] existe.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// A variável da raiz de teste.
pub const POWER_ROOT: &str = "LUCERNAD_POWER_ROOT";

/// A pasta do power-profiles-daemon falso, dentro da raiz.
pub const PROFILES_DIR: &str = "power-profiles";

/// A pasta do GameMode falso, dentro da raiz.
pub const GAMEMODE_DIR: &str = "gamemode";

/// As pastas que uma raiz de teste nunca pode ser nem estar dentro.
const FORBIDDEN: [&str; 3] = ["/sys", "/dev", "/proc"];

/// Se um caminho canônico é o `/` ou fica dentro do `/sys`, do `/dev` ou do
/// `/proc` (por componente).
pub fn forbidden(path: &Path) -> bool {
    path == Path::new("/") || FORBIDDEN.iter().any(|f| path.starts_with(f))
}

/// A raiz pedida pelo valor da variável, já canônica.
///
/// Ausente ou vazia: `Ok(None)` (a máquina de verdade). Definida, precisa
/// ser uma pasta que existe, por caminho absoluto, e que, depois do
/// `canonicalize`, não é o `/` nem fica dentro do `/sys`, do `/dev` ou do
/// `/proc`; senão, `Err(())`. Quem chama nunca volta à máquina de verdade
/// com a variável definida.
#[allow(clippy::result_unit_err)]
pub fn test_base(value: Option<&OsStr>) -> Result<Option<PathBuf>, ()> {
    let Some(value) = value.filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let path = Path::new(value);
    if !path.is_absolute() {
        return Err(());
    }
    let base = path.canonicalize().map_err(|_| ())?;
    if forbidden(&base) || !base.is_dir() {
        return Err(());
    }
    Ok(Some(base))
}

/// Se o daemon roda num container: o `/.dockerenv` do Docker (o container do
/// `dev/test.sh`), o `/run/.containerenv` do Podman, o
/// `/run/host/container-manager` do toolbox e do distrobox e o
/// `/run/systemd/container` do systemd-nspawn (e de outros que o seguem).
pub fn in_container() -> bool {
    [
        "/.dockerenv",
        "/run/.containerenv",
        "/run/host/container-manager",
        "/run/systemd/container",
    ]
    .iter()
    .any(|marker| Path::new(marker).exists())
}

/// Uma pasta da raiz de teste (`base/name`), se existe e, resolvida, não sai
/// da raiz (um link para fora levaria à máquina de verdade).
pub fn inner_dir(base: &Path, name: &str) -> Option<PathBuf> {
    let real = base.join(name).canonicalize().ok()?;
    (real.starts_with(base) && real.is_dir() && !forbidden(&real)).then_some(real)
}
