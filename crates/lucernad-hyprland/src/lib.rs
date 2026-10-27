//! Módulo `hyprland` do `lucernad`: o mínimo do compositor que o modo jogo
//! usa.
//!
//! A [`instance`] acha a pasta do Hyprland; o [`socket`] faz um pedido ao
//! `.socket.sock` com prazo; os [`events`] chegam pelo `.socket2.sock`; as
//! [`options`] são lidas pelo `j/getoption` e mudadas pelo `eval` com
//! `hl.config({...})`; as [`clients`] são as janelas e os monitores. Tudo
//! fica atrás do trait [`Hypr`]: o [`SocketHypr`] de verdade e, na feature
//! `testing`, o `FakeHypr`. O [`module`] publica o `hyprland.status`.
//!
//! Organizado para receber depois os atalhos, os monitores e a entrada (um
//! arquivo por assunto ao lado destes).

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::broadcast;
use tokio::task::AbortHandle;

use crate::clients::{Window, parse_active_workspaces, parse_clients};
use crate::events::{CONNECTED, DISCONNECTED, Event, parse_line};
use crate::instance::Instance;
use crate::options::{OptionValue, config_lua, lua_string, parse_getoption};
use crate::socket::{HYPR_TIMEOUT, HyprError, request};

pub mod clients;
pub mod events;
pub mod instance;
mod module;
pub mod options;
pub mod socket;
#[cfg(feature = "testing")]
pub mod testing;

pub use module::{Hyprland, HyprlandConfig, HyprlandTopics, register};

/// Um futuro em caixa, para o trait ser usado como objeto.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A maior espera entre as tentativas de reconectar os eventos.
pub const EVENTS_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// O Hyprland, para quem pede e para quem assina os eventos.
pub trait Hypr: Send + Sync + 'static {
    /// Um pedido ao `.socket.sock` (`j/getoption …`, `eval …`), com prazo.
    fn request(&self, command: String) -> BoxFuture<'_, Result<String, HyprError>>;

    /// Os eventos, a partir de agora (com o [`events::CONNECTED`] e o
    /// [`events::DISCONNECTED`] do leitor). Chame dentro do runtime.
    fn events(&self) -> broadcast::Receiver<Event>;

    /// A assinatura da instância atual, se há uma.
    fn instance(&self) -> Option<String>;
}

/// O valor da opção `name`.
pub async fn get_option(h: &dyn Hypr, name: &str) -> Result<OptionValue, HyprError> {
    let body = h.request(format!("j/getoption {name}")).await?;
    parse_getoption(&body).map_err(HyprError::Refused)
}

/// Um `eval` com o Lua `lua`; resposta diferente de `ok` é recusa.
pub async fn eval(h: &dyn Hypr, lua: &str) -> Result<(), HyprError> {
    let body = h.request(format!("eval {lua}")).await?;
    let body = body.trim();
    if body == "ok" {
        Ok(())
    } else {
        Err(HyprError::Refused(
            body.strip_prefix("error: ").unwrap_or(body).to_string(),
        ))
    }
}

/// Muda as opções `changes` num `eval` só (nada com a lista vazia).
pub async fn set_options(h: &dyn Hypr, changes: &[(&str, OptionValue)]) -> Result<(), HyprError> {
    if changes.is_empty() {
        return Ok(());
    }
    eval(h, &config_lua(changes)).await
}

/// As janelas e os workspaces à mostra.
pub async fn windows(h: &dyn Hypr) -> Result<(Vec<Window>, Vec<i64>), HyprError> {
    let clients = h.request("j/clients".into()).await?;
    let monitors = h.request("j/monitors".into()).await?;
    let windows = parse_clients(&clients).map_err(HyprError::Refused)?;
    let active = parse_active_workspaces(&monitors).map_err(HyprError::Refused)?;
    Ok((windows, active))
}

/// O Lua que muda a propriedade `prop` da janela `address` (com o `0x`).
/// O Hyprland 0.56 com config em Lua não tem mais o `setprop`: é o
/// despachante `hl.dsp.window.set_prop` (conferido no container).
pub fn window_prop_lua(address: &str, prop: &str, value: &str) -> String {
    format!(
        "hl.dispatch(hl.dsp.window.set_prop({{ window = {}, prop = {}, value = {} }}))",
        lua_string(&format!("address:{address}")),
        lua_string(prop),
        lua_string(value)
    )
}

/// Muda a propriedade `prop` da janela `address` (janela que não existe
/// mais não dá erro).
pub async fn set_window_prop(
    h: &dyn Hypr,
    address: &str,
    prop: &str,
    value: &str,
) -> Result<(), HyprError> {
    eval(h, &window_prop_lua(address, prop, value)).await
}

