//! Módulo `clipboard` do `lucernad`: o histórico da área de transferência do
//! Wayland.
//!
//! A escolha do tipo de cada cópia (e o filtro do conteúdo sensível) fica no
//! [`mime`]; a leitura do pipe, com prazo e limite, no [`pipe`].

pub mod mime;
pub mod pipe;
pub mod search;
pub mod store;
#[cfg(feature = "testing")]
pub mod testing;
pub mod thumb;
