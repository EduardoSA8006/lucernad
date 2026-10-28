//! Módulo `clipboard` do `lucernad`: o histórico da área de transferência do
//! Wayland.
//!
//! A escolha do tipo de cada cópia (e o filtro do conteúdo sensível) fica no
//! [`mime`]; a leitura do pipe, com prazo e limite, no [`pipe`]; a busca e a
//! miniatura, puras, no [`search`] e no [`thumb`]; o banco no [`store`]. O
//! compositor fica atrás do [`backend::Backend`]; o de verdade é o
//! [`wayland::Wayland`].
//!
//! O módulo se registra num servidor com [`register`] e roda com
//! [`Clipboard::spawn`]. Nada é observado antes do primeiro
//! `clipboard.configure` (ou do padrão, [`CONFIGURE_WAIT`] depois do primeiro
//! assinante ou método). A conexão com o compositor existe enquanto houver
//! assinante de `clipboard.history` **ou** enquanto a seleção for nossa
//! (depois de um `clipboard.copy`), com um `WorkGuard` segurando o daemon.

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::future::Future;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::panic::resume_unwind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lucerna_paths::{AppPaths, Env};
use lucerna_service::{RpcError, ServerBuilder, Topic, WorkGuard, WorkTracker};
use lucernad_core::{Detached, GameSignal, ModuleTask, detached, publish_changed};
use rustix::fs::OFlags;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::backend::{Backend, Control, Event, Session, Source};
use crate::mime::{Choice, IMAGE_MAX, Kind, READ_TIMEOUT, choose, offers_for, uri_lines};
use crate::pipe::{ReadError, read_pipe, socket_path};
use crate::store::{ListQuery, NewClip, Origin, Prepared, Store, prepare};
use crate::thumb::sniff;
use crate::wayland::{CONNECT_TIMEOUT, NOT_RESPONDING, Wayland, disconnected};

pub mod backend;
pub mod mime;
pub mod pipe;
pub mod search;
pub mod store;
#[cfg(feature = "testing")]
pub mod testing;
pub mod thumb;
pub mod wayland;

/// O prazo da abertura de uma sessão com o compositor.
pub const OPEN_TIMEOUT: Duration = CONNECT_TIMEOUT;
/// Quanto esperar o primeiro `configure` antes de usar o padrão.
pub const CONFIGURE_WAIT: Duration = Duration::from_secs(2);
/// Quanto esperar para tentar de novo depois de uma falha do compositor.
pub const RETRY: Duration = Duration::from_secs(5);
/// O limite de itens não fixados sem `configure`.
pub const DEFAULT_LIMIT: usize = 100;
/// O maior limite aceito pelo `clipboard.configure`.
pub const MAX_LIMIT: usize = 1000;
/// A maior página do `clipboard.list`.
pub const LIST_MAX: usize = 200;
/// A maior consulta do `clipboard.list`, em caracteres.
const QUERY_MAX: usize = 1000;

/// Quantas fontes nossas esperam o eco, no máximo (um compositor que não
/// ecoa não faz a fila crescer).
const MAX_ECHOES: usize = 16;
/// O id do módulo nas pastas da `lucerna-paths`.
const APP: &str = "clipboard";

/// O texto do estado quando o laço do módulo entra em pânico.
const PANIC_ERROR: &str = "O módulo da área de transferência parou por um erro interno.";
/// O texto do estado sem pasta para o histórico.
const NO_DATA_DIR: &str = "Sem XDG_DATA_HOME nem HOME: não sei onde guardar o histórico.";
/// O texto do estado sem pasta para o histórico sem persistência.
const NO_RUNTIME_DIR: &str =
    "Sem XDG_RUNTIME_DIR: não sei onde guardar o histórico sem persistência.";

/// No modo jogo, quantas cópias esperam na memória (a mais velha cai).
pub const DEFERRED_MAX: usize = 50;

/// O item pedido não existe.
const ITEM_GONE: i32 = 1001;
/// A imagem do `copyImage` não pôde ser lida.
const IMAGE_UNREADABLE: i32 = 1002;
/// Sem histórico ou sem sessão possível.
const UNAVAILABLE: i32 = 1003;

/// De onde o módulo observa e onde guarda.
pub struct ClipboardConfig {
    pub backend: Arc<dyn Backend>,
    /// A pasta do histórico no disco (`$XDG_DATA_HOME/lucerna/clipboard`).
    pub data_dir: Option<PathBuf>,
    /// O `XDG_RUNTIME_DIR`: o socket do Wayland e o histórico sem
    /// persistência (`lucerna/clipboard/`).
    pub runtime_dir: Option<PathBuf>,
    /// O `WAYLAND_DISPLAY` do ambiente do daemon; vence o do `configure`.
    pub display_env: Option<String>,
    /// Quanto esperar o primeiro `configure`.
    pub configure_wait: Duration,
    /// Quanto esperar para tentar de novo depois de uma falha do compositor.
    pub retry: Duration,
    /// O sinal do modo jogo: ligado, as cópias lidas esperam na memória
    /// (até [`DEFERRED_MAX`]), sem miniatura, sem banco e sem publicar.
    pub game: GameSignal,
}

impl ClipboardConfig {
    /// A máquina de verdade: o Wayland do ambiente e as pastas do usuário.
    pub fn system() -> ClipboardConfig {
        let env = Env::from_process();
        ClipboardConfig {
            backend: Arc::new(Wayland::system()),
            data_dir: AppPaths::new(APP, &env)
                .ok()
                .and_then(|paths| paths.data_dir().ok()),
            runtime_dir: env.runtime_dir().ok(),
            display_env: std::env::var("WAYLAND_DISPLAY")
                .ok()
                .filter(|d| !d.is_empty()),
            configure_wait: CONFIGURE_WAIT,
            retry: RETRY,
            game: GameSignal::never(),
        }
    }
}

