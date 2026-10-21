//! A escolha do tipo de cada cópia, o filtro do sensível e os tipos que o
//! `lucernad` oferece quando vira dono da seleção.

use std::sync::Arc;
use std::time::Duration;

/// O maior texto (ou lista de arquivos) lido de uma cópia: 1 MiB.
pub const TEXT_MAX: usize = 1 << 20;
/// A maior imagem lida de uma cópia: 32 MiB.
pub const IMAGE_MAX: usize = 32 << 20;
/// O prazo para quem copiou mandar o conteúdo inteiro pelo pipe.
pub const READ_TIMEOUT: Duration = Duration::from_secs(3);
/// O tipo que marca uma seleção do próprio `lucernad`; o valor é o id do item.
pub const OWN_MIME: &str = "application/x-lucernad-item";
/// Os tipos com que o gerenciador de senhas marca o conteúdo sensível. Basta
/// um deles na lista para a cópia ser ignorada inteira (o valor nem é lido).
pub const SECRET_MIMES: [&str; 2] = [
    "x-kde-passwordManagerHint",
    "application/x-kde-passwordManagerHint",
];
/// Os tipos de texto, na ordem de preferência.
pub const TEXT_MIMES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "TEXT",
    "STRING",
];
/// A lista de arquivos copiados no gerenciador.
pub const URI_LIST: &str = "text/uri-list";
/// A lista de arquivos no formato do Nautilus e afins.
pub const GNOME_FILES: &str = "x-special/gnome-copied-files";

const PNG: &str = "image/png";

/// O tipo de um item do histórico.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Image,
    Files,
}

impl Kind {
    /// O nome do tipo no protocolo e no banco.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Image => "image",
            Kind::Files => "files",
        }
    }

    /// O inverso de [`Kind::as_str`].
    pub fn parse(name: &str) -> Option<Kind> {
        match name {
            "text" => Some(Kind::Text),
            "image" => Some(Kind::Image),
            "files" => Some(Kind::Files),
            _ => None,
        }
    }

    /// O maior conteúdo aceito numa cópia deste tipo.
    pub fn max_size(self) -> usize {
        match self {
            Kind::Image => IMAGE_MAX,
            Kind::Text | Kind::Files => TEXT_MAX,
        }
    }
}

/// O que fazer com uma cópia, pelos tipos que ela oferece.
#[derive(Debug, PartialEq, Eq)]
pub enum Choice {
    /// Marcada como sensível: ignora sem ler.
    Secret,
    /// Uma seleção do próprio `lucernad`: só sobe o item ao topo.
    Own,
    /// Lê o conteúdo neste tipo.
    Read { kind: Kind, mime: String },
    /// Nenhum tipo que o histórico guarde (HTML sozinho, por exemplo).
    Nothing,
}

/// Escolhe o que ler de uma cópia: o sensível primeiro, depois a cópia nossa,
/// `image/png`, outra `image/*` na ordem oferecida, `text/uri-list` e os
/// [`TEXT_MIMES`] na ordem deles.
pub fn choose(mimes: &[String]) -> Choice {
    let has = |wanted: &str| mimes.iter().any(|mime| mime == wanted);
    if SECRET_MIMES.iter().any(|secret| has(secret)) {
        return Choice::Secret;
    }
    if has(OWN_MIME) {
        return Choice::Own;
    }
    let image = if has(PNG) {
        Some(PNG)
    } else {
        mimes
            .iter()
            .map(String::as_str)
            .find(|mime| mime.starts_with("image/"))
    };
    let read = |kind, mime: &str| Choice::Read {
        kind,
        mime: mime.to_string(),
    };
    if let Some(mime) = image {
        return read(Kind::Image, mime);
    }
    if has(URI_LIST) {
        return read(Kind::Files, URI_LIST);
    }
    match TEXT_MIMES.iter().find(|mime| has(mime)) {
        Some(mime) => read(Kind::Text, mime),
        None => Choice::Nothing,
    }
}

/// Os tipos (e os bytes de cada um) que a fonte do `lucernad` oferece para
/// um item: o texto em todos os [`TEXT_MIMES`]; a imagem no `mime` dela; os
/// arquivos em [`URI_LIST`] e em [`GNOME_FILES`]; e sempre, por último,
/// [`OWN_MIME`] com o `id`.
pub fn offers_for(
    kind: Kind,
    mime: Option<&str>,
    data: &[u8],
    id: i64,
) -> Vec<(String, Arc<[u8]>)> {
    let mut offers = Vec::new();
    match kind {
        Kind::Text => {
            let data: Arc<[u8]> = data.into();
            offers.extend(
                TEXT_MIMES
                    .iter()
                    .map(|mime| (mime.to_string(), data.clone())),
            );
        }
        Kind::Image => {
            if let Some(mime) = mime {
                offers.push((mime.to_string(), data.into()));
            }
        }
        Kind::Files => {
            let text = String::from_utf8_lossy(data);
            let uris: Vec<&str> = text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .collect();
            let gnome = format!("copy\n{}", uris.join("\n"));
            offers.push((URI_LIST.to_string(), data.into()));
            offers.push((GNOME_FILES.to_string(), gnome.into_bytes().into()));
        }
    }
    offers.push((OWN_MIME.to_string(), id.to_string().into_bytes().into()));
    offers
}
