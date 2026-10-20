use lucernad_system::Roots;
use lucernad_system::info::{parse_os_release, read_info};
use lucernad_system::testing::testdata;

#[test]
fn machine_info() {
    let i = read_info(&Roots::at(&testdata("machine")));
    assert_eq!(
        serde_json::to_value(&i).unwrap(),
        serde_json::json!({
        "cpuModel": "Intel(R) Core(TM) i7-1165G7", "cpuThreads": 2,
        "hostname": "maquina", "osName": "Arch Linux", "kernel": "6.17.2-arch1-1"})
    );
}

#[test]
fn missing_files_are_empty() {
    let t = tempfile::tempdir().unwrap();
    let i = read_info(&Roots::at(t.path()));
    assert_eq!(
        (i.cpu_model.as_str(), i.cpu_threads, i.hostname.as_str()),
        ("", 0, "")
    );
    assert_eq!(parse_os_release("NAME=x\n"), "");
    assert_eq!(
        parse_os_release("PRETTY_NAME=Debian GNU/Linux 13\n"),
        "Debian GNU/Linux 13",
        "sem aspas"
    );
}
