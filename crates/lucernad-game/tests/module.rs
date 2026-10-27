//! O módulo `game` num servidor de verdade, com o Hyprland falso, o D-Bus
//! falso, o `/proc` falso e o diário numa pasta temporária. As esperas são
//! curtas (o relógio é o de verdade): junção de 20 ms, tela cheia de 150 ms,
//! saída de 300 ms e nova tentativa de 200 ms.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lucerna_ipc::{Client, ClientError};
use lucerna_service::{ServerBuilder, WorkTracker};
use lucernad_game::journal::{Entry, Journal, Store};
use lucernad_game::testing::{FakeGameBus, FakeProc};
use lucernad_game::{GameConfig, GameHandle, Timing, register};
use lucernad_hyprland::clients::Window;
use lucernad_hyprland::options::OptionValue::{Bool, Int};
use lucernad_hyprland::socket::HyprError;
use lucernad_hyprland::testing::{FakeHypr, default_options};
use lucernad_hyprland::{Hypr, window_prop_lua};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::UnixListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

const ME: u32 = 1000;
const ANIM: &str = "animations:enabled";
const BLUR: &str = "decoration:blur:enabled";
const VRR: &str = "misc:vrr";

fn timing() -> Timing {
    Timing {
        settle: Duration::from_millis(20),
        fullscreen_enter: Duration::from_millis(150),
        auto_exit: Duration::from_millis(300),
        restore_retry: Duration::from_millis(200),
        shutdown: Duration::from_secs(5),
    }
}

struct Rig {
    _dir: TempDir,
    hypr: Arc<FakeHypr>,
    bus: Arc<FakeGameBus>,
    proc: FakeProc,
    store: Store,
    light: watch::Receiver<bool>,
    work: WorkTracker,
    path: PathBuf,
    handle: GameHandle,
    _server: JoinHandle<()>,
}

/// O que mexer no Hyprland falso e no diário antes de o módulo subir.
type BeforeSpawn = Box<dyn FnOnce(&FakeHypr, &Store)>;

struct Opts {
    hypr: bool,
    before_spawn: BeforeSpawn,
}

impl Default for Opts {
    fn default() -> Opts {
        Opts {
            hypr: true,
            before_spawn: Box::new(|_, _| {}),
        }
    }
}

fn rig() -> Rig {
    rig_with(Opts::default())
}

fn rig_with(o: Opts) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let hypr = Arc::new(FakeHypr::new());
    let bus = Arc::new(FakeGameBus::new());
    let proc = FakeProc::new(ME);
    let store = Store::new(dir.path().join("game"));
    (o.before_spawn)(&hypr, &store);
    let (tx, light) = watch::channel(false);
    let work = WorkTracker::new();
    let cfg = GameConfig {
        hypr: o.hypr.then(|| hypr.clone() as Arc<dyn Hypr>),
        bus: bus.clone(),
        proc: proc.root(),
        journal: Some(store.clone()),
        light: Some(tx),
        timing: timing(),
    };
    let path = dir.path().join("lucernad.sock");
    let mut builder = ServerBuilder::new("lucernad", "0.1.0");
    let game = register(&mut builder, cfg, work.clone());
    let server = builder.build();
    let listener = UnixListener::bind(&path).unwrap();
    let (task, handle) = game.spawn();
    let server = tokio::spawn(async move {
        let _task = task;
        let _ = server.serve(listener).await;
    });
    Rig {
        _dir: dir,
        hypr,
        bus,
        proc,
        store,
        light,
        work,
        path,
        handle,
        _server: server,
    }
}

fn win(address: &str, pid: i32, class: &str, fullscreen: u8) -> Window {
    Window {
        address: address.into(),
        pid,
        class: class.into(),
        title: class.into(),
        fullscreen,
        workspace: 1,
        mapped: true,
    }
}

impl Rig {
    async fn client(&self) -> Client {
        Client::connect(&self.path, "test", "lucernad")
            .await
            .unwrap()
    }

    async fn call(&self, c: &Client, method: &str, params: Value) -> Value {
        c.call(method, Some(params)).await.unwrap()
    }

