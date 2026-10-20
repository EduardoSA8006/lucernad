use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lucerna_ipc::{Client, ClientError, Topic};
use lucerna_service::ServerBuilder;
use lucernad_system::testing::{FixedStatfs, PanicNvidia, machine_config, testdata};
use lucernad_system::{ModuleTask, Roots, System, SystemConfig, SystemTopics, register};
use serde_json::json;
use tempfile::TempDir;
use tokio::net::UnixListener;
use tokio::task::JoinHandle;

/// Registra o módulo num servidor e serve num socket temporário, sem rodar o
/// laço (quem chama dá o `spawn`).
fn build(cfg: SystemConfig) -> (TempDir, PathBuf, SystemTopics, System, JoinHandle<()>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lucernad.sock");
    let mut builder = ServerBuilder::new("lucernad", "0.1.0");
    let system = register(&mut builder, cfg);
    let topics = system.topics.clone();
    let server = builder.build();
    let listener = UnixListener::bind(&path).unwrap();
    let handle = tokio::spawn(async move {
        let _ = server.serve(listener).await;
    });
    (dir, path, topics, system, handle)
}

/// O [`build`] com o laço rodando. O [`ModuleTask`] vai junto do retorno,
/// mas os testes que o ignoram com `..` o largariam no fim do `let`: por isso
/// ele fica guardado na tarefa do servidor, que vive até o fim do teste.
async fn serve(cfg: SystemConfig) -> (TempDir, PathBuf, SystemTopics, JoinHandle<()>) {
    let (dir, path, topics, system, server) = build(cfg);
    let task: ModuleTask = system.spawn();
    let handle = tokio::spawn(async move {
        let _task = task;
        let _ = server.await;
    });
    (dir, path, topics, handle)
}

async fn client(path: &Path) -> Client {
    Client::connect(path, "test", "lucernad").await.unwrap()
}

