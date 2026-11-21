//! A test-only TIFF-based format registered into the built-in registry.
//! Nothing in the library knows about it: it is added from outside, and
//! its files are identified by the same ladder as NEF.

use photosort_core::identity::identity_for;
use photosort_core::metadata::exif::read_tiff_exif;
use photosort_core::metadata::tiff::TIFF_MAGIC;
use photosort_core::metadata::{FormatReader, ParseInput, Registry};
use photosort_core::model::{FileKind, IdentityKey, Metadata, ParseError, Rational};
use photosort_testkit::{ExifSpec, NikonSpec, Order, nef_file, tiff_file};

const FAKE_KIND: FileKind = FileKind("fake");

/// A TIFF-based format that reads standard EXIF and maker notes like NEF.
struct FakeTiffReader;

impl FormatReader for FakeTiffReader {
    fn kind(&self) -> FileKind {
        FAKE_KIND
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["fkr"]
    }

    fn badge(&self) -> &'static str {
        "F"
    }

    fn is_raw(&self) -> bool {
        true
    }

    fn header_len_hint(&self) -> usize {
        128 * 1024
    }

    fn parse(&self, input: &ParseInput<'_>) -> Result<Metadata, ParseError> {
        read_tiff_exif(input, 0, &[TIFF_MAGIC]).map(|file| file.meta)
    }

    fn lens_write_supported(&self) -> bool {
        false
    }
}

/// Built-in readers plus the fake one, which is registered here and nowhere
/// in the library.
fn registry_with_fake() -> Registry {
    let mut reg = Registry::with_builtin();
    reg.register_format(Box::new(FakeTiffReader))
        .expect("fake reader has a distinct kind and extension");
    reg
}

/// The fake format's bytes: a TIFF with a standard Exif IFD and a Nikon note.
fn fake_file(exif: &ExifSpec) -> Vec<u8> {
    tiff_file(Order::Little, TIFF_MAGIC, exif, Some(&NikonSpec::zf(None)))
}

fn identity_of(reg: &Registry, kind: FileKind, file: &[u8]) -> IdentityKey {
    let len = u64::try_from(file.len()).unwrap();
    let meta = reg
        .parse(kind, file, len)
        .expect("fixture parses through the registry");
    identity_for(Some(&meta), len, file)
}

#[test]
fn fake_extension_is_found_in_any_case_after_registration() {
    let reg = registry_with_fake();
    let reader = reg.format_for_extension("FKR").expect("fkr is registered");
    assert_eq!(reader.kind(), FAKE_KIND);
    assert_eq!(reader.badge(), "F");
    assert!(reader.is_raw());
    assert!(!reader.lens_write_supported());
}

#[test]
fn fake_extension_is_unknown_without_registration() {
    assert!(
        Registry::with_builtin()
            .format_for_extension("FKR")
            .is_none()
    );
}

/// AC12.1: a fake TIFF file with the Nikon counter gets the same counter
/// identity as a NEF built from the same specs.
#[test]
fn fake_tiff_file_gets_the_same_counter_identity_as_a_nef() {
    let reg = registry_with_fake();
    let exif = ExifSpec::zf();
    let nikon = NikonSpec::zf(None);

    let fake = fake_file(&exif);
    let nef = nef_file(Order::Little, &exif, Some(&nikon), None, 0);

    let fake_key = identity_of(&reg, FAKE_KIND, &fake);
    assert_eq!(
        fake_key,
        IdentityKey::Counter {
            make: "NIKON CORPORATION".to_owned(),
            serial: "1234567".to_owned(),
            counter: 75036,
            dto: "2026:11:20 14:59:32".to_owned(),
            subsec: "92".to_owned(),
        }
    );
    assert_eq!(fake_key, identity_of(&reg, FileKind("nef"), &nef));
}

/// AC12.2: the same fake file with Make ACME has no maker-note reader, so it
/// falls to the capture rung.
#[test]
fn fake_tiff_file_with_unknown_make_falls_to_capture_rung() {
    let reg = registry_with_fake();
    let acme = ExifSpec {
        make: Some("ACME".to_owned()),
        ..ExifSpec::zf()
    };

    let key = identity_of(&reg, FAKE_KIND, &fake_file(&acme));
    assert_eq!(
        key,
        IdentityKey::Capture {
            make: "ACME".to_owned(),
            serial: "1234567".to_owned(),
            dto: "2026:11:20 14:59:32".to_owned(),
            subsec: "92".to_owned(),
            iso: 800,
            exposure: Rational { num: 1, den: 250 },
        }
    );
}
