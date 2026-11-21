//! Data contracts that cross phase boundaries: file kinds, parsed
//! metadata, byte ranges and identity keys.
// pattern: Functional Core

/// A file format, identified by a registered reader. Data, not a closed
/// vendor enum: new raw formats add a reader, not a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileKind(pub &'static str);

/// An unsigned EXIF RATIONAL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rational {
    pub num: u32,
    pub den: u32,
}

impl Rational {
    /// Lowest terms, so that `10/2500` and `1/250` compare equal.
    /// A zero denominator is returned unchanged.
    #[must_use]
    pub fn reduced(self) -> Rational {
        let g = gcd(self.num, self.den);
        if g <= 1 {
            return self;
        }
        Rational {
            num: self.num / g,
            den: self.den / g,
        }
    }

    /// `None` when the denominator is zero.
    #[must_use]
    pub fn to_f64(self) -> Option<f64> {
        (self.den != 0).then(|| f64::from(self.num) / f64::from(self.den))
    }
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// A byte range in a file, guaranteed non-empty and inside the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ByteRange {
    pub offset: u64,
    pub len: u64,
}

impl ByteRange {
    /// `None` if the range is empty or extends past `file_len`.
    #[must_use]
    pub fn within(offset: u64, len: u64, file_len: u64) -> Option<ByteRange> {
        let end = offset.checked_add(len)?;
        (len > 0 && end <= file_len).then_some(ByteRange { offset, len })
    }

    #[must_use]
    pub fn end(&self) -> u64 {
        // Cannot overflow: `within` checked it.
        self.offset + self.len
    }
}

/// Everything photosort reads from a file header.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metadata {
    pub make: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
    /// EXIF "YYYY:MM:DD HH:MM:SS".
    pub date_time_original: Option<String>,
    pub subsec_time_original: Option<String>,
    pub iso: Option<u32>,
    pub exposure_time: Option<Rational>,
    pub f_number: Option<Rational>,
    pub focal_length: Option<Rational>,
    pub lens_make: Option<String>,
    pub lens_model: Option<String>,
    /// EXIF Orientation (1-8); previews are rotated by it in Phase 3.
    pub orientation: Option<u16>,
    /// From the vendor `MakerNoteReader` (Nikon: ShutterCount 0x00A7).
    pub shot_counter: Option<u64>,
    /// Embedded ~640 px JPEG.
    pub preview: Option<ByteRange>,
    /// Full-size embedded JPEG in a raw file (NEF JpgFromRaw).
    pub full_jpeg: Option<ByteRange>,
}

/// Identity of a photo, independent of file name and lens edits.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IdentityKey {
    Counter {
        make: String,
        serial: String,
        counter: u64,
        dto: String,
        subsec: String,
    },
    Capture {
        make: String,
        serial: String,
        dto: String,
        subsec: String,
        iso: u32,
        exposure: Rational,
    },
    Content {
        size: u64,
        head_blake3: [u8; 32],
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// The structure continues past the bytes supplied. `needed` is the
    /// smallest head length (from file start) that would let parsing
    /// progress; it never exceeds the file length.
    #[error("need the first {needed} bytes of the file, have {have}")]
    NeedMore { needed: u64, have: u64 },
    #[error("not a {expected} file")]
    WrongFormat { expected: &'static str },
    #[error("malformed {what}")]
    Malformed { what: &'static str },
}
