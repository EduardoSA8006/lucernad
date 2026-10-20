//! Módulo `system` do `lucernad`: as leituras do `/proc` e do `/sys`.
//!
//! Cada leitura é uma função pura sobre o texto do arquivo, mais um ajudante
//! que lê o arquivo de uma raiz ([`Roots`]): nos testes, a raiz é uma pasta de
//! `testdata/`.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::oneshot;

pub mod cpu;
pub mod disks;
pub mod fdinfo;
pub mod gpu;
pub mod info;
pub mod memory;
pub mod net;
pub mod nvidia;
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

/// O fim de um trabalho rodado por [`detached`].
pub(crate) enum Detached<T> {
    /// Voltou a tempo com este valor.
    Done(T),
    /// Não voltou no prazo; a thread segue solta até terminar.
    TimedOut,
    /// Entrou em pânico, com a carga dele.
    Panicked(
        #[cfg_attr(
            not(test),
            expect(
                dead_code,
                reason = "por enquanto, só os testes leem a carga do pânico"
            )
        )]
        Box<dyn Any + Send>,
    ),
    /// A thread não pôde ser criada; `f` não rodou.
    SpawnFailed(std::io::Error),
}

/// Um trabalho entregue a uma thread solta.
pub(crate) type Job = Box<dyn FnOnce() + Send + 'static>;

/// Como o [`detached`] cria a thread (trocável nos testes, para simular a
/// falha).
pub(crate) type Spawn = fn(Job) -> std::io::Result<()>;

/// Cria a thread solta de verdade.
pub(crate) fn spawn_thread(job: Job) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("lucernad-detached".into())
        .spawn(job)
        .map(drop)
}

/// Roda `f` numa `std::thread` solta e espera a resposta até `timeout`.
///
/// Para o trabalho que pode travar (o `statvfs` de uma montagem morta, a
/// NVML): a thread não é do runtime, então uma chamada presa não segura os
/// outros trabalhos nem a saída do processo, como o `spawn_blocking`
/// seguraria. O resultado chega por um `oneshot`; o pânico de `f` é pego.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "a NVML passa a usar na Tarefa 5; os discos usam o detached_with"
    )
)]
pub(crate) async fn detached<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
    timeout: Duration,
) -> Detached<T> {
    detached_with(spawn_thread, f, timeout).await
}

/// O [`detached`], com a criação da thread dada por `spawn`.
pub(crate) async fn detached_with<T: Send + 'static>(
    spawn: Spawn,
    f: impl FnOnce() -> T + Send + 'static,
    timeout: Duration,
) -> Detached<T> {
    let (tx, rx) = oneshot::channel();
    let job: Job = Box::new(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(f)));
    });
    if let Err(e) = spawn(job) {
        return Detached::SpawnFailed(e);
    }
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(Ok(value))) => Detached::Done(value),
        Ok(Ok(Err(panic))) => Detached::Panicked(panic),
        // O `catch_unwind` sempre responde; sem resposta, a thread morreu.
        Ok(Err(closed)) => Detached::Panicked(Box::new(closed)),
        Err(_) => Detached::TimedOut,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn detached_returns_times_out_and_catches_panics() {
        let short = Duration::from_millis(100);
        assert!(matches!(detached(|| 7, short).await, Detached::Done(7)));
        let slow = || std::thread::sleep(Duration::from_millis(500));
        assert!(matches!(detached(slow, short).await, Detached::TimedOut));
        match detached(|| -> () { panic!("de propósito") }, short).await {
            Detached::Panicked(p) => assert_eq!(p.downcast_ref::<&str>(), Some(&"de propósito")),
            _ => panic!("o pânico não foi pego"),
        }
    }

    #[tokio::test]
    async fn spawn_failure_is_its_own_outcome() {
        let fail: Spawn = |_job| Err(std::io::Error::other("sem threads"));
        match detached_with(fail, || 7, Duration::from_millis(100)).await {
            Detached::SpawnFailed(e) => assert_eq!(e.to_string(), "sem threads"),
            _ => panic!("a falha ao criar a thread não foi notada"),
        }
    }
}
