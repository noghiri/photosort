//! Reads embedded preview bytes from disk and decodes them.
// pattern: Imperative Shell

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use eframe::egui;
use image::ImageFormat;
use image::metadata::Orientation;
use photosort_core::model::ByteRange;

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

/// Decodes a JPEG preview and applies its EXIF orientation. Decoding does
/// not apply orientation by itself.
pub(crate) fn decode_preview(
    bytes: &[u8],
    orientation: Option<u16>,
) -> Result<DecodedImage, PreviewError> {
    let mut image = image::load_from_memory_with_format(bytes, ImageFormat::Jpeg)?;
    // Orientations outside 1..=8 map to `None` and leave the image unchanged.
    if let Some(orientation) = orientation
        .and_then(|o| u8::try_from(o).ok())
        .and_then(Orientation::from_exif)
    {
        image.apply_orientation(orientation);
    }
    let rgba = image.to_rgba8();
    Ok(DecodedImage {
        size: [rgba.width() as usize, rgba.height() as usize],
        rgba: rgba.into_raw(),
    })
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
        let image = decode_preview(&tiny_jpeg(640, 424), None).expect("decodes");
        assert_eq!(image.size, [640, 424]);
        assert_eq!(image.rgba.len(), 640 * 424 * 4);
    }

    #[test]
    fn orientation_6_rotates_the_preview_to_portrait() {
        let image = decode_preview(&tiny_jpeg(640, 424), Some(6)).expect("decodes");
        assert_eq!(image.size, [424, 640]);
    }

    #[test]
    fn orientation_1_and_out_of_range_values_leave_the_size_alone() {
        let bytes = tiny_jpeg(640, 424);
        for orientation in [Some(1), Some(9), Some(0x100), None] {
            let image = decode_preview(&bytes, orientation).expect("decodes");
            assert_eq!(image.size, [640, 424], "orientation {orientation:?}");
        }
    }

    #[test]
    fn corrupt_bytes_are_an_error_not_a_panic() {
        assert!(decode_preview(b"\xFF\xD8garbage", None).is_err());
        assert!(decode_preview(&[], None).is_err());
    }

    #[test]
    fn to_color_image_keeps_the_decoded_size() {
        let image = decode_preview(&tiny_jpeg(640, 424), None).expect("decodes");
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
