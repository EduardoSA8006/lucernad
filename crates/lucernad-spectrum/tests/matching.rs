use lucernad_spectrum::backend::OutputNode;
use lucernad_spectrum::dbus::{Bus, Identity};
use lucernad_spectrum::matching::{
    ProcFs, Processes, is_descendant, match_nodes, parse_ppid, player_names, search_root,
};
use lucernad_spectrum::testing::{FakeBus, testdata};

fn procs() -> ProcFs {
    ProcFs::new(testdata("proc"))
}

fn id(name: &str, pid: Option<u32>) -> Identity {
    Identity {
        name: name.to_string(),
        owner_pid: pid,
        desktop_entry: None,
    }
}

fn node(id: u32, serial: u64, pid: Option<u32>, binary: Option<&str>) -> OutputNode {
    OutputNode {
        id,
        serial,
        client_pid: pid,
        binary: binary.map(str::to_string),
    }
}

fn nodes() -> Vec<OutputNode> {
    vec![
        node(89, 121, Some(153543), Some("spotify")),
        node(79, 178, Some(152224), Some("brave")),
        node(95, 190, Some(152224), Some("brave")),
        node(97, 200, Some(1145), Some("plasmashell")),
        node(99, 210, None, None),
    ]
}

#[test]
fn same_pid() {
    let id = id("org.mpris.MediaPlayer2.spotify", Some(153543));
    assert_eq!(match_nodes(&id, &nodes(), &procs()), vec![121]);
}

#[test]
fn descendant_of_owner() {
    let id = id("org.mpris.MediaPlayer2.brave.instance119549", Some(119549));
    assert_eq!(match_nodes(&id, &nodes(), &procs()), vec![178, 190]);
}

#[test]
fn bridge_starts_from_parent() {
    let id = id(
        "org.mpris.MediaPlayer2.plasma-browser-integration",
        Some(120143),
    );
    assert_eq!(match_nodes(&id, &nodes(), &procs()), vec![178, 190]);
}

#[test]
fn generic_owner_does_not_climb() {
    let id = id("org.mpris.MediaPlayer2.kdeconnect", Some(160000));
    assert_eq!(match_nodes(&id, &nodes(), &procs()), Vec::<u64>::new());
}

#[test]
fn reserve_by_exact_name() {
    let spotify = id("org.mpris.MediaPlayer2.spotify", None);
    assert_eq!(match_nodes(&spotify, &nodes(), &procs()), vec![121]);

    let upper = id("org.mpris.MediaPlayer2.Spotify", None);
    assert_eq!(match_nodes(&upper, &nodes(), &procs()), Vec::<u64>::new());

    let entry = Identity {
        desktop_entry: Some("spotify".to_string()),
        ..upper
    };
    assert_eq!(match_nodes(&entry, &nodes(), &procs()), vec![121]);

    // Fora do /proc, como num Flatpak: o PID não casa e a reserva vale.
    let flatpak = id("org.mpris.MediaPlayer2.spotify", Some(999_999));
    assert_eq!(match_nodes(&flatpak, &nodes(), &procs()), vec![121]);
}

#[test]
fn reserve_needs_one_process() {
    let mut nodes = nodes();
    nodes.push(node(101, 230, Some(152300), Some("brave")));
    let id = id("org.mpris.MediaPlayer2.brave.instance1", None);
    assert_eq!(match_nodes(&id, &nodes, &procs()), Vec::<u64>::new());
}

#[test]
fn node_without_client_info_never_matches() {
    // O mpv é dono do nome e está no /proc; o único nó que poderia ser dele
    // (o 210) ainda não tem o info do client.
    let mpv = id("org.mpris.MediaPlayer2.mpv", Some(180000));
    assert_eq!(match_nodes(&mpv, &nodes(), &procs()), Vec::<u64>::new());
    let mpv = id("org.mpris.MediaPlayer2.mpv", None);
    assert_eq!(match_nodes(&mpv, &nodes(), &procs()), Vec::<u64>::new());
}

#[test]
fn reserve_skips_a_visible_owner() {
    // Dois mpv: o exibido (180000, no /proc) ainda sem nó com client
    // conhecido; o outro (180001) tocando. A reserva casaria o outro.
    let mut nodes = nodes();
    nodes.push(node(103, 240, Some(180001), Some("mpv")));
    let id = id("org.mpris.MediaPlayer2.mpv", Some(180000));
    assert_eq!(match_nodes(&id, &nodes, &procs()), Vec::<u64>::new());
}

#[test]
fn flatpak_proxy_owner_allows_the_reserve() {
    // No Flatpak, o dono do nome é o xdg-dbus-proxy, visível no /proc mas
    // irmão do app: conta como dono não visível e a reserva vale.
    let id = id("org.mpris.MediaPlayer2.spotify", Some(200000));
    assert_eq!(match_nodes(&id, &nodes(), &procs()), vec![121]);
}

#[test]
fn generic_root_never_matches() {
    assert_eq!(search_root(&procs(), 1), None);
    assert_eq!(search_root(&procs(), 1000), None);
    assert_eq!(search_root(&procs(), 190000), None);

    let init = id("org.mpris.MediaPlayer2.systemd", Some(1));
    assert_eq!(match_nodes(&init, &nodes(), &procs()), Vec::<u64>::new());
    // Uma ponte órfã, adotada pelo systemd --user.
    let orphan = id(
        "org.mpris.MediaPlayer2.plasma-browser-integration",
        Some(190000),
    );
    assert_eq!(match_nodes(&orphan, &nodes(), &procs()), Vec::<u64>::new());
}

#[test]
fn player_names_drop_the_instance() {
    assert_eq!(
        player_names("org.mpris.MediaPlayer2.brave.instance119549", None),
        vec!["brave"]
    );
    assert_eq!(
        player_names(
            "org.mpris.MediaPlayer2.firefox.instance_1_23",
            Some("firefox")
        ),
        vec!["firefox"]
    );
    assert_eq!(
        player_names("org.mpris.MediaPlayer2.mpv", None),
        vec!["mpv"]
    );
    assert_eq!(player_names("org.outro.Nome", None), Vec::<String>::new());
}

#[test]
fn proc_reading() {
    assert_eq!(parse_ppid("180000 (mpv (x)) S 1000 1 1"), Some(1000));
    assert_eq!(
        procs().get(120143).unwrap().exe,
        "plasma-browser-integration-host"
    );
    assert_eq!(procs().get(180000).unwrap().exe, "mpv");
    assert_eq!(procs().get(4), None);
}

#[test]
fn search_root_rules() {
    assert_eq!(search_root(&procs(), 120143), Some(119549));
    assert_eq!(search_root(&procs(), 153543), Some(153543));
    assert_eq!(search_root(&procs(), 999_999), Some(999_999));
}

#[test]
fn cycle_is_bounded() {
    assert!(!is_descendant(&procs(), 170000, 1));
    assert!(is_descendant(&procs(), 152224, 152224));
}

#[tokio::test]
async fn fake_bus_identifies() {
    let bus = FakeBus::new().owner("a", 7).desktop_entry("a", "x");
    assert_eq!(
        bus.identify("a").await,
        Identity {
            name: "a".to_string(),
            owner_pid: Some(7),
            desktop_entry: Some("x".to_string()),
        }
    );
    assert_eq!(bus.identify("b").await, id("b", None));
}
