use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lucernad_system::Roots;
use lucernad_system::disks::{
    BlockTree, Disk, DiskReader, Kind, MAX_PENDING, Mount, Node, RealStatfs, Statfs, Usage,
    aggregate, is_candidate, parse_mountinfo, read_disks,
};
use lucernad_system::testing::{FixedStatfs, testdata};

fn machine() -> Roots {
    Roots::at(&testdata("machine"))
}

fn mounts(rel: &str) -> Vec<Mount> {
    parse_mountinfo(&std::fs::read_to_string(testdata(rel)).unwrap())
}

#[test]
fn mountinfo_fields_and_escapes() {
    let m = mounts("machine/proc/self/mountinfo");
    assert_eq!(m.len(), 10);
    assert_eq!(
        m[3],
        Mount {
            dev: "8:2".into(),
            source: "/dev/sda2".into(),
            target: "/mnt/dados antigos".into(),
            fstype: "ext4".into()
        }
    );
    assert_eq!(m[6].source, "nas:/export");
}

#[test]
fn candidates_skip_virtual_remote_loop() {
    let c: Vec<_> = mounts("machine/proc/self/mountinfo")
        .into_iter()
        .filter(is_candidate)
        .map(|m| m.target)
        .collect();
    assert_eq!(
        c,
        [
            "/",
            "/home",
            "/boot",
            "/mnt/dados antigos",
            "/srv/proj",
            "/var"
        ]
    );
    let zram = Mount {
        dev: "253:0".into(),
        source: "/dev/zram0".into(),
        target: "/z".into(),
        fstype: "ext4".into(),
    };
    let nfs_dev = Mount {
        dev: "0:50".into(),
        source: "/dev/x".into(),
        target: "/n".into(),
        fstype: "nfs".into(),
    };
    assert!(!is_candidate(&zram) && !is_candidate(&nfs_dev));
}

#[test]
fn physical_disk_of_each_source() {
    let t = BlockTree::read(&machine());
    assert_eq!(
        t.disk_of("/dev/nvme0n1p1").as_deref(),
        Some("nvme0n1"),
        "disco com slaves/ vazia (só o .gitkeep) é Disk"
    );
    assert_eq!(t.disk_of("/dev/sda2").as_deref(), Some("sda"), "partição");
    assert_eq!(
        t.disk_of("/dev/sda").as_deref(),
        Some("sda"),
        "o disco inteiro"
    );
    assert_eq!(
        t.disk_of("/dev/mapper/vg-home").as_deref(),
        Some("nvme0n1"),
        "LVM sobre LUKS"
    );
    assert_eq!(
        t.disk_of("/dev/dm-0").as_deref(),
        Some("nvme0n1"),
        "origem dm-N"
    );
    assert_eq!(
        t.disk_of("/dev/dm-7").as_deref(),
        Some("dm-7"),
        "fora da árvore: o próprio nome"
    );
    assert_eq!(t.disk_of("/dev/root").as_deref(), Some("root"));
    assert_eq!(t.disk_of("/dev/loop0"), None, "loop é Other, sem pai");
    let empty = BlockTree::from_nodes([], []);
    assert_eq!(
        empty.disk_of("/dev/nvme0n1p2").as_deref(),
        Some("nvme0n1p2"),
        "sem árvore: a própria partição"
    );
}

#[test]
fn raid_takes_the_last_parent() {
    let t = BlockTree::read(&Roots::at(&testdata("cases/block-raid")));
    assert_eq!(t.disk_of("/dev/md0").as_deref(), Some("sdc"));
}

#[test]
fn cycle_does_not_hang() {
    let part = |p: &str| Node {
        kind: Kind::Part,
        parent: Some(p.into()),
    };
    let t = BlockTree::from_nodes([("a".into(), part("b")), ("b".into(), part("a"))], []);
    assert_eq!(t.disk_of("/dev/a"), None);
}

fn machine_usages() -> HashMap<String, Usage> {
    [
        ("/dev/mapper/root", 2000, 500),
        ("/dev/nvme0n1p1", 100, 10),
        ("/dev/sda2", 1000, 400),
        ("/dev/mapper/vg-home", 300, 30),
    ]
    .into_iter()
    .map(|(s, size, used)| (s.to_string(), Usage { size, used }))
    .collect()
}

