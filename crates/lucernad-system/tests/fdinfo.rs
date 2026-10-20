use std::collections::BTreeMap;
use std::os::unix::fs::symlink;
use std::sync::atomic::{AtomicU64, Ordering};

use lucernad_system::Roots;
use lucernad_system::fdinfo::{DrmClient, DrmSnapshot, drm_busy, parse_fdinfo, scan};
use lucernad_system::testing::testdata;

const GPU: &str = "0000:00:02.0";
const SEC: f64 = 2e9;
const INTERVAL: u64 = 2000;

fn read(rel: &str) -> String {
    std::fs::read_to_string(testdata(rel)).unwrap()
}

/// Um cliente: `(id, pdev, driver, valores)`.
type C<'a> = (&'a str, &'a str, &'a str, &'a [(&'a str, f64)]);

fn snap(ts: f64, clients: &[C]) -> DrmSnapshot {
    DrmSnapshot {
        ts_ns: ts as u64,
        clients: clients
            .iter()
            .map(|(id, pdev, driver, values)| {
                let client = DrmClient {
                    driver: driver.to_string(),
                    pdev: pdev.to_string(),
                    values: values.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
                };
                ((pdev.to_string(), id.to_string()), client)
            })
            .collect(),
        nvidia_open: false,
    }
}

fn near(v: Option<f64>, want: f64) {
    let v = v.unwrap_or_else(|| panic!("esperava {want}, veio None"));
    assert!((v - want).abs() < 1e-9, "esperava {want}, veio {v}");
}

#[test]
fn parse_i915_xe_and_bad_lines() {
    let (id, c) = parse_fdinfo(&read("fdinfo/i915.txt")).unwrap();
    assert_eq!(
        (id.as_str(), c.driver.as_str(), c.pdev.as_str()),
        ("7", "i915", "0000:00:02.0")
    );
    assert_eq!(
        c.values,
        BTreeMap::from([
            ("engine-capacity-video".into(), 2.0),
            ("engine-copy".into(), 0.0),
            ("engine-render".into(), 500.0)
        ])
    );
    let (id, c) = parse_fdinfo(&read("fdinfo/xe.txt")).unwrap();
    assert_eq!(
        (
            id.as_str(),
            c.values["cycles-rcs"],
            c.values["total-cycles-rcs"]
        ),
        ("9", 10.0, 40.0)
    );
    assert_eq!(parse_fdinfo(&read("fdinfo/no-client.txt")), None);
    assert_eq!(parse_fdinfo("drm-client-id:\t3\n"), None, "sem drm-driver");
}

#[test]
fn scan_reads_only_dri_fds_once_per_client() {
    let t = tempfile::tempdir().unwrap();
    let proc = t.path().join("proc");
    let fd = |pid: &str, n: &str, target: &str, fdinfo: Option<&str>| {
        let dir = proc.join(pid);
        std::fs::create_dir_all(dir.join("fd")).unwrap();
        std::fs::create_dir_all(dir.join("fdinfo")).unwrap();
        symlink(target, dir.join("fd").join(n)).unwrap();
        if let Some(rel) = fdinfo {
            std::fs::write(dir.join("fdinfo").join(n), read(rel)).unwrap();
        }
    };
    fd("100", "3", "/dev/dri/renderD128", Some("fdinfo/i915.txt"));
    fd("100", "4", "/dev/null", Some("fdinfo/xe.txt"));
    fd("200", "5", "/dev/dri/card0", Some("fdinfo/i915.txt"));
    fd("300", "6", "/dev/nvidia0", None);
    // O próprio processo fica de fora, mesmo com a NVIDIA aberta.
    fd(&std::process::id().to_string(), "7", "/dev/nvidiactl", None);
    std::fs::create_dir_all(proc.join("self")).unwrap();
    let roots = Roots {
        procfs: proc.clone(),
        ..Roots::at(t.path())
    };
    let ticks = AtomicU64::new(0);
    let clock = || 10 + 20 * ticks.fetch_add(1, Ordering::SeqCst);
    let snap = scan(&roots, &clock);
    assert_eq!(snap.ts_ns, 20);
    assert_eq!(
        snap.clients.keys().cloned().collect::<Vec<_>>(),
        [("0000:00:02.0".to_string(), "7".to_string())]
    );
    assert!(snap.nvidia_open, "alguém está com a NVIDIA aberta");

    // Só o nvidiactl (não é placa) e só o próprio processo: ninguém usando a NVIDIA.
    let t = tempfile::tempdir().unwrap();
    let proc = t.path().join("proc");
    for (pid, target) in [
        ("400".to_string(), "/dev/nvidiactl"),
        ("401".to_string(), "/dev/nvidia-modeset"),
        (std::process::id().to_string(), "/dev/nvidia0"),
    ] {
        std::fs::create_dir_all(proc.join(&pid).join("fd")).unwrap();
        symlink(target, proc.join(&pid).join("fd/3")).unwrap();
    }
    let roots = Roots {
        procfs: proc,
        ..Roots::at(t.path())
    };
    let snap = scan(&roots, &|| 5);
    assert_eq!(
        (snap.ts_ns, snap.clients.len(), snap.nvidia_open),
        (5, 0, false)
    );
}

