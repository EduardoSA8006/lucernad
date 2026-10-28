//! O perfil de energia e o GameMode da Feral pelo D-Bus (D10 e D11), atrás
//! do trait [`GameBus`].
//!
//! O perfil é segurado pelo `HoldProfile("performance", …)` do
//! power-profiles-daemon (`org.freedesktop.UPower.PowerProfiles`, com o
//! `net.hadess.PowerProfiles` de reserva) na conexão do sistema, que fica
//! viva enquanto houver retenção: o daemon de perfis solta sozinho se ela
//! cair. O GameMode recebe o `RegisterGameByPID` na conexão da sessão. Toda
//! chamada tem o prazo [`DBUS_TIMEOUT`] (o [`timed`]).
//!
//! Com o `LUCERNAD_POWER_ROOT` (a raiz de teste do container do shell, que
//! monta o D-Bus do sistema do host), o [`choose`] troca os dois pelo
//! [`TreeGame`], que grava em arquivos nessa raiz e nunca toca D-Bus nenhum;
//! com a raiz inválida, ou num container sem ela, pelo [`NoGameBus`].

use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use lucernad_core::test_root::{GAMEMODE_DIR, PROFILES_DIR, inner_dir, test_base};
use tokio::sync::Mutex;
use zbus::Connection;
use zbus::fdo::DBusProxy;
use zbus::names::BusName;

/// Um futuro em caixa, para o trait ser usado como objeto.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// O prazo de cada chamada (e da conexão).
pub const DBUS_TIMEOUT: Duration = Duration::from_secs(2);

/// Os serviços de perfis, na ordem de preferência: nome (que é também a
/// interface) e caminho.
const PROFILE_SERVICES: [(&str, &str); 2] = [
    (
        "org.freedesktop.UPower.PowerProfiles",
        "/org/freedesktop/UPower/PowerProfiles",
    ),
    ("net.hadess.PowerProfiles", "/net/hadess/PowerProfiles"),
];
const GAMEMODE: &str = "com.feralinteractive.GameMode";
const GAMEMODE_PATH: &str = "/com/feralinteractive/GameMode";

/// O que deu errado numa chamada.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BusError {
    /// O serviço não está no barramento (ou não há barramento).
    Absent,
    /// O serviço respondeu com erro.
    Failed(String),
    /// Sem resposta no prazo.
    TimedOut,
}

impl fmt::Display for BusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BusError::Absent => write!(f, "o serviço não está no D-Bus"),
            BusError::Failed(e) => f.write_str(e),
            BusError::TimedOut => {
                write!(f, "o D-Bus não respondeu em {} s", DBUS_TIMEOUT.as_secs())
            }
        }
    }
}

impl BusError {
    /// O valor no `effects` do `game.state`.
    pub fn effect(&self) -> String {
        match self {
            BusError::Absent => "unavailable".into(),
            other => format!("error: {other}"),
        }
    }
}

/// Uma chamada com o prazo [`DBUS_TIMEOUT`].
pub async fn timed<T>(call: impl Future<Output = Result<T, BusError>>) -> Result<T, BusError> {
    tokio::time::timeout(DBUS_TIMEOUT, call)
        .await
        .unwrap_or(Err(BusError::TimedOut))
}

/// A resposta do `RegisterGameByPID`/`UnregisterGameByPID`: negativa é erro.
pub fn gamemode_reply(code: i32) -> Result<(), BusError> {
    if code < 0 {
        Err(BusError::Failed(format!(
            "o GameMode recusou o pedido (código {code})"
        )))
    } else {
        Ok(())
    }
}

/// O perfil de energia e o GameMode.
pub trait GameBus: Send + Sync + 'static {
    /// Segura o perfil "desempenho"; devolve o cookie. Cada etapa tem o
    /// próprio prazo: quem chama não põe outro por fora (cortar o futuro
    /// depois de o pedido sair perderia o cookie). Se a resposta não chega,
    /// a implementação larga a conexão (o serviço solta a retenção com ela).
    fn hold_performance(&self) -> BoxFuture<'_, Result<u32, BusError>>;
    /// Solta a retenção `cookie`; se não der, larga a conexão (o serviço
    /// solta a retenção com ela).
    fn release(&self, cookie: u32) -> BoxFuture<'_, Result<(), BusError>>;
    /// Se o GameMode está no barramento da sessão.
    fn gamemode_present(&self) -> BoxFuture<'_, Result<bool, BusError>>;
    /// Registra o processo `pid` no GameMode.
    fn register_game(&self, pid: i32) -> BoxFuture<'_, Result<(), BusError>>;
    /// Tira o processo `pid` do GameMode.
    fn unregister_game(&self, pid: i32) -> BoxFuture<'_, Result<(), BusError>>;
}

