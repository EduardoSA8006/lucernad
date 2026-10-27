//! O pedido ao `.socket.sock` contra o servidor falso numa pasta temporária:
//! a resposta inteira até o EOF, o prazo, o limite e o socket que não existe.

use std::time::{Duration, Instant};

use lucernad_hyprland::socket::{HYPR_REPLY_MAX, HYPR_TIMEOUT, HyprError, request};
use lucernad_hyprland::testing::FakeSockets;

const OPTION: &str = r#"{"option": "animations:enabled", "bool": true, "set": false }"#;

#[tokio::test]
async fn request_returns_body() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeSockets::start(dir.path());
    fake.reply("j/getoption animations:enabled", OPTION);
    let path = dir.path().join(".socket.sock");
    let body = request(&path, "j/getoption animations:enabled", HYPR_TIMEOUT)
        .await
        .unwrap();
    assert_eq!(body, OPTION);
    assert_eq!(fake.requests(), vec!["j/getoption animations:enabled"]);
}

#[tokio::test]
async fn request_times_out() {
    assert_eq!(HYPR_TIMEOUT, Duration::from_secs(2));
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeSockets::start(dir.path());
    fake.hang("j/clients");
    let start = Instant::now();
    let e = request(&dir.path().join(".socket.sock"), "j/clients", HYPR_TIMEOUT)
        .await
        .unwrap_err();
    assert_eq!(e, HyprError::TimedOut);
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_eq!(e.to_string(), "o Hyprland não respondeu em 2 s");
}

#[tokio::test]
async fn reply_over_limit_is_too_large() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeSockets::start(dir.path());
    fake.reply("j/clients", &"x".repeat(HYPR_REPLY_MAX + 1));
    fake.reply("j/monitors", &"x".repeat(HYPR_REPLY_MAX));
    let path = dir.path().join(".socket.sock");
    let e = request(&path, "j/clients", HYPR_TIMEOUT).await.unwrap_err();
    assert_eq!(e, HyprError::TooLarge);
    // No limite exato, passa.
    let body = request(&path, "j/monitors", HYPR_TIMEOUT).await.unwrap();
    assert_eq!(body.len(), HYPR_REPLY_MAX);
}

#[tokio::test]
async fn missing_socket_is_unreachable() {
    let dir = tempfile::tempdir().unwrap();
    let e = request(&dir.path().join(".socket.sock"), "j/version", HYPR_TIMEOUT)
        .await
        .unwrap_err();
    assert!(matches!(e, HyprError::Unreachable(_)), "{e:?}");
    assert!(
        e.to_string().starts_with("sem conexão com o Hyprland"),
        "{e}"
    );
}
