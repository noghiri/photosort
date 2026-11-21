//! Property tests for untrusted input: the built-in readers and the identity
//! ladder must never panic, every located range must lie inside the file, and
//! a `NeedMore` head must converge on the full parse.
//!
//! Case counts default to the phase plan's values. `PROPTEST_CASES`, when set,
//! overrides them, so a local run can go higher.

use std::sync::OnceLock;

use photosort_core::identity::identity_for;
use photosort_core::metadata::Registry;
use photosort_core::metadata::formats::{jpeg, nef};
use photosort_core::model::{ByteRange, FileKind, IdentityKey, Metadata, ParseError, Rational};
use photosort_testkit::{ExifSpec, NikonSpec, Order, jpeg_file, nef_file, tiny_jpeg};
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

/// Mutations aimed at the first bytes of a file also hit the headers, where
/// the structures the parsers walk live. Without this, most writes would land
/// in image data that no parser inspects.
const HEADER_SPAN: usize = 4096;

/// Steps allowed before a `NeedMore` chain is treated as non-convergent.
const MAX_CONVERGENCE_STEPS: usize = 64;

fn config(default_cases: u32) -> ProptestConfig {
    // `with_cases` would ignore PROPTEST_CASES, so the variable is checked here.
    let mut config = ProptestConfig::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = default_cases;
    }
    config
}

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Registry::with_builtin)
}

/// Valid files to mutate and truncate. Each kind appears in both byte orders.
/// Some carry the Nikon maker note and the MPF preview, so those code paths
/// are mutated too.
fn fixtures() -> &'static [(FileKind, Vec<u8>)] {
    static FIXTURES: OnceLock<Vec<(FileKind, Vec<u8>)>> = OnceLock::new();
    FIXTURES.get_or_init(build_fixtures)
}

fn build_fixtures() -> Vec<(FileKind, Vec<u8>)> {
    let exif = ExifSpec::zf();
    let preview = tiny_jpeg(640, 424);
    let jpg_from_raw = tiny_jpeg(1200, 800);
    let mut out = Vec::new();
    for order in [Order::Little, Order::Big] {
        out.push((
            jpeg::KIND,
            jpeg_file(
                order,
                Some(&exif),
                Some(&NikonSpec::zf(None)),
                Some(&preview),
            ),
        ));
        out.push((jpeg::KIND, jpeg_file(order, Some(&exif), None, None)));
        out.push((
            nef::KIND,
            nef_file(
                order,
                &exif,
                Some(&NikonSpec::zf(Some(preview.clone()))),
                Some(&jpg_from_raw),
                0,
            ),
        ));
        out.push((
            nef::KIND,
            nef_file(
                order,
                &exif,
                Some(&NikonSpec {
                    preview_inside: true,
                    ..NikonSpec::zf(Some(preview.clone()))
                }),
                None,
                0,
            ),
        ));
    }
    out
}

/// Every range the metadata locates must lie inside the file. `end()` also
/// panics on overflow, which would be caught as a failure too.
fn assert_ranges_within(meta: &Metadata, file_len: u64) {
    for range in [meta.preview, meta.full_jpeg].into_iter().flatten() {
        assert!(
            range.end() <= file_len,
            "range {range:?} extends past the file length {file_len}"
        );
    }
}

/// A fixture with 1-8 random byte overwrites and an optional truncation.
fn mutated_fixture() -> impl Strategy<Value = (FileKind, Vec<u8>)> {
    let count = fixtures().len();
    (
        0..count,
        prop::collection::vec(
            (any::<bool>(), any::<prop::sample::Index>(), any::<u8>()),
            1..=8,
        ),
        prop::option::of(any::<prop::sample::Index>()),
    )
        .prop_map(|(i, writes, cut)| {
            let (kind, original) = &fixtures()[i];
            let mut bytes = original.clone();
            for (in_header, at, value) in writes {
                let span = if in_header {
                    bytes.len().min(HEADER_SPAN)
                } else {
                    bytes.len()
                };
                bytes[at.index(span)] = value;
            }
            if let Some(cut) = cut {
                bytes.truncate(cut.index(bytes.len() + 1));
            }
            (*kind, bytes)
        })
}

fn arb_text() -> impl Strategy<Value = String> {
    // Mixes in `|` and `%` so the identity escape path is exercised.
    prop_oneof![".*", "[|%7C25a-c]*"]
}

fn opt_text() -> impl Strategy<Value = Option<String>> {
    prop::option::of(arb_text())
}

fn arb_rational() -> impl Strategy<Value = Rational> {
    (any::<u32>(), any::<u32>()).prop_map(|(num, den)| Rational { num, den })
}

