//! O protocolo do `lucernad`, em processo: o `system` sobre a máquina de
//! exemplo e o `spectrum` com os dublês.
#![cfg(any(feature = "system", feature = "spectrum"))]

mod common;

use common::start;
#[cfg(feature = "system")]
use serde_json::Value;
#[cfg(feature = "spectrum")]
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
    assert_eq!(
        c.hello().topics.last().map(String::as_str),
        Some("spectrum.status"),
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
