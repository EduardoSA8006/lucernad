//! As janelas (`j/clients`) e os workspaces à mostra (`j/monitors`).

use serde_json::Value;

/// Uma janela do Hyprland, só com o que o modo jogo usa.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    /// O endereço, com o `0x` (`0x55c473515b60`).
    pub address: String,
    pub pid: i32,
    pub class: String,
    pub title: String,
    /// O modo de tela cheia do Hyprland: 0 nenhum, 1 maximizada, 2 tela
    /// cheia, 3 as duas. Tela cheia de verdade = o bit 2.
    pub fullscreen: u8,
    /// O id do workspace.
    pub workspace: i64,
    pub mapped: bool,
}

fn array(body: &str, what: &str) -> Result<Vec<Value>, String> {
    match serde_json::from_str::<Value>(body) {
        Ok(Value::Array(a)) => Ok(a),
        _ => Err(format!(
            "a resposta do Hyprland para {what} não é uma lista: {}",
            body.chars().take(80).collect::<String>().trim()
        )),
    }
}

/// As janelas do `j/clients` (as sem endereço ou sem pid ficam de fora).
pub fn parse_clients(body: &str) -> Result<Vec<Window>, String> {
    Ok(array(body, "as janelas")?
        .iter()
        .filter_map(|c| {
            let text = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            Some(Window {
                address: c.get("address")?.as_str()?.to_string(),
                pid: i32::try_from(c.get("pid")?.as_i64()?).ok()?,
                class: text("class"),
                title: text("title"),
                fullscreen: c
                    .get("fullscreen")
                    .and_then(Value::as_u64)
                    .and_then(|f| u8::try_from(f).ok())
                    .unwrap_or(0),
                workspace: c
                    .pointer("/workspace/id")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                mapped: c.get("mapped").and_then(Value::as_bool).unwrap_or(true),
            })
        })
        .collect())
}

/// Os workspaces à mostra do `j/monitors`: o `activeWorkspace` de cada
/// monitor e o `specialWorkspace` aberto nele (id diferente de 0).
pub fn parse_active_workspaces(body: &str) -> Result<Vec<i64>, String> {
    let mut ids = Vec::new();
    for m in array(body, "os monitores")? {
        if let Some(id) = m.pointer("/activeWorkspace/id").and_then(Value::as_i64) {
            ids.push(id);
        }
        if let Some(id) = m
            .pointer("/specialWorkspace/id")
            .and_then(Value::as_i64)
            .filter(|id| *id != 0)
        {
            ids.push(id);
        }
    }
    Ok(ids)
}
