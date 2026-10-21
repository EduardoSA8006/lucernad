use std::time::{Duration, Instant};

use lucernad_spectrum::analysis::FFT_SIZE;
use lucernad_spectrum::mix::Rings;

#[test]
fn rings_sum_fresh_nodes() {
    let t0 = Instant::now();
    let mut rings = Rings::new();
    rings.push(1, &[0.25; FFT_SIZE], t0);
    rings.push(2, &[0.5; FFT_SIZE], t0);
    assert_eq!(rings.window(t0), vec![0.75; FFT_SIZE]);
}

#[test]
fn window_keeps_the_last_samples() {
    let t0 = Instant::now();
    let mut rings = Rings::new();
    let ramp: Vec<f32> = (0..5000).map(|i| i as f32).collect();
    rings.push(1, &ramp, t0);
    let window = rings.window(t0);
    assert_eq!(window.len(), FFT_SIZE);
    assert_eq!(window[0], 904.0);
    assert_eq!(window[FFT_SIZE - 1], 4999.0);

    let mut rings = Rings::new();
    rings.push(1, &[1.0; 10], t0);
    let window = rings.window(t0);
    assert!(window[..FFT_SIZE - 10].iter().all(|&s| s == 0.0));
    assert!(window[FFT_SIZE - 10..].iter().all(|&s| s == 1.0));
}

#[test]
fn stale_ring_counts_silence() {
    let t0 = Instant::now();
    let later = t0 + Duration::from_millis(150);
    let mut rings = Rings::new();
    rings.push(1, &[0.25; FFT_SIZE], t0);
    assert_eq!(rings.window(later), vec![0.0; FFT_SIZE]);

    rings.push(2, &[0.5; FFT_SIZE], later);
    assert_eq!(rings.window(later), vec![0.5; FFT_SIZE]);
}

#[test]
fn retain_drops_nodes() {
    let t0 = Instant::now();
    let mut rings = Rings::new();
    rings.push(1, &[0.25; FFT_SIZE], t0);
    rings.push(2, &[0.5; FFT_SIZE], t0);
    rings.retain(&[2]);
    assert_eq!(rings.window(t0), vec![0.5; FFT_SIZE]);
    rings.clear();
    assert_eq!(rings.window(t0), vec![0.0; FFT_SIZE]);
}
