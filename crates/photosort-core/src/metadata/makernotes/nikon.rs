//! Nikon "type 3" maker note: ShutterCount, the body serial and the
//! PreviewIFD preview location.
// pattern: Functional Core

use crate::metadata::registry::{MakerNote, MakerNoteReader, VendorFields};
use crate::metadata::tiff::{
    Ifd, OVERFLOW, TIFF_MAGIC, Tiff, byte_range, bytes_within, optional_tag,
};
use crate::model::{ByteRange, ParseError};

/// The embedded TIFF header starts 10 bytes into the maker note: a 6-byte
/// `Nikon\0` tag, then a 4-byte version. Source: ExifTool MakerNotes.pm:51-57.
const TIFF_BASE_OFFSET: u64 = 10;

const TAG_SERIAL: u16 = 0x001D;
/// Deliberately not MechanicalShutterCount (0x0037): it does not advance in
/// silent mode, so burst frames would collide.
const TAG_SHUTTER_COUNT: u16 = 0x00A7;
const TAG_PREVIEW_IFD: u16 = 0x0011;
const TAG_PREVIEW_START: u16 = 0x0201;
const TAG_PREVIEW_LEN: u16 = 0x0202;

pub struct NikonMakerNotes;

impl MakerNoteReader for NikonMakerNotes {
    fn matches_make(&self, make: &str) -> bool {
        make.trim().to_ascii_uppercase().starts_with("NIKON")
    }

    fn read(&self, note: &MakerNote<'_>) -> Result<VendorFields, ParseError> {
        let header = bytes_within(note.head, note.file_len, note.offset, 7)?;
        if header != b"Nikon\0\x02" {
            return Err(ParseError::WrongFormat {
                expected: "nikon type 3 maker note",
            });
        }
        let base = note.offset.checked_add(TIFF_BASE_OFFSET).ok_or(OVERFLOW)?;
        let (tiff, ifd0_at) = Tiff::parse(note.head, note.file_len, base, &[TIFF_MAGIC])?;
        let ifd0 = tiff.read_ifd(ifd0_at)?;
        Ok(VendorFields {
            shot_counter: optional_tag(&ifd0, TAG_SHUTTER_COUNT, |e| e.first_u32(&tiff))?
                .map(u64::from),
            // An empty string is no serial, as in the Exif body-serial read.
            serial: optional_tag(&ifd0, TAG_SERIAL, |e| e.ascii(&tiff))?.filter(|s| !s.is_empty()),
            preview: preview(&tiff, &ifd0)?,
        })
    }
}

