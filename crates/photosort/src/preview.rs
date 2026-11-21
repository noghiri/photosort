//! Reads embedded preview bytes from disk and decodes them.
// pattern: Imperative Shell

use std::fs::File;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use eframe::egui;
use image::metadata::Orientation;
use image::{ImageFormat, ImageReader, Limits};
use photosort_core::model::ByteRange;

/// Longest side of a tile's texture: 2 × `TILE_MAX` (640), so a full-size tile
/// on a 2× display still has its own pixels. The Z f previews (640 × 424) are
/// below this, so they are not resized.
pub(crate) const TILE_TEXTURE_MAX: u32 = 1280;

/// Largest width or height a preview may declare. Generous: a 45-megapixel
/// frame is 8,256 px a side, well under this. The image crate's default sets
/// no dimension limit, so this is the first check; the byte limit below is the
/// one that bounds memory.
const DECODE_MAX_SIDE: u32 = 16_384;
/// Most memory the decoder may reserve for its output buffer. An 8,256 × 5,504
/// RGB image needs about 136 MB, so 256 MiB covers a 45-megapixel full-size
/// preview, and caps what a hostile header can reserve. The image crate makes
/// this limit non-strict, so it bounds the output buffer, not every allocation.
const DECODE_MAX_ALLOC: u64 = 256 * 1024 * 1024;

/// A decoded RGBA image. Our own type, so scan code and tests do not need egui.
#[derive(Debug, PartialEq)]
pub(crate) struct DecodedImage {
    pub size: [usize; 2],
    pub rgba: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PreviewError {
    #[error("preview does not decode: {0}")]
    Decode(#[from] image::ImageError),
}

/// Reads exactly `range` from `path`. A range past the end of the file is
/// an error.
pub(crate) fn read_range(path: &Path, range: ByteRange) -> io::Result<Vec<u8>> {
    let len = usize::try_from(range.len)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "range is too long"))?;
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(range.offset))?;
    let mut buf = vec![0; len];
    file.read_exact(&mut buf)?;
    Ok(buf)
}

/// Decodes a JPEG preview, applies its EXIF orientation, then fits it inside
/// `max_side` when either side is longer. Decoding does not apply orientation
/// by itself. `None` keeps the full size, which the overlay needs; the tile
/// path passes `Some(TILE_TEXTURE_MAX)`. The decoder's own limits always apply.
pub(crate) fn decode_preview(
    bytes: &[u8],
    orientation: Option<u16>,
    max_side: Option<u32>,
) -> Result<DecodedImage, PreviewError> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), ImageFormat::Jpeg);
    reader.limits(decode_limits());
    let mut image = reader.decode()?;
    // Orientations outside 1..=8 map to `None` and leave the image unchanged.
    if let Some(orientation) = orientation
        .and_then(|o| u8::try_from(o).ok())
        .and_then(Orientation::from_exif)
    {
        image.apply_orientation(orientation);
    }
    // `thumbnail` keeps the aspect ratio and fits inside the square `max`.
    let bound = max_side.filter(|max| image.width() > *max || image.height() > *max);
    if let Some(max) = bound {
        image = image.thumbnail(max, max);
    }
    let rgba = image.to_rgba8();
    Ok(DecodedImage {
        size: [rgba.width() as usize, rgba.height() as usize],
        rgba: rgba.into_raw(),
    })
}

/// The decoder limits for an embedded preview. `Limits` is non-exhaustive, so
/// it starts from `no_limits` and sets each field.
fn decode_limits() -> Limits {
    let mut limits = Limits::no_limits();
    limits.max_image_width = Some(DECODE_MAX_SIDE);
    limits.max_image_height = Some(DECODE_MAX_SIDE);
    limits.max_alloc = Some(DECODE_MAX_ALLOC);
    limits
}

