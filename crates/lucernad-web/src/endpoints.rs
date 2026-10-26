//! As bases das fontes: as de verdade, ou todas sob o servidor local de
//! teste (`LUCERNAD_WEB_BASE`), que só pode ser um IP literal de loopback.

use std::ffi::OsStr;

/// O erro de um `LUCERNAD_WEB_BASE` fora da forma aceita.
const INVALID_BASE: &str = "LUCERNAD_WEB_BASE inválido: precisa ser http://127.0.0.1:<porta> ou http://[::1]:<porta> (só para testes).";

/// As bases das quatro fontes, sem barra no fim; quem pede soma o caminho
/// (`/api/get`, `/search.php`, `/v1/forecast`…).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoints {
    pub lrclib: String,
    pub vagalume: String,
    pub forecast: String,
    pub geocoding: String,
}

impl Endpoints {
    /// As fontes de verdade, na internet.
    pub fn real() -> Endpoints {
        Endpoints {
            lrclib: "https://lrclib.net".to_string(),
            vagalume: "https://api.vagalume.com.br".to_string(),
            forecast: "https://api.open-meteo.com".to_string(),
            geocoding: "https://geocoding-api.open-meteo.com".to_string(),
        }
    }

    /// Todas sob a base de teste: `<base>/lrclib`, `<base>/vagalume`,
    /// `<base>/forecast` e `<base>/geocoding`.
    pub fn under(base: &str) -> Endpoints {
        Endpoints {
            lrclib: format!("{base}/lrclib"),
            vagalume: format!("{base}/vagalume"),
            forecast: format!("{base}/forecast"),
            geocoding: format!("{base}/geocoding"),
        }
    }
}

/// Lê o `LUCERNAD_WEB_BASE`: ausente ou vazio → `Ok(None)`;
/// `http://127.0.0.1:<porta>` ou `http://[::1]:<porta>` (porta de 1 a 65535,
/// sem caminho nem usuário) → `Ok(Some(base))`; qualquer outra coisa → o
/// erro, nunca a rede de verdade.
pub fn base_from_env(value: Option<&OsStr>) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    let text = value.to_str().ok_or_else(|| INVALID_BASE.to_string())?;
    let port = ["http://127.0.0.1:", "http://[::1]:"]
        .iter()
        .find_map(|prefix| text.strip_prefix(prefix))
        .ok_or_else(|| INVALID_BASE.to_string())?;
    let valid = !port.is_empty()
        && port.len() <= 5
        && port.bytes().all(|b| b.is_ascii_digit())
        && port.parse::<u32>().is_ok_and(|p| (1..=65535).contains(&p));
    if valid {
        Ok(Some(text.to_string()))
    } else {
        Err(INVALID_BASE.to_string())
    }
}
