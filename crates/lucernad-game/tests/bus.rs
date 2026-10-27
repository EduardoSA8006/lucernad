//! O perfil de energia e o GameMode com o D-Bus falso: nada aqui fala com um
//! barramento de verdade.

use std::time::{Duration, Instant};

use lucernad_game::bus::{BusError, DBUS_TIMEOUT, GameBus, gamemode_reply, timed};
use lucernad_game::testing::FakeGameBus;

#[tokio::test]
async fn hold_and_release_profile() {
    let bus = FakeGameBus::new();
    assert_eq!(bus.held(), None);
    let cookie = bus.hold_performance().await.unwrap();
    assert_eq!(bus.held(), Some(cookie));
    bus.release(cookie).await.unwrap();
    assert_eq!(bus.held(), None);
    assert_eq!(
        bus.calls(),
        vec![format!("hold"), format!("release {cookie}")]
    );
}

#[tokio::test]
async fn absent_profiles_is_unavailable() {
    let bus = FakeGameBus::new();
    bus.absent_profiles();
    assert_eq!(bus.hold_performance().await, Err(BusError::Absent));
    assert_eq!(BusError::Absent.effect(), "unavailable");
    assert_eq!(BusError::Failed("x".into()).effect(), "error: x");
}

#[tokio::test]
async fn gamemode_register_and_unregister() {
    let bus = FakeGameBus::new();
    assert_eq!(bus.gamemode_present().await, Ok(true));
    bus.register_game(4242).await.unwrap();
    bus.register_game(77).await.unwrap();
    assert_eq!(bus.registered(), vec![4242, 77]);
    bus.unregister_game(4242).await.unwrap();
    assert_eq!(bus.registered(), vec![77]);
    bus.absent_gamemode();
    assert_eq!(bus.gamemode_present().await, Ok(false));
    assert_eq!(bus.register_game(1).await, Err(BusError::Absent));
}

#[test]
fn gamemode_negative_reply_is_failed() {
    assert_eq!(gamemode_reply(0), Ok(()));
    assert_eq!(gamemode_reply(1), Ok(()));
    let e = gamemode_reply(-1).unwrap_err();
    assert!(matches!(e, BusError::Failed(_)), "{e:?}");
    assert_eq!(e.to_string(), "o GameMode recusou o pedido (código -1)");
}

#[tokio::test]
async fn bus_call_times_out() {
    assert_eq!(DBUS_TIMEOUT, Duration::from_secs(2));
    let bus = FakeGameBus::new();
    bus.hang();
    let start = Instant::now();
    assert_eq!(timed(bus.hold_performance()).await, Err(BusError::TimedOut));
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_eq!(
        BusError::TimedOut.effect(),
        "error: o D-Bus não respondeu em 2 s"
    );
}

#[tokio::test]
async fn fail_next_fails_once() {
    let bus = FakeGameBus::new();
    bus.fail_next(BusError::Failed("ocupado".into()));
    assert_eq!(
        bus.hold_performance().await,
        Err(BusError::Failed("ocupado".into()))
    );
    assert!(bus.hold_performance().await.is_ok());
}
