//! Complete synthetic JPEG, NEF and TIFF files.
// pattern: Functional Core

use image::ImageEncoder;
use image::codecs::jpeg::JpegEncoder;

use crate::spec::{ExifSpec, NikonSpec, Order};
use crate::tiff_writer::{IfdBuilder, TiffBytes, write_tiff, write_tiff_with_blobs};

const TIFF_MAGIC: u16 = 42;
const MAKERNOTE_TAG: u16 = 0x927C;
const EXIF_IFD_TAG: u16 = 0x8769;
/// Offset of the embedded TIFF header inside a Nikon maker note.
const NIKON_TIFF_BASE: u64 = 10;

/// A grayscale JPEG with a horizontal gradient.
#[must_use]
pub fn tiny_jpeg(width: u32, height: u32) -> Vec<u8> {
    let pixels: Vec<u8> = (0..width * height)
        .map(|i| ((i % width) * 255 / width.max(1)) as u8)
        .collect();
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 80)
        .write_image(&pixels, width, height, image::ExtendedColorType::L8)
        .expect("grayscale JPEG encodes");
    out
}

/// A Nikon type 3 maker note.
///
/// `preview_abs` is the absolute `(offset, len)` of the preview, used when
/// `spec.preview_inside` is false, and `mn_abs` is the note's absolute offset
/// in the file. Both are needed to aim the PreviewIFD at the preview. Without
/// a preview position, the PreviewIFD is omitted. With `preview_inside`, the
/// preview is appended to the note and the offsets are computed here.
#[must_use]
pub fn nikon_maker_note(
    order: Order,
    spec: &NikonSpec,
    preview_abs: Option<(u64, u64)>,
    mn_abs: u64,
) -> Vec<u8> {
    let preview = spec.preview_jpeg.as_deref();
    let mut note = b"Nikon\0".to_vec();
    note.extend([0x02, 0x10, 0x00, 0x00]);

    let Some(preview) = preview else {
        note.extend(embedded_tiff(order, spec, None));
        return note;
    };

    if spec.preview_inside {
        // Pass 1 finds the TIFF length, which is where the preview starts.
        let rel = embedded_tiff(order, spec, Some((0, 0))).len();
        let tiff = embedded_tiff(order, spec, Some((count(rel), count(preview.len()))));
        note.extend(tiff);
        note.extend(preview);
        return note;
    }

    let Some((preview_at, preview_len)) = preview_abs else {
        note.extend(embedded_tiff(order, spec, None));
        return note;
    };
    // The zero-position placeholder pass wraps here. Truncation is deliberate:
    // that pass only fixes the note's length, which does not depend on the value.
    // A real placement error is caught by the decoded-preview tests.
    let rel = preview_at.wrapping_sub(mn_abs + NIKON_TIFF_BASE) as u32;
    note.extend(embedded_tiff(
        order,
        spec,
        Some((rel, count_u64(preview_len))),
    ));
    note
}

/// A JPEG with optional Exif APP1, optional MPF APP2 and an optional
/// Nikon maker note inside the Exif block.
///
/// Without `exif`, no APP1 is written and `nikon` is ignored. The primary
/// image is `tiny_jpeg(16, 16)`. With `mpf_preview`, the preview is appended
/// after it and indexed from the MPF block.
///
/// The maker note's preview depends on `nikon.preview_inside`. When true, the
/// preview bytes sit inside the note and a PreviewIFD points at them, so
/// `nikon.preview_jpeg` is written. When false, no PreviewIFD is written:
/// a JPEG has no place for an outside preview, so `nikon.preview_jpeg` is
/// ignored in that case.
#[must_use]
pub fn jpeg_file(
    order: Order,
    exif: Option<&ExifSpec>,
    nikon: Option<&NikonSpec>,
    mpf_preview: Option<&[u8]>,
) -> Vec<u8> {
    jpeg_file_with_mpf_order(order, Order::Big, exif, nikon, mpf_preview)
}

/// As `jpeg_file`, with the MPF TIFF in `mpf_order`. `jpeg_file` writes a
/// big-endian MPF block; this lets a test write a little-endian one, and
/// with a byte order that differs from the Exif block's.
#[must_use]
pub fn jpeg_file_with_mpf_order(
    order: Order,
    mpf_order: Order,
    exif: Option<&ExifSpec>,
    nikon: Option<&NikonSpec>,
    mpf_preview: Option<&[u8]>,
) -> Vec<u8> {
    let body = tiny_jpeg(16, 16)[2..].to_vec();
    let app1 = exif
        .map(|e| segment(0xE1, &exif_payload(order, e, nikon)))
        .unwrap_or_default();

    let mut out = vec![0xFF, 0xD8];
    out.extend(&app1);

    if let Some(preview) = mpf_preview {
        // Pass 1: the MPF block's length does not depend on its offsets.
        let zero_block = mpf_block(mpf_order, 0, 0, 0);
        let app2_len = 2 + 2 + 4 + zero_block.len();
        let primary_len = 2 + app1.len() + app2_len + body.len();
        // Offsets are measured from the MM byte: APP2 marker, length and "MPF\0" come first.
        let mm_pos = 2 + app1.len() + 2 + 2 + 4;
        let rel = primary_len - mm_pos;
        let mut app2_payload = b"MPF\0".to_vec();
        app2_payload.extend(mpf_block(
            mpf_order,
            count(primary_len),
            count(preview.len()),
            count(rel),
        ));
        out.extend(segment(0xE2, &app2_payload));
        out.extend(&body);
        out.extend(preview);
        return out;
    }

    out.extend(&body);
    out
}

