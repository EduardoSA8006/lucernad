use std::time::{Duration, Instant};

use lucernad_clipboard::testing::{jpeg_bytes, png_bytes};
use lucernad_clipboard::thumb::{extension_for, sniff, thumbnail};

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn decoded_size(png: &[u8]) -> (u32, u32) {
    let img = image::load_from_memory(png).unwrap();
    (img.width(), img.height())
}

#[test]
fn png_thumbnail_keeps_aspect() {
    let t = thumbnail(&png_bytes(600, 400), "image/png").unwrap();
    assert_eq!((t.width, t.height), (600, 400));
    assert_eq!(decoded_size(&t.png), (272, 181));
}

#[test]
fn jpeg_and_small_images() {
    let t = thumbnail(&jpeg_bytes(100, 50), "image/jpeg").unwrap();
    assert_eq!((t.width, t.height), (100, 50));
    assert_eq!(decoded_size(&t.png), (100, 50));
}

#[test]
fn huge_dimensions_are_refused() {
    let mut png = png_bytes(1, 1);
    // assinatura (8) + tamanho (4) + "IHDR" (4) + dados (13) + CRC (4)
    png[16..20].copy_from_slice(&100_000u32.to_be_bytes());
    png[20..24].copy_from_slice(&100_000u32.to_be_bytes());
    let crc = crc32(&png[12..29]);
    png[29..33].copy_from_slice(&crc.to_be_bytes());
    let start = Instant::now();
    assert!(thumbnail(&png, "image/png").is_err());
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn unknown_image_has_no_thumbnail() {
    let err = thumbnail(b"GIF89a", "image/gif").unwrap_err();
    assert!(err.contains("sem miniatura"));
    assert_eq!(extension_for("image/jpeg"), "jpg");
    assert_eq!(extension_for("image/x-foo"), "img");
}

#[test]
fn sniff_reads_the_signature() {
    assert_eq!(sniff(&png_bytes(2, 2)), Some("image/png"));
    assert_eq!(sniff(&jpeg_bytes(2, 2)), Some("image/jpeg"));
    assert_eq!(sniff(b"%PDF-1.7"), None);
}
