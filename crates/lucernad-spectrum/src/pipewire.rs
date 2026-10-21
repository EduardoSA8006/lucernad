//! O backend de verdade: o PipeWire, numa `std::thread` por sessão.
//!
//! O laço do PipeWire não é `Send`: cada sessão tem a thread dela com o
//! `MainLoop`, recebe os comandos por um `pipewire::channel` e manda os
//! eventos por um `tokio::sync::mpsc`. Nada do PipeWire passa para o runtime.
//!
//! A captura só lê: um fluxo de entrada por nó, com `target.object` = o
//! `object.serial` do nó do player e `node.dont-fallback`, para o WirePlumber
//! destruir o fluxo em vez de ligá-lo ao microfone quando o alvo some. O link
//! do player com o sink não é tocado.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use pipewire as pw;
use pw::client::{Client, ClientListener};
use pw::core::CoreRc;
use pw::properties::PropertiesBox;
use pw::registry::{GlobalObject, RegistryRc};
use pw::spa;
use pw::spa::pod::Pod;
use pw::spa::utils::dict::DictRef;
use pw::stream::{StreamFlags, StreamListener, StreamRc, StreamState};
use pw::types::ObjectType;
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::backend::{Backend, Control, Event, OutputNode, Session};

/// O `node.name` dos fluxos de captura.
pub const NODE_NAME: &str = "lucernad-espectro";
/// O prazo da conexão com o PipeWire.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// O texto da queda da conexão (`Event::Disconnected`).
pub const DISCONNECTED: &str = "O PipeWire fechou a conexão.";

/// A espera para reabrir um fluxo que o servidor destruiu.
const REOPEN_DELAY: Duration = Duration::from_millis(500);
/// O passo do timer que confere os fluxos destruídos.
const TICK: Duration = Duration::from_millis(100);
/// A classe dos nós de saída de áudio dos apps.
const OUTPUT_CLASS: &str = "Stream/Output/Audio";

/// O PipeWire como [`Backend`].
pub struct PipeWire {
    /// O `remote.name` da conexão; `None` é o servidor padrão.
    pub remote: Option<String>,
    /// O prazo para a conexão responder.
    pub timeout: Duration,
}

impl PipeWire {
    /// O servidor do usuário, com o [`CONNECT_TIMEOUT`].
    pub fn system() -> PipeWire {
        PipeWire {
            remote: None,
            timeout: CONNECT_TIMEOUT,
        }
    }
}

impl Backend for PipeWire {
    fn probe(&self) -> Result<(), String> {
        // Abrir e largar: a sessão sem captura não abre fluxo nenhum.
        self.open().map(drop)
    }

    fn open(&self) -> Result<Session, String> {
        let (events_tx, events) = mpsc::unbounded_channel();
        let (commands, commands_rx) = pw::channel::channel();
        let (ready_tx, ready) = std_mpsc::channel();
        let (remote, timeout) = (self.remote.clone(), self.timeout);
        std::thread::Builder::new()
            .name("lucernad-pipewire".into())
            .spawn(move || run(remote, timeout, events_tx, commands_rx, ready_tx))
            .map_err(|e| connect_error(&e))?;

        // A thread tem o prazo dela; este é a reserva se ela travar antes.
        match ready.recv_timeout(timeout + Duration::from_secs(1)) {
            Ok(Ok(())) => Ok(Session {
                events,
                control: Box::new(PipeWireControl { commands }),
            }),
            Ok(Err(e)) => Err(e),
            Err(std_mpsc::RecvTimeoutError::Timeout) => {
                let _ = commands.send(Command::Quit);
                Err(no_answer(timeout))
            }
            Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                Err(connect_error(&"a thread do PipeWire parou"))
            }
        }
    }
}

fn connect_error(e: &dyn std::fmt::Display) -> String {
    format!("Não consegui conectar ao PipeWire: {e}.")
}

fn no_answer(timeout: Duration) -> String {
    let secs = format!("{}", timeout.as_secs_f32()).replace('.', ",");
    format!("O PipeWire não respondeu em {secs} s.")
}

enum Command {
    Capture(Vec<u64>),
    Quit,
}

struct PipeWireControl {
    commands: pw::channel::Sender<Command>,
}

impl Control for PipeWireControl {
    fn capture(&self, serials: &[u64]) {
        let _ = self.commands.send(Command::Capture(serials.to_vec()));
    }
}

impl Drop for PipeWireControl {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Quit);
    }
}

