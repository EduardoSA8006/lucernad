//! O contrato da área de transferência: o que o laço do módulo pede ao
//! compositor e o que recebe.

use std::io::PipeReader;
use std::path::Path;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedReceiver;

/// O que a sessão manda ao laço.
#[derive(Debug, PartialEq)]
pub enum Event {
    /// Uma seleção nova, com os tipos oferecidos. O `offer` cresce a cada
    /// seleção da sessão; só o da última vale no [`Control::receive`].
    Selection { offer: u64, mimes: Vec<String> },
    /// A seleção ficou vazia.
    Cleared,
    /// A nossa fonte foi substituída (outro app copiou).
    Cancelled,
    /// A conexão caiu; o canal fecha logo depois.
    Disconnected(String),
}

/// Uma fonte da seleção: os tipos oferecidos e os bytes de cada um.
#[derive(Clone)]
pub struct Source {
    pub offers: Vec<(String, Arc<[u8]>)>,
}

/// O comando de uma sessão.
pub trait Control: Send {
    /// Pede o conteúdo da seleção `offer` no tipo `mime`: devolve a ponta de
    /// leitura do pipe (leia com o [`crate::pipe::read_pipe`]). Uma oferta
    /// que já não é a atual dá `Err("a seleção mudou")`.
    fn receive(&self, offer: u64, mime: &str) -> Result<PipeReader, String>;
    /// Vira dono da seleção com a `source`, no lugar da fonte anterior.
    fn offer(&self, source: Source);
}

/// Uma conexão aberta. Largá-la fecha a conexão (e a fonte nossa, se houver).
pub struct Session {
    pub events: UnboundedReceiver<Event>,
    pub control: Box<dyn Control>,
}

/// O compositor. O `open` bloqueia: quem chama do runtime usa o `detached`.
pub trait Backend: Send + Sync + 'static {
    /// Conecta ao socket do Wayland e abre a observação da seleção.
    fn open(&self, socket: &Path) -> Result<Session, String>;
}
