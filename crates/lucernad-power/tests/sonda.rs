//! A sonda do D-Bus de verdade (o `zbus` falando com um `dbus-daemon`), com
//! um logind, um UPower e um serviço de perfis falsos registrados por este
//! processo. Rode pelo `tests/isolado.sh`, que sobe um D-Bus do sistema de
//! mentira num container: fora dele, a trava `isolated()` recusa, e o
//! logind, o UPower e os perfis da máquina nunca são tocados.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lucernad_power::bus::{
    BacklightWriter, LogindWriter, NO_SESSION, Point, PowerBus, Profiles, ZbusPower,
};
use lucernad_power::testing::isolated;
use tokio::sync::mpsc::UnboundedReceiver;
use zbus::connection::Builder;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const USER: &str = "/org/freedesktop/login1/user/self";
const SESSION: &str = "/org/freedesktop/login1/session/_2";
const UPOWER: &str = "/org/freedesktop/UPower";
const AC: &str = "/org/freedesktop/UPower/devices/line_power_AC";
const BAT: &str = "/org/freedesktop/UPower/devices/battery_BAT0";
const UPOWER_PROFILES: &str = "org.freedesktop.UPower.PowerProfiles";
const UPOWER_PROFILES_PATH: &str = "/org/freedesktop/UPower/PowerProfiles";
const HADESS: &str = "net.hadess.PowerProfiles";
const HADESS_PATH: &str = "/net/hadess/PowerProfiles";

/// A janela de espera (folgada: o prazo do D-Bus é 2 s).
const WAIT: Duration = Duration::from_secs(6);

fn path(p: &str) -> OwnedObjectPath {
    OwnedObjectPath::try_from(p).unwrap()
}

// --- o logind falso ---------------------------------------------------------

struct FakeUser {
    display: Arc<Mutex<(String, OwnedObjectPath)>>,
}

#[zbus::interface(name = "org.freedesktop.login1.User")]
impl FakeUser {
    #[zbus(property)]
    fn display(&self) -> (String, OwnedObjectPath) {
        self.display.lock().unwrap().clone()
    }
}

struct FakeSession {
    calls: Arc<Mutex<Vec<(String, String, u32)>>>,
}

#[zbus::interface(name = "org.freedesktop.login1.Session")]
impl FakeSession {
    fn set_brightness(&self, subsystem: String, name: String, brightness: u32) {
        self.calls
            .lock()
            .unwrap()
            .push((subsystem, name, brightness));
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "só no D-Bus isolado (tests/isolado.sh)"]
async fn logind_receives_set_brightness() {
    isolated();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let display = Arc::new(Mutex::new(("2".to_string(), path(SESSION))));
    let service = Builder::system()
        .unwrap()
        .name("org.freedesktop.login1")
        .unwrap()
        .serve_at(
            USER,
            FakeUser {
                display: display.clone(),
            },
        )
        .unwrap()
        .serve_at(
            SESSION,
            FakeSession {
                calls: calls.clone(),
            },
        )
        .unwrap()
        .build()
        .await
        .unwrap();

    let writer = LogindWriter::system();
    writer.write("intel_backlight", 9600).await.unwrap();
    assert_eq!(
        *calls.lock().unwrap(),
        vec![("backlight".to_string(), "intel_backlight".to_string(), 9600)]
    );

    // A sessão acaba: o Display vira "/" e o objeto da sessão some. O
    // caminho guardado falha uma vez e o próximo pedido relê o Display.
    *display.lock().unwrap() = (String::new(), path("/"));
    service
        .object_server()
        .remove::<FakeSession, _>(SESSION)
        .await
        .unwrap();
    let err = writer
        .write("intel_backlight", 100)
        .await
        .expect_err("a sessão guardada não existe mais");
    assert_ne!(err, NO_SESSION);
    assert!(!err.is_empty() && err.chars().count() <= 300, "{err}");
    let err = writer.write("intel_backlight", 100).await.unwrap_err();
    assert_eq!(err, NO_SESSION);

    let err = LogindWriter::system()
        .write("intel_backlight", 100)
        .await
        .unwrap_err();
    assert_eq!(err, NO_SESSION);
    assert_eq!(calls.lock().unwrap().len(), 1, "nenhuma escrita a mais");
}

// --- o UPower falso ---------------------------------------------------------

struct FakeUpower {
    devices: Arc<Mutex<Vec<OwnedObjectPath>>>,
}

#[zbus::interface(name = "org.freedesktop.UPower")]
impl FakeUpower {
    fn enumerate_devices(&self) -> Vec<OwnedObjectPath> {
        self.devices.lock().unwrap().clone()
    }
}

struct FakeDevice {
    kind: u32,
    history: Vec<(u32, f64, u32)>,
    asked: Arc<Mutex<Vec<(String, u32, u32)>>>,
}

#[zbus::interface(name = "org.freedesktop.UPower.Device")]
impl FakeDevice {
    #[zbus(property, name = "Type")]
    fn kind(&self) -> u32 {
        self.kind
    }

    #[zbus(property)]
    fn power_supply(&self) -> bool {
        true
    }

