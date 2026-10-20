use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lucernad_system::Roots;
use lucernad_system::disks::DiskReader;
use lucernad_system::nvidia::{NoNvidia, Nvidia, NvidiaReading};
use lucernad_system::sampler::{Groups, Sampler};
use lucernad_system::testing::{
    CountingNvidia, FixedStatfs, copy_tree, proc_with_nvidia_user, testdata,
};

fn reader() -> DiskReader {
    DiskReader::new(Arc::new(FixedStatfs::machine()), Duration::from_millis(200))
}

#[test]
fn stats_from_the_machine() {
    let mut s = Sampler::new(
        Roots::at(&testdata("machine")),
        Box::new(NoNvidia),
        reader(),
        Duration::from_secs(2),
    );
    let st = s.stats();
    assert_eq!(
        (st.mem_total, st.mem_used, st.swap_total, st.swap_used),
        (16384000, 10240000, 2048000, 1536000)
    );
    assert_eq!(
        (st.cpu_temp, st.rx_total, st.tx_total, st.uptime),
        (Some(48.0), 1000, 2000, 12345.67)
    );
    assert_eq!(
        (st.cpu_usage, st.rx_history.len()),
        (0.0, 0),
        "primeira leitura: sem delta"
    );
}

#[test]
fn cycle_start_keeps_last_values_without_history() {
    let t = tempfile::tempdir().unwrap();
    copy_tree(&testdata("machine"), t.path()).unwrap();
    let roots = Roots::at(t.path());
    let mut s = Sampler::new(
        roots.clone(),
        Box::new(NoNvidia),
        reader(),
        Duration::from_secs(2),
    );
    s.stats();
    std::fs::copy(testdata("cases/stat.b"), roots.procfs.join("stat")).unwrap();
    let st = s.stats();
    assert!((st.cpu_usage - 0.2).abs() < 1e-9);
    assert_eq!(st.rx_history.len(), 1);
    s.reset(Groups {
        stats: true,
        ..Groups::default()
    });
    let st = s.stats();
    assert!(
        (st.cpu_usage - 0.2).abs() < 1e-9,
        "começo de ciclo: repete o último uso"
    );
    assert_eq!(
        st.rx_history.len(),
        1,
        "começo de ciclo: sem ponto novo no histórico"
    );
}

#[test]
fn interface_coming_and_going_makes_no_spike() {
    let t = tempfile::tempdir().unwrap();
    copy_tree(&testdata("machine"), t.path()).unwrap();
    let roots = Roots::at(t.path());
    let dev = roots.procfs.join("net/dev");
    std::fs::create_dir_all(roots.sysfs.join("class/net/usb0/device")).unwrap();
    let header = "Inter-|   Receive\n face |bytes\n";
    let mut s = Sampler::new(
        roots.clone(),
        Box::new(NoNvidia),
        reader(),
        Duration::from_secs(2),
    );
    s.stats();
    // Uma interface nova com um total enorme: não vira taxa.
    let text = format!(
        "{header}  eth0: 1000 0 0 0 0 0 0 0 2000 0 0 0 0 0 0 0\n  usb0: 900000000 0 0 0 0 0 0 0 900000000 0 0 0 0 0 0 0\n"
    );
    std::fs::write(&dev, text).unwrap();
    let st = s.stats();
    assert_eq!(
        (st.rx_rate, st.tx_rate),
        (0.0, 0.0),
        "a que apareceu não entra"
    );
    assert_eq!(st.rx_total, 900_001_000);
    // Ela some: a taxa da eth0 segue, sem zerar nem ir negativa.
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(
        &dev,
        format!("{header}  eth0: 3000 0 0 0 0 0 0 0 2000 0 0 0 0 0 0 0\n"),
    )
    .unwrap();
    let st = s.stats();
    assert!(st.rx_rate > 0.0, "a eth0 cresceu: {}", st.rx_rate);
    assert_eq!(st.tx_rate, 0.0);
    assert_eq!(st.rx_history.len(), 2);
}

fn nvidia_roots(tmp: &tempfile::TempDir) -> Roots {
    Roots {
        procfs: proc_with_nvidia_user(tmp.path()),
        sysfs: testdata("cases/nvidia-active/sys"),
        ..Roots::at(&testdata("machine"))
    }
}

#[tokio::test]
async fn gpus_read_nvml_only_with_a_user() {
    let reading = NvidiaReading {
        usage: Some(0.35),
        temp: Some(48.0),
        freq: Some(1200),
        max_freq: Some(2100),
    };
    let nv = CountingNvidia {
        calls: Default::default(),
        reading: Some(reading),
    };
    let calls = nv.calls.clone();
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Sampler::new(
        nvidia_roots(&tmp),
        Box::new(nv.clone()),
        reader(),
        Duration::from_secs(2),
    );
    let g = s.gpus().await;
    assert_eq!((g[0].usage, g[0].temp), (Some(0.35), Some(48.0)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let no_user = Roots {
        sysfs: testdata("cases/nvidia-active/sys"),
        ..Roots::at(&testdata("machine"))
    };
    let mut s = Sampler::new(no_user, Box::new(nv), reader(), Duration::from_secs(2));
    let g = s.gpus().await;
    assert_eq!((g[0].state.as_str(), g[0].usage), ("active", None));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "sem usuário, sem NVML");
}

/// Uma NVML que trava 1 s na primeira chamada.
struct StuckNvidia(Arc<AtomicUsize>);

impl Nvidia for StuckNvidia {
    fn read(&mut self, _pci: &str) -> Option<NvidiaReading> {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            std::thread::sleep(Duration::from_secs(1));
        }
        Some(NvidiaReading {
            usage: Some(0.5),
            ..NvidiaReading::default()
        })
    }
}

#[tokio::test]
async fn stuck_nvml_is_skipped_and_not_called_again() {
    let calls = Arc::new(AtomicUsize::new(0));
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Sampler::new(
        nvidia_roots(&tmp),
        Box::new(StuckNvidia(calls.clone())),
        reader(),
        Duration::from_secs(2),
    )
    .with_nvidia_timeout(Duration::from_millis(100));
    let t0 = Instant::now();
    let g = s.gpus().await;
    assert!(t0.elapsed() < Duration::from_millis(600), "o prazo vale");
    assert_eq!(
        (g[0].state.as_str(), g[0].name.as_str(), g[0].usage),
        ("active", "NVIDIA GeForce RTX 3050 Laptop GPU", None),
        "prazo estourado: a leitura sai sem a NVML"
    );
    let g = s.gpus().await;
    assert_eq!(g[0].usage, None);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a leitura presa não é pedida de novo"
    );
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let g = s.gpus().await;
    assert_eq!(g[0].usage, Some(0.5), "solta, a NVML volta a ser lida");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