#[test]
fn busiest_class_of_one_client() {
    let a = snap(
        0.0,
        &[(
            "1",
            GPU,
            "i915",
            &[("engine-render", 100.0), ("engine-video", 0.0)],
        )],
    );
    let b = snap(
        SEC,
        &[(
            "1",
            GPU,
            "i915",
            &[
                ("engine-render", 100.0 + 0.25 * SEC),
                ("engine-video", 0.5 * SEC),
            ],
        )],
    );
    near(drm_busy(&a, &b, GPU, INTERVAL), 0.5);
}

#[test]
fn sums_clients_and_caps_at_one() {
    let a = snap(
        0.0,
        &[
            ("1", GPU, "i915", &[("engine-render", 0.0)]),
            ("2", GPU, "i915", &[("engine-render", 0.0)]),
        ],
    );
    let b = snap(
        SEC,
        &[
            ("1", GPU, "i915", &[("engine-render", 0.25 * SEC)]),
            ("2", GPU, "i915", &[("engine-render", 0.5 * SEC)]),
        ],
    );
    near(drm_busy(&a, &b, GPU, INTERVAL), 0.75);
    let c = snap(
        SEC,
        &[
            ("1", GPU, "i915", &[("engine-render", 0.9 * SEC)]),
            ("2", GPU, "i915", &[("engine-render", 0.9 * SEC)]),
        ],
    );
    assert_eq!(drm_busy(&a, &c, GPU, INTERVAL), Some(1.0));
}

#[test]
fn client_gone_between_reads_is_left_out() {
    let a = snap(
        0.0,
        &[
            ("1", GPU, "i915", &[("engine-render", 0.0)]),
            ("2", GPU, "i915", &[("engine-render", 5.0 * SEC)]),
        ],
    );
    let b = snap(
        SEC,
        &[
            ("1", GPU, "i915", &[("engine-render", 0.2 * SEC)]),
            ("3", GPU, "i915", &[("engine-render", 9.0 * SEC)]),
        ],
    );
    near(drm_busy(&a, &b, GPU, INTERVAL), 0.2);
}

#[test]
fn xe_by_cycles() {
    let v = |c: f64, tot: f64| {
        [
            ("cycles-rcs", c),
            ("total-cycles-rcs", tot),
            ("cycles-vcs", 0.0),
            ("total-cycles-vcs", tot),
        ]
    };
    let (a1, a2, b1, b2) = (
        v(100.0, 1000.0),
        v(0.0, 1000.0),
        v(300.0, 2000.0),
        v(100.0, 2000.0),
    );
    let a = snap(0.0, &[("1", GPU, "xe", &a1), ("2", GPU, "xe", &a2)]);
    let b = snap(SEC, &[("1", GPU, "xe", &b1), ("2", GPU, "xe", &b2)]);
    near(drm_busy(&a, &b, GPU, INTERVAL), 0.3);
}

#[test]
fn other_card_is_ignored() {
    let other = "0000:01:00.0";
    let a = snap(
        0.0,
        &[
            ("1", GPU, "i915", &[("engine-render", 0.0)]),
            ("1", other, "i915", &[("engine-render", 0.0)]),
        ],
    );
    let b = snap(
        SEC,
        &[
            ("1", GPU, "i915", &[("engine-render", 0.1 * SEC)]),
            ("1", other, "i915", &[("engine-render", 0.9 * SEC)]),
        ],
    );
    near(drm_busy(&a, &b, GPU, INTERVAL), 0.1);
    near(drm_busy(&a, &b, other, INTERVAL), 0.9);
    assert_eq!(
        drm_busy(&a, &b, "0000:09:00.0", INTERVAL),
        None,
        "sem cliente na placa"
    );
}

