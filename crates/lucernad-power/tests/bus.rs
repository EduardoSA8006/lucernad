use std::collections::HashMap;
use std::path::PathBuf;

use lucernad_power::bus::{
    BacklightWriter, MAX_POINTS, NoBus, Point, PowerBus, Profiles, ROOT_FORBIDDEN, TreeWriter,
    clip_error, points_from, profiles_from,
};
use lucernad_power::roots::Roots;
use lucernad_power::testing::{Tree, isolated};
use tempfile::TempDir;
use zbus::zvariant::{OwnedValue, Value};

fn point(time: u32, rate: f64, state: u32) -> Point {
    Point { time, rate, state }
}

#[test]
fn history_is_sorted_and_clean() {
    let points = points_from(vec![
        (3, 2.5, 4),
        (1, -1.0, 2),
        (2, f64::NAN, 1),
        (0, 4.3, 1),
    ]);
    assert_eq!(points, vec![point(0, 4.3, 1), point(3, 2.5, 4)]);

    // Infinito também fica fora; a taxa 0 (o estado 4 da sonda d) fica.
    let points = points_from(vec![(5, f64::INFINITY, 1), (6, 0.0, 4)]);
    assert_eq!(points, vec![point(6, 0.0, 4)]);

    // O UPower manda o mais novo primeiro: 600 pontos viram os 500 mais novos.
    let raw: Vec<_> = (0..600u32).rev().map(|t| (t, 1.0, 2)).collect();
    let points = points_from(raw);
    assert_eq!(points.len(), MAX_POINTS);
    assert_eq!(points.first().map(|p| p.time), Some(100));
    assert_eq!(points.last().map(|p| p.time), Some(599));
}

fn profile(name: &str) -> HashMap<String, OwnedValue> {
    let value = OwnedValue::try_from(Value::from(name)).unwrap();
    let driver = OwnedValue::try_from(Value::from("platform_profile")).unwrap();
    HashMap::from([
        ("Profile".to_string(), value),
        ("Driver".to_string(), driver),
    ])
}

#[test]
fn profiles_from_the_property() {
    // A lista da sonda (d).
    let all = [
        profile("power-saver"),
        profile("balanced"),
        profile("performance"),
    ];
    assert_eq!(
        profiles_from(&all),
        Profiles {
            available: true,
            performance: true
        }
    );
    let without = [profile("power-saver"), profile("balanced")];
    assert_eq!(
        profiles_from(&without),
        Profiles {
            available: true,
            performance: false
        }
    );
    assert_eq!(
        profiles_from(&[]),
        Profiles {
            available: true,
            performance: false
        }
    );
    // Um `Profile` que não é texto não conta como o de desempenho.
    let odd = [HashMap::from([(
        "Profile".to_string(),
        OwnedValue::from(7u32),
    )])];
    assert!(!profiles_from(&odd).performance);
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn actual(dir: &TempDir, device: &str) -> String {
    let path = dir
        .path()
        .join("sys/class/backlight")
        .join(device)
        .join("actual_brightness");
    std::fs::read_to_string(path).unwrap().trim().to_string()
}

#[test]
fn tree_writer_writes_the_fake_file() {
    let dir = TempDir::new().unwrap();
    let tree = Tree::new(dir.path());
    tree.backlight("lucerna_teste", "raw", 1000, 500, 500, None);
    let writer = TreeWriter {
        roots: tree.roots(),
    };

    runtime()
        .block_on(writer.write("lucerna_teste", 700))
        .unwrap();
    assert_eq!(tree.value("lucerna_teste"), 700);
    assert_eq!(actual(&dir, "lucerna_teste"), "700");

    // Um dispositivo que não existe é erro, e não cria nada.
    let err = runtime()
        .block_on(writer.write("outro", 10))
        .expect_err("sem o dispositivo");
    assert!(!err.is_empty());
    assert!(!dir.path().join("sys/class/backlight/outro").exists());
}

#[test]
fn tree_writer_refuses_links_out_of_the_root() {
    let dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let tree = Tree::new(dir.path());
    tree.backlight("lucerna_teste", "raw", 1000, 500, 500, None);
    let target = outside.path().join("brightness");
    std::fs::write(&target, "123\n").unwrap();
    let brightness = dir
        .path()
        .join("sys/class/backlight/lucerna_teste/brightness");
    std::fs::remove_file(&brightness).unwrap();
    std::os::unix::fs::symlink(&target, &brightness).unwrap();
    let writer = TreeWriter {
        roots: tree.roots(),
    };

    let err = runtime()
        .block_on(writer.write("lucerna_teste", 700))
        .expect_err("o link sai da raiz");
    assert!(err.contains("sai da raiz"), "{err}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "123\n");
    assert_eq!(
        actual(&dir, "lucerna_teste"),
        "500",
        "nada escrito quando um dos arquivos sai da raiz"
    );

    // Uma raiz que é o /dev de verdade é recusada antes de abrir qualquer
    // coisa (o dispositivo nem existe: sem a trava, o erro seria outro).
    let real = TreeWriter {
        roots: Roots {
            sysfs: PathBuf::from("/dev"),
            dev: PathBuf::from("/dev"),
        },
    };
    let err = runtime()
        .block_on(real.write("lucerna-inexistente", 1))
        .expect_err("a raiz no /dev");
    assert_eq!(err, ROOT_FORBIDDEN);

    // Um nome de dispositivo que sobe na árvore também fica fora.
    let err = runtime()
        .block_on(writer.write("../../../../outside", 1))
        .expect_err("o nome sobe na árvore");
    assert!(!err.is_empty());
}

#[test]
fn long_dbus_errors_are_clipped() {
    let long = "é".repeat(5000);
    let clipped = clip_error(&long);
    assert_eq!(clipped.chars().count(), 300);
    assert!(clipped.ends_with('…'));
    assert!(clipped.starts_with("éé"));
    // Cortar de novo não muda: quem publica pode passar por ele outra vez.
    assert_eq!(clip_error(&clipped), clipped);

    assert_eq!(clip_error("Access denied"), "Access denied");
    let exact = "a".repeat(300);
    assert_eq!(clip_error(&exact), exact);
}

#[test]
fn sonda_refuses_outside_the_container() {
    // No container da sonda a trava deixa passar: aqui não há o que conferir.
    if std::env::var_os("LUCERNA_ISOLADO").is_some() {
        return;
    }
    let payload = std::panic::catch_unwind(isolated).expect_err("a trava recusa");
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .expect("o texto do pânico");
    assert_eq!(
        message,
        "sonda.rs: só roda pelo tests/isolado.sh, num container"
    );
}

/// A raiz de teste: sem histórico e sem perfis, sem D-Bus nenhum.
#[test]
fn no_bus_has_no_history_nor_profiles() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    assert_eq!(rt.block_on(NoBus.history()), Ok(vec![]));
    let mut rx = NoBus.watch_profiles();
    assert_eq!(
        rx.try_recv().unwrap(),
        Profiles {
            available: false,
            performance: false
        }
    );
}
