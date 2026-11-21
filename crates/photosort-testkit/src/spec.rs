//! Plain data describing the fields to write into a synthetic file.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Little,
    Big,
}

/// Standard EXIF fields to write. `None` omits the tag.
#[derive(Debug, Clone, PartialEq)]
pub struct ExifSpec {
    pub make: Option<String>,
    pub model: Option<String>,
    pub orientation: Option<u16>,
    pub body_serial: Option<String>,          // ExifIFD 0xA431
    pub date_time_original: Option<String>,   // 0x9003
    pub subsec_time_original: Option<String>, // 0x9291
    pub iso: Option<u16>,                     // 0x8827 SHORT
    pub exposure_time: Option<(u32, u32)>,    // 0x829A
    pub f_number: Option<(u32, u32)>,         // 0x829D
    pub focal_length: Option<(u32, u32)>,     // 0x920A
    pub max_aperture: Option<(u32, u32)>,     // 0x9205
    pub subject_distance: Option<(u32, u32)>, // 0x9206
    pub lens_info: Option<[(u32, u32); 4]>,   // 0xA432
    pub lens_make: Option<String>,            // 0xA433
    pub lens_model: Option<String>,           // 0xA434
}

/// Nikon "type 3" maker note content. `None` omits the tag.
#[derive(Debug, Clone, PartialEq)]
pub struct NikonSpec {
    pub serial: Option<String>,        // 0x001D ASCII
    pub mechanical_count: Option<u32>, // 0x0037 LONG
    pub shutter_count: Option<u32>,    // 0x00A7 LONG
    pub preview_jpeg: Option<Vec<u8>>, // via PreviewIFD 0x0011
    /// true: preview bytes stored inside the MakerNote value (its count covers them);
    /// false: preview stored after the TIFF content. In a NEF the outside preview
    /// is a free blob. In a JPEG or TIFF there is nowhere to put it, so the
    /// PreviewIFD is omitted. Real Z f layout is unverified, so tests exercise both.
    pub preview_inside: bool,
}

impl ExifSpec {
    /// Z f-like values with a fake body serial.
    #[must_use]
    pub fn zf() -> Self {
        Self {
            make: Some("NIKON CORPORATION".to_owned()),
            model: Some("NIKON Z f".to_owned()),
            orientation: Some(1),
            body_serial: Some("1234567".to_owned()),
            date_time_original: Some("2026:11:20 14:59:32".to_owned()),
            subsec_time_original: Some("92".to_owned()),
            iso: Some(800),
            exposure_time: Some((1, 250)),
            f_number: Some((28, 10)),
            focal_length: Some((50, 1)),
            max_aperture: Some((297, 100)),
            subject_distance: None,
            lens_info: Some([(50, 1), (50, 1), (18, 10), (18, 10)]),
            lens_make: Some("NIKON".to_owned()),
            lens_model: Some("NIKKOR Z 50mm f/1.8 S".to_owned()),
        }
    }
}

impl NikonSpec {
    /// Z f-like maker note with a fake serial and a shutter count that
    /// differs from the mechanical count.
    #[must_use]
    pub fn zf(preview: Option<Vec<u8>>) -> Self {
        Self {
            serial: Some("1234567".to_owned()),
            mechanical_count: Some(10438),
            shutter_count: Some(75036),
            preview_jpeg: preview,
            preview_inside: false,
        }
    }
}