/// Os tópicos do módulo.
#[derive(Clone)]
pub struct ClipboardTopics {
    /// O comum `clipboard.history`: `{revision, count, pinned}`; `null` até
    /// o banco abrir.
    pub history: Topic,
    /// O comum `clipboard.status`: `{available, error}`.
    pub status: Topic,
}

/// O módulo registrado, pronto para rodar.
pub struct Clipboard {
    pub topics: ClipboardTopics,
    ctx: Ctx,
    /// A única alça forte do histórico: o laço a leva, e o histórico (com os
    /// arquivos sem persistência) cai junto com ele.
    db: Arc<Db>,
    backend: Arc<dyn Backend>,
    retry: Duration,
    work: WorkTracker,
    copies: mpsc::UnboundedReceiver<CopyRequest>,
}

/// O histórico aberto (depois do primeiro `configure`).
type Db = Mutex<Option<Store>>;

/// O que o `configure` pediu.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Settings {
    limit: usize,
    persist: bool,
    /// O `waylandDisplay`, que só vale sem o do ambiente.
    display: Option<String>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            limit: DEFAULT_LIMIT,
            persist: true,
            display: None,
        }
    }
}

/// O conteúdo do `clipboard.status`: o banco e a conexão.
#[derive(Default)]
struct Status {
    /// O banco não abriu, com este texto.
    store_error: Option<String>,
    /// A última abertura da sessão; `None` antes da primeira.
    wayland: Option<Result<(), String>>,
}

impl Status {
    fn to_json(&self) -> Value {
        let (available, error) = match (&self.store_error, &self.wayland) {
            (Some(e), _) => (false, Some(e.clone())),
            (None, Some(Ok(()))) => (true, None),
            (None, Some(Err(e))) => (false, Some(e.clone())),
            (None, None) => (false, None),
        };
        json!({ "available": available, "error": error })
    }
}

/// Um pedido ao laço para virar dono da seleção.
struct CopyRequest {
    source: Source,
    /// O item da fonte (`None` na imagem que não foi guardada).
    own: Option<i64>,
    /// `Err` com o texto do estado quando não há sessão possível.
    reply: oneshot::Sender<Result<(), String>>,
}

/// O que os métodos e o laço dividem.
struct Shared {
    topics: ClipboardTopics,
    data_dir: Option<PathBuf>,
    runtime_dir: Option<PathBuf>,
    display_env: Option<String>,
    configure_wait: Duration,
    /// A configuração aplicada; `None` antes do primeiro `configure`.
    settings: watch::Sender<Option<Settings>>,
    /// Um `configure` (ou o padrão) por vez.
    configuring: tokio::sync::Mutex<()>,
    /// Sobe a cada mudança do histórico.
    revision: AtomicU64,
    status: Mutex<Status>,
    copies: mpsc::UnboundedSender<CopyRequest>,
    /// A vez de preparar uma imagem (uma por vez).
    images: Mutex<()>,
    /// O modo jogo.
    game: GameSignal,
    /// As cópias lidas durante o modo jogo, da mais velha para a mais nova.
    deferred: Mutex<VecDeque<NewClip>>,
}

impl Shared {
    /// Onde o histórico mora com `persist`.
    fn origin(&self, persist: bool) -> Result<Origin, String> {
        let disk = self.data_dir.clone().ok_or(NO_DATA_DIR)?;
        if persist {
            return Ok(Origin::Disk(disk));
        }
        let run = self.runtime_dir.as_ref().ok_or(NO_RUNTIME_DIR)?;
        Ok(Origin::Memory {
            files: run.join("lucerna").join(APP),
            disk,
        })
    }

    /// Publica o `clipboard.history` com uma `revision` nova (chame com o
    /// lock do histórico, para as revisões saírem na ordem das mudanças).
    fn publish_history(&self, store: &Store) {
        match store.counts() {
            Ok((count, pinned)) => {
                let revision = self.revision.fetch_add(1, Ordering::Relaxed) + 1;
                self.topics.history.publish(json!({
                    "revision": revision,
                    "count": count,
                    "pinned": pinned,
                }));
            }
            Err(e) => eprintln!("lucernad: não consegui contar o histórico: {e}"),
        }
    }

