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
    use photosort_testkit::{
        ExifSpec, IfdBuilder, NikonSpec, Order, nef_file, tiny_jpeg, write_tiff,
    };

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

    /// A little-endian TIFF whose IFD0 and one SubIFD each hold an optional
    /// 0x0201/0x0202 JPEG pair. The SubIFD is always present, so a missing
    /// JPEG there is a missing candidate, not a missing structure.
    fn jpeg_candidates(ifd0_jpeg: Option<&[u8]>, sub_jpeg: Option<&[u8]>) -> Vec<u8> {
        let mut ifd0 = IfdBuilder::new();
        if let Some(jpg) = ifd0_jpeg {
            ifd0 = ifd0.blob_offset(0x0201, jpg).blob_len(0x0202, jpg);
        }
        let mut sub = IfdBuilder::new();
        if let Some(jpg) = sub_jpeg {
            sub = sub.blob_offset(0x0201, jpg).blob_len(0x0202, jpg);
        }
        let ifd0 = ifd0.children(0x014A, vec![sub]);
        write_tiff(Order::Little, TIFF_MAGIC, &ifd0, 0).bytes
    }

    /// `full_jpeg` of a TIFF built by `jpeg_candidates`, with `preview` excluded.
    fn full_jpeg_of(file: &[u8], preview: Option<ByteRange>) -> Option<ByteRange> {
        let len = u64::try_from(file.len()).unwrap();
        let (tiff, ifd0_at) = Tiff::parse(file, len, 0, &[TIFF_MAGIC]).unwrap();
        let ifd0 = tiff.read_ifd(ifd0_at).unwrap();
        full_jpeg(&tiff, &ifd0, preview).unwrap()
    }

    /// Where the bytes of `blob` sit in `file`.
    fn range_of(file: &[u8], blob: &[u8]) -> ByteRange {
        let offset = file
            .windows(blob.len())
            .position(|w| w == blob)
            .expect("blob is written");
        ByteRange {
            offset: u64::try_from(offset).unwrap(),
            len: u64::try_from(blob.len()).unwrap(),
        }
    }

    #[test]
    fn larger_jpeg_in_a_subifd_beats_a_smaller_one_in_ifd0() {
        let small = tiny_jpeg(160, 120);
        let big = tiny_jpeg(1200, 800);
        let file = jpeg_candidates(Some(&small), Some(&big));
        let chosen = full_jpeg_of(&file, None).expect("a candidate is chosen");
        assert_eq!(chosen, range_of(&file, &big));
        assert_eq!(decoded_size(slice(&file, chosen)), (1200, 800));
    }

    #[test]
    fn larger_jpeg_in_ifd0_beats_a_smaller_one_in_a_subifd() {
        let big = tiny_jpeg(1200, 800);
        let small = tiny_jpeg(160, 120);
        let file = jpeg_candidates(Some(&big), Some(&small));
        assert_eq!(full_jpeg_of(&file, None), Some(range_of(&file, &big)));
    }

    #[test]
    fn candidate_equal_to_the_preview_range_is_excluded() {
        let small = tiny_jpeg(160, 120);
        let big = tiny_jpeg(1200, 800);
        let file = jpeg_candidates(Some(&small), Some(&big));
        // The larger SubIFD JPEG is the preview, so the smaller IFD0 JPEG wins.
        let chosen = full_jpeg_of(&file, Some(range_of(&file, &big)));
        assert_eq!(chosen, Some(range_of(&file, &small)));
    }

    #[test]
    fn preview_as_the_only_candidate_leaves_no_full_jpeg() {
        let big = tiny_jpeg(1200, 800);
        let file = jpeg_candidates(None, Some(&big));
        assert_eq!(full_jpeg_of(&file, Some(range_of(&file, &big))), None);
    }

    /// Writes `value` into the value field of `tag` in the ExifIFD of a
    /// little-endian NEF, so the entry points at `value`.
    fn point_exif_entry_at(file: &mut [u8], tag: u16, value: u32) {
        let len = u64::try_from(file.len()).unwrap();
        let (tiff, ifd0_at) = Tiff::parse(file, len, 0, &[TIFF_MAGIC]).unwrap();
        let ifd0 = tiff.read_ifd(ifd0_at).unwrap();
        let exif_rel = ifd0.get(0x8769).unwrap().first_u32(&tiff).unwrap();
        let exif_at = tiff.resolve(exif_rel).unwrap();
        let exif = tiff.read_ifd(exif_at).unwrap();
        let index = exif.entries.iter().position(|e| e.tag == tag).unwrap();
        let field = exif_at + 2 + 12 * u64::try_from(index).unwrap() + 8;
        let at = usize::try_from(field).unwrap();
        file[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    /// LensModel points 40 MiB into a file declared 60 MiB long. The NEF bytes
    /// themselves are the whole file, so the read past them is a head request
    /// that no head the shell may read can satisfy. LensModel must read as
    /// absent and every other field must survive.
    #[test]
    fn far_lens_model_offset_drops_only_lens_model() {
        let mut file = nef_file(
            Order::Little,
            &ExifSpec::zf(),
            Some(&NikonSpec::zf(None)),
            None,
            0,
        );
        point_exif_entry_at(&mut file, 0xA434, 40 * 1024 * 1024);
        let makernotes: Vec<Box<dyn MakerNoteReader>> = vec![Box::new(NikonMakerNotes)];
        let input = ParseInput {
            head: &file,
            file_len: 60 * 1024 * 1024,
            makernotes: &makernotes,
        };
        let meta = NefReader
            .parse(&input)
            .expect("a far optional offset does not fail the parse");
        assert_eq!(
            meta,
            Metadata {
                lens_model: None,
                shot_counter: Some(75036),
                ..expected_zf()
            }
        );
    }
}
