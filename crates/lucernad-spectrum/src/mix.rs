//! Os anéis de amostras por nó e a janela somada para a análise.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::analysis::FFT_SIZE;

/// Um anel sem amostra há mais que isto conta como silêncio.
pub const STALE: Duration = Duration::from_millis(100);

struct Ring {
    samples: VecDeque<f32>,
    last: Instant,
}

/// As últimas `FFT_SIZE` amostras de cada nó, por `object.serial`.
#[derive(Default)]
pub struct Rings {
    rings: HashMap<u64, Ring>,
}

impl Rings {
    pub fn new() -> Rings {
        Rings::default()
    }

    /// Acrescenta `mono` ao anel do nó `serial`, guardando as últimas
    /// `FFT_SIZE`.
    pub fn push(&mut self, serial: u64, mono: &[f32], now: Instant) {
        let ring = self.rings.entry(serial).or_insert_with(|| Ring {
            samples: VecDeque::with_capacity(FFT_SIZE),
            last: now,
        });
        ring.last = now;
        let keep = &mono[mono.len().saturating_sub(FFT_SIZE)..];
        let overflow = (ring.samples.len() + keep.len()).saturating_sub(FFT_SIZE);
        ring.samples.drain(..overflow);
        ring.samples.extend(keep);
    }

    /// Mantém só os nós de `serials`.
    pub fn retain(&mut self, serials: &[u64]) {
        self.rings.retain(|serial, _| serials.contains(serial));
    }

    /// Esquece todos os nós.
    pub fn clear(&mut self) {
        self.rings.clear();
    }

    /// A soma dos anéis frescos, alinhada pelo fim, com `FFT_SIZE` amostras;
    /// um anel velho conta zero.
    pub fn window(&self, now: Instant) -> Vec<f32> {
        let mut sum = vec![0.0; FFT_SIZE];
        for ring in self.rings.values() {
            if now.saturating_duration_since(ring.last) > STALE {
                continue;
            }
            let offset = FFT_SIZE - ring.samples.len();
            for (acc, &s) in sum[offset..].iter_mut().zip(&ring.samples) {
                *acc += s;
            }
        }
        sum
    }
}