    /// Muda o estado por `f` e o publica se mudou.
    fn update_status(&self, f: impl FnOnce(&mut Status)) {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut status);
        publish_changed(&self.topics.status, status.to_json());
    }

    /// O erro `1003` com o texto do estado.
    fn unavailable(&self) -> RpcError {
        let error = self.topics.status.current().data["error"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| "o histórico não está aberto".to_string());
        unavailable(&error)
    }

    /// Aplica a configuração `wanted` (bloqueia: chame fora do runtime) e
    /// devolve a que valeu. O primeiro abre o histórico; os seguintes trocam
    /// a persistência e o limite.
    fn apply(&self, db: &Db, old: Option<Settings>, wanted: Settings) -> Settings {
        let mut slot = lock(db);
        let mut applied = wanted.clone();
        let changed = match slot.as_mut() {
            None => {
                let opened = self.origin(wanted.persist).and_then(|origin| {
                    Store::open(origin, wanted.limit)
                        .map_err(|e| format!("Não consegui abrir o histórico: {e}."))
                });
                match opened {
                    Ok(store) => {
                        *slot = Some(store);
                        self.update_status(|s| s.store_error = None);
                        true
                    }
                    Err(e) => {
                        eprintln!("lucernad: {e}");
                        self.update_status(|s| s.store_error = Some(e));
                        false
                    }
                }
            }
            Some(store) => {
                let old = old.unwrap_or_else(|| wanted.clone());
                let mut changed = false;
                if old.persist != wanted.persist {
                    match self.move_store(store, wanted.persist) {
                        Ok(()) => changed = true,
                        Err(e) => {
                            eprintln!("lucernad: não consegui trocar a persistência: {e}");
                            applied.persist = old.persist;
                        }
                    }
                }
                if old.limit != wanted.limit {
                    match store.set_limit(wanted.limit) {
                        Ok(()) => changed = true,
                        Err(e) => eprintln!("lucernad: não consegui aplicar o limite: {e}"),
                    }
                }
                changed
            }
        };
        if changed && let Some(store) = slot.as_ref() {
            self.publish_history(store);
        }
        applied
    }

    /// Leva o histórico para o lado de `persist`. Um `Err` com o histórico já
    /// do outro lado (sobrou algo no disco ao ir para a memória) conta como
    /// trocado: nunca volta a gravar no disco.
    fn move_store(&self, store: &mut Store, persist: bool) -> Result<(), String> {
        let origin = self.origin(persist)?;
        match store.move_to(origin.clone()) {
            Err(e) if *store.origin() == origin => {
                eprintln!("lucernad: a troca da persistência deixou restos: {e}");
                Ok(())
            }
            moved => moved,
        }
    }
}

/// O texto do erro `1003`.
fn unavailable(error: &str) -> RpcError {
    RpcError::app(
        UNAVAILABLE,
        format!(
            "A área de transferência está indisponível: {}.",
            error.trim_end_matches('.')
        ),
    )
}

/// O módulo parou (o laço caiu ou foi largado).
fn stopped() -> RpcError {
    unavailable(PANIC_ERROR)
}

/// Um erro do banco: vai ao log e volta como erro interno.
fn internal(e: String) -> RpcError {
    eprintln!("lucernad: o histórico falhou: {e}");
    RpcError::internal()
}

fn gone(id: i64) -> RpcError {
    RpcError::app(ITEM_GONE, format!("O item {id} não existe mais."))
}

fn lock(db: &Db) -> MutexGuard<'_, Option<Store>> {
    // Um pânico com o lock na mão não derruba os pedidos seguintes.
    db.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// O resultado de uma tarefa de bloqueio; o pânico dela segue adiante.
async fn join<T>(handle: JoinHandle<T>) -> T {
    match handle.await {
        Ok(value) => value,
        Err(e) => resume_unwind(e.into_panic()),
    }
}

/// O que os métodos e o laço usam: o comum e o histórico, que pode já ter
/// caído junto com o laço.
#[derive(Clone)]
struct Ctx {
    shared: Arc<Shared>,
    db: Weak<Db>,
}

impl Ctx {
    /// Aplica a configuração (com `only_first`, só se ainda não houver uma).
    async fn configure(&self, wanted: Settings, only_first: bool) -> Result<(), RpcError> {
        let _one = self.shared.configuring.lock().await;
        let old = self.shared.settings.borrow().clone();
        if only_first && old.is_some() {
            return Ok(());
        }
        let db = self.db.upgrade().ok_or_else(stopped)?;
        let shared = self.shared.clone();
        let applied = join(tokio::task::spawn_blocking(move || {
            shared.apply(&db, old, wanted)
        }))
        .await;
        self.shared.settings.send_replace(Some(applied));
        Ok(())
    }

    /// Espera o primeiro `configure` até o `configure_wait`; depois, aplica
    /// o padrão.
    async fn ensure_configured(&self) -> Result<(), RpcError> {
        if self.shared.settings.borrow().is_some() {
            return Ok(());
        }
        let mut rx = self.shared.settings.subscribe();
        let configured = async move {
            let _ = rx.wait_for(Option::is_some).await.map(drop);
        };
        if tokio::time::timeout(self.shared.configure_wait, configured)
            .await
            .is_err()
        {
            // Numa tarefa própria: quem espera pode desistir (o laço atende
            // uma cópia no meio) sem cortar o `configure` ao meio.
            let ctx = self.clone();
            let default =
                tokio::spawn(async move { ctx.configure(Settings::default(), true).await });
            default.await.map_err(|_| stopped())??;
        }
        Ok(())
    }