/// Uma conexão preguiçosa, refeita depois de um erro de E/S.
struct Lazy {
    system: bool,
    conn: Mutex<Option<Connection>>,
}

impl Lazy {
    fn new(system: bool) -> Lazy {
        Lazy {
            system,
            conn: Mutex::new(None),
        }
    }

    async fn get(&self) -> Result<Connection, BusError> {
        let mut conn = self.conn.lock().await;
        if let Some(c) = conn.as_ref() {
            return Ok(c.clone());
        }
        let fresh = if self.system {
            timed(async { Connection::system().await.map_err(|_| BusError::Absent) }).await?
        } else {
            timed(async { Connection::session().await.map_err(|_| BusError::Absent) }).await?
        };
        *conn = Some(fresh.clone());
        Ok(fresh)
    }

    /// Larga a conexão guardada: sem outros clones vivos, ela fecha.
    async fn forget(&self) {
        *self.conn.lock().await = None;
    }

    async fn forget_on(&self, e: &zbus::Error) {
        if matches!(e, zbus::Error::InputOutput(_)) {
            *self.conn.lock().await = None;
        }
    }
}

fn failed(e: &zbus::Error) -> BusError {
    match e {
        zbus::Error::MethodError(name, desc, _) => BusError::Failed(
            desc.clone()
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| name.to_string()),
        ),
        other => BusError::Failed(other.to_string()),
    }
}

async fn has_owner(conn: &Connection, name: &str) -> Result<bool, BusError> {
    timed(async {
        let dbus = DBusProxy::new(conn).await.map_err(|e| failed(&e))?;
        let name = BusName::try_from(name).map_err(|e| BusError::Failed(e.to_string()))?;
        dbus.name_has_owner(name)
            .await
            .map_err(|e| failed(&e.into()))
    })
    .await
}

/// O perfil e o GameMode pelos barramentos de verdade.
pub struct ZbusGame {
    system: Lazy,
    session: Lazy,
    /// O serviço de perfis que segura a retenção.
    holder: Mutex<Option<&'static str>>,
}

impl ZbusGame {
    /// O D-Bus do sistema (perfis) e da sessão (GameMode); nada conecta até
    /// a primeira chamada.
    pub fn system() -> ZbusGame {
        ZbusGame {
            system: Lazy::new(true),
            session: Lazy::new(false),
            holder: Mutex::new(None),
        }
    }

    async fn call<R>(
        lazy: &Lazy,
        service: &str,
        path: &str,
        method: &str,
        body: &(impl serde::Serialize + zbus::zvariant::DynamicType + Sync),
    ) -> Result<R, BusError>
    where
        R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
    {
        let conn = lazy.get().await?;
        let outcome = tokio::time::timeout(DBUS_TIMEOUT, async {
            let proxy = zbus::Proxy::new(&conn, service, path, service).await?;
            proxy.call::<_, _, R>(method, body).await
        })
        .await;
        match outcome {
            Ok(Ok(r)) => Ok(r),
            Ok(Err(e)) => {
                // Só a queda do barramento esquece a conexão: a do sistema
                // segura o perfil e não pode cair por um erro de método.
                lazy.forget_on(&e).await;
                Err(failed(&e))
            }
            Err(_) => Err(BusError::TimedOut),
        }
    }
}

