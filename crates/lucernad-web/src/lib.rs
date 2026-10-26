//! Módulo `web` do `lucernad`: a letra das músicas e a previsão do tempo, com
//! cache em disco.
//!
//! Por enquanto, só as partes puras: o [`lrc`] lê a letra (sincronizada ou de
//! texto) e o [`matching`] decide se a faixa que uma fonte devolveu é a que
//! foi pedida.

pub mod lrc;
pub mod matching;
