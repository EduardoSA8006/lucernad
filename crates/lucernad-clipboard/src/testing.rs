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
