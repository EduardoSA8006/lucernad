//! O protocolo do `lucernad`, em processo, sobre a máquina de exemplo.
#![cfg(feature = "system")]

mod common;

use common::start;
use serde_json::Value;

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
