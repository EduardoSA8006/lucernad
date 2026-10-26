//! Módulo `web` do `lucernad`: a letra das músicas e a previsão do tempo, com
//! cache em disco.
//!
//! Por enquanto: o [`lrc`] lê a letra (sincronizada ou de texto), o
//! [`matching`] decide se a faixa que uma fonte devolveu é a que foi pedida,
//! o [`http`] faz os pedidos (com prazo, nova tentativa e `Retry-After`) e o
//! [`endpoints`] diz para onde eles vão; o [`cache`] guarda as respostas em
//! disco. As fontes: o [`lyrics`] (a LRCLIB e o Vagalume) e o [`weather`]
//! (o Open-Meteo). O [`net`] diz se há internet (pelo NetworkManager).

pub mod cache;
pub mod endpoints;
pub mod http;
pub mod lrc;
pub mod lyrics;
pub mod matching;
pub mod net;
pub mod weather;

#[cfg(feature = "testing")]
pub mod testing;
