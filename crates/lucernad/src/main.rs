//! O binário `lucernad`: `serve` roda o daemon; `status` e `watch` falam com
//! ele. Sem argumentos (ou com `tui …`), mostra a ajuda: não há TUI.

use std::fmt::Display;
use std::process::ExitCode;

use lucerna_cli::{Invocation, usage_exit};
use lucernad::{Modules, cli, lucernad};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = cli();
    let code = match cli.parse_from(std::env::args_os()) {
        // Sem TUI: `lucernad` e `lucernad tui …` mostram a ajuda.
        Ok(Invocation::Tui(_)) => report(cli.clap().print_help()),
        Ok(Invocation::Serve) => {
            // As tarefas dos módulos vivem até o serviço parar. As threads
            // soltas (statvfs, NVML) não seguram a saída do processo.
            let d = lucernad(Modules::real());
            #[cfg(feature = "system")]
            let _task = d.system_task;
            report(d.service.run().await)
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
