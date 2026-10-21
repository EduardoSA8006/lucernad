//! O backend de verdade: o protocolo de controle de dados do Wayland
//! (`ext-data-control-v1`, ou o `zwlr-data-control-unstable-v1` se o
//! compositor só tiver esse), numa `std::thread` por sessão.
//!
//! A thread da sessão espera com `poll` no fd da conexão e numa ponta de um
//! pipe de despertar; largar o [`Control`] fecha a outra ponta, e a thread
//! sai, levando a conexão. Os eventos vão ao laço por um
//! `tokio::sync::mpsc`. Os pedidos (`receive`, `offer`) saem direto da
//! thread de quem chama: os objetos do `wayland-client` são `Send + Sync`.
//! Cada `send` da nossa fonte é escrito numa thread própria, para quem cola
//! devagar não segurar o laço.

use std::fs::File;
use std::io::{ErrorKind, PipeReader, PipeWriter, Write};
use std::net::Shutdown;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use rustix::event::{PollFd, PollFlags, poll};
use tokio::sync::mpsc::{self, UnboundedSender};
use wayland_client::backend::WaylandError;
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, event_created_child};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1 as ext_device, ext_data_control_manager_v1 as ext_manager,
    ext_data_control_offer_v1 as ext_offer, ext_data_control_source_v1 as ext_source,
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1 as wlr_device, zwlr_data_control_manager_v1 as wlr_manager,
    zwlr_data_control_offer_v1 as wlr_offer, zwlr_data_control_source_v1 as wlr_source,
};

use crate::backend::{Backend, Control, Event, Session, Source};

/// O prazo para o compositor responder à abertura.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// O texto do compositor que não respondeu no [`CONNECT_TIMEOUT`].
pub const NOT_RESPONDING: &str = "O compositor não respondeu em 2 s.";
/// O texto do compositor sem nenhum dos dois protocolos.
pub const NO_PROTOCOL: &str = "O compositor não oferece o controle da área de transferência \
                               (ext-data-control-v1 nem wlr-data-control).";
/// O texto do compositor sem `wl_seat`.
pub const NO_SEAT: &str = "O compositor não tem nenhum wl_seat.";
/// O `Err` do `receive` de uma oferta que já não é a atual.
pub const SELECTION_CHANGED: &str = "a seleção mudou";

/// O texto da falha ao conectar ao socket.
pub fn connect_failed(socket: &Path, e: impl std::fmt::Display) -> String {
    format!(
        "Não consegui conectar ao Wayland ({}): {e}.",
        socket.display()
    )
}

/// O texto da queda da conexão (`Event::Disconnected`).
pub fn disconnected(e: impl std::fmt::Display) -> String {
    format!("O compositor fechou a conexão: {e}.")
}

/// Qual dos dois protocolos de controle de dados.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    /// `ext-data-control-v1`.
    Ext,
    /// `zwlr-data-control-unstable-v1`.
    Wlr,
}

/// O Wayland como [`Backend`].
pub struct Wayland {
    /// Só este protocolo (a sonda testa os dois); `None` prefere o `ext`.
    pub only: Option<Protocol>,
}

impl Wayland {
    /// O compositor que tiver: o `ext` se houver, senão o `wlr`.
    pub fn system() -> Wayland {
        Wayland { only: None }
    }
}

impl Backend for Wayland {
    fn open(&self, socket: &Path) -> Result<Session, String> {
        let stream = UnixStream::connect(socket).map_err(|e| connect_failed(socket, e))?;
        // Uma cópia do socket, para derrubar a conexão se o compositor não
        // responder no prazo (a thread presa no `read` acorda com o EOF).
        let killer = stream.try_clone().map_err(|e| connect_failed(socket, e))?;
        let (wake_reader, wake) = std::io::pipe().map_err(|e| connect_failed(socket, e))?;
        let (events_tx, events) = mpsc::unbounded_channel();
        let (ready_tx, ready) = std_mpsc::channel();
        let only = self.only;
        let thread = std::thread::Builder::new()
            .name("lucernad-wayland".into())
            .spawn(move || run(stream, only, wake_reader, events_tx, ready_tx))
            .map_err(|e| connect_failed(socket, e))?;
        match ready.recv_timeout(CONNECT_TIMEOUT) {
            Ok(Ok(handles)) => Ok(Session {
                events,
                control: Box::new(WaylandControl {
                    handles,
                    wake: Some(wake),
                    thread: Some(thread),
                }),
            }),
            Ok(Err(e)) => Err(e),
            Err(std_mpsc::RecvTimeoutError::Timeout) => {
                let _ = killer.shutdown(Shutdown::Both);
                Err(NOT_RESPONDING.to_string())
            }
            Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                Err(disconnected("a sessão parou ao abrir"))
            }
        }
    }
}