impl GameBus for ZbusGame {
    fn hold_performance(&self) -> BoxFuture<'_, Result<u32, BusError>> {
        Box::pin(async move {
            let conn = self.system.get().await?;
            for (name, path) in PROFILE_SERVICES {
                if !has_owner(&conn, name).await? {
                    continue;
                }
                drop(conn);
                let held: Result<u32, BusError> = Self::call(
                    &self.system,
                    name,
                    path,
                    "HoldProfile",
                    &("performance", "Modo jogo do Lucerna", "lucernad"),
                )
                .await;
                let cookie = match held {
                    Ok(c) => c,
                    Err(e) => {
                        // O pedido pode ter chegado sem a resposta: larga a
                        // conexão para o serviço soltar uma retenção órfã.
                        self.system.forget().await;
                        return Err(e);
                    }
                };
                *self.holder.lock().await = Some(name);
                return Ok(cookie);
            }
            Err(BusError::Absent)
        })
    }

    fn release(&self, cookie: u32) -> BoxFuture<'_, Result<(), BusError>> {
        Box::pin(async move {
            let Some(name) = self.holder.lock().await.take() else {
                return Ok(());
            };
            let path = PROFILE_SERVICES
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, p)| *p)
                .unwrap_or_default();
            let released =
                Self::call::<()>(&self.system, name, path, "ReleaseProfile", &(cookie,)).await;
            if released.is_err() {
                // O cookie se perdeu: a conexão caindo solta a retenção.
                self.system.forget().await;
            }
            released
        })
    }

    fn gamemode_present(&self) -> BoxFuture<'_, Result<bool, BusError>> {
        Box::pin(async move {
            match self.session.get().await {
                Ok(conn) => has_owner(&conn, GAMEMODE).await,
                Err(BusError::Absent) => Ok(false),
                Err(e) => Err(e),
            }
        })
    }

    fn register_game(&self, pid: i32) -> BoxFuture<'_, Result<(), BusError>> {
        Box::pin(async move {
            if !self.gamemode_present().await? {
                return Err(BusError::Absent);
            }
            let me = i32::try_from(std::process::id()).unwrap_or(0);
            let code: i32 = Self::call(
                &self.session,
                GAMEMODE,
                GAMEMODE_PATH,
                "RegisterGameByPID",
                &(me, pid),
            )
            .await?;
            gamemode_reply(code)
        })
    }

    fn unregister_game(&self, pid: i32) -> BoxFuture<'_, Result<(), BusError>> {
        Box::pin(async move {
            if !self.gamemode_present().await? {
                return Err(BusError::Absent);
            }
            let me = i32::try_from(std::process::id()).unwrap_or(0);
            let code: i32 = Self::call(
                &self.session,
                GAMEMODE,
                GAMEMODE_PATH,
                "UnregisterGameByPID",
                &(me, pid),
            )
            .await?;
            gamemode_reply(code)
        })
    }
}

/// O texto do aviso com o `LUCERNAD_POWER_ROOT` inválido.
pub const ROOT_INVALID: &str = "LUCERNAD_POWER_ROOT inválido: o perfil de energia e o GameMode do modo jogo ficam indisponíveis.";

/// O texto do aviso num container sem o `LUCERNAD_POWER_ROOT`.
pub const CONTAINER_REFUSED: &str = "Em container, o perfil de energia e o GameMode do modo jogo só rodam com LUCERNAD_POWER_ROOT: sem ela, o perfil do host mudaria.";

/// Por onde o modo jogo segura o perfil e fala com o GameMode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BusChoice {
    /// Os barramentos de verdade ([`ZbusGame`]).
    Real,
    /// Os falsos em arquivos sob esta raiz ([`TreeGame`]).
    Tree(PathBuf),
    /// Nenhum ([`NoGameBus`]), com o motivo.
    Off(String),
}

/// A escolha pelo valor do `LUCERNAD_POWER_ROOT`: ausente ou vazio, os
/// barramentos de verdade, salvo em container (nenhum); uma raiz válida (a
/// mesma regra do módulo `power`), os falsos sobre ela; inválida, nenhum.
/// Nunca volta aos barramentos de verdade com a variável definida.
pub fn choose(value: Option<&OsStr>, in_container: bool) -> BusChoice {
    match test_base(value) {
        Ok(Some(base)) => BusChoice::Tree(base),
        Ok(None) if in_container => BusChoice::Off(CONTAINER_REFUSED.into()),
        Ok(None) => BusChoice::Real,
        Err(()) => BusChoice::Off(ROOT_INVALID.into()),
    }
}

/// Nenhum perfil e nenhum GameMode: tudo "unavailable".
pub struct NoGameBus;

impl GameBus for NoGameBus {
    fn hold_performance(&self) -> BoxFuture<'_, Result<u32, BusError>> {
        Box::pin(async { Err(BusError::Absent) })
    }

    fn release(&self, _cookie: u32) -> BoxFuture<'_, Result<(), BusError>> {
        Box::pin(async { Ok(()) })
    }

    fn gamemode_present(&self) -> BoxFuture<'_, Result<bool, BusError>> {
        Box::pin(async { Ok(false) })
    }

    fn register_game(&self, _pid: i32) -> BoxFuture<'_, Result<(), BusError>> {
        Box::pin(async { Err(BusError::Absent) })
    }

    fn unregister_game(&self, _pid: i32) -> BoxFuture<'_, Result<(), BusError>> {
        Box::pin(async { Err(BusError::Absent) })
    }
}

/// O perfil a que o falso volta quando não havia um `active`.
const DEFAULT_PROFILE: &str = "balanced";