    /// As janelas mudam e o Hyprland avisa.
    fn windows(&self, w: Vec<Window>) {
        self.hypr.set_windows(w, vec![1]);
        self.hypr.emit("openwindow", "x");
    }

    fn a_game(&self, pid: i32) {
        self.proc.process(
            pid,
            1,
            ME,
            "jogo",
            "/g/jogo",
            &["jogo"],
            &[("SteamAppId", "570")],
        );
    }
}

/// O `game.state` até `ok` aceitar, em até 3 s.
async fn state_until(c: &Client, ok: impl Fn(&Value) -> bool) -> Value {
    let mut last = c.subscribe(&["game.state"]).await.unwrap()[0].data.clone();
    if ok(&last) {
        return last;
    }
    let got = tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(s) = c.next_state().await {
            if s.topic != "game.state" {
                continue;
            }
            last = s.data;
            if ok(&last) {
                return last.clone();
            }
        }
        panic!("conexão fechada");
    })
    .await;
    got.unwrap_or_else(|_| panic!("o estado não chegou; o último: {last}"))
}

/// Espera `cond` até 3 s.
async fn until(what: &str, cond: impl Fn() -> bool) {
    for _ in 0..300 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("não aconteceu: {what}");
}

fn active(v: &Value) -> bool {
    v["active"] == json!(true)
}

#[tokio::test]
async fn fullscreen_window_enters_and_leaves() {
    let r = rig();
    let c = r.client().await;
    r.call(&c, "game.configure", json!({})).await;
    r.windows(vec![win("0xa1", 4321, "kitty", 2)]);
    let s = state_until(&c, active).await;
    assert_eq!(s["reasons"], json!(["fullscreen"]));
    assert_eq!(s["manual"], Value::Null);
    assert!(s["since"].as_u64().unwrap() > 0);
    assert_eq!(
        s["windows"],
        json!([{"address": "0xa1", "class": "kitty", "title": "kitty", "reason": "fullscreen"}])
    );
    assert_eq!(
        s["effects"],
        json!({"compositor": "ok", "vrr": "ok", "tearing": "off", "profile": "ok",
               "gamemode": "ok", "light": "ok"})
    );
    assert_eq!(r.hypr.option(ANIM), Some(Bool(false)));
    assert_eq!(r.hypr.option(BLUR), Some(Bool(false)));
    assert_eq!(r.hypr.option(VRR), Some(Int(2)));
    assert!(r.bus.held().is_some());
    assert_eq!(r.bus.registered(), vec![4321]);
    assert!(*r.light.borrow());
    assert_eq!(r.store.load().unwrap().instance, "fake_1");
    // Sai da tela cheia: 300 ms depois, tudo de volta.
    r.windows(vec![win("0xa1", 4321, "kitty", 0)]);
    let s = state_until(&c, |v| !active(v)).await;
    assert_eq!(s["effects"]["compositor"], json!("off"));
    assert_eq!(s["since"], Value::Null);
    assert_eq!(r.hypr.options(), default_options());
    assert_eq!(r.bus.held(), None);
    assert!(r.bus.registered().is_empty());
    assert!(!*r.light.borrow());
    assert_eq!(r.store.load(), None);
}

#[tokio::test]
async fn manual_set_and_toggle() {
    let r = rig();
    let c = r.client().await;
    assert_eq!(r.call(&c, "game.set", json!({"on": true})).await, json!({}));
    let s = state_until(&c, active).await;
    assert_eq!(s["manual"], json!("on"));
    assert_eq!(s["reasons"], json!(["manual"]));
    // Manual sem janela: nada registrado no GameMode, e o item fica "ok".
    assert_eq!(s["effects"]["gamemode"], json!("ok"));
    assert!(r.bus.registered().is_empty());
    assert_eq!(r.hypr.option(ANIM), Some(Bool(false)));
    assert_eq!(
        r.call(&c, "game.toggle", json!({})).await,
        json!({"active": false})
    );
    state_until(&c, |v| !active(v)).await;
    assert_eq!(r.hypr.options(), default_options());
    assert_eq!(
        r.call(&c, "game.toggle", json!({})).await,
        json!({"active": true})
    );
    state_until(&c, active).await;
    r.call(&c, "game.set", json!({"on": false})).await;
    let s = state_until(&c, |v| !active(v)).await;
    assert_eq!(s["manual"], Value::Null);
}

