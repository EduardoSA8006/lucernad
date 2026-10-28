//! O binário `lucernad` de verdade: `serve` e os subcomandos em processos.
//!
//! Cada teste usa a própria pasta temporária como `HOME` e `XDG_RUNTIME_DIR`;
//! nenhum processo sobra, nem quando um teste entra em pânico.
//!
//! O `serve` daqui lê a máquina de verdade. Nenhum teste assina o
//! `system.gpus` (nem pelo `status`, que assina todos os tópicos): com uma
//! NVIDIA em uso na máquina, isso chamaria a NVML de verdade. O ambiente
//! limpo também deixa o `spectrum` sem o PipeWire e o D-Bus do usuário e o
//! `clipboard` sem `WAYLAND_DISPLAY`: nenhum teste fala com a sessão Wayland
//! do usuário (a área de transferência dele nunca é tocada). O `power` roda
//! sobre uma árvore falsa (`LUCERNAD_POWER_ROOT`) e sem o D-Bus do sistema:
//! o brilho, o logind e o i2c da máquina nunca são tocados. O `web` roda
//! contra um servidor local (`LUCERNAD_WEB_BASE`): nenhum processo daqui vai à
//! internet. O `hyprland` e o `game` falam com um Hyprland falso numa pasta
//! temporária (ou com nenhum), e o perfil e o GameMode do `game` são os
//! falsos em arquivos da raiz do `LUCERNAD_POWER_ROOT`, sem D-Bus.
#![cfg(any(
    feature = "system",
    feature = "spectrum",
    feature = "clipboard",
    feature = "power",
    feature = "web",
    feature = "hyprland"
))]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;
#[cfg(any(feature = "system", feature = "power", feature = "web"))]
use serde_json::json;

/// Um servidor HTTP local (127.0.0.1) num runtime próprio: vive enquanto o
/// teste (síncrono) o guardar e morre com ele. Responde a busca de cidade
/// com Cacaulândia e o resto com 404.
#[cfg(feature = "web")]
struct WebServer {
    // O servidor cai antes do runtime que o roda.
    server: Option<lucernad_web::testing::LocalServer>,
    rt: Option<tokio::runtime::Runtime>,
}

#[cfg(feature = "web")]
impl WebServer {
    fn start() -> WebServer {
        use lucernad_web::testing::{LocalServer, Reply};
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let server = rt.block_on(LocalServer::start(|seen| {
            if seen.path == "/geocoding/v1/search" {
                Reply::json(
                    200,
                    &json!({"results": [{
                        "id": 1, "name": "Cacaulândia", "latitude": -10.3375,
                        "longitude": -62.8986, "country_code": "BR",
                        "country": "Brasil", "admin1": "Rondônia",
                    }]}),
                )
            } else {
                Reply::text(404, "não achei")
            }
        }));
        WebServer {
            server: Some(server),
            rt: Some(rt),
        }
    }

    fn base(&self) -> String {
        self.server.as_ref().unwrap().base()
    }
}

#[cfg(feature = "web")]
impl Drop for WebServer {
    fn drop(&mut self) {
        drop(self.server.take());
        drop(self.rt.take());
    }
}

#[cfg(feature = "web")]
thread_local! {
    // Um servidor por teste (cada teste roda na própria thread): sobe na
    // primeira vez que um processo precisa da base e morre com a thread.
    static WEB: WebServer = WebServer::start();
}

/// A base do servidor local deste teste.
#[cfg(feature = "web")]
fn web_base() -> String {
    WEB.with(WebServer::base)
}

