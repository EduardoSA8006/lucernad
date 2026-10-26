//! O `lucernad` em processo, num socket temporário: o módulo `system` sobre a
//! máquina de exemplo e o `spectrum` sem PipeWire nem D-Bus de verdade.

// Cada arquivo de teste usa uma parte das ferramentas.
#![allow(dead_code)]

use std::path::PathBuf;
use std::time::Duration;

use lucerna_ipc::Client;
use lucernad::{Modules, lucernad};
#[cfg(feature = "clipboard")]
use lucernad_clipboard::testing::quiet_config as clipboard_config;
use lucernad_core::ModuleTask;
#[cfg(feature = "power")]
use lucernad_power::testing::quiet_config as power_config;
#[cfg(feature = "spectrum")]
use lucernad_spectrum::testing::quiet_config;
#[cfg(feature = "system")]
use lucernad_system::testing::machine_config;
#[cfg(feature = "web")]
use lucernad_web::testing::quiet_config as web_config;
use tempfile::TempDir;
use tokio::net::UnixListener;
use tokio::task::JoinHandle;

/// O serviço rodando numa tarefa.
pub struct Running {
    _dir: TempDir,
    pub path: PathBuf,
    _tasks: Vec<ModuleTask>,
    task: JoinHandle<()>,
}

/// Sobe o `lucernad` (o `system` na máquina de exemplo, o `spectrum` e o
/// `clipboard` com os dublês) num socket temporário, sem shell.
pub async fn start() -> Running {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lucernad.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let mut d = lucernad(Modules {
        #[cfg(feature = "system")]
        system: machine_config(),
        #[cfg(feature = "spectrum")]
        spectrum: quiet_config(),
        #[cfg(feature = "clipboard")]
        clipboard: clipboard_config(dir.path()),
        #[cfg(feature = "power")]
        power: power_config(dir.path()),
        #[cfg(feature = "web")]
        web: web_config(dir.path(), "http://127.0.0.1:1"),
    });
    d.service.idle_timeout(Duration::from_secs(60));
    let service = d.service;
    let task = tokio::spawn(async move {
        service.run_with(listener, None).await.unwrap();
    });
    Running {
        _dir: dir,
        path,
        _tasks: vec![
            #[cfg(feature = "system")]
            d.system_task,
            #[cfg(feature = "spectrum")]
            d.spectrum_task,
            #[cfg(feature = "clipboard")]
            d.clipboard_task,
            #[cfg(feature = "power")]
            d.power_task,
            #[cfg(feature = "web")]
            d.web_task,
        ],
        task,
    }
}

impl Running {
    /// Cliente com o aperto de mão feito.
    pub async fn client(&self) -> Client {
        Client::connect(&self.path, "test", "lucernad")
            .await
            .unwrap()
    }

    /// Para o serviço e espera a tarefa terminar.
    pub async fn stop(mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}
