//! A sonda do D-Bus de verdade (o `zbus` falando com um `dbus-daemon`), com
//! um NetworkManager falso registrado por este processo. Rode pelo
//! `tests/isolado.sh`, que sobe um D-Bus do sistema de mentira num container:
//! fora dele, a trava `isolated()` recusa, e o NetworkManager da máquina
//! nunca é tocado.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lucernad_web::net::{Connectivity, Link, NmConnectivity};
use lucernad_web::testing::isolated;
use tokio::sync::watch::Receiver;
use zbus::connection::Builder;

const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";

/// A janela de cada troca.
const WAIT: Duration = Duration::from_secs(2);

struct FakeNm {
    connectivity: Arc<Mutex<u32>>,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager")]
impl FakeNm {
    #[zbus(property)]
    fn connectivity(&self) -> u32 {
        *self.connectivity.lock().unwrap()
    }
}

/// Espera (até [`WAIT`]) o `watch` chegar a `want`.
async fn until(rx: &mut Receiver<Link>, want: Link) {
    let reached = matches!(
        tokio::time::timeout(WAIT, rx.wait_for(|link| *link == want)).await,
        Ok(Ok(_))
    );
    assert!(reached, "esperava {want:?}, ficou {:?}", *rx.borrow());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "só no D-Bus isolado (tests/isolado.sh)"]
async fn follows_network_manager() {
    isolated();
    let connectivity = Arc::new(Mutex::new(4));
    let service = Builder::system()
        .unwrap()
        .name(NM)
        .unwrap()
        .serve_at(
            NM_PATH,
            FakeNm {
                connectivity: connectivity.clone(),
            },
        )
        .unwrap()
        .build()
        .await
        .unwrap();

    let nm = NmConnectivity::system();
    let mut rx = nm.watch();
    until(&mut rx, Link::Online).await;
    // A segunda chamada divide o mesmo acompanhamento.
    assert_eq!(*nm.watch().borrow(), Link::Online);

    // A conectividade cai para "nenhuma": o PropertiesChanged avisa.
    *connectivity.lock().unwrap() = 1;
    let iface = service
        .object_server()
        .interface::<_, FakeNm>(NM_PATH)
        .await
        .unwrap();
    iface
        .get()
        .await
        .connectivity_changed(iface.signal_emitter())
        .await
        .unwrap();
    until(&mut rx, Link::Offline).await;

    // O NetworkManager some: sem o nome, desconhecida.
    service.release_name(NM).await.unwrap();
    until(&mut rx, Link::Unknown).await;

    // Volta com a conectividade completa: o NameOwnerChanged relê.
    *connectivity.lock().unwrap() = 4;
    service.request_name(NM).await.unwrap();
    until(&mut rx, Link::Online).await;
}
