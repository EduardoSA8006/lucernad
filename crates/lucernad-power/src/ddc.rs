//! O protocolo DDC/CI (VESA DDC/CI 1.1) sobre um [`I2cBus`]: só o Get VCP e
//! o Set VCP do brilho (`0x10`) e a leitura do EDID em `0x50`.
//!
//! As mensagens são funções puras ([`get_request`], [`set_request`],
//! [`parse_reply`]); as transações ([`get_brightness`], [`set_brightness`],
//! [`read_edid`]) seguram o `flock` do barramento ([`I2cBus::lock`]) do
//! começo ao fim de cada tentativa, como o libddcutil, para não atropelar o
//! PowerDevil ou o `ddcutil` no mesmo barramento.

use std::io;
use std::thread;
use std::time::Duration;

use rustix::io::Errno;

use crate::i2c::I2cBus;

/// O endereço DDC/CI do monitor.
pub const DDC_ADDR: u16 = 0x37;
/// O endereço da EEPROM do EDID.
pub const EDID_ADDR: u16 = 0x50;
/// O VCP do brilho; o único que este módulo grava.
pub const VCP_BRIGHTNESS: u8 = 0x10;
/// Novas tentativas depois da primeira, quando o monitor não responde ou
/// responde fora do padrão.
pub const RETRIES: usize = 3;

/// O primeiro byte de toda escrita do host (o endereço de origem, `0x51`).
const HOST_SOURCE: u8 = 0x51;
/// O endereço de destino na conta do checksum das escritas (`0x37 << 1`).
const WRITE_CHECKSUM_INIT: u8 = 0x6E;
/// O endereço virtual do host na conta do checksum das respostas.
const REPLY_CHECKSUM_INIT: u8 = 0x50;
/// O primeiro byte de toda resposta do monitor (`0x37 << 1`).
const MONITOR_SOURCE: u8 = 0x6E;
/// O tamanho da resposta ao Get VCP: origem, comprimento, 8 de carga e o
/// checksum.
const GET_REPLY_LEN: usize = 11;
const OP_GET_VCP: u8 = 0x01;
const OP_GET_VCP_REPLY: u8 = 0x02;
const OP_SET_VCP: u8 = 0x03;

/// Os atrasos do padrão (os mesmos do `ddcutil`).
#[derive(Clone, Copy, Debug)]
pub struct DdcTiming {
    /// Entre o pedido do Get VCP e a leitura da resposta.
    pub get_delay: Duration,
    /// Depois do Set VCP, antes da releitura.
    pub set_delay: Duration,
    /// Antes de uma nova tentativa.
    pub retry_delay: Duration,
}

impl DdcTiming {
    /// 40 ms, 50 ms e 100 ms.
    pub fn standard() -> DdcTiming {
        DdcTiming {
            get_delay: Duration::from_millis(40),
            set_delay: Duration::from_millis(50),
            retry_delay: Duration::from_millis(100),
        }
    }
}

/// Um valor de VCP lido do monitor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vcp {
    pub current: u16,
    pub max: u16,
}

/// O que deu errado ao falar com o monitor.
#[derive(Debug, PartialEq, Eq)]
pub enum DdcError {
    /// Sem permissão no `/dev/i2c-N` (`EACCES`/`EPERM`).
    NoAccess,
    /// O `/dev/i2c-N` não existe (`ENOENT`).
    NoDevice(u32),
    /// O monitor não respondeu (ou só respondeu "ocupado").
    NoAnswer,
    /// A resposta veio fora do padrão.
    BadReply,
    /// O monitor não tem o VCP pedido.
    Unsupported,
    /// Outro erro de E/S.
    Io(String),
}

impl DdcError {
    /// O erro de abrir o `/dev/i2c-<bus>`.
    pub fn from_open(bus: u32, err: &io::Error) -> DdcError {
        match err.kind() {
            io::ErrorKind::PermissionDenied => DdcError::NoAccess,
            io::ErrorKind::NotFound => DdcError::NoDevice(bus),
            _ => DdcError::Io(err.to_string()),
        }
    }

