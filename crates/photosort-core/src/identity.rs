//! Identity keys: the counter, capture and content rungs of the identity
//! ladder, and the string form that persisted state stores.
// pattern: Functional Core

use std::fmt;
use std::str::FromStr;

use crate::model::{IdentityKey, Metadata, Rational};

/// Bytes from file start hashed for the content rung.
pub const CONTENT_HEAD_LEN: usize = 64 * 1024;

/// `head` must be the first bytes of the file; at most
/// `CONTENT_HEAD_LEN` of them are hashed. The file name is deliberately
/// not an input, so renaming cannot change identity.
///
/// Lens and aperture fields are not inputs either, because Manual profiles
/// overwrite them on copies.
#[must_use]
pub fn identity_for(meta: Option<&Metadata>, file_len: u64, head: &[u8]) -> IdentityKey {
    meta.and_then(counter_key)
        .or_else(|| meta.and_then(capture_key))
        .unwrap_or_else(|| content_key(file_len, head))
}

/// A string field that is present and not empty.
fn present(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn subsec(meta: &Metadata) -> String {
    present(&meta.subsec_time_original).unwrap_or_default()
}

fn counter_key(meta: &Metadata) -> Option<IdentityKey> {
    Some(IdentityKey::Counter {
        make: present(&meta.make)?,
        serial: present(&meta.serial)?,
        counter: meta.shot_counter?,
        dto: present(&meta.date_time_original)?,
        subsec: subsec(meta),
    })
}

fn capture_key(meta: &Metadata) -> Option<IdentityKey> {
    let exposure = meta.exposure_time.filter(|e| e.den != 0)?;
    Some(IdentityKey::Capture {
        make: present(&meta.make)?,
        serial: present(&meta.serial).unwrap_or_default(),
        dto: present(&meta.date_time_original)?,
        subsec: subsec(meta),
        iso: meta.iso?,
        exposure: exposure.reduced(),
    })
}

fn content_key(file_len: u64, head: &[u8]) -> IdentityKey {
    // A head shorter than the limit is hashed whole.
    let hashed = head.get(..CONTENT_HEAD_LEN).unwrap_or(head);
    IdentityKey::Content {
        size: file_len,
        head_blake3: *blake3::hash(hashed).as_bytes(),
    }
}

/// Why a string is not an identity key.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdentityParseError {
    #[error("identity key has {found} fields, expected {expected}")]
    FieldCount { expected: usize, found: usize },
    #[error("unknown identity key prefix {0:?}")]
    UnknownPrefix(String),
    #[error("invalid {field} in identity key")]
    BadNumber { field: &'static str },
    #[error("invalid content digest in identity key")]
    BadHex,
    #[error("invalid escape sequence in identity key")]
    BadEscape,
}

/// `|` separates fields, so string fields escape `%` and `|`. `%` goes first,
/// so the `%` that `|` introduces is never escaped twice.
fn escape(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    for c in field.chars() {
        match c {
            '%' => out.push_str("%25"),
            '|' => out.push_str("%7C"),
            other => out.push(other),
        }
    }
    out
}

/// Accepts only the two escapes that `escape` writes. Anything else,
/// including a trailing `%`, is an error.
fn unescape(field: &str) -> Result<String, IdentityParseError> {
    let mut parts = field.split('%');
    let mut out = String::with_capacity(field.len());
    out.push_str(parts.next().unwrap_or_default());
    for part in parts {
        if let Some(rest) = part.strip_prefix("25") {
            out.push('%');
            out.push_str(rest);
        } else if let Some(rest) = part.strip_prefix("7C") {
            out.push('|');
            out.push_str(rest);
        } else {
            return Err(IdentityParseError::BadEscape);
        }
    }
    Ok(out)
}

/// An unsigned decimal exactly as `Display` writes it: ASCII digits only, and
/// no leading zero unless the value is `0`. `str::parse` alone would accept
/// `+5` and `005`, which would give two strings for one key.
fn number<T: FromStr>(text: &str, field: &'static str) -> Result<T, IdentityParseError> {
    let canonical = !text.is_empty()
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    if !canonical {
        return Err(IdentityParseError::BadNumber { field });
    }
    text.parse()
        .map_err(|_| IdentityParseError::BadNumber { field })
}

/// `num/den`, both unsigned. A zero denominator is kept, because the parser
/// does not judge the value; `identity_for` never produces one.
fn exposure(text: &str) -> Result<Rational, IdentityParseError> {
    let (num, den) = text
        .split_once('/')
        .ok_or(IdentityParseError::BadNumber { field: "exposure" })?;
    Ok(Rational {
        num: number(num, "exposure")?,
        den: number(den, "exposure")?,
    })
}

/// Lowercase hex only, matching what `Display` writes.
fn digest(hex: &str) -> Result<[u8; 32], IdentityParseError> {
    let digits = hex.as_bytes();
    let valid = digits.len() == 64
        && digits
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b));
    if !valid {
        return Err(IdentityParseError::BadHex);
    }
    let mut out = [0u8; 32];
    let (pairs, _) = digits.as_chunks::<2>();
    for (slot, [hi, lo]) in out.iter_mut().zip(pairs) {
        *slot = (hex_value(*hi) << 4) | hex_value(*lo);
    }
    Ok(out)
}

