//! O contrato do `Backend`: o dublê que os testes do módulo usam, o PipeWire
//! sem servidor e os sinais de teste.

use std::time::{Duration, Instant};

use lucernad_spectrum::backend::{Backend, Event, OutputNode};
use lucernad_spectrum::pipewire::PipeWire;
use lucernad_spectrum::testing::{FakeBackend, noise, sine};
use tokio::sync::mpsc::error::TryRecvError;

fn node(id: u32, serial: u64) -> OutputNode {
    OutputNode {
        id,
        serial,
        client_pid: Some(1000 + id),
        binary: Some("spotify".into()),
    }
}

#[test]
fn fake_session_reports_graph_and_samples() {
    let fake = FakeBackend::new();
    let nodes = vec![node(89, 121), node(95, 178)];
    fake.set_nodes(nodes.clone());

    let mut session = fake.open().expect("abrir a sessão do dublê");
    assert_eq!(session.events.try_recv(), Ok(Event::Graph(nodes)));

    session.control.capture(&[121]);
    assert_eq!(fake.capturing(), vec![121]);

    fake.play(121, 48000, &[0.5; 10]);
    assert_eq!(
        session.events.try_recv(),
        Ok(Event::Samples {
            serial: 121,
            rate: 48000,
            mono: vec![0.5; 10],
        })
    );

    fake.play(178, 48000, &[0.5; 10]);
    assert_eq!(session.events.try_recv(), Err(TryRecvError::Empty));
}

#[test]
fn fake_counts_sessions_and_failures() {
    let fake = FakeBackend::new();
    let session = fake.open().expect("abrir a sessão do dublê");
    assert_eq!(fake.sessions(), 1);
    drop(session);
    assert_eq!(fake.sessions(), 0);
    assert_eq!(fake.opened(), 1);
    assert_eq!(fake.capturing(), Vec::<u64>::new());

    let failing = FakeBackend::failing("x");
    assert_eq!(failing.probe(), Err("x".to_string()));
    assert_eq!(failing.open().err(), Some("x".to_string()));

    let fake = FakeBackend::new();
    let mut session = fake.open().expect("abrir a sessão do dublê");
    // A sessão nova recebe na hora o `Graph` guardado (aqui, vazio).
    assert_eq!(session.events.try_recv(), Ok(Event::Graph(Vec::new())));
    fake.disconnect("y");
    assert_eq!(
        session.events.blocking_recv(),
        Some(Event::Disconnected("y".into()))
    );
    assert_eq!(session.events.blocking_recv(), None);
}

#[test]
fn pipewire_without_server_is_an_error() {
    let pipewire = PipeWire {
        remote: Some("/inexistente/lucernad-teste".into()),
        timeout: Duration::from_secs(2),
    };

    let start = Instant::now();
    let err = pipewire.probe().expect_err("sem servidor, a sonda falha");
    assert!(
        err.starts_with("Não consegui conectar ao PipeWire"),
        "erro: {err}"
    );
    assert!(start.elapsed() < Duration::from_secs(3));

    let start = Instant::now();
    let err = pipewire.open().err().expect("sem servidor, a sessão falha");
    assert!(
        err.starts_with("Não consegui conectar ao PipeWire"),
        "erro: {err}"
    );
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[test]
fn signals_are_deterministic() {
    let wave = sine(1000.0, 48000, 0.5, 13);
    assert_eq!(wave.len(), 13);
    // 12 amostras a 48 kHz são um quarto de período de 1 kHz: o pico.
    assert!((wave[12] - 0.5).abs() < 1e-6, "amostra: {}", wave[12]);

    let a = noise(7, 1000);
    assert_eq!(a, noise(7, 1000));
    assert!(a.iter().all(|x| (-1.0..1.0).contains(x)));
    let mean = a.iter().sum::<f32>() / a.len() as f32;
    assert!(mean.abs() < 0.05, "média: {mean}");
}
