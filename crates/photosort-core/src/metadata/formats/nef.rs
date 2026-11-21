//! NEF reader: EXIF and the Nikon maker note, the PreviewIFD preview, and the
//! full-size JpgFromRaw found in IFD0 or its SubIFDs.
//!
//! Assumption, inferred and not verified against a spec: HE and HE* compression
//! leaves the TIFF container unchanged, so the same offsets locate the JPEG.
//! The oracle tests on real Z f samples check this.
// pattern: Functional Core

use crate::metadata::exif::{TiffFile, read_tiff_exif};
use crate::metadata::registry::{FormatReader, ParseInput};
use crate::metadata::tiff::{Ifd, TIFF_MAGIC, Tiff, byte_range, optional, optional_tag};
use crate::model::{ByteRange, FileKind, Metadata, ParseError};

pub const KIND: FileKind = FileKind("nef");

const TAG_SUB_IFDS: u16 = 0x014A;
const TAG_JPEG_START: u16 = 0x0201;
const TAG_JPEG_LEN: u16 = 0x0202;
/// SubIFDs are followed one level deep only, which bounds the walk and rules
/// out loops. Source: rawler nef.rs:390-406.
const MAX_SUB_IFDS: usize = 16;

pub struct NefReader;

impl FormatReader for NefReader {
    fn kind(&self) -> FileKind {
        KIND
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["nef"]
    }

    fn badge(&self) -> &'static str {
        "N"
    }

    fn is_raw(&self) -> bool {
        true
    }

    fn header_len_hint(&self) -> usize {
        128 * 1024
    }

    fn parse(&self, input: &ParseInput<'_>) -> Result<Metadata, ParseError> {
        let TiffFile {
            tiff,
            ifd0,
            mut meta,
        } = read_tiff_exif(input, 0, &[TIFF_MAGIC])?;
        meta.full_jpeg = full_jpeg(&tiff, &ifd0, meta.preview)?;
        Ok(meta)
    }

    fn lens_write_supported(&self) -> bool {
        true
    }
}

/// The largest JPEG the file embeds that is not the preview. Candidates are
/// IFD0 and each SubIFD; a candidate's JPEG is the range named by its
/// 0x0201/0x0202 pair. ExifTool names only SubIFD 0 "JpgFromRaw", so the
/// index is not relied on.
fn full_jpeg(
    tiff: &Tiff<'_>,
    ifd0: &Ifd,
    preview: Option<ByteRange>,
) -> Result<Option<ByteRange>, ParseError> {
    let sub_ifds = optional_tag(ifd0, TAG_SUB_IFDS, |e| e.u32_values(tiff))?.unwrap_or_default();
    let mut jpegs: Vec<ByteRange> = jpeg_in(tiff, ifd0)?.into_iter().collect();
    for rel in sub_ifds.into_iter().take(MAX_SUB_IFDS) {
        let sub = optional(tiff.resolve(rel).and_then(|at| tiff.read_ifd(at)))?;
        if let Some(sub) = sub {
            jpegs.extend(jpeg_in(tiff, &sub)?);
        }
    }
    Ok(jpegs
        .into_iter()
        .filter(|range| Some(*range) != preview)
        .max_by_key(|range| range.len))
}