/// Um nó de saída visto no registro.
struct NodeGlobal {
    serial: u64,
    client: Option<u32>,
}

/// Um client ligado por `registry.bind`, com o que o info dele trouxe.
struct ClientEntry {
    _listener: ClientListener,
    _proxy: Client,
    pid: Option<u32>,
    binary: Option<String>,
}

/// Um fluxo de captura aberto. O ouvinte sai antes do fluxo.
struct Capture {
    _listener: StreamListener<Format>,
    _stream: StreamRc,
    /// O servidor destruiu o fluxo ou ele deu erro.
    failed: Rc<Cell<bool>>,
}

/// O formato negociado de um fluxo.
#[derive(Default)]
struct Format {
    rate: u32,
    channels: usize,
}

/// O estado da sessão, dentro da thread do PipeWire.
#[derive(Default)]
struct State {
    nodes: BTreeMap<u32, NodeGlobal>,
    clients: HashMap<u32, ClientEntry>,
    /// O conjunto do último `capture`.
    wanted: BTreeSet<u64>,
    captures: HashMap<u64, Capture>,
    /// Quando reabrir um fluxo destruído.
    retry: HashMap<u64, Instant>,
    /// O último `Graph` mandado.
    graph: Option<Vec<OutputNode>>,
}

impl State {
    fn output_nodes(&self) -> Vec<OutputNode> {
        self.nodes
            .iter()
            .map(|(&id, node)| {
                let client = node.client.and_then(|c| self.clients.get(&c));
                OutputNode {
                    id,
                    serial: node.serial,
                    client_pid: client.and_then(|c| c.pid),
                    binary: client.and_then(|c| c.binary.clone()),
                }
            })
            .collect()
    }

    /// Manda o `Graph` se a lista mudou desde o último.
    fn send_graph(&mut self, events: &UnboundedSender<Event>) {
        let nodes = self.output_nodes();
        if self.graph.as_ref() != Some(&nodes) {
            self.graph = Some(nodes.clone());
            let _ = events.send(Event::Graph(nodes));
        }
    }

    /// Deixa os fluxos como o `wanted` pede: fecha os que saíram (do pedido
    /// ou do grafo), larga os que o servidor destruiu e abre os que faltam,
    /// só para nós que estão no grafo.
    fn reconcile(&mut self, core: &CoreRc, events: &UnboundedSender<Event>) {
        let now = Instant::now();
        let in_graph: BTreeSet<u64> = self.nodes.values().map(|n| n.serial).collect();
        let wanted = &self.wanted;
        self.captures
            .retain(|serial, _| wanted.contains(serial) && in_graph.contains(serial));
        self.retry.retain(|serial, _| wanted.contains(serial));

        let failed: Vec<u64> = self
            .captures
            .iter()
            .filter(|(_, c)| c.failed.get())
            .map(|(&serial, _)| serial)
            .collect();
        for serial in failed {
            self.captures.remove(&serial);
            self.retry.insert(serial, now + REOPEN_DELAY);
        }

        for &serial in self.wanted.intersection(&in_graph) {
            if self.captures.contains_key(&serial)
                || self.retry.get(&serial).is_some_and(|&at| at > now)
            {
                continue;
            }
            self.retry.remove(&serial);
            match open_capture(core, serial, events.clone()) {
                Ok(capture) => {
                    self.captures.insert(serial, capture);
                }
                Err(_) => {
                    self.retry.insert(serial, now + REOPEN_DELAY);
                }
            }
        }
    }
}

