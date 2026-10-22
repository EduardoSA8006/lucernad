//! Ajudantes para os testes (feature `testing`): o montador de árvores falsas
//! do sysfs, um EDID de exemplo, os barramentos i2c falsos do DDC/CI e os
//! uevents falsos, o D-Bus falso ([`FakeBus`]), a escrita falsa do brilho
//! ([`FakeWriter`]) e a trava da sonda ([`isolated`]).
//!
//! Nada aqui toca o `/sys` ou o `/dev` de verdade: tudo fica sob a pasta dada
//! ao [`Tree::new`], o [`FakeI2c`] nunca abre um `/dev/i2c-N` e o
//! [`FakeUevents`] nunca abre o socket netlink; o [`FakeBus`] e o
//! [`FakeWriter`] nunca falam com o D-Bus.

use std::collections::HashMap;
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use crate::bus::{BacklightWriter, BoxFuture, Point, PowerBus, Profiles, TreeWriter};
use crate::ddc::{DDC_ADDR, DdcTiming, EDID_ADDR, VCP_BRIGHTNESS};
use crate::i2c::{I2cBus, I2cOpener};
use crate::roots::Roots;
use crate::uevent::{Uevent, Uevents};

/// Monta uma árvore falsa do sysfs sobre um diretório, com os links
/// simbólicos de verdade (`class/drm/<card>` → `devices/<pci>/drm/<card>`,
/// `class/i2c-dev/i2c-N/device` → o adaptador, o `ddc` dos conectores).
pub struct Tree {
    base: PathBuf,
}

impl Tree {
    /// Uma árvore vazia (só `sys` e `dev`) em `dir`.
    pub fn new(dir: &Path) -> Tree {
        let tree = Tree {
            base: dir.to_path_buf(),
        };
        mkdir(&tree.sys(""));
        mkdir(&tree.base.join("dev"));
        tree
    }

    /// A raiz desta árvore.
    pub fn roots(&self) -> Roots {
        Roots::at(&self.base)
    }

    /// Um backlight em `class/backlight/<device>`, com o `device` apontando
    /// para o conector `connector` (`card2-eDP-1`) da placa dele, se dado.
    pub fn backlight(
        &self,
        device: &str,
        kind: &str,
        max: u32,
        value: u32,
        actual: u32,
        connector: Option<&str>,
    ) {
        let dir = self.sys("class/backlight").join(device);
        mkdir(&dir);
        write(&dir.join("type"), kind);
        write(&dir.join("max_brightness"), &max.to_string());
        write(&dir.join("brightness"), &value.to_string());
        write(&dir.join("actual_brightness"), &actual.to_string());
        write(&dir.join("bl_power"), "0");
        if let Some(connector) = connector {
            let class = self.sys("class/drm").join(connector);
            let target = class.canonicalize().unwrap_or(class);
            link(&target, &dir.join("device"));
        }
    }

    /// Um dispositivo em `devices/<path>`, com o `class` se dado.
    pub fn device(&self, path: &str, class: Option<&str>) {
        let dir = self.sys("devices").join(path);
        mkdir(&dir);
        if let Some(class) = class {
            write(&dir.join("class"), class);
        }
    }

    /// Uma placa de vídeo: `devices/<pci>` com o `class`, a `drm/<card>` sob
    /// ela (com o `device` de volta) e o `class/drm/<card>`.
    pub fn gpu(&self, pci: &str, class: &str, card: &str) {
        self.device(pci, Some(class));
        let pci_dir = self.sys("devices").join(pci);
        let card_dir = pci_dir.join("drm").join(card);
        mkdir(&card_dir);
        link(&pci_dir, &card_dir.join("device"));
        link(&card_dir, &self.sys("class/drm").join(card));
    }