#[test]
fn summed_per_disk_root_first() {
    let d = aggregate(
        &mounts("machine/proc/self/mountinfo"),
        &BlockTree::read(&machine()),
        &machine_usages(),
    );
    assert_eq!(
        d,
        vec![
            Disk {
                name: "nvme0n1".into(),
                size: 2400,
                used: 540,
                root: true
            },
            Disk {
                name: "sda".into(),
                size: 1000,
                used: 400,
                root: false
            },
        ]
    );
}

#[test]
fn container_root_keeps_mount_order() {
    let u: HashMap<String, Usage> = [
        (
            "/dev/nvme0n1p2".to_string(),
            Usage {
                size: 2000,
                used: 500,
            },
        ),
        (
            "/dev/sda2".to_string(),
            Usage {
                size: 1000,
                used: 400,
            },
        ),
    ]
    .into();
    let d = aggregate(
        &mounts("cases/mountinfo-container"),
        &BlockTree::read(&machine()),
        &u,
    );
    assert_eq!(
        d.iter()
            .map(|d| (d.name.as_str(), d.root))
            .collect::<Vec<_>>(),
        [("nvme0n1", false), ("sda", false)]
    );
    assert!(aggregate(&[], &BlockTree::read(&machine()), &u).is_empty());
}

#[tokio::test]
async fn one_statvfs_per_source_only_for_candidates() {
    let fs = Arc::new(FixedStatfs::machine());
    let r = DiskReader::new(fs.clone(), Duration::from_millis(200));
    let disks = read_disks(&machine(), &r).await;
    assert_eq!(
        disks[0],
        Disk {
            name: "nvme0n1".into(),
            size: 2400,
            used: 540,
            root: true
        }
    );
    let mut calls = fs.calls();
    calls.sort();
    assert_eq!(
        calls,
        ["/", "/boot", "/mnt/dados antigos", "/var"].map(PathBuf::from)
    );
}

#[tokio::test]
async fn slow_statvfs_does_not_block() {
    let fs = Arc::new(FixedStatfs::machine().slow("/var"));
    let r = DiskReader::new(fs.clone(), Duration::from_millis(200));
    let t0 = std::time::Instant::now();
    let disks = read_disks(&machine(), &r).await;
    assert!(
        t0.elapsed() < Duration::from_millis(800),
        "não esperou a montagem travada"
    );
    assert_eq!(
        (disks[0].size, disks[0].used),
        (2100, 510),
        "/var fica fora da soma"
    );
    read_disks(&machine(), &r).await;
    assert_eq!(
        fs.calls()
            .iter()
            .filter(|p| p.as_path() == Path::new("/var"))
            .count(),
        1,
        "não pede de novo enquanto a primeira não voltou"
    );
}

#[tokio::test]
async fn failed_statvfs_skips_that_source() {
    let fs = Arc::new(FixedStatfs::machine().failing("/mnt/dados antigos"));
    let disks = read_disks(&machine(), &DiskReader::new(fs, Duration::from_millis(200))).await;
    assert_eq!(
        disks.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        ["nvme0n1"]
    );
}

#[test]
fn real_statvfs_of_root() {
    let u = RealStatfs.usage(Path::new("/")).unwrap();
    assert!(u.size > 0 && u.used <= u.size);
}

/// Uma árvore só de discos com estes nomes de kernel.
fn tree_of(names: impl IntoIterator<Item = String>) -> BlockTree {
    let disk = Node {
        kind: Kind::Disk,
        parent: None,
    };
    BlockTree::from_nodes(names.into_iter().map(|n| (n, disk.clone())), [])
}

fn ext4(source: &str, target: &str) -> Mount {
    Mount {
        dev: "0:0".into(),
        source: source.into(),
        target: target.into(),
        fstype: "ext4".into(),
    }
}

