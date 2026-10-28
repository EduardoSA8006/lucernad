//! As janelas e os monitores, com o JSON da sonda (Hyprland 0.56.2, um
//! `kitty` em tela cheia no container `lucerna-polish`).

use lucernad_hyprland::clients::{Window, parse_active_workspaces, parse_clients};

#[test]
fn parses_clients_from_probe() {
    let json = include_str!("data/clients.json");
    let windows = parse_clients(json).unwrap();
    assert_eq!(
        windows,
        vec![Window {
            address: "0x55c473515b60".into(),
            pid: 187283,
            class: "kitty".into(),
            title: "~".into(),
            fullscreen: 2,
            workspace: 1,
            mapped: true,
        }]
    );
    assert_eq!(parse_clients("[]").unwrap(), vec![]);
    assert!(parse_clients("unknown request").is_err());
}

#[test]
fn parses_active_workspaces_from_probe() {
    let json = include_str!("data/monitors.json");
    assert_eq!(parse_active_workspaces(json).unwrap(), vec![1]);
    // Um workspace especial à mostra também conta.
    let two = r#"[{"activeWorkspace": {"id": 3}, "specialWorkspace": {"id": -98}},
                  {"activeWorkspace": {"id": 5}, "specialWorkspace": {"id": 0}}]"#;
    assert_eq!(parse_active_workspaces(two).unwrap(), vec![3, -98, 5]);
}