    /// Um conector `<card>-<name>` da placa `card` (criada antes pelo
    /// [`Tree::gpu`]), com o `status` e o `edid`; com `ddc`, o barramento
    /// AUX `i2c-<n>` sob o conector e o link `ddc` para ele. Chamado de novo,
    /// troca o que já havia.
    pub fn connector(&self, card: &str, name: &str, status: &str, edid: &[u8], ddc: Option<u32>) {
        let card_dir = self
            .sys("class/drm")
            .join(card)
            .canonicalize()
            .expect("a placa do conector (Tree::gpu antes)");
        let entry = format!("{card}-{name}");
        let dir = card_dir.join(&entry);
        mkdir(&dir);
        write(&dir.join("status"), status);
        std::fs::write(dir.join("edid"), edid).expect("escrever o edid");
        link(&dir, &self.sys("class/drm").join(&entry));
        if let Some(n) = ddc {
            let adapter = self.adapter(n, &dir);
            link(&adapter, &dir.join("ddc"));
        }
    }

    /// Um barramento `i2c-<n>` com o `i2c-dev` sob `devices/<parent>`.
    pub fn bus(&self, n: u32, parent: &str) {
        let parent = self.sys("devices").join(parent);
        self.adapter(n, &parent);
    }

    /// Troca o `brightness` do backlight `device`.
    pub fn set_value(&self, device: &str, value: u32) {
        let path = self.brightness(device);
        write(&path, &value.to_string());
    }

    /// O `brightness` do backlight `device`.
    pub fn value(&self, device: &str) -> u32 {
        let path = self.brightness(device);
        let text = std::fs::read_to_string(&path).expect("ler o brightness");
        text.trim().parse().expect("o brightness é um número")
    }

    fn sys(&self, rel: &str) -> PathBuf {
        self.base.join("sys").join(rel)
    }

    fn brightness(&self, device: &str) -> PathBuf {
        self.sys("class/backlight").join(device).join("brightness")
    }

    /// O adaptador `i2c-<n>` sob `parent`, com o `i2c-dev/i2c-<n>` (e o
    /// `device` de volta) e o `class/i2c-dev/i2c-<n>`. Devolve o adaptador.
    fn adapter(&self, n: u32, parent: &Path) -> PathBuf {
        let bus = format!("i2c-{n}");
        let adapter = parent.join(&bus);
        let dev = adapter.join("i2c-dev").join(&bus);
        mkdir(&dev);
        write(&adapter.join("name"), &format!("adaptador falso {n}"));
        write(&dev.join("dev"), &format!("89:{n}"));
        link(&adapter, &dev.join("device"));
        link(&dev, &self.sys("class/i2c-dev").join(&bus));
        adapter
    }
}

/// 128 bytes de um EDID 1.4 válido, com o `serial` no cabeçalho, o nome no
/// descritor 0xFC (cortado em 13 bytes) e a soma de verificação certa.
pub fn edid(name: &str, serial: u32) -> Vec<u8> {
    let mut edid = vec![0u8; 128];
    edid[..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
    edid[8..10].copy_from_slice(&[0x1E, 0x6D]); // "GSM"
    edid[10..12].copy_from_slice(&[0x01, 0x77]);
    edid[12..16].copy_from_slice(&serial.to_le_bytes());
    edid[16] = 1; // semana
    edid[17] = 32; // 2022
    edid[18] = 1;
    edid[19] = 4;
    // Descritor 0: um modo de exemplo (pixel clock diferente de zero).
    edid[54..56].copy_from_slice(&[0x02, 0x3A]);
    // Descritor 1: o número de série em texto (0xFF).
    descriptor(&mut edid[72..90], 0xFF, &serial.to_string());
    // Descritor 2: o nome (0xFC).
    descriptor(&mut edid[90..108], 0xFC, name);
    // Descritor 3: vazio (0x10).
    edid[111] = 0x10;
    let sum = edid[..127].iter().fold(0u8, |acc, b| acc.wrapping_add(*b));
    edid[127] = 0u8.wrapping_sub(sum);
    edid
}

/// Um descritor de texto: `00 00 00 <tag> 00`, até 13 bytes, `0x0A` e espaços.
fn descriptor(slot: &mut [u8], tag: u8, text: &str) {
    slot[..5].copy_from_slice(&[0, 0, 0, tag, 0]);
    let text = &text.as_bytes()[..text.len().min(13)];
    slot[5..5 + text.len()].copy_from_slice(text);
    if text.len() < 13 {
        slot[5 + text.len()] = 0x0A;
        slot[6 + text.len()..].fill(0x20);
    }
}

fn mkdir(dir: &Path) {
    std::fs::create_dir_all(dir).expect("criar a pasta da árvore falsa");
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, format!("{text}\n")).expect("escrever na árvore falsa");
}

