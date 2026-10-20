//! Small derivatives of a file's bytes, published beside it.
//!
//! A derivative is a picture made from a file's own bytes and published as
//! its own message: a thumbnail for the gallery, a preview for the viewer.
//! It is what lets a grid show a drive full of pictures without fetching the
//! drive — a derivative is a few tens of kilobytes, a picture is megabytes.
//!
//! The original makes them on its server and serves them over HTTP. This port
//! has no server, so it makes them here and publishes them to the drive, which
//! is the same thing from the reader's point of view: the picture is already
//! there, and the client fetches a small message instead of the file.
//!
//! The wire format is the original's, so a derivative this port publishes is
//! one the original reads, and one the original published is one this port
//! reads. See [`crate::ops::Rendition`] for the descriptor and
//! [`crate::crypto`] for the container the encrypted form uses.

use std::io::Cursor;

use crate::crypto;
use crate::ops::Rendition;

/// The magic in front of an encrypted derivative's envelope.
const ENVELOPE_MAGIC: &[u8; 4] = b"TDR1";

/// The longest side of a thumbnail, in pixels.
///
/// The original's own number: 512 keeps a grid crisp while staying small over
/// the wire — a 512px JPEG is twenty to forty kilobytes.
pub const THUMBNAIL_EDGE: u32 = 512;

/// The quality a derivative is encoded with, the original's own.
const JPEG_QUALITY: u8 = 82;

/// The largest source that is decoded: 24 megapixels, which covers a phone
/// photo and a DSLR frame, and refuses a decompression bomb.
const MAX_SOURCE_PIXELS: u64 = 24_000_000;

/// A picture made from a file's bytes.
pub struct Picture {
    /// The picture, as a JPEG.
    pub jpeg: Vec<u8>,
    /// Its width, in pixels.
    pub width: u32,
    /// Its height, in pixels.
    pub height: u32,
}

/// Make a picture from a file's bytes, fitted into a square of `max_edge`.
///
/// The picture is flattened onto white, as the original's is, so a transparent
/// PNG does not come out with a black background.
pub fn generate(source: &[u8], max_edge: u32) -> Option<Picture> {
    // The size is checked before the full decode, so a picture that declares
    // an enormous size is refused rather than allocated.
    let config = image::ImageReader::new(Cursor::new(source))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    if config.0 == 0 || config.1 == 0 {
        return None;
    }
    if u64::from(config.0) * u64::from(config.1) > MAX_SOURCE_PIXELS {
        return None;
    }
    let reader = image::ImageReader::new(Cursor::new(source))
        .with_guessed_format()
        .ok()?;
    let picture = reader.decode().ok()?;
    let flat = flatten_onto_white(&picture);
    // A picture smaller than the edge is left alone: enlarging it would cost
    // bytes and tell nobody anything. The original fits a picture the same way.
    let (width, height) = fit_within(flat.width(), flat.height(), max_edge);
    let small = if (width, height) == (flat.width(), flat.height()) {
        flat.clone()
    } else {
        image::imageops::resize(
            &flat,
            width,
            height,
            image::imageops::FilterType::CatmullRom,
        )
    };
    let (width, height) = (small.width(), small.height());
    let mut jpeg = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY);
    encoder.encode_image(&flat).ok()?;
    Some(Picture {
        jpeg,
        width,
        height,
    })
}

/// The largest picture of `width` by `height` that fits in a square of
/// `max_edge`, keeping its shape.
///
/// A picture already inside the square is left as it is: the original does the
/// same, so a small picture is not blown up into a large one.
fn fit_within(width: u32, height: u32, max_edge: u32) -> (u32, u32) {
    if width <= max_edge && height <= max_edge {
        return (width, height);
    }
    if width >= height {
        let scaled = (f64::from(height) * f64::from(max_edge) / f64::from(width)) as u32;
        (max_edge, scaled.max(1))
    } else {
        let scaled = (f64::from(width) * f64::from(max_edge) / f64::from(height)) as u32;
        (scaled.max(1), max_edge)
    }
}

/// A picture with its transparency painted onto white.
fn flatten_onto_white(picture: &image::DynamicImage) -> image::RgbImage {
    let rgba = picture.to_rgba8();
    let (width, height) = rgba.dimensions();
    let mut out = image::RgbImage::new(width, height);
    for (x, y, pixel) in rgba.enumerate_pixels() {
        let [r, g, b, a] = pixel.0;
        // The alpha is the picture's own, so a transparent pixel becomes white
        // rather than the colour of whatever was behind it.
        let blend = |channel: u8| {
            let value = channel as u32 * a as u32;
            ((value + 255 * (255 - a as u32)) / 255) as u8
        };
        out.put_pixel(x, y, image::Rgb([blend(r), blend(g), blend(b)]));
    }
    out
}

/// The envelope a derivative carries: the descriptor, then the picture.
///
/// The descriptor is the same one the wire carries, with its size left out:
/// the size is not known before the envelope is encrypted, and the original
/// leaves it out for the same reason. Every other property of the derivative
/// is authenticated with the picture, so a derivative cannot be moved to
/// another file or another drive.
fn envelope(descriptor: &Rendition, jpeg: &[u8]) -> Vec<u8> {
    let mut binding = descriptor.clone();
    binding.size = 0;
    let described = binding.to_json().into_bytes();
    let mut out = Vec::with_capacity(8 + described.len() + jpeg.len());
    out.extend_from_slice(ENVELOPE_MAGIC);
    out.extend_from_slice(&(described.len() as u32).to_be_bytes());
    out.extend_from_slice(&described);
    out.extend_from_slice(jpeg);
    out
}