    /// Roda `f` com o histórico aberto, numa thread de bloqueio.
    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Shared, &mut Store) -> Result<T, RpcError> + Send + 'static,
    ) -> Result<T, RpcError> {
        let shared = self.shared.clone();
        let db = self.db.clone();
        join(tokio::task::spawn_blocking(move || {
            let db = db.upgrade().ok_or_else(stopped)?;
            let mut slot = lock(&db);
            let store = slot.as_mut().ok_or_else(|| shared.unavailable())?;
            f(&shared, store)
        }))
        .await
    }

    /// O [`Ctx::blocking`] depois da configuração.
    async fn with_store<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Shared, &mut Store) -> Result<T, RpcError> + Send + 'static,
    ) -> Result<T, RpcError> {
        self.ensure_configured().await?;
        self.blocking(f).await
    }

    /// O trabalho pesado de uma cópia, sem o lock (bloqueia); `None` sem
    /// histórico aberto.
    fn prepare_clip(&self, clip: NewClip) -> Result<Option<Prepared>, String> {
        let Some(db) = self.db.upgrade() else {
            return Ok(None);
        };
        let Some(files_dir) = lock(&db).as_ref().map(Store::files_dir) else {
            return Ok(None);
        };
        drop(db);
        // Uma imagem por vez: a decodificação da miniatura pode pedir centenas
        // de MiB, e uma rajada de cópias não multiplica isso.
        let _one = matches!(clip, NewClip::Image { .. })
            .then(|| self.shared.images.lock().unwrap_or_else(|e| e.into_inner()));
        prepare(clip, &files_dir).map(Some)
    }

    /// Grava a cópia preparada e publica o histórico (bloqueia); o id, ou
    /// `None` sem histórico aberto.
    fn insert_prepared(&self, p: &Prepared, now: i64) -> Result<Option<i64>, String> {
        let Some(db) = self.db.upgrade() else {
            return Ok(None);
        };
        let mut slot = lock(&db);
        let Some(store) = slot.as_mut() else {
            return Ok(None);
        };
        let (id, _) = store.insert_ref(p, now)?;
        self.shared.publish_history(store);
        Ok(Some(id))
    }

    /// Grava as cópias preparadas na ordem, como se chegassem agora, e
    /// publica o histórico uma vez (bloqueia).
    fn insert_all(&self, ps: &[Prepared], now: i64) -> Result<(), String> {
        let Some(db) = self.db.upgrade() else {
            return Ok(());
        };
        let mut slot = lock(&db);
        let Some(store) = slot.as_mut() else {
            return Ok(());
        };
        for (i, p) in ps.iter().enumerate() {
            store.insert_ref(p, now + i as i64)?;
        }
        if !ps.is_empty() {
            self.shared.publish_history(store);
        }
        Ok(())
    }

    /// Guarda uma cópia do modo jogo na fila (a mais velha cai além de
    /// [`DEFERRED_MAX`]).
    fn defer(&self, clip: NewClip) {
        let mut q = self
            .shared
            .deferred
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if q.len() == DEFERRED_MAX {
            q.pop_front();
            eprintln!("lucernad: o modo jogo descartou uma cópia antiga da fila");
        }
        q.push_back(clip);
    }

    /// Pede ao laço para virar dono da seleção com `source`.
    async fn offer(&self, source: Source, own: Option<i64>) -> Result<(), RpcError> {
        let (reply, answer) = oneshot::channel();
        self.shared
            .copies
            .send(CopyRequest { source, own, reply })
            .map_err(|_| stopped())?;
        answer
            .await
            .map_err(|_| stopped())?
            .map_err(|e| unavailable(&e))
    }

    async fn list(self, p: Value) -> Result<Value, RpcError> {
        let kind = match field(&p, "kind") {
            None => None,
            Some(v) => Some(
                v.as_str()
                    .and_then(Kind::parse)
                    .ok_or_else(|| bad("kind precisa ser text, image ou files"))?,
            ),
        };
        let query = match field(&p, "query") {
            None => None,
            Some(v) => Some(
                v.as_str()
                    .filter(|q| q.chars().count() <= QUERY_MAX)
                    .ok_or_else(|| bad("query precisa ser um texto de até 1000 caracteres"))?
                    .to_string(),
            ),
        };
        let offset = field(&p, "offset")
            .and_then(Value::as_u64)
            .ok_or_else(|| bad("offset precisa ser um inteiro a partir de 0"))?;
        let limit = field(&p, "limit")
            .and_then(Value::as_u64)
            .filter(|l| (1..=LIST_MAX as u64).contains(l))
            .ok_or_else(|| bad("limit precisa ser um inteiro entre 1 e 200"))?;
        let q = ListQuery {
            kind,
            query,
            offset: offset.try_into().unwrap_or(usize::MAX),
            limit: limit as usize,
        };
        let page = self
            .with_store(move |_, store| store.list(&q).map_err(internal))
            .await?;
        Ok(json!({ "items": page.items, "total": page.total }))
    }

    async fn get(self, p: Value) -> Result<Value, RpcError> {
        let id = id_of(&p)?;
        // O texto vem cortado em `GET_TEXT_MAX`, com `truncated` (a resposta
        // cabe numa mensagem; o shell nunca precisa do texto inteiro).
        self.with_store(move |_, store| store.get(id).map_err(internal)?.ok_or_else(|| gone(id)))
            .await
    }

    async fn copy(self, p: Value) -> Result<Value, RpcError> {
        let id = id_of(&p)?;
        let (kind, mime, data) = self
            .with_store(move |_, store| store.locate(id).map_err(internal)?.ok_or_else(|| gone(id)))
            .await?;
        // O arquivo da imagem é lido sem o lock.
        let data = join(tokio::task::spawn_blocking(move || data.read()))
            .await
            .map_err(internal)?;
        let source = Source {
            offers: offers_for(kind, mime.as_deref(), &data, id),
        };
        self.offer(source, Some(id)).await?;
        self.blocking(move |shared, store| {
            if store.touch(id, now_ms()).map_err(internal)? {
                shared.publish_history(store);
            }
            Ok(())
        })
        .await?;
        Ok(json!({}))
    }

    async fn remove(self, p: Value) -> Result<Value, RpcError> {
        let id = id_of(&p)?;
        self.with_store(move |shared, store| {
            if store.remove(id).map_err(internal)? {
                shared.publish_history(store);
            }
            Ok(json!({}))
        })
        .await
    }

    async fn pin(self, p: Value) -> Result<Value, RpcError> {
        let id = id_of(&p)?;
        let pinned = field(&p, "pinned")
            .and_then(Value::as_bool)
            .ok_or_else(|| bad("pinned precisa ser verdadeiro ou falso"))?;
        self.with_store(move |shared, store| {
            if store.pin(id, pinned).map_err(internal)? {
                shared.publish_history(store);
            }
            Ok(json!({}))
        })
        .await
    }

    async fn clear(self, p: Value) -> Result<Value, RpcError> {
        let all = optional_flag(&p, "all", "all precisa ser verdadeiro ou falso")?.unwrap_or(false);
        self.with_store(move |shared, store| {
            if store.clear(all).map_err(internal)? > 0 {
                shared.publish_history(store);
            }
            Ok(json!({}))
        })
        .await
    }

    async fn copy_image(self, p: Value) -> Result<Value, RpcError> {
        let path = field(&p, "path")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| bad("path precisa ser um caminho absoluto"))?;
        let keep =
            optional_flag(&p, "store", "store precisa ser verdadeiro ou falso")?.unwrap_or(true);
        let (mime, bytes) = join(tokio::task::spawn_blocking(move || read_image(&path)))
            .await
            .map_err(|e| {
                RpcError::app(IMAGE_UNREADABLE, format!("Não consegui ler a imagem: {e}."))
            })?;
        if !keep {
            let source = Source {
                offers: vec![(mime, bytes.into())],
            };
            self.offer(source, None).await?;
            return Ok(json!({ "id": 0 }));
        }
        self.ensure_configured().await?;
        let ctx = self.clone();
        let clip = NewClip::Image {
            mime: mime.clone(),
            bytes,
        };
        let now = now_ms();
        let stored = join(tokio::task::spawn_blocking(move || {
            let Some(p) = ctx.prepare_clip(clip).map_err(internal)? else {
                return Ok(None);
            };
            let id = ctx.insert_prepared(&p, now).map_err(internal)?;
            // Os mesmos bytes servem a seleção, sem outra cópia.
            Ok(id.zip(p.into_image()))
        }))
        .await?;
        let (id, bytes) = stored.ok_or_else(|| self.shared.unavailable())?;
        let source = Source {
            offers: offers_for(Kind::Image, Some(&mime), &bytes, id),
        };
        self.offer(source, Some(id)).await?;
        Ok(json!({ "id": id }))
    }

    async fn configure_method(self, p: Value) -> Result<Value, RpcError> {
        let limit = field(&p, "limit")
            .and_then(Value::as_u64)
            .filter(|l| (1..=MAX_LIMIT as u64).contains(l))
            .ok_or_else(|| bad("limit precisa ser um inteiro entre 1 e 1000"))?;
        let persist = field(&p, "persist")
            .and_then(Value::as_bool)
            .ok_or_else(|| bad("persist precisa ser verdadeiro ou falso"))?;
        let display = match field(&p, "waylandDisplay") {
            None => None,
            Some(v) => Some(
                v.as_str()
                    .ok_or_else(|| bad("waylandDisplay precisa ser um texto"))?
                    .to_string(),
            ),
        };
        let wanted = Settings {
            limit: limit as usize,
            persist,
            display,
        };
        self.configure(wanted, false).await?;
        Ok(json!({}))
    }
}

