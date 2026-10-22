//! O uevent do kernel: um socket `NETLINK_KOBJECT_UEVENT` (grupo 1, o do
//! kernel) lido numa `std::thread`, sem `libudev`.
//!
//! O kernel manda um `change` do `backlight` a cada escrita no `brightness`
//! (`SOURCE=sysfs`, também a do slider do Plasma e a da tecla do firmware) e
//! um `change` do `drm` com `HOTPLUG=1` ao plugar e tirar um monitor. Só
//! passam mensagens com `pid` 0 na origem (o kernel) e inteiras (buffer de
//! [`BUFFER`] bytes; truncada é descartada). `ENOBUFS` no `recv` quer dizer
//! que o kernel descartou eventos: vira [`Uevent::Lost`] (o laço relê a
//! integrada e redescobre), não erro.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rustix::io::Errno;
use rustix::net::netlink::{self, SocketAddrNetlink};
use rustix::net::sockopt::{Timeout, set_socket_timeout};
use rustix::net::{AddressFamily, RecvFlags, SocketFlags, SocketType};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// O tamanho do buffer de leitura; mensagem maior é descartada.
pub const BUFFER: usize = 8 * 1024;
/// O grupo multicast dos uevents do kernel.
const KERNEL_GROUP: u32 = 1;
/// O `SO_RCVTIMEO`: de quanto em quanto tempo a thread confere se o
/// receptor foi largado.
const RECV_TIMEOUT: Duration = Duration::from_secs(1);

/// Um uevent do kernel, ou o aviso de que alguns se perderam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Uevent {
    /// Um evento (`ACTION`, `SUBSYSTEM`, `DEVPATH`).
    Event {
        action: String,
        subsystem: String,
        devpath: String,
    },
    /// `ENOBUFS` no `recv`: o kernel descartou eventos; releia e redescubra.
    Lost,
}

/// O último componente de um `DEVPATH` (`…/intel_backlight` →
/// `intel_backlight`).
pub fn devpath_name(devpath: &str) -> &str {
    devpath
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(devpath)
}

/// Lê uma mensagem do kernel (`ação@devpath\0CHAVE=valor\0…`). O cabeçalho
/// `libudev` (a retransmissão do udevd), a mensagem sem `@` no cabeçalho, sem
/// `SUBSYSTEM` ou fora de UTF-8 → `None`.
pub fn parse_uevent(msg: &[u8]) -> Option<Uevent> {
    let mut fields = msg.split(|&b| b == 0);
    let header = std::str::from_utf8(fields.next()?).ok()?;
    if header == "libudev" {
        return None;
    }
    let (header_action, header_devpath) = header.split_once('@')?;
    let mut action = None;
    let mut subsystem = None;
    let mut devpath = None;
    for field in fields {
        let Ok(field) = std::str::from_utf8(field) else {
            continue;
        };
        match field.split_once('=') {
            Some(("ACTION", v)) => action = Some(v),
            Some(("SUBSYSTEM", v)) => subsystem = Some(v),
            Some(("DEVPATH", v)) => devpath = Some(v),
            _ => {}
        }
    }
    let action = action.unwrap_or(header_action);
    let devpath = devpath.unwrap_or(header_devpath);
    let subsystem = subsystem?;
    if action.is_empty() || devpath.is_empty() || subsystem.is_empty() {
        return None;
    }
    Some(Uevent::Event {
        action: action.to_owned(),
        subsystem: subsystem.to_owned(),
        devpath: devpath.to_owned(),
    })
}

/// A fonte de uevents do módulo (o kernel, ou o dublê dos testes).
pub trait Uevents: Send + Sync + 'static {
    /// Começa a escutar. Largar o receptor encerra a escuta (a thread do
    /// kernel, em até 1 s).
    fn watch(&self) -> Result<UnboundedReceiver<Uevent>, String>;
}

/// O que uma leitura da fonte deu (o `recv` do socket, ou uma fonte falsa
/// nos testes da thread).
#[derive(Debug)]
pub enum Received {
    /// `len` bytes no buffer; `from_kernel` = `pid` 0 na origem;
    /// `truncated` = a mensagem não coube no buffer.
    Message {
        len: usize,
        from_kernel: bool,
        truncated: bool,
    },
    /// Nada no prazo do `SO_RCVTIMEO` (ou interrompido): tenta de novo.
    Timeout,
    /// `ENOBUFS`: eventos perdidos.
    Lost,
    /// Erro sem volta: a thread termina (o receptor vê o canal fechado).
    Closed(String),
}

