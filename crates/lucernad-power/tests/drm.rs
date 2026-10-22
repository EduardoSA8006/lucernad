use std::path::PathBuf;

use lucernad_power::drm::{
    BusMatch, candidate_buses, connected, edid_name, i2c_dev_loaded, is_gpu_bus, match_edid,
};
use lucernad_power::testing::{Tree, edid};
use tempfile::TempDir;

const NVIDIA: &str = "0000:01:00.0";
const INTEL: &str = "0000:00:02.0";

/// O EDID do LG da sonda (c): 256 bytes (o bloco base e uma extensão).
fn lg_edid() -> Vec<u8> {
    let mut edid = edid("LG ULTRAWIDE", 1);
    edid.extend([0u8; 128]);
    edid
}

/// A máquina da sonda (c), com o HDMI da NVIDIA conectado.
fn machine() -> (TempDir, Tree) {
    let dir = TempDir::new().unwrap();
    let tree = Tree::new(dir.path());
    tree.gpu(NVIDIA, "0x030000", "card1");
    tree.bus(0, NVIDIA);
    tree.bus(1, NVIDIA);
    tree.connector("card1", "HDMI-A-1", "connected", &lg_edid(), None);

    tree.gpu(INTEL, "0x030000", "card2");
    tree.connector("card2", "eDP-1", "connected", &edid("Painel", 9), Some(13));
    tree.connector("card2", "DP-1", "disconnected", &[], Some(14));
    tree.bus(4, INTEL);

    tree.device("0000:00:1f.4", Some("0x0c0500"));
    tree.bus(16, "0000:00:1f.4");
    tree.device("0000:00:15.0", Some("0x0c8000"));
    tree.device("0000:00:15.0/i2c_designware.0", None);
    tree.bus(2, "0000:00:15.0/i2c_designware.0");
    (dir, tree)
}

fn pci(tree: &Tree, pci: &str) -> PathBuf {
    tree.roots()
        .sysfs
        .join("devices")
        .join(pci)
        .canonicalize()
        .unwrap()
}

#[test]
fn connected_skips_internal_and_disconnected() {
    let (_dir, tree) = machine();
    let list = connected(&tree.roots());
    let names: Vec<&str> = list.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["HDMI-A-1"]);
    let hdmi = &list[0];
    assert_eq!(hdmi.card, "card1");
    assert_eq!(hdmi.pci, pci(&tree, NVIDIA));
    assert_eq!(hdmi.ddc, None);
    assert_eq!(hdmi.edid.len(), 256);
    assert_eq!(hdmi.edid, lg_edid());
}

#[test]
fn ddc_link_is_used() {
    let (_dir, tree) = machine();
    tree.connector("card2", "DP-1", "connected", &edid("Dell", 2), Some(14));
    let list = connected(&tree.roots());
    let names: Vec<&str> = list.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["DP-1", "HDMI-A-1"], "ordenados por nome");
    assert_eq!(list[0].ddc, Some(14));
    assert_eq!(list[0].card, "card2");
    assert_eq!(list[0].pci, pci(&tree, INTEL));
}

#[test]
fn only_gpu_buses_are_candidates() {
    let (_dir, tree) = machine();
    let roots = tree.roots();
    assert!(i2c_dev_loaded(&roots));
    assert_eq!(candidate_buses(&roots, &pci(&tree, NVIDIA), &[]), [0, 1]);
    assert_eq!(
        candidate_buses(&roots, &pci(&tree, NVIDIA), &[1]),
        [0],
        "o LG já casado"
    );
    assert_eq!(candidate_buses(&roots, &pci(&tree, INTEL), &[13, 14]), [4]);
    assert_eq!(
        candidate_buses(&roots, &pci(&tree, INTEL), &[]),
        [4, 13, 14],
        "o AUX sob o conector é da placa"
    );
    assert!(!is_gpu_bus(&roots, 16), "o SMBus do i801");
    assert!(!is_gpu_bus(&roots, 2), "o DesignWare do touchpad");
    assert!(is_gpu_bus(&roots, 13), "o AUX sob o conector");
    assert!(is_gpu_bus(&roots, 0));
    assert!(!is_gpu_bus(&roots, 99), "barramento que não existe");

    let dir = TempDir::new().unwrap();
    let empty = Tree::new(dir.path());
    assert!(!i2c_dev_loaded(&empty.roots()), "sem o i2c-dev");
    assert_eq!(
        candidate_buses(&empty.roots(), &pci(&tree, NVIDIA), &[]),
        Vec::<u32>::new()
    );
}

#[test]
fn edid_name_is_clean() {
    assert_eq!(
        edid_name(&edid("LG ULTRAWIDE", 1)).as_deref(),
        Some("LG ULTRAWIDE")
    );
    assert_eq!(
        edid_name(&edid("\x1b[2JLG\x07", 1)).as_deref(),
        Some("[2JLG"),
        "só os imprimíveis"
    );
    assert_eq!(edid_name(&edid("  Dell  ", 1)).as_deref(), Some("Dell"));
    assert_eq!(edid_name(&edid("", 1)), None, "nome vazio");

    let mut nameless = edid("LG ULTRAWIDE", 1);
    for offset in [54, 72, 90, 108] {
        if nameless[offset..offset + 3] == [0, 0, 0] && nameless[offset + 3] == 0xFC {
            nameless[offset + 3] = 0x10;
        }
    }
    assert_eq!(edid_name(&nameless), None, "sem descritor 0xFC");
    assert_eq!(edid_name(&[]), None, "EDID vazio");
}

#[test]
fn edid_matching() {
    let a = lg_edid();
    let first: [u8; 128] = a[..128].try_into().unwrap();
    assert_eq!(
        match_edid(&a, &[(0, None), (1, Some(first))]),
        BusMatch::Found(1)
    );
    let other: [u8; 128] = edid("LG ULTRAWIDE", 2)[..].try_into().unwrap();
    assert_eq!(
        match_edid(&a, &[(0, None), (1, Some(other))]),
        BusMatch::NotFound
    );
    assert_eq!(match_edid(&a, &[]), BusMatch::NotFound);
    let zeros = [0u8; 128];
    assert_eq!(
        match_edid(&zeros, &[(0, Some(zeros))]),
        BusMatch::NotFound,
        "128 zeros não são um EDID"
    );
    assert_eq!(
        match_edid(&[], &[(1, Some(first))]),
        BusMatch::NotFound,
        "conector sem EDID"
    );
}

#[test]
fn identical_edids_are_not_guessed() {
    let a = lg_edid();
    let first: [u8; 128] = a[..128].try_into().unwrap();
    assert_eq!(
        match_edid(&a, &[(0, Some(first)), (1, Some(first))]),
        BusMatch::Ambiguous
    );
}
