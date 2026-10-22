//! O `lucernad`: o daemon do Lucerna, um binário com um módulo por área.
//!
//! Cada módulo é um crate ligado por uma feature (`system`, `spectrum`,
//! `clipboard` e `power`, as quatro padrão) e se registra no mesmo serviço. [`lucernad`] monta o serviço
//! com os módulos ligados; [`cli`] é a linha de comando: os subcomandos comuns
//! do SDK (`status` e `watch`) e, com o `spectrum`, o `player`; com o
//! `clipboard`, o `historico` e o `copiar`; com o `power`, o `brilho`.

use lucerna_cli::CliApp;
use lucerna_service::ServiceBuilder;

/// Id do app.
pub const APP: &str = "lucernad";

/// A configuração de cada módulo ligado.
pub struct Modules {
    /// O módulo `system`.
    #[cfg(feature = "system")]
    pub system: lucernad_system::SystemConfig,
    /// O módulo `spectrum`.
    #[cfg(feature = "spectrum")]
    pub spectrum: lucernad_spectrum::SpectrumConfig,
    /// O módulo `clipboard`.
    #[cfg(feature = "clipboard")]
    pub clipboard: lucernad_clipboard::ClipboardConfig,
    /// O módulo `power`.
    #[cfg(feature = "power")]
    pub power: lucernad_power::PowerConfig,
}

impl Modules {
    /// Os módulos sobre a máquina de verdade.
    pub fn real() -> Modules {
        Modules {
            #[cfg(feature = "system")]
            system: lucernad_system::SystemConfig::system(),
            #[cfg(feature = "spectrum")]
            spectrum: lucernad_spectrum::SpectrumConfig::system(),
            #[cfg(feature = "clipboard")]
            clipboard: lucernad_clipboard::ClipboardConfig::system(),
            #[cfg(feature = "power")]
            power: lucernad_power::PowerConfig::system(),
        }
    }
}

/// O serviço montado, os tópicos e as tarefas dos módulos.
///
/// As tarefas dos módulos vivem enquanto os campos `*_task` viverem: ao usar
/// só o `service`, mantenha-os no escopo até o serviço parar.
pub struct Lucernad {
    /// O serviço, pronto para `run` ou `run_with`.
    pub service: ServiceBuilder,
    /// Os tópicos do módulo `system`.
    #[cfg(feature = "system")]
    pub system: lucernad_system::SystemTopics,
    /// O laço do módulo `system`; soltá-lo encerra a amostragem.
    #[cfg(feature = "system")]
    pub system_task: lucernad_core::ModuleTask,
    /// Os tópicos do módulo `spectrum`.
    #[cfg(feature = "spectrum")]
    pub spectrum: lucernad_spectrum::SpectrumTopics,
    /// O laço do módulo `spectrum`; soltá-lo encerra a captura.
    #[cfg(feature = "spectrum")]
    pub spectrum_task: lucernad_core::ModuleTask,
    /// Os tópicos do módulo `clipboard`.
    #[cfg(feature = "clipboard")]
    pub clipboard: lucernad_clipboard::ClipboardTopics,
    /// O laço do módulo `clipboard`; soltá-lo encerra a observação e solta a
    /// seleção segurada.
    #[cfg(feature = "clipboard")]
    pub clipboard_task: lucernad_core::ModuleTask,
    /// Os tópicos do módulo `power`.
    #[cfg(feature = "power")]
    pub power: lucernad_power::PowerTopics,
    /// O laço do módulo `power`; soltá-lo encerra a observação das telas.
    #[cfg(feature = "power")]
    pub power_task: lucernad_core::ModuleTask,
}

