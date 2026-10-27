//! O módulo `hyprland` num servidor de verdade: o `hyprland.status` sem
//! Hyprland e com o Hyprland falso (por socket, numa pasta temporária).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lucerna_ipc::Client;
use lucerna_service::ServerBuilder;
use lucernad_hyprland::testing::{FakeHypr, FakeSockets};
use lucernad_hyprland::{Hypr, HyprlandConfig, SocketHypr, register};
use serde_json::{Value, json};
use tokio::net::UnixListener;
use tokio::task::JoinHandle;

fn serve(dir: &Path, hypr: Arc<dyn Hypr>) -> (PathBuf, JoinHandle<()>) {
    let path = dir.join("lucernad.sock");
    let mut builder = ServerBuilder::new("lucernad", "0.1.0");
    let module = register(&mut builder, HyprlandConfig { hypr });
    let server = builder.build();
    let listener = UnixListener::bind(&path).unwrap();
    let task = module.spawn();
    let handle = tokio::spawn(async move {
        let _task = task;
        let _ = server.serve(listener).await;
    });
    (path, handle)
}

/// O `hyprland.status` até `ok` aceitar, em até 5 s.
async fn status_until(path: &Path, ok: impl Fn(&Value) -> bool) -> Value {
    let c = Client::connect(path, "test", "lucernad").await.unwrap();
    let first = c.subscribe(&["hyprland.status"]).await.unwrap();
    let mut last = first[0].data.clone();
    if ok(&last) {
        return last;
    }
    let found = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(s) = c.next_state().await {
            last = s.data;
            if ok(&last) {
                return last.clone();
            }
        }
        panic!("conexão fechada");
    })
    .await;
    found.unwrap_or_else(|_| panic!("o estado não chegou; o último: {last}"))
}

#[tokio::test]
async fn status_without_instance_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let hypr = Arc::new(SocketHypr::at(&dir.path().join("run"), None));
    let (path, _server) = serve(dir.path(), hypr);
    let s = status_until(&path, |v| v["error"].is_string()).await;
    assert_eq!(s["available"], json!(false));
    assert_eq!(s["instance"], Value::Null);
    assert_eq!(s["version"], Value::Null);
    assert!(
        s["error"]
            .as_str()
            .unwrap()
            .contains("Hyprland não está rodando"),
        "{s}"
    );
}

#[tokio::test]
async fn status_with_fake_is_available_with_version() {
    let dir = tempfile::tempdir().unwrap();
    let hypr = Arc::new(FakeHypr::new());
    let (path, _server) = serve(dir.path(), hypr);
    let s = status_until(&path, |v| v["available"] == json!(true)).await;
    assert_eq!(
        s,
        json!({"available": true, "instance": "fake_1", "version": "0.56.2", "error": null})
    );
}

#[tokio::test]
async fn status_follows_the_socket_instance() {
    let dir = tempfile::tempdir().unwrap();
    let run = dir.path().join("run");
    let inst = run.join("hypr/sig_9");
    std::fs::create_dir_all(&inst).unwrap();
    let fake = FakeSockets::start(&inst);
    fake.reply(
        "j/version",
        r#"{"branch": "v0.56.2", "version": "0.56.2", "tag": "v0.56.2"}"#,
    );
    let hypr = Arc::new(SocketHypr::at(&run, None));
    let (path, _server) = serve(dir.path(), hypr);
    let s = status_until(&path, |v| v["available"] == json!(true)).await;
    assert_eq!(s["instance"], json!("sig_9"));
    assert_eq!(s["version"], json!("0.56.2"));
}
