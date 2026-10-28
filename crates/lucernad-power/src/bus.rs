//! O D-Bus do sistema: a escrita do brilho da tela integrada pelo logind, o
//! histórico de consumo do UPower e os perfis de energia, atrás dos traits
//! [`BacklightWriter`] e [`PowerBus`]; e o [`TreeWriter`], que grava na
//! árvore falsa do `LUCERNAD_POWER_ROOT` em vez de falar com o logind.
//!
//! Toda chamada ao D-Bus tem prazo ([`DBUS_TIMEOUT`]): nada aqui trava o laço
//! do módulo. Os textos de erro devolvidos são só a causa (o módulo põe o
//! começo do Formato, "Não consegui ajustar…"), já cortados pelo
//! [`clip_error`].

use std::collections::HashMap;
use std::fs::File;
use std::future::{Future, poll_fn};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::timeout;
use zbus::export::futures_core::Stream;
use zbus::fdo::DBusProxy;
use zbus::names::BusName;
use zbus::proxy::CacheProperties;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::{Connection, MatchRule, MessageStream, Proxy};

use lucernad_core::test_root::{PROFILES_DIR, inner_dir};

use crate::roots::{Roots, forbidden};

/// Um futuro em caixa, para os traits serem usados como objeto.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// O prazo de cada chamada ao D-Bus (e da conexão).
pub const DBUS_TIMEOUT: Duration = Duration::from_secs(2);

/// Sem o D-Bus do sistema, de quanto em quanto tempo os perfis tentam de novo.
pub const PROFILES_RETRY: Duration = Duration::from_secs(30);

/// O máximo de pontos do histórico publicados (os mais novos).
pub const MAX_POINTS: usize = 500;

/// O maior texto de erro vindo do D-Bus, em caracteres (com o "…").
pub const ERROR_MAX: usize = 300;

/// O período do `GetHistory`, em segundos (a última hora).
pub const HISTORY_TIMESPAN: u32 = 3600;

/// Quantos pontos o `GetHistory` pede.
pub const HISTORY_RESOLUTION: u32 = 120;

/// O erro do logind sem sessão gráfica (o `Display` do usuário é `/`). É o
/// texto inteiro do Formato: o módulo publica sem o começo "Não consegui
/// ajustar…".
pub const NO_SESSION: &str = "O logind não tem uma sessão gráfica deste usuário.";

/// O erro do logind com a sessão gráfica do usuário inativa (outro usuário
/// na tela, pela troca rápida de usuário): nada é gravado. Também é o texto
/// inteiro, publicado sem o começo "Não consegui ajustar…".
pub const INACTIVE_SESSION: &str = "A sessão gráfica deste usuário não está ativa.";

/// O erro do [`TreeWriter`] quando um arquivo de brilho leva para fora da
/// raiz de teste (um link para o `/sys` de verdade).
pub const OUTSIDE_ROOT: &str = "o arquivo de brilho sai da raiz de teste";

/// O erro do [`TreeWriter`] quando a própria raiz cai no `/sys`, no `/dev`
/// ou no `/proc` (ou é o `/`).
pub const ROOT_FORBIDDEN: &str = "a raiz de teste fica no /sys, no /dev ou no /proc";

/// Os tipos de sessão gráfica do logind.
const GRAPHICAL: [&str; 2] = ["wayland", "x11"];

const LOGIND: &str = "org.freedesktop.login1";
const LOGIND_USER: &str = "/org/freedesktop/login1/user/self";
const UPOWER: &str = "org.freedesktop.UPower";
const UPOWER_PATH: &str = "/org/freedesktop/UPower";
const UPOWER_DEVICE: &str = "org.freedesktop.UPower.Device";
/// O `Type` de bateria do `UPower.Device`.
const BATTERY: u32 = 2;

/// Os serviços de perfis, na ordem de preferência: nome (que é também a
/// interface) e caminho.
const PROFILE_SERVICES: [(&str, &str); 2] = [
    (
        "org.freedesktop.UPower.PowerProfiles",
        "/org/freedesktop/UPower/PowerProfiles",
    ),
    ("net.hadess.PowerProfiles", "/net/hadess/PowerProfiles"),
];

/// Sem serviço de perfis.
const NO_PROFILES: Profiles = Profiles {
    available: false,
    performance: false,
};

/// Um ponto do histórico de consumo: o instante (segundos desde a época), a
/// taxa em W e o estado da bateria (o `State` do UPower).
#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    pub time: u32,
    pub rate: f64,
    pub state: u32,
}