/// O gerente do protocolo escolhido.
enum Manager {
    Ext(ext_manager::ExtDataControlManagerV1),
    Wlr(wlr_manager::ZwlrDataControlManagerV1),
}

/// O dispositivo do `wl_seat`.
enum Device {
    Ext(ext_device::ExtDataControlDeviceV1),
    Wlr(wlr_device::ZwlrDataControlDeviceV1),
}

/// Uma oferta de seleção.
enum Offer {
    Ext(ext_offer::ExtDataControlOfferV1),
    Wlr(wlr_offer::ZwlrDataControlOfferV1),
}

/// Uma fonte nossa.
enum SourceProxy {
    Ext(ext_source::ExtDataControlSourceV1),
    Wlr(wlr_source::ZwlrDataControlSourceV1),
}

impl Manager {
    fn bind(globals: &GlobalList, qh: &QueueHandle<State>, only: Option<Protocol>) -> Option<Self> {
        if only != Some(Protocol::Wlr)
            && let Ok(m) = globals.bind(qh, 1..=1, ())
        {
            return Some(Manager::Ext(m));
        }
        if only != Some(Protocol::Ext)
            && let Ok(m) = globals.bind(qh, 1..=2, ())
        {
            return Some(Manager::Wlr(m));
        }
        None
    }

    fn device(&self, seat: &WlSeat, qh: &QueueHandle<State>) -> Device {
        match self {
            Manager::Ext(m) => Device::Ext(m.get_data_device(seat, qh, ())),
            Manager::Wlr(m) => Device::Wlr(m.get_data_device(seat, qh, ())),
        }
    }

    fn create_source(&self, qh: &QueueHandle<State>, data: SourceData) -> SourceProxy {
        match self {
            Manager::Ext(m) => SourceProxy::Ext(m.create_data_source(qh, data)),
            Manager::Wlr(m) => SourceProxy::Wlr(m.create_data_source(qh, data)),
        }
    }
}

impl Device {
    fn set_selection(&self, source: &SourceProxy) {
        match (self, source) {
            (Device::Ext(d), SourceProxy::Ext(s)) => d.set_selection(Some(s)),
            (Device::Wlr(d), SourceProxy::Wlr(s)) => d.set_selection(Some(s)),
            // O gerente cria a fonte do mesmo protocolo do dispositivo.
            _ => {}
        }
    }
}

impl Offer {
    fn mimes(&self) -> Vec<String> {
        let data = match self {
            Offer::Ext(o) => o.data::<OfferData>(),
            Offer::Wlr(o) => o.data::<OfferData>(),
        };
        data.map(|d| lock(&d.0).clone()).unwrap_or_default()
    }

    fn receive(&self, mime: &str, fd: std::os::fd::BorrowedFd<'_>) {
        match self {
            Offer::Ext(o) => o.receive(mime.to_string(), fd),
            Offer::Wlr(o) => o.receive(mime.to_string(), fd),
        }
    }

    fn destroy(&self) {
        match self {
            Offer::Ext(o) => o.destroy(),
            Offer::Wlr(o) => o.destroy(),
        }
    }
}

impl SourceProxy {
    fn offer(&self, mime: &str) {
        match self {
            SourceProxy::Ext(s) => s.offer(mime.to_string()),
            SourceProxy::Wlr(s) => s.offer(mime.to_string()),
        }
    }

    fn destroy(&self) {
        match self {
            SourceProxy::Ext(s) => s.destroy(),
            SourceProxy::Wlr(s) => s.destroy(),
        }
    }

    fn id(&self) -> wayland_client::backend::ObjectId {
        match self {
            SourceProxy::Ext(s) => s.id(),
            SourceProxy::Wlr(s) => s.id(),
        }
    }
}

/// Os tipos de uma oferta, que chegam nos eventos `offer` dela.
#[derive(Default)]
struct OfferData(Mutex<Vec<String>>);

/// Os bytes de cada tipo de uma fonte nossa.
struct SourceData {
    offers: Vec<(String, Arc<[u8]>)>,
}

/// O que a thread da sessão e o [`Control`] dividem.
#[derive(Default)]
struct Shared {
    /// A seleção atual e o número dela.
    current: Mutex<Option<(u64, Offer)>>,
    /// A nossa fonte, enquanto for a seleção.
    owned: Mutex<Option<SourceProxy>>,
}

