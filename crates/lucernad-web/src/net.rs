//! A conectividade: se há internet, pelo NetworkManager (o D-Bus do sistema).
//!
//! O [`NmConnectivity`] lê a propriedade `Connectivity` de
//! `org.freedesktop.NetworkManager` e relê a cada `PropertiesChanged` do
//! caminho dele e a cada `NameOwnerChanged` do nome (o NetworkManager saiu
//! ou voltou). Sem barramento ou sem o nome, o estado é [`Link::Unknown`] (o
//! módulo tenta a rede), e a conexão é tentada de novo a cada [`RETRY`].

use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::timeout;
use zbus::export::futures_core::Stream;
use zbus::fdo::DBusProxy;
use zbus::names::BusName;
use zbus::proxy::CacheProperties;
use zbus::{Connection, MatchRule, MessageStream, Proxy};

/// O nome, o caminho e a interface do NetworkManager.
const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";

/// O prazo de cada chamada ao D-Bus.
pub const DBUS_TIMEOUT: Duration = Duration::from_secs(2);

/// A espera antes de tentar o barramento de novo (sem ele, ou depois de ele
/// cair).
pub const RETRY: Duration = Duration::from_secs(30);

/// Se há internet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// Há conexão (completa, limitada ou atrás de um portal).
    Online,
    /// Nenhuma conexão: o módulo não tenta a rede.
    Offline,
    /// Não dá para saber (sem NetworkManager): o módulo tenta.
    Unknown,
}

/// O `Connectivity` do NetworkManager: 1 (nenhuma) → [`Link::Offline`]; 2
/// (portal), 3 (limitada) e 4 (completa) → [`Link::Online`]; 0
/// (desconhecida) e qualquer outro → [`Link::Unknown`].
pub fn link_from(connectivity: u32) -> Link {
    match connectivity {
        1 => Link::Offline,
        2..=4 => Link::Online,
        _ => Link::Unknown,
    }
}

/// De onde vem a conectividade.
pub trait Connectivity: Send + Sync + 'static {
    /// O estado e as trocas. Chame dentro do runtime: a primeira chamada
    /// começa a tarefa que acompanha, as outras dividem o mesmo `watch`.
    fn watch(&self) -> watch::Receiver<Link>;
}

/// A conectividade pelo NetworkManager no D-Bus do sistema (preguiçosa: nada
/// é aberto até o primeiro [`Connectivity::watch`]).
#[derive(Clone)]
pub struct NmConnectivity {
    /// O lado de quem publica, enquanto a tarefa acompanha (`None` antes do
    /// primeiro `watch`).
    tx: Arc<Mutex<Option<watch::Sender<Link>>>>,
}

impl NmConnectivity {
    /// O D-Bus do sistema.
    pub fn system() -> NmConnectivity {
        NmConnectivity {
            tx: Arc::new(Mutex::new(None)),
        }
    }
}

