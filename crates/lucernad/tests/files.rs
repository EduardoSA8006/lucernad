//! Os arquivos que acompanham o binário: manifesto e unidades do systemd.

use lucerna_manifest::Manifest;

#[test]
fn manifest_is_valid_and_has_no_panel() {
    let m = Manifest::from_json(include_str!("../data/lucernad.json")).unwrap();
    m.validate().unwrap();
    assert_eq!(
        (m.app.id.as_str(), m.app.socket.as_str()),
        ("lucernad", "lucernad.sock")
    );
    assert!(m.panel.is_none() && m.card.is_none() && m.app.tui.is_none());
}

#[test]
fn units_match_the_paths_rules() {
    let s = include_str!("../systemd/lucerna-lucernad.socket");
    assert!(
        s.contains("ListenStream=%t/lucerna/lucernad.sock\n")
            && s.contains("DirectoryMode=0700\n")
            && s.contains("SocketMode=0600\n")
    );
    let svc = include_str!("../systemd/lucerna-lucernad.service");
    assert!(
        svc.contains("ExecStart=/usr/bin/lucernad serve\n")
            && svc.contains("Requires=lucerna-lucernad.socket\n")
            && svc.contains("After=lucerna-lucernad.socket\n")
    );
    // O modo jogo desfeito pelo diário mesmo com o daemon morto.
    assert!(svc.contains("ExecStopPost=/usr/bin/lucernad restaurar-jogo\n"));
}