/// O serviço de perfis existe e se tem o perfil de desempenho.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profiles {
    pub available: bool,
    pub performance: bool,
}

/// Os pontos do `GetHistory` (`a(udu)`, o mais novo primeiro) do mais
/// antigo ao mais novo, sem taxa negativa nem não finita, no máximo
/// [`MAX_POINTS`] (os mais novos).
pub fn points_from(raw: Vec<(u32, f64, u32)>) -> Vec<Point> {
    let mut points: Vec<Point> = raw
        .into_iter()
        .filter(|(_, rate, _)| rate.is_finite() && *rate >= 0.0)
        .map(|(time, rate, state)| Point { time, rate, state })
        .collect();
    points.sort_by_key(|p| p.time);
    let extra = points.len().saturating_sub(MAX_POINTS);
    points.drain(..extra);
    points
}

/// A propriedade `Profiles` (`aa{sv}`) do serviço de perfis: disponível, e
/// com desempenho se algum `Profile` for `"performance"`.
pub fn profiles_from(list: &[HashMap<String, OwnedValue>]) -> Profiles {
    let performance = list.iter().any(|profile| {
        profile
            .get("Profile")
            .and_then(|v| v.downcast_ref::<&str>().ok())
            == Some("performance")
    });
    Profiles {
        available: true,
        performance,
    }
}

/// Um texto de erro com no máximo [`ERROR_MAX`] caracteres: o que passa é
/// cortado em fronteira de caractere e termina em "…". Cortar de novo não
/// muda nada. Todo erro vindo do D-Bus passa por ele antes de ser publicado.
pub fn clip_error(e: &str) -> String {
    if e.chars().count() <= ERROR_MAX {
        return e.to_string();
    }
    let mut clipped: String = e.chars().take(ERROR_MAX - 1).collect();
    clipped.push('…');
    clipped
}

/// Quem grava o brilho da tela integrada.
pub trait BacklightWriter: Send + Sync + 'static {
    /// Grava `value` (bruto) no backlight `device`. O `Err` é a causa, para
    /// o módulo pôr no texto do Formato (salvo o [`NO_SESSION`], que já é o
    /// texto inteiro).
    fn write<'a>(&'a self, device: &'a str, value: u32) -> BoxFuture<'a, Result<(), String>>;

    /// Por onde a escrita vai, para o texto do erro ("Não consegui ajustar o
    /// brilho <target>: <causa>."): o logind, salvo a árvore de teste.
    fn target(&self) -> &'static str {
        "pelo logind"
    }
}

/// O histórico e os perfis.
pub trait PowerBus: Send + Sync + 'static {
    /// O histórico de consumo da bateria; sem bateria, `Ok(vec![])`.
    fn history(&self) -> BoxFuture<'_, Result<Vec<Point>, String>>;

    /// Os perfis: manda o estado atual e cada mudança (o serviço subir, cair
    /// ou mudar a lista). Sem D-Bus, tenta de novo a cada
    /// [`PROFILES_RETRY`]. Largar o receptor encerra a tarefa. Precisa de um
    /// runtime do `tokio`.
    fn watch_profiles(&self) -> UnboundedReceiver<Profiles>;
}

/// Nenhum D-Bus: sem histórico e sem serviço de perfis.
pub struct NoBus;

impl PowerBus for NoBus {
    fn history(&self) -> BoxFuture<'_, Result<Vec<Point>, String>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn watch_profiles(&self) -> UnboundedReceiver<Profiles> {
        let (tx, rx) = unbounded_channel();
        let _ = tx.send(NO_PROFILES);
        rx
    }
}

/// De quanto em quanto tempo o [`TreeBus`] relê o serviço de perfis falso.
pub const TREE_POLL: Duration = Duration::from_secs(2);

/// A raiz de teste do `LUCERNAD_POWER_ROOT`: sem histórico, e os perfis do
/// power-profiles-daemon falso em arquivos (o mesmo que o modo jogo segura):
/// o serviço existe enquanto a pasta `power-profiles` da raiz existir (e,
/// resolvida, não sair dela), com o perfil de desempenho. Relê a cada
/// [`TREE_POLL`]; nunca fala com o D-Bus.
pub struct TreeBus {
    base: PathBuf,
    poll: Duration,
}

impl TreeBus {
    /// O falso sob `base` (já canônica).
    pub fn new(base: PathBuf) -> TreeBus {
        TreeBus {
            base,
            poll: TREE_POLL,
        }
    }