fn arb_range() -> impl Strategy<Value = ByteRange> {
    (any::<u64>(), any::<u64>()).prop_map(|(offset, len)| ByteRange { offset, len })
}

/// Arbitrary metadata, including values no real file could hold, such as
/// out-of-range ranges and zero denominators.
fn arb_metadata() -> impl Strategy<Value = Metadata> {
    (
        (opt_text(), opt_text(), opt_text(), opt_text(), opt_text()),
        (
            prop::option::of(any::<u32>()),
            prop::option::of(arb_rational()),
            prop::option::of(arb_rational()),
            prop::option::of(arb_rational()),
        ),
        (opt_text(), opt_text(), prop::option::of(any::<u16>())),
        (
            prop::option::of(any::<u64>()),
            prop::option::of(arb_range()),
            prop::option::of(arb_range()),
        ),
    )
        .prop_map(
            |(
                (make, model, serial, date_time_original, subsec_time_original),
                (iso, exposure_time, f_number, focal_length),
                (lens_make, lens_model, orientation),
                (shot_counter, preview, full_jpeg),
            )| Metadata {
                make,
                model,
                serial,
                date_time_original,
                subsec_time_original,
                iso,
                exposure_time,
                f_number,
                focal_length,
                lens_make,
                lens_model,
                orientation,
                shot_counter,
                preview,
                full_jpeg,
            },
        )
}

proptest! {
    #![proptest_config(config(1024))]

    /// Random bytes under random file lengths: each reader returns `Ok` or
    /// `Err`, and any range it locates is inside the file.
    #[test]
    fn random_bytes_never_panic_and_ranges_stay_in_bounds(
        head in prop::collection::vec(any::<u8>(), 0..4096),
        extra in 0..=1_000_000u64,
    ) {
        let file_len = u64::try_from(head.len()).unwrap() + extra;
        for kind in [jpeg::KIND, nef::KIND] {
            if let Ok(meta) = registry().parse(kind, &head, file_len) {
                assert_ranges_within(&meta, file_len);
            }
        }
    }

    /// Valid files with 1-8 byte overwrites and an optional truncation. The
    /// file length is the length of the bytes that remain, as on disk.
    #[test]
    fn mutated_valid_files_never_panic_and_ranges_stay_in_bounds(
        (kind, bytes) in mutated_fixture(),
    ) {
        let file_len = u64::try_from(bytes.len()).unwrap();
        if let Ok(meta) = registry().parse(kind, &bytes, file_len) {
            assert_ranges_within(&meta, file_len);
        }
    }

    /// A valid file parsed from a head of random length `k` must converge.
    /// Each `NeedMore` must make progress and stay within the file. Once a
    /// head gives `Ok`, that result must equal the full parse.
    #[test]
    fn need_more_converges_to_the_full_parse(
        fixture in 0..fixtures().len(),
        start in any::<prop::sample::Index>(),
    ) {
        let (kind, file) = &fixtures()[fixture];
        let len = u64::try_from(file.len()).unwrap();
        let full = registry()
            .parse(*kind, file, len)
            .expect("a valid fixture parses from its full bytes");

        let mut k = start.index(file.len() + 1);
        let mut converged = None;
        for step in 0..MAX_CONVERGENCE_STEPS {
            let head = file.get(..k).expect("k is at most the file length");
            match registry().parse(*kind, head, len) {
                Ok(meta) => {
                    converged = Some(meta);
                    break;
                }
                Err(ParseError::NeedMore { needed, .. }) => {
                    prop_assert!(
                        needed > u64::try_from(k).unwrap(),
                        "step {step}: NeedMore {needed} does not pass head {k}"
                    );
                    prop_assert!(
                        needed <= len,
                        "step {step}: NeedMore {needed} exceeds file length {len}"
                    );
                    k = usize::try_from(needed).unwrap();
                }
                Err(other) => {
                    prop_assert!(false, "step {step}: head {k} gave {other:?}, not NeedMore");
                }
            }
        }
        prop_assert!(
            converged.is_some(),
            "no convergence within {MAX_CONVERGENCE_STEPS} steps, last head {k}"
        );
        prop_assert_eq!(converged, Some(full));
    }

    /// `identity_for` is total: no metadata, length or head makes it panic.
    /// Its key must also survive the string form that persisted state stores.
    #[test]
    fn identity_for_never_panics_and_its_key_round_trips(
        meta in prop::option::of(arb_metadata()),
        file_len in any::<u64>(),
        head in prop::collection::vec(any::<u8>(), 0..4096),
    ) {
        let key = identity_for(meta.as_ref(), file_len, &head);
        prop_assert_eq!(key.to_string().parse::<IdentityKey>(), Ok(key));
    }
}
