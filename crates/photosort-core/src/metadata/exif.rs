//! Standard EXIF fields (IFD0 and the ExifIFD) read through the shared TIFF
//! walker, plus the maker-note fields reachable from the ExifIFD.
// pattern: Functional Core

use crate::metadata::registry::{MakerNote, ParseInput, VendorFields};
use crate::metadata::tiff::{Ifd, Tiff, optional, optional_tag};
use crate::model::{Metadata, ParseError};

const TAG_MAKE: u16 = 0x010F;
const TAG_MODEL: u16 = 0x0110;
const TAG_ORIENTATION: u16 = 0x0112;
const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_DATE_TIME_ORIGINAL: u16 = 0x9003;
const TAG_SUBSEC_TIME_ORIGINAL: u16 = 0x9291;
const TAG_BODY_SERIAL: u16 = 0xA431;
const TAG_LENS_MAKE: u16 = 0xA433;
const TAG_LENS_MODEL: u16 = 0xA434;
const TAG_ISO: u16 = 0x8827;
const TAG_EXPOSURE_TIME: u16 = 0x829A;
const TAG_F_NUMBER: u16 = 0x829D;
const TAG_FOCAL_LENGTH: u16 = 0x920A;
const TAG_MAKER_NOTE: u16 = 0x927C;

/// A parsed TIFF header with IFD0 and the metadata read from it.
pub struct TiffFile<'a> {
    pub tiff: Tiff<'a>,
    pub ifd0: Ifd,
    pub meta: Metadata,
}

/// Parses a TIFF header at `base`, then the EXIF fields reachable from IFD0.
///
/// Errors from the header and IFD0 propagate. Every optional tag goes
/// through `optional`, so a malformed field reads as absent while
/// `NeedMore` still reaches the caller.
pub fn read_tiff_exif<'a>(
    input: &ParseInput<'a>,
    base: u64,
    magic: &[u16],
) -> Result<TiffFile<'a>, ParseError> {
    let (tiff, ifd0_at) = Tiff::parse(input.head, input.file_len, base, magic)?;
    let ifd0 = tiff.read_ifd(ifd0_at)?;
    let mut meta = Metadata {
        make: text(&ifd0, TAG_MAKE, &tiff)?,
        model: text(&ifd0, TAG_MODEL, &tiff)?,
        orientation: optional_tag(&ifd0, TAG_ORIENTATION, |e| e.first_u32(&tiff))?
            .and_then(|v| u16::try_from(v).ok()),
        ..Metadata::default()
    };

    if let Some(exif) = read_exif_ifd(&ifd0, &tiff)? {
        read_exif_fields(&mut meta, &exif, &tiff)?;
        let vendor = vendor_fields(input, &meta, &exif, &tiff)?;
        if meta.serial.is_none() {
            meta.serial = vendor.serial;
        }
        meta.shot_counter = vendor.shot_counter;
        meta.preview = vendor.preview;
    }

    Ok(TiffFile { tiff, ifd0, meta })
}

/// A text tag, with an empty string read as absent.
fn text(ifd: &Ifd, tag: u16, tiff: &Tiff<'_>) -> Result<Option<String>, ParseError> {
    Ok(optional_tag(ifd, tag, |e| e.ascii(tiff))?.filter(|s| !s.is_empty()))
}

fn read_exif_ifd(ifd0: &Ifd, tiff: &Tiff<'_>) -> Result<Option<Ifd>, ParseError> {
    optional_tag(ifd0, TAG_EXIF_IFD, |e| {
        e.first_u32(tiff)
            .and_then(|rel| tiff.resolve(rel))
            .and_then(|at| tiff.read_ifd(at))
    })
}

fn read_exif_fields(meta: &mut Metadata, exif: &Ifd, tiff: &Tiff<'_>) -> Result<(), ParseError> {
    meta.date_time_original = text(exif, TAG_DATE_TIME_ORIGINAL, tiff)?;
    meta.subsec_time_original = text(exif, TAG_SUBSEC_TIME_ORIGINAL, tiff)?;
    meta.serial = text(exif, TAG_BODY_SERIAL, tiff)?;
    meta.lens_make = text(exif, TAG_LENS_MAKE, tiff)?;
    meta.lens_model = text(exif, TAG_LENS_MODEL, tiff)?;
    meta.iso = optional_tag(exif, TAG_ISO, |e| e.first_u32(tiff))?;
    meta.exposure_time = optional_tag(exif, TAG_EXPOSURE_TIME, |e| e.rational(tiff))?;
    meta.f_number = optional_tag(exif, TAG_F_NUMBER, |e| e.rational(tiff))?;
    meta.focal_length = optional_tag(exif, TAG_FOCAL_LENGTH, |e| e.rational(tiff))?;
    Ok(())
}

