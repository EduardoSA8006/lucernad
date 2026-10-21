//! O que o D-Bus da sessão diz de um player: o PID do dono do nome e o
//! `DesktopEntry` do MPRIS, atrás do trait [`Bus`].

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::timeout;
use zbus::Connection;
use zbus::fdo::DBusProxy;
use zbus::names::BusName;
use zbus::proxy::CacheProperties;

/// Um futuro em caixa, para o trait [`Bus`] ser usado como objeto.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// O prazo padrão de cada chamada ao D-Bus.
pub const DBUS_TIMEOUT: Duration = Duration::from_secs(1);

/// O que se sabe do player `name`; o que não deu para saber fica `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    /// O nome D-Bus do player (`org.mpris.MediaPlayer2.*`).
    pub name: String,
    /// O PID do dono do nome.
    pub owner_pid: Option<u32>,
    /// A propriedade `DesktopEntry` do MPRIS.
    pub desktop_entry: Option<String>,
}

/// O barramento que identifica os players.
pub trait Bus: Send + Sync + 'static {
    /// O que o barramento diz de `name`; nunca falha, só deixa campos `None`.
    fn identify<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Identity>;
}

/// O D-Bus da sessão do usuário, pelo `zbus`.
pub struct SessionBus {
    timeout: Duration,
    /// A conexão guardada; volta a `None` depois de um erro de E/S, para a
    /// próxima chamada refazer.
    conn: Mutex<Option<Connection>>,
}

impl SessionBus {
    /// Um barramento com `timeout` em cada chamada (a conexão, o PID e o
    /// `DesktopEntry`). Não conecta até o primeiro [`Bus::identify`].
    pub fn new(timeout: Duration) -> SessionBus {
        SessionBus {
            timeout,
            conn: Mutex::new(None),
        }
    }

    async fn connection(&self) -> Option<Connection> {
        let mut conn = self.conn.lock().await;
        if let Some(conn) = conn.as_ref() {
            return Some(conn.clone());
        }
        let fresh = timeout(self.timeout, Connection::session())
            .await
            .ok()?
            .ok()?;
        *conn = Some(fresh.clone());
        Some(fresh)
    }

    /// Esquece a conexão se o erro for de E/S (o barramento caiu).
    async fn forget_on_io(&self, err: &zbus::Error) {
        if matches!(err, zbus::Error::InputOutput(_)) {
            *self.conn.lock().await = None;
        }
    }

    async fn owner_pid(&self, conn: &Connection, name: &BusName<'_>) -> Option<u32> {
        let call = async {
            let proxy = DBusProxy::new(conn).await?;
            proxy
                .get_connection_unix_process_id(name.clone())
                .await
                .map_err(zbus::Error::from)
        };
        match timeout(self.timeout, call).await.ok()? {
            Ok(pid) => Some(pid),
            Err(err) => {
                self.forget_on_io(&err).await;
                None
            }
        }
    }

    async fn desktop_entry(&self, conn: &Connection, name: &BusName<'_>) -> Option<String> {
        let call = async {
            let proxy: zbus::Proxy<'_> = zbus::proxy::Builder::new(conn)
                .destination(name.clone())?
                .path("/org/mpris/MediaPlayer2")?
                .interface("org.mpris.MediaPlayer2")?
                .cache_properties(CacheProperties::No)
                .build()
                .await?;
            proxy.get_property::<String>("DesktopEntry").await
        };
        match timeout(self.timeout, call).await.ok()? {
            Ok(entry) => Some(entry),
            Err(err) => {
                self.forget_on_io(&err).await;
                None
            }
        }
    }
}

impl Bus for SessionBus {
    fn identify<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Identity> {
        Box::pin(async move {
            let mut id = Identity {
                name: name.to_string(),
                ..Identity::default()
            };
            let Ok(bus_name) = BusName::try_from(name) else {
                return id;
            };
            let Some(conn) = self.connection().await else {
                return id;
            };
            id.owner_pid = self.owner_pid(&conn, &bus_name).await;
            id.desktop_entry = self.desktop_entry(&conn, &bus_name).await;
            id
        })
    }
}
