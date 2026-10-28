//! A detecção dos jogos sobre um `/proc` falso numa pasta temporária: nada
//! aqui lê o `/proc` de verdade.

use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;

use lucernad_game::detect::{ANCESTORS_MAX, GameCache, is_game};
use lucernad_game::testing::FakeProc;

const ME: u32 = 1000;

#[test]
fn steam_app_id_is_a_game() {
    let p = FakeProc::new(ME);
    p.process(
        500,
        1,
        ME,
        "hl2_linux",
        "/games/hl2_linux",
        &["./hl2_linux"],
        &[("SteamAppId", "220")],
    );
    p.process(
        501,
        1,
        ME,
        "game",
        "/games/game",
        &["game"],
        &[("SteamGameId", "999")],
    );
    assert!(is_game(&p.root(), 500));
    assert!(is_game(&p.root(), 501));
}

#[test]
fn steam_app_id_zero_is_not() {
    let p = FakeProc::new(ME);
    p.process(
        500,
        1,
        ME,
        "x",
        "/x",
        &["x"],
        &[("SteamAppId", "0"), ("SteamGameId", "")],
    );
    assert!(!is_game(&p.root(), 500));
}

#[test]
fn steam_client_without_app_id_is_not() {
    let p = FakeProc::new(ME);
    p.process(
        400,
        1,
        ME,
        "steam",
        "/usr/lib/steam/steam",
        &["steam"],
        &[("HOME", "/home/x")],
    );
    p.process(
        401,
        400,
        ME,
        "steamwebhelper",
        "/s/steamwebhelper",
        &["steamwebhelper"],
        &[],
    );
    assert!(!is_game(&p.root(), 400));
    assert!(!is_game(&p.root(), 401));
}

#[test]
fn proton_child_inherits_steam_app_id() {
    let p = FakeProc::new(ME);
    p.process(400, 1, ME, "steam", "/usr/lib/steam/steam", &["steam"], &[]);
    p.process(
        600,
        400,
        ME,
        "reaper",
        "/s/reaper",
        &["reaper", "SteamLaunch"],
        &[("SteamAppId", "570")],
    );
    // O wine-preloader sem ambiente legível: o ancestral (o reaper) decide.
    p.process(
        601,
        600,
        ME,
        "dota2.exe",
        "/p/wine64-preloader",
        &["Z:\\dota2.exe"],
        &[],
    );
    p.process(602, 601, ME, "x", "/x", &["x"], &[]);
    assert!(is_game(&p.root(), 602));
}

#[test]
fn gamescope_is_a_game() {
    let p = FakeProc::new(ME);
    p.process(
        700,
        1,
        ME,
        "gamescope",
        "/usr/bin/gamescope",
        &["gamescope", "--", "x"],
        &[],
    );
    assert!(is_game(&p.root(), 700));
}

#[test]
fn wine_exe_is_a_game() {
    let p = FakeProc::new(ME);
    p.process(800, 1, ME, "x.exe", "/opt/x", &["C:\\Games\\x.exe"], &[]);
    p.process(801, 1, ME, "wine", "/usr/bin/wine", &["wine", "setup"], &[]);
    p.process(802, 1, ME, "w", "/usr/bin/wine-preloader", &["w"], &[]);
    p.process(803, 1, ME, "notepad", "/usr/bin/notepad", &["notepad"], &[]);
    assert!(is_game(&p.root(), 800));
    assert!(is_game(&p.root(), 801));
    assert!(is_game(&p.root(), 802));
    assert!(!is_game(&p.root(), 803));
}

#[test]
fn lutris_wrapper_ancestor_is_a_game() {
    let p = FakeProc::new(ME);
    p.process(
        900,
        1,
        ME,
        "lutris-wrapper",
        "/usr/bin/python3",
        &["lutris-wrapper: Jogo"],
        &[],
    );
    p.process(901, 900, ME, "sh", "/usr/bin/sh", &["sh", "-c", "run"], &[]);
    p.process(902, 901, ME, "jogo", "/games/jogo", &["./jogo"], &[]);
    assert!(is_game(&p.root(), 902));
}

#[test]
fn other_users_process_is_never_read() {
    let p = FakeProc::new(ME);
    p.process(300, 1, 0, "x", "/x", &["x.exe"], &[("SteamAppId", "570")]);
    let environ = p.path(300).join("environ");
    std::fs::set_permissions(&environ, std::fs::Permissions::from_mode(0o000)).unwrap();
    assert!(!is_game(&p.root(), 300));
    // Filho meu de um processo de outro usuário: a subida para nele.
    p.process(301, 300, ME, "y", "/y", &["y"], &[]);
    assert!(!is_game(&p.root(), 301));
    // O meu, com o environ ilegível, não dá erro.
    p.process(302, 1, ME, "z", "/z", &["z"], &[]);
    std::fs::set_permissions(
        p.path(302).join("environ"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    assert!(!is_game(&p.root(), 302));
}

#[test]
fn ancestor_chain_stops_at_eight() {
    assert_eq!(ANCESTORS_MAX, 8);
    let p = FakeProc::new(ME);
    // 100 é o jogo, a 8 níveis da janela (108); a janela 109 fica a 9.
    p.process(100, 1, ME, "jogo", "/g", &["g"], &[("SteamAppId", "1")]);
    for pid in 101..=109 {
        p.process(pid, pid - 1, ME, "sh", "/bin/sh", &["sh"], &[]);
    }
    assert!(is_game(&p.root(), 108));
    assert!(!is_game(&p.root(), 109));
}

#[test]
fn missing_pid_is_not_a_game() {
    let p = FakeProc::new(ME);
    assert!(!is_game(&p.root(), 4242));
    assert!(!is_game(&p.root(), 0));
    assert!(!is_game(&p.root(), -1));
}

#[test]
fn cache_forgets_dead_pids() {
    let p = FakeProc::new(ME);
    p.process(500, 1, ME, "g", "/g", &["g"], &[("SteamAppId", "1")]);
    let mut cache = GameCache::default();
    assert!(cache.is_game(&p.root(), 500));
    // Guardado: o processo pode sumir do /proc e a resposta fica.
    std::fs::remove_dir_all(p.path(500)).unwrap();
    assert!(cache.is_game(&p.root(), 500));
    cache.retain(&HashSet::new());
    assert!(!cache.is_game(&p.root(), 500));
}
