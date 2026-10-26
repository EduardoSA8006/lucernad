//! A conectividade: os valores do NetworkManager, o `FakeLink` e a trava da
//! sonda. Nada aqui fala com o D-Bus.

use std::time::Duration;

use lucernad_web::net::{Connectivity, Link, link_from};
use lucernad_web::testing::{FakeLink, isolated};

#[test]
fn connectivity_values() {
    assert_eq!(link_from(0), Link::Unknown);
    assert_eq!(link_from(1), Link::Offline);
    assert_eq!(link_from(2), Link::Online);
    assert_eq!(link_from(3), Link::Online);
    assert_eq!(link_from(4), Link::Online);
    assert_eq!(link_from(9), Link::Unknown);
}

#[tokio::test]
async fn fake_link_delivers() {
    let link = FakeLink::new(Link::Online);
    let mut rx = link.watch();
    assert_eq!(*rx.borrow_and_update(), Link::Online);
    link.set(Link::Offline);
    tokio::time::timeout(Duration::from_secs(1), rx.changed())
        .await
        .expect("a troca chega")
        .unwrap();
    assert_eq!(*rx.borrow(), Link::Offline);
    // Um clone divide o mesmo estado.
    let other = link.clone();
    other.set(Link::Unknown);
    assert_eq!(*link.watch().borrow(), Link::Unknown);
}

#[test]
fn sonda_refuses_outside_the_container() {
    // No container da sonda a trava deixa passar: aqui não há o que conferir.
    if std::env::var_os("LUCERNA_ISOLADO").is_some() {
        return;
    }
    let payload = std::panic::catch_unwind(isolated).expect_err("a trava recusa");
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .expect("o texto do pânico");
    assert_eq!(
        message,
        "sonda.rs: só roda pelo tests/isolado.sh, num container"
    );
}
