//! Módulo `web` do `lucernad`: a letra das músicas e a previsão do tempo, com
//! cache em disco.
//!
//! Por enquanto: o [`lrc`] lê a letra (sincronizada ou de texto), o
//! [`matching`] decide se a faixa que uma fonte devolveu é a que foi pedida,
//! o [`http`] faz os pedidos (com prazo, nova tentativa e `Retry-After`) e o
//! [`endpoints`] diz para onde eles vão.

pub mod endpoints;
pub mod http;
pub mod lrc;
pub mod matching;

#[cfg(feature = "testing")]
pub mod testing;
