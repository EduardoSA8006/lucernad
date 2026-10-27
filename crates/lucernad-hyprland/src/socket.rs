//! O pedido ao `.socket.sock`: uma conexão por pedido, o comando escrito, a
//! escrita fechada e a resposta lida até o EOF, com prazo e limite de
//! tamanho. Nada aqui segura o laço de quem chama além do prazo.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// O prazo de um pedido (conectar, escrever e ler a resposta inteira).
pub const HYPR_TIMEOUT: Duration = Duration::from_secs(2);
/// A maior resposta aceita, em bytes.
pub const HYPR_REPLY_MAX: usize = 4 * 1024 * 1024;

/// O que deu errado num pedido ao Hyprland.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HyprError {
    /// Sem socket, ou a conexão caiu no meio (o Hyprland não está rodando).
    Unreachable(String),
    /// O socket aceitou e não respondeu no prazo.
    TimedOut,
    /// A resposta passou de [`HYPR_REPLY_MAX`].
    TooLarge,
    /// O Hyprland respondeu com um erro (o texto dele).
    Refused(String),
}

impl fmt::Display for HyprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HyprError::Unreachable(e) => write!(f, "sem conexão com o Hyprland: {e}"),
            HyprError::TimedOut => write!(
                f,
                "o Hyprland não respondeu em {} s",
                HYPR_TIMEOUT.as_secs()
            ),
            HyprError::TooLarge => write!(
                f,
                "a resposta do Hyprland passou de {} MiB",
                HYPR_REPLY_MAX / (1024 * 1024)
            ),
            HyprError::Refused(e) => write!(f, "o Hyprland recusou o pedido: {e}"),
        }
    }
}

impl std::error::Error for HyprError {}

/// Manda `command` ao socket `path` e devolve a resposta inteira (texto,
/// com bytes inválidos trocados por U+FFFD).
pub async fn request(path: &Path, command: &str, timeout: Duration) -> Result<String, HyprError> {
    let exchange = async {
        let lost = |e: std::io::Error| HyprError::Unreachable(e.to_string());
        let mut stream = UnixStream::connect(path).await.map_err(lost)?;
        stream.write_all(command.as_bytes()).await.map_err(lost)?;
        stream.shutdown().await.map_err(lost)?;
        let mut body = Vec::new();
        (&mut stream)
            .take(HYPR_REPLY_MAX as u64 + 1)
            .read_to_end(&mut body)
            .await
            .map_err(lost)?;
        if body.len() > HYPR_REPLY_MAX {
            return Err(HyprError::TooLarge);
        }
        Ok(String::from_utf8_lossy(&body).into_owned())
    };
    match tokio::time::timeout(timeout, exchange).await {
        Ok(result) => result,
        Err(_) => Err(HyprError::TimedOut),
    }
}
