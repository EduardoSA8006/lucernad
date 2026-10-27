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
