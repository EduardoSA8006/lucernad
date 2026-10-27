//! O diário do modo jogo numa pasta temporária.

use std::os::unix::fs::PermissionsExt;

use lucernad_game::journal::{Entry, Journal, Store};
use lucernad_hyprland::options::OptionValue;

fn sample() -> Journal {
    Journal {
        instance: "sig_1".into(),
        entries: vec![Entry {
            option: "misc:vrr".into(),
            before: OptionValue::Int(0),
            applied: OptionValue::Int(2),
        }],
        tearing_windows: vec!["0xab".into()],
    }
}

#[test]
fn save_load_and_clear() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().join("lucerna/game"));
    assert_eq!(store.load(), None);
    store.save(&sample()).unwrap();
    assert_eq!(store.load(), Some(sample()));
    let mode = std::fs::metadata(dir.path().join("lucerna/game"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700, "a pasta é privada");
    let file = std::fs::metadata(store.path()).unwrap().permissions().mode();
    assert_eq!(file & 0o777, 0o600, "o arquivo também");
    assert!(store.path().ends_with("lucerna/game/restore.json"));
    assert!(!dir.path().join("lucerna/game/restore.json.tmp").exists());
    store.clear();
    assert_eq!(store.load(), None);
    store.clear(); // sem arquivo, nada acontece
}

#[test]
fn broken_journal_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().to_path_buf());
    std::fs::write(store.path(), "{não é json").unwrap();
    assert_eq!(store.load(), None);
    assert!(!store.path().exists(), "o diário estragado é apagado");
}