    /// Outro intervalo de releitura (os testes).
    pub fn with_poll(mut self, poll: Duration) -> TreeBus {
        self.poll = poll;
        self
    }
}

impl PowerBus for TreeBus {
    fn history(&self) -> BoxFuture<'_, Result<Vec<Point>, String>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn watch_profiles(&self) -> UnboundedReceiver<Profiles> {
        let (tx, rx) = unbounded_channel();
        let base = self.base.clone();
        let poll = self.poll;
        tokio::spawn(async move {
            let mut last = None;
            loop {
                let available = inner_dir(&base, PROFILES_DIR).is_some();
                let now = Profiles {
                    available,
                    performance: available,
                };
                publish(&tx, &mut last, now);
                tokio::select! {
                    () = tx.closed() => return,
                    () = tokio::time::sleep(poll) => {}
                }
            }
        });
        rx
    }
}

/// A conexão preguiçosa com o D-Bus do sistema, refeita depois de um erro de
/// E/S (como o `SessionBus` do espectro).
struct SystemBus {
    conn: Mutex<Option<Connection>>,
}

impl SystemBus {
    fn new() -> SystemBus {
        SystemBus {
            conn: Mutex::new(None),
        }
    }

    async fn connection(&self) -> Result<Connection, String> {
        let mut conn = self.conn.lock().await;
        if let Some(conn) = conn.as_ref() {
            return Ok(conn.clone());
        }
        let fresh = within(Connection::system())
            .await
            .map_err(|e| format!("sem o D-Bus do sistema: {e}"))?;
        *conn = Some(fresh.clone());
        Ok(fresh)
    }

    /// Esquece a conexão se o erro for de E/S (o barramento caiu).
    async fn forget_on_io(&self, err: &Failure) {
        if matches!(err, Failure::Bus(zbus::Error::InputOutput(_))) {
            self.forget().await;
        }
    }

    async fn forget(&self) {
        *self.conn.lock().await = None;
    }
}

/// O que deu errado numa chamada.
enum Failure {
    Bus(zbus::Error),
    Late,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Bus(zbus::Error::MethodError(name, desc, _)) => match desc {
                Some(desc) if !desc.is_empty() => f.write_str(desc),
                _ => f.write_str(name.as_str()),
            },
            Failure::Bus(e) => write!(f, "{e}"),
            Failure::Late => write!(f, "o D-Bus não respondeu em {} s", DBUS_TIMEOUT.as_secs()),
        }
    }
}

impl From<zbus::Error> for Failure {
    fn from(e: zbus::Error) -> Failure {
        Failure::Bus(e)
    }
}

impl From<zbus::fdo::Error> for Failure {
    fn from(e: zbus::fdo::Error) -> Failure {
        Failure::Bus(e.into())
    }
}

/// Uma chamada com o prazo [`DBUS_TIMEOUT`].
async fn within<T, E: Into<Failure>>(
    call: impl Future<Output = Result<T, E>>,
) -> Result<T, Failure> {
    match timeout(DBUS_TIMEOUT, call).await {
        Ok(result) => result.map_err(Into::into),
        Err(_) => Err(Failure::Late),
    }
}

/// Um proxy sem cache de propriedades (cada leitura vai ao barramento).
async fn proxy<'a>(
    conn: &Connection,
    name: &'a str,
    path: &'a str,
    interface: &'a str,
) -> Result<Proxy<'a>, Failure> {
    let build = zbus::proxy::Builder::new(conn)
        .destination(name)?
        .path(path)?
        .interface(interface)?
        .cache_properties(CacheProperties::No)
        .build();
    within(build).await
}

/// O histórico do UPower e os perfis pelo D-Bus do sistema.
#[derive(Clone)]
pub struct ZbusPower {
    bus: Arc<SystemBus>,
    retry: Duration,
}

impl ZbusPower {
    /// O D-Bus do sistema; não conecta até a primeira chamada.
    pub fn system() -> ZbusPower {
        ZbusPower {
            bus: Arc::new(SystemBus::new()),
            retry: PROFILES_RETRY,
        }
    }

