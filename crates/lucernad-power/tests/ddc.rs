use std::io;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use lucernad_power::ddc::{
    DdcError, EDID_ADDR, Vcp, get_brightness, get_request, parse_reply, read_edid, set_brightness,
    set_request,
};
use lucernad_power::i2c::{I2cBus, I2cOpener};
use lucernad_power::testing::{FakeI2c, FakeMonitor, edid, fast_timing};

/// O LG da sonda (c), no barramento 1.
fn lg() -> FakeMonitor {
    FakeMonitor {
        edid: edid("LG ULTRAWIDE", 1),
        current: 70,
        max: 100,
        off_by: 0,
    }
}

fn with_lg() -> FakeI2c {
    FakeI2c::new().monitor(1, lg())
}

fn open(i2c: &FakeI2c, bus: u32) -> Box<dyn I2cBus> {
    i2c.open(bus).unwrap()
}

/// O XOR de `init` com todos os bytes.
fn xor(init: u8, bytes: &[u8]) -> u8 {
    bytes.iter().fold(init, |acc, b| acc ^ b)
}

#[test]
fn requests_match_the_standard() {
    assert_eq!(get_request(0x10), vec![0x51, 0x82, 0x01, 0x10, 0xAC]);
    assert_eq!(
        set_request(0x10, 70),
        vec![0x51, 0x84, 0x03, 0x10, 0x00, 0x46, 0xEE]
    );
}

#[test]
fn reply_parsing() {
    let good = [
        0x6E, 0x88, 0x02, 0x00, 0x10, 0x00, 0x00, 0x64, 0x00, 0x46, 0x86,
    ];
    assert_eq!(
        parse_reply(0x10, &good),
        Ok(Vcp {
            current: 70,
            max: 100
        })
    );

    let mut bad_checksum = good;
    bad_checksum[10] = 0x87;
    assert_eq!(parse_reply(0x10, &bad_checksum), Err(DdcError::BadReply));

    // A "null message": o monitor está ocupado.
    assert_eq!(
        parse_reply(0x10, &[0x6E, 0x80, 0xBE]),
        Err(DdcError::NoAnswer)
    );
    // A mesma, como sai do `read` de 11 bytes (o resto em zeros).
    let mut null11 = [0u8; 11];
    null11[..3].copy_from_slice(&[0x6E, 0x80, 0xBE]);
    assert_eq!(parse_reply(0x10, &null11), Err(DdcError::NoAnswer));

    // Vazia e curta demais.
    assert_eq!(parse_reply(0x10, &[]), Err(DdcError::BadReply));
    assert_eq!(parse_reply(0x10, &good[..5]), Err(DdcError::BadReply));

    // Resultado 1: o monitor não tem esse VCP.
    let mut unsupported = good;
    unsupported[3] = 0x01;
    unsupported[10] = xor(0x50, &unsupported[..10]);
    assert_eq!(parse_reply(0x10, &unsupported), Err(DdcError::Unsupported));

    // Outro código de VCP na resposta.
    let mut other = good;
    other[4] = 0x12;
    other[10] = xor(0x50, &other[..10]);
    assert_eq!(parse_reply(0x10, &other), Err(DdcError::BadReply));

    // Máximo 0 não serve para percentual.
    let mut zero_max = good;
    zero_max[7] = 0x00;
    zero_max[10] = xor(0x50, &zero_max[..10]);
    assert_eq!(parse_reply(0x10, &zero_max), Err(DdcError::BadReply));
}

#[test]
fn get_and_set_through_the_fake() {
    let i2c = with_lg();
    let mut bus = open(&i2c, 1);
    let t = fast_timing();
    assert_eq!(
        get_brightness(bus.as_mut(), &t),
        Ok(Vcp {
            current: 70,
            max: 100
        })
    );
    assert_eq!(
        set_brightness(bus.as_mut(), 40, &t),
        Ok(Vcp {
            current: 40,
            max: 100
        })
    );
    assert_eq!(i2c.current(1), 40);

    // A releitura devolve o que o monitor guardou, não o que se pediu.
    let i2c = FakeI2c::new().monitor(1, FakeMonitor { off_by: -1, ..lg() });
    let mut bus = open(&i2c, 1);
    assert_eq!(set_brightness(bus.as_mut(), 40, &t).unwrap().current, 39);

    // O `hang` do fake prende a leitura até o `release`.
    let i2c = with_lg();
    i2c.hang(1);
    let mut bus = open(&i2c, 1);
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || tx.send(get_brightness(bus.as_mut(), &t)).unwrap());
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    i2c.release(1);
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        Ok(Vcp {
            current: 70,
            max: 100
        })
    );
    worker.join().unwrap();
}