    /// O erro de uma escrita ou leitura no meio da transação. Sem `ACK` do
    /// monitor (`ENXIO`, `EREMOTEIO`, `EIO`, `ETIMEDOUT`, `EAGAIN`) é falta de resposta.
    fn from_transfer(err: &io::Error) -> DdcError {
        if err.kind() == io::ErrorKind::PermissionDenied {
            return DdcError::NoAccess;
        }
        match Errno::from_io_error(err) {
            Some(Errno::NXIO | Errno::IO | Errno::REMOTEIO | Errno::TIMEDOUT | Errno::AGAIN) => {
                DdcError::NoAnswer
            }
            _ => DdcError::Io(err.to_string()),
        }
    }

    /// O texto para a linha da tela (os do Formato).
    pub fn message(&self) -> String {
        match self {
            DdcError::NoAccess => "Sem acesso ao barramento do monitor: instale a regra udev do \
                                   lucernad (veja o README)."
                .to_string(),
            DdcError::NoDevice(bus) => {
                format!("Sem o /dev/i2c-{bus}: carregue o módulo i2c-dev (veja o README).")
            }
            DdcError::NoAnswer => "O monitor não respondeu ao DDC/CI.".to_string(),
            DdcError::BadReply => "O monitor respondeu fora do padrão DDC/CI.".to_string(),
            DdcError::Unsupported => {
                "O monitor não deixa ajustar o brilho pelo DDC/CI.".to_string()
            }
            DdcError::Io(e) => format!("O monitor não respondeu ao DDC/CI: {e}."),
        }
    }

    /// Vale tentar de novo? Acesso negado e VCP não suportado, não.
    fn retryable(&self) -> bool {
        !matches!(
            self,
            DdcError::NoAccess | DdcError::NoDevice(_) | DdcError::Unsupported
        )
    }
}

/// Uma escrita do host: `[0x51, 0x80 | n, carga…, chk]`, com `chk` o XOR de
/// `0x6E` e de tudo antes dele.
fn message(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 3);
    out.push(HOST_SOURCE);
    out.push(0x80 | payload.len() as u8);
    out.extend_from_slice(payload);
    out.push(xor(WRITE_CHECKSUM_INIT, &out));
    out
}

fn xor(init: u8, bytes: &[u8]) -> u8 {
    bytes.iter().fold(init, |acc, b| acc ^ b)
}

/// O pedido de Get VCP do `code`.
pub fn get_request(code: u8) -> Vec<u8> {
    message(&[OP_GET_VCP, code])
}

/// O pedido de Set VCP do `code` com `value`.
pub fn set_request(code: u8, value: u16) -> Vec<u8> {
    let [high, low] = value.to_be_bytes();
    message(&[OP_SET_VCP, code, high, low])
}