/// O campo `key` dos parâmetros; `null` conta como ausente.
fn field<'a>(p: &'a Value, key: &str) -> Option<&'a Value> {
    p.get(key).filter(|v| !v.is_null())
}

fn bad(text: &str) -> RpcError {
    RpcError::invalid_params(text)
}

fn id_of(p: &Value) -> Result<i64, RpcError> {
    field(p, "id")
        .and_then(Value::as_i64)
        .ok_or_else(|| bad("id precisa ser o número de um item"))
}

fn optional_flag(p: &Value, key: &str, text: &str) -> Result<Option<bool>, RpcError> {
    match field(p, key) {
        None => Ok(None),
        Some(v) => v.as_bool().map(Some).ok_or_else(|| bad(text)),
    }
}

/// Lê uma imagem do disco (até [`IMAGE_MAX`]), com o mime pela assinatura
/// dos bytes (a extensão pode mentir).
fn read_image(path: &Path) -> Result<(String, Vec<u8>), String> {
    let shown = path.display();
    // Sem bloquear na abertura (um FIFO prenderia a chamada); só o arquivo
    // comum é lido.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(OFlags::NONBLOCK.bits() as i32)
        .open(path)
        .map_err(|e| format!("{shown}: {e}"))?;
    let regular = file
        .metadata()
        .map_err(|e| format!("{shown}: {e}"))?
        .is_file();
    if !regular {
        return Err(format!("{shown} não é um arquivo comum"));
    }
    let mut bytes = Vec::new();
    file.take(IMAGE_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{shown}: {e}"))?;
    if bytes.len() > IMAGE_MAX {
        return Err(format!("{shown} passa de {} MiB", IMAGE_MAX >> 20));
    }
    let mime = sniff(&bytes).ok_or_else(|| format!("{shown} não é uma imagem"))?;
    Ok((mime.to_string(), bytes))
}

/// Declara um método que recebe o [`Ctx`] e os parâmetros (`null` sem eles).
fn method<F, Fut>(server: &mut ServerBuilder, name: &str, ctx: &Ctx, f: F)
where
    F: Fn(Ctx, Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    let ctx = ctx.clone();
    server.method(name, move |params| {
        f(ctx.clone(), params.unwrap_or(Value::Null))
    });
}

/// Declara os tópicos `clipboard.history` e `clipboard.status` e os oito
/// métodos.
pub fn register(server: &mut ServerBuilder, cfg: ClipboardConfig, work: WorkTracker) -> Clipboard {
    let topics = ClipboardTopics {
        history: server.topic("clipboard.history", Value::Null),
        status: server.topic("clipboard.status", Status::default().to_json()),
    };
    let (copies_tx, copies) = mpsc::unbounded_channel();
    let shared = Arc::new(Shared {
        topics: topics.clone(),
        data_dir: cfg.data_dir,
        runtime_dir: cfg.runtime_dir,
        display_env: cfg.display_env,
        configure_wait: cfg.configure_wait,
        settings: watch::Sender::new(None),
        configuring: tokio::sync::Mutex::new(()),
        revision: AtomicU64::new(0),
        status: Mutex::new(Status::default()),
        copies: copies_tx,
        images: Mutex::new(()),
        game: cfg.game.clone(),
        deferred: Mutex::new(VecDeque::new()),
    });
    let db = Arc::new(Mutex::new(None));
    let ctx = Ctx {
        shared,
        db: Arc::downgrade(&db),
    };
    method(server, "clipboard.list", &ctx, Ctx::list);
    method(server, "clipboard.get", &ctx, Ctx::get);
    method(server, "clipboard.copy", &ctx, Ctx::copy);
    method(server, "clipboard.remove", &ctx, Ctx::remove);
    method(server, "clipboard.pin", &ctx, Ctx::pin);
    method(server, "clipboard.clear", &ctx, Ctx::clear);
    method(server, "clipboard.copyImage", &ctx, Ctx::copy_image);
    method(server, "clipboard.configure", &ctx, Ctx::configure_method);
    Clipboard {
        topics,
        ctx,
        db,
        backend: cfg.backend,
        retry: cfg.retry,
        work,
        copies,
    }
}

impl Clipboard {
    /// Roda o laço do módulo e o supervisor (chame dentro do runtime). Se o
    /// laço entrar em pânico, o `clipboard.status` fica indisponível; o
    /// servidor continua atendendo. Largar o [`ModuleTask`] aborta o laço, e
    /// o histórico sem persistência leva os arquivos junto.
    pub fn spawn(self) -> ModuleTask {
        let status = self.topics.status.clone();
        let game = self.ctx.shared.game.clone();
        let module = Module {
            settings: self.ctx.shared.settings.subscribe(),
            ctx: self.ctx,
            _db: self.db,
            backend: self.backend,
            retry: self.retry,
            work: self.work,
            copies: self.copies,
            game,
            parked: None,
            owned: None,
            defaulting: false,
            echoes: VecDeque::new(),
            last_read: None,
        };
        let run = tokio::spawn(run(module));
        lucernad_core::supervise(
            run,
            status,
            "da área de transferência",
            json!({ "available": false, "error": PANIC_ERROR }),
        )
    }
}

/// A seleção nossa: o item dela e o trabalho que segura o daemon.
struct Owned {
    id: Option<i64>,
    _work: WorkGuard,
}

/// Por que uma sessão terminou.
enum Exit {
    /// Sem assinante e sem seleção nossa, ou a exibição mudou.
    Idle,
    /// A conexão caiu, com este texto: espera o `retry` e tenta de novo.
    Dropped(String),
}

/// O estado do laço que atravessa as sessões.
struct Module {
    ctx: Ctx,
    /// Segura o histórico enquanto o laço viver.
    _db: Arc<Db>,
    backend: Arc<dyn Backend>,
    retry: Duration,
    work: WorkTracker,
    copies: mpsc::UnboundedReceiver<CopyRequest>,
    settings: watch::Receiver<Option<Settings>>,
    /// O modo jogo (para processar a fila ao sair).
    game: GameSignal,
    /// Um pedido de cópia recebido fora da sessão, para a próxima.
    parked: Option<CopyRequest>,
    owned: Option<Owned>,
    /// O padrão já foi pedido (uma vez na vida do módulo).
    defaulting: bool,
    /// Os tipos (em ordem) de cada fonte nossa ainda sem eco, da mais velha
    /// para a mais nova; só valem enquanto a seleção for nossa.
    echoes: VecDeque<Vec<String>>,
    /// Fecha quando a última leitura gravou (ou desistiu): a ordem das cópias.
    last_read: Option<oneshot::Receiver<()>>,
}

impl Module {
    fn shared(&self) -> &Shared {
        &self.ctx.shared
    }

    /// O histórico abriu (o `history` deixa de ser `null`).
    fn store_open(&self) -> bool {
        !self.shared().topics.history.current().data.is_null()
    }

    /// O socket da exibição: a do ambiente vence a do `configure`.
    fn socket(&mut self) -> Result<PathBuf, String> {
        let configured = self
            .settings
            .borrow_and_update()
            .as_ref()
            .and_then(|s| s.display.clone());
        let shared = self.shared();
        let display = shared
            .display_env
            .clone()
            .or(configured)
            .unwrap_or_default();
        socket_path(&display, shared.runtime_dir.as_deref())
    }

    fn wayland(&self, result: Result<(), String>) {
        self.shared().update_status(|s| s.wayland = Some(result));
    }

    /// Espera haver o que fazer: um pedido de cópia (devolvido) ou um
    /// assinante com o histórico aberto (`None`).
    async fn wait_for_work(&mut self) -> Option<CopyRequest> {
        let history = self.shared().topics.history.clone();
        loop {
            if let Some(request) = self.parked.take() {
                return Some(request);
            }
            self.settings.borrow_and_update();
            let subscribed = history.subscribers() > 0;
            if subscribed {
                self.start_default();
                if self.store_open() {
                    return None;
                }
            }
            tokio::select! {
                _ = history.wait_subscribed(), if !subscribed => {}
                _ = self.settings.changed() => {}
                request = self.copies.recv() => self.parked = request,
                () = self.game.changed() => self.game_changed(),
            }
        }
    }

    /// Sem `configure`, o padrão depois da espera, numa tarefa própria: uma
    /// cópia pedida no meio (o `copyImage` sem guardar) é atendida já, e a
    /// espera não é largada por isso.
    fn start_default(&mut self) {
        if self.defaulting || self.settings.borrow().is_some() {
            return;
        }
        self.defaulting = true;
        let ctx = self.ctx.clone();
        tokio::spawn(async move {
            let _ = ctx.ensure_configured().await;
        });
    }

    /// Espera uma mudança da configuração, um pedido de cópia ou o `retry`.
    async fn wait(&mut self, retry: Option<Duration>) {
        let sleep = async move {
            match retry {
                Some(d) => tokio::time::sleep(d).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = sleep => {}
            _ = self.settings.changed() => {}
            request = self.copies.recv() => self.parked = request,
            () = self.game.changed() => self.game_changed(),
        }
    }

    /// Abre uma sessão; o resultado também vai ao `status`.
    async fn open(&mut self, socket: &Path) -> Result<Session, String> {
        let backend = self.backend.clone();
        let socket = socket.to_path_buf();
        let opened = match detached(move || backend.open(&socket), OPEN_TIMEOUT).await {
            Detached::Done(result) => result,
            Detached::TimedOut => Err(NOT_RESPONDING.to_string()),
            Detached::SpawnFailed(e) => {
                Err(format!("Não consegui criar a thread do Wayland: {e}."))
            }
            Detached::Panicked(p) => resume_unwind(p),
        };
        match &opened {
            Ok(_) => self.wayland(Ok(())),
            Err(e) => {
                eprintln!("lucernad: {e}");
                self.wayland(Err(e.clone()));
            }
        }
        opened
    }

    /// O laço de uma sessão aberta, até ela terminar.
    async fn session(
        &mut self,
        session: Session,
        socket: PathBuf,
        first: Option<CopyRequest>,
    ) -> Exit {
        let Session {
            mut events,
            control,
        } = session;
        let history = self.shared().topics.history.clone();
        if let Some(request) = first {
            self.serve(&*control, request);
        }
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Some(Event::Selection { offer, mimes }) => {
                        self.selection(&*control, offer, mimes);
                    }
                    Some(Event::Cancelled) => self.release(),
                    Some(Event::Cleared) => {}
                    Some(Event::Disconnected(why)) => return Exit::Dropped(why),
                    None => return Exit::Dropped(disconnected("a sessão terminou")),
                },
                request = self.copies.recv() => {
                    if let Some(request) = request {
                        self.serve(&*control, request);
                    }
                }
                _ = self.settings.changed() => {
                    if self.socket().ok().as_ref() != Some(&socket) {
                        return Exit::Idle;
                    }
                }
                _ = history.wait_unsubscribed(), if self.owned.is_none() => return Exit::Idle,
                () = self.game.changed() => self.game_changed(),
                // Um assinante chegou numa sessão aberta só pela seleção nossa.
                _ = history.wait_subscribed(), if !self.defaulting && self.settings.borrow().is_none() => {
                    self.start_default();
                }
            }
        }
    }

    /// O modo jogo mudou: ao sair, as cópias da fila vão ao histórico, na
    /// ordem e depois das leituras em voo.
    fn game_changed(&mut self) {
        if self.game.is_on() {
            return;
        }
        let previous = self.last_read.take();
        let (done, last) = oneshot::channel::<()>();
        self.last_read = Some(last);
        let ctx = self.ctx.clone();
        tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            flush_deferred(ctx).await;
            drop(done);
        });
    }

    /// A seleção deixou de ser nossa: o trabalho e os ecos esperados saem.
    fn release(&mut self) {
        self.owned = None;
        self.echoes.clear();
    }

    /// Vira dono da seleção com a fonte do pedido.
    fn serve(&mut self, control: &dyn Control, request: CopyRequest) {
        let mimes = sorted(request.source.offers.iter().map(|(m, _)| m.clone()));
        if self.echoes.len() == MAX_ECHOES {
            self.echoes.pop_front();
        }
        self.echoes.push_back(mimes);
        control.offer(request.source);
        self.owned = Some(Owned {
            id: request.own,
            _work: self.work.hold(),
        });
        let _ = request.reply.send(Ok(()));
    }

    /// Uma seleção nova: o eco da nossa fonte é pulado; o resto, pela
    /// escolha do tipo.
    fn selection(&mut self, control: &dyn Control, offer: u64, mimes: Vec<String>) {
        // O eco de uma fonte nossa (e o das mais velhas, que não vêm mais).
        // Uma seleção alheia no meio não mexe na fila.
        if self.owned.is_some() {
            let these = sorted(mimes.iter().cloned());
            if let Some(at) = self.echoes.iter().position(|ours| *ours == these) {
                self.echoes.drain(..=at);
                return;
            }
        }
        // Sem assinante (a sessão só segura a seleção nossa) e antes do
        // `configure`, nada é lido: o histórico pode estar desligado.
        if self.shared().topics.history.subscribers() == 0 || !self.store_open() {
            return;
        }
        match choose(&mimes) {
            Choice::Secret => eprintln!("lucernad: cópia sensível ignorada"),
            Choice::Own => {
                if let Some(id) = self.owned.as_ref().and_then(|o| o.id) {
                    let ctx = self.ctx.clone();
                    tokio::spawn(async move {
                        let touched = ctx
                            .blocking(move |shared, store| {
                                if store.touch(id, now_ms()).map_err(internal)? {
                                    shared.publish_history(store);
                                }
                                Ok(())
                            })
                            .await;
                        if let Err(e) = touched {
                            eprintln!("lucernad: não consegui subir o item {id}: {}", e.message);
                        }
                    });
                }
            }
            Choice::Read { kind, mime } => self.read(control, offer, kind, mime),
            Choice::Nothing => {}
        }
    }

    /// Lê uma cópia fora do laço e a grava na ordem das cópias.
    fn read(&mut self, control: &dyn Control, offer: u64, kind: Kind, mime: String) {
        let reader = match control.receive(offer, &mime) {
            Ok(reader) => reader,
            Err(e) => {
                eprintln!("lucernad: não consegui pedir a cópia ({mime}): {e}");
                return;
            }
        };
        let now = now_ms();
        let deadline = Instant::now() + READ_TIMEOUT;
        let previous = self.last_read.take();
        let (done, last) = oneshot::channel::<()>();
        self.last_read = Some(last);
        let ctx = self.ctx.clone();
        tokio::spawn(async move {
            let job_ctx = ctx.clone();
            let job_mime = mime.clone();
            let got = tokio::task::spawn_blocking(move || {
                let bytes = read_pipe(reader, kind.max_size(), deadline).map_err(read_error)?;
                match clip_of(kind, job_mime, bytes) {
                    // O modo jogo: sem o trabalho pesado agora.
                    Some(clip) if job_ctx.shared.game.is_on() => Ok(Some(Got::Deferred(clip))),
                    Some(clip) => job_ctx.prepare_clip(clip).map(|p| p.map(Got::Ready)),
                    None => Ok(None),
                }
            })
            .await;
            // Uma cópia mais nova espera a anterior gravar (ou desistir).
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let inserted = match got {
                Ok(Ok(Some(Got::Ready(p)))) => {
                    let ctx = ctx.clone();
                    tokio::task::spawn_blocking(move || ctx.insert_prepared(&p, now))
                        .await
                        .unwrap_or_else(|e| Err(format!("pânico: {e}")))
                        .map(drop)
                }
                Ok(Ok(Some(Got::Deferred(clip)))) => {
                    ctx.defer(clip);
                    // O modo jogo acabou no meio: a fila vai já.
                    if !ctx.shared.game.is_on() {
                        flush_deferred(ctx.clone()).await;
                    }
                    Ok(())
                }
                Ok(Ok(None)) => Ok(()),
                Ok(Err(e)) => Err(e),
                Err(e) => Err(format!("pânico: {e}")),
            };
            if let Err(e) = inserted {
                eprintln!("lucernad: cópia ({mime}) descartada: {e}");
            }
            drop(done);
        });
    }
}

