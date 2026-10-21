//! O protocolo do `lucernad`, em processo: o `system` sobre a máquina de
//! exemplo, o `spectrum` e o `clipboard` com os dublês.
#![cfg(any(feature = "system", feature = "spectrum", feature = "clipboard"))]

mod common;

use common::start;
#[cfg(feature = "system")]
use serde_json::Value;
#[cfg(any(feature = "spectrum", feature = "clipboard"))]
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