    async fn history_on(&self, conn: &Connection) -> Result<Vec<Point>, Failure> {
        let upower = proxy(conn, UPOWER, UPOWER_PATH, UPOWER).await?;
        let devices: Vec<OwnedObjectPath> = within(upower.call("EnumerateDevices", &())).await?;
        for path in devices {
            // Um aparelho que não responde (ou some no meio) fica de fora,
            // sem derrubar o histórico.
            let Ok(device) = proxy(conn, UPOWER, path.as_str(), UPOWER_DEVICE).await else {
                continue;
            };
            if !matches!(
                within(device.get_property::<u32>("Type")).await,
                Ok(BATTERY)
            ) {
                continue;
            }
            if !matches!(
                within(device.get_property::<bool>("PowerSupply")).await,
                Ok(true)
            ) {
                continue;
            }
            let raw: Vec<(u32, f64, u32)> = within(device.call(
                "GetHistory",
                &("rate", HISTORY_TIMESPAN, HISTORY_RESOLUTION),
            ))
            .await?;
            return Ok(points_from(raw));
        }
        Ok(Vec::new())
    }
}

impl PowerBus for ZbusPower {
    fn history(&self) -> BoxFuture<'_, Result<Vec<Point>, String>> {
        Box::pin(async move {
            let conn = self.bus.connection().await.map_err(|e| clip_error(&e))?;
            match self.history_on(&conn).await {
                Ok(points) => Ok(points),
                Err(err) => {
                    self.bus.forget_on_io(&err).await;
                    Err(clip_error(&err.to_string()))
                }
            }
        })
    }

    fn watch_profiles(&self) -> UnboundedReceiver<Profiles> {
        let (tx, rx) = unbounded_channel();
        tokio::spawn(follow_profiles(self.clone(), tx));
        rx
    }
}

/// Como terminou o acompanhamento numa conexão.
enum Ended {
    /// O receptor foi largado.
    Closed,
    /// A conexão caiu (ou os sinais não deram para assinar).
    Lost,
}

/// A tarefa do [`PowerBus::watch_profiles`].
async fn follow_profiles(power: ZbusPower, tx: UnboundedSender<Profiles>) {
    let mut last = None;
    loop {
        match power.bus.connection().await {
            Ok(conn) => {
                if let Ended::Closed = follow_on(&conn, &tx, &mut last).await {
                    return;
                }
                power.bus.forget().await;
            }
            Err(e) => eprintln!("lucernad: os perfis de energia: {}", clip_error(&e)),
        }
        publish(&tx, &mut last, NO_PROFILES);
        tokio::select! {
            () = tx.closed() => return,
            () = tokio::time::sleep(power.retry) => {}
        }
    }
}

/// Manda `now` se mudou.
fn publish(tx: &UnboundedSender<Profiles>, last: &mut Option<Profiles>, now: Profiles) {
    if *last != Some(now) {
        *last = Some(now);
        let _ = tx.send(now);
    }
}

/// Acompanha os perfis numa conexão: assina o `NameOwnerChanged` dos dois
/// nomes e o `PropertiesChanged` dos dois caminhos, lê o estado e relê a
/// cada sinal.
async fn follow_on(
    conn: &Connection,
    tx: &UnboundedSender<Profiles>,
    last: &mut Option<Profiles>,
) -> Ended {
    let mut streams = Vec::new();
    for (name, path) in PROFILE_SERVICES {
        for rule in [owner_rule(name), properties_rule(path)] {
            let stream = match rule {
                Ok(rule) => within(MessageStream::for_match_rule(rule, conn, None)).await,
                Err(e) => Err(Failure::Bus(e)),
            };
            match stream {
                Ok(stream) => streams.push(stream),
                Err(e) => {
                    eprintln!(
                        "lucernad: os perfis de energia: {}",
                        clip_error(&e.to_string())
                    );
                    return Ended::Lost;
                }
            }
        }
    }
    // Os sinais assinados antes da leitura: nenhuma mudança fica no meio.
    // Uma leitura incerta (`None`) não publica: fica o último estado.
    if let Some(now) = current_profiles(conn).await {
        publish(tx, last, now);
    }
    loop {
        tokio::select! {
            () = tx.closed() => return Ended::Closed,
            alive = next_signal(&mut streams) => {
                if !alive {
                    return Ended::Lost;
                }
                if let Some(now) = current_profiles(conn).await {
                    publish(tx, last, now);
                }
            }
        }
    }
}

fn owner_rule(name: &str) -> zbus::Result<MatchRule<'_>> {
    Ok(MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .path("/org/freedesktop/DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .arg(0, name)?
        .build())
}

fn properties_rule(path: &str) -> zbus::Result<MatchRule<'_>> {
    Ok(MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .path(path)?
        .interface("org.freedesktop.DBus.Properties")?
        .member("PropertiesChanged")?
        .build())
}

