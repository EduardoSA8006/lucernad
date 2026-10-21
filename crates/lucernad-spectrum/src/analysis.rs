//! A análise como função pura: amostras → 40 bandas de 0 a 1.
//!
//! Janela de Hann, FFT (uma de `FFT_SIZE` e, para os graves, uma de
//! `LOW_FFT_SIZE`), o maior bin de cada banda logarítmica, inclinação, ganho
//! automático e suavização.

use std::f32::consts::TAU;
use std::ops::Range;
use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

/// O número de bandas.
pub const BANDS: usize = 40;
/// O tamanho da FFT, em amostras.
pub const FFT_SIZE: usize = 4096;
/// O tamanho da FFT dos graves (as bandas com centro abaixo de
/// `LOW_SPLIT_HZ`), em amostras: o dobro, para cada banda grave ter bin
/// próprio (5,9 Hz por bin a 48 kHz). É também o tamanho da janela guardada.
pub const LOW_FFT_SIZE: usize = 2 * FFT_SIZE;
/// As bandas com centro abaixo disto usam a FFT dos graves.
pub const LOW_SPLIT_HZ: f32 = 100.0;
/// Os quadros por segundo.
pub const FPS: u32 = 60;
/// A frequência da borda de baixo da primeira banda.
pub const LOW_HZ: f32 = 45.0;
/// A frequência da borda de cima da última banda.
pub const HIGH_HZ: f32 = 14_000.0;
/// A faixa mostrada, em dB abaixo da referência.
pub const RANGE_DB: f32 = 54.0;
/// A inclinação em volta de 1 kHz, em dB por oitava.
pub const TILT_DB_PER_OCTAVE: f32 = 1.5;
/// A parte do caminho até o alvo que o valor sobe por quadro.
pub const ATTACK: f32 = 0.6;
/// A gravidade da queda, em unidades por segundo ao quadrado.
pub const GRAVITY: f32 = 12.0;
/// A constante de tempo da subida da referência, em segundos.
pub const REF_RISE_S: f32 = 0.25;
/// A descida da referência, em dB por segundo.
pub const REF_FALL_DB_S: f32 = 3.0;
/// O piso da referência, em dBFS.
pub const REF_FLOOR_DB: f32 = -45.0;
/// O nível do silêncio, em dBFS.
pub const MIN_DB: f32 = -120.0;

/// A duração de um quadro, em segundos.
const FRAME_DT: f32 = 1.0 / FPS as f32;

/// As `BANDS + 1` bordas das bandas, de `LOW_HZ` a `HIGH_HZ`, em escala log.
pub fn band_edges() -> [f32; BANDS + 1] {
    let ratio = HIGH_HZ / LOW_HZ;
    std::array::from_fn(|i| LOW_HZ * ratio.powf(i as f32 / BANDS as f32))
}

/// A banda que contém `freq`; `None` fora de [`LOW_HZ`, `HIGH_HZ`).
pub fn band_of(freq: f32) -> Option<usize> {
    if !(LOW_HZ..HIGH_HZ).contains(&freq) {
        return None;
    }
    let edges = band_edges();
    (0..BANDS).find(|&b| freq < edges[b + 1])
}

/// Os módulos dos `FFT_SIZE/2 + 1` bins de `window`, em dBFS, com janela de
/// Hann: um seno de amplitude 1 no centro de um bin dá 0 dB.
///
/// Usa as **primeiras** `FFT_SIZE` amostras de `window` e preenche com zeros
/// à direita se faltarem (o contrário do [`Analyzer::frame`], que usa as
/// últimas e preenche à esquerda).
pub fn magnitudes_db(window: &[f32]) -> Vec<f32> {
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    spectrum_db(fft.as_ref(), window)
}

