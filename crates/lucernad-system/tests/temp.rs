use std::path::{Path, PathBuf};

use lucernad_system::Roots;
use lucernad_system::temp::{find_sensor, pick_sensor, read_temp};
use lucernad_system::testing::testdata;

#[test]
fn preferred_sensor() {
    let chips = |l: &[(&str, &str)]| {
        l.iter()
            .map(|(d, n)| (PathBuf::from(d), n.to_string()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        pick_sensor(&chips(&[("/x/hwmon0", "nvme")])),
        None,
        "sem sensor de CPU conhecido"
    );
    assert_eq!(
        pick_sensor(&chips(&[
            ("/x/hwmon0", "acpitz"),
            ("/x/hwmon1", "coretemp")
        ])),
        Some(PathBuf::from("/x/hwmon1/temp1_input")),
        "coretemp antes do acpitz"
    );
    let roots = Roots::at(&testdata("machine"));
    let path = find_sensor(&roots).unwrap();
    assert!(path.ends_with("hwmon1/temp1_input"));
    assert_eq!(read_temp(&path), Some(48.0));
    assert_eq!(read_temp(Path::new("/inexistente")), None);
}