#[tokio::test]
async fn pending_is_per_source_and_target() {
    let fs = Arc::new(FixedStatfs::machine().slow("/var"));
    let r = DiskReader::new(fs.clone(), Duration::from_millis(100));
    let t = tree_of(["sdb1".into(), "sdc1".into()]);
    assert!(r.usages(&[ext4("/dev/sdb1", "/var")], &t).await.is_empty());
    r.usages(&[ext4("/dev/sdb1", "/var")], &t).await;
    r.usages(&[ext4("/dev/sdc1", "/var")], &t).await;
    assert_eq!(
        fs.calls()
            .iter()
            .filter(|p| p.as_path() == Path::new("/var"))
            .count(),
        2,
        "a mesma origem no mesmo alvo não é pedida de novo; outra origem é"
    );
}

#[tokio::test]
async fn pending_has_a_global_cap() {
    let stuck: Vec<String> = (0..=MAX_PENDING).map(|i| format!("/s{i}")).collect();
    let fs = Arc::new(
        stuck
            .iter()
            .fold(FixedStatfs::machine(), |fs, p| fs.slow(p)),
    );
    let r = DiskReader::new(fs.clone(), Duration::from_millis(100));
    let mounts: Vec<Mount> = stuck
        .iter()
        .enumerate()
        .map(|(i, p)| ext4(&format!("/dev/sd{i}"), p))
        .collect();
    let t = tree_of((0..=MAX_PENDING).map(|i| format!("sd{i}")));
    assert_eq!(MAX_PENDING, 16);
    assert!(r.usages(&mounts[..MAX_PENDING], &t).await.is_empty());
    let t0 = std::time::Instant::now();
    assert!(r.usages(&mounts[MAX_PENDING..], &t).await.is_empty());
    assert!(
        t0.elapsed() < Duration::from_millis(50),
        "acima do teto conta como prazo estourado, sem esperar"
    );
    assert_eq!(
        fs.calls().len(),
        MAX_PENDING,
        "acima do teto nenhuma thread nova"
    );
}

#[tokio::test]
async fn fuse_and_unknown_sources_are_not_called() {
    let fs = Arc::new(FixedStatfs::machine());
    let r = DiskReader::new(fs.clone(), Duration::from_millis(200));
    let t = BlockTree::read(&machine());
    let with = |source: &str, target: &str, fstype: &str| Mount {
        dev: "0:0".into(),
        source: source.into(),
        target: target.into(),
        fstype: fstype.into(),
    };
    let mounts = [
        with("/dev/x", "/var", "fuse.evil"),
        with("/dev/sda2", "/var", "fuse"),
        with("/dev/nvme0n1p1", "/var", "fuse.sshfs2"),
        with("/dev/x", "/boot", "ext4"),
        with("/dev/mapper/nada", "/boot", "ext4"),
    ];
    assert!(r.usages(&mounts, &t).await.is_empty());
    assert!(
        fs.calls().is_empty(),
        "nem FUSE nem origem fora do /sys/block"
    );
    assert!(!is_candidate(&mounts[0]) && !is_candidate(&mounts[1]));

    let fuseblk = with("/dev/sda2", "/mnt/dados antigos", "fuseblk");
    assert!(is_candidate(&fuseblk));
    let u = r.usages(&[fuseblk], &t).await;
    assert_eq!(u.len(), 1, "fuseblk de um bloco real (ntfs-3g) conta");
    let outside = with("/dev/x", "/mnt/dados antigos", "fuseblk");
    assert!(r.usages(&[outside], &t).await.is_empty());
    assert_eq!(fs.calls().len(), 1, "fuseblk fora da árvore não é pedido");
}

#[tokio::test]
async fn dev_root_resolves_by_major_minor() {
    let roots = Roots::at(&testdata("cases/dev-root"));
    let fs = Arc::new(FixedStatfs::machine());
    let r = DiskReader::new(fs.clone(), Duration::from_millis(200));
    let disks = read_disks(&roots, &r).await;
    assert_eq!(
        disks,
        [Disk {
            name: "sda".into(),
            size: 2000,
            used: 500,
            root: true
        }],
        "/dev/root com o maj:min da sda2 soma na sda"
    );
    assert_eq!(
        fs.calls(),
        [PathBuf::from("/")],
        "maj:min que não resolve para um nó da árvore (link para fora, sem link, anônimo) não é pedido"
    );
}
