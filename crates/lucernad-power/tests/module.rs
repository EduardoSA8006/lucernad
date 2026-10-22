//! O módulo `power` num servidor de verdade, com os dublês do `fake_config`
//! (árvore falsa, `FakeI2c`, `FakeBus`, `FakeUevents`, `FakeWriter`): a
//! descoberta, o juntar por tela, o plugar pelo uevent, os tópicos e os
//! métodos. Nada aqui abre `/dev/i2c-*`, o D-Bus, o netlink ou o `/sys`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lucerna_ipc::{Client, ClientError};
use lucerna_service::{ServerBuilder, WorkTracker};
use lucernad_power::bus::{BacklightWriter, BoxFuture};
use lucernad_power::bus::{Point, Profiles};
use lucernad_power::ddc::set_request;
use lucernad_power::roots::ROOT_INVALID;
use lucernad_power::testing::{FakeBus, FakeMonitor, Fakes, Tree, edid, fake_config, lg};
use lucernad_power::{
    Discoveries, HISTORY_RESOLUTION, HISTORY_SPAN, PowerConfig, PowerTopics, register,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::UnixListener;
use tokio::task::JoinHandle;
use tokio::time::Instant;

const NVIDIA: &str = "0000:01:00.0";
const LG: &str = "ddc:HDMI-A-1";
const DELL: &str = "ddc:DP-1";
const NO_ANSWER: &str = "O monitor não respondeu ao DDC/CI.";
const NO_ACCESS: &str =
    "Sem acesso ao barramento do monitor: instale a regra udev do lucernad (veja o README).";
const BACKLIGHT_DEVPATH: &str =
    "/devices/pci0000:00/0000:00:02.0/drm/card2/card2-eDP-1/intel_backlight";
/// O prazo folgado das esperas (≥ 3× o que o módulo leva).
const LONG: Duration = Duration::from_secs(3);

/// O módulo num servidor, num socket em `dir`, com o laço rodando; o
/// `ModuleTask` fica guardado na tarefa do servidor, que vive até o fim do
/// teste.
fn serve(
    dir: &Path,
    cfg: PowerConfig,
    work: WorkTracker,
) -> (PathBuf, PowerTopics, Discoveries, JoinHandle<()>) {
    let path = dir.join("lucernad.sock");
    let mut builder = ServerBuilder::new("lucernad", "0.1.0");
    let power = register(&mut builder, cfg, work);
    let topics = power.topics.clone();
    let discoveries = power.discoveries();
    let server = builder.build();
    let listener = UnixListener::bind(&path).unwrap();
    let task = power.spawn();
    let handle = tokio::spawn(async move {
        let _task = task;
        let _ = server.serve(listener).await;
    });
    (path, topics, discoveries, handle)
}

/// Os dublês, o diretório, o servidor e o contador de trabalho de um teste.
struct Rig {
    _dir: TempDir,
    f: Fakes,
    work: WorkTracker,
    path: PathBuf,
    t: PowerTopics,
    discoveries: Discoveries,
    _server: JoinHandle<()>,
}

fn rig() -> Rig {
    rig_with(|_| {})
}

/// O [`rig`] com a configuração mexida por `tweak`.
fn rig_with(tweak: impl FnOnce(&mut PowerConfig)) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let (mut cfg, f) = fake_config(dir.path());
    tweak(&mut cfg);
    let work = WorkTracker::new();
    let (path, t, discoveries, server) = serve(dir.path(), cfg, work.clone());
    Rig {
        _dir: dir,
        f,
        work,
        path,
        t,
        discoveries,
        _server: server,
    }
}

/// Um monitor lento: o `op_timeout` maior que o tempo de uma operação.
fn slow_rig() -> Rig {
    rig_with(|cfg| cfg.op_timeout = Duration::from_millis(600))
}

async fn client(path: &Path) -> Client {
    Client::connect(path, "test", "lucernad").await.unwrap()
}

async fn call(c: &Client, method: &str, params: Value) -> Result<Value, ClientError> {
    c.call(method, Some(params)).await
}

async fn set(c: &Client, id: &str, percent: i64) {
    let r = call(c, "power.set", json!({"id": id, "percent": percent})).await;
    assert_eq!(r.unwrap(), json!({}), "power.set {id} {percent}");
}

async fn change(c: &Client, id: &str, delta: i64) {
    let r = call(c, "power.change", json!({"id": id, "delta": delta})).await;
    assert_eq!(r.unwrap(), json!({}), "power.change {id} {delta}");
}

