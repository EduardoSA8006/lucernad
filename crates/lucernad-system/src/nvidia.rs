//! A leitura da NVIDIA pela NVML, atrás do trait [`Nvidia`].
//!
//! Substitui o `nvidia-smi` do `gpu-status.sh`. Quem decide se a NVML pode
//! ser chamada é o [`read_gpus`](crate::gpu::read_gpus): só com a placa ativa
//! e com outro processo usando ela, para a NVML nunca segurar acordada uma
//! placa ociosa. A chamada roda fora do runtime, numa thread solta com prazo
//! (o `detached`), porque o driver pode demorar.

use nvml_wrapper::Nvml as NvmlLib;
use nvml_wrapper::enum_wrappers::device::{Clock, TemperatureSensor};

/// O que a NVML diz de uma placa; campo que falha fica `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NvidiaReading {
    /// Uso de 0 a 1.
    pub usage: Option<f64>,
    /// Temperatura em °C (inteira, como a NVML dá).
    pub temp: Option<f64>,
    /// Frequência gráfica atual, em MHz.
    pub freq: Option<u32>,
    /// Frequência gráfica máxima, em MHz.
    pub max_freq: Option<u32>,
}

/// Uma fonte de leituras da NVIDIA, pelo endereço PCI (`0000:01:00.0`).
pub trait Nvidia: Send {
    /// A leitura da placa; `None` se a biblioteca ou a placa não responderem.
    fn read(&mut self, pci: &str) -> Option<NvidiaReading>;
}

/// A NVML de verdade (`libnvidia-ml.so`, carregada em tempo de execução).
///
/// Não guarda nada entre leituras: cada [`Nvidia::read`] faz o `init`, lê e
/// larga a biblioteca, para não manter um handle aberto no driver.
#[derive(Clone, Copy, Debug, Default)]
pub struct Nvml;

impl Nvidia for Nvml {
    fn read(&mut self, pci: &str) -> Option<NvidiaReading> {
        let nvml = NvmlLib::init().ok()?;
        let device = nvml.device_by_pci_bus_id(pci).ok()?;
        Some(NvidiaReading {
            usage: device
                .utilization_rates()
                .ok()
                .map(|u| f64::from(u.gpu) / 100.0),
            temp: device
                .temperature(TemperatureSensor::Gpu)
                .ok()
                .map(f64::from),
            freq: device.clock_info(Clock::Graphics).ok(),
            max_freq: device.max_clock_info(Clock::Graphics).ok(),
        })
    }
}

/// Sem NVIDIA (ou sem NVML): sempre `None`.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoNvidia;

impl Nvidia for NoNvidia {
    fn read(&mut self, _pci: &str) -> Option<NvidiaReading> {
        None
    }
}