/// O binário com um ambiente limpo (sem `HYPRLAND_INSTANCE_SIGNATURE` e sem
/// Hyprland no `XDG_RUNTIME_DIR`): `HOME`, `XDG_RUNTIME_DIR`, `PATH` e, para
/// o `power`, a árvore falsa em `dir/power` e um D-Bus do sistema que não
/// existe; para o `web`, o `LUCERNAD_WEB_BASE` do servidor local do teste.
fn lucernad(dir: &Path) -> Command {
    // A raiz falsa precisa existir (vazia, sem telas) em todo processo.
    std::fs::create_dir_all(dir.join("power")).unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_lucernad"));
    c.env_clear()
        .env("HOME", dir)
        .env("XDG_RUNTIME_DIR", dir.join("run"))
        .env("LUCERNAD_POWER_ROOT", dir.join("power"))
        .env(
            "DBUS_SYSTEM_BUS_ADDRESS",
            format!("unix:path={}", dir.join("sem-barramento").display()),
        )
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}", dir.join("sem-barramento").display()),
        )
        // O Hyprland, só o falso do teste (em `run/hypr/sig_teste`, quando
        // o teste o sobe): sem ele, o módulo fica indisponível.
        .env("HYPRLAND_INSTANCE_SIGNATURE", "sig_teste")
        .env("PATH", std::env::var_os("PATH").unwrap_or_default());
    // Sem isto o binário iria às fontes de verdade (LRCLIB, Open-Meteo).
    #[cfg(feature = "web")]
    c.env("LUCERNAD_WEB_BASE", web_base());
    c
}

/// A tela integrada falsa `lucerna_teste` (máx. 1000, valor 500) em `dir/power`.
#[cfg(feature = "power")]
fn fake_panel(dir: &Path) -> std::path::PathBuf {
    let power = dir.join("power");
    std::fs::create_dir_all(&power).unwrap();
    lucernad_power::testing::Tree::new(&power).backlight(
        "lucerna_teste",
        "raw",
        1000,
        500,
        500,
        None,
    );
    power.join("sys/class/backlight/lucerna_teste/brightness")
}