/// The NEF preview named by the PreviewIFD (tag 0x0011). Its offsets are
/// relative to the embedded TIFF header.
fn preview(tiff: &Tiff<'_>, ifd0: &Ifd) -> Result<Option<ByteRange>, ParseError> {
    let Some(preview_ifd) = optional_tag(ifd0, TAG_PREVIEW_IFD, |e| {
        e.first_u32(tiff)
            .and_then(|rel| tiff.resolve(rel))
            .and_then(|at| tiff.read_ifd(at))
    })?
    else {
        return Ok(None);
    };
    byte_range(tiff, &preview_ifd, TAG_PREVIEW_START, TAG_PREVIEW_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::exif::read_tiff_exif;
    use crate::metadata::registry::ParseInput;
    use crate::metadata::test_support::{decoded_size, slice};
    use crate::model::Metadata;
    use photosort_testkit::{ExifSpec, NikonSpec, Order, nef_file, tiny_jpeg};

    fn read_nef(file: &[u8]) -> Metadata {
        let makernotes: Vec<Box<dyn MakerNoteReader>> = vec![Box::new(NikonMakerNotes)];
        let input = ParseInput {
            head: file,
            file_len: u64::try_from(file.len()).unwrap(),
            makernotes: &makernotes,
        };
        read_tiff_exif(&input, 0, &[42]).unwrap().meta
    }

    #[test]
    fn matches_nikon_makes_case_insensitively() {
        assert!(NikonMakerNotes.matches_make("NIKON CORPORATION"));
        assert!(NikonMakerNotes.matches_make("Nikon"));
        assert!(!NikonMakerNotes.matches_make("CANON"));
    }

    #[test]
    fn reads_shutter_count_not_mechanical_count_in_both_byte_orders() {
        for order in [Order::Little, Order::Big] {
            let file = nef_file(order, &ExifSpec::zf(), Some(&NikonSpec::zf(None)), None, 0);
            assert_eq!(read_nef(&file).shot_counter, Some(75036), "order {order:?}");
        }
    }

    #[test]
    fn serial_comes_from_maker_note_when_body_serial_is_absent() {
        let exif = ExifSpec {
            body_serial: None,
            ..ExifSpec::zf()
        };
        let nikon = NikonSpec {
            serial: Some("7654321".to_owned()),
            ..NikonSpec::zf(None)
        };
        let file = nef_file(Order::Little, &exif, Some(&nikon), None, 0);
        assert_eq!(read_nef(&file).serial.as_deref(), Some("7654321"));
    }

    /// An empty maker-note serial reads as absent, as an empty body serial does.
    #[test]
    fn empty_maker_note_serial_reads_as_absent() {
        let exif = ExifSpec {
            body_serial: None,
            ..ExifSpec::zf()
        };
        let nikon = NikonSpec {
            serial: Some(String::new()),
            ..NikonSpec::zf(None)
        };
        let file = nef_file(Order::Big, &exif, Some(&nikon), None, 0);
        assert_eq!(read_nef(&file).serial, None);
    }

    #[test]
    fn body_serial_wins_over_maker_note_serial() {
        let nikon = NikonSpec {
            serial: Some("7654321".to_owned()),
            ..NikonSpec::zf(None)
        };
        let file = nef_file(Order::Big, &ExifSpec::zf(), Some(&nikon), None, 0);
        assert_eq!(read_nef(&file).serial.as_deref(), Some("1234567"));
    }

    #[test]
    fn preview_range_decodes_to_preview_size_in_both_layouts_and_orders() {
        let preview = tiny_jpeg(640, 424);
        for order in [Order::Little, Order::Big] {
            for preview_inside in [false, true] {
                let nikon = NikonSpec {
                    preview_jpeg: Some(preview.clone()),
                    preview_inside,
                    ..NikonSpec::zf(None)
                };
                let file = nef_file(order, &ExifSpec::zf(), Some(&nikon), None, 0);
                let range = read_nef(&file).preview.expect("preview located");
                let bytes = slice(&file, range);
                assert_eq!(
                    bytes,
                    preview.as_slice(),
                    "order {order:?}, inside {preview_inside}"
                );
                assert_eq!(decoded_size(bytes), (640, 424));
            }
        }
    }

    #[test]
    fn header_other_than_type_3_is_wrong_format() {
        let head = b"Nikon\0\x01\x00\x00\x00\x00\x00\x00";
        let note = MakerNote {
            head,
            file_len: u64::try_from(head.len()).unwrap(),
            offset: 0,
            len: u64::try_from(head.len()).unwrap(),
            parent_order: crate::metadata::tiff::ByteOrder::Little,
        };
        assert_eq!(
            NikonMakerNotes.read(&note),
            Err(ParseError::WrongFormat {
                expected: "nikon type 3 maker note"
            })
        );
    }

    #[test]
    fn non_nikon_make_leaves_vendor_fields_absent_and_parse_succeeds() {
        let exif = ExifSpec {
            make: Some("ACME".to_owned()),
            body_serial: None,
            ..ExifSpec::zf()
        };
        let file = nef_file(Order::Big, &exif, Some(&NikonSpec::zf(None)), None, 0);
        let meta = read_nef(&file);
        assert_eq!(meta.shot_counter, None);
        assert_eq!(meta.serial, None);
        assert_eq!(meta.make.as_deref(), Some("ACME"));
    }

    #[test]
    fn type_1_header_leaves_vendor_fields_absent_and_parse_succeeds() {
        let mut file = nef_file(
            Order::Little,
            &ExifSpec::zf(),
            Some(&NikonSpec::zf(None)),
            None,
            0,
        );
        let header_at = file
            .windows(7)
            .position(|w| w == b"Nikon\0\x02")
            .expect("type 3 header is written");
        file[header_at + 6] = 0x01;
        let meta = read_nef(&file);
        assert_eq!(meta.shot_counter, None);
        assert_eq!(meta.serial.as_deref(), Some("1234567"));
    }
}
