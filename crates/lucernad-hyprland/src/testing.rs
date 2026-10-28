//! Os dublês do Hyprland para os testes: o [`FakeHypr`], em memória (o
//! trait [`Hypr`] direto), e o [`FakeSockets`], um servidor dos dois sockets
//! numa pasta temporária (respostas roteirizadas, eventos injetados). Os dois
//! respondem o que não é roteirizado com o mesmo [`Model`]: as opções (o
//! `eval` do `hl.config` muda o que o `getoption` lê), as janelas, os
//! monitores e as propriedades das janelas.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, watch};
use tokio::task::AbortHandle;

use crate::clients::Window;
use crate::events::Event;
use crate::instance::{EVENTS_SOCKET, REQUEST_SOCKET};
use crate::options::OptionValue;
use crate::socket::HyprError;
use crate::{BoxFuture, Hypr};

/// As cinco opções do modo jogo com os valores conferidos no container
/// (Hyprland 0.56.2).
pub fn default_options() -> BTreeMap<String, OptionValue> {
    [
        ("decoration:blur:enabled", OptionValue::Bool(true)),
        ("decoration:shadow:enabled", OptionValue::Bool(true)),
        ("animations:enabled", OptionValue::Bool(true)),
        ("misc:vrr", OptionValue::Int(0)),
        ("general:allow_tearing", OptionValue::Bool(false)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

/// O estado de um Hyprland falso e as respostas aos comandos.
pub struct Model {
    pub options: BTreeMap<String, OptionValue>,
    pub windows: Vec<Window>,
    pub active_workspaces: Vec<i64>,
    /// As propriedades mudadas, na ordem: (endereço, propriedade, valor).
    pub props: Vec<(String, String, String)>,
    pub version: String,
}

impl Default for Model {
    fn default() -> Model {
        Model {
            options: default_options(),
            windows: Vec::new(),
            active_workspaces: vec![1],
            props: Vec::new(),
            version: "0.56.2".into(),
        }
    }
}

impl Model {
    /// A resposta a `command`, mudando o estado como o Hyprland faria.
    pub fn handle(&mut self, command: &str) -> String {
        if let Some(name) = command.strip_prefix("j/getoption ") {
            return match self.options.get(name) {
                Some(v) => {
                    let mut o = json!({"option": name});
                    let kv = serde_json::to_value(v).unwrap();
                    for (k, v) in kv.as_object().unwrap() {
                        o[k] = v.clone();
                    }
                    o["set"] = json!(true);
                    o.to_string()
                }
                None => "no such option".into(),
            };
        }
        if let Some(lua) = command.strip_prefix("eval ") {
            if let Some(changes) = parse_config_lua(lua) {
                for (k, v) in changes {
                    self.options.insert(k, v);
                }
                return "ok".into();
            }
            if let Some(p) = parse_set_prop(lua) {
                self.props.push(p);
                return "ok".into();
            }
            return format!("error: o Hyprland falso não entende: {lua}");
        }
        match command {
            "j/clients" => Value::Array(
                self.windows
                    .iter()
                    .map(|w| {
                        json!({"address": w.address, "mapped": w.mapped, "hidden": false,
                               "workspace": {"id": w.workspace, "name": w.workspace.to_string()},
                               "class": w.class, "title": w.title, "pid": w.pid,
                               "fullscreen": w.fullscreen, "fullscreenClient": w.fullscreen})
                    })
                    .collect(),
            )
            .to_string(),
            "j/monitors" => Value::Array(
                self.active_workspaces
                    .iter()
                    .enumerate()
                    .map(|(i, ws)| {
                        json!({"id": i, "name": format!("FAKE-{i}"),
                               "activeWorkspace": {"id": ws, "name": ws.to_string()},
                               "specialWorkspace": {"id": 0, "name": ""}})
                    })
                    .collect(),
            )
            .to_string(),
            "j/version" => {
                json!({"version": self.version, "tag": format!("v{}", self.version)}).to_string()
            }
            _ => "unknown request".into(),
        }
    }

    /// O valor de uma propriedade de janela, pela última mudança.
    pub fn prop(&self, address: &str, prop: &str) -> Option<String> {
        self.props
            .iter()
            .rev()
            .find(|(a, p, _)| a == address && p == prop)
            .map(|(_, _, v)| v.clone())
    }
}

/// As opções de um `hl.config({...})` (o Lua que o [`crate::options::config_lua`]
/// gera), achatadas com `:`; `None` se não for isso.
pub fn parse_config_lua(lua: &str) -> Option<Vec<(String, OptionValue)>> {
    let body = lua.trim().strip_prefix("hl.config(")?.strip_suffix(')')?;
    let mut p = Lua {
        s: body.chars().collect(),
        i: 0,
    };
    let mut out = Vec::new();
    p.table("", &mut out)?;
    p.ws();
    (p.i == p.s.len()).then_some(out)
}

/// O (endereço, propriedade, valor) de um `hl.dispatch(hl.dsp.window.set_prop({...}))`.
fn parse_set_prop(lua: &str) -> Option<(String, String, String)> {
    let body = lua
        .trim()
        .strip_prefix("hl.dispatch(hl.dsp.window.set_prop(")?
        .strip_suffix("))")?;
    let mut p = Lua {
        s: body.chars().collect(),
        i: 0,
    };
    let mut fields = Vec::new();
    p.table("", &mut fields)?;
    let get = |k: &str| {
        fields
            .iter()
            .find(|(n, _)| n == k)
            .and_then(|(_, v)| match v {
                OptionValue::Str(s) => Some(s.clone()),
                _ => None,
            })
    };
    let window = get("window")?;
    Some((
        window.strip_prefix("address:")?.to_string(),
        get("prop")?,
        get("value")?,
    ))
}

/// Um leitor mínimo do Lua das tabelas de configuração.
struct Lua {
    s: Vec<char>,
    i: usize,
}

impl Lua {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_whitespace() {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: char) -> bool {
        self.ws();
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn ident(&mut self) -> Option<String> {
        self.ws();
        let start = self.i;
        while self.i < self.s.len() && (self.s[self.i].is_alphanumeric() || self.s[self.i] == '_') {
            self.i += 1;
        }
        (self.i > start).then(|| self.s[start..self.i].iter().collect())
    }

    fn string(&mut self) -> Option<String> {
        if !self.eat('"') {
            return None;
        }
        let mut out = String::new();
        loop {
            let c = *self.s.get(self.i)?;
            self.i += 1;
            match c {
                '"' => return Some(out),
                '\\' => {
                    let e = *self.s.get(self.i)?;
                    self.i += 1;
                    match e {
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        d if d.is_ascii_digit() => {
                            let mut n = d.to_digit(10)?;
                            for _ in 0..2 {
                                match self.s.get(self.i).and_then(|c| c.to_digit(10)) {
                                    Some(x) => {
                                        n = n * 10 + x;
                                        self.i += 1;
                                    }
                                    None => break,
                                }
                            }
                            out.push(char::from_u32(n)?);
                        }
                        other => out.push(other),
                    }
                }
                c => out.push(c),
            }
        }
    }

    fn table(&mut self, prefix: &str, out: &mut Vec<(String, OptionValue)>) -> Option<()> {
        if !self.eat('{') {
            return None;
        }
        if self.eat('}') {
            return Some(());
        }
        loop {
            let key = if self.eat('[') {
                let k = self.string()?;
                if !self.eat(']') {
                    return None;
                }
                k
            } else {
                self.ident()?
            };
            if !self.eat('=') {
                return None;
            }
            let name = if prefix.is_empty() {
                key
            } else {
                format!("{prefix}:{key}")
            };
            self.ws();
            match self.s.get(self.i)? {
                '{' => self.table(&name, out)?,
                '"' => {
                    let v = self.string()?;
                    out.push((name, OptionValue::Str(v)));
                }
                _ => {
                    let word = {
                        let start = self.i;
                        while self.i < self.s.len() && !matches!(self.s[self.i], ',' | '}' | ' ') {
                            self.i += 1;
                        }
                        self.s[start..self.i].iter().collect::<String>()
                    };
                    let v = match word.as_str() {
                        "true" => OptionValue::Bool(true),
                        "false" => OptionValue::Bool(false),
                        w if w.contains(['.', 'e']) => OptionValue::Float(w.parse().ok()?),
                        w => OptionValue::Int(w.parse().ok()?),
                    };
                    out.push((name, v));
                }
            }
            if self.eat(',') {
                continue;
            }
            if self.eat('}') {
                return Some(());
            }
            return None;
        }
    }
}

/// Um Hyprland em memória: o [`Model`], os pedidos gravados, os eventos
/// injetados e as falhas roteirizadas.
pub struct FakeHypr {
    model: Mutex<Model>,
    requests: Mutex<Vec<String>>,
    fail_next: Mutex<Vec<HyprError>>,
    fail_on: Mutex<Vec<(String, HyprError)>>,
    unreachable: Mutex<bool>,
    hang: Mutex<Vec<String>>,
    panic_on: Mutex<Vec<String>>,
    instance: Mutex<Option<String>>,
    events: broadcast::Sender<Event>,
}

impl Default for FakeHypr {
    fn default() -> FakeHypr {
        FakeHypr::new()
    }
}

impl FakeHypr {
    /// Um Hyprland 0.56.2 da instância `fake_1`, com as opções de fábrica e
    /// sem janelas.
    pub fn new() -> FakeHypr {
        let (events, _) = broadcast::channel(256);
        FakeHypr {
            model: Mutex::new(Model::default()),
            requests: Mutex::new(Vec::new()),
            fail_next: Mutex::new(Vec::new()),
            fail_on: Mutex::new(Vec::new()),
            unreachable: Mutex::new(false),
            hang: Mutex::new(Vec::new()),
            panic_on: Mutex::new(Vec::new()),
            instance: Mutex::new(Some("fake_1".into())),
            events,
        }
    }

    pub fn set_option(&self, name: &str, value: OptionValue) {
        self.model
            .lock()
            .unwrap()
            .options
            .insert(name.to_string(), value);
    }

    pub fn option(&self, name: &str) -> Option<OptionValue> {
        self.model.lock().unwrap().options.get(name).cloned()
    }

    /// Todas as opções agora.
    pub fn options(&self) -> BTreeMap<String, OptionValue> {
        self.model.lock().unwrap().options.clone()
    }

    /// As janelas e os workspaces à mostra.
    pub fn set_windows(&self, windows: Vec<Window>, active_workspaces: Vec<i64>) {
        let mut m = self.model.lock().unwrap();
        m.windows = windows;
        m.active_workspaces = active_workspaces;
    }

    /// O valor da propriedade `prop` da janela, pela última mudança.
    pub fn prop(&self, address: &str, prop: &str) -> Option<String> {
        self.model.lock().unwrap().prop(address, prop)
    }

    /// Injeta um evento.
    pub fn emit(&self, name: &str, data: &str) {
        let _ = self.events.send(Event {
            name: name.into(),
            data: data.into(),
        });
    }

    /// O "reload": as opções voltam a `file` (as que não estão lá ficam) e
    /// o `configreloaded` é emitido.
    pub fn reload(&self, file: &[(&str, OptionValue)]) {
        {
            let mut m = self.model.lock().unwrap();
            for (k, v) in file {
                m.options.insert(k.to_string(), v.clone());
            }
        }
        self.emit("configreloaded", "");
    }

    /// Os comandos recebidos, na ordem.
    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    pub fn clear_requests(&self) {
        self.requests.lock().unwrap().clear();
    }

    /// O próximo pedido falha com `e` (os seguintes, na ordem das chamadas).
    pub fn fail_next(&self, e: HyprError) {
        self.fail_next.lock().unwrap().push(e);
    }

    /// O próximo pedido que começa com `prefix` falha com `e` (uma vez).
    pub fn fail_on(&self, prefix: &str, e: HyprError) {
        self.fail_on.lock().unwrap().push((prefix.to_string(), e));
    }

    /// Com `true`, todo pedido dá `Unreachable` (o Hyprland fora do ar).
    pub fn set_unreachable(&self, down: bool) {
        *self.unreachable.lock().unwrap() = down;
    }

    /// Os comandos que começam com `prefix` nunca respondem.
    pub fn hang(&self, prefix: &str) {
        self.hang.lock().unwrap().push(prefix.to_string());
    }

    /// O próximo pedido que começa com `prefix` entra em pânico (uma vez):
    /// para testar o pânico de quem chama.
    pub fn panic_on(&self, prefix: &str) {
        self.panic_on.lock().unwrap().push(prefix.to_string());
    }

    /// Troca a assinatura da instância (o Hyprland reiniciou).
    pub fn set_instance(&self, sig: Option<&str>) {
        *self.instance.lock().unwrap() = sig.map(str::to_string);
    }
}

impl Hypr for FakeHypr {
    fn request(&self, command: String) -> BoxFuture<'_, Result<String, HyprError>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(command.clone());
            let hang = self
                .hang
                .lock()
                .unwrap()
                .iter()
                .any(|p| command.starts_with(p.as_str()));
            if hang {
                std::future::pending::<()>().await;
            }
            let boom = {
                let mut p = self.panic_on.lock().unwrap();
                p.iter()
                    .position(|x| command.starts_with(x.as_str()))
                    .map(|i| p.remove(i))
            };
            if let Some(prefix) = boom {
                panic!("pânico de teste em {prefix}");
            }
            if *self.unreachable.lock().unwrap() {
                return Err(HyprError::Unreachable(
                    "o Hyprland falso está fora do ar".into(),
                ));
            }
            {
                let mut f = self.fail_next.lock().unwrap();
                if !f.is_empty() {
                    return Err(f.remove(0));
                }
                let mut on = self.fail_on.lock().unwrap();
                if let Some(i) = on.iter().position(|(p, _)| command.starts_with(p.as_str())) {
                    return Err(on.remove(i).1);
                }
            }
            Ok(self.model.lock().unwrap().handle(&command))
        })
    }

    fn events(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    fn instance(&self) -> Option<String> {
        self.instance.lock().unwrap().clone()
    }
}

/// A resposta roteirizada de um comando.
#[derive(Clone)]
enum Script {
    Body(String),
    Hang,
}

#[derive(Default)]
struct Shared {
    /// Prefixo do comando → resposta; o último roteiro de um prefixo vence.
    scripts: Vec<(String, Script)>,
    requests: Vec<String>,
    model: Model,
}

impl Shared {
    fn script(&mut self, prefix: &str, s: Script) {
        self.scripts.retain(|(p, _)| p != prefix);
        self.scripts.push((prefix.to_string(), s));
    }

    fn lookup(&self, command: &str) -> Option<Script> {
        self.scripts
            .iter()
            .filter(|(p, _)| command.starts_with(p.as_str()))
            .max_by_key(|(p, _)| p.len())
            .map(|(_, s)| s.clone())
    }
}

/// Os sockets `.socket.sock` e `.socket2.sock` de um Hyprland falso numa
/// pasta. Crie dentro de um runtime do `tokio`; largar para de atender (os
/// arquivos ficam).
pub struct FakeSockets {
    shared: Arc<Mutex<Shared>>,
    events: broadcast::Sender<String>,
    kick: watch::Sender<u64>,
    tasks: Vec<AbortHandle>,
}

impl FakeSockets {
    /// Atende em `dir/.socket.sock` e `dir/.socket2.sock` (a pasta precisa
    /// existir). Comando sem roteiro vai ao [`Model`].
    pub fn start(dir: &Path) -> FakeSockets {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (events, _) = broadcast::channel(256);
        let (kick, _) = watch::channel(0u64);
        let requests = UnixListener::bind(dir.join(REQUEST_SOCKET)).unwrap();
        let listen_events = UnixListener::bind(dir.join(EVENTS_SOCKET)).unwrap();
        let s = shared.clone();
        let req_task = tokio::spawn(async move {
            while let Ok((stream, _)) = requests.accept().await {
                tokio::spawn(serve_request(stream, s.clone()));
            }
        });
        let ev = events.clone();
        let k = kick.clone();
        let ev_task = tokio::spawn(async move {
            while let Ok((stream, _)) = listen_events.accept().await {
                tokio::spawn(serve_events(stream, ev.subscribe(), k.subscribe()));
            }
        });
        FakeSockets {
            shared,
            events,
            kick,
            tasks: vec![req_task.abort_handle(), ev_task.abort_handle()],
        }
    }

    /// Os comandos que começam com `command_prefix` respondem `body`.
    pub fn reply(&self, command_prefix: &str, body: &str) {
        self.shared
            .lock()
            .unwrap()
            .script(command_prefix, Script::Body(body.to_string()));
    }

    /// Os comandos que começam com `command_prefix` nunca respondem.
    pub fn hang(&self, command_prefix: &str) {
        self.shared
            .lock()
            .unwrap()
            .script(command_prefix, Script::Hang);
    }

    /// Os comandos recebidos, na ordem.
    pub fn requests(&self) -> Vec<String> {
        self.shared.lock().unwrap().requests.clone()
    }

    /// Mexe no [`Model`] (as opções, as janelas).
    pub fn with_model<T>(&self, f: impl FnOnce(&mut Model) -> T) -> T {
        f(&mut self.shared.lock().unwrap().model)
    }

    /// Manda `line` (sem o `\n`) a quem está conectado no socket de eventos.
    pub fn emit(&self, line: &str) {
        let _ = self.events.send(line.to_string());
    }

    /// Quantas conexões de eventos estão abertas agora.
    pub fn event_clients(&self) -> usize {
        self.events.receiver_count()
    }

    /// Fecha as conexões de eventos abertas (o socket continua aceitando).
    pub fn drop_events(&self) {
        self.kick.send_modify(|n| *n += 1);
    }
}

impl Drop for FakeSockets {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
        self.drop_events();
    }
}

async fn serve_request(mut stream: UnixStream, shared: Arc<Mutex<Shared>>) {
    let mut buf = Vec::new();
    if stream.read_to_end(&mut buf).await.is_err() {
        return;
    }
    let command = String::from_utf8_lossy(&buf).into_owned();
    let script = {
        let mut s = shared.lock().unwrap();
        s.requests.push(command.clone());
        match s.lookup(&command) {
            Some(Script::Body(b)) => Some(b),
            Some(Script::Hang) => None,
            None => Some(s.model.handle(&command)),
        }
    };
    let Some(body) = script else {
        std::future::pending::<()>().await;
        return;
    };
    let _ = stream.write_all(body.as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn serve_events(
    mut stream: UnixStream,
    mut lines: broadcast::Receiver<String>,
    mut kick: watch::Receiver<u64>,
) {
    kick.borrow_and_update();
    loop {
        tokio::select! {
            line = lines.recv() => match line {
                Ok(line) => {
                    if stream.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            },
            _ = kick.changed() => return,
        }
    }
}
