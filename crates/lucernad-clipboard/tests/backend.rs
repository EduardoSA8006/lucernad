//! O contrato do `Backend`: o dublê que os testes do módulo usam, o Wayland
//! sem compositor e a trava da sonda.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lucernad_clipboard::backend::{Backend, Event, Source};
use lucernad_clipboard::pipe::read_pipe;
use lucernad_clipboard::testing::{FakeBackend, isolated};
use lucernad_clipboard::wayland::Wayland;
use tokio::sync::mpsc::error::TryRecvError;

fn socket() -> &'static Path {
    Path::new("/run/teste/wayland-1")
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(3)
}

#[test]
fn fake_copy_reads_through_a_pipe() {
    let fake = FakeBackend::new();
    let mut session = fake.open(socket()).expect("abrir a sessão do dublê");

    let offer = fake.copy(&["text/plain"], &[("text/plain", b"oi")]);
    assert_eq!(offer, 1);
    assert_eq!(
        session.events.try_recv(),
        Ok(Event::Selection {
            offer: 1,
            mimes: vec!["text/plain".to_string()],
        })
    );

    let reader = session
        .control
        .receive(1, "text/plain")
        .expect("receber a oferta atual");
    assert_eq!(read_pipe(reader, 100, deadline()), Ok(b"oi".to_vec()));
    assert_eq!(
        session.control.receive(0, "text/plain").err(),
        Some("a seleção mudou".to_string())
    );
    // Conta todo `receive`, também o da oferta velha.
    assert_eq!(fake.reads(), 2);

    // A cópia presa: o pipe nunca fecha.
    let offer = fake.copy_stuck(&["text/plain"]);
    assert!(matches!(
        session.events.try_recv(),
        Ok(Event::Selection { .. })
    ));
    let reader = session
        .control
        .receive(offer, "text/plain")
        .expect("receber");
    let short = Instant::now() + Duration::from_millis(100);
    assert!(
        read_pipe(reader, 100, short).is_err(),
        "o pipe ficou aberto"
    );
    assert_eq!(fake.reads(), 3);
}

#[test]
fn fake_serves_and_cancels() {
    let fake = FakeBackend::new();
    let mut session = fake.open(socket()).expect("abrir a sessão do dublê");
    assert_eq!(fake.owned(), None);

    let data: Arc<[u8]> = b"x".as_slice().into();
    session.control.offer(Source {
        offers: vec![("text/plain".to_string(), data)],
    });
    assert_eq!(fake.owned(), Some(vec!["text/plain".to_string()]));
    assert_eq!(fake.paste("text/plain"), Some(b"x".to_vec()));
    assert_eq!(fake.paste("image/png"), None);
    // Como o compositor de verdade, a nossa fonte volta como seleção comum.
    assert_eq!(
        session.events.try_recv(),
        Ok(Event::Selection {
            offer: 1,
            mimes: vec!["text/plain".to_string()],
        })
    );

    fake.cancel();
    assert_eq!(session.events.try_recv(), Ok(Event::Cancelled));
    assert_eq!(fake.owned(), None);

    // Sem fonte nossa, o `cancel` não manda nada.
    fake.cancel();
    assert_eq!(session.events.try_recv(), Err(TryRecvError::Empty));
}

#[test]
fn fake_counts_and_fails() {
    let fake = FakeBackend::new();
    let session = fake.open(socket()).expect("abrir a sessão do dublê");
    assert_eq!(fake.sessions(), 1);
    drop(session);
    assert_eq!(fake.sessions(), 0);
    assert_eq!(fake.opened(), 1);
    assert_eq!(fake.sockets(), vec![PathBuf::from("/run/teste/wayland-1")]);

    let failing = FakeBackend::failing("e");
    assert_eq!(failing.open(socket()).err(), Some("e".to_string()));
    assert_eq!(failing.opened(), 0);

    let mut session = fake.open(socket()).expect("abrir de novo");
    fake.disconnect("y");
    assert_eq!(
        session.events.try_recv(),
        Ok(Event::Disconnected("y".to_string()))
    );
    assert_eq!(session.events.try_recv(), Err(TryRecvError::Disconnected));
    assert_eq!(fake.opened(), 2);

    let panicking = FakeBackend::panicking();
    let result = std::panic::catch_unwind(|| panicking.open(socket()).map(drop));
    assert!(result.is_err(), "o open do dublê em pânico");
}

#[test]
fn sonda_refuses_outside_the_container() {
    // No container da sonda a trava deixa passar: aqui não há o que conferir.
    if std::env::var_os("LUCERNA_ISOLADO").is_some() {
        return;
    }
    let payload = std::panic::catch_unwind(isolated).expect_err("a trava recusa");
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .expect("o texto do pânico");
    assert_eq!(
        message,
        "sonda.rs: só roda pelo tests/isolado.sh, num container"
    );
}

#[test]
fn wayland_without_compositor_is_an_error() {
    let start = Instant::now();
    let err = Wayland::system()
        .open(Path::new("/inexistente/wayland-9"))
        .err()
        .expect("sem compositor, a abertura falha");
    assert!(
        err.starts_with("Não consegui conectar ao Wayland (/inexistente/wayland-9)"),
        "{err}"
    );
    assert!(start.elapsed() < Duration::from_secs(3));
}