/// Espera o `seq` do tópico chegar a `n` até `within`; senão, pânico.
async fn wait_seq_within(topic: &Topic, n: u64, within: Duration) {
    let end = tokio::time::Instant::now() + within;
    while topic.current().seq < n {
        assert!(
            tokio::time::Instant::now() < end,
            "{}: seq {} < {n} depois de {within:?}",
            topic.name(),
            topic.current().seq
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn wait_seq(topic: &Topic, n: u64) {
    wait_seq_within(topic, n, Duration::from_secs(1)).await;
}

/// O `testdata/machine/proc` copiado para `dir`, com um processo (300) com o
/// `/dev/nvidia0` aberto.
fn proc_with_nvidia_user(dir: &TempDir) -> PathBuf {
    lucernad_system::testing::proc_with_nvidia_user(dir.path())
}

#[tokio::test]
async fn handshake_lists_the_system_topics_and_method() {
    let (_d, path, ..) = serve(machine_config()).await;
    let c = client(&path).await;
    assert_eq!(
        c.hello().topics,
        [
            "system.info",
            "system.stats",
            "system.disks",
            "system.gpus",
            "system.status"
        ]
    );
    assert_eq!(c.hello().methods, ["system.configure"]);
}

#[tokio::test]
async fn info_is_read_once_at_start() {
    let (_d, path, ..) = serve(machine_config()).await;
    let st = client(&path)
        .await
        .subscribe(&["system.info", "system.status"])
        .await
        .unwrap();
    assert_eq!(
        st[0].data,
        json!({"cpuModel":"Intel(R) Core(TM) i7-1165G7","cpuThreads":2,"hostname":"maquina","osName":"Arch Linux","kernel":"6.17.2-arch1-1"})
    );
    assert_eq!(st[1].data, json!({"available": true, "error": null}));
}

#[tokio::test]
async fn nothing_runs_without_subscribers() {
    let fs = Arc::new(FixedStatfs::machine());
    let (_d, _path, topics, ..) = serve(SystemConfig {
        statfs: fs.clone(),
        ..machine_config()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        (
            topics.stats.current().seq,
            topics.disks.current().seq,
            topics.gpus.current().seq
        ),
        (0, 0, 0)
    );
    assert!(fs.calls().is_empty(), "nenhum statvfs sem assinante");
}

#[tokio::test]
async fn sampling_follows_each_topic() {
    let fs = Arc::new(FixedStatfs::machine());
    let (_d, path, topics, ..) = serve(SystemConfig {
        statfs: fs.clone(),
        ..machine_config()
    })
    .await;
    let c = client(&path).await;
    c.subscribe(&["system.stats"]).await.unwrap();
    wait_seq(&topics.stats, 2).await;
    assert_eq!(
        (topics.disks.current().seq, fs.calls().len()),
        (0, 0),
        "só o grupo assinado roda"
    );
    c.subscribe(&["system.disks"]).await.unwrap();
    wait_seq(&topics.disks, 1).await;
    assert_eq!(
        topics.disks.current().data["disks"][0],
        json!({"name":"nvme0n1","size":2400,"used":540,"root":true})
    );
    c.unsubscribe(&["system.stats", "system.disks"])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (a, b) = (topics.stats.current().seq, topics.disks.current().seq);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        (topics.stats.current().seq, topics.disks.current().seq),
        (a, b),
        "para ao sair"
    );
}

#[tokio::test]
async fn second_client_leaving_keeps_sampling() {
    let (_d, path, topics, ..) = serve(machine_config()).await;
    let (a, b) = (client(&path).await, client(&path).await);
    a.subscribe(&["system.stats"]).await.unwrap();
    b.subscribe(&["system.stats"]).await.unwrap();
    drop(b);
    let n = topics.stats.current().seq;
    wait_seq(&topics.stats, n + 3).await;
    drop(a);
}

#[tokio::test]
async fn configure_validates_and_changes_the_interval() {
    let (_d, path, topics, ..) = serve(machine_config()).await;
    let c = client(&path).await;
    for bad in [
        json!({"interval_ms": 499}),
        json!({"interval_ms": 10001}),
        json!({"interval_ms": "abc"}),
        json!({}),
    ] {
        match c.call("system.configure", Some(bad)).await {
            Err(ClientError::Rpc(e)) => assert_eq!(
                (e.code, e.message.as_str()),
                (
                    -32602,
                    "Parâmetros inválidos: interval_ms precisa ser um inteiro entre 500 e 10000."
                )
            ),
            r => panic!("esperava -32602, veio {r:?}"),
        }
    }
    assert_eq!(
        c.call("system.configure", Some(json!({"interval_ms": 500})))
            .await
            .unwrap(),
        json!({})
    );
    c.subscribe(&["system.stats"]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let n = topics.stats.current().seq;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(
        topics.stats.current().seq - n <= 3,
        "com 500 ms, no máximo 3 amostras em 1,2 s"
    );
}

#[tokio::test]
async fn stuck_disk_does_not_stop_stats() {
    let fs = Arc::new(FixedStatfs::machine().slow("/"));
    let (_d, path, topics, ..) = serve(SystemConfig {
        statfs: fs,
        ..machine_config()
    })
    .await;
    let c = client(&path).await; // vivo até o fim: largar o cliente desfaz a assinatura
    c.subscribe(&["system.stats", "system.disks"])
        .await
        .unwrap();
    wait_seq_within(&topics.stats, 3, Duration::from_millis(1500)).await;
    wait_seq(&topics.disks, 1).await;
    assert_eq!(
        topics.disks.current().data["disks"][0]["size"],
        400,
        "a raiz travada fica fora; sobra o /boot e o /var"
    );
}

#[tokio::test]
async fn unavailable_without_proc_stat() {
    let t = tempfile::tempdir().unwrap();
    let (_d, path, topics, ..) = serve(SystemConfig {
        roots: Roots::at(t.path()),
        ..machine_config()
    })
    .await;
    let st = client(&path)
        .await
        .subscribe(&["system.status", "system.stats"])
        .await
        .unwrap();
    assert_eq!(st[0].data["available"], false);
    assert!(
        st[0].data["error"]
            .as_str()
            .unwrap()
            .starts_with("Não consegui ler ")
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(topics.stats.current().seq, 0, "o laço não roda");
}

#[tokio::test]
async fn panic_marks_the_module_unavailable_and_the_server_goes_on() {
    let tmp = tempfile::tempdir().unwrap();
    // O pânico precisa da NVML chamada: a placa ativa e alguém com /dev/nvidia0 aberto. O procfs é uma
    // cópia da máquina num tempdir com 300/fd/6 → /dev/nvidia0 (link criado no teste).
    let roots = Roots {
        procfs: proc_with_nvidia_user(&tmp),
        sysfs: testdata("cases/nvidia-active/sys"),
        ..Roots::at(&testdata("machine"))
    };
    let (_d, path, topics, ..) = serve(SystemConfig {
        roots,
        nvidia: Box::new(PanicNvidia),
        ..machine_config()
    })
    .await;
    let c = client(&path).await; // vivo até o fim
    c.subscribe(&["system.gpus"]).await.unwrap();
    wait_seq(&topics.status, 1).await;
    assert_eq!(
        topics.status.current().data,
        json!({"available": false, "error": "O módulo de sistema parou por um erro interno."})
    );
    let st = client(&path)
        .await
        .subscribe(&["system.info"])
        .await
        .unwrap();
    assert_eq!(
        st[0].data["hostname"], "maquina",
        "o servidor continua atendendo"
    );
    drop(c);
}

#[tokio::test]
async fn dropping_the_task_stops_sampling() {
    let (_d, path, topics, system, _server) = build(machine_config());
    let task = system.spawn();
    let c = client(&path).await;
    c.subscribe(&["system.stats"]).await.unwrap();
    wait_seq(&topics.stats, 2).await;
    drop(task);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let n = topics.stats.current().seq;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        topics.stats.current().seq,
        n,
        "largar o ModuleTask para o laço"
    );
    assert_eq!(
        topics.status.current().data,
        json!({"available": true, "error": null}),
        "parar não é erro"
    );
}
