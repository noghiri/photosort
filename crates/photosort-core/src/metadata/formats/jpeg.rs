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
///
/// A segment whose declared end is past end-of-file is `Malformed`: its bytes
/// can never arrive. Deviation from phase_02.md: the plan propagates every
/// walk error. Here, once one base is recorded, a later corrupt marker or
/// truncated segment keeps the bases found so far, so one bad segment does not
/// discard valid EXIF. With no base yet, the error fails the file, so a JPEG
/// cut inside its APP1 is an error rather than a name-only placeholder.
/// `NeedMore` always propagates, so the shell can supply the bytes it asks
/// for. The walk also stops as soon as both bases are set, because reading
/// on could only add `NeedMore` re-reads.
fn walk_segments(input: &ParseInput<'_>) -> Result<Segments, ParseError> {
    let mut segments = Segments::default();
    match scan_segments(input, &mut segments) {
        Ok(()) => Ok(segments),
        Err(need @ ParseError::NeedMore { .. }) => Err(need),
        Err(_) if segments.exif_base.is_some() || segments.mpf_base.is_some() => Ok(segments),
        Err(err) => Err(err),
    }
}

/// Records the bases into `segments` as it goes, so a caller that gets an
/// error still sees the bases found before it.
fn scan_segments(input: &ParseInput<'_>, segments: &mut Segments) -> Result<(), ParseError> {
    let mut pos: u64 = 2;
    // Every position used below was either read by `byte_at` or
    // `segment_length`, which succeeded, or is one of those reads' ends.
    // Arithmetic on those positions therefore stays within `head` plus at
    // most one segment length (65,535 bytes), so no u64 sum can overflow.
    for _ in 0..MAX_SEGMENTS {
        if byte_at(input, pos)? != 0xFF {
            return Err(ParseError::Malformed {
                what: "jpeg marker",
            });
        }
        // `byte_at(pos)` succeeded, so `pos` is inside `head`.
        let mut code_at = pos + 1;
        let mut code = byte_at(input, code_at)?;
        while code == 0xFF {
            // The previous `byte_at(code_at)` read a 0xFF, so `code_at` is inside `head`.
            code_at += 1;
            code = byte_at(input, code_at)?;
        }
        if code == SOS || code == EOI {
            return Ok(());
        }
        // `code_at` was read by `byte_at`, so `code_at + 1` cannot overflow.
        let length_at = code_at + 1;
        if code == 0x01 || (0xD0..=0xD7).contains(&code) {
            pos = length_at;
            continue;
        }
        let len = segment_length(input, length_at)?;
        // A segment whose declared end is past EOF is truncated. Its bytes can
        // never arrive, so it is `Malformed`, not `NeedMore`. The check uses
        // `file_len`, not `head.len()`, so a short head still asks for more.
        if length_at.checked_add(u64::from(len)).ok_or(OVERFLOW)? > input.file_len {
            return Err(ParseError::Malformed {
                what: "jpeg segment past end of file",
            });
        }
        // `segment_length` read the two bytes at `length_at`, so `length_at + 2` is at most `head.len()`.
        let payload = length_at + 2;
        // `segment_length` rejects `len < 2`, so the subtraction cannot underflow.
        let payload_len = u64::from(len) - 2;
        if code == APP1
            && segments.exif_base.is_none()
            && has_prefix(input, payload, payload_len, b"Exif\0\0")?
        {
            // `has_prefix` returned true, so it read the six bytes at `payload`.
            segments.exif_base = Some(payload + 6);
        }
        if code == APP2
            && segments.mpf_base.is_none()
            && has_prefix(input, payload, payload_len, b"MPF\0")?
        {
            // `has_prefix` returned true, so it read the four bytes at `payload`.
            segments.mpf_base = Some(payload + 4);
        }
        if segments.exif_base.is_some() && segments.mpf_base.is_some() {
            return Ok(());
        }
        // `length_at + 2` is inside `head` and `len` is at most 65,535, so this
        // is at most `head.len() + 65,533`. The next `byte_at` checks it.
        pos = length_at + u64::from(len);
    }
    Ok(())
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
    use photosort_testkit::{
        ExifSpec, NikonSpec, Order, jpeg_file, jpeg_file_with_mpf_order, tiff_file, tiny_jpeg,
    };

    fn read_jpeg(file: &[u8]) -> Result<Metadata, ParseError> {
        parse_head(file, file.len())
    }

    /// Parses `head` as the first bytes of a file of `file_len` bytes.
    fn parse_head(head: &[u8], file_len: usize) -> Result<Metadata, ParseError> {
        let makernotes: Vec<Box<dyn MakerNoteReader>> = vec![Box::new(NikonMakerNotes)];
        let input = ParseInput {
            head,
            file_len: u64::try_from(file_len).unwrap(),
            makernotes: &makernotes,
        };
        JpegReader.parse(&input)
    }

    /// Offset just past the first segment with `marker`, found by walking the
    /// segment headers of a file the testkit wrote.
    fn segment_end(file: &[u8], marker: u8) -> usize {
        let mut pos = 2;
        loop {
            let len = usize::from(u16::from_be_bytes([file[pos + 2], file[pos + 3]]));
            if file[pos + 1] == marker {
                return pos + 2 + len;
            }
            pos += 2 + len;
        }
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

    /// Offset of the MPF TIFF header: the first `MPF\0` marker payload plus 4.
    fn mpf_tiff_base(file: &[u8]) -> usize {
        file.windows(4)
            .position(|w| w == b"MPF\0")
            .expect("MPF APP2 is written")
            + 4
    }

    #[test]
    fn maker_note_preview_is_kept_when_there_is_no_mpf() {
        let preview = tiny_jpeg(640, 424);
        let nikon = NikonSpec {
            preview_jpeg: Some(preview.clone()),
            preview_inside: true,
            ..NikonSpec::zf(None)
        };
        for order in [Order::Little, Order::Big] {
            let file = jpeg_file(order, Some(&ExifSpec::zf()), Some(&nikon), None);
            let range = read_jpeg(&file)
                .unwrap()
                .preview
                .expect("maker-note preview located");
            let bytes = slice(&file, range);
            assert_eq!(bytes, preview.as_slice(), "order {order:?}");
            assert_eq!(decoded_size(bytes), (640, 424));
        }
    }

    #[test]
    fn maker_note_preview_is_kept_when_the_mpf_index_is_broken() {
        let note_preview = tiny_jpeg(640, 424);
        let mpf_preview = tiny_jpeg(320, 212);
        let nikon = NikonSpec {
            preview_jpeg: Some(note_preview.clone()),
            preview_inside: true,
            ..NikonSpec::zf(None)
        };
        for order in [Order::Little, Order::Big] {
            let mut file = jpeg_file(
                order,
                Some(&ExifSpec::zf()),
                Some(&nikon),
                Some(&mpf_preview),
            );

            // Intact MPF: its preview wins over the maker-note preview.
            let intact = read_jpeg(&file)
                .unwrap()
                .preview
                .expect("MPF preview located");
            assert_eq!(
                decoded_size(slice(&file, intact)),
                (320, 212),
                "order {order:?}"
            );

            // An IFD entry count above the walker's limit breaks the MPF index.
            let base = mpf_tiff_base(&file);
            file[base + 8..base + 10].copy_from_slice(&[0xFF, 0xFF]);
            let range = read_jpeg(&file)
                .unwrap()
                .preview
                .expect("maker-note preview kept");
            let bytes = slice(&file, range);
            assert_eq!(bytes, note_preview.as_slice(), "order {order:?}");
            assert_eq!(decoded_size(bytes), (640, 424));
        }
    }

    /// The MPF index is read in the MPF TIFF's own byte order, whatever the
    /// Exif block uses. A reader hard-coded to big-endian fails the
    /// little-endian MPF cases.
    #[test]
    fn mpf_preview_is_read_in_the_mpf_byte_order_not_the_exif_order() {
        let preview = tiny_jpeg(640, 424);
        for order in [Order::Little, Order::Big] {
            for mpf_order in [Order::Little, Order::Big] {
                let file = jpeg_file_with_mpf_order(
                    order,
                    mpf_order,
                    Some(&ExifSpec::zf()),
                    None,
                    Some(&preview),
                );
                let range = read_jpeg(&file)
                    .unwrap()
                    .preview
                    .expect("MPF preview located");
                let bytes = slice(&file, range);
                assert_eq!(
                    bytes,
                    preview.as_slice(),
                    "exif {order:?}, mpf {mpf_order:?}"
                );
                assert_eq!(decoded_size(bytes), (640, 424));
            }
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

    /// A corrupt marker after a usable APP1 keeps the EXIF. The plan's walker
    /// failed the whole file here.
    #[test]
    fn exif_is_kept_when_a_later_marker_is_corrupt() {
        let file = jpeg_file(Order::Little, Some(&ExifSpec::zf()), None, None);
        let mut corrupt = file.clone();
        corrupt[segment_end(&file, 0xE1)] = 0x00;
        let clean = read_jpeg(&file).unwrap();
        assert!(clean.make.is_some(), "the clean file has EXIF");
        assert_eq!(read_jpeg(&corrupt), Ok(clean));
    }

    /// Once the EXIF and MPF bases are both found the walk stops. A head that
    /// ends right after the MPF segment therefore parses without a re-read.
    #[test]
    fn walk_stops_once_exif_and_mpf_are_found() {
        let preview = tiny_jpeg(320, 212);
        let file = jpeg_file(Order::Little, Some(&ExifSpec::zf()), None, Some(&preview));
        let head = &file[..segment_end(&file, 0xE2)];
        assert_eq!(parse_head(head, file.len()), read_jpeg(&file));
    }

    /// With EXIF found but the MPF segment not yet read, a short head still
    /// asks for more. Returning the EXIF alone would drop the preview.
    #[test]
    fn need_more_propagates_while_the_mpf_base_is_missing() {
        let preview = tiny_jpeg(320, 212);
        let file = jpeg_file(Order::Little, Some(&ExifSpec::zf()), None, Some(&preview));
        let head = &file[..segment_end(&file, 0xE1)];
        assert!(matches!(
            parse_head(head, file.len()),
            Err(ParseError::NeedMore { .. })
        ));
    }

    /// An APP1 whose declared end passes end-of-file, with no earlier base,
    /// fails the file. It is not read as partial EXIF.
    #[test]
    fn truncated_app1_with_no_earlier_base_is_malformed() {
        let file = jpeg_file(Order::Little, Some(&ExifSpec::zf()), None, None);
        let cut = segment_end(&file, 0xE1) - 1;
        assert_eq!(
            read_jpeg(&file[..cut]),
            Err(ParseError::Malformed {
                what: "jpeg segment past end of file"
            })
        );
    }

    /// A complete APP1 followed by an APP2 cut at end-of-file keeps the EXIF.
    /// The truncated APP2 is dropped as the partial-result rule allows.
    #[test]
    fn complete_app1_followed_by_a_truncated_app2_keeps_the_exif() {
        let preview = tiny_jpeg(320, 212);
        let file = jpeg_file(Order::Little, Some(&ExifSpec::zf()), None, Some(&preview));
        let exif_only = &file[..segment_end(&file, 0xE1)];
        let cut = segment_end(&file, 0xE2) - 1;
        let expected = read_jpeg(exif_only).expect("the EXIF-only file parses");
        assert!(expected.make.is_some(), "the APP1 holds EXIF");
        assert_eq!(read_jpeg(&file[..cut]), Ok(expected));
    }

    /// MPF found with no Exif APP1, then a corrupt marker. The walk error must
    /// keep the MPF base, so the preview survives. The corrupt byte is the
    /// first marker after the MPF segment, which makes the walk fail with
    /// `Malformed`, not `NeedMore`.
    #[test]
    fn mpf_is_kept_when_a_later_marker_is_corrupt_and_there_is_no_exif() {
        let preview = tiny_jpeg(320, 212);
        let file = jpeg_file(Order::Little, None, None, Some(&preview));
        let mut corrupt = file.clone();
        corrupt[segment_end(&file, 0xE2)] = 0x00;
        let clean = read_jpeg(&file).unwrap();
        assert!(clean.preview.is_some(), "the clean file has an MPF preview");
        assert_eq!(read_jpeg(&corrupt), Ok(clean));
    }
}