/// O que a abertura entrega ao [`Control`].
struct Handles {
    conn: Connection,
    qh: QueueHandle<State>,
    manager: Manager,
    device: Device,
    shared: Arc<Shared>,
}

/// O estado da fila da sessão.
struct State {
    events: UnboundedSender<Event>,
    shared: Arc<Shared>,
    /// O número da última seleção.
    next: u64,
    /// O dispositivo terminou (o `finished`).
    finished: bool,
}

impl State {
    fn selection(&mut self, offer: Option<Offer>) {
        let mut current = lock(&self.shared.current);
        if let Some((_, old)) = current.take() {
            old.destroy();
        }
        let event = match offer {
            Some(offer) => {
                self.next += 1;
                let mimes = offer.mimes();
                *current = Some((self.next, offer));
                Event::Selection {
                    offer: self.next,
                    mimes,
                }
            }
            None => Event::Cleared,
        };
        drop(current);
        let _ = self.events.send(event);
    }

    fn send(data: &SourceData, mime: &str, fd: std::os::fd::OwnedFd) {
        let Some((_, bytes)) = data.offers.iter().find(|(m, _)| m == mime) else {
            return;
        };
        let bytes = bytes.clone();
        // Quem cola pode fechar antes do fim: o erro de pipe quebrado fica
        // de fora.
        let _ = std::thread::Builder::new()
            .name("lucernad-colar".into())
            .spawn(move || {
                let _ = File::from(fd).write_all(&bytes);
            });
    }

    fn cancelled(&mut self, source: &SourceProxy) {
        let mut owned = lock(&self.shared.owned);
        if owned.as_ref().is_some_and(|s| s.id() == source.id()) {
            *owned = None;
            let _ = self.events.send(Event::Cancelled);
        }
        drop(owned);
        source.destroy();
    }
}