/// A NEF-like TIFF: a 1-pixel-high IFD0 with two SubIFDs (JpgFromRaw and a
/// fake 64-byte raw strip) and an Exif IFD.
///
/// `padding` zero bytes come before the blobs. The NEF preview, when
/// `preview_inside` is false, is placed after that padding, so a test can
/// push it past a head buffer.
#[must_use]
pub fn nef_file(
    order: Order,
    exif: &ExifSpec,
    nikon: Option<&NikonSpec>,
    jpg_from_raw: Option<&[u8]>,
    padding: usize,
) -> Vec<u8> {
    let raw = vec![0u8; 64];
    let free_preview: Vec<Vec<u8>> = match nikon {
        Some(n) if !n.preview_inside => n.preview_jpeg.iter().cloned().collect(),
        _ => Vec::new(),
    };

    // Pass 1: the maker note's length is fixed, so the layout is stable.
    let mn_zero = nikon.map(|n| nikon_maker_note(order, n, Some((0, 0)), 0));
    let first = write_tiff_with_blobs(
        order,
        TIFF_MAGIC,
        &nef_ifd0(exif, jpg_from_raw, &raw, mn_zero.as_deref()),
        padding,
        &free_preview,
    );
    // Without a maker note there is no MakerNote value to locate, so the
    // layout is final after pass 1.
    let mn_at = nikon.map(|_| maker_note_offset(&first));
    let preview_abs = if free_preview.is_empty() {
        None
    } else {
        let at = first.blob_offsets.last().copied().unwrap_or(0);
        Some((count_u64_usize(at), count_u64_usize(free_preview[0].len())))
    };

    // Pass 2: the real maker note, now that the preview's position is known.
    let mn = match (nikon, mn_at) {
        (Some(n), Some(at)) => Some(nikon_maker_note(order, n, preview_abs, at)),
        _ => None,
    };
    let last = write_tiff_with_blobs(
        order,
        TIFF_MAGIC,
        &nef_ifd0(exif, jpg_from_raw, &raw, mn.as_deref()),
        padding,
        &free_preview,
    );
    if let Some(at) = mn_at {
        debug_assert_eq!(maker_note_offset(&last), at);
    }
    last.bytes
}

/// A minimal TIFF with IFD0 and an Exif IFD, for non-JPEG, non-NEF formats.
/// The maker note follows the same `preview_inside` rule as `jpeg_file`.
#[must_use]
pub fn tiff_file(order: Order, magic: u16, exif: &ExifSpec, nikon: Option<&NikonSpec>) -> Vec<u8> {
    let mn = nikon.map(|n| nikon_maker_note(order, n, None, 0));
    let ifd0 = ifd0_common(exif).child(EXIF_IFD_TAG, exif_ifd(exif, mn.as_deref()));
    write_tiff(order, magic, &ifd0, 0).bytes
}

/// "Exif\0\0" followed by the TIFF of an Exif APP1 block.
fn exif_payload(order: Order, exif: &ExifSpec, nikon: Option<&NikonSpec>) -> Vec<u8> {
    let mn = nikon.map(|n| nikon_maker_note(order, n, None, 0));
    let ifd0 = ifd0_common(exif).child(EXIF_IFD_TAG, exif_ifd(exif, mn.as_deref()));
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend(write_tiff(order, TIFF_MAGIC, &ifd0, 0).bytes);
    payload
}

/// A JPEG marker segment with its big-endian length.
fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
    let len = u16::try_from(payload.len() + 2).expect("segment fits in u16");
    let mut out = vec![0xFF, marker];
    out.extend(len.to_be_bytes());
    out.extend(payload);
    out
}

/// The MPF TIFF in `order`. Its primary entry names the primary image
/// and its preview entry names the appended preview at `preview_rel`.
fn mpf_block(order: Order, primary_len: u32, preview_len: u32, preview_rel: u32) -> Vec<u8> {
    // Each MP entry is 16 bytes (phase plan Task 7: N x 16-byte entries in a
    // 32-byte 0xB002 for two images): attribute, size, offset, then two
    // 16-bit dependent-image offsets, which are zero here.
    let mut index = Vec::with_capacity(32);
    for v in [0x0003_0000, primary_len, 0, 0] {
        index.extend(enc32(order, v));
    }
    for v in [0x0001_0001, preview_len, preview_rel, 0] {
        index.extend(enc32(order, v));
    }
    let ifd0 = IfdBuilder::new()
        .undefined(0xB000, b"0100")
        .long(0xB001, &[2])
        .undefined(0xB002, &index);
    write_tiff(order, TIFF_MAGIC, &ifd0, 0).bytes
}