#[tokio::test]
async fn manual_off_with_game_open_latches() {
    let r = rig();
    let c = r.client().await;
    r.call(&c, "game.configure", json!({})).await;
    r.a_game(500);
    r.windows(vec![win("0xg", 500, "dota2", 0)]);
    state_until(&c, |v| active(v) && v["reasons"] == json!(["game"])).await;
    r.call(&c, "game.set", json!({"on": false})).await;
    let s = state_until(&c, |v| !active(v)).await;
    assert_eq!(s["manual"], json!("off"));
    // O jogo continua aberto e mexendo nas janelas: continua desligado.
    r.windows(vec![win("0xg", 500, "dota2", 0)]);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!active(&state_until(&c, |_| true).await));
    assert_eq!(r.hypr.options(), default_options());
}

#[tokio::test]
async fn configure_turning_item_off_undoes_it_now() {
    let r = rig();
    let c = r.client().await;
    r.call(&c, "game.set", json!({"on": true})).await;
    state_until(&c, active).await;
    assert!(r.bus.held().is_some());
    r.call(
        &c,
        "game.configure",
        json!({"compositor": {"effects": false}, "system": {"profile": false, "light": false}}),
    )
    .await;
    let s = state_until(&c, |v| v["effects"]["compositor"] == json!("off")).await;
    assert!(active(&s));
    assert_eq!(s["effects"]["profile"], json!("off"));
    assert_eq!(s["effects"]["light"], json!("off"));
    assert_eq!(s["effects"]["vrr"], json!("ok"));
    assert_eq!(r.hypr.option(ANIM), Some(Bool(true)), "desfeito na hora");
    assert_eq!(r.hypr.option(VRR), Some(Int(2)));
    assert_eq!(r.bus.held(), None);
    assert!(!*r.light.borrow());
    r.call(&c, "game.set", json!({"on": false})).await;
    state_until(&c, |v| !active(v)).await;
    assert_eq!(r.hypr.options(), default_options());
}

#[tokio::test]
async fn nothing_automatic_before_configure() {
    let r = rig();
    let c = r.client().await;
    r.windows(vec![win("0xa1", 4321, "kitty", 2)]);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!active(&state_until(&c, |_| true).await));
    assert!(!r.hypr.requests().iter().any(|q| q.starts_with("eval")));
    r.call(&c, "game.configure", json!({})).await;
    state_until(&c, active).await;
}

#[tokio::test]
async fn never_list_keeps_video_out() {
    let r = rig();
    let c = r.client().await;
    // A lista de fábrica: o mpv em tela cheia não liga nada.
    r.call(&c, "game.configure", json!({})).await;
    r.windows(vec![win("0xv", 77, "mpv", 2)]);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!active(&state_until(&c, |_| true).await));
    // Tirado da lista pelo usuário, o vídeo em tela cheia liga.
    r.call(&c, "game.configure", json!({"never": []})).await;
    state_until(&c, |v| active(v) && v["reasons"] == json!(["fullscreen"])).await;
}

#[tokio::test]
async fn no_hyprland_still_does_manual() {
    let r = rig_with(Opts {
        hypr: false,
        ..Opts::default()
    });
    let c = r.client().await;
    r.call(&c, "game.set", json!({"on": true})).await;
    let s = state_until(&c, active).await;
    assert_eq!(
        s["effects"],
        json!({"compositor": "unavailable", "vrr": "unavailable", "tearing": "off",
               "profile": "ok", "gamemode": "ok", "light": "ok"})
    );
    // O Hyprland do socket fora do ar dá o mesmo.
    let r = rig();
    r.hypr.set_unreachable(true);
    r.bus.absent_profiles();
    r.bus.absent_gamemode();
    let c = r.client().await;
    r.call(&c, "game.set", json!({"on": true})).await;
    let s = state_until(&c, |v| {
        active(v) && v["effects"]["compositor"] == json!("unavailable")
    })
    .await;
    assert_eq!(s["effects"]["profile"], json!("unavailable"));
    assert_eq!(s["effects"]["gamemode"], json!("unavailable"));
}

