//! Fixtures shared by the metadata reader tests: the Z f values the testkit
//! writes, and helpers that slice and decode located ranges.
// pattern: Functional Core

use crate::model::{ByteRange, Metadata, Rational};

/// The `Metadata` that `ExifSpec::zf()` writes, without maker-note fields.
pub fn expected_zf() -> Metadata {
    Metadata {
        make: Some("NIKON CORPORATION".to_owned()),
        model: Some("NIKON Z f".to_owned()),
        serial: Some("1234567".to_owned()),
        date_time_original: Some("2026:11:20 14:59:32".to_owned()),
        subsec_time_original: Some("92".to_owned()),
        iso: Some(800),
        exposure_time: Some(Rational { num: 1, den: 250 }),
        f_number: Some(Rational { num: 28, den: 10 }),
        focal_length: Some(Rational { num: 50, den: 1 }),
        lens_make: Some("NIKON".to_owned()),
        lens_model: Some("NIKKOR Z 50mm f/1.8 S".to_owned()),
        orientation: Some(1),
        ..Metadata::default()
    }
}

/// The bytes a located range covers.
pub fn slice(file: &[u8], range: ByteRange) -> &[u8] {
    let start = usize::try_from(range.offset).unwrap();
    let end = usize::try_from(range.end()).unwrap();
    file.get(start..end).expect("range lies inside the file")
}

/// Width and height of a JPEG, decoded.
pub fn decoded_size(bytes: &[u8]) -> (u32, u32) {
    let img = image::load_from_memory(bytes).expect("preview decodes");
    (img.width(), img.height())
}
