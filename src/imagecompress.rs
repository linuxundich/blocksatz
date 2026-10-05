//! Re-encodes a local PNG or JPEG as WebP before it's *uploaded* to
//! WordPress (the blog's images are WebP throughout), shrinking anything
//! oversized on the way. Only ever changes the bytes/filename/mime type
//! *sent*; the local file and the article's own `![]()` reference are never
//! touched. Transparency survives - WebP has an alpha channel.
//!
//! Decoded with `gdk-pixbuf`, encoded with the `webp` crate the image
//! editor (`imageedit.rs`) already uses.

use gdk_pixbuf::prelude::*;

/// No side is ever scaled *up* - this only ever shrinks an oversized image
/// down to at most this many pixels on its longer edge.
const MAX_DIMENSION: i32 = 2000;

pub struct CompressedImage {
    pub bytes: Vec<u8>,
    pub filename: String,
    pub mime_type: &'static str,
}

/// `bytes`/`filename` unchanged, for every case where conversion doesn't
/// apply or failed - the single fallback path so every early return below
/// looks the same.
fn unchanged(bytes: &[u8], filename: &str) -> CompressedImage {
    CompressedImage {
        bytes: bytes.to_vec(),
        filename: filename.to_string(),
        mime_type: crate::export::mime_from_extension(filename),
    }
}

pub fn maybe_compress(bytes: &[u8], filename: &str) -> CompressedImage {
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    if !matches!(ext.as_str(), "png" | "jpg" | "jpeg") {
        return unchanged(bytes, filename);
    }
    let Some(pixbuf) = decode(bytes) else {
        return unchanged(bytes, filename);
    };
    let (width, height) = (pixbuf.width(), pixbuf.height());
    if width <= 0 || height <= 0 {
        return unchanged(bytes, filename);
    }
    let (target_w, target_h) = scaled_dimensions(width, height, MAX_DIMENSION);
    let scaled = if (target_w, target_h) != (width, height) {
        let Some(scaled) = pixbuf.scale_simple(target_w, target_h, gdk_pixbuf::InterpType::Bilinear) else {
            return unchanged(bytes, filename);
        };
        scaled
    } else {
        pixbuf
    };
    CompressedImage {
        bytes: crate::imageedit::encode_webp(&scaled),
        filename: replace_extension(filename, "webp"),
        mime_type: "image/webp",
    }
}

fn decode(bytes: &[u8]) -> Option<gdk_pixbuf::Pixbuf> {
    let loader = gdk_pixbuf::PixbufLoader::new();
    loader.write(bytes).ok()?;
    loader.close().ok()?;
    loader.pixbuf()
}

/// Scales `(width, height)` down to fit within `max_dimension` on the
/// longer edge, preserving aspect ratio - unchanged if it already fits.
/// Pure and gdk-pixbuf-free, so this - the actual sizing decision - is
/// unit-testable without a real image.
fn scaled_dimensions(width: i32, height: i32, max_dimension: i32) -> (i32, i32) {
    let longest = width.max(height);
    if longest <= max_dimension {
        return (width, height);
    }
    let scale = f64::from(max_dimension) / f64::from(longest);
    let scaled_w = ((f64::from(width) * scale).round() as i32).max(1);
    let scaled_h = ((f64::from(height) * scale).round() as i32).max(1);
    (scaled_w, scaled_h)
}

fn replace_extension(filename: &str, new_ext: &str) -> String {
    match filename.rsplit_once('.') {
        Some((stem, _old_ext)) => format!("{stem}.{new_ext}"),
        None => format!("{filename}.{new_ext}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(alpha: u8) -> Vec<u8> {
        let pixbuf = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, true, 8, 64, 48).unwrap();
        pixbuf.fill(0x3366_9900 | u32::from(alpha));
        pixbuf.save_to_bufferv("png", &[]).unwrap()
    }

    #[test]
    fn a_png_is_uploaded_as_webp() {
        let out = maybe_compress(&png(255), "shot.png");
        assert_eq!(out.mime_type, "image/webp");
        assert_eq!(out.filename, "shot.webp");
        assert_eq!(&out.bytes[8..12], b"WEBP");
    }

    #[test]
    fn a_transparent_png_keeps_its_alpha_as_webp() {
        let out = maybe_compress(&png(128), "logo.png");
        let decoded = decode(&out.bytes).map(|p| p.has_alpha());
        // Only checkable where gdk-pixbuf has a WebP loader.
        assert!(decoded.is_none() || decoded == Some(true));
        assert_eq!(out.mime_type, "image/webp");
    }

    #[test]
    fn a_gif_is_left_alone() {
        let out = maybe_compress(b"GIF89a", "anim.gif");
        assert_eq!(out.filename, "anim.gif");
    }

    #[test]
    fn scaled_dimensions_leaves_an_already_small_image_unchanged() {
        assert_eq!(scaled_dimensions(800, 600, 2000), (800, 600));
    }

    #[test]
    fn scaled_dimensions_shrinks_a_wide_image_preserving_aspect_ratio() {
        assert_eq!(scaled_dimensions(4000, 2000, 2000), (2000, 1000));
    }

    #[test]
    fn scaled_dimensions_shrinks_a_tall_image_preserving_aspect_ratio() {
        assert_eq!(scaled_dimensions(2000, 4000, 2000), (1000, 2000));
    }

    #[test]
    fn replace_extension_swaps_a_known_extension() {
        assert_eq!(replace_extension("photo.png", "jpg"), "photo.jpg");
    }

    #[test]
    fn replace_extension_appends_when_there_is_no_extension() {
        assert_eq!(replace_extension("photo", "jpg"), "photo.jpg");
    }
}
