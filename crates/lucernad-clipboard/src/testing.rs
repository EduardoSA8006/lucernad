//! Dublês e dados de teste (feature `testing`).

use std::collections::HashMap;
use std::io::{Cursor, PipeReader, PipeWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use image::{ImageFormat, RgbImage};
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::backend::{Backend, Control, Event, Session, Source};
use crate::wayland::SELECTION_CHANGED;

fn gradient(w: u32, h: u32) -> RgbImage {
    RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([(x * 255 / w.max(1)) as u8, (y * 255 / h.max(1)) as u8, 128])
    })
}

fn encode(w: u32, h: u32, format: ImageFormat) -> Vec<u8> {
    let mut out = Vec::new();
    gradient(w, h)
        .write_to(&mut Cursor::new(&mut out), format)
        .expect("codificar a imagem de teste");
    out
}

/// Um PNG com um degradê.
pub fn png_bytes(w: u32, h: u32) -> Vec<u8> {
    encode(w, h, ImageFormat::Png)
}

/// Um JPEG com um degradê.
pub fn jpeg_bytes(w: u32, h: u32) -> Vec<u8> {
    encode(w, h, ImageFormat::Jpeg)
}

/// Um JPEG com a orientação EXIF dada (1 a 8) num APP1 logo depois do SOI.
pub fn jpeg_bytes_oriented(w: u32, h: u32, orientation: u16) -> Vec<u8> {
    let plain = jpeg_bytes(w, h);
    let mut app1 = vec![0xFF, 0xE1, 0x00, 34];
    app1.extend_from_slice(b"Exif\0\0");
    app1.extend_from_slice(b"MM\0*\0\0\0\x08");
    app1.extend_from_slice(&[0, 1, 0x01, 0x12, 0, 3, 0, 0, 0, 1]);
    app1.extend_from_slice(&orientation.to_be_bytes());
    app1.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    let mut out = plain[..2].to_vec();
    out.extend_from_slice(&app1);
    out.extend_from_slice(&plain[2..]);
    out
}

/// A trava da sonda: entra em pânico a menos que haja `LUCERNA_ISOLADO=1`
/// (que só o `tests/isolado.sh` exporta) **e** um container (`/.dockerenv`
/// ou `/run/.containerenv`). Um `cargo test -- --ignored` no host nunca
/// alcança a sessão do usuário.
pub fn isolated() {
    let flag = std::env::var_os("LUCERNA_ISOLADO").is_some_and(|v| v == "1");
    let container = Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists();
    assert!(
        flag && container,
        "sonda.rs: só roda pelo tests/isolado.sh, num container"
    );
}

/// O dublê do [`Backend`]: o teste copia, cola e derruba na mão.
#[derive(Clone, Default)]
pub struct FakeBackend {
    state: Arc<Mutex<FakeState>>,
}

#[derive(Default)]
struct FakeState {
    failing: Option<String>,
    panicking: bool,
    opened: usize,
    live: usize,
    sockets: Vec<PathBuf>,
    reads: usize,
    /// O número da última seleção.
    next: u64,
    /// A seleção atual (de outro app).
    selection: Option<FakeSelection>,
    /// A nossa fonte, enquanto for a seleção.
    owned: Option<Source>,
    /// As pontas de escrita das cópias presas, que nunca fecham.
    stuck: Vec<PipeWriter>,
    /// A sessão aberta por último, enquanto viva.
    current: Option<FakeSession>,
}

struct FakeSelection {
    offer: u64,
    data: HashMap<String, Vec<u8>>,
    stuck: bool,
}

struct FakeSession {
    id: usize,
    /// `None` depois do [`FakeBackend::disconnect`].
    events: Option<UnboundedSender<Event>>,
}

impl FakeState {
    fn send(&self, event: Event) {
        if let Some(events) = self.current.as_ref().and_then(|s| s.events.as_ref()) {
            let _ = events.send(event);
        }
    }
}

impl FakeBackend {
    /// Um dublê que abre sessões.
    pub fn new() -> FakeBackend {
        FakeBackend::default()
    }

    /// Um dublê cujo `open` falha com `msg`.
    pub fn failing(msg: &str) -> FakeBackend {
        let fake = FakeBackend::new();
        fake.lock().failing = Some(msg.to_string());
        fake
    }

    /// Um dublê cujo `open` entra em pânico.
    pub fn panicking() -> FakeBackend {
        let fake = FakeBackend::new();
        fake.lock().panicking = true;
        fake
    }

    /// Outro app copia: manda `Selection` com os `mimes` à sessão aberta e
    /// guarda os bytes de cada tipo (um tipo sem bytes devolve vazio).
    /// Devolve o número da oferta.
    pub fn copy(&self, mimes: &[&str], data: &[(&str, &[u8])]) -> u64 {
        let data = data
            .iter()
            .map(|(mime, bytes)| (mime.to_string(), bytes.to_vec()))
            .collect();
        self.select(mimes, data, false)
    }