#[test]
fn none_without_window_or_common_client() {
    let b = snap(SEC, &[("1", GPU, "i915", &[("engine-render", SEC)])]);
    assert_eq!(
        drm_busy(
            &DrmSnapshot::default(),
            &DrmSnapshot::default(),
            GPU,
            INTERVAL
        ),
        None,
        "janela zero"
    );
    assert_eq!(drm_busy(&b, &b, GPU, INTERVAL), None, "ts igual");
    let later = snap(SEC, &[("1", GPU, "i915", &[("engine-render", SEC)])]);
    let earlier = snap(0.0, &[("1", GPU, "i915", &[("engine-render", 0.0)])]);
    assert_eq!(
        drm_busy(&later, &earlier, GPU, INTERVAL),
        None,
        "relógio voltando"
    );
    let other = snap(0.0, &[("2", GPU, "i915", &[("engine-render", 0.0)])]);
    assert_eq!(
        drm_busy(&other, &b, GPU, INTERVAL),
        None,
        "nenhum cliente em comum"
    );
}

#[test]
fn counter_going_back_counts_zero() {
    let a = snap(
        0.0,
        &[
            ("1", GPU, "i915", &[("engine-render", 5.0 * SEC)]),
            ("2", GPU, "i915", &[("engine-render", 0.0)]),
        ],
    );
    let b = snap(
        SEC,
        &[
            ("1", GPU, "i915", &[("engine-render", 1.0)]),
            ("2", GPU, "i915", &[("engine-render", 0.3 * SEC)]),
        ],
    );
    near(drm_busy(&a, &b, GPU, INTERVAL), 0.3);
    let c = snap(SEC, &[("1", GPU, "i915", &[("engine-render", 1.0)])]);
    assert_eq!(drm_busy(&a, &c, GPU, INTERVAL), Some(0.0));
}

#[test]
fn stale_previous_read_is_dropped() {
    let a = snap(0.0, &[("1", GPU, "i915", &[("engine-render", 0.0)])]);
    let ok = snap(6e9, &[("1", GPU, "i915", &[("engine-render", 3e9)])]);
    let old = snap(6e9 + 1.0, &[("1", GPU, "i915", &[("engine-render", 3e9)])]);
    near(drm_busy(&a, &ok, GPU, INTERVAL), 0.5);
    assert_eq!(
        drm_busy(&a, &old, GPU, INTERVAL),
        None,
        "janela de mais de 6 s com intervalo de 2 s"
    );
}

#[test]
fn divided_by_class_capacity() {
    let v = |r: f64, vid: f64| {
        [
            ("engine-render", r),
            ("engine-video", vid),
            ("engine-capacity-video", 2.0),
        ]
    };
    let (a1, b1, b2) = (v(0.0, 0.0), v(0.1 * SEC, 0.8 * SEC), v(0.0, 0.8 * SEC));
    let a = snap(0.0, &[("1", GPU, "i915", &a1), ("2", GPU, "i915", &a1)]);
    let b = snap(SEC, &[("1", GPU, "i915", &b1), ("2", GPU, "i915", &b2)]);
    // video: 1,6 s ocupados em 2 s de janela com 2 motores = 0,8 (capacidade fora da conta de ns)
    near(drm_busy(&a, &b, GPU, INTERVAL), 0.8);
}

#[test]
fn first_read_against_default_is_none() {
    // O primeiro ciclo do Sampler: a anterior é a vazia (ts 0, sem clientes).
    let first = DrmSnapshot::default();
    let cur = snap(
        0.5e9,
        &[
            ("1", GPU, "i915", &[("engine-render", 0.4e9)]),
            (
                "2",
                GPU,
                "xe",
                &[("cycles-rcs", 10.0), ("total-cycles-rcs", 40.0)],
            ),
        ],
    );
    assert_eq!(
        drm_busy(&first, &cur, GPU, INTERVAL),
        None,
        "sem cliente em comum com a vazia, mesmo dentro da janela"
    );
    let late = DrmSnapshot {
        ts_ns: 60_000_000_000,
        ..cur.clone()
    };
    assert_eq!(drm_busy(&first, &late, GPU, INTERVAL), None, "janela velha");
    // A segunda leitura, contra a primeira de verdade, já dá número.
    let next = snap(
        0.5e9 + SEC,
        &[("1", GPU, "i915", &[("engine-render", 0.4e9 + 0.5 * SEC)])],
    );
    near(drm_busy(&cur, &next, GPU, INTERVAL), 0.5);
}