static THREADS: AtomicUsize = AtomicUsize::new(0);

/// Quantas threads de leitura de uevent estão vivas (para os testes).
pub fn threads() -> usize {
    THREADS.load(Ordering::SeqCst)
}

/// Desconta a thread do [`threads`] ao terminar, até num pânico.
struct Counted;

impl Drop for Counted {
    fn drop(&mut self) {
        THREADS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Sobe a thread de leitura sobre `recv` (que enche o buffer dado). A thread
/// termina quando o receptor é largado (conferido a cada leitura, então
/// `recv` precisa voltar de tempos em tempos: o `SO_RCVTIMEO`) ou quando
/// `recv` dá [`Received::Closed`].
pub fn spawn_reader<F>(mut recv: F) -> Result<UnboundedReceiver<Uevent>, String>
where
    F: FnMut(&mut [u8]) -> Received + Send + 'static,
{
    let (tx, rx) = unbounded_channel();
    THREADS.fetch_add(1, Ordering::SeqCst);
    let counted = Counted;
    std::thread::Builder::new()
        .name("lucernad-uevent".into())
        .spawn(move || {
            let _counted = counted;
            read_loop(&mut recv, &tx);
        })
        .map_err(|e| format!("não consegui subir a thread do uevent: {e}"))?;
    Ok(rx)
}

/// Só o que o módulo usa vai ao canal (que é ilimitado): o `backlight` e o
/// `drm`.
fn wanted(event: &Uevent) -> bool {
    matches!(event, Uevent::Event { subsystem, .. } if subsystem == "backlight" || subsystem == "drm")
}

fn read_loop(recv: &mut dyn FnMut(&mut [u8]) -> Received, tx: &UnboundedSender<Uevent>) {
    let mut buf = vec![0u8; BUFFER];
    while !tx.is_closed() {
        let event = match recv(&mut buf) {
            Received::Message {
                len,
                from_kernel: true,
                truncated: false,
            } => parse_uevent(&buf[..len.min(buf.len())]).filter(wanted),
            Received::Message { .. } | Received::Timeout => None,
            Received::Lost => Some(Uevent::Lost),
            Received::Closed(error) => {
                eprintln!("lucernad: o uevent do kernel parou: {error:?}");
                return;
            }
        };
        if let Some(event) = event
            && tx.send(event).is_err()
        {
            return;
        }
    }
}

/// Os uevents do kernel por um socket `NETLINK_KOBJECT_UEVENT` (só em
/// produção; nenhum teste abre o socket de verdade).
pub struct KernelUevents;

impl Uevents for KernelUevents {
    fn watch(&self) -> Result<UnboundedReceiver<Uevent>, String> {
        let fd = rustix::net::socket_with(
            AddressFamily::NETLINK,
            SocketType::DGRAM,
            SocketFlags::CLOEXEC,
            Some(netlink::KOBJECT_UEVENT),
        )
        .map_err(|e| format!("não consegui abrir o socket do uevent: {e}"))?;
        rustix::net::bind(&fd, &SocketAddrNetlink::new(0, KERNEL_GROUP))
            .map_err(|e| format!("não consegui escutar o uevent do kernel: {e}"))?;
        set_socket_timeout(&fd, Timeout::Recv, Some(RECV_TIMEOUT))
            .map_err(|e| format!("não consegui pôr o prazo no socket do uevent: {e}"))?;
        spawn_reader(move |buf: &mut [u8]| {
            match rustix::net::recvfrom(&fd, &mut *buf, RecvFlags::TRUNC) {
                Ok((got, len, addr)) => {
                    let from_kernel = addr
                        .and_then(|addr| SocketAddrNetlink::try_from(addr).ok())
                        .is_some_and(|addr| addr.pid() == 0);
                    Received::Message {
                        len: got,
                        from_kernel,
                        truncated: len > got,
                    }
                }
                Err(Errno::AGAIN | Errno::INTR) => Received::Timeout,
                Err(Errno::NOBUFS) => Received::Lost,
                Err(e) => Received::Closed(e.to_string()),
            }
        })
    }
}