fn jpeg_in(tiff: &Tiff<'_>, ifd: &Ifd) -> Result<Option<ByteRange>, ParseError> {
    Ok(optional(byte_range(tiff, ifd, TAG_JPEG_START, TAG_JPEG_LEN))?.flatten())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::makernotes::nikon::NikonMakerNotes;
    use crate::metadata::registry::MakerNoteReader;
    use crate::metadata::test_support::{decoded_size, expected_zf, slice};
    use crate::metadata::tiff::{TIFF_MAGIC, Tiff};
    use photosort_testkit::{ExifSpec, NikonSpec, Order, nef_file, tiny_jpeg};

    fn parse_with_nikon(file: &[u8], head_len: usize) -> Result<Metadata, ParseError> {
        let makernotes: Vec<Box<dyn MakerNoteReader>> = vec![Box::new(NikonMakerNotes)];
        let input = ParseInput {
            head: &file[..head_len],
            file_len: u64::try_from(file.len()).unwrap(),
            makernotes: &makernotes,
        };
        NefReader.parse(&input)
    }

    fn read_nef(file: &[u8]) -> Metadata {
        parse_with_nikon(file, file.len()).unwrap()
    }

    /// Offset and length of the MakerNote value, located by the TIFF walker.
    fn maker_note_span(file: &[u8]) -> (u64, u64) {
        let len = u64::try_from(file.len()).unwrap();
        let (tiff, ifd0_at) = Tiff::parse(file, len, 0, &[TIFF_MAGIC]).unwrap();
        let ifd0 = tiff.read_ifd(ifd0_at).unwrap();
        let exif_rel = ifd0.get(0x8769).unwrap().first_u32(&tiff).unwrap();
        let exif = tiff.read_ifd(tiff.resolve(exif_rel).unwrap()).unwrap();
        exif.get(0x927C).unwrap().location(&tiff).unwrap()
    }

    fn nikon_with_preview(preview: Vec<u8>, preview_inside: bool) -> NikonSpec {
        NikonSpec {
            preview_jpeg: Some(preview),
            preview_inside,
            ..NikonSpec::zf(None)
        }
    }

    #[test]
    fn reads_exif_and_shot_counter_in_both_byte_orders() {
        let expected = Metadata {
            shot_counter: Some(75036),
            ..expected_zf()
        };
        for order in [Order::Little, Order::Big] {
            let file = nef_file(order, &ExifSpec::zf(), Some(&NikonSpec::zf(None)), None, 0);
            assert_eq!(read_nef(&file), expected, "order {order:?}");
        }
    }

    #[test]
    fn preview_and_jpg_from_raw_decode_to_their_sizes() {
        let preview = tiny_jpeg(640, 424);
        let jpg = tiny_jpeg(1200, 800);
        for order in [Order::Little, Order::Big] {
            let nikon = nikon_with_preview(preview.clone(), false);
            let file = nef_file(order, &ExifSpec::zf(), Some(&nikon), Some(&jpg), 0);
            let meta = read_nef(&file);

            let preview_range = meta.preview.expect("PreviewIFD preview located");
            assert_eq!(
                slice(&file, preview_range),
                preview.as_slice(),
                "order {order:?}"
            );
            assert_eq!(decoded_size(slice(&file, preview_range)), (640, 424));

            let full_range = meta.full_jpeg.expect("JpgFromRaw located");
            assert_eq!(slice(&file, full_range), jpg.as_slice(), "order {order:?}");
            assert_eq!(decoded_size(slice(&file, full_range)), (1200, 800));
        }
    }

    #[test]
    fn preview_inside_the_maker_note_lies_within_its_value() {
        let preview = tiny_jpeg(640, 424);
        for order in [Order::Little, Order::Big] {
            let nikon = nikon_with_preview(preview.clone(), true);
            let file = nef_file(order, &ExifSpec::zf(), Some(&nikon), None, 0);
            let (note_at, note_len) = maker_note_span(&file);
            let range = read_nef(&file).preview.expect("preview located");
            assert!(
                note_at <= range.offset && range.end() <= note_at + note_len,
                "preview {range:?} outside note {note_at}+{note_len}, order {order:?}"
            );
            assert_eq!(decoded_size(slice(&file, range)), (640, 424));
        }
    }

    #[test]
    fn ranges_past_a_128_kib_head_are_located_without_need_more() {
        let preview = tiny_jpeg(640, 424);
        let jpg = tiny_jpeg(1200, 800);
        let head_len = 128 * 1024;
        let nikon = nikon_with_preview(preview.clone(), false);
        let file = nef_file(
            Order::Little,
            &ExifSpec::zf(),
            Some(&nikon),
            Some(&jpg),
            200 * 1024,
        );
        let meta = parse_with_nikon(&file, head_len).expect("parses inside the head");

        let preview_range = meta.preview.expect("preview located");
        let full_range = meta.full_jpeg.expect("JpgFromRaw located");
        assert!(preview_range.offset >= u64::try_from(head_len).unwrap());
        assert!(full_range.offset >= u64::try_from(head_len).unwrap());
        assert_eq!(slice(&file, preview_range), preview.as_slice());
        assert_eq!(slice(&file, full_range), jpg.as_slice());
    }

    #[test]
    fn nef_without_sub_ifd_jpeg_or_maker_note_has_no_vendor_or_raw_jpeg() {
        let file = nef_file(Order::Big, &ExifSpec::zf(), None, None, 0);
        assert_eq!(read_nef(&file), expected_zf());
    }
}