    /// Como o [`FakeBackend::copy`], mas o `receive` devolve um pipe que
    /// nunca fecha (o app que não termina de escrever).
    pub fn copy_stuck(&self, mimes: &[&str]) -> u64 {
        self.select(mimes, HashMap::new(), true)
    }

    fn select(&self, mimes: &[&str], data: HashMap<String, Vec<u8>>, stuck: bool) -> u64 {
        let mut state = self.lock();
        state.next += 1;
        let offer = state.next;
        state.selection = Some(FakeSelection { offer, data, stuck });
        state.send(Event::Selection {
            offer,
            mimes: mimes.iter().map(|m| m.to_string()).collect(),
        });
        offer
    }

    /// Outro app copiou por cima da nossa fonte: `Cancelled`, se havia uma.
    pub fn cancel(&self) {
        let mut state = self.lock();
        if state.owned.take().is_some() {
            state.send(Event::Cancelled);
        }
    }

    /// Manda `Disconnected(why)` à sessão aberta e fecha o canal dela.
    pub fn disconnect(&self, why: &str) {
        let mut state = self.lock();
        if let Some(events) = state.current.as_mut().and_then(|s| s.events.take()) {
            let _ = events.send(Event::Disconnected(why.to_string()));
        }
    }

    /// O que a nossa fonte serve no tipo `mime`; `None` sem fonte ou sem o
    /// tipo.
    pub fn paste(&self, mime: &str) -> Option<Vec<u8>> {
        let state = self.lock();
        let source = state.owned.as_ref()?;
        source
            .offers
            .iter()
            .find(|(m, _)| m == mime)
            .map(|(_, bytes)| bytes.to_vec())
    }

    /// Os tipos da nossa fonte, enquanto ela for a seleção.
    pub fn owned(&self) -> Option<Vec<String>> {
        let state = self.lock();
        let source = state.owned.as_ref()?;
        Some(source.offers.iter().map(|(m, _)| m.clone()).collect())
    }

    /// Quantos `receive` as sessões pediram.
    pub fn reads(&self) -> usize {
        self.lock().reads
    }

    /// As sessões abertas agora (ainda não largadas).
    pub fn sessions(&self) -> usize {
        self.lock().live
    }

    /// As sessões abertas desde a criação do dublê.
    pub fn opened(&self) -> usize {
        self.lock().opened
    }

    /// Os sockets pedidos ao `open`, em ordem.
    pub fn sockets(&self) -> Vec<PathBuf> {
        self.lock().sockets.clone()
    }

    fn lock(&self) -> MutexGuard<'_, FakeState> {
        // Um teste que entra em pânico com o lock na mão não deve derrubar os
        // outros que dividem o dublê.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Backend for FakeBackend {
    fn open(&self, socket: &Path) -> Result<Session, String> {
        let mut state = self.lock();
        if state.panicking {
            drop(state);
            panic!("o FakeBackend entrou em pânico no open");
        }
        state.sockets.push(socket.to_path_buf());
        if let Some(msg) = &state.failing {
            return Err(msg.clone());
        }
        let (tx, events) = mpsc::unbounded_channel();
        state.opened += 1;
        state.live += 1;
        let id = state.opened;
        state.current = Some(FakeSession {
            id,
            events: Some(tx),
        });
        Ok(Session {
            events,
            control: Box::new(FakeControl {
                id,
                state: self.state.clone(),
            }),
        })
    }
}

struct FakeControl {
    id: usize,
    state: Arc<Mutex<FakeState>>,
}

impl FakeControl {
    fn lock(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Control for FakeControl {
    fn receive(&self, offer: u64, mime: &str) -> Result<PipeReader, String> {
        let mut state = self.lock();
        state.reads += 1;
        let Some(selection) = state.selection.as_ref().filter(|s| s.offer == offer) else {
            return Err(SELECTION_CHANGED.to_string());
        };
        let (reader, mut writer) = std::io::pipe().map_err(|e| e.to_string())?;
        if selection.stuck {
            state.stuck.push(writer);
        } else {
            let bytes = selection.data.get(mime).cloned().unwrap_or_default();
            std::thread::spawn(move || {
                let _ = writer.write_all(&bytes);
            });
        }
        Ok(reader)
    }

    fn offer(&self, source: Source) {
        let mut state = self.lock();
        if state.current.as_ref().is_some_and(|s| s.id == self.id) {
            state.owned = Some(source);
        }
    }
}

impl Drop for FakeControl {
    fn drop(&mut self) {
        let mut state = self.lock();
        state.live -= 1;
        if state.current.as_ref().is_some_and(|s| s.id == self.id) {
            state.current = None;
            // Largar a sessão fecha a conexão, e a nossa fonte vai junto.
            state.owned = None;
        }
    }
}