/// Os `Dispatch` de um dos protocolos (os dois têm a mesma forma).
macro_rules! data_control {
    ($variant:ident, $manager:ident :: $Manager:ident, $device:ident :: $Device:ident,
     $offer:ident :: $Offer:ident, $source:ident :: $Source:ident) => {
        impl Dispatch<$manager::$Manager, ()> for State {
            fn event(
                _: &mut Self,
                _: &$manager::$Manager,
                _: $manager::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }

        impl Dispatch<$device::$Device, ()> for State {
            fn event(
                state: &mut Self,
                _: &$device::$Device,
                event: $device::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                match event {
                    // Os tipos chegam na própria oferta (OfferData).
                    $device::Event::DataOffer { .. } => {}
                    $device::Event::Selection { id } => state.selection(id.map(Offer::$variant)),
                    // A seleção primária não entra no histórico.
                    $device::Event::PrimarySelection { id: Some(offer) } => offer.destroy(),
                    $device::Event::Finished => state.finished = true,
                    _ => {}
                }
            }

            event_created_child!(State, $device::$Device, [
                $device::EVT_DATA_OFFER_OPCODE => ($offer::$Offer, OfferData::default()),
            ]);
        }

        impl Dispatch<$offer::$Offer, OfferData> for State {
            fn event(
                _: &mut Self,
                _: &$offer::$Offer,
                event: $offer::Event,
                data: &OfferData,
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                if let $offer::Event::Offer { mime_type } = event {
                    lock(&data.0).push(mime_type);
                }
            }
        }

        impl Dispatch<$source::$Source, SourceData> for State {
            fn event(
                state: &mut Self,
                source: &$source::$Source,
                event: $source::Event,
                data: &SourceData,
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                match event {
                    $source::Event::Send { mime_type, fd } => State::send(data, &mime_type, fd),
                    $source::Event::Cancelled => {
                        state.cancelled(&SourceProxy::$variant(source.clone()))
                    }
                    _ => {}
                }
            }
        }
    };
}

data_control!(
    Ext,
    ext_manager::ExtDataControlManagerV1,
    ext_device::ExtDataControlDeviceV1,
    ext_offer::ExtDataControlOfferV1,
    ext_source::ExtDataControlSourceV1
);
data_control!(
    Wlr,
    wlr_manager::ZwlrDataControlManagerV1,
    wlr_device::ZwlrDataControlDeviceV1,
    wlr_offer::ZwlrDataControlOfferV1,
    wlr_source::ZwlrDataControlSourceV1
);

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wayland_client::protocol::wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: wayland_client::protocol::wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// A conexão, o registro, o gerente, o `wl_seat` e o dispositivo, com um
/// `roundtrip` para a seleção atual já chegar.
fn setup(
    stream: UnixStream,
    only: Option<Protocol>,
    events: UnboundedSender<Event>,
) -> Result<(EventQueue<State>, State, Handles), String> {
    let conn = Connection::from_socket(stream).map_err(disconnected)?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(disconnected)?;
    let qh = queue.handle();
    let manager = Manager::bind(&globals, &qh, only).ok_or_else(|| NO_PROTOCOL.to_string())?;
    let seat: WlSeat = globals
        .bind(&qh, 1..=1, ())
        .map_err(|_| NO_SEAT.to_string())?;
    let device = manager.device(&seat, &qh);
    let shared = Arc::new(Shared::default());
    let mut state = State {
        events,
        shared: shared.clone(),
        next: 0,
        finished: false,
    };
    queue.roundtrip(&mut state).map_err(disconnected)?;
    let handles = Handles {
        conn,
        qh,
        manager,
        device,
        shared,
    };
    Ok((queue, state, handles))
}

/// A thread da sessão: abre e, se a abertura deu certo, despacha até a
/// conexão cair ou o [`Control`] ser largado.
fn run(
    stream: UnixStream,
    only: Option<Protocol>,
    wake: PipeReader,
    events: UnboundedSender<Event>,
    ready: std_mpsc::Sender<Result<Handles, String>>,
) {
    let (mut queue, mut state, handles) = match setup(stream, only, events.clone()) {
        Ok(opened) => opened,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let conn = handles.conn.clone();
    if ready.send(Ok(handles)).is_err() {
        // Passou do prazo: quem abriu já desistiu.
        return;
    }
    if let Err(why) = dispatch(&conn, &mut queue, &mut state, &wake) {
        let _ = events.send(Event::Disconnected(why));
    }
}

/// O laço da thread. `Ok` quando o [`Control`] foi largado; `Err` com o
/// texto da queda.
fn dispatch(
    conn: &Connection,
    queue: &mut EventQueue<State>,
    state: &mut State,
    wake: &PipeReader,
) -> Result<(), String> {
    loop {
        queue.dispatch_pending(state).map_err(disconnected)?;
        if state.finished {
            return Err(disconnected(
                "o dispositivo da área de transferência terminou",
            ));
        }
        match conn.flush() {
            Ok(()) => {}
            Err(WaylandError::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
            Err(e) => return Err(disconnected(e)),
        }
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let (readable, woken) = {
            let fd = guard.connection_fd();
            let mut fds = [
                PollFd::new(&fd, PollFlags::IN),
                PollFd::new(wake, PollFlags::IN),
            ];
            match poll(&mut fds, None) {
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(disconnected(e)),
            }
            (!fds[0].revents().is_empty(), !fds[1].revents().is_empty())
        };
        if woken {
            // A outra ponta do pipe fechou: o Control foi largado.
            return Ok(());
        }
        if readable {
            match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => return Err(disconnected(e)),
            }
        }
    }
}

/// O [`Control`] do Wayland.
struct WaylandControl {
    handles: Handles,
    /// Largar fecha o pipe de despertar, e a thread da sessão sai.
    wake: Option<PipeWriter>,
    thread: Option<JoinHandle<()>>,
}

impl Control for WaylandControl {
    fn receive(&self, offer: u64, mime: &str) -> Result<PipeReader, String> {
        let (reader, writer) = std::io::pipe().map_err(|e| e.to_string())?;
        {
            let current = lock(&self.handles.shared.current);
            match current.as_ref() {
                Some((n, o)) if *n == offer => o.receive(mime, writer.as_fd()),
                _ => return Err(SELECTION_CHANGED.to_string()),
            }
        }
        self.handles.conn.flush().map_err(disconnected)?;
        // A ponta de escrita sai daqui na hora: só quem copiou a segura, e o
        // EOF chega quando ele fecha.
        drop(writer);
        Ok(reader)
    }

    fn offer(&self, source: Source) {
        let h = &self.handles;
        // O lock segura o `cancelled` da fonte anterior até a troca: senão
        // ele acharia que a fonte nova foi substituída.
        let mut owned = lock(&h.shared.owned);
        let proxy = h.manager.create_source(
            &h.qh,
            SourceData {
                offers: source.offers.clone(),
            },
        );
        for (mime, _) in &source.offers {
            proxy.offer(mime);
        }
        h.device.set_selection(&proxy);
        if let Some(old) = owned.replace(proxy) {
            old.destroy();
        }
        drop(owned);
        let _ = h.conn.flush();
    }
}

impl Drop for WaylandControl {
    fn drop(&mut self) {
        drop(self.wake.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
