//! Ajudantes para os testes (feature `testing`).

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::mpsc::{self, UnboundedSender};

use crate::backend::{Backend, Control, Event, OutputNode, Session};

/// O caminho de `rel` dentro do `testdata/` deste crate.
pub fn testdata(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join(rel)
}

/// `n` amostras de `amp·sin(2π·freq·i/rate)`.
pub fn sine(freq: f32, rate: u32, amp: f32, n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let phase = std::f64::consts::TAU * f64::from(freq) * i as f64 / f64::from(rate);
            amp * phase.sin() as f32
        })
        .collect()
}

/// `n` amostras de ruído branco em [−1, 1), de um gerador linear congruente
/// próprio: a mesma `seed` dá a mesma sequência.
pub fn noise(seed: u64, n: usize) -> Vec<f32> {
    let mut state = seed;
    (0..n)
        .map(|_| {
            // As constantes do MMIX (Knuth); os 24 bits altos viram [0, 1).
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let unit = (state >> 40) as f32 / (1u64 << 24) as f32;
            unit * 2.0 - 1.0
        })
        .collect()
}

/// O dublê do [`Backend`]: o teste põe os nós e as amostras na mão.
#[derive(Clone, Default)]
pub struct FakeBackend {
    state: Arc<Mutex<FakeState>>,
}

#[derive(Default)]
struct FakeState {
    nodes: Vec<OutputNode>,
    failing: Option<String>,
    panicking: bool,
    opened: usize,
    live: usize,
    /// A sessão aberta por último, enquanto viva.
    current: Option<FakeSession>,
}

struct FakeSession {
    id: usize,
    /// `None` depois do [`FakeBackend::disconnect`].
    events: Option<UnboundedSender<Event>>,
    capturing: Vec<u64>,
}

impl FakeBackend {
    /// Um dublê que abre sessões.
    pub fn new() -> FakeBackend {
        FakeBackend::default()
    }

    /// Um dublê cujos `probe` e `open` falham com `msg`.
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

    /// Guarda os nós e manda o `Graph` à sessão aberta.
    pub fn set_nodes(&self, nodes: Vec<OutputNode>) {
        let mut state = self.lock();
        state.nodes = nodes.clone();
        if let Some(events) = state.current.as_ref().and_then(|s| s.events.as_ref()) {
            let _ = events.send(Event::Graph(nodes));
        }
    }

    /// Manda `Samples` à sessão aberta, só se `serial` está na captura atual.
    pub fn play(&self, serial: u64, rate: u32, mono: &[f32]) {
        let state = self.lock();
        let Some(session) = state.current.as_ref() else {
            return;
        };
        if let Some(events) = &session.events
            && session.capturing.contains(&serial)
        {
            let _ = events.send(Event::Samples {
                serial,
                rate,
                mono: mono.to_vec(),
            });
        }
    }

    /// Manda `Disconnected(why)` à sessão aberta e fecha o canal dela.
    pub fn disconnect(&self, why: &str) {
        let mut state = self.lock();
        if let Some(events) = state.current.as_mut().and_then(|s| s.events.take()) {
            let _ = events.send(Event::Disconnected(why.to_string()));
        }
    }

    /// As sessões abertas agora (ainda não largadas).
    pub fn sessions(&self) -> usize {
        self.lock().live
    }

    /// As sessões abertas desde a criação do dublê.
    pub fn opened(&self) -> usize {
        self.lock().opened
    }

    /// O último `capture` da sessão aberta; vazio sem sessão.
    pub fn capturing(&self) -> Vec<u64> {
        self.lock()
            .current
            .as_ref()
            .map(|s| s.capturing.clone())
            .unwrap_or_default()
    }

    fn lock(&self) -> MutexGuard<'_, FakeState> {
        // Um teste que entra em pânico com o lock na mão não deve derrubar os
        // outros que dividem o dublê.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Backend for FakeBackend {
    fn probe(&self) -> Result<(), String> {
        match &self.lock().failing {
            Some(msg) => Err(msg.clone()),
            None => Ok(()),
        }
    }

    fn open(&self) -> Result<Session, String> {
        let mut state = self.lock();
        if state.panicking {
            drop(state);
            panic!("o FakeBackend entrou em pânico no open");
        }
        if let Some(msg) = &state.failing {
            return Err(msg.clone());
        }
        let (tx, events) = mpsc::unbounded_channel();
        let _ = tx.send(Event::Graph(state.nodes.clone()));
        state.opened += 1;
        state.live += 1;
        let id = state.opened;
        state.current = Some(FakeSession {
            id,
            events: Some(tx),
            capturing: Vec::new(),
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
    fn capture(&self, serials: &[u64]) {
        let mut state = self.lock();
        if let Some(session) = state.current.as_mut().filter(|s| s.id == self.id) {
            session.capturing = serials.to_vec();
        }
    }
}

impl Drop for FakeControl {
    fn drop(&mut self) {
        let mut state = self.lock();
        state.live -= 1;
        if state.current.as_ref().is_some_and(|s| s.id == self.id) {
            state.current = None;
        }
    }
}
