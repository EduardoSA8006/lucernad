use lucernad_system::net::{
    NetTotals, is_physical, parse_interfaces, parse_net_dev, push_history, rate, rates,
};
use lucernad_system::testing::testdata;

#[test]
fn physical_and_virtual_interfaces() {
    assert!(is_physical("enp3s0") && is_physical("wlan0"));
    for name in [
        "lo", "docker0", "veth1a2b", "br-123", "virbr0", "vnet3", "tun0", "tap0", "wg0", "zt5",
    ] {
        assert!(!is_physical(name), "{name}");
    }
}

#[test]
fn totals_only_from_physical() {
    let t = parse_net_dev(&std::fs::read_to_string(testdata("machine/proc/net/dev")).unwrap());
    assert_eq!(t, NetTotals { rx: 1000, tx: 2000 });
}

#[test]
fn rate_and_history() {
    assert_eq!(rate(1000, 3000, 2.0), 1000.0);
    assert_eq!(rate(3000, 1000, 2.0), 0.0, "contador que volta conta 0");
    assert_eq!(rate(1000, 3000, 0.0), 0.0, "janela zero: 0, não NaN");
    let mut h: Vec<f64> = (0..30).map(f64::from).collect();
    push_history(&mut h, 99.0);
    assert_eq!((h.len(), h[0], h[29]), (30, 1.0, 99.0));
    let mut short = vec![1.0, 2.0];
    push_history(&mut short, 3.0);
    assert_eq!(short, vec![1.0, 2.0, 3.0]);
}

#[test]
fn malformed_net_dev_lines() {
    let text = "cabeçalho\ncabeçalho\nlinha sem dois pontos\n  eth0: 1000 x 0 0 0 0 0 0 2000\n  wlan0: abc 0 0 0 0 0 0 0 def\n";
    assert_eq!(
        parse_net_dev(text),
        NetTotals { rx: 1000, tx: 2000 },
        "linha sem `:` fica fora; campo não numérico conta 0"
    );
}

#[test]
fn interfaces_one_by_one() {
    let text = std::fs::read_to_string(testdata("machine/proc/net/dev")).unwrap();
    let ifaces = parse_interfaces(&text);
    assert_eq!(ifaces.len(), 1, "só as físicas");
    assert_eq!(ifaces["eth0"], NetTotals { rx: 1000, tx: 2000 });
}

#[test]
fn rates_only_from_interfaces_in_both_readings() {
    let map = |l: &[(&str, u64, u64)]| {
        l.iter()
            .map(|(n, rx, tx)| (n.to_string(), NetTotals { rx: *rx, tx: *tx }))
            .collect()
    };
    let prev = map(&[("eth0", 1000, 2000), ("wlan0", 5000, 5000)]);
    let cur = map(&[("eth0", 3000, 2400), ("usb0", 900_000, 900_000)]);
    assert_eq!(
        rates(&prev, &cur, 2.0),
        (1000.0, 200.0),
        "a que sumiu e a que apareceu não entram"
    );
    let back = map(&[("eth0", 10, 10)]);
    assert_eq!(
        rates(&prev, &back, 2.0),
        (0.0, 0.0),
        "contador que volta conta 0"
    );
}
