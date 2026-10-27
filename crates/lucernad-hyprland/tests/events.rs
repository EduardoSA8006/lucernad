//! As linhas do `.socket2.sock` e o leitor de eventos do [`SocketHypr`]
//! contra o servidor falso numa pasta temporária.

use std::time::Duration;

use lucernad_hyprland::events::{CONNECTED, DISCONNECTED, Event, parse_line};
use lucernad_hyprland::testing::FakeSockets;
use lucernad_hyprland::{Hypr, SocketHypr};
use tokio::sync::broadcast;

#[test]
fn parses_event_lines() {
    assert_eq!(
        parse_line("fullscreen>>1"),
        Some(Event {
            name: "fullscreen".into(),
            data: "1".into()
        })
    );
    let e = parse_line("openwindow>>55c4,1,kitty,um, dois>>três").unwrap();
    assert_eq!(e.name, "openwindow");
    assert_eq!(e.data, "55c4,1,kitty,um, dois>>três");
    assert_eq!(
        parse_line("configreloaded>>"),
        Some(Event {
            name: "configreloaded".into(),
            data: String::new()
        })
    );
    assert_eq!(parse_line("sem separador"), None);
    assert_eq!(parse_line(">>sem nome"), None);
}

/// O próximo evento com nome `name`, até 5 s.
async fn next_named(rx: &mut broadcast::Receiver<Event>, name: &str) -> Event {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let e = rx.recv().await.unwrap();
            if e.name == name {
                return e;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("o evento {name} não chegou"))
}

#[tokio::test]
async fn events_reconnect_after_drop() {
    let run = tempfile::tempdir().unwrap();
    let dir = run.path().join("hypr/sig_1");
    std::fs::create_dir_all(&dir).unwrap();
    let fake = FakeSockets::start(&dir);
    let hypr = SocketHypr::at(run.path(), Some("sig_1"));
    let mut rx = hypr.events();
    let c = next_named(&mut rx, CONNECTED).await;
    assert_eq!(c.data, "sig_1");
    fake.emit("openwindow>>abc,1,kitty,kitty");
    assert_eq!(
        next_named(&mut rx, "openwindow").await.data,
        "abc,1,kitty,kitty"
    );
    fake.drop_events();
    next_named(&mut rx, DISCONNECTED).await;
    next_named(&mut rx, CONNECTED).await;
    fake.emit("fullscreen>>1");
    assert_eq!(next_named(&mut rx, "fullscreen").await.data, "1");
    assert_eq!(hypr.instance().as_deref(), Some("sig_1"));
}

/// Uma instância morta: a pasta e o `.socket.sock` ficaram (o Hyprland caiu),
/// mas ninguém atende.
fn dead_instance(run: &std::path::Path, sig: &str) {
    let dir = run.join("hypr").join(sig);
    std::fs::create_dir_all(&dir).unwrap();
    let l = std::os::unix::net::UnixListener::bind(dir.join(".socket.sock")).unwrap();
    drop(l);
    std::os::unix::net::UnixListener::bind(dir.join(".socket2.sock"))
        .map(drop)
        .unwrap();
}

#[tokio::test]
async fn dead_signature_falls_back_to_the_live_instance() {
    let run = tempfile::tempdir().unwrap();
    dead_instance(run.path(), "morta_1");
    let live = run.path().join("hypr/viva_2");
    std::fs::create_dir_all(&live).unwrap();
    let fake = FakeSockets::start(&live);
    // O daemon ficou com a assinatura da sessão que caiu.
    let hypr = SocketHypr::at(run.path(), Some("morta_1"));
    let v = lucernad_hyprland::version(&hypr).await.unwrap();
    assert_eq!(v, "0.56.2");
    assert_eq!(hypr.instance().as_deref(), Some("viva_2"));
    // E os eventos também vão para a viva.
    let mut rx = hypr.events();
    let c = next_named(&mut rx, CONNECTED).await;
    assert_eq!(c.data, "viva_2");
    fake.emit("fullscreen>>1");
    assert_eq!(next_named(&mut rx, "fullscreen").await.data, "1");
}
