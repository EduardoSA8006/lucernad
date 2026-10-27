//! Módulo `game` do `lucernad`: o modo jogo.
//!
//! A [`detect`]ção lê o `/proc` (raiz trocável) para saber se a janela é de
//! um jogo; o [`state`] decide, sem E/S, quando entrar e sair (gatilhos,
//! trava manual, esperas); o [`config`] é o `game.configure`; o
//! [`compositor`] desliga os efeitos do Hyprland com o [`journal`] gravado
//! antes e restaura exatamente; o [`bus`] segura o perfil de desempenho e
//! registra o jogo no GameMode. O laço do [`register`]/[`Game::spawn`] junta
//! tudo e publica o `game.state`; o [`GameHandle::shutdown`] desfaz tudo
//! antes de o processo sair, e o [`compositor::restore_from_journal_cli`] é o
//! `lucernad restaurar-jogo` do `ExecStopPost`.

pub mod bus;
pub mod compositor;
pub mod config;
pub mod detect;
pub mod journal;
mod module;
pub mod state;
#[cfg(feature = "testing")]
pub mod testing;

pub use module::{
    Game, GameConfig, GameHandle, GameTopics, RESTORE_RETRY, SETTLE, SHUTDOWN_WAIT, Timing,
    register,
};
