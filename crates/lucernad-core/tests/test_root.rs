//! A raiz de teste do `LUCERNAD_POWER_ROOT`, comum ao `power` e ao `game`.

use std::ffi::OsStr;

use lucernad_core::test_root::{GAMEMODE_DIR, POWER_ROOT, PROFILES_DIR, forbidden, test_base};
use tempfile::TempDir;

#[test]
fn names_of_the_fake_tree() {
    assert_eq!(POWER_ROOT, "LUCERNAD_POWER_ROOT");
    assert_eq!(PROFILES_DIR, "power-profiles");
    assert_eq!(GAMEMODE_DIR, "gamemode");
}

#[test]
fn base_is_checked() {
    assert_eq!(test_base(None), Ok(None));
    assert_eq!(test_base(Some(OsStr::new(""))), Ok(None));

    let dir = TempDir::new().unwrap();
    let base = dir.path().canonicalize().unwrap();
    assert_eq!(test_base(Some(dir.path().as_os_str())), Ok(Some(base)));

    let missing = dir.path().join("nao-existe");
    let file = dir.path().join("arquivo");
    std::fs::write(&file, "").unwrap();
    let to_sys = dir.path().join("para-o-sys");
    std::os::unix::fs::symlink("/sys", &to_sys).unwrap();
    for bad in [
        OsStr::new("rel"),
        OsStr::new("/"),
        OsStr::new("/sys/class"),
        OsStr::new("/dev"),
        OsStr::new("/proc/self"),
        missing.as_os_str(),
        file.as_os_str(),
        to_sys.as_os_str(),
    ] {
        assert_eq!(test_base(Some(bad)), Err(()), "{bad:?}");
    }
}

#[test]
fn forbidden_is_by_component() {
    assert!(forbidden(std::path::Path::new("/")));
    assert!(forbidden(std::path::Path::new("/sys/class")));
    assert!(!forbidden(std::path::Path::new("/sysfoo")));
    assert!(!forbidden(std::path::Path::new("/tmp/x")));
}

#[test]
fn inner_dir_stays_in_the_root() {
    use lucernad_core::test_root::inner_dir;
    let dir = TempDir::new().unwrap();
    let base = dir.path().canonicalize().unwrap();
    assert_eq!(inner_dir(&base, "power-profiles"), None);
    std::fs::create_dir(base.join("power-profiles")).unwrap();
    assert_eq!(
        inner_dir(&base, "power-profiles"),
        Some(base.join("power-profiles"))
    );
    let outside = TempDir::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), base.join("gamemode")).unwrap();
    assert_eq!(inner_dir(&base, "gamemode"), None);
}