/// Monta o serviço com os módulos ligados. Chame dentro do runtime: começa as
/// tarefas dos módulos.
pub fn lucernad(modules: Modules) -> Lucernad {
    // Sem nenhum módulo, ninguém registra nada no servidor.
    #[cfg_attr(
        not(any(
            feature = "system",
            feature = "spectrum",
            feature = "clipboard",
            feature = "power"
        )),
        allow(unused_mut)
    )]
    let mut service = ServiceBuilder::new(APP, env!("CARGO_PKG_VERSION"));
    #[cfg(feature = "system")]
    let system = lucernad_system::register(service.server(), modules.system);
    #[cfg(feature = "spectrum")]
    let spectrum = lucernad_spectrum::register(service.server(), modules.spectrum);
    #[cfg(any(feature = "clipboard", feature = "power"))]
    let work = service.work();
    #[cfg(feature = "clipboard")]
    let clipboard = lucernad_clipboard::register(service.server(), modules.clipboard, work.clone());
    #[cfg(feature = "power")]
    let power = lucernad_power::register(service.server(), modules.power, work);
    #[cfg(not(any(
        feature = "system",
        feature = "spectrum",
        feature = "clipboard",
        feature = "power"
    )))]
    let Modules {} = modules;
    Lucernad {
        service,
        #[cfg(feature = "system")]
        system: system.topics.clone(),
        #[cfg(feature = "system")]
        system_task: system.spawn(),
        #[cfg(feature = "spectrum")]
        spectrum: spectrum.topics.clone(),
        #[cfg(feature = "spectrum")]
        spectrum_task: spectrum.spawn(),
        #[cfg(feature = "clipboard")]
        clipboard: clipboard.topics.clone(),
        #[cfg(feature = "clipboard")]
        clipboard_task: clipboard.spawn(),
        #[cfg(feature = "power")]
        power: power.topics.clone(),
        #[cfg(feature = "power")]
        power_task: power.spawn(),
    }
}

/// A linha de comando: os subcomandos comuns do SDK e os dos módulos.
pub fn cli() -> CliApp {
    let app = CliApp::new(
        APP,
        "lucernad",
        env!("CARGO_PKG_VERSION"),
        "O daemon do Lucerna: os dados do sistema, o espectro do som, o histórico da área de transferência e o brilho das telas para o shell.",
    );
    #[cfg(feature = "spectrum")]
    let app = app.command(player_command());
    #[cfg(feature = "clipboard")]
    let app = app.command(history_command()).command(copy_command());
    #[cfg(feature = "power")]
    let app = app.command(brightness_command());
    app
}

/// `lucernad player [NOME]`: o `spectrum.configure` (sem nome, `null`).
#[cfg(feature = "spectrum")]
fn player_command() -> lucerna_cli::CliCommand {
    use lucerna_cli::clap::{Arg, Command};
    use lucerna_cli::{Call, CliCommand};

    CliCommand {
        command: Command::new("player")
            .about("Escolhe o player cujo som o espectro mostra (sem nome, nenhum).")
            .arg(
                Arg::new("nome")
                    .value_name("NOME")
                    .help("O nome D-Bus do player, como org.mpris.MediaPlayer2.spotify."),
            ),
        to_call: |m| {
            let player = m.get_one::<String>("nome").cloned();
            Ok(Call {
                method: "spectrum.configure".into(),
                params: Some(serde_json::json!({ "player": player })),
            })
        },
        render: None,
    }
}

/// `lucernad historico [BUSCA]`: o `clipboard.list` com os 20 mais recentes.
#[cfg(feature = "clipboard")]
fn history_command() -> lucerna_cli::CliCommand {
    use lucerna_cli::clap::{Arg, Command};
    use lucerna_cli::{Call, CliCommand};

    CliCommand {
        command: Command::new("historico")
            .about("Mostra os últimos itens da área de transferência (com BUSCA, só os que casam).")
            .arg(
                Arg::new("busca")
                    .value_name("BUSCA")
                    .help("O texto a procurar, sem distinguir maiúsculas nem acentos."),
            ),
        to_call: |m| {
            let mut params = serde_json::json!({ "offset": 0, "limit": 20 });
            if let Some(q) = m.get_one::<String>("busca") {
                params["query"] = serde_json::Value::String(q.clone());
            }
            Ok(Call {
                method: "clipboard.list".into(),
                params: Some(params),
            })
        },
        render: Some(render_history),
    }
}

