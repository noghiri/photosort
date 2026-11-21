//! JPEG reader: EXIF from APP1, the MPF preview from APP2.
// pattern: Functional Core

use crate::metadata::exif::read_tiff_exif;
use crate::metadata::registry::{FormatReader, ParseInput};
use crate::metadata::tiff::{
    ByteOrder, OVERFLOW, TIFF_MAGIC, Tiff, bytes_within, optional, optional_tag,
};
use crate::model::{ByteRange, FileKind, Metadata, ParseError};

pub const KIND: FileKind = FileKind("jpeg");

/// Marker segments walked before giving up. Real files need only a handful
/// to reach SOS; the cap bounds work on hostile input.
const MAX_SEGMENTS: usize = 512;
const MAX_MPF_ENTRIES: usize = 64;
const MPF_ENTRY_LEN: usize = 16;
const APP1: u8 = 0xE1;
const APP2: u8 = 0xE2;
const SOS: u8 = 0xDA;
const EOI: u8 = 0xD9;
const TAG_MP_ENTRY: u16 = 0xB002;

pub struct JpegReader;

impl FormatReader for JpegReader {
    fn kind(&self) -> FileKind {
        KIND
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["jpg", "jpeg"]
    }

    fn badge(&self) -> &'static str {
        "J"
    }

    fn is_raw(&self) -> bool {
        false
    }

    fn header_len_hint(&self) -> usize {
        128 * 1024
    }

    fn parse(&self, input: &ParseInput<'_>) -> Result<Metadata, ParseError> {
        parse_jpeg(input)
    }

    fn lens_write_supported(&self) -> bool {
        true
    }
}

fn parse_jpeg(input: &ParseInput<'_>) -> Result<Metadata, ParseError> {
    if bytes_within(input.head, input.file_len, 0, 2)? != b"\xFF\xD8" {
        return Err(ParseError::WrongFormat { expected: "jpeg" });
    }
    let segments = walk_segments(input)?;
    let mut meta = match segments.exif_base {
        Some(base) => read_tiff_exif(input, base, &[TIFF_MAGIC])?.meta,
        None => Metadata::default(),
    };
    // A broken MPF index only costs the MPF preview; the maker-note preview
    // from the EXIF pass is kept in that case.
    let mpf = match segments.mpf_base {
        Some(base) => optional(mpf_preview(input, base))?.flatten(),
        None => None,
    };
    meta.preview = mpf.or(meta.preview);
    Ok(meta)
}

#[derive(Debug, Default)]
struct Segments {
    /// Absolute start of the first Exif APP1 TIFF header.
    exif_base: Option<u64>,
    /// Absolute start of the first MPF APP2 TIFF header.
    mpf_base: Option<u64>,
}

/// Walks the marker segments from the SOI up to SOS or EOI, recording where
/// the first Exif APP1 and MPF APP2 TIFF headers start. Stops after
/// `MAX_SEGMENTS` segments and keeps what it found.
fn walk_segments(input: &ParseInput<'_>) -> Result<Segments, ParseError> {
    let mut segments = Segments::default();
    let mut pos: u64 = 2;
    for _ in 0..MAX_SEGMENTS {
        if byte_at(input, pos)? != 0xFF {
            return Err(ParseError::Malformed {
                what: "jpeg marker",
            });
        }
        let mut code_at = pos + 1;
        let mut code = byte_at(input, code_at)?;
        while code == 0xFF {
            code_at += 1;
            code = byte_at(input, code_at)?;
        }
        if code == SOS || code == EOI {
            return Ok(segments);
        }
        let length_at = code_at + 1;
        if code == 0x01 || (0xD0..=0xD7).contains(&code) {
            pos = length_at;
            continue;
        }
        let len = segment_length(input, length_at)?;
        let payload = length_at + 2;
        let payload_len = u64::from(len) - 2;
        if code == APP1
            && segments.exif_base.is_none()
            && has_prefix(input, payload, payload_len, b"Exif\0\0")?
        {
            segments.exif_base = Some(payload + 6);
        }
        if code == APP2
            && segments.mpf_base.is_none()
            && has_prefix(input, payload, payload_len, b"MPF\0")?
        {
            segments.mpf_base = Some(payload + 4);
        }
        pos = length_at + u64::from(len);
    }
    Ok(segments)
}

fn byte_at(input: &ParseInput<'_>, at: u64) -> Result<u8, ParseError> {
    match bytes_within(input.head, input.file_len, at, 1)? {
        [b] => Ok(*b),
        _ => Err(ParseError::Malformed {
            what: "jpeg marker",
        }),
    }
}

/// The big-endian segment length at `at`, which counts its own two bytes.
fn segment_length(input: &ParseInput<'_>, at: u64) -> Result<u16, ParseError> {
    let len = match bytes_within(input.head, input.file_len, at, 2)? {
        [hi, lo] => u16::from_be_bytes([*hi, *lo]),
        _ => {
            return Err(ParseError::Malformed {
                what: "jpeg segment length",
            });
        }
    };
    if len < 2 {
        return Err(ParseError::Malformed {
            what: "jpeg segment length",
        });
    }
    Ok(len)
}

fn has_prefix(
    input: &ParseInput<'_>,
    at: u64,
    payload_len: u64,
    prefix: &[u8],
) -> Result<bool, ParseError> {
    let n = prefix.len() as u64;
    if payload_len < n {
        return Ok(false);
    }
    Ok(bytes_within(input.head, input.file_len, at, n)? == prefix)
}