/// Os módulos em dBFS dos `len/2 + 1` bins de uma FFT do tamanho de `fft`,
/// sobre as primeiras amostras de `window` (zeros se faltarem).
fn spectrum_db(fft: &dyn Fft<f32>, window: &[f32]) -> Vec<f32> {
    let size = fft.len();
    let mut buf: Vec<Complex<f32>> = (0..size)
        .map(|n| {
            let hann = 0.5 * (1.0 - (TAU * n as f32 / size as f32).cos());
            Complex::new(window.get(n).copied().unwrap_or(0.0) * hann, 0.0)
        })
        .collect();
    fft.process(&mut buf);
    buf[..=size / 2]
        .iter()
        .map(|x| 20.0 * (x.norm() * 4.0 / size as f32).max(1e-6).log10())
        .collect()
}

/// O tamanho da FFT que mede a banda `b`: `LOW_FFT_SIZE` para as bandas com
/// centro abaixo de `LOW_SPLIT_HZ`, `FFT_SIZE` para as outras.
pub fn fft_size_of(b: usize) -> usize {
    let edges = band_edges();
    if (edges[b] * edges[b + 1]).sqrt() < LOW_SPLIT_HZ {
        LOW_FFT_SIZE
    } else {
        FFT_SIZE
    }
}

/// Os bins da banda `b` numa FFT de `size` amostras a `rate`; vazio quando
/// nenhum bin cai dentro dela.
pub fn band_bins(b: usize, size: usize, rate: u32) -> Range<usize> {
    let edges = band_edges();
    let bin_hz = rate as f32 / size as f32;
    let first = (edges[b] / bin_hz).ceil() as usize;
    let end = (first..=size / 2)
        .find(|&k| k as f32 * bin_hz >= edges[b + 1])
        .unwrap_or(size / 2 + 1);
    first..end.max(first)
}

/// O nível de cada banda, em dB, com a inclinação de `TILT_DB_PER_OCTAVE`,
/// sobre os bins `mags_db` de uma única FFT (o tamanho sai do número de
/// bins: `(mags_db.len() - 1) * 2`).
///
/// O valor é o maior bin dentro da banda; sem bin, a interpolação linear no
/// centro geométrico; acima de Nyquist, `MIN_DB`.
pub fn bands_db(mags_db: &[f32], rate: u32) -> [f32; BANDS] {
    let edges = band_edges();
    let bin_hz = rate as f32 / ((mags_db.len().max(2) - 1) * 2) as f32;
    std::array::from_fn(|b| {
        let (lo, hi) = (edges[b], edges[b + 1]);
        let center = (lo * hi).sqrt();
        let first = (lo / bin_hz).ceil() as usize;
        let level = if let Some(max) = (first..mags_db.len())
            .take_while(|&k| (k as f32) * bin_hz < hi)
            .map(|k| mags_db[k])
            .reduce(f32::max)
        {
            max
        } else {
            let pos = center / bin_hz;
            let i = pos.floor() as usize;
            match (mags_db.get(i), mags_db.get(i + 1)) {
                (Some(a), Some(c)) => a + (c - a) * (pos - i as f32),
                // Acima de Nyquist não há bin vizinho.
                _ => MIN_DB,
            }
        };
        level + TILT_DB_PER_OCTAVE * (center / 1000.0).log2()
    })
}

/// As bandas pelas duas FFTs: as de [`fft_size_of`] `LOW_FFT_SIZE` saem de
/// `low_mags_db`, as outras de `mags_db`.
pub fn bands_db_split(low_mags_db: &[f32], mags_db: &[f32], rate: u32) -> [f32; BANDS] {
    let low = bands_db(low_mags_db, rate);
    let high = bands_db(mags_db, rate);
    std::array::from_fn(|b| {
        if fft_size_of(b) == LOW_FFT_SIZE {
            low[b]
        } else {
            high[b]
        }
    })
}

/// A referência do ganho automático, em dBFS.
pub struct AutoGain {
    pub reference_db: f32,
}

impl AutoGain {
    pub fn new() -> AutoGain {
        AutoGain {
            reference_db: REF_FLOOR_DB,
        }
    }