/// Espera o arquivo ter `want`, até 2 s.
#[cfg(any(feature = "power", feature = "game"))]
fn wait_file(path: &Path, want: &str) {
    let start = Instant::now();
    loop {
        let got = std::fs::read_to_string(path).unwrap_or_default();
        if got.trim() == want {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{}: queria {want}, tem {got:?}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Um processo filho que é morto e esperado ao cair.
struct Serve(Child);

impl Serve {
    fn child(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        if let Ok(None) = self.0.try_wait() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

/// O socket do `serve` dentro de `dir`.
fn socket(dir: &Path) -> std::path::PathBuf {
    dir.join("run/lucerna/lucernad.sock")
}

/// Sobe `lucernad serve` com a ociosidade `idle_ms` e espera o socket.
fn serve(dir: &Path, idle_ms: u64) -> Serve {
    std::fs::create_dir_all(dir.join("run")).unwrap();
    let mut srv = Serve(
        lucernad(dir)
            .arg("serve")
            .env("LUCERNA_IDLE_TIMEOUT_MS", idle_ms.to_string())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(dir.join("serve.log")).unwrap())
            .spawn()
            .unwrap(),
    );
    let start = Instant::now();
    while !socket(dir).exists() {
        assert!(
            srv.child().try_wait().unwrap().is_none(),
            "o serve saiu antes de criar o socket"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "o socket não apareceu"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    srv
}

/// Espera o processo sair, até `limite`.
fn wait_exit(child: &mut Child, limite: Duration) -> Option<ExitStatus> {
    let start = Instant::now();
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return Some(s);
        }
        if start.elapsed() >= limite {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `--json watch <topic>` com as linhas lidas numa thread (o `recv_timeout`
/// impede o teste de travar), já com o retrato inicial lido: a conexão está
/// aceita e contada pelo serviço. Devolve também esse retrato.
fn watch_json(dir: &Path, topic: &str) -> (Serve, mpsc::Receiver<String>, Value) {
    let mut watch = Serve(
        lucernad(dir)
            .args(["--json", "watch", topic])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = watch.child().stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let first = line(&rx);
    assert_eq!(first["topic"], topic);
    (watch, rx, first)
}

/// A próxima linha JSON do `watch`, em até 5 s.
fn line(rx: &mpsc::Receiver<String>) -> Value {
    serde_json::from_str(&rx.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap()
}

/// Manda `SIGTERM` ao processo.
fn sigterm(child: &Child) {
    let pid = child.id().to_string();
    let st = Command::new("kill").args(["-TERM", &pid]).status().unwrap();
    assert!(st.success());
}

#[test]
fn cli_help_status_and_exit_codes() {
    let t = tempfile::tempdir().unwrap();
    let out = lucernad(t.path()).arg("status").output().unwrap();
    assert_eq!(out.status.code(), Some(2)); // ainda sem serviço
    assert!(
        String::from_utf8_lossy(&out.stderr).starts_with("Sem conexão com o serviço lucernad"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Sem TUI: a ajuda, com 0, também para o `tui …` que o shell usaria.
    for args in [&[][..], &["tui"]] {
        let out = lucernad(t.path()).args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "sem argumentos: a ajuda");
        assert!(String::from_utf8_lossy(&out.stdout).contains("status"));
    }
    let out = lucernad(t.path()).arg("--version").output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains(env!("CARGO_PKG_VERSION")));

    #[cfg(feature = "power")]
    let brightness = fake_panel(t.path());
    let _srv = serve(t.path(), 60_000);
    #[cfg(feature = "system")]
    {
        // O `watch` no lugar do `status`, que assinaria o `system.gpus`.
        let (_w, _rx, info) = watch_json(t.path(), "system.info");
        assert!(info["data"]["hostname"].is_string(), "{info}");
        let (_w, _rx, status) = watch_json(t.path(), "system.status");
        assert_eq!(
            status["data"],
            json!({"available": true, "error": null}),
            "{status}"
        );
        let (_w, rx, stats) = watch_json(t.path(), "system.stats");
        assert_eq!(stats["data"], Value::Null, "nada antes da primeira amostra");
        assert!(line(&rx)["data"]["memTotal"].as_u64().unwrap() > 0);
    }
    #[cfg(feature = "spectrum")]
    {
        // Também pelo `watch`: o `status` assinaria o `system.gpus`.
        let (_w, _rx, v) = watch_json(t.path(), "spectrum.status");
        assert_eq!(v["data"]["capturing"], false, "{v}");
        assert!(v["data"]["available"].is_boolean(), "{v}");
        let out = lucernad(t.path())
            .args(["player", "org.mpris.MediaPlayer2.teste"])
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = lucernad(t.path())
            .args(["player", "spotify"])
            .output()
            .unwrap();
        assert_ne!(out.status.code(), Some(0));
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("player precisa ser"), "stderr: {err}");
    }
    #[cfg(feature = "clipboard")]
    {
        let (_w, _rx, v) = watch_json(t.path(), "clipboard.status");
        assert_eq!(
            v["data"]["available"], false,
            "sem Wayland no ambiente: {v}"
        );
        // Sem `configure`, o módulo espera 2 s e aplica o padrão.
        let out = lucernad(t.path()).arg("historico").output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("Nada no histórico."),
            "stdout: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        let out = lucernad(t.path()).args(["copiar", "abc"]).output().unwrap();
        assert_eq!(out.status.code(), Some(64), "uso errado");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("ID precisa ser um número"), "stderr: {err}");
        let out = lucernad(t.path()).args(["copiar", "99"]).output().unwrap();
        assert_ne!(out.status.code(), Some(0));
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("não existe mais"), "stderr: {err}");
    }
    #[cfg(feature = "web")]
    web_cli(t.path());
    #[cfg(feature = "power")]
    {
        // Só o status: a raiz é a falsa e o D-Bus não existe.
        let (_w, _rx, v) = watch_json(t.path(), "power.status");
        assert_eq!(v["data"], json!({"available": true, "error": null}), "{v}");
        let out = lucernad(t.path()).arg("brilho").output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("backlight  50%  Tela integrada"),
            "stdout: {stdout}"
        );
        let out = lucernad(t.path())
            .args(["brilho", "backlight", "80"])
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        wait_file(&brightness, "800");
        let out = lucernad(t.path())
            .args(["brilho", "backlight"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(64), "uso errado");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("Passe o ID e o PERCENT juntos"),
            "stderr: {err}"
        );
        let out = lucernad(t.path())
            .args(["brilho", "backlight", "-10"])
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        wait_file(&brightness, "700");
        let out = lucernad(t.path())
            .args(["brilho", "backlight", "0"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(64), "uso errado");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("PERCENT precisa ser"), "stderr: {err}");
        let out = lucernad(t.path())
            .args(["brilho", "x", "50"])
            .output()
            .unwrap();
        assert_ne!(out.status.code(), Some(0));
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("Essa tela não existe."), "stderr: {err}");
        // Sem valor lido, o `+N` não tem base: o 1003 com o erro da linha,
        // e nada é gravado.
        std::fs::write(&brightness, "lixo\n").unwrap();
        let out = lucernad(t.path())
            .args(["brilho", "backlight", "+5"])
            .output()
            .unwrap();
        assert_ne!(out.status.code(), Some(0));
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("Não consegui ler o brilho da tela integrada: o valor não é um número."),
            "stderr: {err}"
        );
        assert_eq!(std::fs::read_to_string(&brightness).unwrap(), "lixo\n");
    }
    #[cfg(feature = "hyprland")]
    {
        // Sem Hyprland no XDG_RUNTIME_DIR temporário: indisponível, com o motivo.
        let (_w, rx, mut v) = watch_json(t.path(), "hyprland.status");
        while !v["data"]["error"]
            .as_str()
            .unwrap_or_default()
            .contains("não está rodando")
        {
            v = line(&rx);
        }
        assert_eq!(v["data"]["available"], false, "{v}");
    }
}

/// O `web`: a busca de cidade, o `rede` e o `faixa` pela linha de comando, o
/// módulo indisponível com uma base inválida e a chave que nunca aparece.
#[cfg(feature = "web")]
fn web_cli(dir: &Path) {
    let log = || std::fs::read_to_string(dir.join("serve.log")).unwrap();
    assert!(
        log().contains(&format!(
            "LUCERNAD_WEB_BASE={}: a rede é de teste",
            web_base()
        )),
        "log: {}",
        log()
    );
    let (_w, _rx, status) = watch_json(dir, "web.status");
    assert_eq!(status["data"]["available"], true, "{status}");
    // Antes do `rede` o módulo está offline.
    let out = lucernad(dir).args(["cidade", "Cacau"]).output().unwrap();
    assert_ne!(out.status.code(), Some(0));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Modo offline ligado"), "stderr: {err}");
    let out = lucernad(dir).arg("rede").output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = lucernad(dir).args(["cidade", "Cacau"]).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Cacaulândia, Rondônia, Brasil  (-10.3375, -62.8986)"),
        "stdout: {stdout}"
    );
    let out = lucernad(dir)
        .args(["rede", "--cidade", "X"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(64), "uso errado");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("Passe --cidade, --lat e --lon juntos"),
        "stderr: {err}"
    );
    let out = lucernad(dir)
        .args(["faixa", "Legião Urbana", "Tempo Perdido", "201"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = lucernad(dir)
        .args(["faixa", "A", "B", "90000"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(64), "uso errado");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("SEGUNDOS precisa ser"), "stderr: {err}");
    // A chave vem só do ambiente e nunca aparece no log nem na saída.
    let out = lucernad(dir)
        .arg("rede")
        .env("LUCERNAD_VAGALUME_KEY", "CHAVE-SECRETA")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let all = format!(
        "{}{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        log()
    );
    assert!(!all.contains("CHAVE-SECRETA"), "a chave vazou: {all}");
}

/// Uma base que não é de loopback deixa o módulo indisponível, nunca na rede
/// de verdade.
#[cfg(feature = "web")]
#[test]
fn invalid_web_base_makes_the_module_unavailable() {
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(t.path().join("run")).unwrap();
    let mut srv = Serve(
        lucernad(t.path())
            .arg("serve")
            .env("LUCERNAD_WEB_BASE", "https://example.com")
            .env("LUCERNA_IDLE_TIMEOUT_MS", "60000")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let start = Instant::now();
    while !socket(t.path()).exists() {
        assert!(srv.child().try_wait().unwrap().is_none());
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(10));
    }
    let (_watch, _rx, v) = watch_json(t.path(), "web.status");
    assert_eq!(v["data"]["available"], false, "{v}");
    assert_eq!(
        v["data"]["error"],
        "LUCERNAD_WEB_BASE inválido: precisa ser http://127.0.0.1:<porta> ou http://[::1]:<porta> (só para testes).",
        "{v}"
    );
}

#[cfg(feature = "system")]
#[test]
fn idle_exit() {
    let t = tempfile::tempdir().unwrap();
    let mut srv = serve(t.path(), 200);
    let (watch, _rx, _) = watch_json(t.path(), "system.info");
    std::thread::sleep(Duration::from_millis(500)); // > 2× a ociosidade
    assert!(
        srv.child().try_wait().unwrap().is_none(),
        "com cliente conectado não sai"
    );
    drop(watch); // kill + wait
    assert!(
        wait_exit(srv.child(), Duration::from_secs(2))
            .expect("o serviço não saiu")
            .success(),
        "sai sozinho e com 0"
    );
    assert!(!socket(t.path()).exists(), "o socket manual é apagado");
}

#[test]
fn sigterm_exits_clean() {
    let t = tempfile::tempdir().unwrap();
    let mut srv = serve(t.path(), 60_000);
    sigterm(srv.child());
    assert!(
        wait_exit(srv.child(), Duration::from_secs(2))
            .expect("o serviço não saiu com SIGTERM")
            .success(),
        "sai com 0"
    );
    assert!(!socket(t.path()).exists(), "o socket manual é apagado");
}

/// Não dá para travar um `statvfs` de verdade no CI: confere só que o `serve`
/// medindo os discos sai com 0 no `SIGTERM`. A garantia de que uma montagem
/// travada não segura a saída vem do `detached` (thread solta, Tarefa 3).
#[cfg(feature = "system")]
#[test]
fn exits_even_with_a_stuck_thread() {
    let t = tempfile::tempdir().unwrap();
    let mut srv = serve(t.path(), 60_000);
    let (_watch, _rx, _) = watch_json(t.path(), "system.disks");
    sigterm(srv.child());
    assert!(
        wait_exit(srv.child(), Duration::from_secs(2))
            .expect("o serviço não saiu com SIGTERM")
            .success(),
        "sai com 0"
    );
    assert!(!socket(t.path()).exists(), "o socket manual é apagado");
}

#[test]
fn second_serve_is_refused() {
    let t = tempfile::tempdir().unwrap();
    let mut srv = serve(t.path(), 60_000);
    let out = lucernad(t.path()).arg("serve").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("Já há um serviço atendendo em"),
        "stderr: {err}"
    );
    assert!(
        srv.child().try_wait().unwrap().is_none(),
        "o primeiro continua"
    );
}

/// Um Hyprland falso (os dois sockets) em `dir/run/hypr/sig_teste`, num
/// runtime próprio que vive enquanto o teste o guardar.
#[cfg(feature = "game")]
struct FakeHyprland {
    fake: Option<lucernad_hyprland::testing::FakeSockets>,
    rt: Option<tokio::runtime::Runtime>,
}

#[cfg(feature = "game")]
impl FakeHyprland {
    fn start(dir: &Path) -> FakeHyprland {
        let inst = dir.join("run/hypr/sig_teste");
        std::fs::create_dir_all(&inst).unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let fake = rt.block_on(async { lucernad_hyprland::testing::FakeSockets::start(&inst) });
        FakeHyprland {
            fake: Some(fake),
            rt: Some(rt),
        }
    }

    fn fake(&self) -> &lucernad_hyprland::testing::FakeSockets {
        self.fake.as_ref().unwrap()
    }

    fn options(
        &self,
    ) -> std::collections::BTreeMap<String, lucernad_hyprland::options::OptionValue> {
        self.fake().with_model(|m| m.options.clone())
    }
}

#[cfg(feature = "game")]
impl Drop for FakeHyprland {
    fn drop(&mut self) {
        drop(self.fake.take());
        drop(self.rt.take());
    }
}

#[cfg(feature = "game")]
fn run_ok(dir: &Path, args: &[&str]) -> String {
    let out = lucernad(dir).args(args).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{args:?}: stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[cfg(feature = "game")]
#[test]
fn jogo_cli_round_trip() {
    use lucernad_hyprland::options::OptionValue::Bool;
    let t = tempfile::tempdir().unwrap();
    let hypr = FakeHyprland::start(t.path());
    let before = hypr.options();
    let _srv = serve(t.path(), 60_000);
    assert_eq!(run_ok(t.path(), &["jogo"]).trim(), "desligado");
    run_ok(t.path(), &["jogo", "ligar"]);
    let evals: Vec<String> = hypr
        .fake()
        .requests()
        .into_iter()
        .filter(|r| r.starts_with("eval"))
        .collect();
    assert_eq!(
        evals,
        vec![
            "eval hl.config({ decoration = { blur = { enabled = false }, shadow = { enabled = false } }, animations = { enabled = false }, misc = { vrr = 2 } })"
        ]
    );
    assert_eq!(hypr.options()["animations:enabled"], Bool(false));
    let journal = t.path().join("run/lucerna/game/restore.json");
    assert!(journal.exists(), "o diário gravado");
    let shown = run_ok(t.path(), &["jogo"]);
    assert!(shown.starts_with("ligado (manual) há "), "{shown}");
    assert_eq!(run_ok(t.path(), &["jogo", "alternar"]).trim(), "desligado");
    assert_eq!(hypr.options(), before, "restaurado");
    assert!(!journal.exists());
    let out = lucernad(t.path())
        .args(["jogo", "talvez"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(64), "uso errado");
}

/// Com o `LUCERNAD_POWER_ROOT`, o perfil de energia do modo jogo é o falso
/// em arquivos da raiz (`power/power-profiles`): o D-Bus do sistema nunca é
/// tocado, mesmo com um power-profiles-daemon de verdade na máquina.
#[cfg(feature = "game")]
#[test]
fn jogo_holds_the_fake_profile_under_the_test_root() {
    let t = tempfile::tempdir().unwrap();
    let profiles = t.path().join("power/power-profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    std::fs::write(profiles.join("active"), "power-saver\n").unwrap();
    let _hypr = FakeHyprland::start(t.path());
    let _srv = serve(t.path(), 60_000);
    run_ok(t.path(), &["jogo", "ligar"]);
    wait_file(&profiles.join("active"), "performance");
    assert!(profiles.join("holds/1").exists());
    run_ok(t.path(), &["jogo", "desligar"]);
    wait_file(&profiles.join("active"), "power-saver");
}

#[cfg(feature = "game")]
#[test]
fn restaurar_jogo_without_journal_exits_zero() {
    let t = tempfile::tempdir().unwrap();
    let out = lucernad(t.path()).arg("restaurar-jogo").output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());
    // Escondido da ajuda.
    let help = run_ok(t.path(), &["--help"]);
    assert!(!help.contains("restaurar-jogo"), "{help}");
    assert!(help.contains("jogo"), "{help}");
}

/// O daemon morto com `kill -9` no meio do jogo: o `restaurar-jogo` (o
/// `ExecStopPost`) desfaz pelo diário, sem daemon; e um daemon novo também
/// desfaz ao subir.
#[cfg(feature = "game")]
#[test]
fn killed_daemon_is_undone_by_the_journal() {
    let t = tempfile::tempdir().unwrap();
    let hypr = FakeHyprland::start(t.path());
    let before = hypr.options();
    let journal = t.path().join("run/lucerna/game/restore.json");
    {
        let mut srv = serve(t.path(), 60_000);
        run_ok(t.path(), &["jogo", "ligar"]);
        assert_ne!(hypr.options(), before);
        srv.child().kill().unwrap(); // SIGKILL
        srv.child().wait().unwrap();
    }
    // O socket à mão sobra com o SIGKILL (o do systemd não).
    std::fs::remove_file(socket(t.path())).unwrap();
    assert!(journal.exists(), "o diário fica com o daemon morto");
    assert_eq!(run_ok(t.path(), &["restaurar-jogo"]), "");
    assert_eq!(hypr.options(), before);
    assert!(!journal.exists());
    // De novo, agora sem o restaurar-jogo: o daemon que sobe desfaz.
    {
        let mut srv = serve(t.path(), 60_000);
        run_ok(t.path(), &["jogo", "ligar"]);
        srv.child().kill().unwrap();
        srv.child().wait().unwrap();
    }
    std::fs::remove_file(socket(t.path())).unwrap();
    assert_ne!(hypr.options(), before);
    let _srv = serve(t.path(), 60_000);
    let start = Instant::now();
    while hypr.options() != before {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "não restaurou ao subir"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!journal.exists());
}

/// O `SIGTERM` (o serviço parando) desfaz o modo jogo antes de sair.
#[cfg(feature = "game")]
#[test]
fn sigterm_undoes_game_mode() {
    let t = tempfile::tempdir().unwrap();
    let hypr = FakeHyprland::start(t.path());
    let before = hypr.options();
    let mut srv = serve(t.path(), 60_000);
    run_ok(t.path(), &["jogo", "ligar"]);
    assert_ne!(hypr.options(), before);
    sigterm(srv.child());
    assert!(
        wait_exit(srv.child(), Duration::from_secs(5))
            .expect("não saiu")
            .success()
    );
    assert_eq!(
        hypr.options(),
        before,
        "{}",
        std::fs::read_to_string(t.path().join("serve.log")).unwrap_or_default()
    );
}
