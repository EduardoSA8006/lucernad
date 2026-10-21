//! Módulo `spectrum` do `lucernad`: o espectro do som do player exibido.
//!
//! A captura fica atrás do trait [`backend::Backend`]: o de verdade é o
//! [`pipewire::PipeWire`], que liga um fluxo de entrada a cada nó de saída do
//! player sem mexer no roteamento; nos testes, o dublê é o
//! `testing::FakeBackend` (feature `testing`). O casamento do player com os
//! nós fica no [`matching`], sobre o `/proc` e o D-Bus ([`dbus::Bus`], com o
//! dublê `testing::FakeBus`).

pub mod analysis;
pub mod backend;
pub mod dbus;
pub mod matching;
pub mod mix;
pub mod pipewire;
#[cfg(feature = "testing")]
pub mod testing;
