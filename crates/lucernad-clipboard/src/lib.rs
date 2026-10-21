//! Módulo `clipboard` do `lucernad`: o histórico da área de transferência do
//! Wayland.
//!
//! A escolha do tipo de cada cópia (e o filtro do conteúdo sensível) fica no
//! [`mime`]; a leitura do pipe, com prazo e limite, no [`pipe`]. O
//! compositor fica atrás do [`backend::Backend`]; o de verdade é o
//! [`wayland::Wayland`].

pub mod backend;
pub mod mime;
pub mod pipe;
pub mod search;
pub mod store;
#[cfg(feature = "testing")]
pub mod testing;
pub mod thumb;
pub mod wayland;
