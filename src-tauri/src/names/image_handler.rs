// Copied from bee-san/Japanese_Character_Name_Dictionary (characterdictionary.tokyo),
// yomitan-dict-builder/src/image_handler.rs at 2e3949d. MIT licence, Copyright (c) 2026 Autumn (Bee).
// Telly's changes are marked "Telly:".

use image::imageops::FilterType;
use image::ImageFormat;
use std::io::Cursor;

/// Maximum dimensions for character portrait thumbnails (2× for retina).
const MAX_WIDTH: u32 = 160;
const MAX_HEIGHT: u32 = 200;

pub struct ImageHandler;

impl ImageHandler {
    /// Detect file extension from raw image bytes by checking magic bytes.
    pub fn detect_extension(bytes: &[u8]) -> &'static str {
        if bytes.len() >= 4 {
            // JPEG: FF D8 FF
            if bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
                return "jpg";
            }
            // PNG: 89 50 4E 47
            if bytes[0] == 0x89 && bytes[1] == 0x50 && bytes[2] == 0x4E && bytes[3] == 0x47 {
                return "png";
            }
            // GIF: 47 49 46
            if bytes[0] == 0x47 && bytes[1] == 0x49 && bytes[2] == 0x46 {
                return "gif";
            }
            // WebP: RIFF....WEBP
            if bytes[0] == 0x52
                && bytes[1] == 0x49
                && bytes[2] == 0x46
                && bytes[3] == 0x46
                && bytes.len() >= 12
                && bytes[8] == 0x57
                && bytes[9] == 0x45
                && bytes[10] == 0x42
                && bytes[11] == 0x50
            {
                return "webp";
            }
        }
        "jpg" // fallback
    }

    /// Resize raw image bytes to fit within MAX_WIDTH × MAX_HEIGHT, output as JPEG.
    /// Returns (resized_bytes, ext, width, height) on success,
    /// or the original (bytes, detected_ext, 0, 0) on failure.
    pub fn resize_image(bytes: &[u8]) -> (Vec<u8>, &'static str, u32, u32) {
        // Try to decode the image
        let img = match image::load_from_memory(bytes) {
            Ok(img) => img,
            Err(_) => {
                // Can't decode — return original bytes with detected extension
                return (bytes.to_vec(), Self::detect_extension(bytes), 0, 0);
            }
        };

        let (w, h) = (img.width(), img.height());

        // Only resize if larger than our max dimensions
        let resized = if w > MAX_WIDTH || h > MAX_HEIGHT {
            img.resize(MAX_WIDTH, MAX_HEIGHT, FilterType::Lanczos3)
        } else {
            img
        };

        let (rw, rh) = (resized.width(), resized.height());

        // Encode as JPEG (widely supported by Yomitan and all browsers)
        let mut buf = Cursor::new(Vec::new());
        match resized.write_to(&mut buf, ImageFormat::Jpeg) {
            Ok(_) => (buf.into_inner(), "jpg", rw, rh),
            Err(_) => (bytes.to_vec(), Self::detect_extension(bytes), w, h),
        }
    }

    /// Build the filename for a character image in the ZIP.
    pub fn make_filename(char_id: &str, ext: &str) -> String {
        format!("c{}.{}", char_id, ext)
    }
}

