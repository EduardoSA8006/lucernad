use lucernad_spectrum::analysis::{
    Analyzer, AutoGain, BANDS, FFT_SIZE, FPS, HIGH_HZ, LOW_HZ, MIN_DB, RANGE_DB, Smoother,
    TILT_DB_PER_OCTAVE, band_edges, band_of, bands_db, levels,
};
use lucernad_spectrum::testing::{noise, sine};

const DT: f32 = 1.0 / FPS as f32;

fn argmax(values: &[f32]) -> usize {
    let mut best = 0;
    for (i, v) in values.iter().enumerate() {
        if *v > values[best] {
            best = i;
        }
    }
    best
}

/// Roda `frames` quadros sobre `signal`, avançando `hop` amostras por quadro.
fn run(analyzer: &mut Analyzer, signal: &[f32], hop: usize, frames: usize) -> Vec<[f32; BANDS]> {
    (0..frames)
        .map(|f| {
            let end = FFT_SIZE + f * hop;
            analyzer.frame(&signal[end - FFT_SIZE..end])
        })
        .collect()
}

#[test]
fn band_edges_are_logarithmic() {
    let edges = band_edges();
    assert!((edges[0] - LOW_HZ).abs() < 0.01);
    assert!((edges[BANDS] - HIGH_HZ).abs() < 0.01);
    let ratio = edges[1] / edges[0];
    for i in 0..BANDS {
        assert!((edges[i + 1] / edges[i] - ratio).abs() < 1e-4);
    }
}

#[test]
fn band_of_maps_frequencies() {
    assert_eq!(band_of(1000.0), Some(25));
    assert_eq!(band_of(100.0), Some(6));
    assert_eq!(band_of(LOW_HZ), Some(0));
    assert_eq!(band_of(LOW_HZ - 1.0), None);
    assert_eq!(band_of(17_000.0), None);
}

#[test]
fn sine_lights_its_band() {
    for (rate, hop) in [(48_000, 800), (44_100, 735)] {
        let signal = sine(1000.0, rate, 0.5, FFT_SIZE + 120 * hop);
        let mut analyzer = Analyzer::new(rate);
        let last = *run(&mut analyzer, &signal, hop, 120).last().unwrap();
        assert_eq!(argmax(&last), 25, "rate {rate}");
        if rate == 48_000 {
            assert!(last[25] > 0.9, "pico {}", last[25]);
            for (b, v) in last.iter().enumerate() {
                if !(22..=28).contains(&b) {
                    assert!(*v < 0.2, "banda {b} = {v}");
                }
            }
        }
    }
}

#[test]
fn silence_is_zero() {
    let mut analyzer = Analyzer::new(48_000);
    for _ in 0..60 {
        assert_eq!(analyzer.frame(&[0.0; FFT_SIZE]), [0.0; BANDS]);
    }
}

#[test]
fn white_noise_spreads() {
    let signal: Vec<f32> = noise(1, FFT_SIZE + 120 * 800)
        .iter()
        .map(|s| s * 0.3)
        .collect();
    let mut analyzer = Analyzer::new(48_000);
    let last = *run(&mut analyzer, &signal, 800, 120).last().unwrap();
    // Com a inclinação de +1,5 dB/oitava (−6,6 dB no centro da banda 0), o
    // ruído branco sobe para os agudos e os graves podem ficar baixos.
    assert!(last.iter().filter(|&&v| v > 0.05).count() >= 36, "{last:?}");
    assert!(last[BANDS - 12..].iter().all(|&v| v > 0.3), "{last:?}");
}

#[test]
fn gain_rises_fast_and_falls_slowly() {
    let mut gain = AutoGain::new();
    for _ in 0..60 {
        gain.step(-6.0, DT);
    }
    assert!(gain.reference_db > -7.0, "{}", gain.reference_db);
    for _ in 0..60 {
        gain.step(-46.0, DT);
    }
    assert!(
        (-10.5..=-9.0).contains(&gain.reference_db),
        "{}",
        gain.reference_db
    );
    for _ in 0..1200 {
        gain.step(-46.0, DT);
    }
    assert_eq!(gain.reference_db, -45.0);

    // Subida: em 0,25 s (15 quadros) cobre 1 − 1/e do caminho de −45 a −6.
    let mut gain = AutoGain::new();
    for _ in 0..15 {
        gain.step(-6.0, DT);
    }
    assert!(
        (-21.0..=-19.7).contains(&gain.reference_db),
        "{}",
        gain.reference_db
    );

    // Um máximo entre o piso e a referência: desce até ele e para.
    let mut gain = AutoGain::new();
    gain.reference_db = -10.0;
    for _ in 0..500 {
        gain.step(-30.0, DT);
    }
    assert_eq!(gain.reference_db, -30.0);
}

#[test]
fn steady_sine_does_not_pump() {
    // Seno constante, com um corte de 6 dB depois de 5 s.
    let hop = 800;
    let total = 600;
    let cut = 300;
    let signal: Vec<f32> = sine(1000.0, 48_000, 0.5, FFT_SIZE + total * hop)
        .iter()
        .enumerate()
        .map(|(i, s)| {
            if i < FFT_SIZE + cut * hop {
                *s
            } else {
                s / 2.0
            }
        })
        .collect();
    let mut analyzer = Analyzer::new(48_000);
    let band: Vec<f32> = run(&mut analyzer, &signal, hop, total)
        .iter()
        .map(|f| f[25])
        .collect();
    let spread = |frames: &[f32]| {
        let min = frames.iter().copied().fold(f32::MAX, f32::min);
        let max = frames.iter().copied().fold(f32::MIN, f32::max);
        max - min
    };
    assert!(spread(&band[240..cut]) < 0.02, "antes do corte");
    assert!(spread(&band[total - 60..]) < 0.02, "depois do corte");
    // O corte não faz o anel despencar nem oscilar de um quadro para o outro.
    let step = band[cut - 10..]
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    assert!(step < 0.05, "passo {step}");
}