fn left_journal(h: &FakeHypr, store: &Store) {
    // Um daemon morto no meio do jogo: o Hyprland com os efeitos desligados
    // e o diário no disco.
    h.set_option(ANIM, Bool(false));
    h.set_option(VRR, Int(2));
    store
        .save(&Journal {
            instance: "fake_1".into(),
            entries: vec![
                Entry {
                    option: ANIM.into(),
                    before: Bool(true),
                    applied: Bool(false),
                },
                Entry {
                    option: VRR.into(),
                    before: Int(0),
                    applied: Int(2),
                },
            ],
            tearing_windows: vec![],
        })
        .unwrap();
}

#[tokio::test]
async fn startup_restores_journal_before_anything() {
    let r = rig_with(Opts {
        before_spawn: Box::new(left_journal),
        ..Opts::default()
    });
    until("o diário apagado", || r.store.load().is_none()).await;
    let first = r.hypr.requests().first().cloned().unwrap();
    assert_eq!(first, format!("j/getoption {ANIM}"));
    assert_eq!(r.hypr.options(), default_options());
    // E depois entra de novo, com o "antes" = o restaurado.
    let c = r.client().await;
    r.call(&c, "game.set", json!({"on": true})).await;
    state_until(&c, active).await;
    r.call(&c, "game.set", json!({"on": false})).await;
    state_until(&c, |v| !active(v)).await;
    assert_eq!(r.hypr.options(), default_options());
}

#[tokio::test]
async fn work_guard_held_while_active_or_journal_left() {
    let r = rig();
    let c = r.client().await;
    assert_eq!(r.work.pending(), 0);
    r.call(&c, "game.set", json!({"on": true})).await;
    state_until(&c, active).await;
    assert_eq!(r.work.pending(), 1);
    // A restauração falha: o diário fica, o WorkGuard também, e o erro vai
    // ao effects.compositor.
    r.hypr.fail_on("eval", HyprError::TimedOut);
    r.call(&c, "game.set", json!({"on": false})).await;
    let s = state_until(&c, |v| !active(v)).await;
    assert!(
        s["effects"]["compositor"]
            .as_str()
            .unwrap()
            .starts_with("error: "),
        "{s}"
    );
    assert!(r.store.load().is_some());
    assert_eq!(r.work.pending(), 1);
    // A nova tentativa (200 ms) dá certo e solta tudo.
    until("o diário apagado", || r.store.load().is_none()).await;
    until("o WorkGuard solto", || r.work.pending() == 0).await;
    assert_eq!(r.hypr.options(), default_options());
    state_until(&c, |v| v["effects"]["compositor"] == json!("off")).await;
}

#[tokio::test]
async fn gamemode_follows_trigger_windows() {
    let r = rig();
    let c = r.client().await;
    r.call(&c, "game.configure", json!({"always": ["osu!"]}))
        .await;
    r.a_game(500);
    r.windows(vec![
        win("0xg", 500, "dota2", 0),
        win("0xo", 600, "osu!", 0),
    ]);
    state_until(&c, |v| {
        active(v) && v["windows"].as_array().unwrap().len() == 2
    })
    .await;
    until("os dois registrados", || {
        r.bus.registered() == vec![500, 600]
    })
    .await;
    r.windows(vec![win("0xg", 500, "dota2", 0)]);
    until("o osu! fora", || r.bus.registered() == vec![500]).await;
    assert!(r.bus.calls().contains(&"unregister 600".to_string()));
}