/// A thread da sessão: conecta, avisa por `ready` e roda o laço até o
/// `Quit` ou a queda da conexão.
fn run(
    remote: Option<String>,
    timeout: Duration,
    events: UnboundedSender<Event>,
    commands: pw::channel::Receiver<Command>,
    ready: std_mpsc::Sender<Result<(), String>>,
) {
    let connected = (|| {
        let mainloop = pw::main_loop::MainLoopRc::new(None)?;
        let context = pw::context::ContextRc::new(&mainloop, None)?;
        let props = remote.map(|remote| {
            let mut props = PropertiesBox::new();
            props.insert("remote.name", remote);
            props
        });
        let core = context.connect_rc(props)?;
        let registry = core.get_registry_rc()?;
        Ok::<_, pw::Error>((mainloop, context, core, registry))
    })();
    let (mainloop, context, core, registry) = match connected {
        Ok(connected) => connected,
        Err(e) => {
            let _ = ready.send(Err(connect_error(&e)));
            return;
        }
    };

    let state = Rc::new(RefCell::new(State::default()));
    let ready = Rc::new(RefCell::new(Some(ready)));

    let registry_listener = {
        let (state, weak_registry, events) =
            (Rc::downgrade(&state), registry.downgrade(), events.clone());
        let removed = (state.clone(), events.clone());
        registry
            .add_listener_local()
            .global(move |global| {
                let (Some(state), Some(registry)) = (state.upgrade(), weak_registry.upgrade())
                else {
                    return;
                };
                on_global(&state, &registry, global, &events);
            })
            .global_remove(move |id| {
                let (state, events) = &removed;
                if let Some(state) = state.upgrade() {
                    let mut state = state.borrow_mut();
                    if state.nodes.remove(&id).is_some() {
                        state.send_graph(events);
                    }
                    state.clients.remove(&id);
                }
            })
            .register()
    };

    // A conexão responde quando o `sync` volta; aí manda o primeiro `Graph`.
    let pending = core.sync(0);
    let core_listener = {
        let (state, events, ready, quit) = (
            Rc::downgrade(&state),
            events.clone(),
            ready.clone(),
            mainloop.downgrade(),
        );
        let error_events = events.clone();
        let error_ready = ready.clone();
        let error_quit = mainloop.downgrade();
        core.add_listener_local()
            .done(move |id, seq| {
                if id != pw::core::PW_ID_CORE || pending.as_ref().ok() != Some(&seq) {
                    return;
                }
                if let Some(state) = state.upgrade() {
                    state.borrow_mut().send_graph(&events);
                }
                let ready = ready.borrow_mut().take();
                // Se quem abriu desistiu (o prazo dele acabou), a sessão sai.
                if ready.is_some_and(|ready| ready.send(Ok(())).is_err())
                    && let Some(mainloop) = quit.upgrade()
                {
                    mainloop.quit();
                }
            })
            .error(move |id, _seq, _res, message| {
                if id != pw::core::PW_ID_CORE {
                    return;
                }
                match error_ready.borrow_mut().take() {
                    Some(ready) => {
                        let _ = ready.send(Err(connect_error(&message)));
                    }
                    None => {
                        let _ = error_events.send(Event::Disconnected(DISCONNECTED.into()));
                    }
                }
                if let Some(mainloop) = error_quit.upgrade() {
                    mainloop.quit();
                }
            })
            .register()
    };

    let deadline = {
        let (ready, quit) = (ready.clone(), mainloop.downgrade());
        let timer = mainloop.loop_().add_timer(move |_| {
            if let Some(ready) = ready.borrow_mut().take() {
                let _ = ready.send(Err(no_answer(timeout)));
                if let Some(mainloop) = quit.upgrade() {
                    mainloop.quit();
                }
            }
        });
        let _ = timer.update_timer(Some(timeout), None);
        timer
    };

    let tick = {
        let (state, core, events) = (Rc::downgrade(&state), core.clone(), events.clone());
        let timer = mainloop.loop_().add_timer(move |_| {
            if let Some(state) = state.upgrade() {
                state.borrow_mut().reconcile(&core, &events);
            }
        });
        let _ = timer.update_timer(Some(TICK), Some(TICK));
        timer
    };

    let receiver = {
        let (state, core, events, quit) = (
            Rc::downgrade(&state),
            core.clone(),
            events.clone(),
            mainloop.downgrade(),
        );
        commands.attach(mainloop.loop_(), move |command| match command {
            Command::Capture(serials) => {
                if let Some(state) = state.upgrade() {
                    let mut state = state.borrow_mut();
                    state.wanted = serials.into_iter().collect();
                    state.reconcile(&core, &events);
                }
            }
            Command::Quit => {
                if let Some(mainloop) = quit.upgrade() {
                    mainloop.quit();
                }
            }
        })
    };

    mainloop.run();

    // Desmonta na ordem: fluxos e clients (proxies) antes da conexão.
    drop(receiver);
    drop(tick);
    drop(deadline);
    {
        let mut state = state.borrow_mut();
        state.captures.clear();
        state.clients.clear();
    }
    drop(core_listener);
    drop(registry_listener);
    drop(state);
    drop(registry);
    drop(core);
    drop(context);
    drop(mainloop);
}