fn enc32(order: Order, v: u32) -> [u8; 4] {
    match order {
        Order::Little => v.to_le_bytes(),
        Order::Big => v.to_be_bytes(),
    }
}

/// Standard IFD0 fields shared by every file kind.
fn ifd0_common(exif: &ExifSpec) -> IfdBuilder {
    let mut ifd = IfdBuilder::new();
    if let Some(make) = &exif.make {
        ifd = ifd.ascii(0x010F, make);
    }
    if let Some(model) = &exif.model {
        ifd = ifd.ascii(0x0110, model);
    }
    if let Some(orientation) = exif.orientation {
        ifd = ifd.short(0x0112, &[orientation]);
    }
    ifd
}

/// The Exif IFD for `exif`, plus MakerNote when given.
fn exif_ifd(exif: &ExifSpec, makernote: Option<&[u8]>) -> IfdBuilder {
    let mut ifd = IfdBuilder::new();
    if let Some(v) = &exif.date_time_original {
        ifd = ifd.ascii(0x9003, v);
    }
    if let Some(v) = &exif.subsec_time_original {
        ifd = ifd.ascii(0x9291, v);
    }
    if let Some(v) = &exif.body_serial {
        ifd = ifd.ascii(0xA431, v);
    }
    if let Some(v) = exif.iso {
        ifd = ifd.short(0x8827, &[v]);
    }
    if let Some(v) = exif.exposure_time {
        ifd = ifd.rational(0x829A, &[v]);
    }
    if let Some(v) = exif.f_number {
        ifd = ifd.rational(0x829D, &[v]);
    }
    if let Some(v) = exif.focal_length {
        ifd = ifd.rational(0x920A, &[v]);
    }
    if let Some(v) = exif.max_aperture {
        ifd = ifd.rational(0x9205, &[v]);
    }
    if let Some(v) = exif.subject_distance {
        ifd = ifd.rational(0x9206, &[v]);
    }
    if let Some(v) = exif.lens_info {
        ifd = ifd.rational(0xA432, &v);
    }
    if let Some(v) = &exif.lens_make {
        ifd = ifd.ascii(0xA433, v);
    }
    if let Some(v) = &exif.lens_model {
        ifd = ifd.ascii(0xA434, v);
    }
    if let Some(note) = makernote {
        ifd = ifd.undefined(MAKERNOTE_TAG, note);
    }
    ifd
}

/// IFD0 for the NEF: the common fields, two SubIFDs and the Exif IFD.
/// SubIFD0 keeps its compression tag but omits the JPEG pointers when
/// `jpg_from_raw` is None.
fn nef_ifd0(
    exif: &ExifSpec,
    jpg_from_raw: Option<&[u8]>,
    raw: &[u8],
    makernote: Option<&[u8]>,
) -> IfdBuilder {
    let mut sub0 = IfdBuilder::new().long(0x00FE, &[0]).short(0x0103, &[6]);
    if let Some(jpg) = jpg_from_raw {
        sub0 = sub0.blob_offset(0x0201, jpg).blob_len(0x0202, jpg);
    }
    let sub1 = IfdBuilder::new()
        .long(0x00FE, &[0])
        .short(0x0103, &[1])
        .blob_offset(0x0111, raw)
        .blob_len(0x0117, raw);
    ifd0_common(exif)
        .long(0x00FE, &[1])
        .children(0x014A, vec![sub0, sub1])
        .child(EXIF_IFD_TAG, exif_ifd(exif, makernote))
}

/// Absolute offset of the MakerNote value in a serialized TIFF.
fn maker_note_offset(tiff: &TiffBytes) -> u64 {
    let span = tiff
        .values
        .iter()
        .find(|v| v.tag == MAKERNOTE_TAG)
        .expect("maker note is present");
    count_u64_usize(span.offset)
}

/// The embedded TIFF of a Nikon maker note, with an optional PreviewIFD
/// holding the `(offset, len)` pair relative to this TIFF's header.
fn embedded_tiff(order: Order, spec: &NikonSpec, preview: Option<(u32, u32)>) -> Vec<u8> {
    let mut ifd0 = IfdBuilder::new();
    if let Some(serial) = &spec.serial {
        ifd0 = ifd0.ascii(0x001D, serial);
    }
    if let Some(mechanical) = spec.mechanical_count {
        ifd0 = ifd0.long(0x0037, &[mechanical]);
    }
    if let Some(shutter) = spec.shutter_count {
        ifd0 = ifd0.long(0x00A7, &[shutter]);
    }
    if let Some((offset, len)) = preview {
        let preview_ifd = IfdBuilder::new()
            .long(0x0201, &[offset])
            .long(0x0202, &[len]);
        ifd0 = ifd0.child(0x0011, preview_ifd);
    }
    write_tiff(order, TIFF_MAGIC, &ifd0, 0).bytes
}

fn count(n: usize) -> u32 {
    u32::try_from(n).expect("value fits in u32")
}

fn count_u64(n: u64) -> u32 {
    u32::try_from(n).expect("value fits in u32")
}

fn count_u64_usize(n: usize) -> u64 {
    u64::try_from(n).expect("usize fits in u64")
}