#[test]
fn attack_and_gravity() {
    let mut smoother = Smoother::new();
    let up = [1.0; BANDS];
    assert!((smoother.step(&up, DT)[0] - 0.6).abs() < 1e-6);
    assert!((smoother.step(&up, DT)[0] - 0.84).abs() < 1e-6);

    let down = [0.0; BANDS];
    let mut smoother = Smoother::from_values([1.0; BANDS]);
    let mut y = [1.0; BANDS];
    for _ in 0..30 {
        y = smoother.step(&down, DT);
    }
    assert!((y[0] - 0.225).abs() < 1e-4, "{}", y[0]);
    for _ in 30..35 {
        y = smoother.step(&down, DT);
    }
    assert_eq!(y[0], 0.0);

    // Um alvo acima do valor zera a velocidade da queda.
    let mut smoother = Smoother::from_values([1.0; BANDS]);
    for _ in 0..10 {
        smoother.step(&down, DT);
    }
    let before = smoother.step(&[0.0; BANDS], DT)[0];
    let lifted = smoother.step(&[1.0; BANDS], DT)[0];
    assert!(lifted > before);
    let after = smoother.step(&[0.0; BANDS], DT)[0];
    assert!((lifted - after - 6.0 * DT * DT).abs() < 1e-6);
}

#[test]
fn levels_map_the_range() {
    let db = |x: f32| levels(&[x; BANDS], -10.0)[0];
    assert!((db(-10.0) - 1.0).abs() < 1e-6);
    assert!((db(-10.0 - RANGE_DB / 2.0) - 0.5).abs() < 1e-6);
    assert_eq!(db(-10.0 - RANGE_DB), 0.0);
    assert_eq!(db(0.0), 1.0);
}

#[test]
fn short_window_is_padded() {
    let mut analyzer = Analyzer::new(48_000);
    let out = analyzer.frame(&[0.1; 100]);
    assert_eq!(out.len(), BANDS);
    // As 100 amostras ficam na ponta da janela, onde o Hann é ~0: o pico é
    // 1,9e-5 (−94,2 dBFS, no bin 0, abaixo da primeira banda). Sob a
    // referência de −45 dBFS o piso é −99 dBFS: o vazamento mal passa dele
    // em umas bandas graves, e o quadro fica praticamente apagado.
    assert!(out.iter().all(|&v| v < 0.01), "{out:?}");
}

/// Os `FFT_SIZE/2 + 1` bins planos em `db`.
fn flat(db: f32) -> Vec<f32> {
    vec![db; FFT_SIZE / 2 + 1]
}

fn center(edges: &[f32; BANDS + 1], b: usize) -> f32 {
    (edges[b] * edges[b + 1]).sqrt()
}

#[test]
fn tilt_follows_the_octaves() {
    let edges = band_edges();
    let out = bands_db(&flat(-20.0), 48_000);
    for b in [0, 10, 26, 40, 47] {
        let expected = -20.0 + TILT_DB_PER_OCTAVE * (center(&edges, b) / 1000.0).log2();
        assert!((out[b] - expected).abs() < 0.05, "banda {b}: {}", out[b]);
    }
    // Duas bandas separadas por uma oitava (razão 2 entre os centros)
    // diferem TILT_DB_PER_OCTAVE: entre os centros da primeira e da última
    // banda (45 Hz a 16 kHz) são 8,3 oitavas.
    let octaves = (center(&edges, 47) / center(&edges, 0)).log2();
    assert!((out[47] - out[0] - TILT_DB_PER_OCTAVE * octaves).abs() < 0.05);
    assert!((octaves - 8.3).abs() < 0.05, "{octaves}");
}

#[test]
fn band_without_bin_interpolates_at_the_geometric_center() {
    // A banda 1 (50,9 a 57,5 Hz) fica entre os bins 4 e 5 (46,9 e 58,6 Hz).
    let edges = band_edges();
    let mut mags = flat(0.0);
    mags[5] = 100.0;
    let out = bands_db(&mags, 48_000);
    let c = center(&edges, 1);
    let pos = c / (48_000.0 / FFT_SIZE as f32);
    let expected = 100.0 * (pos - 4.0) + TILT_DB_PER_OCTAVE * (c / 1000.0).log2();
    assert!((out[1] - expected).abs() < 0.05, "{} vs {expected}", out[1]);
}

#[test]
fn bands_above_nyquist_are_silent() {
    // A 24 kHz, Nyquist é 12 kHz: a banda a 16 kHz não tem o que medir.
    let out = bands_db(&flat(-20.0), 24_000);
    let edges = band_edges();
    let expected = MIN_DB + TILT_DB_PER_OCTAVE * (center(&edges, 47) / 1000.0).log2();
    assert!((out[47] - expected).abs() < 0.05, "{}", out[47]);
    assert!(out[26] > -30.0, "{}", out[26]);
}
