//! A sonda do backend de verdade, num compositor isolado: rode pelo
//! `tests/isolado.sh`, que sobe um sway headless próprio num container, com
//! um `XDG_RUNTIME_DIR` temporário. Fora dele, os testes ficam ignorados, e
//! cada um começa pela trava [`isolated`]: a sessão do usuário nunca é
//! tocada.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use lucernad_clipboard::backend::{Backend, Event, Session, Source};
use lucernad_clipboard::mime::{IMAGE_MAX, Kind, offers_for};
use lucernad_clipboard::pipe::read_pipe;
use lucernad_clipboard::testing::{isolated, png_bytes};
use lucernad_clipboard::wayland::{Protocol, Wayland};

/// O socket do sway isolado.
fn socket() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR do isolado.sh");
    PathBuf::from(dir).join("wayland-1")
}

/// O `wl-copy` do container, contra o sway isolado.
fn wl_copy(args: &[&str], stdin: &[u8]) {
    let mut child = Command::new("wl-copy")
        .args(args)
        .env("WAYLAND_DISPLAY", socket())
        .stdin(Stdio::piped())
        .spawn()
        .expect("rodar o wl-copy");
    child
        .stdin
        .take()
        .expect("o stdin do wl-copy")
        .write_all(stdin)
        .expect("escrever no wl-copy");
    assert!(child.wait().expect("esperar o wl-copy").success());
}

/// O `wl-paste` do container, contra o sway isolado.
fn wl_paste(args: &[&str]) -> Vec<u8> {
    let out = Command::new("wl-paste")
        .args(args)
        .env("WAYLAND_DISPLAY", socket())
        .output()
        .expect("rodar o wl-paste");
    assert!(out.status.success(), "wl-paste {args:?} falhou");
    out.stdout
}

/// O próximo evento que passa no `keep`, em até `within`.
fn next_event(session: &mut Session, within: Duration, keep: impl Fn(&Event) -> bool) -> Event {
    let end = Instant::now() + within;
    loop {
        match session.events.try_recv() {
            Ok(event) if keep(&event) => return event,
            Ok(_) => {}
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                assert!(Instant::now() < end, "nenhum evento esperado em {within:?}");
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("a sessão fechou: {e}"),
        }
    }
}

/// Espera uma seleção com `mime` entre os tipos e lê o conteúdo nele.
fn read_selection(session: &mut Session, mime: &str) -> Vec<u8> {
    let event = next_event(
        session,
        Duration::from_secs(2),
        |e| matches!(e, Event::Selection { mimes, .. } if mimes.iter().any(|m| m == mime)),
    );
    let Event::Selection { offer, .. } = event else {
        unreachable!()
    };
    let reader = session.control.receive(offer, mime).expect("receber");
    read_pipe(reader, IMAGE_MAX, Instant::now() + Duration::from_secs(3)).expect("ler o pipe")
}

#[test]
#[ignore = "só no compositor isolado (tests/isolado.sh)"]
fn sees_a_real_copy_and_reads_it() {
    isolated();
    for protocol in [Protocol::Ext, Protocol::Wlr] {
        let backend = Wayland {
            only: Some(protocol),
        };
        let mut session = backend.open(&socket()).expect("abrir a sessão");

        wl_copy(&[], "olá mundo".as_bytes());
        let text = read_selection(&mut session, "text/plain;charset=utf-8");
        assert_eq!(String::from_utf8_lossy(&text), "olá mundo", "{protocol:?}");

        let png = png_bytes(64, 64);
        wl_copy(&["--type", "image/png"], &png);
        assert_eq!(
            read_selection(&mut session, "image/png"),
            png,
            "{protocol:?}"
        );
    }
}

#[test]
#[ignore = "só no compositor isolado (tests/isolado.sh)"]
fn serves_what_it_offers() {
    isolated();
    let mut session = Wayland::system().open(&socket()).expect("abrir a sessão");
    let offers = offers_for(Kind::Text, None, b"colado", 5);
    let mut expected: Vec<String> = offers.iter().map(|(m, _)| m.clone()).collect();
    session.control.offer(Source { offers });

    // A nossa seleção volta como uma seleção comum.
    next_event(
        &mut session,
        Duration::from_secs(2),
        |e| matches!(e, Event::Selection { mimes, .. } if mimes.len() == expected.len()),
    );
    let listed = String::from_utf8(wl_paste(&["--list-types"])).expect("tipos em UTF-8");
    let mut listed: Vec<String> = listed.lines().map(str::to_string).collect();
    listed.sort();
    expected.sort();
    assert_eq!(listed, expected);
    assert_eq!(wl_paste(&["--no-newline"]), b"colado");
    assert_eq!(
        wl_paste(&["--no-newline", "--type", "application/x-lucernad-item"]),
        b"5"
    );

    wl_copy(&[], b"outro");
    next_event(&mut session, Duration::from_secs(2), |e| {
        *e == Event::Cancelled
    });
}

#[test]
#[ignore = "só no compositor isolado (tests/isolado.sh)"]
fn compositor_drop_is_an_event() {
    isolated();
    let mut session = Wayland::system().open(&socket()).expect("abrir a sessão");
    let pid = std::env::var("LUCERNA_SWAY_PID").expect("LUCERNA_SWAY_PID do isolado.sh");
    let status = Command::new("kill")
        .arg(&pid)
        .status()
        .expect("rodar o kill");
    assert!(status.success(), "matar o sway {pid}");
    let event = next_event(&mut session, Duration::from_secs(2), |e| {
        matches!(e, Event::Disconnected(_))
    });
    let Event::Disconnected(why) = event else {
        unreachable!()
    };
    assert!(why.starts_with("O compositor fechou a conexão: "), "{why}");
}