/// Vendor fields from the MakerNote, when the Make has a registered reader.
/// No reader, or a note that fails to parse, gives empty fields. `NeedMore`
/// propagates.
fn vendor_fields(
    input: &ParseInput<'_>,
    meta: &Metadata,
    exif: &Ifd,
    tiff: &Tiff<'_>,
) -> Result<VendorFields, ParseError> {
    let (Some(make), Some(entry)) = (meta.make.as_deref(), exif.get(TAG_MAKER_NOTE)) else {
        return Ok(VendorFields::default());
    };
    let Some(reader) = input.makernote_for(make) else {
        return Ok(VendorFields::default());
    };
    let Some((offset, len)) = optional(entry.location(tiff))? else {
        return Ok(VendorFields::default());
    };
    let note = MakerNote {
        head: tiff.head(),
        file_len: tiff.file_len(),
        offset,
        len,
        parent_order: tiff.order(),
    };
    Ok(optional(reader.read(&note))?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::registry::MakerNoteReader;
    use crate::model::Rational;
    use photosort_testkit::{ExifSpec, NikonSpec, Order, tiff_file};

    fn read(file: &[u8], makernotes: &[Box<dyn MakerNoteReader>]) -> Result<Metadata, ParseError> {
        let input = ParseInput {
            head: file,
            file_len: u64::try_from(file.len()).unwrap(),
            makernotes,
        };
        read_tiff_exif(&input, 0, &[42]).map(|f| f.meta)
    }

    fn expected_zf() -> Metadata {
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

    #[test]
    fn reads_every_standard_field_in_both_byte_orders() {
        for order in [Order::Little, Order::Big] {
            let file = tiff_file(order, 42, &ExifSpec::zf(), None);
            assert_eq!(read(&file, &[]).unwrap(), expected_zf(), "order {order:?}");
        }
    }

    macro_rules! omitted_tag_reads_as_none {
        ($($test:ident: $spec:ident => $meta:ident;)*) => {$(
            #[test]
            fn $test() {
                let mut spec = ExifSpec::zf();
                spec.$spec = None;
                let file = tiff_file(Order::Little, 42, &spec, None);
                assert_eq!(
                    read(&file, &[]).unwrap(),
                    Metadata { $meta: None, ..expected_zf() }
                );
            }
        )*};
    }

    omitted_tag_reads_as_none! {
        omitting_make_clears_only_make: make => make;
        omitting_model_clears_only_model: model => model;
        omitting_orientation_clears_only_orientation: orientation => orientation;
        omitting_body_serial_clears_only_serial: body_serial => serial;
        omitting_date_time_clears_only_date_time: date_time_original => date_time_original;
        omitting_subsec_clears_only_subsec: subsec_time_original => subsec_time_original;
        omitting_iso_clears_only_iso: iso => iso;
        omitting_exposure_clears_only_exposure: exposure_time => exposure_time;
        omitting_f_number_clears_only_f_number: f_number => f_number;
        omitting_focal_length_clears_only_focal_length: focal_length => focal_length;
        omitting_lens_make_clears_only_lens_make: lens_make => lens_make;
        omitting_lens_model_clears_only_lens_model: lens_model => lens_model;
    }

    #[test]
    fn maker_note_without_a_registered_reader_is_not_an_error() {
        let spec = ExifSpec {
            make: Some("ACME".to_owned()),
            ..ExifSpec::zf()
        };
        let file = tiff_file(Order::Big, 42, &spec, Some(&NikonSpec::zf(None)));
        let meta = read(&file, &[]).unwrap();
        assert_eq!(meta.make.as_deref(), Some("ACME"));
        assert_eq!(meta.shot_counter, None);
    }

    #[test]
    fn short_head_reports_need_more_instead_of_a_partial_read() {
        let file = tiff_file(Order::Little, 42, &ExifSpec::zf(), None);
        let input = ParseInput {
            head: &file[..20],
            file_len: u64::try_from(file.len()).unwrap(),
            makernotes: &[],
        };
        let err = read_tiff_exif(&input, 0, &[42]).err().unwrap();
        assert!(
            matches!(err, ParseError::NeedMore { have: 20, .. }),
            "got {err:?}"
        );
    }
}
