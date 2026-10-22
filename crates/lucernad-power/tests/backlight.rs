use std::ffi::OsStr;

use lucernad_power::backlight::{Backlight, choose, read_value};
use lucernad_power::roots::root_from_env;
use lucernad_power::testing::Tree;
use tempfile::TempDir;

fn tree() -> (TempDir, Tree) {
    let dir = TempDir::new().unwrap();
    let tree = Tree::new(dir.path());
    (dir, tree)
}

fn chosen(tree: &Tree) -> Option<String> {
    choose(&tree.roots()).map(|b| b.device)
}

#[test]
fn firmware_beats_platform_beats_raw() {
    let (_dir, tree) = tree();
    tree.backlight("intel_backlight", "raw", 19200, 9600, 9600, None);
    tree.backlight("p", "platform", 10, 5, 5, None);
    tree.backlight("acpi_video0", "firmware", 100, 50, 50, None);
    assert_eq!(chosen(&tree).as_deref(), Some("acpi_video0"));

    let (_dir, tree) = self::tree();
    tree.backlight("intel_backlight", "raw", 19200, 9600, 9600, None);
    tree.backlight("p", "platform", 10, 5, 5, None);
    assert_eq!(chosen(&tree).as_deref(), Some("p"), "sem o firmware");

    let (_dir, tree) = self::tree();
    tree.backlight("b", "raw", 100, 50, 50, None);
    tree.backlight("a", "raw", 100, 50, 50, None);
    assert_eq!(chosen(&tree).as_deref(), Some("a"), "no mesmo tipo, o nome");
}

#[test]
fn value_comes_from_brightness_not_actual() {
    // A tampa fechada da sonda (c): o painel desligado dá 0 no actual.
    let (_dir, tree) = tree();
    tree.backlight("intel_backlight", "raw", 19200, 19200, 0, None);
    assert_eq!(
        choose(&tree.roots()),
        Some(Backlight {
            device: "intel_backlight".into(),
            max: 19200,
            output: None,
        })
    );
    assert_eq!(read_value(&tree.roots(), "intel_backlight").unwrap(), 19200);
}

#[test]
fn output_from_the_device_link() {
    let (_dir, tree) = tree();
    tree.backlight(
        "intel_backlight",
        "raw",
        19200,
        9600,
        9600,
        Some("card2-eDP-1"),
    );
    let chosen = choose(&tree.roots()).unwrap();
    assert_eq!(chosen.output.as_deref(), Some("eDP-1"));

    let (_dir, tree) = self::tree();
    tree.backlight("intel_backlight", "raw", 19200, 9600, 9600, None);
    assert_eq!(choose(&tree.roots()).unwrap().output, None, "sem device");
}

#[test]
fn zero_max_is_not_a_backlight() {
    let (_dir, tree) = tree();
    tree.backlight("intel_backlight", "raw", 0, 0, 0, None);
    assert_eq!(choose(&tree.roots()), None, "max 0");

    let (_dir, tree) = self::tree();
    assert_eq!(choose(&tree.roots()), None, "nenhuma pasta");
}

#[test]
fn env_root_is_checked() {
    assert!(matches!(root_from_env(None, false), Ok(None)));
    assert!(matches!(
        root_from_env(Some(OsStr::new("")), false),
        Ok(None)
    ));

    let dir = TempDir::new().unwrap();
    let roots = root_from_env(Some(dir.path().as_os_str()), false)
        .unwrap()
        .unwrap();
    let base = dir.path().canonicalize().unwrap();
    assert_eq!(roots.sysfs, base.join("sys"));
    assert_eq!(roots.dev, base.join("dev"));

    let missing = dir.path().join("nao-existe");
    for bad in [
        OsStr::new("rel"),
        OsStr::new("/"),
        OsStr::new("/sys/class"),
        OsStr::new("/dev"),
        OsStr::new("/proc/self"),
        missing.as_os_str(),
    ] {
        let err = root_from_env(Some(bad), false).unwrap_err();
        assert_eq!(
            err,
            "LUCERNAD_POWER_ROOT inválido: precisa ser uma pasta de teste fora do /sys, do /dev e do /proc.",
            "{bad:?}"
        );
    }

    // A comparação é por componente: "sysfoo" não é o /sys.
    let sysfoo = dir.path().join("sysfoo");
    std::fs::create_dir(&sysfoo).unwrap();
    let roots = root_from_env(Some(sysfoo.as_os_str()), false)
        .unwrap()
        .unwrap();
    assert_eq!(roots.sysfs, sysfoo.canonicalize().unwrap().join("sys"));
}

#[test]
fn container_without_root_is_refused() {
    let container = "Em container, o módulo de energia só roda com LUCERNAD_POWER_ROOT (uma árvore falsa): sem ela, o brilho do host mudaria.";
    assert_eq!(root_from_env(None, true).unwrap_err(), container);
    assert_eq!(
        root_from_env(Some(OsStr::new("")), true).unwrap_err(),
        container
    );

    let dir = TempDir::new().unwrap();
    let roots = root_from_env(Some(dir.path().as_os_str()), true)
        .unwrap()
        .unwrap();
    assert_eq!(roots.sysfs, dir.path().canonicalize().unwrap().join("sys"));
}
