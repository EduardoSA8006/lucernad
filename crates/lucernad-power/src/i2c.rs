//! O barramento i2c por trás do DDC/CI: o trait [`I2cBus`] (escrever e ler
//! num endereço, travar e destravar o barramento), quem o abre
//! ([`I2cOpener`]) e o de verdade, sobre o `/dev/i2c-N` ([`LinuxI2c`]).
//!
//! Os testes usam o `testing::FakeI2c`; o [`LinuxI2c`] só é usado pelo módulo
//! em produção e pela sonda manual.

use std::fs::File;
use std::io;
use std::path::PathBuf;

use i2cdev::core::I2CDevice;
use i2cdev::linux::LinuxI2CDevice;
use rustix::fs::{FlockOperation, flock};

use crate::ddc::DDC_ADDR;

/// Um barramento i2c aberto.
pub trait I2cBus: Send {
    /// Escreve `data` no dispositivo de endereço `addr`.
    fn write(&mut self, addr: u16, data: &[u8]) -> io::Result<()>;
    /// Lê `buf.len()` bytes do dispositivo de endereço `addr`.
    fn read(&mut self, addr: u16, buf: &mut [u8]) -> io::Result<()>;
    /// Trava o barramento para uma transação (o `flock(LOCK_EX)` que o
    /// libddcutil também pega).
    fn lock(&mut self) -> io::Result<()>;
    /// Destrava o barramento.
    fn unlock(&mut self);
}

/// Abre o barramento `i2c-<bus>`.
pub trait I2cOpener: Send + Sync + 'static {
    fn open(&self, bus: u32) -> io::Result<Box<dyn I2cBus>>;
}

/// Os barramentos de verdade, em `<dev>/i2c-<n>`.
pub struct LinuxI2c {
    pub dev: PathBuf,
}

impl I2cOpener for LinuxI2c {
    fn open(&self, bus: u32) -> io::Result<Box<dyn I2cBus>> {
        let path = self.dev.join(format!("i2c-{bus}"));
        // O `flock` vai num descritor próprio do mesmo arquivo: o do
        // `LinuxI2CDevice` não sai dele sem `unsafe`. O `flock` vale para o
        // arquivo, então o libddcutil e o PowerDevil, que travam o mesmo
        // `/dev/i2c-N`, esperam por este descritor do mesmo jeito.
        let lock = File::open(&path)?;
        let device = LinuxI2CDevice::new(&path, DDC_ADDR).map_err(io::Error::from)?;
        Ok(Box::new(LinuxBus {
            device,
            lock,
            addr: DDC_ADDR,
        }))
    }
}

/// Um `/dev/i2c-N` aberto, com o endereço do escravo atual.
struct LinuxBus {
    device: LinuxI2CDevice,
    lock: File,
    addr: u16,
}

impl LinuxBus {
    fn address(&mut self, addr: u16) -> io::Result<()> {
        if self.addr != addr {
            self.device
                .set_slave_address(addr)
                .map_err(io::Error::from)?;
            self.addr = addr;
        }
        Ok(())
    }
}

impl I2cBus for LinuxBus {
    fn write(&mut self, addr: u16, data: &[u8]) -> io::Result<()> {
        self.address(addr)?;
        self.device.write(data).map_err(io::Error::from)
    }

    fn read(&mut self, addr: u16, buf: &mut [u8]) -> io::Result<()> {
        self.address(addr)?;
        self.device.read(buf).map_err(io::Error::from)
    }

    fn lock(&mut self) -> io::Result<()> {
        flock(&self.lock, FlockOperation::LockExclusive).map_err(io::Error::from)
    }

    fn unlock(&mut self) {
        // Destravar não falha num descritor válido; e fechar o arquivo
        // destrava de qualquer jeito.
        let _ = flock(&self.lock, FlockOperation::Unlock);
    }
}
