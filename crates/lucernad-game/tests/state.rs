//! A decisão de entrar e sair, com instantes sintéticos (sem relógio).

use std::time::{Duration, Instant};

use lucernad_game::config::GameSettings;
use lucernad_game::state::{AUTO_EXIT, FULLSCREEN_ENTER, Machine, Reason, Trigger, triggers};
use lucernad_hyprland::clients::Window;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn win(address: &str, pid: i32, class: &str, fullscreen: u8, workspace: i64) -> Window {
    Window {
        address: address.into(),
        pid,
        class: class.into(),
        title: format!("{class} título"),
        fullscreen,
        workspace,
        mapped: true,
    }
}

/// Os gatilhos com `games` como os pids que são jogo.
fn trig(windows: &[Window], active: &[i64], games: &[i32], s: &GameSettings) -> Vec<Trigger> {
    triggers(windows, active, |pid| games.contains(&pid), s)
}

fn reasons(t: &[Trigger]) -> Vec<Reason> {
    t.iter().map(|t| t.reason).collect()
}

fn settings() -> GameSettings {
    GameSettings {
        never: vec!["mpv".into()],
        always: vec!["osu!".into()],
        ..GameSettings::default()
    }
}

#[test]
fn never_beats_fullscreen_and_game() {
    let s = settings();
    let w = [win("0x1", 10, "MPV", 2, 1)];
    assert!(trig(&w, &[1], &[10], &s).is_empty());
}

#[test]
fn always_triggers_without_fullscreen() {
    let s = settings();
    // Num workspace escondido e sem tela cheia: a lista "sempre" vale.
    let t = trig(&[win("0x1", 10, "osu!", 0, 7)], &[1], &[], &s);
    assert_eq!(reasons(&t), vec![Reason::Always]);
    // Sem nenhuma automática ligada, também.
    let off = GameSettings {
        auto_fullscreen: false,
        auto_games: false,
        ..s
    };
    assert_eq!(
        trig(&[win("0x1", 10, "OSU!", 0, 7)], &[1], &[], &off).len(),
        1
    );
}

#[test]
fn maximized_is_not_fullscreen() {
    let s = settings();
    assert!(trig(&[win("0x1", 10, "kitty", 1, 1)], &[1], &[], &s).is_empty());
    let t = trig(&[win("0x1", 10, "kitty", 3, 1)], &[1], &[], &s);
    assert_eq!(reasons(&t), vec![Reason::Fullscreen]);
}

#[test]
fn fullscreen_on_hidden_workspace_does_not_count() {
    let s = settings();
    assert!(trig(&[win("0x1", 10, "kitty", 2, 2)], &[1], &[], &s).is_empty());
    // Já um jogo num workspace escondido conta (D23).
    let t = trig(&[win("0x1", 10, "dota2", 0, 2)], &[1], &[10], &s);
    assert_eq!(reasons(&t), vec![Reason::Game]);
}

#[test]
fn fullscreen_needs_one_second() {
    assert_eq!(FULLSCREEN_ENTER, Duration::from_secs(1));
    let s = settings();
    let t0 = Instant::now();
    let mut m = Machine::new();
    m.update(trig(&[win("0x1", 10, "kitty", 2, 1)], &[1], &[], &s), t0);
    assert!(!m.view().active);
    assert_eq!(m.next_deadline(), Some(t0 + FULLSCREEN_ENTER));
    // O mesmo gatilho a cada recálculo não reinicia a espera.
    m.update(
        trig(&[win("0x1", 10, "kitty", 2, 1)], &[1], &[], &s),
        t0 + ms(500),
    );
    m.tick(t0 + ms(999));
    assert!(!m.view().active);
    m.tick(t0 + ms(1000));
    let v = m.view();
    assert!(v.active && v.since.is_some());
    assert_eq!(v.reasons, vec!["fullscreen"]);
    // Sair da tela cheia antes de 1 s não liga nada.
    let mut m = Machine::new();
    m.update(trig(&[win("0x1", 10, "kitty", 2, 1)], &[1], &[], &s), t0);
    m.update(vec![], t0 + ms(600));
    m.tick(t0 + ms(1600));
    assert!(!m.view().active);
}

#[test]
fn game_enters_immediately() {
    let s = settings();
    let t0 = Instant::now();
    let mut m = Machine::new();
    m.update(trig(&[win("0x1", 10, "dota2", 0, 1)], &[1], &[10], &s), t0);
    let v = m.view();
    assert!(v.active);
    assert_eq!(v.reasons, vec!["game"]);
    assert_eq!(v.windows.len(), 1);
    assert_eq!(v.manual, None);
}

#[test]
fn window_recreated_within_grace_keeps_active() {
    let s = settings();
    let t0 = Instant::now();
    let mut m = Machine::new();
    m.update(trig(&[win("0x1", 10, "dota2", 0, 1)], &[1], &[10], &s), t0);
    let since = m.view().since;
    m.update(vec![], t0 + ms(100));
    for step in (200..=2100).step_by(100) {
        m.tick(t0 + ms(step));
        assert!(m.view().active, "desligou em {step} ms");
    }
    m.update(
        trig(&[win("0x2", 11, "dota2", 0, 1)], &[1], &[11], &s),
        t0 + ms(2100),
    );
    m.tick(t0 + ms(10_000));
    let v = m.view();
    assert!(v.active);
    assert_eq!(v.since, since, "o since não recomeça");
}

