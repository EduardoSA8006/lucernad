//! O `lucernad`: o daemon do Lucerna, um binário com um módulo por área.
//!
//! Cada módulo é um crate ligado por uma feature (`system`, `spectrum` e
//! `clipboard`, as três padrão) e se registra no mesmo serviço. [`lucernad`] monta o serviço
//! com os módulos ligados; [`cli`] é a linha de comando: os subcomandos comuns
//! do SDK (`status` e `watch`) e, com o `spectrum`, o `player`; com o
//! `clipboard`, o `historico` e o `copiar`.

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
}

/// Monta o serviço com os módulos ligados. Chame dentro do runtime: começa as
/// tarefas dos módulos.
pub fn lucernad(modules: Modules) -> Lucernad {
    // Sem nenhum módulo, ninguém registra nada no servidor.
    #[cfg_attr(
        not(any(feature = "system", feature = "spectrum", feature = "clipboard")),
        allow(unused_mut)
    )]
    let mut service = ServiceBuilder::new(APP, env!("CARGO_PKG_VERSION"));
    #[cfg(feature = "system")]
    let system = lucernad_system::register(service.server(), modules.system);
    #[cfg(feature = "spectrum")]
    let spectrum = lucernad_spectrum::register(service.server(), modules.spectrum);
    #[cfg(feature = "clipboard")]
    let work = service.work();
    #[cfg(feature = "clipboard")]
    let clipboard = lucernad_clipboard::register(service.server(), modules.clipboard, work);
    #[cfg(not(any(feature = "system", feature = "spectrum", feature = "clipboard")))]
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
    }
}

/// A linha de comando: os subcomandos comuns do SDK e os dos módulos.
pub fn cli() -> CliApp {
    let app = CliApp::new(
        APP,
        "lucernad",
        env!("CARGO_PKG_VERSION"),
        "O daemon do Lucerna: os dados do sistema, o espectro do som e o histórico da área de transferência para o shell.",
    );
    #[cfg(feature = "spectrum")]
    let app = app.command(player_command());
    #[cfg(feature = "clipboard")]
    let app = app.command(history_command()).command(copy_command());
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
            let kind = i["kind"].as_str().unwrap_or("?");
            let what = if kind == "image" {
                format!(
                    "{}×{} {}",
                    i["width"].as_u64().unwrap_or(0),
                    i["height"].as_u64().unwrap_or(0),
                    i["mime"].as_str().unwrap_or("")
                )
            } else {
                i["preview"].as_str().unwrap_or("").to_string()
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
