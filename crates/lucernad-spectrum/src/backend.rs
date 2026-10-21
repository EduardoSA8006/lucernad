//! O contrato da captura: o que o laço do módulo pede e o que recebe.

use tokio::sync::mpsc::UnboundedReceiver;

/// Um nó de saída de áudio de um app (`media.class` `Stream/Output/Audio`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputNode {
    /// O id do nó no registro.
    pub id: u32,
    /// O `object.serial` do nó, que não se repete na vida do servidor.
    pub serial: u64,
    /// O `application.process.id` do info do client dono do nó (o
    /// `pipewire.sec.pid` das propriedades globais é o do `pipewire-pulse`
    /// para os clientes pulse).
    pub client_pid: Option<u32>,
    /// O `application.process.binary` do info do client dono do nó.
    pub binary: Option<String>,
}

/// O que a sessão manda ao laço.
#[derive(Debug, PartialEq)]
pub enum Event {
    /// A lista inteira de nós de saída, em ordem de `id`: a cada nó que entra
    /// ou sai e a cada info de client que chega.
    Graph(Vec<OutputNode>),
    /// Amostras de um nó capturado, em mono (a média dos canais).
    Samples {
        serial: u64,
        rate: u32,
        mono: Vec<f32>,
    },
    /// A conexão caiu; o canal fecha logo depois.
    Disconnected(String),
}

/// O comando da captura de uma sessão.
pub trait Control: Send {
    /// O conjunto inteiro de nós desejado, pelo `object.serial`: abre os
    /// fluxos que faltam e fecha os que saíram.
    fn capture(&self, serials: &[u64]);
}

/// Uma conexão aberta. Largá-la fecha os fluxos e a conexão.
pub struct Session {
    pub events: UnboundedReceiver<Event>,
    pub control: Box<dyn Control>,
}

/// A fonte de áudio. Os dois métodos bloqueiam: quem chama do runtime usa o
/// `detached`.
pub trait Backend: Send + Sync + 'static {
    /// Conecta e desconecta: diz se o servidor está alcançável.
    fn probe(&self) -> Result<(), String>;
    /// Abre uma sessão: conexão, registro e, a pedido, os fluxos.
    fn open(&self) -> Result<Session, String>;
}
