//! A instância do Hyprland numa pasta temporária: a da assinatura e, sem
//! ela, a mais nova com socket. Nada aqui olha o `XDG_RUNTIME_DIR` de verdade.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use lucernad_hyprland::instance::find;

/// Uma instância falsa: a pasta, o `hyprland.lock` com o `mtime` dado e,
/// com `socket`, um `.socket.sock` (um arquivo comum basta para o `find`).
fn instance(runtime: &Path, sig: &str, age_s: u64, socket: bool) {
    let dir = runtime.join("hypr").join(sig);
    fs::create_dir_all(&dir).unwrap();
    let lock = dir.join("hyprland.lock");
    fs::write(&lock, "123\n").unwrap();
    let when = SystemTime::now() - Duration::from_secs(age_s);
    fs::File::options()
        .write(true)
        .open(&lock)
        .unwrap()
        .set_modified(when)
        .unwrap();
    if socket {
        fs::write(dir.join(".socket.sock"), "").unwrap();
    }
}

#[test]
fn finds_signature_dir() {
    let run = tempfile::tempdir().unwrap();
    instance(run.path(), "aaa_1", 10, true);
    instance(run.path(), "bbb_2", 1, true);
    let i = find(run.path(), Some("aaa_1")).unwrap();
    assert_eq!(i.signature, "aaa_1");
    assert_eq!(i.dir, run.path().join("hypr/aaa_1"));
    assert_eq!(i.request_path(), run.path().join("hypr/aaa_1/.socket.sock"));
    assert_eq!(i.events_path(), run.path().join("hypr/aaa_1/.socket2.sock"));
    // Assinatura sem pasta (ou com barra) é erro, não a mais nova.
    assert!(find(run.path(), Some("zzz_9")).is_err());
    assert!(find(run.path(), Some("../aaa_1")).is_err());
    assert!(find(run.path(), Some("")).is_err());
}

#[test]
fn without_signature_picks_newest_live() {
    let run = tempfile::tempdir().unwrap();
    instance(run.path(), "velha", 100, true);
    instance(run.path(), "nova", 5, true);
    // A mais nova de todas não tem socket: o Hyprland dela já saiu.
    instance(run.path(), "morta", 1, false);
    let i = find(run.path(), None).unwrap();
    assert_eq!(i.signature, "nova");
}

#[test]
fn missing_runtime_dir_is_an_error() {
    let run = tempfile::tempdir().unwrap();
    let e = find(&run.path().join("nada"), None).unwrap_err();
    assert!(e.contains("Hyprland"), "{e}");
    // A pasta `hypr` existe, mas sem nenhuma instância viva.
    fs::create_dir_all(run.path().join("hypr/morta")).unwrap();
    let e = find(run.path(), None).unwrap_err();
    assert!(e.contains("Hyprland"), "{e}");
}
