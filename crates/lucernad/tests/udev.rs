//! A regra udev e o `modules-load` que acompanham o binário.

#[test]
fn udev_rule_only_matches_gpu_classes() {
    let rules = include_str!("../data/60-lucernad-i2c.rules");
    let lines: Vec<&str> = rules
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    assert_eq!(
        lines,
        [r#"SUBSYSTEM=="i2c-dev", KERNEL=="i2c-[0-9]*", ATTRS{class}=="0x03*", TAG+="uaccess""#]
    );
    assert_eq!(include_str!("../data/lucernad-i2c.conf"), "i2c-dev\n");
}
