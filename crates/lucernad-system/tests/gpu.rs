use std::collections::HashMap;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::sync::atomic::Ordering;

use lucernad_system::Roots;
use lucernad_system::gpu::{Vendor, list_cards, nvidia_model, read_gpus};
use lucernad_system::nvidia::{NoNvidia, NvidiaReading};
use lucernad_system::testing::{CountingNvidia, PanicNvidia, testdata};
use serde_json::json;

fn machine() -> Roots {
    Roots::at(&testdata("machine"))
}

#[test]
fn cards_skip_connectors_and_unknown_vendors() {
    let c = list_cards(&machine());
    assert_eq!(
        c.iter()
            .map(|c| (c.card.as_str(), c.vendor, c.pci.as_str(), c.suspended))
            .collect::<Vec<_>>(),
        [
            ("card0", Vendor::Intel, "0000:00:02.0", false),
            ("card1", Vendor::Nvidia, "0000:01:00.0", true),
            ("card2", Vendor::Amd, "0000:03:00.0", false),
            ("card3", Vendor::Intel, "0000:04:00.0", false)
        ]
    );
}

#[test]
fn vendors_and_model() {
    assert_eq!(Vendor::from_id("0x10de"), Some(Vendor::Nvidia));
    assert_eq!(Vendor::from_id("0x1002"), Some(Vendor::Amd));
    assert_eq!(Vendor::from_id("0x8086"), Some(Vendor::Intel));
    assert_eq!(Vendor::from_id("0x1af4"), None);
    assert_eq!(
        [Vendor::Nvidia, Vendor::Amd, Vendor::Intel].map(|v| (v.as_str(), v.generic_name())),
        [
            ("nvidia", "NVIDIA"),
            ("amd", "AMD Radeon"),
            ("intel", "Intel (integrada)")
        ]
    );
    assert_eq!(
        nvidia_model(&machine(), "0000:01:00.0").as_deref(),
        Some("NVIDIA GeForce RTX 3050 Laptop GPU")
    );
    assert_eq!(nvidia_model(&machine(), "0000:09:00.0"), None);
}

#[test]
fn the_same_values_as_gpu_status() {
    let r = machine();
    let g = read_gpus(
        &r,
        &list_cards(&r),
        &mut NoNvidia,
        false,
        &HashMap::from([("0000:00:02.0".to_string(), 0.4)]),
    );
    assert_eq!(
        serde_json::to_value(&g).unwrap(),
        json!([
            {"card":"card0","vendor":"intel","pci":"0000:00:02.0","state":"active","name":"Intel (integrada)","usage":0.4,"temp":null,"freq":1450,"maxFreq":1500},
            {"card":"card1","vendor":"nvidia","pci":"0000:01:00.0","state":"suspended","name":"NVIDIA GeForce RTX 3050 Laptop GPU","usage":null,"temp":null,"freq":null,"maxFreq":null},
            {"card":"card2","vendor":"amd","pci":"0000:03:00.0","state":"active","name":"AMD Radeon","usage":0.37,"temp":61.0,"freq":null,"maxFreq":null},
            {"card":"card3","vendor":"intel","pci":"0000:04:00.0","state":"active","name":"Intel (integrada)","usage":null,"temp":null,"freq":900,"maxFreq":2050}
        ])
    );
}

#[test]
fn suspended_cards_are_not_read() {
    let r = machine();
    let mut nv = CountingNvidia {
        calls: Default::default(),
        reading: Some(NvidiaReading::default()),
    };
    read_gpus(&r, &list_cards(&r), &mut nv, true, &HashMap::new());
    assert_eq!(
        nv.calls.load(Ordering::SeqCst),
        0,
        "NVIDIA suspensa: a NVML nem é tocada, mesmo com a placa aberta por alguém"
    );
    // O PanicNvidia confirma por outro lado: com a placa suspensa, ele nunca é chamado.
    read_gpus(&r, &list_cards(&r), &mut PanicNvidia, true, &HashMap::new());
    let amd = Roots {
        sysfs: testdata("cases/amd-suspended/sys"),
        ..machine()
    };
    let g = read_gpus(
        &amd,
        &list_cards(&amd),
        &mut NoNvidia,
        false,
        &HashMap::new(),
    );
    assert_eq!(
        (g[0].state.as_str(), g[0].usage, g[0].temp),
        ("suspended", None, None),
        "AMD suspensa: o gpu_busy_percent fica sem ler"
    );
}