#[test]
fn null_and_bad_replies_are_retried_then_fail() {
    let t = fast_timing();
    let vcp = Ok(Vcp {
        current: 70,
        max: 100,
    });

    let i2c = with_lg();
    i2c.busy_times(1, 2);
    assert_eq!(get_brightness(open(&i2c, 1).as_mut(), &t), vcp);

    let i2c = with_lg();
    i2c.corrupt_times(1, 3);
    assert_eq!(get_brightness(open(&i2c, 1).as_mut(), &t), vcp);
    assert_eq!(i2c.writes(1).len(), 4);

    let i2c = with_lg();
    i2c.busy_times(1, 4);
    assert_eq!(
        get_brightness(open(&i2c, 1).as_mut(), &t),
        Err(DdcError::NoAnswer)
    );
    assert_eq!(i2c.writes(1).len(), 4);

    let i2c = with_lg().silent(1);
    assert_eq!(
        get_brightness(open(&i2c, 1).as_mut(), &t),
        Err(DdcError::NoAnswer)
    );
    let writes = i2c.writes(1);
    assert_eq!(writes.len(), 4);
    assert!(writes.iter().all(|w| *w == (0x37, get_request(0x10))));

    // Um barramento sem monitor abre e só devolve zeros.
    let i2c = FakeI2c::new();
    assert_eq!(
        get_brightness(open(&i2c, 7).as_mut(), &t),
        Err(DdcError::NoAnswer)
    );
    assert_eq!(i2c.reads(7), 4);
}

