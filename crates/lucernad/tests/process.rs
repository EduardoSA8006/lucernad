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
//! do usuário (a área de transferência dele nunca é tocada).
#![cfg(any(feature = "system", feature = "spectrum", feature = "clipboard"))]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;
#[cfg(feature = "system")]
use serde_json::json;

/// O binário com um ambiente limpo: só `HOME`, `XDG_RUNTIME_DIR` e `PATH`.
fn lucernad(dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_lucernad"));
    c.env_clear()
        .env("HOME", dir)
        .env("XDG_RUNTIME_DIR", dir.join("run"))
        .env("PATH", std::env::var_os("PATH").unwrap_or_default());
    c
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
