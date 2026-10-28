//! Os eventos do `.socket2.sock`: uma linha `nome>>dados` cada.
//!
//! O leitor do [`crate::SocketHypr`] também manda dois eventos próprios,
//! [`CONNECTED`] (com a assinatura da instância nos dados) e
//! [`DISCONNECTED`], quando a conexão de eventos abre e cai: quem assina
//! sabe quando reler o estado inteiro.

/// A conexão de eventos abriu (dados: a assinatura da instância).
pub const CONNECTED: &str = "lucernad-connected";
/// A conexão de eventos caiu.
pub const DISCONNECTED: &str = "lucernad-disconnected";

/// Um evento do Hyprland.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// O nome (`openwindow`, `fullscreen`, `configreloaded`…).
    pub name: String,
    /// O resto da linha depois do primeiro `>>` (pode ter vírgulas e `>>`).
    pub data: String,
}

/// Uma linha do socket de eventos (sem o `\n`); `None` sem `>>` ou sem nome.
pub fn parse_line(line: &str) -> Option<Event> {
    let (name, data) = line.split_once(">>")?;
    if name.is_empty() {
        return None;
    }
    Some(Event {
        name: name.to_string(),
        data: data.to_string(),
    })
}
