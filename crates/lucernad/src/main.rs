//! O binário `lucernad`: `serve` roda o daemon; `status`, `watch`, `player`, `historico`, `copiar`, `brilho`,
//! `faixa`, `cidade`, `rede` e `jogo` falam com ele. Sem argumentos (ou com `tui …`), mostra a ajuda: não há TUI.
//! O escondido `restaurar-jogo` (o `ExecStopPost` do serviço) desfaz o modo jogo pelo diário, sem daemon.

use std::fmt::Display;
use std::process::ExitCode;

use lucerna_cli::{Invocation, usage_exit};
use lucernad::{Modules, cli, lucernad};

#[tokio::main]
async fn main() -> ExitCode {
    // Fora do `lucerna-cli`: tratado antes dele, e escondido da ajuda.
    #[cfg(feature = "game")]
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("restaurar-jogo")) {
        let code = lucernad_game::compositor::restore_from_journal_cli().await;
        return ExitCode::from(u8::try_from(code).unwrap_or(1));
    }
    let cli = cli();
    let code = match cli.parse_from(std::env::args_os()) {
        // Sem TUI: `lucernad` e `lucernad tui …` mostram a ajuda.
        Ok(Invocation::Tui(_)) => report(cli.clap().print_help()),
        Ok(Invocation::Serve) => {
            // As tarefas dos módulos vivem até o serviço parar. As threads
            // soltas (statvfs, NVML, PipeWire, Wayland) não seguram a saída do processo.
            let d = lucernad(Modules::real());
            #[cfg(feature = "system")]
            let _system_task = d.system_task;
            #[cfg(feature = "spectrum")]
            let _spectrum_task = d.spectrum_task;
            #[cfg(feature = "clipboard")]
            let _clipboard_task = d.clipboard_task;
            #[cfg(feature = "power")]
            let _power_task = d.power_task;
            #[cfg(feature = "web")]
            let _web_task = d.web_task;
            #[cfg(feature = "hyprland")]
            let _hyprland_task = d.hyprland_task;
            #[cfg(feature = "game")]
            let _game_task = d.game_task;
            let r = d.service.run().await;
            // Antes de sair, o modo jogo desfaz tudo (o ExecStopPost cobre
            // a queda).
            #[cfg(feature = "game")]
            d.game_handle.shutdown().await;
            report(r)
        }
        Ok(Invocation::Cli(run)) => cli.run_from_env(run).await,
        Err(e) => usage_exit(&e),
    };
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// `0` no sucesso; no erro, `erro: …` no stderr e `1`.
fn report<E: Display>(r: Result<(), E>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("erro: {e}");
            1
        }
    }
}