/// Encrypt a derivative for a drive whose files are encrypted.
///
/// The envelope is sealed with the drive's own stream, so the picture never
/// reaches the disk in the clear and the descriptor travels with it.
pub fn encrypt(jpeg: &[u8], master: &[u8; 32], descriptor: &Rendition) -> Result<Vec<u8>, String> {
    let plain = envelope(descriptor, jpeg);
    let mut out = Vec::new();
    let mut source = plain.as_slice();
    crypto::encrypt_stream(&mut source, &mut out, master).map_err(|e| e.to_string())?;
    Ok(out)
}

/// Read a derivative back, and check that it is the one the descriptor names.
pub fn decrypt(raw: &[u8], master: &[u8; 32], descriptor: &Rendition) -> Result<Vec<u8>, String> {
    if raw.len() as i64 != descriptor.size {
        return Err("the derivative is not the size it says".to_string());
    }
    let mut plain = Vec::new();
    let mut source = raw;
    crypto::decrypt_stream(&mut source, &mut plain, master).map_err(|e| e.to_string())?;
    if plain.len() < 8 || &plain[..4] != ENVELOPE_MAGIC {
        return Err("the derivative is not a derivative".to_string());
    }
    let described = u32::from_be_bytes([plain[4], plain[5], plain[6], plain[7]]) as usize;
    let start = 8 + described;
    if plain.len() < start {
        return Err("the derivative is cut short".to_string());
    }
    // The descriptor inside the envelope has to be the one the wire named,
    // bar its size: that is what binds the picture to its file.
    let mut binding = descriptor.clone();
    binding.size = 0;
    let expected = binding.to_json();
    let actual = String::from_utf8_lossy(&plain[8..start]).to_string();
    if actual != expected {
        return Err("the derivative belongs to another file".to_string());
    }
    Ok(plain[start..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> Rendition {
        Rendition {
            channel_id: 42,
            file_msg_id: 7,
            content_msg_id: 7,
            upload_uuid: String::new(),
            kind: crate::ops::RENDITION_THUMBNAIL.to_string(),
            version: 1,
            size: 0,
            plaintext_size: 10,
            width: 100,
            height: 80,
            encrypted: true,
        }
    }

    /// A picture is made from the bytes, fitted into the square, and flattened
    /// onto white.
    #[test]
    fn a_picture_is_fitted_and_flattened() {
        let source = image::RgbImage::from_fn(1000, 600, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 200])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(source)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .expect("the source to be written");

        let picture = generate(&bytes, THUMBNAIL_EDGE).expect("a picture");
        assert_eq!(picture.width, THUMBNAIL_EDGE);
        assert!(picture.height < picture.width);
        // The bytes are a JPEG, which is what the original publishes.
        assert_eq!(&picture.jpeg[..2], &[0xff, 0xd8]);
    }

    /// A picture smaller than the edge is left alone, the way the original
    /// leaves it: enlarging it would cost bytes and tell nobody anything.
    #[test]
    fn a_small_picture_is_not_enlarged() {
        let source = image::RgbImage::from_fn(64, 64, |x, y| {
            image::Rgb([(x * 4) as u8, (y * 4) as u8, 128])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(source)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .expect("the source to be written");

        let picture = generate(&bytes, THUMBNAIL_EDGE).expect("a picture");
        assert_eq!(picture.width, 64);
        assert_eq!(picture.height, 64);
    }

    /// A transparent picture comes out on white, not on black.
    #[test]
    fn a_transparent_picture_is_flattened_onto_white() {
        let source = image::RgbaImage::from_fn(10, 10, |_, _| image::Rgba([0, 0, 0, 0]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(source)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .expect("the source to be written");

        let picture = generate(&bytes, THUMBNAIL_EDGE).expect("a picture");
        let made = image::load_from_memory(&picture.jpeg).expect("a picture");
        let pixel = made.to_rgb8().get_pixel(0, 0).0;
        assert!(pixel[0] > 240 && pixel[1] > 240 && pixel[2] > 240);
    }

    /// A derivative is sealed with the drive's key, and comes back only with
    /// it.
    #[test]
    fn a_derivative_is_sealed_and_opened_again() {
        let key = [7u8; 32];
        let mut descriptor = descriptor();
        let jpeg = b"a picture".to_vec();
        let sealed = encrypt(&jpeg, &key, &descriptor).expect("the derivative to be sealed");
        descriptor.size = sealed.len() as i64;

        let opened = decrypt(&sealed, &key, &descriptor).expect("the derivative to be opened");
        assert_eq!(opened, jpeg);

        // The wrong key opens nothing.
        assert!(decrypt(&sealed, &[8u8; 32], &descriptor).is_err());
    }

    /// A derivative that belongs to another file is refused, even with the
    /// right key: that is what binds a picture to the file it was made from.
    #[test]
    fn a_derivative_of_another_file_is_refused() {
        let key = [7u8; 32];
        let mut descriptor = descriptor();
        let sealed = encrypt(b"a picture", &key, &descriptor).expect("sealed");
        descriptor.size = sealed.len() as i64;

        let mut other = descriptor.clone();
        other.file_msg_id = 8;
        assert!(decrypt(&sealed, &key, &other).is_err());
    }
}