/// O próximo sinal de qualquer um dos fluxos: `true` com um sinal, `false`
/// quando um deles terminou (a conexão caiu). Uma mensagem com erro sozinha
/// é pulada: a queda de verdade termina o fluxo.
async fn next_signal(streams: &mut [MessageStream]) -> bool {
    poll_fn(|cx| {
        for stream in streams.iter_mut() {
            loop {
                match Pin::new(&mut *stream).poll_next(cx) {
                    Poll::Ready(Some(Ok(_))) => return Poll::Ready(true),
                    Poll::Ready(Some(Err(_))) => continue,
                    Poll::Ready(None) => return Poll::Ready(false),
                    Poll::Pending => break,
                }
            }
        }
        Poll::Pending
    })
    .await
}

/// O estado dos perfis agora: o primeiro serviço com dono e com a
/// propriedade `Profiles` legível; nenhum nome com dono, indisponível. Só
/// pergunta a quem tem dono (um nome sem dono ativaria o serviço). `None`
/// quando não deu para saber (um erro ou um prazo estourado no meio): quem
/// chama fica com o último estado, porque só a falta de dono (ou a queda da
/// conexão) diz que o serviço sumiu.
async fn current_profiles(conn: &Connection) -> Option<Profiles> {
    let dbus = within(DBusProxy::new(conn)).await.ok()?;
    let mut uncertain = false;
    for (name, path) in PROFILE_SERVICES {
        let bus_name = BusName::try_from(name).ok()?;
        if !within(dbus.name_has_owner(bus_name)).await.ok()? {
            continue;
        }
        let read = async {
            let service = proxy(conn, name, path, name).await?;
            within(service.get_property::<Vec<HashMap<String, OwnedValue>>>("Profiles")).await
        };
        match read.await {
            Ok(list) => return Some(profiles_from(&list)),
            Err(_) => uncertain = true,
        }
    }
    (!uncertain).then_some(NO_PROFILES)
}

/// Por que não há o caminho da sessão.
enum NoPath {
    /// O `Display` é `/`: sem sessão gráfica.
    NoSession,
    /// A sessão não está ativa (outro usuário na tela).
    Inactive,
    /// A leitura do `Display` ou da sessão falhou.
    Failed(Failure),
}

/// A escrita pelo logind: `SetBrightness("backlight", <dispositivo>,
/// <valor>)` na sessão do `Display` de `/org/freedesktop/login1/user/self`
/// (a sessão gráfica do usuário). O caminho da sessão fica guardado e é
/// relido depois de um erro.
pub struct LogindWriter {
    bus: SystemBus,
    session: Mutex<Option<OwnedObjectPath>>,
}

impl LogindWriter {
    /// O logind do D-Bus do sistema; não conecta até a primeira escrita.
    pub fn system() -> LogindWriter {
        LogindWriter {
            bus: SystemBus::new(),
            session: Mutex::new(None),
        }
    }

    /// O caminho da sessão gráfica, guardado ou lido do `Display`.
    async fn session(&self, conn: &Connection) -> Result<OwnedObjectPath, NoPath> {
        let mut session = self.session.lock().await;
        if let Some(path) = session.as_ref() {
            return Ok(path.clone());
        }
        let read = async {
            let user = proxy(conn, LOGIND, LOGIND_USER, "org.freedesktop.login1.User").await?;
            within(user.get_property::<(String, OwnedObjectPath)>("Display")).await
        };
        let (_, path) = read.await.map_err(NoPath::Failed)?;
        if path.as_str() == "/" {
            return Err(NoPath::NoSession);
        }
        *session = Some(path.clone());
        Ok(path)
    }

    /// Se a sessão `path` pode receber o brilho agora: ativa e gráfica (o
    /// `Type` `wayland` ou `x11`, ou com assento). Conferido a cada escrita:
    /// com a troca rápida de usuário, a sessão fica inativa e o brilho é do
    /// outro usuário.
    async fn usable(session: &Proxy<'_>) -> Result<(), NoPath> {
        let active: bool = within(session.get_property("Active"))
            .await
            .map_err(NoPath::Failed)?;
        let kind: String = within(session.get_property("Type"))
            .await
            .map_err(NoPath::Failed)?;
        let (seat, _): (String, OwnedObjectPath) = within(session.get_property("Seat"))
            .await
            .map_err(NoPath::Failed)?;
        if !GRAPHICAL.contains(&kind.as_str()) && seat.is_empty() {
            return Err(NoPath::NoSession);
        }
        if !active {
            return Err(NoPath::Inactive);
        }
        Ok(())
    }