/// Um objeto novo no registro: guarda os nós de saída e liga os clients.
fn on_global(
    state: &Rc<RefCell<State>>,
    registry: &RegistryRc,
    global: &GlobalObject<&DictRef>,
    events: &UnboundedSender<Event>,
) {
    let Some(props) = global.props else {
        return;
    };
    match global.type_ {
        ObjectType::Node if props.get("media.class") == Some(OUTPUT_CLASS) => {
            let Some(serial) = props.get("object.serial").and_then(|s| s.parse().ok()) else {
                return;
            };
            let client = props.get("client.id").and_then(|c| c.parse().ok());
            let mut state = state.borrow_mut();
            state.nodes.insert(global.id, NodeGlobal { serial, client });
            state.send_graph(events);
        }
        ObjectType::Client => {
            let Ok(proxy) = registry.bind::<Client, _>(global) else {
                return;
            };
            let id = global.id;
            let (weak, events) = (Rc::downgrade(state), events.clone());
            let listener = proxy
                .add_listener_local()
                .info(move |info| {
                    let Some(state) = weak.upgrade() else {
                        return;
                    };
                    let Some(props) = info.props() else {
                        return;
                    };
                    let mut state = state.borrow_mut();
                    if let Some(client) = state.clients.get_mut(&id) {
                        client.pid = props
                            .get("application.process.id")
                            .and_then(|p| p.parse().ok());
                        client.binary = props.get("application.process.binary").map(Into::into);
                    }
                    state.send_graph(&events);
                })
                .register();
            state.borrow_mut().clients.insert(
                id,
                ClientEntry {
                    _listener: listener,
                    _proxy: proxy,
                    pid: None,
                    binary: None,
                },
            );
        }
        _ => {}
    }
}

/// Abre um fluxo de entrada ligado ao nó `serial`.
fn open_capture(
    core: &CoreRc,
    serial: u64,
    events: UnboundedSender<Event>,
) -> Result<Capture, pw::Error> {
    let mut props = PropertiesBox::new();
    props.insert("media.type", "Audio");
    props.insert("media.category", "Capture");
    props.insert("node.name", NODE_NAME);
    props.insert("node.description", "Espectro do Lucerna");
    props.insert("application.name", "lucernad");
    props.insert("target.object", serial.to_string());
    props.insert("node.dont-fallback", "true");
    props.insert("node.dont-reconnect", "true");
    props.insert("node.dont-move", "true");
    props.insert("node.passive", "true");
    let stream = StreamRc::new(core.clone(), NODE_NAME, props)?;

    let failed = Rc::new(Cell::new(false));
    let flag = failed.clone();
    let listener = stream
        .add_local_listener_with_user_data(Format::default())
        .state_changed(move |_, _, _, new| {
            if matches!(new, StreamState::Error(_) | StreamState::Unconnected) {
                flag.set(true);
            }
        })
        .param_changed(|_, format, id, param| {
            let Some(param) = param else {
                return;
            };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let mut info = spa::param::audio::AudioInfoRaw::new();
            if info.parse(param).is_ok() {
                format.rate = info.rate();
                format.channels = info.channels() as usize;
            }
        })
        .process(move |stream, format| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            if format.channels == 0 {
                return;
            }
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let (offset, size) = (data.chunk().offset() as usize, data.chunk().size() as usize);
            let Some(bytes) = data.data() else {
                return;
            };
            let end = offset.saturating_add(size).min(bytes.len());
            let mono = downmix(&bytes[offset.min(end)..end], format.channels);
            if !mono.is_empty() {
                let _ = events.send(Event::Samples {
                    serial,
                    rate: format.rate,
                    mono,
                });
            }
        })
        .register()?;

    let format = enum_format();
    let mut params = [Pod::from_bytes(&format).ok_or(pw::Error::CreationFailed)?];
    stream.connect(
        spa::utils::Direction::Input,
        None,
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    Ok(Capture {
        _listener: listener,
        _stream: stream,
        failed,
    })
}

/// O `EnumFormat` dos fluxos: `F32LE` só, com a taxa e os canais do grafo.
fn enum_format() -> Vec<u8> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    let object = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .map(|(cursor, _)| cursor.into_inner())
    .unwrap_or_default()
}

/// Amostras `F32LE` intercaladas em `channels` canais → a média por quadro.
fn downmix(bytes: &[u8], channels: usize) -> Vec<f32> {
    let frame = channels * size_of::<f32>();
    bytes
        .chunks_exact(frame)
        .map(|frame| {
            let sum: f32 = frame
                .chunks_exact(size_of::<f32>())
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .sum();
            sum / channels as f32
        })
        .collect()
}