    /// Sobe rápido até o maior nível do quadro, desce devagar e nunca fica
    /// abaixo de `REF_FLOOR_DB`.
    pub fn step(&mut self, frame_max_db: f32, dt: f32) {
        if frame_max_db > self.reference_db {
            self.reference_db +=
                (frame_max_db - self.reference_db) * (1.0 - (-dt / REF_RISE_S).exp());
        } else {
            self.reference_db = (self.reference_db - REF_FALL_DB_S * dt).max(frame_max_db);
        }
        self.reference_db = self.reference_db.max(REF_FLOOR_DB);
    }
}

impl Default for AutoGain {
    fn default() -> AutoGain {
        AutoGain::new()
    }
}

/// Os níveis de 0 a 1: `reference_db` é 1 e `RANGE_DB` abaixo dela é 0.
pub fn levels(bands_db: &[f32; BANDS], reference_db: f32) -> [f32; BANDS] {
    bands_db.map(|db| ((db - (reference_db - RANGE_DB)) / RANGE_DB).clamp(0.0, 1.0))
}

/// A suavização: ataque rápido, queda com gravidade.
pub struct Smoother {
    values: [f32; BANDS],
    velocity: [f32; BANDS],
}

impl Smoother {
    pub fn new() -> Smoother {
        Smoother::from_values([0.0; BANDS])
    }

    /// Um suavizador que começa em `values`.
    pub fn from_values(values: [f32; BANDS]) -> Smoother {
        Smoother {
            values,
            velocity: [0.0; BANDS],
        }
    }

    /// Avança um quadro de `dt` segundos rumo a `target`.
    pub fn step(&mut self, target: &[f32; BANDS], dt: f32) -> [f32; BANDS] {
        for ((y, v), &t) in self.values.iter_mut().zip(&mut self.velocity).zip(target) {
            if t >= *y {
                *y += (t - *y) * ATTACK;
                *v = 0.0;
            } else {
                *v += GRAVITY * dt;
                *y = t.max(*y - *v * dt);
            }
        }
        self.values
    }
}

impl Default for Smoother {
    fn default() -> Smoother {
        Smoother::new()
    }
}

/// A cadeia inteira: janela de amostras → 40 bandas suavizadas.
pub struct Analyzer {
    rate: u32,
    fft: Arc<dyn Fft<f32>>,
    low_fft: Arc<dyn Fft<f32>>,
    gain: AutoGain,
    smoother: Smoother,
}

impl Analyzer {
    pub fn new(rate: u32) -> Analyzer {
        let mut planner = FftPlanner::<f32>::new();
        Analyzer {
            rate,
            fft: planner.plan_fft_forward(FFT_SIZE),
            low_fft: planner.plan_fft_forward(LOW_FFT_SIZE),
            gain: AutoGain::new(),
            smoother: Smoother::new(),
        }
    }

    /// Um quadro, sobre as últimas `LOW_FFT_SIZE` amostras de `window` (os
    /// graves) e as últimas `FFT_SIZE` (o resto); com menos que isso, zeros à
    /// esquerda.
    pub fn frame(&mut self, window: &[f32]) -> [f32; BANDS] {
        let tail = &window[window.len().saturating_sub(LOW_FFT_SIZE)..];
        let mut padded = vec![0.0; LOW_FFT_SIZE - tail.len()];
        padded.extend_from_slice(tail);
        let db = bands_db_split(
            &spectrum_db(self.low_fft.as_ref(), &padded),
            &spectrum_db(self.fft.as_ref(), &padded[LOW_FFT_SIZE - FFT_SIZE..]),
            self.rate,
        );
        let max = db.iter().copied().fold(MIN_DB, f32::max);
        self.gain.step(max, FRAME_DT);
        self.smoother
            .step(&levels(&db, self.gain.reference_db), FRAME_DT)
    }
}
