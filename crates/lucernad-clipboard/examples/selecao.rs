//! Copiar e colar sem o `wl-clipboard`, pelo próprio backend Wayland do
//! módulo (para a fumaça e a conferência, nunca contra a sessão do usuário
//! sem querer: a exibição é a do `WAYLAND_DISPLAY` e do `XDG_RUNTIME_DIR`).
//!
//! ```text
//! selecao oferecer MIME… < dados   vira dono da seleção com os bytes do stdin
//!                                  em cada tipo; imprime "pronto" e sai com 0
//!                                  quando outra cópia a substitui
//! selecao ler MIME                 imprime os bytes do tipo na seleção atual
//!                                  (sem ele, sai com 1)
//! selecao tipos                    imprime os tipos da seleção atual
//! ```

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use lucernad_clipboard::backend::{Backend, Event, Session, Source};
use lucernad_clipboard::mime::{IMAGE_MAX, READ_TIMEOUT};
use lucernad_clipboard::pipe::{read_pipe, socket_path};
use lucernad_clipboard::wayland::Wayland;

const USAGE: &str = "uso: selecao oferecer MIME… < dados | selecao ler MIME | selecao tipos";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("oferecer") if args.len() > 1 => offer(&args[1..]),
        Some("ler") if args.len() == 2 => read(&args[1]),
        Some("tipos") if args.len() == 1 => types(),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("selecao: {e}");
            ExitCode::FAILURE
        }
    }
}

fn open() -> Result<Session, String> {
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let socket = socket_path(&display, runtime.as_deref())?;
    Wayland::system().open(&socket)
}

/// A seleção atual, que chega com a abertura (o `open` faz um roundtrip).
fn current(session: &mut Session) -> Option<(u64, Vec<String>)> {
    let mut found = None;
    while let Ok(event) = session.events.try_recv() {
        found = match event {
            Event::Selection { offer, mimes } => Some((offer, mimes)),
            _ => None,
        };
    }
    found
}

fn offer(mimes: &[String]) -> Result<ExitCode, String> {
    let mut data = Vec::new();
    std::io::stdin()
        .read_to_end(&mut data)
        .map_err(|e| format!("ler o stdin: {e}"))?;
    let data: Arc<[u8]> = data.into();
    let mut session = open()?;
    current(&mut session);
    session.control.offer(Source {
        offers: mimes.iter().map(|m| (m.clone(), data.clone())).collect(),
    });
    let wanted = sorted(mimes.to_vec());
    let mut ready = false;
    while let Some(event) = session.events.blocking_recv() {
        match event {
            // A nossa fonte volta como seleção: virou dono.
            Event::Selection { mimes, .. } if !ready && sorted(mimes.clone()) == wanted => {
                ready = true;
                println!("pronto");
                let _ = std::io::stdout().flush();
            }
            Event::Cancelled => return Ok(ExitCode::SUCCESS),
            Event::Disconnected(why) => return Err(why),
            _ => {}
        }
    }
    Err("a sessão fechou".to_string())
}

fn sorted(mut mimes: Vec<String>) -> Vec<String> {
    mimes.sort();
    mimes
}

fn read(mime: &str) -> Result<ExitCode, String> {
    let mut session = open()?;
    let Some((offer, mimes)) = current(&mut session) else {
        return Ok(ExitCode::FAILURE);
    };
    if !mimes.iter().any(|m| m == mime) {
        return Ok(ExitCode::FAILURE);
    }
    let reader = session.control.receive(offer, mime)?;
    let data = read_pipe(reader, IMAGE_MAX, Instant::now() + READ_TIMEOUT)
        .map_err(|e| format!("ler a seleção: {e:?}"))?;
    let mut out = std::io::stdout();
    out.write_all(&data)
        .and_then(|()| out.flush())
        .map_err(|e| format!("escrever: {e}"))?;
    Ok(ExitCode::SUCCESS)
}

fn types() -> Result<ExitCode, String> {
    let mut session = open()?;
    for mime in current(&mut session).map(|(_, m)| m).unwrap_or_default() {
        println!("{mime}");
    }
    Ok(ExitCode::SUCCESS)
}
