//! Os dublês do Hyprland para os testes: o [`FakeSockets`], um servidor dos
//! dois sockets numa pasta temporária (pedidos com respostas roteirizadas,
//! eventos injetados).

use std::path::Path;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, watch};
use tokio::task::AbortHandle;

use crate::instance::{EVENTS_SOCKET, REQUEST_SOCKET};

/// A resposta roteirizada de um comando.
#[derive(Clone)]
enum Script {
    Body(String),
    Hang,
}

#[derive(Default)]
struct Shared {
    /// Prefixo do comando → resposta; o último roteiro de um prefixo vence.
    scripts: Vec<(String, Script)>,
    requests: Vec<String>,
}

impl Shared {
    fn script(&mut self, prefix: &str, s: Script) {
        self.scripts.retain(|(p, _)| p != prefix);
        self.scripts.push((prefix.to_string(), s));
    }

    fn lookup(&self, command: &str) -> Option<Script> {
        self.scripts
            .iter()
            .filter(|(p, _)| command.starts_with(p.as_str()))
            .max_by_key(|(p, _)| p.len())
            .map(|(_, s)| s.clone())
    }
}

/// Os sockets `.socket.sock` e `.socket2.sock` de um Hyprland falso numa
/// pasta. Crie dentro de um runtime do `tokio`; largar para de atender (os
/// arquivos ficam).
pub struct FakeSockets {
    shared: Arc<Mutex<Shared>>,
    events: broadcast::Sender<String>,
    kick: watch::Sender<u64>,
    tasks: Vec<AbortHandle>,
}

impl FakeSockets {
    /// Atende em `dir/.socket.sock` e `dir/.socket2.sock` (a pasta precisa
    /// existir). Comando sem roteiro responde `unknown request`.
    pub fn start(dir: &Path) -> FakeSockets {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (events, _) = broadcast::channel(256);
        let (kick, _) = watch::channel(0u64);
        let requests = UnixListener::bind(dir.join(REQUEST_SOCKET)).unwrap();
        let listen_events = UnixListener::bind(dir.join(EVENTS_SOCKET)).unwrap();
        let s = shared.clone();
        let req_task = tokio::spawn(async move {
            while let Ok((stream, _)) = requests.accept().await {
                tokio::spawn(serve_request(stream, s.clone()));
            }
        });
        let ev = events.clone();
        let k = kick.clone();
        let ev_task = tokio::spawn(async move {
            while let Ok((stream, _)) = listen_events.accept().await {
                tokio::spawn(serve_events(stream, ev.subscribe(), k.subscribe()));
            }
        });
        FakeSockets {
            shared,
            events,
            kick,
            tasks: vec![req_task.abort_handle(), ev_task.abort_handle()],
        }
    }

    /// Os comandos que começam com `command_prefix` respondem `body`.
    pub fn reply(&self, command_prefix: &str, body: &str) {
        self.shared
            .lock()
            .unwrap()
            .script(command_prefix, Script::Body(body.to_string()));
    }

    /// Os comandos que começam com `command_prefix` nunca respondem.
    pub fn hang(&self, command_prefix: &str) {
        self.shared
            .lock()
            .unwrap()
            .script(command_prefix, Script::Hang);
    }

    /// Os comandos recebidos, na ordem.
    pub fn requests(&self) -> Vec<String> {
        self.shared.lock().unwrap().requests.clone()
    }

    /// Manda `line` (sem o `\n`) a quem está conectado no socket de eventos.
    pub fn emit(&self, line: &str) {
        let _ = self.events.send(line.to_string());
    }

    /// Quantas conexões de eventos estão abertas agora.
    pub fn event_clients(&self) -> usize {
        self.events.receiver_count()
    }

    /// Fecha as conexões de eventos abertas (o socket continua aceitando).
    pub fn drop_events(&self) {
        self.kick.send_modify(|n| *n += 1);
    }
}

impl Drop for FakeSockets {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
        self.drop_events();
    }
}

async fn serve_request(mut stream: UnixStream, shared: Arc<Mutex<Shared>>) {
    let mut buf = Vec::new();
    if stream.read_to_end(&mut buf).await.is_err() {
        return;
    }
    let command = String::from_utf8_lossy(&buf).into_owned();
    let script = {
        let mut s = shared.lock().unwrap();
        s.requests.push(command.clone());
        s.lookup(&command)
    };
    let body = match script {
        Some(Script::Body(b)) => b,
        Some(Script::Hang) => {
            std::future::pending::<()>().await;
            return;
        }
        None => "unknown request".to_string(),
    };
    let _ = stream.write_all(body.as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn serve_events(
    mut stream: UnixStream,
    mut lines: broadcast::Receiver<String>,
    mut kick: watch::Receiver<u64>,
) {
    kick.borrow_and_update();
    loop {
        tokio::select! {
            line = lines.recv() => match line {
                Ok(line) => {
                    if stream.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            },
            _ = kick.changed() => return,
        }
    }
}
