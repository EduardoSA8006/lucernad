use lucernad_spectrum::analysis::{
    Analyzer, AutoGain, BANDS, BASS_BORROW, BASS_RANGE_DB, FFT_SIZE, FPS, GRAVITY, HIGH_HZ,
    LOW_FFT_SIZE, LOW_HZ, LOW_SPLIT_HZ, MIN_DB, RANGE_DB, Smoother, TILT_DB_PER_OCTAVE, band_bins,
    band_edges, band_of, bands_db, bands_db_split, borrow_bass, fft_size_of, levels, range_of,
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
            let end = LOW_FFT_SIZE + f * hop;
            analyzer.frame(&signal[end - LOW_FFT_SIZE..end])
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
    assert_eq!(band_of(1000.0), Some(21));
    assert_eq!(band_of(100.0), Some(5));
    assert_eq!(band_of(LOW_HZ), Some(0));
    assert_eq!(band_of(LOW_HZ - 1.0), None);
    assert_eq!(band_of(17_000.0), None);
}

#[test]
fn sine_lights_its_band() {
    for (rate, hop) in [(48_000, 800), (44_100, 735)] {
        let signal = sine(1000.0, rate, 0.5, LOW_FFT_SIZE + 120 * hop);
        let mut analyzer = Analyzer::new(rate);
        let last = *run(&mut analyzer, &signal, hop, 120).last().unwrap();
        assert_eq!(argmax(&last), 21, "rate {rate}");
        if rate == 48_000 {
            assert!(last[21] > 0.9, "pico {}", last[21]);
            for (b, v) in last.iter().enumerate() {
                if !(18..=24).contains(&b) {
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
        assert_eq!(analyzer.frame(&[0.0; LOW_FFT_SIZE]), [0.0; BANDS]);
    }
}

#[test]
fn white_noise_spreads() {
    let signal: Vec<f32> = noise(1, LOW_FFT_SIZE + 120 * 800)
        .iter()
        .map(|s| s * 0.3)
        .collect();
    let mut analyzer = Analyzer::new(48_000);
    let last = *run(&mut analyzer, &signal, 800, 120).last().unwrap();
    // Com a inclinação de +1,5 dB/oitava (−6,6 dB no centro da banda 0), o
    // ruído branco sobe para os agudos e os graves podem ficar baixos.
    assert!(last.iter().filter(|&&v| v > 0.05).count() >= 30, "{last:?}");
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
    let signal: Vec<f32> = sine(1000.0, 48_000, 0.5, LOW_FFT_SIZE + total * hop)
        .iter()
        .enumerate()
        .map(|(i, s)| {
            if i < LOW_FFT_SIZE + cut * hop {
                *s
            } else {
                s / 2.0
            }
        })
        .collect();
    let mut analyzer = Analyzer::new(48_000);
    let band: Vec<f32> = run(&mut analyzer, &signal, hop, total)
        .iter()
        .map(|f| f[21])
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

    // A queda: depois de k quadros, 1 − GRAVITY·dt²·k(k+1)/2; de 1 a 0 em
    // ~0,4 s (24 quadros com GRAVITY 12).
    let down = [0.0; BANDS];
    let mut smoother = Smoother::from_values([1.0; BANDS]);
    let mut y = [1.0; BANDS];
    for _ in 0..15 {
        y = smoother.step(&down, DT);
    }
    let expected = 1.0 - GRAVITY * DT * DT * (15.0 * 16.0) / 2.0;
    assert!((y[0] - expected).abs() < 1e-4, "{} vs {expected}", y[0]);
    let mut frames = 15;
    while y[0] > 0.0 {
        y = smoother.step(&down, DT);
        frames += 1;
    }
    let fall = frames as f32 * DT;
    assert!((0.35..=0.45).contains(&fall), "queda em {fall} s");

    // Um alvo acima do valor zera a velocidade da queda.
    let mut smoother = Smoother::from_values([1.0; BANDS]);
    for _ in 0..10 {
        smoother.step(&down, DT);
    }
    let before = smoother.step(&[0.0; BANDS], DT)[0];
    let lifted = smoother.step(&[1.0; BANDS], DT)[0];
    assert!(lifted > before);
    let after = smoother.step(&[0.0; BANDS], DT)[0];
    assert!((lifted - after - GRAVITY * DT * DT).abs() < 1e-6);
}

#[test]
fn levels_map_the_range() {
    // Uma banda que não é grave: RANGE_DB.
    let db = |x: f32| levels(&[x; BANDS], -10.0)[BANDS - 1];
    assert!((db(-10.0) - 1.0).abs() < 1e-6);
    assert!((db(-10.0 - RANGE_DB / 2.0) - 0.5).abs() < 1e-6);
    assert_eq!(db(-10.0 - RANGE_DB), 0.0);
    assert_eq!(db(0.0), 1.0);
}

#[test]
fn bass_bands_have_a_shorter_range() {
    // As bandas da FFT dos graves usam BASS_RANGE_DB; as outras, RANGE_DB.
    for b in 0..BANDS {
        let expected = if fft_size_of(b) == LOW_FFT_SIZE {
            BASS_RANGE_DB
        } else {
            RANGE_DB
        };
        assert_eq!(range_of(b), expected, "banda {b}");
    }
    // O mesmo nível, metade da faixa dos graves abaixo da referência: 0,5
    // na banda 0 e mais que isso numa banda de cima.
    let out = levels(&[-10.0 - BASS_RANGE_DB / 2.0; BANDS], -10.0);
    assert!((out[0] - 0.5).abs() < 1e-6, "{}", out[0]);
    let expected = 1.0 - BASS_RANGE_DB / 2.0 / RANGE_DB;
    assert!((out[BANDS - 1] - expected).abs() < 1e-6);
    // BASS_RANGE_DB abaixo da referência a banda grave já é 0.
    assert_eq!(levels(&[-10.0 - BASS_RANGE_DB; BANDS], -10.0)[0], 0.0);
}

#[test]
fn bass_borrows_from_the_band_above() {
    let bass = (0..BANDS)
        .filter(|&b| fft_size_of(b) == LOW_FFT_SIZE)
        .count();
    // Graves em zero e a primeira banda de cima em 0,8: em cascata, cada
    // grave fica com BASS_BORROW da vizinha (0,4, 0,2, 0,1…).
    let mut input = [0.0; BANDS];
    input[bass] = 0.8;
    let out = borrow_bass(&input);
    for (b, level) in out.iter().enumerate().take(bass) {
        let expected = 0.8 * BASS_BORROW.powi((bass - b) as i32);
        assert!((level - expected).abs() < 1e-6, "banda {b}: {level}");
    }
    // Uma banda grave acima do empréstimo fica como está; as de cima não mudam.
    let mut input = [0.3; BANDS];
    input[0] = 0.9;
    let out = borrow_bass(&input);
    assert_eq!(out[0], 0.9);
    assert_eq!(out[bass..], input[bass..]);
    // As de cima nunca emprestam.
    let mut input = [0.0; BANDS];
    input[BANDS - 1] = 1.0;
    assert_eq!(borrow_bass(&input)[BANDS - 2], 0.0);
}

#[test]
fn short_window_is_padded() {
    let mut analyzer = Analyzer::new(48_000);
    let out = analyzer.frame(&[0.1; 100]);
    assert_eq!(out.len(), BANDS);
    // As 100 amostras ficam na ponta da janela, onde o Hann é ~0: o pico é
    // ínfimo (no bin 0, abaixo da primeira banda). Sob a
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
    for b in [0, 10, 21, 33, BANDS - 1] {
        let expected = -20.0 + TILT_DB_PER_OCTAVE * (center(&edges, b) / 1000.0).log2();
        assert!((out[b] - expected).abs() < 0.05, "banda {b}: {}", out[b]);
    }
    // Duas bandas separadas por uma oitava (razão 2 entre os centros)
    // diferem TILT_DB_PER_OCTAVE: entre os centros da primeira e da última
    // banda (45 Hz a 14 kHz) são 8,07 oitavas.
    let last = BANDS - 1;
    let octaves = (center(&edges, last) / center(&edges, 0)).log2();
    assert!((out[last] - out[0] - TILT_DB_PER_OCTAVE * octaves).abs() < 0.05);
    assert!((octaves - 8.07).abs() < 0.01, "{octaves}");
}

#[test]
fn band_without_bin_interpolates_at_the_geometric_center() {
    // Numa FFT de FFT_SIZE, a banda 2 (60,0 a 69,2 Hz) fica entre os bins 5
    // e 6 (58,6 e 70,3 Hz).
    let edges = band_edges();
    let mut mags = flat(0.0);
    mags[6] = 100.0;
    let out = bands_db(&mags, 48_000);
    let c = center(&edges, 2);
    let pos = c / (48_000.0 / FFT_SIZE as f32);
    let expected = 100.0 * (pos - 5.0) + TILT_DB_PER_OCTAVE * (c / 1000.0).log2();
    assert!((out[2] - expected).abs() < 0.05, "{} vs {expected}", out[2]);
}

#[test]
fn bands_above_nyquist_are_silent() {
    // A 24 kHz, Nyquist é 12 kHz: a última banda (12,1 a 14 kHz) não tem o
    // que medir.
    let out = bands_db(&flat(-20.0), 24_000);
    let edges = band_edges();
    let expected = MIN_DB + TILT_DB_PER_OCTAVE * (center(&edges, BANDS - 1) / 1000.0).log2();
    assert!(
        (out[BANDS - 1] - expected).abs() < 0.05,
        "{}",
        out[BANDS - 1]
    );
    assert!(out[21] > -30.0, "{}", out[21]);
}

#[test]
fn every_band_has_its_own_bin() {
    // Os graves pela FFT maior: nenhuma banda interpola, a 48 e a 44,1 kHz.
    for rate in [48_000, 44_100] {
        for b in 0..BANDS {
            let bins = band_bins(b, fft_size_of(b), rate);
            assert!(!bins.is_empty(), "banda {b} sem bin a {rate} Hz");
        }
    }
    // Só as bandas com centro abaixo de LOW_SPLIT_HZ usam a FFT maior: as 6
    // primeiras (45 a 106 Hz).
    let edges = band_edges();
    for b in 0..BANDS {
        let low = (edges[b] * edges[b + 1]).sqrt() < LOW_SPLIT_HZ;
        assert_eq!(fft_size_of(b) == LOW_FFT_SIZE, low, "banda {b}");
    }
    assert_eq!(
        (0..BANDS)
            .filter(|&b| fft_size_of(b) == LOW_FFT_SIZE)
            .count(),
        6
    );
    // Com a FFT de FFT_SIZE só, a banda 2 ficaria sem bin.
    assert!(band_bins(2, FFT_SIZE, 48_000).is_empty());
}

#[test]
fn split_takes_the_bass_from_the_bigger_fft() {
    let low = vec![-10.0; LOW_FFT_SIZE / 2 + 1];
    let high = flat(-30.0);
    let out = bands_db_split(&low, &high, 48_000);
    let edges = band_edges();
    for (b, level) in out.iter().enumerate() {
        let base = if fft_size_of(b) == LOW_FFT_SIZE {
            -10.0
        } else {
            -30.0
        };
        let expected = base + TILT_DB_PER_OCTAVE * (center(&edges, b) / 1000.0).log2();
        assert!((level - expected).abs() < 0.05, "banda {b}: {level}");
    }
}

#[test]
fn low_sine_lights_its_band() {
    // Um seno no bin 8 da FFT dos graves (46,9 Hz, na banda 0, de 45 a
    // 51,9 Hz) acende a banda 0; o vizinho (52,7 Hz, banda 1) fica 6 dB
    // abaixo pelo Hann.
    let hop = 800;
    let freq = 8.0 * 48_000.0 / LOW_FFT_SIZE as f32;
    let signal = sine(freq, 48_000, 0.5, LOW_FFT_SIZE + 120 * hop);
    let mut analyzer = Analyzer::new(48_000);
    let last = *run(&mut analyzer, &signal, hop, 120).last().unwrap();
    assert_eq!(argmax(&last), 0, "{last:?}");
    assert!(last[0] > 0.9, "{}", last[0]);
}
