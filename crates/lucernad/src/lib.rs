//! O `lucernad`: o daemon do Lucerna, um binário com um módulo por área.
//!
//! Cada módulo é um crate ligado por uma feature (`system` e `spectrum`, as
//! duas padrão) e se registra no mesmo serviço. [`lucernad`] monta o serviço
//! com os módulos ligados; [`cli`] é a linha de comando: os subcomandos comuns
//! do SDK (`status` e `watch`) e, com o `spectrum`, o `player`.

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
}

impl Modules {
    /// Os módulos sobre a máquina de verdade.
    pub fn real() -> Modules {
        Modules {
            #[cfg(feature = "system")]
            system: lucernad_system::SystemConfig::system(),
            #[cfg(feature = "spectrum")]
            spectrum: lucernad_spectrum::SpectrumConfig::system(),
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
}

/// Monta o serviço com os módulos ligados. Chame dentro do runtime: começa as
/// tarefas dos módulos.
pub fn lucernad(modules: Modules) -> Lucernad {
    // Sem nenhum módulo, ninguém registra nada no servidor.
    #[cfg_attr(not(any(feature = "system", feature = "spectrum")), allow(unused_mut))]
    let mut service = ServiceBuilder::new(APP, env!("CARGO_PKG_VERSION"));
    #[cfg(feature = "system")]
    let system = lucernad_system::register(service.server(), modules.system);
    #[cfg(feature = "spectrum")]
    let spectrum = lucernad_spectrum::register(service.server(), modules.spectrum);
    #[cfg(not(any(feature = "system", feature = "spectrum")))]
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
    }
}

/// A linha de comando: os subcomandos comuns do SDK e os dos módulos.
pub fn cli() -> CliApp {
    let app = CliApp::new(
        APP,
        "lucernad",
        env!("CARGO_PKG_VERSION"),
        "O daemon do Lucerna: os dados do sistema e o espectro do som para o shell.",
    );
    #[cfg(feature = "spectrum")]
    let app = app.command(player_command());
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