#[test]
fn access_errors_are_not_retried() {
    let i2c = with_lg().denied(1);
    let err = i2c.open(1).err().unwrap();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    let ddc = DdcError::from_open(1, &err);
    assert_eq!(ddc, DdcError::NoAccess);
    assert_eq!(
        ddc.message(),
        "Sem acesso ao barramento do monitor: instale a regra udev do lucernad (veja o README)."
    );
    assert_eq!(i2c.opened(), vec![1]);
    assert!(i2c.writes(1).is_empty());

    let missing = io::Error::from_raw_os_error(2);
    assert_eq!(DdcError::from_open(5, &missing), DdcError::NoDevice(5));
    assert_eq!(
        DdcError::NoDevice(5).message(),
        "Sem o /dev/i2c-5: carregue o módulo i2c-dev (veja o README)."
    );
    let eperm = io::Error::from_raw_os_error(1);
    assert_eq!(DdcError::from_open(5, &eperm), DdcError::NoAccess);
    assert_eq!(
        DdcError::NoAnswer.message(),
        "O monitor não respondeu ao DDC/CI."
    );
    // O detalhe do errno não vai para a tela (só para o log).
    assert_eq!(
        DdcError::Io("errno 16".to_string()).message(),
        "O monitor não respondeu ao DDC/CI."
    );
    assert_eq!(
        DdcError::BadReply.message(),
        "O monitor respondeu fora do padrão DDC/CI."
    );
    assert_eq!(
        DdcError::Unsupported.message(),
        "O monitor não deixa ajustar o brilho pelo DDC/CI."
    );

    // Resultado "não suportado": uma tentativa só.
    struct Unsupported(usize);
    impl I2cBus for Unsupported {
        fn write(&mut self, _addr: u16, _data: &[u8]) -> io::Result<()> {
            self.0 += 1;
            Ok(())
        }
        fn read(&mut self, _addr: u16, buf: &mut [u8]) -> io::Result<()> {
            let mut reply = [0x6E, 0x88, 0x02, 0x01, 0x10, 0, 0, 0, 0, 0, 0];
            reply[10] = xor(0x50, &reply[..10]);
            buf.copy_from_slice(&reply[..buf.len()]);
            Ok(())
        }
        fn lock(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn unlock(&mut self) {}
    }
    let mut bus = Unsupported(0);
    assert_eq!(
        get_brightness(&mut bus, &fast_timing()),
        Err(DdcError::Unsupported)
    );
    assert_eq!(bus.0, 1);

    // Acesso negado no meio da transação: sem nova tentativa.
    struct Denied(usize);
    impl I2cBus for Denied {
        fn write(&mut self, _addr: u16, _data: &[u8]) -> io::Result<()> {
            self.0 += 1;
            Err(io::Error::from_raw_os_error(13))
        }
        fn read(&mut self, _addr: u16, _buf: &mut [u8]) -> io::Result<()> {
            Ok(())
        }
        fn lock(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn unlock(&mut self) {}
    }
    let mut bus = Denied(0);
    assert_eq!(
        set_brightness(&mut bus, 10, &fast_timing()),
        Err(DdcError::NoAccess)
    );
    assert_eq!(bus.0, 1);
}

#[test]
fn edid_is_read_at_0x50() {
    let i2c = with_lg();
    let mut bus = open(&i2c, 1);
    let got = read_edid(bus.as_mut()).unwrap();
    assert_eq!(got.to_vec(), lg().edid[..128].to_vec());
    assert_eq!(i2c.writes(1), vec![(EDID_ADDR, vec![0x00])]);
    assert_eq!(i2c.reads(1), 1);
}

#[test]
fn every_transaction_is_locked() {
    let i2c = with_lg();
    let mut bus = open(&i2c, 1);
    let t = fast_timing();
    get_brightness(bus.as_mut(), &t).unwrap();
    assert_eq!(i2c.locks(1), (1, 1));
    // O Set VCP é uma transação e a releitura, outra.
    set_brightness(bus.as_mut(), 40, &t).unwrap();
    assert_eq!(i2c.locks(1), (3, 3));
    read_edid(bus.as_mut()).unwrap();
    assert_eq!(i2c.locks(1), (4, 4));

    // Com novas tentativas, um lock por tentativa.
    let i2c = with_lg();
    i2c.busy_times(1, 2);
    get_brightness(open(&i2c, 1).as_mut(), &t).unwrap();
    assert_eq!(i2c.locks(1), (3, 3));

    // Um pânico no meio da transação ainda destrava o barramento.
    #[derive(Default)]
    struct Panics {
        locks: usize,
        unlocks: usize,
    }
    impl I2cBus for Panics {
        fn write(&mut self, _addr: u16, _data: &[u8]) -> io::Result<()> {
            panic!("o driver caiu");
        }
        fn read(&mut self, _addr: u16, _buf: &mut [u8]) -> io::Result<()> {
            Ok(())
        }
        fn lock(&mut self) -> io::Result<()> {
            self.locks += 1;
            Ok(())
        }
        fn unlock(&mut self) {
            self.unlocks += 1;
        }
    }
    let mut bus = Panics::default();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        get_brightness(&mut bus, &t)
    }));
    assert!(caught.is_err());
    assert_eq!((bus.locks, bus.unlocks), (1, 1));
}

#[test]
fn only_brightness_is_ever_written() {
    let i2c = with_lg();
    let mut bus = open(&i2c, 1);
    let t = fast_timing();
    get_brightness(bus.as_mut(), &t).unwrap();
    set_brightness(bus.as_mut(), 40, &t).unwrap();
    set_brightness(bus.as_mut(), u16::MAX, &t).unwrap();
    read_edid(bus.as_mut()).unwrap();
    let writes = i2c.writes(1);
    assert_eq!(writes.len(), 6);
    for (addr, data) in writes {
        let ok = matches!(
            (addr, data.as_slice()),
            (0x37, [0x51, _, 0x01, 0x10, _])
                | (0x37, [0x51, _, 0x03, 0x10, _, _, _])
                | (0x50, [0x00])
        );
        assert!(ok, "escrita fora do permitido: {addr:#x} {data:02x?}");
    }
}