/// O código e o texto de um erro do método.
fn rpc_error(result: Result<Value, ClientError>) -> (i32, String) {
    match result {
        Err(ClientError::Rpc(e)) => (e.code, e.message),
        other => panic!("esperava um erro do método: {other:?}"),
    }
}

fn screens(t: &PowerTopics) -> Vec<Value> {
    t.screens.current().data["screens"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn screen(t: &PowerTopics, id: &str) -> Option<Value> {
    screens(t).into_iter().find(|s| s["id"] == id)
}

/// O `percent` da tela `id` (`None` sem a tela ou sem valor).
fn pct(t: &PowerTopics, id: &str) -> Option<i64> {
    screen(t, id).and_then(|s| s["percent"].as_i64())
}

fn ids(t: &PowerTopics) -> Vec<String> {
    screens(t)
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect()
}

/// Espera `cond` valer até `within`; senão, pânico com `what`.
async fn wait_until(within: Duration, what: &str, mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + within;
    while !cond() {
        assert!(Instant::now() < end, "{what}: não aconteceu em {within:?}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

async fn wait_pct(t: &PowerTopics, id: &str, p: i64) {
    wait_until(LONG, &format!("{id} em {p}%"), || pct(t, id) == Some(p)).await;
}

/// Um cliente assinando o `power.screens`, com as duas telas descobertas e
/// o uevent sendo escutado.
async fn watching(r: &Rig) -> Client {
    let c = client(&r.path).await;
    c.subscribe(&["power.screens"]).await.unwrap();
    wait_pct(&r.t, "backlight", 50).await;
    wait_pct(&r.t, LG, 70).await;
    wait_until(LONG, "o uevent escutado", || r.f.uevents.watching() == 1).await;
    c
}

fn opened(f: &Fakes) -> Vec<u32> {
    let mut list = f.i2c.opened();
    list.sort_unstable();
    list.dedup();
    list
}

/// Os Set VCP gravados no barramento `bus`.
fn set_writes(f: &Fakes, bus: u32) -> Vec<Vec<u8>> {
    f.i2c
        .writes(bus)
        .into_iter()
        .filter(|(addr, data)| *addr == 0x37 && data.get(2) == Some(&0x03))
        .map(|(_, data)| data)
        .collect()
}

/// Um segundo monitor (um Dell no `card1-DP-1`, no barramento 0).
fn plug_dell(r: &Rig) {
    let dell = edid("DELL U2720Q", 2);
    r.f.i2c.clone().monitor(
        0,
        FakeMonitor {
            edid: dell.clone(),
            current: 50,
            max: 100,
            off_by: 0,
        },
    );
    r.f.tree
        .connector("card1", "DP-1", "connected", &dell, None);
}

#[tokio::test]
async fn handshake_lists_topics_and_methods() {
    let r = rig();
    let c = client(&r.path).await;
    assert_eq!(
        c.hello().topics,
        [
            "power.screens",
            "power.history",
            "power.profiles",
            "power.status"
        ]
    );
    assert!(c.hello().streams.is_empty());
    assert_eq!(
        c.hello().methods,
        ["power.set", "power.change", "power.list"]
    );
    assert_eq!(r.t.screens.current().data, Value::Null);
    assert_eq!(r.t.history.current().data, Value::Null);
    assert_eq!(r.t.profiles.current().data, Value::Null);
    assert_eq!(
        r.t.status.current().data,
        json!({"available": true, "error": null})
    );
    assert_eq!((HISTORY_SPAN, HISTORY_RESOLUTION), (3600, 120));
}

#[tokio::test]
async fn params_are_validated() {
    let r = rig();
    let c = client(&r.path).await;
    let long = "x".repeat(65);
    let id_text = "id precisa ser o texto de uma tela, com até 64 caracteres";
    let percent_text = "percent precisa ser um inteiro entre 1 e 100";
    let delta_text = "delta precisa ser um inteiro entre -100 e 100";
    let cases = [
        ("power.set", json!({"id": 1, "percent": 50}), id_text),
        ("power.set", json!({"id": long, "percent": 50}), id_text),
        ("power.set", json!({"percent": 50}), id_text),
        (
            "power.set",
            json!({"id": "backlight", "percent": 0}),
            percent_text,
        ),
        (
            "power.set",
            json!({"id": "backlight", "percent": 101}),
            percent_text,
        ),
        (
            "power.set",
            json!({"id": "backlight", "percent": 50.5}),
            percent_text,
        ),
        (
            "power.change",
            json!({"id": "backlight", "delta": 101}),
            delta_text,
        ),
        (
            "power.change",
            json!({"id": "backlight", "delta": -101}),
            delta_text,
        ),
        (
            "power.change",
            json!({"id": "backlight", "delta": "5"}),
            delta_text,
        ),
        ("power.change", json!({"id": 7, "delta": 5}), id_text),
    ];
    for (method, params, text) in cases {
        assert_eq!(
            rpc_error(call(&c, method, params.clone()).await),
            (-32602, format!("Parâmetros inválidos: {text}.")),
            "{method} {params}"
        );
    }
    for (method, params) in [
        ("power.set", json!({"id": "ddc:nada", "percent": 50})),
        ("power.change", json!({"id": "ddc:nada", "delta": 5})),
    ] {
        assert_eq!(
            rpc_error(call(&c, method, params).await),
            (1001, "Essa tela não existe.".to_string())
        );
    }
    assert!(r.f.writer.writes().is_empty());
}

#[tokio::test]
async fn screens_are_discovered() {
    let r = rig();
    let _c = watching(&r).await;
    assert_eq!(
        r.t.screens.current().data,
        json!({"screens": [
            {"id": "backlight", "kind": "backlight", "output": "eDP-1", "name": "Tela integrada",
             "value": 9600, "max": 19200, "percent": 50},
            {"id": LG, "kind": "ddc", "output": "HDMI-A-1", "name": "LG ULTRAWIDE",
             "value": 70, "max": 100, "percent": 70},
        ]})
    );
    // O EDID nos dois barramentos da NVIDIA; nenhum outro aberto.
    assert_eq!(opened(&r.f), [0, 1]);
}

#[tokio::test]
async fn set_goes_through_the_writer_and_rereads() {
    let r = rig();
    let c = watching(&r).await;
    set(&c, "backlight", 30).await;
    wait_pct(&r.t, "backlight", 30).await;
    assert_eq!(r.f.writer.writes(), [("intel_backlight".to_string(), 5760)]);
    assert_eq!(r.f.tree.value("intel_backlight"), 5760);
    assert_eq!(screen(&r.t, "backlight").unwrap()["value"], 5760);
}

#[tokio::test]
async fn ddc_set_rereads_the_real_value() {
    let r = rig();
    r.f.i2c
        .clone()
        .monitor(1, FakeMonitor { off_by: -1, ..lg() });
    let c = watching(&r).await;
    set(&c, LG, 40).await;
    wait_until(LONG, "o LG relido em 39", || {
        screen(&r.t, LG).is_some_and(|s| s["value"] == 39)
    })
    .await;
    assert_eq!(pct(&r.t, LG), Some(39));
    assert_eq!(set_writes(&r.f, 1), [set_request(0x10, 40)]);
}

#[tokio::test]
async fn burst_of_changes_coalesces_on_a_slow_monitor() {
    let r = slow_rig();
    let c = watching(&r).await;
    r.f.i2c.slow(1, Duration::from_millis(150));

    // Guarda cada retrato do `power.screens` publicado durante a rajada.
    let seen = Arc::new(std::sync::Mutex::new(Vec::<(u64, Option<i64>)>::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let recorder = {
        let (topic, seen, stop) = (r.t.screens.clone(), seen.clone(), stop.clone());
        tokio::spawn(async move {
            let mut last = topic.current().seq;
            while !stop.load(Ordering::SeqCst) {
                let state = topic.current();
                if state.seq != last {
                    let lg = state.data["screens"]
                        .as_array()
                        .and_then(|list| list.iter().find(|s| s["id"] == LG))
                        .and_then(|s| s["percent"].as_i64());
                    seen.lock().unwrap().push((state.seq, lg));
                    last = state.seq;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
    };
    let start = r.t.screens.current().seq;

    for _ in 0..20 {
        change(&c, LG, 5).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    wait_until(Duration::from_secs(5), "o LG em 100%", || {
        pct(&r.t, LG) == Some(100)
    })
    .await;
    stop.store(true, Ordering::SeqCst);
    recorder.await.unwrap();

    assert!(set_writes(&r.f, 1).len() <= 3, "{:?}", set_writes(&r.f, 1));
    assert_eq!(r.f.i2c.current(1), 100);
    let seen = seen.lock().unwrap().clone();
    // Nenhum retrato pulado (então nenhum valor do meio escapou da conta).
    let seqs: Vec<u64> = seen.iter().map(|(seq, _)| *seq).collect();
    let expected: Vec<u64> = (start + 1..=start + seqs.len() as u64).collect();
    assert_eq!(seqs, expected, "retratos pulados: {seen:?}");
    assert!(
        seen.iter()
            .all(|(_, p)| !p.is_some_and(|p| p > 70 && p < 100)),
        "valor intermediário publicado: {seen:?}"
    );
    wait_until(LONG, "o trabalho solto", || r.work.pending() == 0).await;
}

#[tokio::test]
async fn dead_monitor_keeps_its_row_and_the_others_work() {
    let r = rig();
    plug_dell(&r);
    let c = watching(&r).await;
    wait_pct(&r.t, DELL, 50).await;

    r.f.i2c.hang(0);
    set(&c, DELL, 30).await;
    wait_until(LONG, "o Dell com o erro", || {
        screen(&r.t, DELL).is_some_and(|s| s["error"] == NO_ANSWER)
    })
    .await;
    assert_eq!(ids(&r.t), ["backlight", DELL, LG]);
    assert_eq!(pct(&r.t, DELL), Some(50), "fica o último valor conhecido");

    let start = Instant::now();
    set(&c, "backlight", 20).await;
    set(&c, LG, 50).await;
    wait_pct(&r.t, "backlight", 20).await;
    wait_pct(&r.t, LG, 50).await;
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(pct(&r.t, "backlight"), Some(20));
    assert!(screen(&r.t, LG).unwrap().get("error").is_none());
    // O trabalho não fica preso na thread travada.
    wait_until(LONG, "o trabalho solto", || r.work.pending() == 0).await;
    r.f.i2c.release(0);
}

#[tokio::test]
async fn no_access_is_a_row_error_and_backlight_still_works() {
    let r = rig();
    r.f.i2c.clone().denied(1);
    let c = client(&r.path).await;
    c.subscribe(&["power.screens"]).await.unwrap();
    wait_until(LONG, "o LG com o erro de acesso", || {
        screen(&r.t, LG).is_some_and(|s| s["error"] == NO_ACCESS)
    })
    .await;
    assert_eq!(
        screen(&r.t, LG).unwrap(),
        json!({"id": LG, "kind": "ddc", "output": "HDMI-A-1", "name": "LG ULTRAWIDE",
               "value": null, "max": null, "percent": null, "error": NO_ACCESS})
    );
    set(&c, "backlight", 40).await;
    wait_pct(&r.t, "backlight", 40).await;
}

#[tokio::test]
async fn opener_never_sees_a_non_gpu_bus() {
    let r = rig();
    let tree = &r.f.tree;
    tree.device("0000:00:1f.4", Some("0x0c0500"));
    tree.bus(16, "0000:00:1f.4");
    tree.device("0000:00:15.0", Some("0x0c8000"));
    tree.device("0000:00:15.0/i2c_designware.0", None);
    tree.bus(2, "0000:00:15.0/i2c_designware.0");
    tree.connector("card1", "DP-1", "connected", &edid("Fantasma", 3), None);

    let c = client(&r.path).await;
    c.subscribe(&["power.screens"]).await.unwrap();
    wait_pct(&r.t, LG, 70).await;
    assert_eq!(
        screen(&r.t, DELL).unwrap(),
        json!({"id": DELL, "kind": "ddc", "output": "DP-1", "name": "Fantasma",
               "value": null, "max": null, "percent": null,
               "error": "Não achei o barramento deste monitor."})
    );
    assert_eq!(opened(&r.f), [0, 1]);
}

#[tokio::test]
async fn external_change_reaches_the_topic() {
    let r = rig();
    let _c = watching(&r).await;
    r.f.tree.set_value("intel_backlight", 4800);
    r.f.uevents.send("change", "backlight", BACKLIGHT_DEVPATH);
    wait_pct(&r.t, "backlight", 25).await;

    // Outro dispositivo de backlight, ou um `drm`: nada muda.
    r.f.tree.set_value("intel_backlight", 1920);
    r.f.uevents.send(
        "change",
        "backlight",
        "/devices/pci0000:00/0000:00:02.0/backlight/acpi_video0",
    );
    r.f.uevents.send(
        "change",
        "drm",
        "/devices/pci0000:00/0000:00:02.0/drm/card2",
    );
    // Mais de 3× o `hotplug_delay` (100 ms): a nova descoberta já rodou.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(pct(&r.t, "backlight"), Some(25));
}

#[tokio::test]
async fn lost_events_reread_and_rediscover() {
    let r = rig();
    let _c = watching(&r).await;
    r.f.tree.set_value("intel_backlight", 1920);
    r.f.uevents.lose();
    wait_pct(&r.t, "backlight", 10).await;
    assert_eq!(
        r.t.status.current().data,
        json!({"available": true, "error": null})
    );
}

#[tokio::test]
async fn hotplug_burst_rediscovers_once() {
    let r = rig();
    let _c = watching(&r).await;
    let (reads0, reads1, opens) = (r.f.i2c.reads(0), r.f.i2c.reads(1), r.f.i2c.opened());
    let discoveries = r.discoveries.get();
    let drm = "/devices/pci0000:00/0000:01:00.0/drm/card1";
    for _ in 0..10 {
        r.f.uevents.send("change", "drm", drm);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(r.f.i2c.reads(0), reads0, "nenhuma leitura de i2c");
    assert_eq!(r.f.i2c.reads(1), reads1, "nenhuma leitura de i2c");
    assert_eq!(r.f.i2c.opened(), opens);
    assert_eq!(
        r.discoveries.get(),
        discoveries + 1,
        "a rajada vira uma descoberta"
    );

    // Um conector novo: só o barramento 0 (o 1 já é do LG) é lido.
    plug_dell(&r);
    r.f.uevents.send("change", "drm", drm);
    wait_pct(&r.t, DELL, 50).await;
    assert_eq!(r.f.i2c.reads(1), reads1);
    assert!(r.f.i2c.reads(0) > reads0);
    assert_eq!(ids(&r.t), ["backlight", DELL, LG]);

    // Tirado, some.
    r.f.tree
        .connector("card1", "DP-1", "disconnected", &[], None);
    r.f.uevents.send("change", "drm", drm);
    wait_until(LONG, "o Dell fora da lista", || {
        screen(&r.t, DELL).is_none()
    })
    .await;
    assert_eq!(ids(&r.t), ["backlight", LG]);
}

#[tokio::test]
async fn standby_monitor_recovers_on_the_next_set() {
    let r = rig();
    // A descoberta só ouve "ocupado" (a primeira tentativa e as três novas).
    r.f.i2c.busy_times(1, 4);
    let c = client(&r.path).await;
    c.subscribe(&["power.screens"]).await.unwrap();
    wait_until(LONG, "o LG sem resposta", || {
        screen(&r.t, LG).is_some_and(|s| s["error"] == NO_ANSWER)
    })
    .await;
    assert_eq!(screen(&r.t, LG).unwrap()["percent"], Value::Null);

    let before = r.f.i2c.writes(1).len();
    set(&c, LG, 50).await;
    wait_pct(&r.t, LG, 50).await;
    assert!(screen(&r.t, LG).unwrap().get("error").is_none());
    // Um Get VCP antes do Set.
    let after: Vec<u8> = r.f.i2c.writes(1)[before..]
        .iter()
        .filter(|(addr, _)| *addr == 0x37)
        .map(|(_, data)| data[2])
        .collect();
    assert_eq!(after.first(), Some(&0x01), "{after:?}");
    assert!(after.contains(&0x03), "{after:?}");
}

#[tokio::test]
async fn stale_ddc_value_is_reread_before_change() {
    let r = rig();
    let c = watching(&r).await;
    // O botão do monitor; o `stale` do `fake_config` é de 100 ms.
    r.f.i2c.set_current(1, 30);
    tokio::time::sleep(Duration::from_millis(150)).await;
    change(&c, LG, 5).await;
    wait_pct(&r.t, LG, 35).await;
    assert_eq!(set_writes(&r.f, 1), [set_request(0x10, 35)]);
}

#[tokio::test]
async fn uevent_only_with_subscriber() {
    let r = rig();
    let c = client(&r.path).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(r.f.uevents.watching(), 0);
    c.subscribe(&["power.screens"]).await.unwrap();
    wait_until(LONG, "o uevent escutado", || r.f.uevents.watching() == 1).await;
    c.unsubscribe(&["power.screens"]).await.unwrap();
    wait_until(Duration::from_secs(1), "o uevent largado", || {
        r.f.uevents.watching() == 0
    })
    .await;
}

#[tokio::test]
async fn methods_work_without_subscriber() {
    let r = rig();
    let c = client(&r.path).await;
    let list = call(&c, "power.list", json!({})).await.unwrap();
    let listed: Vec<&str> = list["screens"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(listed, ["backlight", LG]);
    assert_eq!(list["screens"][1]["percent"], 70);
    set(&c, "backlight", 60).await;
    wait_until(LONG, "a escrita da integrada", || {
        r.f.writer.writes() == [("intel_backlight".to_string(), 11520)]
    })
    .await;
    assert_eq!(r.f.uevents.watching(), 0);
}

#[tokio::test]
async fn work_is_held_while_writing() {
    let r = slow_rig();
    let c = client(&r.path).await;
    // A descoberta sem o monitor lento.
    call(&c, "power.list", json!({})).await.unwrap();
    r.f.i2c.slow(1, Duration::from_millis(150));
    set(&c, LG, 10).await;
    assert_eq!(r.work.pending(), 1);
    wait_pct(&r.t, LG, 10).await;
    wait_until(LONG, "o trabalho solto", || r.work.pending() == 0).await;
}

#[tokio::test]
async fn writer_failure_is_a_row_error() {
    let r = rig();
    let c = watching(&r).await;
    r.f.writer.fails("Access denied");
    set(&c, "backlight", 30).await;
    wait_until(LONG, "a integrada com o erro", || {
        screen(&r.t, "backlight").is_some_and(|s| {
            s["error"] == "Não consegui ajustar o brilho pelo logind: Access denied."
        })
    })
    .await;
    assert_eq!(pct(&r.t, "backlight"), Some(50));
    wait_until(LONG, "o trabalho solto", || r.work.pending() == 0).await;
}

#[tokio::test]
async fn history_only_with_subscriber() {
    let r = rig();
    r.f.bus.clone().with_history(vec![
        Point {
            time: 100,
            rate: 4.25,
            state: 1,
        },
        Point {
            time: 220,
            rate: 0.0,
            state: 4,
        },
    ]);
    let c = client(&r.path).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(r.f.bus.history_calls(), 0);

    c.subscribe(&["power.history"]).await.unwrap();
    let points = json!({"points": [
        {"time": 100, "rate": 4.25, "state": 1},
        {"time": 220, "rate": 0.0, "state": 4},
    ]});
    wait_until(LONG, "o histórico", || {
        r.t.history.current().data == points
    })
    .await;
    wait_until(LONG, "a segunda leitura", || r.f.bus.history_calls() >= 2).await;

    r.f.bus.history_fails("sem UPower");
    let failed =
        json!({"points": [], "error": "Não consegui ler o histórico do UPower: sem UPower."});
    wait_until(LONG, "o erro do histórico", || {
        r.t.history.current().data == failed
    })
    .await;

    c.unsubscribe(&["power.history"]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let calls = r.f.bus.history_calls();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(r.f.bus.history_calls(), calls, "sem assinante, sem leitura");
}

#[tokio::test]
async fn history_error_is_clipped() {
    let r = rig();
    r.f.bus.history_fails(&"x".repeat(5000));
    let c = client(&r.path).await;
    c.subscribe(&["power.history"]).await.unwrap();
    wait_until(LONG, "o erro do histórico", || {
        !r.t.history.current().data["error"].is_null()
    })
    .await;
    let data = r.t.history.current().data;
    let error = data["error"].as_str().unwrap();
    let fixed = "Não consegui ler o histórico do UPower: .".chars().count();
    assert!(error.chars().count() <= 300 + fixed, "{}", error.len());
    assert!(error.starts_with("Não consegui ler o histórico do UPower: xxx"));
    assert!(error.ends_with("….") || error.ends_with('…'), "{error}");
    assert_eq!(data["points"], json!([]));
}

#[tokio::test]
async fn profiles_follow_the_bus() {
    let r = rig();
    let c = client(&r.path).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(r.f.bus.watching(), 0);
    c.subscribe(&["power.profiles"]).await.unwrap();
    wait_until(LONG, "os perfis", || {
        r.t.profiles.current().data == json!({"available": true, "performance": true})
    })
    .await;
    r.f.bus.set_profiles(Profiles {
        available: false,
        performance: false,
    });
    wait_until(LONG, "os perfis mudados", || {
        r.t.profiles.current().data == json!({"available": false, "performance": false})
    })
    .await;
    c.unsubscribe(&["power.profiles"]).await.unwrap();
    wait_until(Duration::from_secs(1), "o receptor largado", || {
        r.f.bus.watching() == 0
    })
    .await;
}

#[tokio::test]
async fn bad_root_makes_the_module_unavailable() {
    let r = rig_with(|cfg| cfg.root_error = Some(ROOT_INVALID.to_string()));
    let c = client(&r.path).await;
    assert_eq!(
        r.t.status.current().data,
        json!({"available": false, "error": ROOT_INVALID})
    );
    let unavailable = format!(
        "O módulo de energia está indisponível: {}.",
        ROOT_INVALID.trim_end_matches('.')
    );
    assert_eq!(
        rpc_error(call(&c, "power.list", json!({})).await),
        (1002, unavailable.clone())
    );
    assert_eq!(
        rpc_error(call(&c, "power.set", json!({"id": "backlight", "percent": 50})).await),
        (1002, unavailable)
    );
    c.subscribe(&["power.screens", "power.history", "power.profiles"])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(r.t.screens.current().data, Value::Null);
    assert!(r.f.i2c.opened().is_empty());
    assert!(r.f.writer.writes().is_empty());
    assert_eq!(r.f.uevents.watching(), 0);
    assert_eq!(r.f.bus.history_calls(), 0);
}

#[tokio::test]
async fn panic_marks_the_module_unavailable() {
    let r = rig_with(|cfg| cfg.bus = Arc::new(FakeBus::panicking()));
    let c = client(&r.path).await;
    c.subscribe(&["power.history"]).await.unwrap();
    let panicked = json!({
        "available": false,
        "error": "O módulo de energia parou por um erro interno.",
    });
    wait_until(LONG, "o erro interno", || {
        r.t.status.current().data == panicked
    })
    .await;
    let again = client(&r.path).await;
    assert_eq!(again.hello().app, "lucernad");
    assert_eq!(
        rpc_error(call(&again, "power.list", json!({})).await),
        (
            1002,
            "O módulo de energia está indisponível: O módulo de energia parou por um erro interno."
                .to_string()
        )
    );
}

#[tokio::test]
async fn missing_i2c_dev_is_a_row_error() {
    let dir = tempfile::tempdir().unwrap();
    let (mut cfg, f) = fake_config(dir.path());
    // Uma árvore sem o `class/i2c-dev` (o módulo do kernel fora).
    let bare = dir.path().join("sem-i2c-dev");
    let tree = Tree::new(&bare);
    tree.gpu(NVIDIA, "0x030000", "card1");
    tree.connector(
        "card1",
        "HDMI-A-1",
        "connected",
        &edid("LG ULTRAWIDE", 1),
        None,
    );
    cfg.roots = tree.roots();
    let work = WorkTracker::new();
    let (path, t, _discoveries, _server) = serve(dir.path(), cfg, work);
    let c = client(&path).await;
    let list = call(&c, "power.list", json!({})).await.unwrap();
    assert_eq!(
        list,
        json!({"screens": [
            {"id": LG, "kind": "ddc", "output": "HDMI-A-1", "name": "LG ULTRAWIDE",
             "value": null, "max": null, "percent": null,
             "error": "Sem o módulo i2c-dev: os monitores externos precisam dele (veja o README)."},
        ]})
    );
    assert_eq!(t.screens.current().data, list);
    assert!(f.i2c.opened().is_empty());
}

#[tokio::test]
async fn panel_is_reread_without_subscriber() {
    let r = rig();
    let c = client(&r.path).await;
    call(&c, "power.list", json!({})).await.unwrap();
    // Sem assinante não há uevent: a mudança de fora só se vê relendo.
    r.f.tree.set_value("intel_backlight", 4800);
    c.subscribe(&["power.screens"]).await.unwrap();
    wait_pct(&r.t, "backlight", 25).await;
    c.unsubscribe(&["power.screens"]).await.unwrap();
    wait_until(LONG, "o uevent largado", || r.f.uevents.watching() == 0).await;

    // Um ajuste sem assinante soma sobre o valor de agora.
    r.f.tree.set_value("intel_backlight", 1920);
    change(&c, "backlight", 5).await;
    wait_until(LONG, "a escrita de 15%", || {
        r.f.writer.writes() == [("intel_backlight".to_string(), 2880)]
    })
    .await;
}

#[tokio::test]
async fn error_rows_retry_on_explicit_requests() {
    let r = rig();
    r.f.i2c.clone().denied(1);
    let c = client(&r.path).await;
    let list = call(&c, "power.list", json!({})).await.unwrap();
    assert_eq!(list["screens"][1]["error"], NO_ACCESS);
    // Ainda sem acesso: o ajuste diz o erro da linha.
    assert_eq!(
        rpc_error(call(&c, "power.set", json!({"id": LG, "percent": 50})).await),
        (1003, NO_ACCESS.to_string())
    );
    // A regra instalada: o próximo ajuste acha o monitor.
    r.f.i2c.allow(1);
    set(&c, LG, 50).await;
    wait_pct(&r.t, LG, 50).await;
    assert!(screen(&r.t, LG).unwrap().get("error").is_none());
}

/// Um defeito posto nos dublês antes da descoberta.
type Breaker = fn(&Fakes);

#[tokio::test]
async fn ddc_errors_reach_the_row() {
    let cases: [(Breaker, &str); 3] = [
        (
            |f| drop(f.i2c.clone().missing(1)),
            "Sem o /dev/i2c-1: carregue o módulo i2c-dev (veja o README).",
        ),
        (
            |f| f.i2c.corrupt_times(1, 4),
            "O monitor respondeu fora do padrão DDC/CI.",
        ),
        (
            |f| drop(f.i2c.clone().unsupported(1)),
            "O monitor não deixa ajustar o brilho pelo DDC/CI.",
        ),
    ];
    for (break_it, error) in cases {
        let r = rig();
        break_it(&r.f);
        let c = client(&r.path).await;
        let list = call(&c, "power.list", json!({})).await.unwrap();
        let lg = &list["screens"][1];
        assert_eq!(lg["error"], error);
        assert_eq!(lg["percent"], Value::Null, "{error}");
    }
}

#[tokio::test]
async fn unreadable_panel_is_a_row_error() {
    let r = rig();
    let brightness =
        r.f.tree
            .roots()
            .sysfs
            .join("class/backlight/intel_backlight/brightness");
    std::fs::write(&brightness, "lixo\n").unwrap();
    let c = client(&r.path).await;
    let list = call(&c, "power.list", json!({})).await.unwrap();
    let panel = &list["screens"][0];
    let error = panel["error"].as_str().unwrap();
    assert!(
        error.starts_with("Não consegui ler o brilho da tela integrada: "),
        "{error}"
    );
    assert!(error.ends_with('.'));
    assert_eq!(panel["percent"], Value::Null);
}

/// Um writer que entra em pânico.
struct PanicWriter;

impl BacklightWriter for PanicWriter {
    fn write<'a>(&'a self, _device: &'a str, _value: u32) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { panic!("PanicWriter: de propósito") })
    }
}

#[tokio::test]
async fn writer_panic_is_a_row_error() {
    let r = rig_with(|cfg| cfg.writer = Arc::new(PanicWriter));
    let c = watching(&r).await;
    set(&c, "backlight", 30).await;
    wait_until(LONG, "a integrada com o erro", || {
        screen(&r.t, "backlight").is_some_and(|s| {
            s["error"] == "Não consegui ajustar o brilho pelo logind: erro interno."
        })
    })
    .await;
    assert_eq!(pct(&r.t, "backlight"), Some(50));
    wait_until(LONG, "o trabalho solto", || r.work.pending() == 0).await;
    // O módulo segue: o status não caiu.
    assert_eq!(
        r.t.status.current().data,
        json!({"available": true, "error": null})
    );
}

#[tokio::test]
async fn hotplug_does_not_wait_for_a_stuck_new_monitor() {
    // Um prazo longo: se o hotplug esperasse a primeira leitura, o laço
    // ficaria parado nele.
    let r = rig_with(|cfg| cfg.op_timeout = Duration::from_secs(3));
    let c = watching(&r).await;
    let dell = edid("DELL U2720Q", 2);
    r.f.i2c.clone().monitor(
        20,
        FakeMonitor {
            edid: dell.clone(),
            current: 50,
            max: 100,
            off_by: 0,
        },
    );
    r.f.i2c.hang(20);
    // Pelo link `ddc` (o AUX do conector): nenhuma leitura de EDID.
    r.f.tree
        .connector("card1", "DP-1", "connected", &dell, Some(20));
    r.f.uevents.send(
        "change",
        "drm",
        "/devices/pci0000:00/0000:01:00.0/drm/card1",
    );
    wait_until(LONG, "o Dell na lista", || screen(&r.t, DELL).is_some()).await;

    let start = Instant::now();
    set(&c, "backlight", 20).await;
    wait_pct(&r.t, "backlight", 20).await;
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(pct(&r.t, DELL), None);
    r.f.i2c.release(20);
    wait_pct(&r.t, DELL, 50).await;
}