/// Lê a resposta ao Get VCP do `code`:
/// `[0x6E, 0x88, 0x02, resultado, código, tipo, maxH, maxL, atualH, atualL, chk]`,
/// com `chk` o XOR de `0x50` e dos 10 primeiros. O comprimento vem do segundo
/// byte (`& 0x7F`) e os bytes além dele são ignorados, então a "null message"
/// (`[0x6E, 0x80, 0xBE]`, o monitor ocupado) é reconhecida no buffer de 11.
pub fn parse_reply(code: u8, bytes: &[u8]) -> Result<Vcp, DdcError> {
    // Só zeros: ninguém respondeu no barramento.
    if bytes.iter().all(|&b| b == 0) {
        return Err(DdcError::NoAnswer);
    }
    if bytes.len() < 3 || bytes[0] != MONITOR_SOURCE || bytes[1] & 0x80 == 0 {
        return Err(DdcError::BadReply);
    }
    let len = usize::from(bytes[1] & 0x7F);
    let total = len + 3;
    if bytes.len() < total {
        return Err(DdcError::BadReply);
    }
    let frame = &bytes[..total];
    if xor(REPLY_CHECKSUM_INIT, &frame[..total - 1]) != frame[total - 1] {
        return Err(DdcError::BadReply);
    }
    if len == 0 {
        // A "null message": o monitor não tem resposta agora.
        return Err(DdcError::NoAnswer);
    }
    if total != GET_REPLY_LEN || frame[2] != OP_GET_VCP_REPLY {
        return Err(DdcError::BadReply);
    }
    match frame[3] {
        0 => {}
        1 => return Err(DdcError::Unsupported),
        _ => return Err(DdcError::BadReply),
    }
    if frame[4] != code {
        return Err(DdcError::BadReply);
    }
    let max = u16::from_be_bytes([frame[6], frame[7]]);
    let current = u16::from_be_bytes([frame[8], frame[9]]);
    if max == 0 {
        return Err(DdcError::BadReply);
    }
    Ok(Vcp { current, max })
}

/// Roda `attempt` entre `lock` e `unlock`.
fn locked<T>(
    bus: &mut dyn I2cBus,
    attempt: impl FnOnce(&mut dyn I2cBus) -> Result<T, DdcError>,
) -> Result<T, DdcError> {
    bus.lock().map_err(|e| DdcError::from_transfer(&e))?;
    let result = attempt(bus);
    bus.unlock();
    result
}

/// Tenta `attempt` uma vez e mais [`RETRIES`] vezes, com `retry_delay` entre
/// elas, enquanto o erro valer nova tentativa.
fn with_retries<T>(
    t: &DdcTiming,
    mut attempt: impl FnMut() -> Result<T, DdcError>,
) -> Result<T, DdcError> {
    let mut tries = 0;
    loop {
        match attempt() {
            Err(e) if e.retryable() && tries < RETRIES => {
                tries += 1;
                thread::sleep(t.retry_delay);
            }
            result => return result,
        }
    }
}

/// Lê o brilho (VCP `0x10`) do monitor.
pub fn get_brightness(bus: &mut dyn I2cBus, t: &DdcTiming) -> Result<Vcp, DdcError> {
    with_retries(t, || {
        locked(bus, |bus| {
            bus.write(DDC_ADDR, &get_request(VCP_BRIGHTNESS))
                .map_err(|e| DdcError::from_transfer(&e))?;
            thread::sleep(t.get_delay);
            let mut reply = [0u8; GET_REPLY_LEN];
            bus.read(DDC_ADDR, &mut reply)
                .map_err(|e| DdcError::from_transfer(&e))?;
            parse_reply(VCP_BRIGHTNESS, &reply)
        })
    })
}

/// Grava o brilho (VCP `0x10`), espera `set_delay` e relê: devolve o que o
/// monitor guardou, não o que se pediu.
pub fn set_brightness(bus: &mut dyn I2cBus, value: u16, t: &DdcTiming) -> Result<Vcp, DdcError> {
    with_retries(t, || {
        locked(bus, |bus| {
            bus.write(DDC_ADDR, &set_request(VCP_BRIGHTNESS, value))
                .map_err(|e| DdcError::from_transfer(&e))
        })
    })?;
    thread::sleep(t.set_delay);
    get_brightness(bus, t)
}

/// Lê o bloco base (128 bytes) do EDID em `0x50`, a partir do offset 0.
pub fn read_edid(bus: &mut dyn I2cBus) -> Result<[u8; 128], DdcError> {
    locked(bus, |bus| {
        bus.write(EDID_ADDR, &[0x00])
            .map_err(|e| DdcError::from_transfer(&e))?;
        let mut edid = [0u8; 128];
        bus.read(EDID_ADDR, &mut edid)
            .map_err(|e| DdcError::from_transfer(&e))?;
        Ok(edid)
    })
}