/// Uma cópia lida: pronta para o banco, ou guardada para depois do jogo.
enum Got {
    Ready(Prepared),
    Deferred(NewClip),
}

/// Processa a fila do modo jogo: prepara cada cópia, grava todas na ordem e
/// publica uma vez.
async fn flush_deferred(ctx: Ctx) {
    let clips: Vec<NewClip> = ctx
        .shared
        .deferred
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .drain(..)
        .collect();
    if clips.is_empty() {
        return;
    }
    let now = now_ms();
    let job = tokio::task::spawn_blocking(move || {
        let mut ready = Vec::new();
        for clip in clips {
            match ctx.prepare_clip(clip) {
                Ok(Some(p)) => ready.push(p),
                Ok(None) => {}
                Err(e) => eprintln!("lucernad: cópia do modo jogo descartada: {e}"),
            }
        }
        ctx.insert_all(&ready, now)
    })
    .await;
    match job {
        Ok(Ok(())) => {}
        Ok(Err(e)) => eprintln!("lucernad: as cópias do modo jogo não foram gravadas: {e}"),
        Err(e) => eprintln!("lucernad: as cópias do modo jogo não foram gravadas: pânico: {e}"),
    }
}

/// Os tipos em ordem, para comparar uma seleção com a nossa fonte.
fn sorted(mimes: impl Iterator<Item = String>) -> Vec<String> {
    let mut mimes: Vec<String> = mimes.collect();
    mimes.sort();
    mimes
}

