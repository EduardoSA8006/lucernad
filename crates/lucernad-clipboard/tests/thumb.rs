use std::time::{Duration, Instant};

use lucernad_clipboard::testing::{jpeg_bytes, jpeg_bytes_oriented, png_bytes};
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

/// Um PNG de 1 x 1 com o IHDR reescrito para `w` x `h` (o CRC recalculado).
fn png_declaring(w: u32, h: u32) -> Vec<u8> {
    let mut png = png_bytes(1, 1);
    // assinatura (8) + tamanho (4) + "IHDR" (4) + dados (13) + CRC (4)
    png[16..20].copy_from_slice(&w.to_be_bytes());
    png[20..24].copy_from_slice(&h.to_be_bytes());
    let crc = crc32(&png[12..29]);
    png[29..33].copy_from_slice(&crc.to_be_bytes());
    png
}

#[test]
fn huge_dimensions_are_refused() {
    let start = Instant::now();
    assert!(thumbnail(&png_declaring(100_000, 100_000), "image/png").is_err());
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn declared_allocation_over_the_limit_is_refused() {
    // 16 000 x 16 000 cabe no limite de lado, mas são 768 MB de saída (> 512 MiB).
    let start = Instant::now();
    let err = thumbnail(&png_declaring(16_000, 16_000), "image/png").unwrap_err();
    assert!(err.to_lowercase().contains("limit"), "{err}");
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn exif_orientation_is_applied() {
    // Orientação 6: gira 90°, então a imagem vista é 40 x 100.
    let t = thumbnail(&jpeg_bytes_oriented(100, 40, 6), "image/jpeg").unwrap();
    assert_eq!((t.width, t.height), (40, 100));
    assert_eq!(decoded_size(&t.png), (40, 100));
    let t = thumbnail(&jpeg_bytes_oriented(100, 40, 1), "image/jpeg").unwrap();
    assert_eq!((t.width, t.height), (100, 40));
}

#[test]
fn mime_case_is_ignored() {
    assert!(thumbnail(&png_bytes(4, 4), "IMAGE/PNG").is_ok());
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