/// Um link simbólico em `at` para `target`, trocando o que houver.
fn link(target: &Path, at: &Path) {
    if let Some(parent) = at.parent() {
        mkdir(parent);
    }
    if at.symlink_metadata().is_ok() {
        std::fs::remove_file(at).expect("trocar o link da árvore falsa");
    }
    symlink(target, at).expect("criar o link da árvore falsa");
}

/// Os atrasos do DDC/CI zerados, para os testes não esperarem.
pub fn fast_timing() -> DdcTiming {
    DdcTiming {
        get_delay: Duration::ZERO,
        set_delay: Duration::ZERO,
        retry_delay: Duration::ZERO,
    }
}

/// Um monitor falso atrás de um barramento do [`FakeI2c`].
#[derive(Clone, Debug)]
pub struct FakeMonitor {
    /// O EDID que ele devolve em 0x50.
    pub edid: Vec<u8>,
    /// O brilho atual (VCP 0x10).
    pub current: u16,
    /// O máximo do brilho.
    pub max: u16,
    /// O que ele guarda a mais ou a menos do que recebe num Set VCP.
    pub off_by: i16,
}

/// Barramentos i2c falsos, compartilhados entre os clones: cada barramento
/// tem (ou não) um [`FakeMonitor`] e os defeitos que o teste pedir. Abrir um
/// barramento sem monitor dá certo e toda leitura dele vem com zeros, como um
/// barramento vazio de verdade.
#[derive(Clone, Default)]
pub struct FakeI2c {
    shared: Arc<FakeShared>,
}

#[derive(Default)]
struct FakeShared {
    state: Mutex<FakeState>,
    /// Acorda as leituras presas pelo [`FakeI2c::hang`].
    wake: Condvar,
}

#[derive(Default)]
struct FakeState {
    buses: HashMap<u32, I2cState>,
    opened: Vec<u32>,
}

#[derive(Default)]
struct I2cState {
    monitor: Option<FakeMonitor>,
    denied: bool,
    silent: bool,
    busy: usize,
    corrupt: usize,
    slow: Duration,
    hang: bool,
    writes: Vec<(u16, Vec<u8>)>,
    locks: usize,
    unlocks: usize,
    reads: usize,
    /// O que a última escrita pediu.
    pending: Pending,
}

#[derive(Clone, Copy, Default)]
enum Pending {
    #[default]
    Nothing,
    Vcp(u8),
    Edid(usize),
}

impl FakeI2c {
    /// Nenhum barramento com monitor.
    pub fn new() -> FakeI2c {
        FakeI2c::default()
    }

    /// Põe `m` no barramento `bus`.
    pub fn monitor(self, bus: u32, m: FakeMonitor) -> FakeI2c {
        self.with(bus, |b| b.monitor = Some(m));
        self
    }

    /// Abrir o barramento `bus` dá `EACCES`.
    pub fn denied(self, bus: u32) -> FakeI2c {
        self.with(bus, |b| b.denied = true);
        self
    }

    /// O barramento `bus` só devolve zeros.
    pub fn silent(self, bus: u32) -> FakeI2c {
        self.with(bus, |b| b.silent = true);
        self
    }

    /// As próximas `n` leituras de VCP do `bus` devolvem a "null message".
    pub fn busy_times(&self, bus: u32, n: usize) {
        self.with(bus, |b| b.busy = n);
    }

    /// As próximas `n` leituras de VCP do `bus` vêm com o checksum errado.
    pub fn corrupt_times(&self, bus: u32, n: usize) {
        self.with(bus, |b| b.corrupt = n);
    }

    /// Cada escrita e cada leitura no `bus` demora `d`.
    pub fn slow(&self, bus: u32, d: Duration) {
        self.with(bus, |b| b.slow = d);
    }

    /// A próxima leitura do `bus` não volta até o [`FakeI2c::release`].
    pub fn hang(&self, bus: u32) {
        self.with(bus, |b| b.hang = true);
    }