    async fn set(&self, conn: &Connection, device: &str, value: u32) -> Result<(), String> {
        let result = match self.session(conn).await {
            Ok(path) => self.set_on(conn, &path, device, value).await,
            Err(err) => Err(err),
        };
        match result {
            Ok(()) => Ok(()),
            Err(NoPath::NoSession) => Err(NO_SESSION.to_string()),
            Err(NoPath::Inactive) => Err(INACTIVE_SESSION.to_string()),
            Err(NoPath::Failed(err)) => {
                self.bus.forget_on_io(&err).await;
                Err(clip_error(&err.to_string()))
            }
        }
    }

    /// Confere a sessão `path` e grava nela. Qualquer erro esquece o caminho
    /// guardado: o próximo pedido relê o `Display`.
    async fn set_on(
        &self,
        conn: &Connection,
        path: &OwnedObjectPath,
        device: &str,
        value: u32,
    ) -> Result<(), NoPath> {
        let call = async {
            let session = proxy(
                conn,
                LOGIND,
                path.as_str(),
                "org.freedesktop.login1.Session",
            )
            .await
            .map_err(NoPath::Failed)?;
            Self::usable(&session).await?;
            within(session.call::<_, _, ()>("SetBrightness", &("backlight", device, value)))
                .await
                .map_err(NoPath::Failed)
        };
        let result = call.await;
        if result.is_err() {
            *self.session.lock().await = None;
        }
        result
    }
}

impl BacklightWriter for LogindWriter {
    fn write<'a>(&'a self, device: &'a str, value: u32) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let conn = self.bus.connection().await.map_err(|e| clip_error(&e))?;
            self.set(&conn, device, value).await
        })
    }
}

/// A escrita na árvore falsa do `LUCERNAD_POWER_ROOT`: grava o `brightness`
/// e o `actual_brightness` do backlight, nunca o logind. Cada arquivo passa
/// pelo `canonicalize` e o que não fica sob a raiz canônica é recusado
/// ([`OUTSIDE_ROOT`]): um link para o `/sys` de verdade nunca é escrito.
pub struct TreeWriter {
    pub roots: Roots,
}

impl TreeWriter {
    fn write_now(&self, device: &str, value: u32) -> Result<(), String> {
        let root = self.roots.sysfs.canonicalize().map_err(|e| io_text(&e))?;
        if forbidden(&root) {
            return Err(ROOT_FORBIDDEN.to_string());
        }
        let dir = self.roots.sysfs.join("class/backlight").join(device);
        let brightness = inside(&root, &dir.join("brightness"))?;
        // O `actual_brightness` é opcional, mas também precisa ficar dentro.
        let actual_path = dir.join("actual_brightness");
        let actual = match actual_path.symlink_metadata() {
            Ok(_) => Some(inside(&root, &actual_path)?),
            Err(_) => None,
        };
        let text = format!("{value}\n");
        for path in std::iter::once(brightness).chain(actual) {
            // O `O_NOFOLLOW`: um link posto no lugar do arquivo depois do
            // `canonicalize` é recusado (`ELOOP`), não seguido.
            let flags = OFlags::WRONLY | OFlags::TRUNC | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            let fd = match rustix::fs::open(&path, flags, Mode::empty()) {
                Ok(fd) => fd,
                Err(Errno::LOOP) => return Err(OUTSIDE_ROOT.to_string()),
                Err(e) => return Err(io_text(&e.into())),
            };
            File::from(fd)
                .write_all(text.as_bytes())
                .map_err(|e| io_text(&e))?;
        }
        Ok(())
    }
}

impl BacklightWriter for TreeWriter {
    fn write<'a>(&'a self, device: &'a str, value: u32) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move { self.write_now(device, value) })
    }

    fn target(&self) -> &'static str {
        "na árvore de teste"
    }
}

/// O caminho canônico de `path`, se ficar sob `root` (por componente).
fn inside(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let real = path.canonicalize().map_err(|e| io_text(&e))?;
    if real.starts_with(root) {
        Ok(real)
    } else {
        Err(OUTSIDE_ROOT.to_string())
    }
}

/// O erro de E/S da árvore de teste, sem o errno.
fn io_text(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::NotFound => "o arquivo de brilho não existe na raiz de teste",
        io::ErrorKind::PermissionDenied => "sem permissão no arquivo de brilho da raiz de teste",
        _ => "não deu para gravar o arquivo de brilho da raiz de teste",
    }
    .to_string()
}
