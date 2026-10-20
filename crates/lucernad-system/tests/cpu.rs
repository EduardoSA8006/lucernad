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