/// The MPF preview: the first index entry whose type is a large thumbnail,
/// located relative to the MPF TIFF header at `base`. Source: ExifTool
/// MPF.pm:197-231, as cited in the phase plan.
fn mpf_preview(input: &ParseInput<'_>, base: u64) -> Result<Option<ByteRange>, ParseError> {
    let (tiff, ifd0_at) = Tiff::parse(input.head, input.file_len, base, &[TIFF_MAGIC])?;
    let ifd0 = tiff.read_ifd(ifd0_at)?;
    let Some(index) = optional_tag(&ifd0, TAG_MP_ENTRY, |e| e.raw(&tiff))? else {
        return Ok(None);
    };
    if index.len() / MPF_ENTRY_LEN > MAX_MPF_ENTRIES {
        return Err(ParseError::Malformed { what: "mpf index" });
    }
    let order = tiff.order();
    let (entries, _) = index.as_chunks::<MPF_ENTRY_LEN>();
    let found = entries.iter().find_map(|entry| {
        let attr = word(order, entry, 0)?;
        let size = word(order, entry, 4)?;
        let offset = word(order, entry, 8)?;
        let is_large_thumbnail = attr & 0x0F_0000 == 0x01_0000;
        (offset != 0 && size != 0 && is_large_thumbnail).then_some((offset, size))
    });
    let Some((offset, size)) = found else {
        return Ok(None);
    };
    let start = base.checked_add(u64::from(offset)).ok_or(OVERFLOW)?;
    Ok(ByteRange::within(start, u64::from(size), input.file_len))
}

/// A `u32` at `at` in an MPF entry, in the MPF TIFF's byte order.
fn word(order: ByteOrder, entry: &[u8], at: usize) -> Option<u32> {
    let bytes: [u8; 4] = entry.get(at..at + 4)?.try_into().ok()?;
    Some(match order {
        ByteOrder::Little => u32::from_le_bytes(bytes),
        ByteOrder::Big => u32::from_be_bytes(bytes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::makernotes::nikon::NikonMakerNotes;
    use crate::metadata::registry::MakerNoteReader;
    use crate::metadata::test_support::{decoded_size, expected_zf, slice};
    use photosort_testkit::{ExifSpec, NikonSpec, Order, jpeg_file, tiff_file, tiny_jpeg};

    fn read_jpeg(file: &[u8]) -> Result<Metadata, ParseError> {
        let makernotes: Vec<Box<dyn MakerNoteReader>> = vec![Box::new(NikonMakerNotes)];
        let input = ParseInput {
            head: file,
            file_len: u64::try_from(file.len()).unwrap(),
            makernotes: &makernotes,
        };
        JpegReader.parse(&input)
    }

    #[test]
    fn reads_exif_and_shot_counter_in_both_byte_orders() {
        let expected = Metadata {
            shot_counter: Some(75036),
            ..expected_zf()
        };
        for order in [Order::Little, Order::Big] {
            let file = jpeg_file(
                order,
                Some(&ExifSpec::zf()),
                Some(&NikonSpec::zf(None)),
                None,
            );
            assert_eq!(read_jpeg(&file).unwrap(), expected, "order {order:?}");
        }
    }

    #[test]
    fn mpf_preview_locates_a_640_by_424_jpeg() {
        let preview = tiny_jpeg(640, 424);
        for order in [Order::Little, Order::Big] {
            let file = jpeg_file(order, Some(&ExifSpec::zf()), None, Some(&preview));
            let range = read_jpeg(&file)
                .unwrap()
                .preview
                .expect("MPF preview located");
            let bytes = slice(&file, range);
            assert_eq!(bytes, preview.as_slice(), "order {order:?}");
            assert_eq!(decoded_size(bytes), (640, 424));
        }
    }

    #[test]
    fn jpeg_without_app1_or_app2_has_default_metadata() {
        let file = jpeg_file(Order::Little, None, None, None);
        assert_eq!(read_jpeg(&file).unwrap(), Metadata::default());
    }

    #[test]
    fn short_head_reports_need_more_within_the_file() {
        let file = jpeg_file(Order::Little, Some(&ExifSpec::zf()), None, None);
        let file_len = u64::try_from(file.len()).unwrap();
        let input = ParseInput {
            head: &file[..200],
            file_len,
            makernotes: &[],
        };
        match JpegReader.parse(&input) {
            Err(ParseError::NeedMore { needed, have }) => {
                assert_eq!(have, 200);
                assert!(needed > have && needed <= file_len, "needed {needed}");
            }
            other => panic!("expected NeedMore, got {other:?}"),
        }
    }

    #[test]
    fn non_jpeg_is_wrong_format() {
        let file = tiff_file(Order::Little, 42, &ExifSpec::zf(), None);
        assert_eq!(
            read_jpeg(&file),
            Err(ParseError::WrongFormat { expected: "jpeg" })
        );
    }

    #[test]
    fn byte_other_than_marker_prefix_is_malformed() {
        let file = [0xFF, 0xD8, 0x12, 0x34, 0x00, 0x02];
        assert_eq!(
            read_jpeg(&file),
            Err(ParseError::Malformed {
                what: "jpeg marker"
            })
        );
    }
}
