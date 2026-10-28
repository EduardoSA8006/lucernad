//! O `game.configure`: o padrão do Formato, os erros `-32602` e as classes
//! guardadas em minúsculas.

use lucernad_game::config::{DEFAULT_NEVER, GameSettings, parse};
use serde_json::json;

/// O texto do `-32602` do SDK com o detalhe `d`.
fn invalid(d: &str) -> String {
    format!("Parâmetros inválidos: {d}.")
}

#[test]
fn configure_defaults_when_missing() {
    let d = GameSettings::default();
    assert!(d.auto_fullscreen && d.auto_games);
    assert!(d.effects && d.vrr && !d.tearing);
    assert!(d.profile && d.gamemode && d.light);
    assert!(d.always.is_empty());
    // A lista "nunca" de fábrica: os players e navegadores comuns (D24).
    assert_eq!(
        d.never,
        DEFAULT_NEVER
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    );
    for c in ["mpv", "vlc", "firefox", "chromium", "brave-browser", "zen"] {
        assert!(d.never.iter().any(|n| n == c), "falta {c}");
    }
    assert_eq!(parse(&json!({})).unwrap(), d);
    assert_eq!(parse(&json!(null)).unwrap(), d);
    let s = parse(
        &json!({"auto": {"games": false}, "compositor": {"tearing": true},
                          "always": ["Osu!", "steam_app_570"], "never": []}),
    )
    .unwrap();
    assert!(s.auto_fullscreen && !s.auto_games && s.tearing && s.effects);
    assert_eq!(s.always, vec!["osu!", "steam_app_570"]);
    assert!(
        s.never.is_empty(),
        "never: [] é a lista vazia, não a de fábrica"
    );
}

#[test]
fn configure_rejects_bad_lists() {
    const LISTS: &str =
        "always e never precisam ser listas de até 100 classes com até 128 caracteres";
    let many: Vec<String> = (0..101).map(|i| format!("c{i}")).collect();
    let long = "x".repeat(129);
    for bad in [
        json!({"always": many}),
        json!({"never": [long]}),
        json!({"always": 3}),
        json!({"never": ["ok", 1]}),
    ] {
        let e = parse(&bad).unwrap_err();
        assert_eq!(e.code, -32602, "{bad}");
        assert_eq!(e.message, invalid(LISTS), "{bad}");
    }
    // No limite, passa.
    let hundred: Vec<String> = (0..100).map(|i| format!("c{i}")).collect();
    assert!(parse(&json!({"always": hundred, "never": ["y".repeat(128)]})).is_ok());
    let e = parse(&json!({"system": {"light": "sim"}})).unwrap_err();
    assert_eq!(e.code, -32602);
    assert_eq!(e.message, invalid("system.light precisa ser true ou false"));
    let e = parse(&json!({"auto": {"fullscreen": 1}})).unwrap_err();
    assert_eq!(
        e.message,
        invalid("auto.fullscreen precisa ser true ou false")
    );
    let e = parse(&json!({"compositor": true})).unwrap_err();
    assert_eq!(e.message, invalid("compositor precisa ser um objeto"));
}
