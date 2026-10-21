//! Dublês e dados de teste (feature `testing`).

use std::io::Cursor;

use image::{ImageFormat, RgbImage};

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