    fn get_history(&self, kind: String, timespan: u32, resolution: u32) -> Vec<(u32, f64, u32)> {
        self.asked
            .lock()
            .unwrap()
            .push((kind, timespan, resolution));
        self.history.clone()
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "só no D-Bus isolado (tests/isolado.sh)"]
async fn history_from_the_battery() {
    isolated();
    let devices = Arc::new(Mutex::new(vec![path(AC), path(BAT)]));
    let asked = Arc::new(Mutex::new(Vec::new()));
    // Como na sonda (d): o mais novo primeiro, taxa 0 no estado 4.
    let history = vec![
        (1792682797, 4.312, 1),
        (1792682677, 0.0, 4),
        (1792682557, 5.5, 1),
    ];
    let _service = Builder::system()
        .unwrap()
        .name("org.freedesktop.UPower")
        .unwrap()
        .serve_at(
            UPOWER,
            FakeUpower {
                devices: devices.clone(),
            },
        )
        .unwrap()
        .serve_at(
            AC,
            FakeDevice {
                kind: 1,
                history: vec![(1, 99.0, 1)],
                asked: asked.clone(),
            },
        )
        .unwrap()
        .serve_at(
            BAT,
            FakeDevice {
                kind: 2,
                history,
                asked: asked.clone(),
            },
        )
        .unwrap()
        .build()
        .await
        .unwrap();

    let power = ZbusPower::system();
    let points = power.history().await.unwrap();
    let point = |time, rate, state| Point { time, rate, state };
    assert_eq!(
        points,
        vec![
            point(1792682557, 5.5, 1),
            point(1792682677, 0.0, 4),
            point(1792682797, 4.312, 1),
        ]
    );
    assert_eq!(
        *asked.lock().unwrap(),
        vec![("rate".to_string(), 3600, 120)],
        "só a bateria, com o pedido do spec"
    );

    // Sem bateria: nenhum ponto, sem erro.
    devices.lock().unwrap().retain(|p| p.as_str() == AC);
    assert_eq!(power.history().await, Ok(vec![]));
}

// --- os perfis falsos -------------------------------------------------------

/// A lista `Profiles` (`aa{sv}`) a partir dos nomes.
fn profile_list(names: &[String]) -> Vec<HashMap<String, OwnedValue>> {
    names
        .iter()
        .map(|name| {
            let value = OwnedValue::try_from(Value::from(name.as_str())).unwrap();
            HashMap::from([("Profile".to_string(), value)])
        })
        .collect()
}

struct UpowerProfiles {
    names: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "org.freedesktop.UPower.PowerProfiles")]
impl UpowerProfiles {
    #[zbus(property)]
    fn profiles(&self) -> Vec<HashMap<String, OwnedValue>> {
        profile_list(&self.names.lock().unwrap())
    }
}

struct HadessProfiles {
    names: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "net.hadess.PowerProfiles")]
impl HadessProfiles {
    #[zbus(property)]
    fn profiles(&self) -> Vec<HashMap<String, OwnedValue>> {
        profile_list(&self.names.lock().unwrap())
    }
}

fn names(list: &[&str]) -> Arc<Mutex<Vec<String>>> {
    Arc::new(Mutex::new(list.iter().map(|s| s.to_string()).collect()))
}

/// Espera o receptor mandar `want`, guardando o que veio antes.
async fn until(rx: &mut UnboundedReceiver<Profiles>, want: Profiles) {
    let mut seen = Vec::new();
    let found = tokio::time::timeout(WAIT, async {
        while let Some(p) = rx.recv().await {
            seen.push(p);
            if p == want {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(found, "esperava {want:?}; veio {seen:?}");
}

fn profiles(available: bool, performance: bool) -> Profiles {
    Profiles {
        available,
        performance,
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "só no D-Bus isolado (tests/isolado.sh)"]
async fn profiles_follow_the_service() {
    isolated();
    let all = names(&["power-saver", "balanced", "performance"]);
    let first = Builder::system()
        .unwrap()
        .name(UPOWER_PROFILES)
        .unwrap()
        .name(HADESS)
        .unwrap()
        .serve_at(UPOWER_PROFILES_PATH, UpowerProfiles { names: all.clone() })
        .unwrap()
        .serve_at(HADESS_PATH, HadessProfiles { names: all.clone() })
        .unwrap()
        .build()
        .await
        .unwrap();

    let power = ZbusPower::system();
    let mut rx = power.watch_profiles();
    let start = tokio::time::timeout(WAIT, rx.recv()).await.unwrap();
    assert_eq!(start, Some(profiles(true, true)), "o estado atual primeiro");

    // O serviço cai: os dois nomes somem.
    first.release_name(UPOWER_PROFILES).await.unwrap();
    first.release_name(HADESS).await.unwrap();
    until(&mut rx, profiles(false, false)).await;
    drop(first);

    // Volta só o net.hadess, sem o desempenho.
    let some = names(&["power-saver", "balanced"]);
    let second = Builder::system()
        .unwrap()
        .name(HADESS)
        .unwrap()
        .serve_at(
            HADESS_PATH,
            HadessProfiles {
                names: some.clone(),
            },
        )
        .unwrap()
        .build()
        .await
        .unwrap();
    until(&mut rx, profiles(true, false)).await;

    // A lista muda, com o PropertiesChanged.
    some.lock().unwrap().push("performance".to_string());
    let iface = second
        .object_server()
        .interface::<_, HadessProfiles>(HADESS_PATH)
        .await
        .unwrap();
    let emitter: &SignalEmitter<'_> = iface.signal_emitter();
    iface.get().await.profiles_changed(emitter).await.unwrap();
    until(&mut rx, profiles(true, true)).await;

    // Largar o receptor encerra a tarefa; o serviço não precisa saber.
    drop(rx);
}
