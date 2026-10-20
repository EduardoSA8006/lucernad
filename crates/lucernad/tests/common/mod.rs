//! O `lucernad` em processo, num socket temporário, sobre a máquina de exemplo.

// Cada arquivo de teste usa uma parte das ferramentas.
#![allow(dead_code)]

use std::path::PathBuf;
use std::time::Duration;

use lucerna_ipc::Client;
use lucernad::{Modules, lucernad};
use lucernad_system::ModuleTask;
use lucernad_system::testing::machine_config;
use tempfile::TempDir;
use tokio::net::UnixListener;
use tokio::task::JoinHandle;

/// O serviço rodando numa tarefa.
pub struct Running {
    _dir: TempDir,
    pub path: PathBuf,
    _system_task: ModuleTask,
    task: JoinHandle<()>,
}

/// Sobe o `lucernad` (módulo `system` na máquina de exemplo) num socket
/// temporário, sem shell.
pub async fn start() -> Running {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lucernad.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let mut d = lucernad(Modules {
        system: machine_config(),
    });
    d.service.idle_timeout(Duration::from_secs(60));
    let service = d.service;
    let task = tokio::spawn(async move {
        service.run_with(listener, None).await.unwrap();
    });
    Running {
        _dir: dir,
        path,
        _system_task: d.system_task,
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