/// O perfil de energia e o GameMode falsos, em arquivos sob a raiz de teste.
///
/// - O power-profiles-daemon existe se a pasta `power-profiles` existe. O
///   `active` é o perfil ativo. O `HoldProfile` grava `holds/<cookie>` (o
///   menor número livre a partir de 1) e põe `performance` no `active`; a
///   primeira retenção guarda o perfil de antes no `before` (sem `active`,
///   `balanced`). O `ReleaseProfile` apaga a retenção e, sem nenhuma, volta
///   o `before` ao `active`.
/// - O GameMode existe se a pasta `gamemode` existe; o registro é o arquivo
///   `games/<pid>`.
///
/// Uma pasta que, resolvida, sai da raiz (um link) conta como ausente.
pub struct TreeGame {
    base: PathBuf,
    lock: std::sync::Mutex<()>,
}

fn io_failed(e: &io::Error) -> BusError {
    BusError::Failed(format!("a raiz de teste: {e}"))
}

impl TreeGame {
    /// Os falsos sob `base` (já canônica).
    pub fn new(base: PathBuf) -> TreeGame {
        TreeGame {
            base,
            lock: std::sync::Mutex::new(()),
        }
    }

    fn dir(&self, name: &str) -> Option<PathBuf> {
        inner_dir(&self.base, name)
    }

    fn hold_now(&self) -> Result<u32, BusError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let dir = self.dir(PROFILES_DIR).ok_or(BusError::Absent)?;
        let holds = dir.join("holds");
        fs::create_dir_all(&holds).map_err(|e| io_failed(&e))?;
        let cookie = (1..=u32::MAX)
            .find(|c| !holds.join(c.to_string()).exists())
            .ok_or_else(|| BusError::Failed("sem cookie livre".into()))?;
        if count(&holds)? == 0 {
            let before = fs::read_to_string(dir.join("active"))
                .map(|s| s.trim().to_string())
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| DEFAULT_PROFILE.into());
            fs::write(dir.join("before"), format!("{before}\n")).map_err(|e| io_failed(&e))?;
        }
        fs::write(
            holds.join(cookie.to_string()),
            "performance\nModo jogo do Lucerna\nlucernad\n",
        )
        .map_err(|e| io_failed(&e))?;
        fs::write(dir.join("active"), "performance\n").map_err(|e| io_failed(&e))?;
        Ok(cookie)
    }

    fn release_now(&self, cookie: u32) -> Result<(), BusError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let dir = self.dir(PROFILES_DIR).ok_or(BusError::Absent)?;
        let holds = dir.join("holds");
        match fs::remove_file(holds.join(cookie.to_string())) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(BusError::Failed(format!(
                    "nenhuma retenção com o cookie {cookie}"
                )));
            }
            Err(e) => return Err(io_failed(&e)),
        }
        if count(&holds)? == 0 {
            let before = fs::read_to_string(dir.join("before"))
                .map(|s| s.trim().to_string())
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| DEFAULT_PROFILE.into());
            fs::write(dir.join("active"), format!("{before}\n")).map_err(|e| io_failed(&e))?;
            let _ = fs::remove_file(dir.join("before"));
        }
        Ok(())
    }

    fn game_file(&self, pid: i32, create: bool) -> Result<PathBuf, BusError> {
        let dir = self.dir(GAMEMODE_DIR).ok_or(BusError::Absent)?;
        let games = dir.join("games");
        if create {
            fs::create_dir_all(&games).map_err(|e| io_failed(&e))?;
        }
        Ok(games.join(pid.to_string()))
    }
}

/// Quantas entradas há numa pasta.
fn count(dir: &Path) -> Result<usize, BusError> {
    Ok(fs::read_dir(dir).map_err(|e| io_failed(&e))?.count())
}

impl GameBus for TreeGame {
    fn hold_performance(&self) -> BoxFuture<'_, Result<u32, BusError>> {
        Box::pin(async move { self.hold_now() })
    }

    fn release(&self, cookie: u32) -> BoxFuture<'_, Result<(), BusError>> {
        Box::pin(async move { self.release_now(cookie) })
    }

    fn gamemode_present(&self) -> BoxFuture<'_, Result<bool, BusError>> {
        Box::pin(async move { Ok(self.dir(GAMEMODE_DIR).is_some()) })
    }

    fn register_game(&self, pid: i32) -> BoxFuture<'_, Result<(), BusError>> {
        Box::pin(async move {
            let file = self.game_file(pid, true)?;
            fs::write(file, format!("{pid}\n")).map_err(|e| io_failed(&e))
        })
    }

    fn unregister_game(&self, pid: i32) -> BoxFuture<'_, Result<(), BusError>> {
        Box::pin(async move {
            match fs::remove_file(self.game_file(pid, false)?) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(io_failed(&e)),
            }
        })
    }
}
