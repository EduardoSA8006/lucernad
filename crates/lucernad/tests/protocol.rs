//! O protocolo do `lucernad`, em processo: o `system` sobre a máquina de
//! exemplo, o `spectrum` e o `clipboard` com os dublês.
#![cfg(any(
    feature = "system",
    feature = "spectrum",
    feature = "clipboard",
    feature = "power",
    feature = "web",
    feature = "hyprland"
))]

mod common;

use common::start;
#[cfg(feature = "system")]
use serde_json::Value;
#[cfg(any(
    feature = "spectrum",
    feature = "clipboard",
    feature = "power",
    feature = "web",
    feature = "hyprland"
))]
use serde_json::json;

#[cfg(feature = "system")]
#[tokio::test]
async fn the_daemon_serves_the_system_module() {
    let r = start().await;
    let c = r.client().await;
    assert_eq!(c.hello().app, "lucernad");
    assert_eq!(c.hello().version, env!("CARGO_PKG_VERSION"));
    for t in [
        "system.info",
        "system.stats",
        "system.disks",
        "system.gpus",
        "system.status",
    ] {
        assert!(c.hello().topics.iter().any(|x| x == t), "falta {t}");
    }
    assert!(
        c.hello().methods.iter().any(|m| m == "system.configure"),
        "falta o system.configure"
    );
    let st = c.subscribe(&["system.stats"]).await.unwrap();
    assert_eq!(st[0].data, Value::Null, "nada antes da primeira amostra");
    let s = c.next_state().await.unwrap();
    assert_eq!(s.topic, "system.stats");
    assert_eq!(s.data["memTotal"], 16384000);
    r.stop().await;
}

#[cfg(feature = "spectrum")]
#[tokio::test]
async fn the_daemon_serves_the_spectrum_module() {
    let r = start().await;
    let c = r.client().await;
    assert_eq!(c.hello().streams, ["spectrum.bands"]);
    assert!(
        c.hello().topics.iter().any(|t| t == "spectrum.status"),
        "{:?}",
        c.hello().topics
    );
    let v = c
        .call("spectrum.configure", Some(json!({"player": null})))
        .await
        .unwrap();
    assert_eq!(v, json!({}));
    r.stop().await;
}

#[cfg(feature = "clipboard")]
#[tokio::test]
async fn the_daemon_serves_the_clipboard_module() {
    let r = start().await;
    let c = r.client().await;
    for t in ["clipboard.history", "clipboard.status"] {
        assert!(c.hello().topics.iter().any(|x| x == t), "falta {t}");
    }
    assert!(
        c.hello().methods.iter().any(|m| m == "clipboard.list"),
        "falta o clipboard.list"
    );
    let v = c
        .call(
            "clipboard.configure",
            Some(json!({"limit": 100, "persist": false})),
        )
        .await
        .unwrap();
    assert_eq!(v, json!({}));
    let v = c
        .call("clipboard.list", Some(json!({"offset": 0, "limit": 5})))
        .await
        .unwrap();
    assert_eq!(v, json!({"items": [], "total": 0}));
    r.stop().await;
}

#[cfg(feature = "power")]
#[tokio::test]
async fn the_daemon_serves_the_power_module() {
    let r = start().await;
    let c = r.client().await;
    for t in [
        "power.screens",
        "power.history",
        "power.profiles",
        "power.status",
    ] {
        assert!(c.hello().topics.iter().any(|x| x == t), "falta {t}");
    }
    assert!(
        c.hello().methods.iter().any(|m| m == "power.list"),
        "falta o power.list"
    );
    let v = c.call("power.list", Some(json!({}))).await.unwrap();
    assert_eq!(v, json!({"screens": []}));
    r.stop().await;
}

#[cfg(feature = "web")]
#[tokio::test]
async fn the_daemon_serves_the_web_module() {
    let r = start().await;
    let c = r.client().await;
    for t in ["web.lyrics", "web.weather", "web.status"] {
        assert!(c.hello().topics.iter().any(|x| x == t), "falta {t}");
    }
    assert!(
        c.hello().methods.iter().any(|m| m == "web.searchCity"),
        "falta o web.searchCity"
    );
    // Antes do `configure` o módulo está offline: o 1001, sem nenhum pedido.
    let e = c
        .call("web.searchCity", Some(json!({"query": "x"})))
        .await
        .unwrap_err();
    match e {
        lucerna_ipc::ClientError::Rpc(e) => assert_eq!(e.code, 1001, "{}", e.message),
        other => panic!("esperava o 1001: {other:?}"),
    }
    r.stop().await;
}

#[cfg(feature = "hyprland")]
#[tokio::test]
async fn the_daemon_serves_the_hyprland_module() {
    let r = start().await;
    let c = r.client().await;
    assert!(
        c.hello().topics.iter().any(|x| x == "hyprland.status"),
        "falta o hyprland.status"
    );
    let mut s = c.subscribe(&["hyprland.status"]).await.unwrap()[0]
        .data
        .clone();
    while s["available"] != json!(true) {
        s = tokio::time::timeout(std::time::Duration::from_secs(5), c.next_state())
            .await
            .unwrap()
            .unwrap()
            .data;
    }
    assert_eq!(s["version"], json!("0.56.2"));
    r.stop().await;
}

#[cfg(feature = "game")]
#[tokio::test]
async fn the_daemon_serves_the_game_module() {
    let r = start().await;
    let c = r.client().await;
    for t in ["game.state", "game.status"] {
        assert!(c.hello().topics.iter().any(|x| x == t), "falta {t}");
    }
    for m in ["game.set", "game.toggle", "game.configure", "game.get"] {
        assert!(c.hello().methods.iter().any(|x| x == m), "falta {m}");
    }
    let s = c.call("game.get", Some(json!({}))).await.unwrap();
    assert_eq!(s["active"], json!(false));
    // Ligado na mão, o compositor do Hyprland falso do módulo hyprland.
    c.call("game.set", Some(json!({"on": true}))).await.unwrap();
    let s = c.call("game.get", Some(json!({}))).await.unwrap();
    assert_eq!(s["active"], json!(true), "{s}");
    assert_eq!(s["effects"]["compositor"], json!("ok"), "{s}");
    assert_eq!(s["effects"]["light"], json!("ok"), "{s}");
    c.call("game.set", Some(json!({"on": false})))
        .await
        .unwrap();
    r.stop().await;
}
