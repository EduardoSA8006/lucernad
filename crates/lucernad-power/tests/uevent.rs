//! O uevent do kernel: o formato da mensagem, a thread de leitura (sobre uma
//! fonte falsa: nenhum teste abre o socket netlink de verdade) e o dublê.

use std::time::{Duration, Instant};

use lucernad_power::testing::FakeUevents;
use lucernad_power::uevent::{
    NoUevents, Received, Uevent, Uevents, devpath_name, parse_uevent, spawn_reader,
};

const BACKLIGHT: &str = "/devices/pci0000:00/0000:00:02.0/drm/card2/card2-eDP-1/intel_backlight";

fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !ok() {
        assert!(Instant::now() < deadline, "esperei demais: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn parses_kernel_messages() {
    let msg = b"change@/devices/pci0000:00/0000:00:02.0/drm/card2/card2-eDP-1/intel_backlight\0ACTION=change\0DEVPATH=/devices/pci0000:00/0000:00:02.0/drm/card2/card2-eDP-1/intel_backlight\0SUBSYSTEM=backlight\0SOURCE=sysfs\0SEQNUM=9\0";
    let event = parse_uevent(msg);
    assert_eq!(
        event,
        Some(Uevent::Event {
            action: "change".into(),
            subsystem: "backlight".into(),
            devpath: BACKLIGHT.into(),
        })
    );
    assert_eq!(devpath_name(BACKLIGHT), "intel_backlight");

    let drm = b"change@/devices/pci0000:00/0000:01:00.0/drm/card1\0ACTION=change\0DEVPATH=/devices/pci0000:00/0000:01:00.0/drm/card1\0SUBSYSTEM=drm\0HOTPLUG=1\0SEQNUM=10\0";
    match parse_uevent(drm) {
        Some(Uevent::Event { subsystem, .. }) => assert_eq!(subsystem, "drm"),
        other => panic!("esperava um evento drm, veio {other:?}"),
    }
}

#[test]
fn rejects_garbage() {
    assert_eq!(
        parse_uevent(b"libudev\0\xfe\xed\xca\xfe\0ACTION=change\0SUBSYSTEM=drm\0"),
        None
    );
    assert_eq!(parse_uevent(b""), None);
    assert_eq!(parse_uevent(b"sem arroba"), None);
    assert_eq!(
        parse_uevent(b"change@/devices/x\0ACTION=change\0DEVPATH=/devices/x\0SEQNUM=1\0"),
        None
    );
}

/// A thread de leitura sobre uma fonte falsa (o que o `KernelUevents` faz
/// com o socket): só mensagens do kernel e inteiras passam, o `ENOBUFS` vira
/// `Lost`, subsistema que o módulo não usa fica fora e largar o receptor
/// encerra a thread (o contador volta).
///
/// O `threads()` é um contador global do processo: este é o único teste
/// deste binário que sobe a thread de leitura, então rodar em paralelo com os
/// outros não o mexe. Um segundo teste que suba a thread precisa de um
/// `Mutex` de teste em volta dos dois.
#[test]
fn reader_thread_filters_and_ends_when_dropped() {
    let before = lucernad_power::uevent::threads();
    let good = b"change@/devices/x/intel_backlight\0ACTION=change\0DEVPATH=/devices/x/intel_backlight\0SUBSYSTEM=backlight\0".to_vec();
    let usb = b"add@/devices/usb1\0ACTION=add\0DEVPATH=/devices/usb1\0SUBSYSTEM=usb\0".to_vec();
    let mut script = vec![
        (usb, true, false),           // do kernel, mas não é backlight nem drm: fora
        (good.clone(), false, false), // de um processo (pid ≠ 0): fora
        (good.clone(), true, true),   // truncada: fora
        (good.clone(), true, false),  // do kernel e inteira: passa
    ]
    .into_iter();
    let mut lost_sent = false;
    let mut rx = spawn_reader(move |buf: &mut [u8]| {
        if let Some((msg, from_kernel, truncated)) = script.next() {
            buf[..msg.len()].copy_from_slice(&msg);
            return Received::Message {
                len: msg.len(),
                from_kernel,
                truncated,
            };
        }
        if !lost_sent {
            lost_sent = true;
            return Received::Lost;
        }
        std::thread::sleep(Duration::from_millis(20));
        Received::Timeout
    })
    .expect("a thread sobe");
    assert_eq!(lucernad_power::uevent::threads(), before + 1);

    let first = rx.blocking_recv().expect("o evento do kernel");
    assert!(matches!(first, Uevent::Event { ref subsystem, .. } if subsystem == "backlight"));
    assert_eq!(rx.blocking_recv(), Some(Uevent::Lost));

    drop(rx);
    wait_until("a thread terminar", || {
        lucernad_power::uevent::threads() == before
    });
}

#[test]
fn fake_delivers_and_counts() {
    let fake = FakeUevents::new();
    assert_eq!(fake.watching(), 0);
    let mut rx = fake.watch().expect("o dublê sempre abre");
    assert_eq!(fake.watching(), 1);
    fake.send("change", "backlight", BACKLIGHT);
    assert_eq!(
        rx.blocking_recv(),
        Some(Uevent::Event {
            action: "change".into(),
            subsystem: "backlight".into(),
            devpath: BACKLIGHT.into(),
        })
    );
    fake.lose();
    assert_eq!(rx.blocking_recv(), Some(Uevent::Lost));
    drop(rx);
    assert_eq!(fake.watching(), 0);
}

/// A raiz de teste: a escuta abre, fica calada e não fecha.
#[test]
fn no_uevents_stays_open_and_silent() {
    let source = NoUevents::default();
    let mut rx = source.watch().unwrap();
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    drop(rx);
    // Uma escuta nova depois de largar a anterior também.
    let mut rx = source.watch().unwrap();
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
}