/// Converts for upload to an egui texture.
pub(crate) fn to_color_image(image: &DecodedImage) -> egui::ColorImage {
    egui::ColorImage::from_rgba_unmultiplied(image.size, &image.rgba)
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use photosort_testkit::tiny_jpeg;

    use super::*;

    #[test]
    fn a_640_by_424_jpeg_decodes_to_640_by_424() {
        let image = decode_preview(&tiny_jpeg(640, 424), None, None).expect("decodes");
        assert_eq!(image.size, [640, 424]);
        assert_eq!(image.rgba.len(), 640 * 424 * 4);
    }

    #[test]
    fn orientation_6_rotates_the_preview_to_portrait() {
        let image = decode_preview(&tiny_jpeg(640, 424), Some(6), None).expect("decodes");
        assert_eq!(image.size, [424, 640]);
    }

    #[test]
    fn orientation_1_and_out_of_range_values_leave_the_size_alone() {
        let bytes = tiny_jpeg(640, 424);
        for orientation in [Some(1), Some(9), Some(0x100), None] {
            let image = decode_preview(&bytes, orientation, None).expect("decodes");
            assert_eq!(image.size, [640, 424], "orientation {orientation:?}");
        }
    }

    #[test]
    fn corrupt_bytes_are_an_error_not_a_panic() {
        assert!(decode_preview(b"\xFF\xD8garbage", None, None).is_err());
        assert!(decode_preview(&[], None, None).is_err());
    }

    /// A 2000 × 1500 preview, which is larger than any tile texture, is fitted
    /// to the tile bound with its aspect ratio kept.
    #[test]
    fn an_oversized_preview_is_fitted_to_the_tile_texture_bound() {
        let image =
            decode_preview(&tiny_jpeg(2000, 1500), None, Some(TILE_TEXTURE_MAX)).expect("decodes");
        assert_eq!(image.size, [1280, 960]);
        assert_eq!(image.rgba.len(), 1280 * 960 * 4);
    }

    /// A portrait preview is bounded by its long side, which is the height.
    #[test]
    fn the_bound_applies_to_the_long_side_of_a_portrait_preview() {
        let image =
            decode_preview(&tiny_jpeg(1500, 2000), None, Some(TILE_TEXTURE_MAX)).expect("decodes");
        assert_eq!(image.size, [960, 1280]);
    }

    /// Orientation 6 turns a 2000 × 1500 preview into 1500 × 2000 before the
    /// bound applies, so the result is portrait: 960 × 1280.
    #[test]
    fn orientation_and_the_bound_together_give_a_portrait_fit() {
        let image = decode_preview(&tiny_jpeg(2000, 1500), Some(6), Some(TILE_TEXTURE_MAX))
            .expect("decodes");
        assert_eq!(image.size, [960, 1280]);
    }

    /// The Z f preview (640 × 424) is within the bound, so it is not resized.
    #[test]
    fn a_preview_within_the_bound_keeps_its_size() {
        let image =
            decode_preview(&tiny_jpeg(640, 424), None, Some(TILE_TEXTURE_MAX)).expect("decodes");
        assert_eq!(image.size, [640, 424]);
    }

    /// Without a bound the full size is kept. The overlay decodes this way.
    #[test]
    fn without_a_bound_a_large_preview_keeps_its_full_size() {
        let image = decode_preview(&tiny_jpeg(2000, 1500), None, None).expect("decodes");
        assert_eq!(image.size, [2000, 1500]);
    }

    /// A side past the decoder's dimension limit is refused before decoding.
    /// The image is only 8 px high, so the test stays cheap.
    #[test]
    fn a_preview_past_the_decoder_dimension_limit_is_an_error() {
        let bytes = tiny_jpeg(DECODE_MAX_SIDE + 1, 8);
        assert!(decode_preview(&bytes, None, None).is_err());
    }

    #[test]
    fn to_color_image_keeps_the_decoded_size() {
        let image = decode_preview(&tiny_jpeg(640, 424), None, None).expect("decodes");
        let color = to_color_image(&image);
        assert_eq!(color.size, [640, 424]);
    }

    #[test]
    fn read_range_returns_exactly_the_requested_bytes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("card.bin");
        let content: Vec<u8> = (0..=255).collect();
        std::fs::write(&path, &content).expect("write");
        let bytes = read_range(&path, ByteRange { offset: 10, len: 5 }).expect("in range");
        assert_eq!(bytes, vec![10, 11, 12, 13, 14]);
    }

    #[test]
    fn read_range_past_the_end_is_an_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("card.bin");
        std::fs::write(&path, [0u8; 256]).expect("write");
        let err = read_range(
            &path,
            ByteRange {
                offset: 250,
                len: 10,
            },
        )
        .expect_err("range ends past the file");
        assert_eq!(err.kind(), ErrorKind::UnexpectedEof);
    }
}
