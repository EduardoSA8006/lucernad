//! O comum dos módulos do `lucernad`: o trabalho com prazo numa thread solta
//! ([`detached`]), o supervisor do laço de um módulo ([`supervise`]) e a
//! publicação só do que mudou ([`publish_changed`]) e o sinal do modo jogo
//! ([`GameSignal`]), que deixa os módulos mais leves durante um jogo.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use lucerna_service::Topic;
use serde_json::Value;
use tokio::sync::{oneshot, watch};
use tokio::task::{AbortHandle, JoinHandle};

/// O sinal do modo jogo: ligado enquanto o módulo `game` quer o `lucernad`
/// mais leve (coleta espaçada, espectro pausado, cópias adiadas). Clonar dá
/// outra alça para o mesmo sinal.
#[derive(Clone)]
pub struct GameSignal {
    rx: watch::Receiver<bool>,
}

impl GameSignal {
    /// Um sinal desligado e quem o liga.
    pub fn channel() -> (watch::Sender<bool>, GameSignal) {
        let (tx, rx) = watch::channel(false);
        (tx, GameSignal { rx })
    }

    /// Um sinal sempre desligado (sem o módulo `game`).
    pub fn never() -> GameSignal {
        let (_tx, rx) = watch::channel(false);
        GameSignal { rx }
    }

    /// Se o sinal está ligado agora.
    pub fn is_on(&self) -> bool {
        *self.rx.borrow()
    }

    /// Volta quando o sinal muda. Sem quem o ligue (o [`GameSignal::never`],
    /// ou o emissor largado), nunca volta.
    pub async fn changed(&mut self) {
        if self.rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// O laço e o supervisor de um módulo; largar aborta os dois.
#[must_use = "largar o ModuleTask para o módulo"]
pub struct ModuleTask {
    tasks: Vec<AbortHandle>,
}

impl ModuleTask {
    /// As tarefas `tasks`, abortadas quando o `ModuleTask` for largado.
    pub fn new(tasks: Vec<AbortHandle>) -> ModuleTask {
        ModuleTask { tasks }
    }
}

impl Drop for ModuleTask {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// Vigia o laço `run` de um módulo (chame dentro do runtime): se ele entrar
/// em pânico, escreve `lucernad: o módulo {module} parou: …` no log e publica
/// `error` em `status`; o servidor continua atendendo.
pub fn supervise(
    run: JoinHandle<()>,
    status: Topic,
    module: &'static str,
    error: Value,
) -> ModuleTask {
    let run_abort = run.abort_handle();
    let supervisor = tokio::spawn(async move {
        if let Err(e) = run.await
            && e.is_panic()
        {
            let msg = panic_message(&*e.into_panic());
            eprintln!("lucernad: o módulo {module} parou: {msg:?}");
            status.publish(error);
        }
    });
    ModuleTask::new(vec![supervisor.abort_handle(), run_abort])
}

/// Publica `data` em `topic` só se ele mudou desde o retrato atual.
pub fn publish_changed(topic: &Topic, data: Value) {
    if topic.current().data != data {
        topic.publish(data);
    }
}

/// O texto de um pânico (`&str` ou `String`); vazio com outra carga.
pub fn panic_message(p: &(dyn Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_default()
}

/// O fim de um trabalho rodado por [`detached`].
pub enum Detached<T> {
    /// Voltou a tempo com este valor.
    Done(T),
    /// Não voltou no prazo; a thread segue solta até terminar.
    TimedOut,
    /// Entrou em pânico, com a carga dele.
    Panicked(Box<dyn Any + Send>),
    /// A thread não pôde ser criada; `f` não rodou.
    SpawnFailed(std::io::Error),
}

/// Um trabalho entregue a uma thread solta.
pub type Job = Box<dyn FnOnce() + Send + 'static>;

/// Como o [`detached`] cria a thread (trocável nos testes, para simular a
/// falha).
pub type Spawn = fn(Job) -> std::io::Result<()>;

/// Cria a thread solta de verdade.
pub fn spawn_thread(job: Job) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("lucernad-detached".into())
        .spawn(job)
        .map(drop)
}

/// Roda `f` numa `std::thread` solta e espera a resposta até `timeout`.
///
/// Para o trabalho que pode travar (o `statvfs` de uma montagem morta, a
/// NVML, a conexão com o PipeWire): a thread não é do runtime, então uma
/// chamada presa não segura os outros trabalhos nem a saída do processo, como
/// o `spawn_blocking` seguraria. O resultado chega por um `oneshot`; o pânico
/// de `f` é pego.
pub async fn detached<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
    timeout: Duration,
) -> Detached<T> {
    detached_with(spawn_thread, f, timeout).await
}

/// Roda `f` numa thread solta criada por `spawn`, sem prazo: a resposta (ou
/// o pânico de `f`) chega pelo `oneshot` quando a thread termina. Para quem
/// precisa saber quando a thread acabou de verdade (o [`detached`] desiste no
/// prazo e a thread segue solta).
pub fn detached_task<T: Send + 'static>(
    spawn: Spawn,
    f: impl FnOnce() -> T + Send + 'static,
) -> std::io::Result<oneshot::Receiver<std::thread::Result<T>>> {
    let (tx, rx) = oneshot::channel();
    let job: Job = Box::new(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(f)));
    });
    spawn(job)?;
    Ok(rx)
}

/// O [`detached`], com a criação da thread dada por `spawn`.
pub async fn detached_with<T: Send + 'static>(
    spawn: Spawn,
    f: impl FnOnce() -> T + Send + 'static,
    timeout: Duration,
) -> Detached<T> {
    let rx = match detached_task(spawn, f) {
        Ok(rx) => rx,
        Err(e) => return Detached::SpawnFailed(e),
    };
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
    async fn game_signal_never_is_off() {
        let mut s = GameSignal::never();
        assert!(!s.is_on());
        let waited = tokio::time::timeout(Duration::from_millis(50), s.changed()).await;
        assert!(waited.is_err(), "o never não pode mudar");
    }

    #[tokio::test]
    async fn game_signal_follows_sender() {
        let (tx, mut s) = GameSignal::channel();
        let mut other = s.clone();
        assert!(!s.is_on());
        tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(1), s.changed())
            .await
            .unwrap();
        assert!(s.is_on() && other.is_on());
        tx.send(false).unwrap();
        tokio::time::timeout(Duration::from_secs(1), other.changed())
            .await
            .unwrap();
        assert!(!other.is_on());
        s.changed().await;
        drop(tx);
        let waited = tokio::time::timeout(Duration::from_millis(50), s.changed()).await;
        assert!(waited.is_err(), "sem emissor, o changed fica parado");
    }

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
    async fn detached_task_answers_when_the_thread_ends() {
        let rx = detached_task(spawn_thread, || {
            std::thread::sleep(Duration::from_millis(50));
            7
        })
        .unwrap();
        assert_eq!(rx.await.unwrap().unwrap(), 7);
        let rx = detached_task(spawn_thread, || -> () { panic!("de propósito") }).unwrap();
        let p = rx.await.unwrap().unwrap_err();
        assert_eq!(p.downcast_ref::<&str>(), Some(&"de propósito"));
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