/// Troca os caracteres de controle (C0, DEL e C1) e as marcas de direção
/// (U+200E/F, U+202A–202E, U+2066–2069) por U+FFFD: um texto copiado com
/// escapes ANSI ou com a direção invertida não mexe no terminal de quem lê o
/// histórico.
#[cfg(any(feature = "clipboard", feature = "power"))]
fn plain(s: &str) -> String {
    let direction = |c: char| matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
    s.chars()
        .map(|c| {
            if c.is_control() || direction(c) {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect()
}

/// Uma linha `id  tipo  prévia` por item; imagem mostra `LxA mime`.
#[cfg(feature = "clipboard")]
fn render_history(v: &serde_json::Value) -> String {
    let items = v["items"].as_array().map(Vec::as_slice).unwrap_or_default();
    if items.is_empty() {
        return "Nada no histórico.".to_string();
    }
    items
        .iter()
        .map(|i| {
            let kind = plain(i["kind"].as_str().unwrap_or("?"));
            let what = if kind == "image" {
                format!(
                    "{}×{} {}",
                    i["width"].as_u64().unwrap_or(0),
                    i["height"].as_u64().unwrap_or(0),
                    plain(i["mime"].as_str().unwrap_or(""))
                )
            } else {
                plain(i["preview"].as_str().unwrap_or(""))
            };
            format!("{}  {kind}  {what}", i["id"].as_u64().unwrap_or(0))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `lucernad copiar ID`: o `clipboard.copy`.
#[cfg(feature = "clipboard")]
fn copy_command() -> lucerna_cli::CliCommand {
    use lucerna_cli::clap::{Arg, Command};
    use lucerna_cli::{Call, CliCommand};

    CliCommand {
        command: Command::new("copiar")
            .about("Põe um item do histórico na área de transferência.")
            .arg(
                Arg::new("id")
                    .value_name("ID")
                    .required(true)
                    .help("O número do item, como o `historico` mostra."),
            ),
        to_call: |m| {
            let id = m
                .get_one::<String>("id")
                .and_then(|s| s.parse::<u64>().ok())
                .ok_or_else(|| "ID precisa ser um número".to_string())?;
            Ok(Call {
                method: "clipboard.copy".into(),
                params: Some(serde_json::json!({ "id": id })),
            })
        },
        render: None,
    }
}

/// `lucernad brilho [ID PERCENT|+N|-N]`: o `power.list`, o `power.set` ou o
/// `power.change`.
#[cfg(feature = "power")]
fn brightness_command() -> lucerna_cli::CliCommand {
    use lucerna_cli::clap::{Arg, Command};
    use lucerna_cli::{Call, CliCommand};

    CliCommand {
        command: Command::new("brilho")
            .about("Mostra as telas com brilho ajustável; com ID e PERCENT, ajusta uma (+N e -N somam ao valor atual).")
            .arg(
                Arg::new("id")
                    .value_name("ID")
                    .help("A tela, como o `brilho` mostra (backlight ou o id do monitor)."),
            )
            .arg(
                Arg::new("percent")
                    .value_name("PERCENT")
                    .allow_hyphen_values(true)
                    .help("De 1 a 100, ou +N/-N para somar ao valor atual."),
            ),
        to_call: |m| {
            let id = m.get_one::<String>("id");
            let percent = m.get_one::<String>("percent");
            let (id, percent) = match (id, percent) {
                (None, None) => {
                    return Ok(Call {
                        method: "power.list".into(),
                        params: Some(serde_json::json!({})),
                    });
                }
                (Some(id), Some(percent)) => (id, percent),
                _ => return Err("Passe o ID e o PERCENT juntos".to_string()),
            };
            let bad = || "PERCENT precisa ser um inteiro entre 1 e 100, ou +N/-N com N até 100".to_string();
            let number = |s: &str| -> Result<i64, String> {
                if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(bad());
                }
                s.parse::<i64>()
                    .ok()
                    .filter(|n| (1..=100).contains(n))
                    .ok_or_else(bad)
            };
            if let Some(n) = percent.strip_prefix('+') {
                let delta = number(n)?;
                Ok(Call {
                    method: "power.change".into(),
                    params: Some(serde_json::json!({ "id": id, "delta": delta })),
                })
            } else if let Some(n) = percent.strip_prefix('-') {
                let delta = number(n)?;
                Ok(Call {
                    method: "power.change".into(),
                    params: Some(serde_json::json!({ "id": id, "delta": -delta })),
                })
            } else {
                let percent = number(percent)?;
                Ok(Call {
                    method: "power.set".into(),
                    params: Some(serde_json::json!({ "id": id, "percent": percent })),
                })
            }
        },
        render: Some(render_screens),
    }
}

/// Uma linha `id  N%  nome` por tela (`—` no lugar do percentual quando a
/// tela não tem valor lido, e `  (erro)` quando houver); só o
/// `power.list` traz `screens`: o `{}` do `set` não imprime nada.
#[cfg(feature = "power")]
fn render_screens(v: &serde_json::Value) -> String {
    let Some(screens) = v["screens"].as_array() else {
        return String::new();
    };
    if screens.is_empty() {
        return "Nenhuma tela com brilho ajustável.".to_string();
    }
    screens
        .iter()
        .map(|s| {
            let percent = match s["percent"].as_u64() {
                Some(p) => format!("{p}%"),
                None => "—".to_string(),
            };
            let mut line = format!(
                "{}  {percent}  {}",
                plain(s["id"].as_str().unwrap_or_default()),
                plain(s["name"].as_str().unwrap_or_default())
            );
            if let Some(e) = s["error"].as_str() {
                line.push_str(&format!("  ({})", plain(e)));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(all(test, feature = "power"))]
mod power_tests {
    use super::render_screens;
    use serde_json::json;

    #[test]
    fn brilho_lines_have_no_control_characters() {
        let v = json!({"screens": [
            {"id": "backlight", "kind": "backlight", "name": "a\u{1b}[2Jb", "value": 5, "max": 10,
             "percent": 50, "error": "x\u{202E}y"},
            {"id": "ddc", "kind": "ddc", "name": "LG", "value": 1, "max": 100, "percent": 1},
        ]});
        let out = render_screens(&v);
        assert_eq!(
            out,
            "backlight  50%  a\u{FFFD}[2Jb  (x\u{FFFD}y)\nddc  1%  LG"
        );
        assert!(!out.chars().any(|c| c.is_control() && c != '\n'));
    }

    #[test]
    fn screen_without_value_shows_a_dash() {
        let v = json!({"screens": [
            {"id": "ddc:DP-1", "kind": "ddc", "output": "DP-1", "name": "Monitor DP-1",
             "value": null, "max": null, "percent": null,
             "error": "Não achei o barramento deste monitor."},
        ]});
        assert_eq!(
            render_screens(&v),
            "ddc:DP-1  —  Monitor DP-1  (Não achei o barramento deste monitor.)"
        );
    }

    #[test]
    fn no_screens_says_so_and_set_prints_nothing() {
        assert_eq!(
            render_screens(&json!({"screens": []})),
            "Nenhuma tela com brilho ajustável."
        );
        assert_eq!(render_screens(&json!({})), "");
    }
}

#[cfg(all(test, feature = "clipboard"))]
mod tests {
    use super::render_history;
    use serde_json::json;

    #[test]
    fn history_lines_have_no_control_characters() {
        let v = json!({"items": [
            {"id": 3, "kind": "text", "preview": "a\u{1b}[2Jb\u{9b}c\nd"},
            {"id": 2, "kind": "image", "width": 640, "height": 480, "mime": "image/png"},
            {"id": 1, "kind": "files", "preview": "x.pdf"},
        ]});
        let out = render_history(&v);
        assert_eq!(
            out,
            "3  text  a\u{FFFD}[2Jb\u{FFFD}c\u{FFFD}d\n2  image  640×480 image/png\n1  files  x.pdf"
        );
        assert!(!out.chars().any(|c| c.is_control() && c != '\n'));
    }

    #[test]
    fn history_lines_have_no_direction_marks() {
        let v = json!({"items": [
            {"id": 1, "kind": "text", "preview": "a\u{200E}b\u{200F}c\u{202A}d\u{202E}e\u{2066}f\u{2069}g"},
        ]});
        assert_eq!(
            render_history(&v),
            "1  text  a\u{FFFD}b\u{FFFD}c\u{FFFD}d\u{FFFD}e\u{FFFD}f\u{FFFD}g"
        );
    }

    #[test]
    fn empty_history_says_so() {
        assert_eq!(render_history(&json!({"items": []})), "Nada no histórico.");
    }
}