#[test]
fn active_nvidia_comes_from_nvml() {
    let r = Roots {
        sysfs: testdata("cases/nvidia-active/sys"),
        ..machine()
    };
    let reading = NvidiaReading {
        usage: Some(0.35),
        temp: Some(48.0),
        freq: Some(1200),
        max_freq: Some(2100),
    };
    let mut nv = CountingNvidia {
        calls: Default::default(),
        reading: Some(reading),
    };
    let g = read_gpus(&r, &list_cards(&r), &mut nv, false, &HashMap::new());
    assert_eq!(
        (g[0].state.as_str(), g[0].usage, g[0].temp),
        ("active", None, None),
        "ativa sem ninguém usando: a NVML não é chamada"
    );
    assert_eq!(nv.calls.load(Ordering::SeqCst), 0);
    let g = read_gpus(&r, &list_cards(&r), &mut nv, true, &HashMap::new());
    assert_eq!(
        (g[0].usage, g[0].temp, g[0].freq, g[0].max_freq),
        (Some(0.35), Some(48.0), Some(1200), Some(2100))
    );
    assert_eq!(nv.calls.load(Ordering::SeqCst), 1);
    let g = read_gpus(&r, &list_cards(&r), &mut NoNvidia, false, &HashMap::new());
    assert_eq!(
        (g[0].name.as_str(), g[0].usage),
        ("NVIDIA GeForce RTX 3050 Laptop GPU", None),
        "sem NVML: só o nome"
    );
}

/// Uma placa num sysfs de mentira: `files` são caminhos relativos à pasta da
/// placa, com o conteúdo.
fn write_card(sys: &Path, card: &str, files: &[(&str, &str)]) {
    for (rel, text) in files {
        let path = sys.join("class/drm").join(card).join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

#[test]
fn suspended_intel_has_no_frequency() {
    let t = tempfile::tempdir().unwrap();
    let sys = t.path().join("sys");
    write_card(
        &sys,
        "card0",
        &[
            ("device/vendor", "0x8086\n"),
            ("device/uevent", "PCI_SLOT_NAME=0000:00:02.0\n"),
            ("device/power/runtime_status", "suspended\n"),
            ("gt_act_freq_mhz", "300\n"),
            ("gt_RP0_freq_mhz", "1500\n"),
        ],
    );
    let r = Roots {
        sysfs: sys,
        ..machine()
    };
    let usage = HashMap::from([("0000:00:02.0".to_string(), 0.4)]);
    let g = read_gpus(&r, &list_cards(&r), &mut NoNvidia, false, &usage);
    assert_eq!(
        (g[0].state.as_str(), g[0].usage, g[0].freq, g[0].max_freq),
        ("suspended", None, None, None),
        "Intel suspensa: nem frequência nem uso"
    );
}

#[test]
fn i915_frequency_comes_before_xe() {
    let t = tempfile::tempdir().unwrap();
    let sys = t.path().join("sys");
    write_card(
        &sys,
        "card0",
        &[
            ("device/vendor", "0x8086\n"),
            ("device/uevent", "PCI_SLOT_NAME=0000:00:02.0\n"),
            ("device/power/runtime_status", "active\n"),
            ("gt_act_freq_mhz", "1450\n"),
            ("gt_RP0_freq_mhz", "1500\n"),
            ("device/tile0/gt0/freq0/act_freq", "900\n"),
            ("device/tile0/gt0/freq0/rp0_freq", "2050\n"),
        ],
    );
    let r = Roots {
        sysfs: sys,
        ..machine()
    };
    let g = read_gpus(&r, &list_cards(&r), &mut NoNvidia, false, &HashMap::new());
    assert_eq!((g[0].freq, g[0].max_freq), (Some(1450), Some(1500)));
}

#[test]
fn pci_falls_back_to_the_resolved_device() {
    let t = tempfile::tempdir().unwrap();
    let sys = t.path().join("sys");
    let real = sys.join("devices/pci0000:00/0000:07:00.0");
    std::fs::create_dir_all(real.join("power")).unwrap();
    std::fs::write(real.join("vendor"), "0x1002\n").unwrap();
    std::fs::write(real.join("uevent"), "DRIVER=amdgpu\n").unwrap();
    std::fs::write(real.join("power/runtime_status"), "suspended\n").unwrap();
    std::fs::create_dir_all(sys.join("class/drm/card0")).unwrap();
    symlink(&real, sys.join("class/drm/card0/device")).unwrap();
    let r = Roots {
        sysfs: sys,
        ..machine()
    };
    let c = list_cards(&r);
    assert_eq!(
        c.iter()
            .map(|c| (c.card.as_str(), c.vendor, c.pci.as_str()))
            .collect::<Vec<_>>(),
        [("card0", Vendor::Amd, "0000:07:00.0")],
        "sem PCI_SLOT_NAME: o nome do device resolvido"
    );
}