#[tokio::test]
async fn tearing_marks_trigger_windows() {
    let r = rig();
    let c = r.client().await;
    r.call(
        &c,
        "game.configure",
        json!({"compositor": {"tearing": true}}),
    )
    .await;
    r.a_game(500);
    r.windows(vec![win("0xg", 500, "dota2", 0)]);
    let s = state_until(&c, active).await;
    assert_eq!(s["effects"]["tearing"], json!("ok"));
    assert_eq!(r.hypr.option("general:allow_tearing"), Some(Bool(true)));
    assert_eq!(r.hypr.prop("0xg", "immediate").as_deref(), Some("1"));
    r.windows(vec![]);
    state_until(&c, |v| !active(v)).await;
    assert_eq!(r.hypr.prop("0xg", "immediate").as_deref(), Some("0"));
    assert_eq!(r.hypr.options(), default_options());
    assert!(r.hypr.requests().contains(&format!(
        "eval {}",
        window_prop_lua("0xg", "immediate", "0")
    )));
}

#[tokio::test]
async fn reload_mid_game_reapplies() {
    let r = rig();
    let c = r.client().await;
    r.call(&c, "game.set", json!({"on": true})).await;
    state_until(&c, active).await;
    // O usuário salva o arquivo durante o jogo: o reload volta tudo ao
    // arquivo (agora com o desfoque desligado).
    r.hypr.reload(&[
        (BLUR, Bool(false)),
        ("decoration:shadow:enabled", Bool(true)),
        (ANIM, Bool(true)),
        (VRR, Int(0)),
    ]);
    until("reaplicado", || r.hypr.option(ANIM) == Some(Bool(false))).await;
    r.call(&c, "game.set", json!({"on": false})).await;
    state_until(&c, |v| !active(v)).await;
    // O desfoque voltou do reload no valor que o modo jogo pôs: fica o antes
    // do diário (D5, decisão do usuário).
    assert_eq!(r.hypr.option(BLUR), Some(Bool(true)), "do lado seguro");
    assert_eq!(r.hypr.option(ANIM), Some(Bool(true)));
    assert_eq!(r.hypr.option(VRR), Some(Int(0)));
}

#[tokio::test]
async fn shutdown_restores_everything() {
    let r = rig();
    let c = r.client().await;
    r.call(&c, "game.set", json!({"on": true})).await;
    state_until(&c, active).await;
    assert!(r.bus.held().is_some());
    r.handle.shutdown().await;
    assert_eq!(r.hypr.options(), default_options());
    assert_eq!(r.bus.held(), None);
    assert!(!*r.light.borrow());
    assert_eq!(r.store.load(), None);
    assert_eq!(r.work.pending(), 0);
}

#[tokio::test]
async fn bad_params_are_32602() {
    let r = rig();
    let c = r.client().await;
    for (method, params, text) in [
        ("game.set", json!({}), "on precisa ser true ou false"),
        (
            "game.set",
            json!({"on": "sim"}),
            "on precisa ser true ou false",
        ),
        (
            "game.configure",
            json!({"always": [1]}),
            "always e never precisam ser listas de até 100 classes com até 128 caracteres",
        ),
        (
            "game.configure",
            json!({"auto": {"games": 0}}),
            "auto.games precisa ser true ou false",
        ),
    ] {
        match c.call(method, Some(params.clone())).await {
            Err(ClientError::Rpc(e)) => {
                assert_eq!(e.code, -32602, "{method} {params}");
                assert_eq!(e.message, format!("Parâmetros inválidos: {text}."));
            }
            other => panic!("{method} {params}: {other:?}"),
        }
    }
}

#[tokio::test]
async fn shutdown_restores_even_with_a_retry_scheduled() {
    let r = rig();
    let c = r.client().await;
    r.call(&c, "game.set", json!({"on": true})).await;
    state_until(&c, active).await;
    // A restauração falha uma vez: a nova tentativa fica para daqui a 200 ms.
    r.hypr.fail_on("eval", HyprError::TimedOut);
    r.call(&c, "game.set", json!({"on": false})).await;
    assert!(r.store.load().is_some());
    // O serviço para antes da nova tentativa: o shutdown restaura assim mesmo.
    r.handle.shutdown().await;
    assert_eq!(r.hypr.options(), default_options());
    assert_eq!(r.store.load(), None);
    assert_eq!(r.work.pending(), 0);
}