    /// Solta a leitura presa pelo [`FakeI2c::hang`].
    pub fn release(&self, bus: u32) {
        self.with(bus, |b| b.hang = false);
        self.shared.wake.notify_all();
    }

    /// Troca o brilho do monitor do `bus` por fora (o botão do monitor).
    pub fn set_current(&self, bus: u32, v: u16) {
        self.with(bus, |b| {
            if let Some(m) = b.monitor.as_mut() {
                m.current = v;
            }
        });
    }

    /// O brilho do monitor do `bus` (0 sem monitor).
    pub fn current(&self, bus: u32) -> u16 {
        self.with(bus, |b| b.monitor.as_ref().map_or(0, |m| m.current))
    }

    /// Cada `open`, na ordem.
    pub fn opened(&self) -> Vec<u32> {
        self.state().opened.clone()
    }

    /// O endereço e os bytes de cada escrita no `bus`.
    pub fn writes(&self, bus: u32) -> Vec<(u16, Vec<u8>)> {
        self.with(bus, |b| b.writes.clone())
    }

    /// Quantos `lock` e quantos `unlock` no `bus`.
    pub fn locks(&self, bus: u32) -> (usize, usize) {
        self.with(bus, |b| (b.locks, b.unlocks))
    }

    /// Quantas leituras no `bus`.
    pub fn reads(&self, bus: u32) -> usize {
        self.with(bus, |b| b.reads)
    }

    fn state(&self) -> MutexGuard<'_, FakeState> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn with<T>(&self, bus: u32, f: impl FnOnce(&mut I2cState) -> T) -> T {
        f(self.state().buses.entry(bus).or_default())
    }

    fn slowness(&self, bus: u32) {
        let d = self.with(bus, |b| b.slow);
        if !d.is_zero() {
            std::thread::sleep(d);
        }
    }
}

impl I2cOpener for FakeI2c {
    fn open(&self, bus: u32) -> io::Result<Box<dyn I2cBus>> {
        let mut state = self.state();
        state.opened.push(bus);
        if state.buses.entry(bus).or_default().denied {
            return Err(io::Error::from_raw_os_error(13)); // EACCES
        }
        Ok(Box::new(OpenBus {
            i2c: self.clone(),
            bus,
        }))
    }
}

/// Um barramento aberto do [`FakeI2c`].
struct OpenBus {
    i2c: FakeI2c,
    bus: u32,
}

impl I2cBus for OpenBus {
    fn write(&mut self, addr: u16, data: &[u8]) -> io::Result<()> {
        self.i2c.slowness(self.bus);
        self.i2c.with(self.bus, |b| {
            b.writes.push((addr, data.to_vec()));
            b.pending = Pending::Nothing;
            let valid = data.len() >= 3
                && data[0] == 0x51
                && usize::from(data[1] & 0x7F) + 3 == data.len()
                && data.iter().fold(0x6E, |acc, x| acc ^ x) == 0;
            match (addr, data) {
                (EDID_ADDR, [offset]) => b.pending = Pending::Edid(usize::from(*offset)),
                (DDC_ADDR, [_, _, 0x01, code, _]) if valid => b.pending = Pending::Vcp(*code),
                (DDC_ADDR, [_, _, 0x03, VCP_BRIGHTNESS, high, low, _]) if valid => {
                    if let Some(m) = b.monitor.as_mut() {
                        let asked = i32::from(u16::from_be_bytes([*high, *low]));
                        let kept = (asked + i32::from(m.off_by)).clamp(0, i32::from(m.max));
                        m.current = kept as u16;
                    }
                }
                _ => {}
            }
        });
        Ok(())
    }

