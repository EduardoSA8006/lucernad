use std::collections::BTreeMap;

use lucernad_system::Roots;
use lucernad_system::net::{
    NetTotals, is_physical, parse_interfaces, push_history, rate, rates, read_interfaces, sum,
};
use lucernad_system::testing::testdata;

/// Um sysfs falso com `class/net/<if>` para cada nome; os de `with_device`
/// ganham a pasta `device` (como o link de verdade, que aponta para o
/// dispositivo).
fn fake_net(with_device: &[&str], without: &[&str]) -> (tempfile::TempDir, Roots) {
    let t = tempfile::tempdir().unwrap();
    let roots = Roots::at(t.path());
    let net = roots.sysfs.join("class/net");
    for name in with_device {
        std::fs::create_dir_all(net.join(name).join("device")).unwrap();
    }
    for name in without {
        std::fs::create_dir_all(net.join(name)).unwrap();
    }
    (t, roots)
}

const VIRTUAL: [&str; 7] = [
    "CloudflareWARP",
    "wg0",
    "tun0",
    "docker0",
    "veth1a2b",
    "br-123",
    "lo",
];

#[test]
fn physical_means_a_device_behind() {
    let (_t, roots) = fake_net(&["wlan0", "enp3s0", "lo"], &VIRTUAL[..6]);
    assert!(is_physical(&roots, "wlan0") && is_physical(&roots, "enp3s0"));
    for name in VIRTUAL {
        assert!(!is_physical(&roots, name), "{name}");
    }
    assert!(!is_physical(&roots, "eth9"), "sem pasta no sysfs");
    assert!(!is_physical(&roots, "../wlan0") && !is_physical(&roots, ""));
}

#[test]
fn tunnel_is_not_counted_twice() {
    let (_t, roots) = fake_net(&["wlan0"], &["CloudflareWARP", "lo"]);
    std::fs::create_dir_all(roots.procfs.join("net")).unwrap();
    std::fs::write(
        roots.procfs.join("net/dev"),
        "h\nh\n    lo: 50 0 0 0 0 0 0 0 50\n  wlan0: 1000 0 0 0 0 0 0 0 2000\nCloudflareWARP: 900 0 0 0 0 0 0 0 1800\n",
    )
    .unwrap();
    let ifaces = read_interfaces(&roots);
    assert_eq!(ifaces.keys().collect::<Vec<_>>(), ["wlan0"]);
    assert_eq!(sum(&ifaces), NetTotals { rx: 1000, tx: 2000 });
}

/// Um `/proc/net/dev` em `roots` com as interfaces dadas (rx, tx).
fn write_net_dev(roots: &Roots, lines: &[(&str, u64, u64)]) {
    std::fs::create_dir_all(roots.procfs.join("net")).unwrap();
    let mut text = String::from("h\nh\n");
    for (name, rx, tx) in lines {
        text += &format!("{name}: {rx} 0 0 0 0 0 0 0 {tx}\n");
    }
    std::fs::write(roots.procfs.join("net/dev"), text).unwrap();
}

#[test]
fn without_any_device_all_but_lo_count() {
    let (_t, roots) = fake_net(&[], &["eth0", "lo"]);
    write_net_dev(&roots, &[("lo", 50, 50), ("eth0", 1000, 2000)]);
    let ifaces = read_interfaces(&roots);
    assert_eq!(
        ifaces.keys().collect::<Vec<_>>(),
        ["eth0"],
        "container: a eth0 (veth) conta, a lo não"
    );
    let (_t, roots) = fake_net(&["wlan0"], &["CloudflareWARP"]);
    write_net_dev(
        &roots,
        &[("wlan0", 1000, 2000), ("CloudflareWARP", 900, 1800)],
    );
    assert_eq!(
        read_interfaces(&roots).keys().collect::<Vec<_>>(),
        ["wlan0"],
        "com alguma física, só as físicas"
    );
}

#[test]
fn totals_only_from_physical() {
    let t = sum(&read_interfaces(&Roots::at(&testdata("machine"))));
    assert_eq!(t, NetTotals { rx: 1000, tx: 2000 }, "só a eth0 tem device");
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
        sum(&parse_interfaces(text, |_| true)),
        NetTotals { rx: 1000, tx: 2000 },
        "linha sem `:` fica fora; campo não numérico conta 0"
    );
}

#[test]
fn interfaces_one_by_one() {
    let text = std::fs::read_to_string(testdata("machine/proc/net/dev")).unwrap();
    let all = parse_interfaces(&text, |_| true);
    assert_eq!(all.len(), 3);
    let ifaces: BTreeMap<_, _> = parse_interfaces(&text, |n| n == "eth0");
    assert_eq!(ifaces.len(), 1, "só as que o filtro aceita");
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
