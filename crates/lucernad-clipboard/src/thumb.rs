//! A miniatura das imagens copiadas, com limites contra a bomba de pixels.

use std::io::Cursor;

use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};

/// O lado maior da miniatura.
pub const THUMB_SIDE: u32 = 272;
/// O maior lado aceito na decodificação.
pub const MAX_SIDE: u32 = 16_384;
/// A maior alocação aceita na decodificação.
pub const MAX_ALLOC: u64 = 512 << 20;

/// A miniatura em PNG e as dimensões da imagem original.
#[derive(Debug)]
pub struct Thumb {
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
}

/// Decodifica com limites e reduz até `THUMB_SIDE`, sem aumentar.
pub fn thumbnail(bytes: &[u8], mime: &str) -> Result<Thumb, String> {
    let format = match mime {
        "image/png" => ImageFormat::Png,
        "image/jpeg" => ImageFormat::Jpeg,
        "image/webp" => ImageFormat::WebP,
        other => return Err(format!("sem miniatura para {other}")),
    };
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let (width, height) = decoder.dimensions();
    let img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    let small = if width > THUMB_SIDE || height > THUMB_SIDE {
        img.thumbnail(THUMB_SIDE, THUMB_SIDE)
    } else {
        img
    };
    let mut png = Vec::new();
    small
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(Thumb { width, height, png })
}

/// A extensão do arquivo para o tipo.
pub fn extension_for(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        "image/avif" => "avif",
        "image/tiff" => "tiff",
        _ => "img",
    }
}

/// O tipo pela assinatura dos bytes.
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    match image::guess_format(bytes).ok()? {
        ImageFormat::Png => Some("image/png"),
        ImageFormat::Jpeg => Some("image/jpeg"),
        ImageFormat::WebP => Some("image/webp"),
        ImageFormat::Gif => Some("image/gif"),
        ImageFormat::Bmp => Some("image/bmp"),
        ImageFormat::Avif => Some("image/avif"),
        ImageFormat::Tiff => Some("image/tiff"),
        _ => None,
    }
}