impl Connectivity for NmConnectivity {
    fn watch(&self) -> watch::Receiver<Link> {
        let mut tx = self
            .tx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Com algum receptor vivo, a tarefa continua: divide o mesmo `watch`.
        // Sem nenhum, ela terminou (ou vai terminar, e nunca ganha outro
        // receptor): começa outra.
        if let Some(tx) = tx.as_ref().filter(|tx| tx.receiver_count() > 0) {
            return tx.subscribe();
        }
        let (fresh, rx) = watch::channel(Link::Unknown);
        tokio::spawn(follow(fresh.clone()));
        *tx = Some(fresh);
        rx
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

/// A tarefa do [`NmConnectivity`]: conecta, acompanha enquanto a conexão
/// durar e, sem ela, publica [`Link::Unknown`] e tenta de novo em [`RETRY`].
/// O aviso no log sai só na primeira falha seguida. Termina quando todos os
/// receptores caem.
async fn follow(tx: watch::Sender<Link>) {
    let mut warned = false;
    loop {
        match within(Connection::system()).await {
            Ok(conn) => {
                warned = false;
                if let Ended::Closed = follow_on(&conn, &tx).await {
                    return;
                }
            }
            Err(e) => {
                if !warned {
                    eprintln!(
                        "lucernad: a conectividade: sem o D-Bus do sistema ({e}); \
                         tento de novo a cada {} s",
                        RETRY.as_secs()
                    );
                    warned = true;
                }
            }
        }
        publish(&tx, Link::Unknown);
        tokio::select! {
            () = tx.closed() => return,
            () = tokio::time::sleep(RETRY) => {}
        }
    }
}

/// Como terminou o acompanhamento numa conexão.
enum Ended {
    /// Todos os receptores caíram.
    Closed,
    /// A conexão caiu (ou os sinais não deram para assinar).
    Lost,
}

/// Manda `now` se mudou.
fn publish(tx: &watch::Sender<Link>, now: Link) {
    tx.send_if_modified(|link| {
        let changed = *link != now;
        *link = now;
        changed
    });
}

/// Acompanha numa conexão: assina o `NameOwnerChanged` do nome e o
/// `PropertiesChanged` do caminho, lê o estado e relê a cada sinal.
async fn follow_on(conn: &Connection, tx: &watch::Sender<Link>) -> Ended {
    let mut streams = Vec::new();
    for rule in [owner_rule(), properties_rule()] {
        let stream = match rule {
            Ok(rule) => within(MessageStream::for_match_rule(rule, conn, None)).await,
            Err(e) => Err(Failure::Bus(e)),
        };
        match stream {
            Ok(stream) => streams.push(stream),
            Err(e) => {
                eprintln!("lucernad: a conectividade: {e}");
                return Ended::Lost;
            }
        }
    }
    // Os sinais assinados antes da leitura: nenhuma troca fica no meio.
    publish(tx, current(conn).await);
    loop {
        tokio::select! {
            () = tx.closed() => return Ended::Closed,
            alive = next_signal(&mut streams) => {
                if !alive {
                    return Ended::Lost;
                }
                publish(tx, current(conn).await);
            }
        }
    }
}

fn owner_rule() -> zbus::Result<MatchRule<'static>> {
    Ok(MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .path("/org/freedesktop/DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .arg(0, NM)?
        .build())
}

fn properties_rule() -> zbus::Result<MatchRule<'static>> {
    Ok(MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .path(NM_PATH)?
        .interface("org.freedesktop.DBus.Properties")?
        .member("PropertiesChanged")?
        .build())
}

/// O próximo sinal de qualquer um dos fluxos: `true` com um sinal, `false`
/// quando um deles terminou (a conexão caiu). Uma mensagem com erro sozinha
/// é pulada.
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

/// A conectividade agora: sem dono do nome, [`Link::Unknown`] (só pergunta a
/// quem tem dono: um nome sem dono ativaria o serviço); com dono, o
/// `Connectivity` dele. Uma leitura que falha ou estoura o prazo também dá
/// [`Link::Unknown`]: o módulo tenta a rede em vez de ficar preso num
/// `Offline` velho com o NetworkManager travado.
async fn current(conn: &Connection) -> Link {
    let read = async {
        let dbus = within(DBusProxy::new(conn)).await?;
        let name = BusName::try_from(NM).map_err(zbus::Error::from)?;
        if !within(dbus.name_has_owner(name)).await? {
            return Ok(Link::Unknown);
        }
        let build = zbus::proxy::Builder::<Proxy<'_>>::new(conn)
            .destination(NM)?
            .path(NM_PATH)?
            .interface(NM)?
            .cache_properties(CacheProperties::No)
            .build();
        let nm = within(build).await?;
        let value = within(nm.get_property::<u32>("Connectivity")).await?;
        Ok::<_, Failure>(link_from(value))
    };
    read.await.unwrap_or(Link::Unknown)
}