    fn read(&mut self, addr: u16, buf: &mut [u8]) -> io::Result<()> {
        self.i2c.slowness(self.bus);
        let shared = &self.i2c.shared;
        let mut state = self.i2c.state();
        while state.buses.get(&self.bus).is_some_and(|b| b.hang) {
            state = shared
                .wake
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        let b = state.buses.entry(self.bus).or_default();
        b.reads += 1;
        buf.fill(0);
        let Some(m) = b.monitor.as_ref().filter(|_| !b.silent) else {
            return Ok(());
        };
        match (addr, b.pending) {
            (DDC_ADDR, Pending::Vcp(code)) => {
                let reply = if b.busy > 0 {
                    b.busy -= 1;
                    vec![0x6E, 0x80, 0xBE]
                } else {
                    let result = u8::from(code != VCP_BRIGHTNESS);
                    let [max_h, max_l] = m.max.to_be_bytes();
                    let [cur_h, cur_l] = m.current.to_be_bytes();
                    let mut reply = vec![
                        0x6E, 0x88, 0x02, result, code, 0x00, max_h, max_l, cur_h, cur_l,
                    ];
                    let mut chk = reply.iter().fold(0x50, |acc, x| acc ^ x);
                    if b.corrupt > 0 {
                        b.corrupt -= 1;
                        chk ^= 0x01;
                    }
                    reply.push(chk);
                    reply
                };
                let n = reply.len().min(buf.len());
                buf[..n].copy_from_slice(&reply[..n]);
            }
            (EDID_ADDR, Pending::Edid(offset)) => {
                let edid = m.edid.get(offset..).unwrap_or(&[]);
                let n = edid.len().min(buf.len());
                buf[..n].copy_from_slice(&edid[..n]);
            }
            _ => {}
        }
        Ok(())
    }

    fn lock(&mut self) -> io::Result<()> {
        self.i2c.with(self.bus, |b| b.locks += 1);
        Ok(())
    }

    fn unlock(&mut self) {
        self.i2c.with(self.bus, |b| b.unlocks += 1);
    }
}

/// Uevents falsos: o teste manda eventos e perdas para quem estiver
/// escutando, sem socket nem thread.
#[derive(Clone, Default)]
pub struct FakeUevents {
    senders: Arc<Mutex<Vec<tokio::sync::mpsc::UnboundedSender<Uevent>>>>,
}

impl FakeUevents {
    /// Sem ninguém escutando.
    pub fn new() -> FakeUevents {
        FakeUevents::default()
    }

    /// Manda um evento a todos os receptores vivos.
    pub fn send(&self, action: &str, subsystem: &str, devpath: &str) {
        self.deliver(Uevent::Event {
            action: action.to_owned(),
            subsystem: subsystem.to_owned(),
            devpath: devpath.to_owned(),
        });
    }

    /// Manda um [`Uevent::Lost`] (o `ENOBUFS` do socket).
    pub fn lose(&self) {
        self.deliver(Uevent::Lost);
    }

    /// Quantos receptores estão vivos.
    pub fn watching(&self) -> usize {
        let mut senders = lock(&self.senders);
        senders.retain(|tx| !tx.is_closed());
        senders.len()
    }

    fn deliver(&self, event: Uevent) {
        let mut senders = lock(&self.senders);
        senders.retain(|tx| tx.send(event.clone()).is_ok());
    }
}

impl Uevents for FakeUevents {
    fn watch(&self) -> Result<tokio::sync::mpsc::UnboundedReceiver<Uevent>, String> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        lock(&self.senders).push(tx);
        Ok(rx)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A trava da sonda do D-Bus (`tests/sonda.rs`): só passa com
/// `LUCERNA_ISOLADO=1` **e** dentro de um container (`/.dockerenv` ou
/// `/run/.containerenv`), que é onde o `tests/isolado.sh` sobe um D-Bus do
/// sistema de mentira. Fora disso, entra em pânico: a sonda nunca fala com o
/// logind, o UPower ou os perfis da máquina.
pub fn isolated() {
    let flag = std::env::var_os("LUCERNA_ISOLADO").is_some_and(|v| v == "1");
    let container = Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists();
    assert!(
        flag && container,
        "sonda.rs: só roda pelo tests/isolado.sh, num container"
    );
}

/// O dublê do [`PowerBus`]: o teste dá o histórico, faz falhar e muda os
/// perfis na mão.
#[derive(Clone)]
pub struct FakeBus {
    state: Arc<Mutex<FakeBusState>>,
}

struct FakeBusState {
    history: Vec<Point>,
    failing: Option<String>,
    panicking: bool,
    history_calls: usize,
    profiles: Profiles,
    senders: Vec<tokio::sync::mpsc::UnboundedSender<Profiles>>,
}

impl Default for FakeBus {
    fn default() -> FakeBus {
        FakeBus {
            state: Arc::new(Mutex::new(FakeBusState {
                history: Vec::new(),
                failing: None,
                panicking: false,
                history_calls: 0,
                profiles: Profiles {
                    available: true,
                    performance: true,
                },
                senders: Vec::new(),
            })),
        }
    }
}

impl FakeBus {
    /// Sem histórico (sem bateria) e com os perfis `{true, true}`.
    pub fn new() -> FakeBus {
        FakeBus::default()
    }