/// Only called on bytes that `digest` has already checked.
fn hex_value(digit: u8) -> u8 {
    match digit {
        b'0'..=b'9' => digit - b'0',
        _ => digit - b'a' + 10,
    }
}

impl fmt::Display for IdentityKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Counter {
                make,
                serial,
                counter,
                dto,
                subsec,
            } => write!(
                f,
                "ctr|{}|{}|{counter}|{}|{}",
                escape(make),
                escape(serial),
                escape(dto),
                escape(subsec)
            ),
            Self::Capture {
                make,
                serial,
                dto,
                subsec,
                iso,
                exposure,
            } => write!(
                f,
                "cap|{}|{}|{}|{}|{iso}|{}/{}",
                escape(make),
                escape(serial),
                escape(dto),
                escape(subsec),
                exposure.num,
                exposure.den
            ),
            Self::Content { size, head_blake3 } => {
                write!(f, "cnt|{size}|")?;
                for byte in head_blake3 {
                    write!(f, "{byte:02x}")?;
                }
                Ok(())
            }
        }
    }
}

impl FromStr for IdentityKey {
    type Err = IdentityParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let fields: Vec<&str> = s.split('|').collect();
        let prefix = fields.first().copied().unwrap_or_default();
        let expected = match prefix {
            "ctr" => 6,
            "cap" => 7,
            "cnt" => 3,
            other => return Err(IdentityParseError::UnknownPrefix(other.to_owned())),
        };
        if fields.len() != expected {
            return Err(IdentityParseError::FieldCount {
                expected,
                found: fields.len(),
            });
        }
        match fields.as_slice() {
            ["ctr", make, serial, counter, dto, subsec] => Ok(IdentityKey::Counter {
                make: unescape(make)?,
                serial: unescape(serial)?,
                counter: number(counter, "counter")?,
                dto: unescape(dto)?,
                subsec: unescape(subsec)?,
            }),
            ["cap", make, serial, dto, subsec, iso, exposure_text] => Ok(IdentityKey::Capture {
                make: unescape(make)?,
                serial: unescape(serial)?,
                dto: unescape(dto)?,
                subsec: unescape(subsec)?,
                iso: number(iso, "iso")?,
                exposure: exposure(exposure_text)?,
            }),
            ["cnt", size, hex] => Ok(IdentityKey::Content {
                size: number(size, "size")?,
                head_blake3: digest(hex)?,
            }),
            _ => Err(IdentityParseError::UnknownPrefix(prefix.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::formats::{jpeg, nef};
    use crate::metadata::registry::Registry;
    use crate::model::FileKind;
    use photosort_testkit::{ExifSpec, NikonSpec, Order, jpeg_file, nef_file};
    use proptest::prelude::*;

    const ZF_DTO: &str = "2026:11:20 14:59:32";

    /// Identity of a file, parsed through the built-in registry.
    fn key_of(kind: FileKind, file: &[u8]) -> IdentityKey {
        let len = u64::try_from(file.len()).unwrap();
        let meta = Registry::with_builtin()
            .parse(kind, file, len)
            .expect("fixture parses");
        identity_for(Some(&meta), len, file)
    }

    fn nef_bytes(exif: &ExifSpec, nikon: Option<&NikonSpec>) -> Vec<u8> {
        nef_file(Order::Little, exif, nikon, None, 0)
    }

    fn nef_key(exif: &ExifSpec, nikon: Option<&NikonSpec>) -> IdentityKey {
        key_of(nef::KIND, &nef_bytes(exif, nikon))
    }

    fn zf_nikon() -> NikonSpec {
        NikonSpec::zf(None)
    }

    fn zf_nikon_without_counter() -> NikonSpec {
        NikonSpec {
            shutter_count: None,
            ..NikonSpec::zf(None)
        }
    }

    /// AC4.3: a NEF whose Nikon note carries ShutterCount gets the counter rung.
    #[test]
    fn nef_with_shutter_count_uses_counter_rung() {
        let key = nef_key(&ExifSpec::zf(), Some(&zf_nikon()));
        assert_eq!(
            key,
            IdentityKey::Counter {
                make: "NIKON CORPORATION".to_owned(),
                serial: "1234567".to_owned(),
                counter: 75036,
                dto: ZF_DTO.to_owned(),
                subsec: "92".to_owned(),
            }
        );
    }

    /// AC4.3: the same NEF without ShutterCount falls to the capture rung.
    #[test]
    fn nef_without_shutter_count_uses_capture_rung() {
        let key = nef_key(&ExifSpec::zf(), Some(&zf_nikon_without_counter()));
        assert_eq!(
            key,
            IdentityKey::Capture {
                make: "NIKON CORPORATION".to_owned(),
                serial: "1234567".to_owned(),
                dto: ZF_DTO.to_owned(),
                subsec: "92".to_owned(),
                iso: 800,
                exposure: Rational { num: 1, den: 250 },
            }
        );
    }

    /// AC4.3: a JPEG with no EXIF has no usable metadata and gets the content rung.
    #[test]
    fn jpeg_without_exif_uses_content_rung() {
        let file = jpeg_file(Order::Little, None, None, None);
        assert_eq!(
            key_of(jpeg::KIND, &file),
            IdentityKey::Content {
                size: u64::try_from(file.len()).unwrap(),
                head_blake3: *blake3::hash(&file).as_bytes(),
            }
        );
    }

    /// AC4.3: with no metadata at all, `identity_for` uses the content rung.
    #[test]
    fn identity_for_none_uses_content_rung() {
        let head = b"not a photo";
        let len = u64::try_from(head.len()).unwrap();
        assert_eq!(
            identity_for(None, len, head),
            IdentityKey::Content {
                size: len,
                head_blake3: *blake3::hash(head).as_bytes(),
            }
        );
    }

    /// The content rung hashes at most `CONTENT_HEAD_LEN` bytes, so a change
    /// past that point does not change the key, and a change before it does.
    #[test]
    fn content_rung_hashes_only_the_first_content_head_len_bytes() {
        let mut head = vec![0u8; CONTENT_HEAD_LEN + 10];
        let len = u64::try_from(head.len()).unwrap();
        let base = identity_for(None, len, &head);

        head[CONTENT_HEAD_LEN + 5] = 0xFF;
        assert_eq!(identity_for(None, len, &head), base);

        head[3] = 0xFF;
        assert_ne!(identity_for(None, len, &head), base);
    }

    /// Renaming is covered by construction: `identity_for` takes no file name,
    /// so there is no rename case to write. The fields below are the ones that
    /// AC4.4 says must not change the key.
    fn lens_and_aperture_edits() -> Vec<(&'static str, ExifSpec)> {
        vec![
            (
                "LensMake",
                ExifSpec {
                    lens_make: Some("SIGMA".to_owned()),
                    ..ExifSpec::zf()
                },
            ),
            (
                "LensModel",
                ExifSpec {
                    lens_model: Some("SIGMA 35mm F1.4".to_owned()),
                    ..ExifSpec::zf()
                },
            ),
            (
                "LensInfo",
                ExifSpec {
                    lens_info: Some([(35, 1), (35, 1), (14, 10), (14, 10)]),
                    ..ExifSpec::zf()
                },
            ),
            (
                "FNumber",
                ExifSpec {
                    f_number: Some((56, 10)),
                    ..ExifSpec::zf()
                },
            ),
            (
                "FocalLength",
                ExifSpec {
                    focal_length: Some((85, 1)),
                    ..ExifSpec::zf()
                },
            ),
            (
                "MaxApertureValue",
                ExifSpec {
                    max_aperture: Some((140, 100)),
                    ..ExifSpec::zf()
                },
            ),
            (
                "SubjectDistance",
                ExifSpec {
                    subject_distance: Some((12, 10)),
                    ..ExifSpec::zf()
                },
            ),
        ]
    }

    /// AC4.4, counter rung. Renaming is covered by construction (no name input).
    /// Each case changes one lens or aperture field and must leave the key equal.
    #[test]
    fn lens_and_aperture_fields_leave_the_counter_key_unchanged() {
        let nikon = zf_nikon();
        let base_bytes = nef_bytes(&ExifSpec::zf(), Some(&nikon));
        let base = key_of(nef::KIND, &base_bytes);
        assert!(matches!(base, IdentityKey::Counter { .. }));

        for (field, edited) in lens_and_aperture_edits() {
            let edited_bytes = nef_bytes(&edited, Some(&nikon));
            assert_ne!(edited_bytes, base_bytes, "{field} edit changed no bytes");
            assert_eq!(
                key_of(nef::KIND, &edited_bytes),
                base,
                "changing {field} changed the counter key"
            );
        }
    }

    /// AC4.4, capture rung. Renaming is covered by construction (no name input).
    /// Each case changes one lens or aperture field and must leave the key equal.
    #[test]
    fn lens_and_aperture_fields_leave_the_capture_key_unchanged() {
        let nikon = zf_nikon_without_counter();
        let base_bytes = nef_bytes(&ExifSpec::zf(), Some(&nikon));
        let base = key_of(nef::KIND, &base_bytes);
        assert!(matches!(base, IdentityKey::Capture { .. }));

        for (field, edited) in lens_and_aperture_edits() {
            let edited_bytes = nef_bytes(&edited, Some(&nikon));
            assert_ne!(edited_bytes, base_bytes, "{field} edit changed no bytes");
            assert_eq!(
                key_of(nef::KIND, &edited_bytes),
                base,
                "changing {field} changed the capture key"
            );
        }
    }

    /// AC4.5: the JPEG and NEF of one capture share the counter key.
    #[test]
    fn jpeg_and_nef_of_one_capture_share_the_counter_key() {
        for order in [Order::Little, Order::Big] {
            let nikon = zf_nikon();
            let jpeg = jpeg_file(order, Some(&ExifSpec::zf()), Some(&nikon), None);
            let nef = nef_file(order, &ExifSpec::zf(), Some(&nikon), None, 0);
            let jpeg_key = key_of(jpeg::KIND, &jpeg);
            assert!(matches!(jpeg_key, IdentityKey::Counter { .. }), "{order:?}");
            assert_eq!(jpeg_key, key_of(nef::KIND, &nef), "{order:?}");
        }
    }

    /// AC4.5: the NEF stores exposure as 10/2500 and the JPEG as 1/250. Both
    /// must still share the capture key, because the key reduces the fraction.
    #[test]
    fn jpeg_and_nef_share_the_capture_key_with_nef_exposure_as_10_over_2500() {
        let nikon = zf_nikon_without_counter();
        let nef_exif = ExifSpec {
            exposure_time: Some((10, 2500)),
            ..ExifSpec::zf()
        };
        let nef = nef_file(Order::Little, &nef_exif, Some(&nikon), None, 0);

        let meta = Registry::with_builtin()
            .parse(nef::KIND, &nef, u64::try_from(nef.len()).unwrap())
            .expect("NEF fixture parses");
        assert_eq!(meta.exposure_time, Some(Rational { num: 10, den: 2500 }));

        let jpeg = jpeg_file(Order::Little, Some(&ExifSpec::zf()), Some(&nikon), None);
        let jpeg_key = key_of(jpeg::KIND, &jpeg);
        assert!(matches!(jpeg_key, IdentityKey::Capture { .. }));
        assert_eq!(jpeg_key, key_of(nef::KIND, &nef));
    }

    /// AC12.2: a Make with no maker-note reader (ACME here, with a Nikon note
    /// present) falls to the capture rung without error.
    #[test]
    fn make_without_maker_note_reader_falls_to_capture_rung() {
        let acme = ExifSpec {
            make: Some("ACME".to_owned()),
            ..ExifSpec::zf()
        };
        let key = nef_key(&acme, Some(&zf_nikon()));
        assert_eq!(
            key,
            IdentityKey::Capture {
                make: "ACME".to_owned(),
                serial: "1234567".to_owned(),
                dto: ZF_DTO.to_owned(),
                subsec: "92".to_owned(),
                iso: 800,
                exposure: Rational { num: 1, den: 250 },
            }
        );
    }

    /// AC12.2: a Make with no maker-note reader and no DateTimeOriginal falls
    /// to the content rung without error.
    #[test]
    fn make_without_reader_and_without_dto_falls_to_content_rung() {
        let acme = ExifSpec {
            make: Some("ACME".to_owned()),
            date_time_original: None,
            ..ExifSpec::zf()
        };
        let file = jpeg_file(Order::Little, Some(&acme), None, None);
        assert!(matches!(
            key_of(jpeg::KIND, &file),
            IdentityKey::Content { .. }
        ));
    }

    #[test]
    fn display_joins_fields_with_pipes_in_the_documented_form() {
        let key = IdentityKey::Counter {
            make: "NIKON CORPORATION".to_owned(),
            serial: "1234567".to_owned(),
            counter: 75036,
            dto: ZF_DTO.to_owned(),
            subsec: "92".to_owned(),
        };
        assert_eq!(
            key.to_string(),
            "ctr|NIKON CORPORATION|1234567|75036|2026:11:20 14:59:32|92"
        );
    }

    #[test]
    fn empty_strings_round_trip() {
        let key = IdentityKey::Counter {
            make: String::new(),
            serial: String::new(),
            counter: 0,
            dto: String::new(),
            subsec: String::new(),
        };
        assert_eq!(key.to_string(), "ctr|||0||");
        assert_eq!(key.to_string().parse::<IdentityKey>(), Ok(key));
    }

    #[test]
    fn pipe_percent_and_literal_percent_7c_round_trip() {
        let key = IdentityKey::Counter {
            make: "A|B%C%7C".to_owned(),
            serial: "%".to_owned(),
            counter: 1,
            dto: "|".to_owned(),
            subsec: "%7C".to_owned(),
        };
        let text = key.to_string();
        assert_eq!(text, "ctr|A%7CB%25C%257C|%25|1|%7C|%257C");
        assert_eq!(text.parse::<IdentityKey>(), Ok(key));
    }

    fn arb_text() -> impl Strategy<Value = String> {
        prop_oneof![".*", "[|%7C25a-c]*"]
    }

    fn arb_key() -> impl Strategy<Value = IdentityKey> {
        prop_oneof![
            (arb_text(), arb_text(), any::<u64>(), arb_text(), arb_text()).prop_map(
                |(make, serial, counter, dto, subsec)| IdentityKey::Counter {
                    make,
                    serial,
                    counter,
                    dto,
                    subsec,
                }
            ),
            (
                arb_text(),
                arb_text(),
                arb_text(),
                arb_text(),
                any::<u32>(),
                (any::<u32>(), any::<u32>()),
            )
                .prop_map(|(make, serial, dto, subsec, iso, (num, den))| {
                    IdentityKey::Capture {
                        make,
                        serial,
                        dto,
                        subsec,
                        iso,
                        exposure: Rational { num, den },
                    }
                }),
            (any::<u64>(), prop::array::uniform32(any::<u8>()))
                .prop_map(|(size, head_blake3)| IdentityKey::Content { size, head_blake3 }),
        ]
    }

    proptest! {
        #[test]
        fn display_then_from_str_round_trips(key in arb_key()) {
            prop_assert_eq!(key.to_string().parse::<IdentityKey>(), Ok(key));
        }
    }

    #[test]
    fn from_str_rejects_wrong_field_count() {
        assert_eq!(
            "ctr|a|b|1|c".parse::<IdentityKey>(),
            Err(IdentityParseError::FieldCount {
                expected: 6,
                found: 5,
            })
        );
        assert_eq!(
            "cnt|1|00|extra".parse::<IdentityKey>(),
            Err(IdentityParseError::FieldCount {
                expected: 3,
                found: 4,
            })
        );
    }

    #[test]
    fn from_str_rejects_bad_number() {
        assert_eq!(
            "ctr|a|b|notanumber|c|d".parse::<IdentityKey>(),
            Err(IdentityParseError::BadNumber { field: "counter" })
        );
        assert_eq!(
            "cap|a|b|c|d|800|1-250".parse::<IdentityKey>(),
            Err(IdentityParseError::BadNumber { field: "exposure" })
        );
    }

    /// `Display` never writes a sign or a leading zero, and `FromStr` must not
    /// accept either, or two strings would name one key.
    #[test]
    fn from_str_rejects_signed_and_zero_padded_numbers() {
        let digest = "0".repeat(64);
        for text in ["+5", "005", "-5", "00", ""] {
            assert_eq!(
                format!("cnt|{text}|{digest}").parse::<IdentityKey>(),
                Err(IdentityParseError::BadNumber { field: "size" }),
                "size {text:?}"
            );
            assert_eq!(
                format!("ctr|a|b|{text}|c|d").parse::<IdentityKey>(),
                Err(IdentityParseError::BadNumber { field: "counter" }),
                "counter {text:?}"
            );
        }
    }

    #[test]
    fn from_str_rejects_signed_and_zero_padded_capture_fields() {
        for iso in ["+800", "0800"] {
            assert_eq!(
                format!("cap|a|b|c|d|{iso}|1/250").parse::<IdentityKey>(),
                Err(IdentityParseError::BadNumber { field: "iso" }),
                "iso {iso:?}"
            );
        }
        for exposure in ["+1/250", "01/250", "1/+250", "1/0250"] {
            assert_eq!(
                format!("cap|a|b|c|d|800|{exposure}").parse::<IdentityKey>(),
                Err(IdentityParseError::BadNumber { field: "exposure" }),
                "exposure {exposure:?}"
            );
        }
    }

    #[test]
    fn from_str_accepts_zero_as_a_canonical_number() {
        assert!(
            format!("cnt|0|{}", "0".repeat(64))
                .parse::<IdentityKey>()
                .is_ok()
        );
        assert_eq!(
            "ctr|a|b|0|c|d".parse::<IdentityKey>(),
            Ok(IdentityKey::Counter {
                make: "a".to_owned(),
                serial: "b".to_owned(),
                counter: 0,
                dto: "c".to_owned(),
                subsec: "d".to_owned(),
            })
        );
    }

    #[test]
    fn from_str_rejects_bad_hex() {
        let digits = "0".repeat(64);
        let uppercase = "A".repeat(64);
        let short = "0".repeat(63);
        let not_hex = format!("{}g", "0".repeat(63));
        for digest in [uppercase, short, not_hex] {
            assert_eq!(
                format!("cnt|1|{digest}").parse::<IdentityKey>(),
                Err(IdentityParseError::BadHex)
            );
        }
        assert!(format!("cnt|1|{digits}").parse::<IdentityKey>().is_ok());
    }

    #[test]
    fn from_str_rejects_unknown_prefix() {
        assert_eq!(
            "xyz|a|b".parse::<IdentityKey>(),
            Err(IdentityParseError::UnknownPrefix("xyz".to_owned()))
        );
    }

    #[test]
    fn from_str_rejects_bad_escape() {
        assert_eq!(
            "ctr|a%zzb|x|1|x|x".parse::<IdentityKey>(),
            Err(IdentityParseError::BadEscape)
        );
        assert_eq!(
            "ctr|a%|x|1|x|x".parse::<IdentityKey>(),
            Err(IdentityParseError::BadEscape)
        );
    }
}