#[tokio::test]
async fn slow_hold_is_still_released() {
    // O serviço de perfis segura o perfil e demora a devolver o cookie
    // (mais que o prazo de uma chamada): o cookie não pode se perder.
    let r = rig();
    r.bus
        .slow_hold(lucernad_game::bus::DBUS_TIMEOUT + Duration::from_millis(300));
    let c = r.client().await;
    r.call(&c, "game.set", json!({"on": true})).await;
    assert!(r.bus.held().is_some());
    r.call(&c, "game.set", json!({"on": false})).await;
    state_until(&c, |v| !active(v)).await;
    assert_eq!(r.bus.held(), None, "a retenção ficou presa");
}

#[tokio::test]
async fn journal_left_at_startup_holds_the_work_guard() {
    // O diário de um daemon que caiu e o Hyprland ainda sem responder ao
    // eval: o daemon não pode sair por ociosidade com o diário no disco.
    let r = rig_with(Opts {
        before_spawn: Box::new(|h, store| {
            left_journal(h, store);
            h.fail_on("eval", HyprError::TimedOut);
        }),
        ..Opts::default()
    });
    let c = r.client().await;
    state_until(&c, |v| {
        v["effects"]["compositor"]
            .as_str()
            .is_some_and(|e| e.starts_with("error: "))
    })
    .await;
    assert!(r.store.load().is_some());
    assert_eq!(r.work.pending(), 1, "o WorkGuard com o diário pendente");
    until("o diário apagado", || r.store.load().is_none()).await;
    until("o WorkGuard solto", || r.work.pending() == 0).await;
    assert_eq!(r.hypr.options(), default_options());
}

#[tokio::test]
async fn window_without_pid_is_not_sent_to_gamemode() {
    let r = rig();
    let c = r.client().await;
    r.call(&c, "game.configure", json!({"always": ["osu!"]}))
        .await;
    r.windows(vec![win("0xo", 0, "osu!", 0), win("0xn", -1, "osu!", 0)]);
    let s = state_until(&c, active).await;
    assert_eq!(s["effects"]["gamemode"], json!("ok"), "{s}");
    assert!(
        !r.bus.calls().iter().any(|c| c.starts_with("register")),
        "{:?}",
        r.bus.calls()
    );
}

#[tokio::test]
async fn clients_failure_keeps_the_manual_off_latch() {
    let r = rig();
    let c = r.client().await;
    r.call(&c, "game.configure", json!({})).await;
    r.a_game(500);
    r.windows(vec![win("0xg", 500, "dota2", 0)]);
    state_until(&c, active).await;
    r.call(&c, "game.set", json!({"on": false})).await;
    state_until(&c, |v| v["manual"] == json!("off")).await;
    // Um soluço do socket (sem resposta no prazo) por mais que a espera da
    // saída: os gatilhos de antes ficam.
    for _ in 0..3 {
        r.hypr.fail_on("j/clients", HyprError::TimedOut);
    }
    for _ in 0..3 {
        r.hypr.emit("openwindow", "x");
        tokio::time::sleep(Duration::from_millis(170)).await;
    }
    r.hypr.emit("openwindow", "x");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let s = state_until(&c, |_| true).await;
    assert!(!active(&s), "religou contra o pedido: {s}");
    assert_eq!(s["manual"], json!("off"));
}

#[tokio::test]
async fn loop_panic_still_undoes_the_effects() {
    let r = rig();
    let c = r.client().await;
    r.call(&c, "game.set", json!({"on": true})).await;
    state_until(&c, active).await;
    assert!(*r.light.borrow());
    // O laço entra em pânico no meio do jogo.
    r.hypr.panic_on("j/clients");
    let _ = c.call("game.configure", Some(json!({}))).await;
    until("o diário apagado", || r.store.load().is_none()).await;
    assert_eq!(r.hypr.options(), default_options());
    assert!(!*r.light.borrow(), "o modo leve desligado");
    let st = c.subscribe(&["game.status"]).await.unwrap();
    assert_eq!(st[0].data["available"], json!(false));
}