    /// Com estes pontos no histórico.
    pub fn with_history(self, points: Vec<Point>) -> FakeBus {
        lock(&self.state).history = points;
        self
    }

    /// Um dublê cujo `history` entra em pânico.
    pub fn panicking() -> FakeBus {
        let bus = FakeBus::new();
        lock(&bus.state).panicking = true;
        bus
    }

    /// Daqui em diante, o `history` falha com `e`.
    pub fn history_fails(&self, e: &str) {
        lock(&self.state).failing = Some(e.to_owned());
    }

    /// Muda os perfis e manda a todos os receptores vivos.
    pub fn set_profiles(&self, p: Profiles) {
        let mut state = lock(&self.state);
        state.profiles = p;
        state.senders.retain(|tx| tx.send(p).is_ok());
    }

    /// Quantas vezes o `history` foi chamado.
    pub fn history_calls(&self) -> usize {
        lock(&self.state).history_calls
    }

    /// Quantos receptores dos perfis estão vivos.
    pub fn watching(&self) -> usize {
        let mut state = lock(&self.state);
        state.senders.retain(|tx| !tx.is_closed());
        state.senders.len()
    }
}

impl PowerBus for FakeBus {
    fn history(&self) -> BoxFuture<'_, Result<Vec<Point>, String>> {
        let result = {
            let mut state = lock(&self.state);
            state.history_calls += 1;
            assert!(!state.panicking, "FakeBus: o history em pânico");
            match &state.failing {
                Some(e) => Err(e.clone()),
                None => Ok(state.history.clone()),
            }
        };
        Box::pin(async move { result })
    }

    fn watch_profiles(&self) -> tokio::sync::mpsc::UnboundedReceiver<Profiles> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = lock(&self.state);
        let _ = tx.send(state.profiles);
        state.senders.push(tx);
        rx
    }
}

/// O dublê do [`BacklightWriter`]: grava na árvore falsa como o
/// [`TreeWriter`] e anota cada escrita feita; o teste faz falhar ou demorar.
#[derive(Clone)]
pub struct FakeWriter {
    tree: Arc<TreeWriter>,
    state: Arc<Mutex<FakeWriterState>>,
}

#[derive(Default)]
struct FakeWriterState {
    failing: Option<String>,
    slow: Duration,
    writes: Vec<(String, u32)>,
}

impl FakeWriter {
    /// Grava no `brightness` (e no `actual_brightness`) da árvore `tree`.
    pub fn new(tree: Tree) -> FakeWriter {
        FakeWriter {
            tree: Arc::new(TreeWriter {
                roots: tree.roots(),
            }),
            state: Arc::default(),
        }
    }

    /// Daqui em diante, toda escrita falha com `e` (sem gravar).
    pub fn fails(&self, e: &str) {
        lock(&self.state).failing = Some(e.to_owned());
    }

    /// Daqui em diante, toda escrita demora `d` antes de gravar.
    pub fn slow(&self, d: Duration) {
        lock(&self.state).slow = d;
    }

    /// As escritas que deram certo, em ordem: `(dispositivo, valor)`.
    pub fn writes(&self) -> Vec<(String, u32)> {
        lock(&self.state).writes.clone()
    }
}

impl BacklightWriter for FakeWriter {
    fn write<'a>(&'a self, device: &'a str, value: u32) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let slow = lock(&self.state).slow;
            if !slow.is_zero() {
                tokio::time::sleep(slow).await;
            }
            if let Some(e) = lock(&self.state).failing.clone() {
                return Err(e);
            }
            self.tree.write(device, value).await?;
            lock(&self.state).writes.push((device.to_owned(), value));
            Ok(())
        })
    }
}
