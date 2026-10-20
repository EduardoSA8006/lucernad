use lucernad_system::net::{NetTotals, is_physical, parse_net_dev, push_history, rate};
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
