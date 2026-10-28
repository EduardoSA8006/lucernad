//! O perfil de energia e o GameMode falsos da raiz de teste
//! (`LUCERNAD_POWER_ROOT`): arquivos numa pasta temporária, nunca o D-Bus.

use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use lucernad_game::bus::{BusChoice, BusError, GameBus, NoGameBus, TreeGame, choose};
use tempfile::TempDir;

fn read(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path).unwrap()
}

fn tree() -> (TempDir, TreeGame) {
    let dir = TempDir::new().unwrap();
    let game = TreeGame::new(dir.path().canonicalize().unwrap());
    (dir, game)
}

#[tokio::test]
async fn without_the_folders_both_are_absent() {
    let (_dir, game) = tree();
    assert_eq!(game.hold_performance().await, Err(BusError::Absent));
    assert_eq!(game.gamemode_present().await, Ok(false));
    assert_eq!(game.register_game(4242).await, Err(BusError::Absent));
}

#[tokio::test]
async fn hold_and_release_write_the_profile() {
    let (dir, game) = tree();
    let profiles = dir.path().join("power-profiles");
    fs::create_dir(&profiles).unwrap();
    fs::write(profiles.join("active"), "power-saver\n").unwrap();

    let first = game.hold_performance().await.unwrap();
    assert_eq!(first, 1);
    assert_eq!(read(profiles.join("active")), "performance\n");
    assert_eq!(read(profiles.join("before")), "power-saver\n");
    assert_eq!(
        read(profiles.join("holds/1")),
        "performance\nModo jogo do Lucerna\nlucernad\n"
    );

    let second = game.hold_performance().await.unwrap();
    assert_eq!(second, 2);
    assert_eq!(read(profiles.join("before")), "power-saver\n");

    game.release(first).await.unwrap();
    assert!(!profiles.join("holds/1").exists());
    assert_eq!(read(profiles.join("active")), "performance\n");

    game.release(second).await.unwrap();
    assert_eq!(read(profiles.join("active")), "power-saver\n");
    assert!(!profiles.join("before").exists());
    assert_eq!(fs::read_dir(profiles.join("holds")).unwrap().count(), 0);

    assert!(matches!(game.release(99).await, Err(BusError::Failed(_))));
}

#[tokio::test]
async fn without_an_active_profile_it_goes_back_to_balanced() {
    let (dir, game) = tree();
    let profiles = dir.path().join("power-profiles");
    fs::create_dir(&profiles).unwrap();
    let cookie = game.hold_performance().await.unwrap();
    assert_eq!(read(profiles.join("before")), "balanced\n");
    game.release(cookie).await.unwrap();
    assert_eq!(read(profiles.join("active")), "balanced\n");
}

#[tokio::test]
async fn gamemode_registers_in_files() {
    let (dir, game) = tree();
    let gamemode = dir.path().join("gamemode");
    fs::create_dir(&gamemode).unwrap();
    assert_eq!(game.gamemode_present().await, Ok(true));
    game.register_game(4242).await.unwrap();
    game.register_game(77).await.unwrap();
    assert!(gamemode.join("games/4242").exists());
    assert!(gamemode.join("games/77").exists());
    game.unregister_game(4242).await.unwrap();
    assert!(!gamemode.join("games/4242").exists());
    assert!(gamemode.join("games/77").exists());
}

#[tokio::test]
async fn links_out_of_the_root_are_absent() {
    let (dir, game) = tree();
    let outside = TempDir::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("power-profiles")).unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("gamemode")).unwrap();
    assert_eq!(game.hold_performance().await, Err(BusError::Absent));
    assert_eq!(game.gamemode_present().await, Ok(false));
    assert_eq!(game.register_game(1).await, Err(BusError::Absent));
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn no_bus_is_always_absent() {
    let bus = NoGameBus;
    assert_eq!(bus.hold_performance().await, Err(BusError::Absent));
    assert_eq!(bus.release(1).await, Ok(()));
    assert_eq!(bus.gamemode_present().await, Ok(false));
    assert_eq!(bus.register_game(1).await, Err(BusError::Absent));
    assert_eq!(bus.unregister_game(1).await, Err(BusError::Absent));
}

#[test]
fn the_variable_chooses_the_bus() {
    assert_eq!(choose(None, false), BusChoice::Real);
    assert_eq!(choose(Some(OsStr::new("")), false), BusChoice::Real);

    let container = "Em container, o perfil de energia e o GameMode do modo jogo só rodam com LUCERNAD_POWER_ROOT: sem ela, o perfil do host mudaria.";
    assert_eq!(choose(None, true), BusChoice::Off(container.into()));

    let dir = TempDir::new().unwrap();
    let base = dir.path().canonicalize().unwrap();
    for inside in [false, true] {
        assert_eq!(
            choose(Some(dir.path().as_os_str()), inside),
            BusChoice::Tree(base.clone())
        );
    }

    let invalid = "LUCERNAD_POWER_ROOT inválido: o perfil de energia e o GameMode do modo jogo ficam indisponíveis.";
    for bad in ["rel", "/", "/sys", "/nao/existe"] {
        assert_eq!(
            choose(Some(OsStr::new(bad)), false),
            BusChoice::Off(invalid.into()),
            "{bad}"
        );
    }
}