fn read_error(e: ReadError) -> String {
    match e {
        ReadError::TooBig => "passou do limite".to_string(),
        ReadError::TimedOut => format!(
            "o app não terminou de mandar em {} s",
            READ_TIMEOUT.as_secs()
        ),
        ReadError::Io(e) => e,
    }
}

/// A cópia lida como item; `None` para o que não entra (texto só com
/// espaços, lista sem arquivos, imagem vazia).
fn clip_of(kind: Kind, mime: String, bytes: Vec<u8>) -> Option<NewClip> {
    let text = |bytes: Vec<u8>| {
        String::from_utf8(bytes)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
    };
    match kind {
        Kind::Text => {
            let text = text(bytes);
            (!text.trim().is_empty()).then_some(NewClip::Text(text))
        }
        Kind::Files => {
            let list = text(bytes);
            (!uri_lines(&list).is_empty()).then_some(NewClip::Files(list))
        }
        Kind::Image => (!bytes.is_empty()).then_some(NewClip::Image { mime, bytes }),
    }
}

/// O laço do módulo: enquanto houver assinante (com o histórico aberto) ou
/// seleção nossa, uma sessão com o compositor.
async fn run(mut m: Module) {
    loop {
        let pending = m.wait_for_work().await;
        let socket = match m.socket() {
            Ok(socket) => socket,
            Err(e) => {
                m.wayland(Err(e.clone()));
                if let Some(request) = pending {
                    let _ = request.reply.send(Err(e));
                }
                m.wait(None).await;
                continue;
            }
        };
        let session = match m.open(&socket).await {
            Ok(session) => session,
            Err(e) => {
                if let Some(request) = pending {
                    let _ = request.reply.send(Err(e));
                }
                m.wait(Some(m.retry)).await;
                continue;
            }
        };
        let exit = m.session(session, socket, pending).await;
        // A sessão já foi largada, e a seleção nossa com ela.
        m.release();
        if let Exit::Dropped(why) = exit {
            eprintln!("lucernad: {why}");
            m.wayland(Err(why));
            m.wait(Some(m.retry)).await;
        }
    }
}
