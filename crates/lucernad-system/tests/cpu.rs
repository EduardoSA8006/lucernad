use lucernad_system::cpu::{CpuTimes, cpu_usage, parse_stat};
use lucernad_system::testing::testdata;

#[test]
fn usage_between_two_reads() {
    let a = parse_stat(&std::fs::read_to_string(testdata("machine/proc/stat")).unwrap()).unwrap();
    assert_eq!(
        a,
        CpuTimes {
            idle: 800,
            total: 1000
        }
    );
    let b = parse_stat(&std::fs::read_to_string(testdata("cases/stat.b")).unwrap()).unwrap();
    assert!((cpu_usage(a, b).unwrap() - 0.2).abs() < 1e-9);
    assert_eq!(cpu_usage(b, a), None, "total que não cresce: sem leitura");
    assert_eq!(cpu_usage(a, a), None);
    assert_eq!(parse_stat("lixo"), None);
}

#[test]
fn malformed_stat_and_idle_going_back() {
    assert_eq!(parse_stat("cpu 1 2"), None, "menos de 4 campos");
    assert_eq!(parse_stat("cpu a b c d"), None, "campos não numéricos");
    let a = CpuTimes {
        idle: 800,
        total: 1000,
    };
    let b = CpuTimes {
        idle: 700,
        total: 1100,
    };
    assert_eq!(
        cpu_usage(a, b),
        Some(1.0),
        "idle que volta conta 0 e o uso fica em 1"
    );
}