#[test]
fn auto_exits_after_three_seconds() {
    assert_eq!(AUTO_EXIT, Duration::from_secs(3));
    let s = settings();
    let t0 = Instant::now();
    let mut m = Machine::new();
    m.update(trig(&[win("0x1", 10, "dota2", 0, 1)], &[1], &[10], &s), t0);
    m.update(vec![], t0 + ms(500));
    assert_eq!(m.next_deadline(), Some(t0 + ms(500) + AUTO_EXIT));
    m.tick(t0 + ms(3499));
    let v = m.view();
    assert!(v.active);
    assert_eq!(v.reasons, vec!["game"], "na espera, o motivo fica");
    assert!(v.windows.is_empty());
    m.tick(t0 + ms(3500));
    let v = m.view();
    assert!(!v.active && v.since.is_none() && v.reasons.is_empty());
    assert_eq!(m.next_deadline(), None);
}

#[test]
fn manual_on_ignores_auto_exit() {
    let s = settings();
    let t0 = Instant::now();
    let mut m = Machine::new();
    m.update(trig(&[win("0x1", 10, "dota2", 0, 1)], &[1], &[10], &s), t0);
    m.set_manual(true, t0 + ms(100));
    m.update(vec![], t0 + ms(200));
    m.tick(t0 + ms(60_000));
    let v = m.view();
    assert!(v.active);
    assert_eq!(v.manual, Some(true));
    assert_eq!(v.reasons, vec!["manual"]);
    m.set_manual(false, t0 + ms(60_001));
    let v = m.view();
    assert!(!v.active);
    assert_eq!(v.manual, None, "sem gatilho, não prende desligado");
}

#[test]
fn manual_off_latches_until_triggers_go() {
    let s = settings();
    let t0 = Instant::now();
    let game = || trig(&[win("0x1", 10, "dota2", 0, 1)], &[1], &[10], &s);
    let mut m = Machine::new();
    m.update(game(), t0);
    m.set_manual(false, t0 + ms(100));
    assert!(!m.view().active);
    assert_eq!(m.view().manual, Some(false));
    // O jogo continua aberto: continua desligado.
    m.update(game(), t0 + ms(5_000));
    m.tick(t0 + ms(20_000));
    assert!(!m.view().active);
    // O jogo fecha; passados 3 s, a trava cai.
    m.update(vec![], t0 + ms(20_000));
    m.tick(t0 + ms(22_000));
    assert_eq!(m.view().manual, Some(false));
    m.tick(t0 + ms(23_000));
    assert_eq!(m.view().manual, None);
    assert!(!m.view().active);
    // Um jogo novo liga de novo.
    m.update(
        trig(&[win("0x9", 99, "outro", 0, 1)], &[1], &[99], &s),
        t0 + ms(24_000),
    );
    assert!(m.view().active);
}

#[test]
fn manual_on_without_triggers() {
    let t0 = Instant::now();
    let mut m = Machine::new();
    m.set_manual(true, t0);
    let v = m.view();
    assert!(v.active && v.windows.is_empty());
    assert_eq!(v.reasons, vec!["manual"]);
    assert_eq!(m.next_deadline(), None);
}

#[test]
fn auto_disabled_ignores_triggers() {
    let s = GameSettings {
        auto_fullscreen: false,
        auto_games: false,
        ..settings()
    };
    let w = [win("0x1", 10, "dota2", 2, 1), win("0x2", 11, "kitty", 2, 1)];
    assert!(trig(&w, &[1], &[10], &s).is_empty());
    let only_fs = GameSettings {
        auto_games: false,
        ..settings()
    };
    // O jogo em tela cheia ainda entra pela tela cheia.
    assert_eq!(
        reasons(&trig(&w[..1], &[1], &[10], &only_fs)),
        vec![Reason::Fullscreen]
    );
}

#[test]
fn reasons_are_ordered() {
    let s = settings();
    let t0 = Instant::now();
    let mut m = Machine::new();
    let w = [
        win("0x1", 10, "osu!", 0, 1),
        win("0x2", 11, "kitty", 2, 1),
        win("0x3", 12, "dota2", 0, 1),
    ];
    m.update(trig(&w, &[1], &[12], &s), t0);
    m.set_manual(true, t0);
    m.tick(t0 + ms(1000));
    assert_eq!(
        m.view().reasons,
        vec!["manual", "fullscreen", "game", "always"]
    );
}

#[test]
fn unmapped_windows_do_not_count() {
    let s = settings();
    let mut w = win("0x1", 10, "dota2", 2, 1);
    w.mapped = false;
    assert!(trig(&[w], &[1], &[10], &s).is_empty());
}