/// A versão do Hyprland (o campo `version` do `j/version`).
pub async fn version(h: &dyn Hypr) -> Result<String, HyprError> {
    let body = h.request("j/version".into()).await?;
    serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| {
            v.get("version")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .ok_or_else(|| HyprError::Refused(body.trim().to_string()))
}

/// O que o [`SocketHypr`] e o leitor de eventos dividem.
struct Inner {
    runtime: Option<PathBuf>,
    signature: Option<String>,
    current: Mutex<Option<Instance>>,
    /// As instâncias que não atendem mais (o socket ficou, o Hyprland não).
    dead: Mutex<Vec<String>>,
    events: broadcast::Sender<Event>,
}

impl Inner {
    /// A instância guardada, ou achada agora: a da assinatura e, sem ela
    /// (ou com ela morta: o Hyprland caiu e a pasta ficou), a mais nova viva
    /// fora das que já não atenderam.
    fn instance(&self) -> Result<Instance, HyprError> {
        if let Some(i) = self.current.lock().unwrap().clone() {
            return Ok(i);
        }
        let runtime = self
            .runtime
            .as_deref()
            .ok_or_else(|| HyprError::Unreachable("sem o XDG_RUNTIME_DIR".into()))?;
        // As mortas ficam de fora; sem nenhuma outra, volta a tentar todas
        // (uma falha passageira não deixa o daemon sem instância).
        let dead = self.dead.lock().unwrap().clone();
        let preferred = self
            .signature
            .as_deref()
            .filter(|s| !dead.iter().any(|d| d == s));
        let found = match preferred {
            Some(sig) => instance::find(runtime, Some(sig))
                .or_else(|_| instance::find_except(runtime, None, &dead)),
            None => instance::find_except(runtime, None, &dead),
        }
        .or_else(|e| {
            if dead.is_empty() {
                return Err(e);
            }
            self.dead.lock().unwrap().clear();
            instance::find(runtime, self.signature.as_deref())
                .or_else(|_| instance::find(runtime, None))
        })
        .map_err(HyprError::Unreachable)?;
        *self.current.lock().unwrap() = Some(found.clone());
        Ok(found)
    }

    fn forget(&self) {
        *self.current.lock().unwrap() = None;
    }

    /// A instância `i` não atende: esquece e não volta a ela.
    fn mark_dead(&self, i: &Instance) {
        let mut dead = self.dead.lock().unwrap();
        if !dead.contains(&i.signature) {
            dead.push(i.signature.clone());
        }
        drop(dead);
        self.forget();
    }
}

/// O Hyprland de verdade, pelos sockets. A instância é achada no primeiro
/// pedido e refeita quando o socket some; o leitor de eventos começa no
/// primeiro [`Hypr::events`] e reconecta com espera de 1, 2, 4… até 30 s.
pub struct SocketHypr {
    inner: Arc<Inner>,
    reader: Mutex<Option<AbortHandle>>,
}

impl SocketHypr {
    /// O Hyprland do ambiente (`XDG_RUNTIME_DIR` e
    /// `HYPRLAND_INSTANCE_SIGNATURE`).
    pub fn discover() -> SocketHypr {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        let signature = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
            .ok()
            .filter(|s| !s.is_empty());
        SocketHypr::new(runtime, signature)
    }

    /// O Hyprland em `runtime_dir/hypr`, com a `signature` preferida.
    pub fn at(runtime_dir: &Path, signature: Option<&str>) -> SocketHypr {
        SocketHypr::new(
            Some(runtime_dir.to_path_buf()),
            signature.map(str::to_string),
        )
    }

    fn new(runtime: Option<PathBuf>, signature: Option<String>) -> SocketHypr {
        let (events, _) = broadcast::channel(256);
        SocketHypr {
            inner: Arc::new(Inner {
                runtime,
                signature,
                current: Mutex::new(None),
                dead: Mutex::new(Vec::new()),
                events,
            }),
            reader: Mutex::new(None),
        }
    }
}

impl Drop for SocketHypr {
    fn drop(&mut self) {
        if let Some(r) = self.reader.lock().unwrap().take() {
            r.abort();
        }
    }
}

impl Hypr for SocketHypr {
    fn request(&self, command: String) -> BoxFuture<'_, Result<String, HyprError>> {
        Box::pin(async move {
            let first = self.inner.instance()?;
            match request(&first.request_path(), &command, HYPR_TIMEOUT).await {
                Err(HyprError::Unreachable(e)) => {
                    // O socket sumiu ou não atende: acha de novo (sem esta) e,
                    // se houver outra instância, tenta uma vez nela.
                    self.inner.mark_dead(&first);
                    let again = self.inner.instance()?;
                    if again == first {
                        return Err(HyprError::Unreachable(e));
                    }
                    request(&again.request_path(), &command, HYPR_TIMEOUT).await
                }
                other => other,
            }
        })
    }

    fn events(&self) -> broadcast::Receiver<Event> {
        let rx = self.inner.events.subscribe();
        let mut reader = self.reader.lock().unwrap();
        if reader.is_none() {
            let inner = self.inner.clone();
            *reader = Some(tokio::spawn(read_events(inner)).abort_handle());
        }
        rx
    }

    fn instance(&self) -> Option<String> {
        self.inner
            .current
            .lock()
            .unwrap()
            .as_ref()
            .map(|i| i.signature.clone())
    }
}

/// O leitor do `.socket2.sock`: conecta, manda cada linha como evento e,
/// quando cai, reconecta com espera crescente.
async fn read_events(inner: Arc<Inner>) {
    let mut delay = Duration::from_secs(1);
    loop {
        let connected = match inner.instance() {
            Ok(i) => match UnixStream::connect(i.events_path()).await {
                Ok(s) => Some((i, s)),
                Err(_) => {
                    inner.mark_dead(&i);
                    None
                }
            },
            Err(_) => None,
        };
        match connected {
            Some((instance, stream)) => {
                delay = Duration::from_secs(1);
                let _ = inner.events.send(Event {
                    name: CONNECTED.into(),
                    data: instance.signature.clone(),
                });
                let mut lines = BufReader::new(stream).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(e) = parse_line(&line) {
                        let _ = inner.events.send(e);
                    }
                }
                let _ = inner.events.send(Event {
                    name: DISCONNECTED.into(),
                    data: instance.signature,
                });
            }
            None => inner.forget(),
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(EVENTS_BACKOFF_MAX);
    }
}
