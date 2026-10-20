//! O `lucernad`: o daemon do Lucerna, um binário com um módulo por área.
//!
//! Cada módulo é um crate ligado por uma feature (o primeiro é o `system`,
//! padrão) e se registra no mesmo serviço. [`lucernad`] monta o serviço com
//! os módulos ligados; [`cli`] é a linha de comando, só com os subcomandos
//! comuns do SDK (`status` e `watch`).

use lucerna_cli::CliApp;
use lucerna_service::ServiceBuilder;

/// Id do app.
pub const APP: &str = "lucernad";

/// A configuração de cada módulo ligado.
pub struct Modules {
    /// O módulo `system`.
    #[cfg(feature = "system")]
    pub system: lucernad_system::SystemConfig,
}

impl Modules {
    /// Os módulos sobre a máquina de verdade.
    pub fn real() -> Modules {
        Modules {
            #[cfg(feature = "system")]
            system: lucernad_system::SystemConfig::system(),
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
    pub system_task: lucernad_system::ModuleTask,
}

/// Monta o serviço com os módulos ligados. Chame dentro do runtime: começa as
/// tarefas dos módulos.
pub fn lucernad(modules: Modules) -> Lucernad {
    // Sem nenhum módulo, ninguém registra nada no servidor.
    #[cfg_attr(not(feature = "system"), allow(unused_mut))]
    let mut service = ServiceBuilder::new(APP, env!("CARGO_PKG_VERSION"));
    #[cfg(feature = "system")]
    let system = lucernad_system::register(service.server(), modules.system);
    #[cfg(not(feature = "system"))]
    let Modules {} = modules;
    Lucernad {
        service,
        #[cfg(feature = "system")]
        system: system.topics.clone(),
        #[cfg(feature = "system")]
        system_task: system.spawn(),
    }
}

/// A linha de comando: só os subcomandos comuns do SDK.
pub fn cli() -> CliApp {
    CliApp::new(
        APP,
        "lucernad",
        env!("CARGO_PKG_VERSION"),
        "O daemon do Lucerna: os dados do sistema para o shell.",
    )
}
