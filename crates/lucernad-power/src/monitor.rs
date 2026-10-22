//! A thread de cada monitor DDC: dona do [`I2cBus`] dele, recebe os pedidos
//! por um `std::sync::mpsc` e responde por `oneshot`.
//!
//! O laço nunca espera um monitor sem prazo (quem espera é o
//! [`Link::request`] do chamador, com `tokio::time::timeout`). Uma thread presa
//! num `read` do driver nunca é morta: é o vazamento aceito, que some quando
//! o monitor sai (o canal fecha e a thread termina ao voltar) ou o daemon
//! termina.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use tokio::sync::oneshot;

use crate::ddc::{DdcError, DdcTiming, Vcp, get_brightness, set_brightness};
use crate::i2c::{I2cBus, I2cOpener};

/// O que se pede ao monitor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cmd {
    /// Ler o brilho (Get VCP).
    Get,
    /// Gravar o brilho e reler (Set VCP e Get VCP).
    Set(u16),
}

struct Job {
    cmd: Cmd,
    reply: oneshot::Sender<Result<Vcp, DdcError>>,
}

/// A ponta do laço para a thread de um monitor; largar encerra a thread
/// (quando ela voltar do pedido em curso).
pub(crate) struct Link {
    tx: Sender<Job>,
    opener: Arc<dyn I2cOpener>,
    bus: u32,
    timing: DdcTiming,
}

impl Link {
    /// Sobe a thread do monitor no barramento `bus` (aberto no primeiro
    /// pedido, e de novo depois de um erro de acesso).
    pub(crate) fn spawn(opener: Arc<dyn I2cOpener>, bus: u32, timing: DdcTiming) -> Link {
        let tx = start(opener.clone(), bus, timing);
        Link {
            tx,
            opener,
            bus,
            timing,
        }
    }

    /// Manda `cmd` à thread; a resposta chega pelo receptor (fechado sem
    /// resposta se a thread morreu no meio). Uma thread morta é trocada por
    /// outra.
    pub(crate) fn request(&mut self, cmd: Cmd) -> oneshot::Receiver<Result<Vcp, DdcError>> {
        let (reply, rx) = oneshot::channel();
        if let Err(back) = self.tx.send(Job { cmd, reply }) {
            eprintln!(
                "lucernad: a thread do monitor no i2c-{} tinha parado; subindo outra",
                self.bus
            );
            self.tx = start(self.opener.clone(), self.bus, self.timing);
            // Um canal novo aceita; se não aceitar, o receptor fecha sozinho.
            let _ = self.tx.send(back.0);
        }
        rx
    }
}

fn start(opener: Arc<dyn I2cOpener>, bus: u32, timing: DdcTiming) -> Sender<Job> {
    let (tx, rx) = channel();
    let spawned = std::thread::Builder::new()
        .name(format!("lucernad-ddc-{bus}"))
        .spawn(move || serve(&*opener, bus, &timing, &rx));
    if let Err(e) = spawned {
        // Sem thread, o `rx` cai e cada pedido volta sem resposta (o monitor
        // fica com o erro de não respondeu).
        eprintln!("lucernad: não consegui criar a thread do monitor no i2c-{bus}: {e}");
    }
    tx
}

fn serve(opener: &dyn I2cOpener, bus: u32, timing: &DdcTiming, rx: &Receiver<Job>) {
    let mut open: Option<Box<dyn I2cBus>> = None;
    for job in rx {
        let result = run(opener, bus, timing, &mut open, job.cmd);
        if let Err(DdcError::Io(detail)) = &result {
            eprintln!("lucernad: o monitor no i2c-{bus} falhou: {detail}");
        }
        if matches!(
            result,
            Err(DdcError::NoAccess | DdcError::NoDevice(_) | DdcError::Io(_))
        ) {
            // Abre de novo no próximo pedido (a regra udev instalada, o
            // módulo carregado).
            open = None;
        }
        let _ = job.reply.send(result);
    }
}

fn run(
    opener: &dyn I2cOpener,
    bus: u32,
    timing: &DdcTiming,
    open: &mut Option<Box<dyn I2cBus>>,
    cmd: Cmd,
) -> Result<Vcp, DdcError> {
    if open.is_none() {
        *open = Some(opener.open(bus).map_err(|e| DdcError::from_open(bus, &e))?);
    }
    let Some(device) = open.as_mut() else {
        return Err(DdcError::NoAnswer);
    };
    match cmd {
        Cmd::Get => get_brightness(&mut **device, timing),
        Cmd::Set(value) => set_brightness(&mut **device, value, timing),
    }
}
