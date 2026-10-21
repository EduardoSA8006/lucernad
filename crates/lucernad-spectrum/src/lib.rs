//! Módulo `spectrum` do `lucernad`: o espectro do som do player exibido.
//!
//! A captura fica atrás do trait [`backend::Backend`]: o de verdade é o
//! [`pipewire::PipeWire`], que liga um fluxo de entrada a cada nó de saída do
//! player sem mexer no roteamento; nos testes, o dublê é o
//! `testing::FakeBackend` (feature `testing`).

pub mod backend;
pub mod pipewire;
#[cfg(feature = "testing")]
pub mod testing;
