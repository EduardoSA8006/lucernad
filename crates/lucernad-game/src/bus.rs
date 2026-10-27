//! O perfil de energia e o GameMode da Feral pelo D-Bus (D10 e D11), atrás
//! do trait [`GameBus`].
//!
//! O perfil é segurado pelo `HoldProfile("performance", …)` do
//! power-profiles-daemon (`org.freedesktop.UPower.PowerProfiles`, com o
//! `net.hadess.PowerProfiles` de reserva) na conexão do sistema, que fica
//! viva enquanto houver retenção: o daemon de perfis solta sozinho se ela
//! cair. O GameMode recebe o `RegisterGameByPID` na conexão da sessão. Toda
//! chamada tem o prazo [`DBUS_TIMEOUT`] (o [`timed`]).

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

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
