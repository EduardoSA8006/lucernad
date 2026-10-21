use lucernad_spectrum::analysis::{
    Analyzer, AutoGain, BANDS, FFT_SIZE, FPS, Smoother, band_edges, band_of, levels,
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
    assert!((edges[0] - 30.0).abs() < 0.01);
    assert!((edges[BANDS] - 16_000.0).abs() < 0.01);
    let ratio = edges[1] / edges[0];
    for i in 0..BANDS {
        assert!((edges[i + 1] / edges[i] - ratio).abs() < 1e-4);
    }
}

#[test]
fn band_of_maps_frequencies() {
    assert_eq!(band_of(1000.0), Some(26));
    assert_eq!(band_of(100.0), Some(9));
    assert_eq!(band_of(20.0), None);
    assert_eq!(band_of(17_000.0), None);
}

#[test]
fn sine_lights_its_band() {
    for (rate, hop) in [(48_000, 800), (44_100, 735)] {
        let signal = sine(1000.0, rate, 0.5, FFT_SIZE + 120 * hop);
        let mut analyzer = Analyzer::new(rate);
        let last = *run(&mut analyzer, &signal, hop, 120).last().unwrap();
        assert_eq!(argmax(&last), 26, "rate {rate}");
        if rate == 48_000 {
            assert!(last[26] > 0.9, "pico {}", last[26]);
            for (b, v) in last.iter().enumerate() {
                if !(23..=29).contains(&b) {
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
    // Com a inclinação de +3 dB/oitava (−15,2 dB a 30 Hz), o ruído branco
    // sobe para os agudos e os graves podem ficar baixos.
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
}

#[test]
fn steady_sine_does_not_pump() {
    let hop = 800;
    let signal = sine(1000.0, 48_000, 0.5, FFT_SIZE + 360 * hop);
    let mut analyzer = Analyzer::new(48_000);
    let frames = run(&mut analyzer, &signal, hop, 360);
    let tail: Vec<f32> = frames[300..].iter().map(|f| f[26]).collect();
    let min = tail.iter().copied().fold(f32::MAX, f32::min);
    let max = tail.iter().copied().fold(f32::MIN, f32::max);
    assert!(max - min < 0.02, "variou {}", max - min);
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
    assert!((db(-34.0) - 0.5).abs() < 1e-6);
    assert_eq!(db(-58.0), 0.0);
    assert_eq!(db(0.0), 1.0);
}

#[test]
fn short_window_is_padded() {
    let mut analyzer = Analyzer::new(48_000);
    let out = analyzer.frame(&[0.1; 100]);
    assert_eq!(out.len(), BANDS);
    // As 100 amostras ficam na ponta da janela, onde o Hann é ~0: o módulo
    // (< 4e-6, abaixo de −108 dBFS) cai fora dos 48 dB sob a referência
    // de −45 dBFS, e o quadro é de zeros.
    assert_eq!(out, [0.0; BANDS]);
}
