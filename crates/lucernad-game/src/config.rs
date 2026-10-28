//! A configuração do modo jogo, mandada pelo shell no `game.configure`
//! (substitui tudo; campo ausente = o padrão).

use lucerna_service::RpcError;
use serde_json::Value;

/// O máximo de classes em cada lista.
pub const CLASSES_MAX: usize = 100;
/// O maior nome de classe, em caracteres.
pub const CLASS_CHARS_MAX: usize = 128;

/// A lista "nunca" de fábrica (D24): os players e navegadores comuns, para
/// um vídeo em tela cheia não ligar o modo jogo. O usuário pode tirar itens
/// dela (o shell manda a lista inteira).
pub const DEFAULT_NEVER: [&str; 18] = [
    "mpv",
    "vlc",
    "io.github.celluloid_player.celluloid",
    "org.gnome.totem",
    "org.kde.haruna",
    "smplayer",
    "firefox",
    "firefox-esr",
    "librewolf",
    "floorp",
    "zen",
    "zen-browser",
    "chromium",
    "google-chrome",
    "brave-browser",
    "vivaldi-stable",
    "microsoft-edge",
    "opera",
];

const LISTS_ERROR: &str =
    "always e never precisam ser listas de até 100 classes com até 128 caracteres";

/// O que o modo jogo faz e quando entra.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameSettings {
    /// Entra pela tela cheia de verdade.
    pub auto_fullscreen: bool,
    /// Entra por um jogo detectado.
    pub auto_games: bool,
    /// Classes (em minúsculas) que sempre ligam o modo.
    pub always: Vec<String>,
    /// Classes (em minúsculas) que nunca contam.
    pub never: Vec<String>,
    /// Desfoque, sombras e animações desligados.
    pub effects: bool,
    /// VRR na janela em tela cheia.
    pub vrr: bool,
    /// Tearing (desligado por padrão).
    pub tearing: bool,
    /// O perfil de energia "desempenho".
    pub profile: bool,
    /// O GameMode da Feral.
    pub gamemode: bool,
    /// O `lucernad` mais leve.
    pub light: bool,
}

impl Default for GameSettings {
    fn default() -> GameSettings {
        GameSettings {
            auto_fullscreen: true,
            auto_games: true,
            always: Vec::new(),
            never: DEFAULT_NEVER.iter().map(|s| s.to_string()).collect(),
            effects: true,
            vrr: true,
            tearing: false,
            profile: true,
            gamemode: true,
            light: true,
        }
    }
}

/// O campo `key`; `null` conta como ausente.
fn field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.get(key).filter(|v| !v.is_null())
}

/// A seção `name` (um objeto, ou `None` se ausente).
fn section<'a>(p: &'a Value, name: &str) -> Result<Option<&'a Value>, RpcError> {
    match field(p, name) {
        None => Ok(None),
        Some(v) if v.is_object() => Ok(Some(v)),
        Some(_) => Err(RpcError::invalid_params(&format!(
            "{name} precisa ser um objeto"
        ))),
    }
}

/// Lê o booleano `sec.key` para `out` (fica o padrão se ausente).
fn flag(sec: Option<&Value>, name: &str, key: &str, out: &mut bool) -> Result<(), RpcError> {
    let Some(v) = sec.and_then(|s| field(s, key)) else {
        return Ok(());
    };
    *out = v.as_bool().ok_or_else(|| {
        RpcError::invalid_params(&format!("{name}.{key} precisa ser true ou false"))
    })?;
    Ok(())
}

fn classes(v: Option<&Value>, default: &[String]) -> Result<Vec<String>, RpcError> {
    let bad = || RpcError::invalid_params(LISTS_ERROR);
    let Some(v) = v else {
        return Ok(default.to_vec());
    };
    let list = v.as_array().ok_or_else(bad)?;
    if list.len() > CLASSES_MAX {
        return Err(bad());
    }
    list.iter()
        .map(|c| {
            c.as_str()
                .filter(|c| c.chars().count() <= CLASS_CHARS_MAX)
                .map(str::to_lowercase)
                .ok_or_else(bad)
        })
        .collect()
}

/// Os parâmetros do `game.configure` (`null` = tudo padrão).
pub fn parse(p: &Value) -> Result<GameSettings, RpcError> {
    let mut s = GameSettings::default();
    if p.is_null() {
        return Ok(s);
    }
    if !p.is_object() {
        return Err(RpcError::invalid_params(
            "os parâmetros precisam ser um objeto",
        ));
    }
    let auto = section(p, "auto")?;
    flag(auto, "auto", "fullscreen", &mut s.auto_fullscreen)?;
    flag(auto, "auto", "games", &mut s.auto_games)?;
    let comp = section(p, "compositor")?;
    flag(comp, "compositor", "effects", &mut s.effects)?;
    flag(comp, "compositor", "vrr", &mut s.vrr)?;
    flag(comp, "compositor", "tearing", &mut s.tearing)?;
    let sys = section(p, "system")?;
    flag(sys, "system", "profile", &mut s.profile)?;
    flag(sys, "system", "gamemode", &mut s.gamemode)?;
    flag(sys, "system", "light", &mut s.light)?;
    s.always = classes(field(p, "always"), &s.always)?;
    s.never = classes(field(p, "never"), &s.never)?;
    Ok(s)
}
