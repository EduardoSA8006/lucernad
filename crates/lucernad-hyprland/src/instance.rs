//! A instância do Hyprland: a pasta `$XDG_RUNTIME_DIR/hypr/<assinatura>`
//! com os dois sockets.
//!
//! Com o `HYPRLAND_INSTANCE_SIGNATURE`, é a pasta dela; sem ele (o serviço do
//! systemd nem sempre o herda), a mais nova pelo `mtime` do `hyprland.lock`
//! entre as que ainda têm o `.socket.sock`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// O socket dos pedidos, dentro da pasta da instância.
pub const REQUEST_SOCKET: &str = ".socket.sock";
/// O socket dos eventos, dentro da pasta da instância.
pub const EVENTS_SOCKET: &str = ".socket2.sock";

/// Uma instância do Hyprland.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    /// A assinatura (o nome da pasta).
    pub signature: String,
    /// A pasta com os sockets.
    pub dir: PathBuf,
}

impl Instance {
    /// O `.socket.sock` (pedido e resposta).
    pub fn request_path(&self) -> PathBuf {
        self.dir.join(REQUEST_SOCKET)
    }

    /// O `.socket2.sock` (os eventos, uma linha cada).
    pub fn events_path(&self) -> PathBuf {
        self.dir.join(EVENTS_SOCKET)
    }
}

/// Acha a instância em `runtime_dir` (o `XDG_RUNTIME_DIR`): a da
/// `signature`, ou a mais nova viva. O erro é um texto em pt-BR.
pub fn find(runtime_dir: &Path, signature: Option<&str>) -> Result<Instance, String> {
    let hypr = runtime_dir.join("hypr");
    if let Some(sig) = signature {
        let valid = !sig.is_empty() && sig != "." && sig != ".." && !sig.contains('/');
        if !valid {
            return Err(format!("a assinatura do Hyprland é inválida: {sig:?}"));
        }
        let dir = hypr.join(sig);
        if !dir.join(REQUEST_SOCKET).exists() {
            return Err(format!(
                "a instância {sig} do Hyprland não tem socket em {}",
                dir.display()
            ));
        }
        return Ok(Instance {
            signature: sig.to_string(),
            dir,
        });
    }
    let entries = std::fs::read_dir(&hypr)
        .map_err(|e| format!("o Hyprland não está rodando (sem {}: {e})", hypr.display()))?;
    let mut best: Option<(SystemTime, Instance)> = None;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.join(REQUEST_SOCKET).exists() {
            continue;
        }
        let Some(signature) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let when = std::fs::metadata(dir.join("hyprland.lock"))
            .or_else(|_| std::fs::metadata(&dir))
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(t, _)| when > *t) {
            best = Some((when, Instance { signature, dir }));
        }
    }
    best.map(|(_, i)| i).ok_or_else(|| {
        format!(
            "o Hyprland não está rodando (nenhuma instância viva em {})",
            hypr.display()
        )
    })
}
