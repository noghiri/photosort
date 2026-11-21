//! Bounds-checked TIFF/IFD walker shared by every TIFF-based structure
//! (EXIF in JPEG APP1, NEF, the MPF index and vendor maker notes).
//!
//! Offsets stored inside a TIFF are relative to its header (`base`).
//! Every position this module returns is an absolute file offset. Only
//! the first `head.len()` bytes of the file are available; a read past
//! that but inside the file returns `ParseError::NeedMore`.
// pattern: Functional Core

use crate::model::{ByteRange, ParseError, Rational};

/// TIFF/EXIF/NEF magic number.
pub const TIFF_MAGIC: u16 = 42;
/// Real IFDs have well under 200 entries.
const MAX_IFD_ENTRIES: u16 = 1024;
/// Upper bound on values decoded from one entry.
const MAX_VALUES: u32 = 4096;

pub(crate) const OVERFLOW: ParseError = ParseError::Malformed {
    what: "offset overflow",
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteOrder {
    Little,
    Big,
}

#[derive(Debug, Clone, Copy)]
pub struct Tiff<'a> {
    head: &'a [u8],
    file_len: u64,
    base: u64,
    order: ByteOrder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub tag: u16,
    pub field_type: u16,
    pub count: u32,
    /// Absolute offset of the entry's 4-byte value/offset field.
    value_field: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ifd {
    pub entries: Vec<Entry>,
    /// Absolute offset of the next IFD in the chain, if any.
    pub next: Option<u64>,
}

impl Ifd {
    #[must_use]
    pub fn get(&self, tag: u16) -> Option<&Entry> {
        self.entries.iter().find(|e| e.tag == tag)
    }
}

impl<'a> Tiff<'a> {
    /// Parses the 8-byte header at absolute offset `base`; returns the
    /// walker and the absolute offset of IFD0. `magic` lists accepted
    /// values for header bytes 2..4.
    pub fn parse(
        head: &'a [u8],
        file_len: u64,
        base: u64,
        magic: &[u16],
    ) -> Result<(Self, u64), ParseError> {
        let probe = Tiff {
            head,
            file_len,
            base,
            order: ByteOrder::Little,
        };
        let order = match probe.bytes(base, 8)?.get(0..2) {
            Some(b"II") => ByteOrder::Little,
            Some(b"MM") => ByteOrder::Big,
            _ => return Err(ParseError::WrongFormat { expected: "tiff" }),
        };
        let tiff = Tiff { order, ..probe };
        // base + 8 <= head.len() was proven by `bytes` above.
        if !magic.contains(&tiff.u16_at(base + 2)?) {
            return Err(ParseError::WrongFormat { expected: "tiff" });
        }
        let ifd0 = tiff.resolve(tiff.u32_at(base + 4)?)?;
        Ok((tiff, ifd0))
    }

    #[must_use]
    pub fn order(&self) -> ByteOrder {
        self.order
    }

    #[must_use]
    pub fn base(&self) -> u64 {
        self.base
    }

    #[must_use]
    pub fn head(&self) -> &'a [u8] {
        self.head
    }

    #[must_use]
    pub fn file_len(&self) -> u64 {
        self.file_len
    }

    /// `len` bytes at absolute offset `abs`.
    pub fn bytes(&self, abs: u64, len: u64) -> Result<&'a [u8], ParseError> {
        bytes_within(self.head, self.file_len, abs, len)
    }

    pub fn u16_at(&self, abs: u64) -> Result<u16, ParseError> {
        let b: [u8; 2] = self.bytes(abs, 2)?.try_into().map_err(|_| OVERFLOW)?;
        Ok(match self.order {
            ByteOrder::Little => u16::from_le_bytes(b),
            ByteOrder::Big => u16::from_be_bytes(b),
        })
    }

    pub fn u32_at(&self, abs: u64) -> Result<u32, ParseError> {
        let b: [u8; 4] = self.bytes(abs, 4)?.try_into().map_err(|_| OVERFLOW)?;
        Ok(match self.order {
            ByteOrder::Little => u32::from_le_bytes(b),
            ByteOrder::Big => u32::from_be_bytes(b),
        })
    }

    /// Converts an offset stored in this TIFF to an absolute offset.
    pub fn resolve(&self, rel: u32) -> Result<u64, ParseError> {
        self.base.checked_add(u64::from(rel)).ok_or(OVERFLOW)
    }

    pub fn read_ifd(&self, abs: u64) -> Result<Ifd, ParseError> {
        let count = self.u16_at(abs)?;
        if count > MAX_IFD_ENTRIES {
            return Err(ParseError::Malformed {
                what: "ifd entry count",
            });
        }
        let first = abs.checked_add(2).ok_or(OVERFLOW)?;
        let table_len = u64::from(count) * 12;
        // Proves the whole table and the next-IFD pointer are present,
        // so the arithmetic below cannot overflow.
        self.bytes(first, table_len + 4)?;
        let entries = (0..u64::from(count))
            .map(|i| {
                let at = first + i * 12;
                Ok(Entry {
                    tag: self.u16_at(at)?,
                    field_type: self.u16_at(at + 2)?,
                    count: self.u32_at(at + 4)?,
                    value_field: at + 8,
                })
            })
            .collect::<Result<Vec<_>, ParseError>>()?;
        let next = match self.u32_at(first + table_len)? {
            0 => None,
            rel => Some(self.resolve(rel)?),
        };
        Ok(Ifd { entries, next })
    }
}

fn type_size(field_type: u16) -> Option<u64> {
    match field_type {
        1 | 2 | 6 | 7 => Some(1),   // BYTE, ASCII, SBYTE, UNDEFINED
        3 | 8 => Some(2),           // SHORT, SSHORT
        4 | 9 | 11 | 13 => Some(4), // LONG, SLONG, FLOAT, IFD
        5 | 10 | 12 => Some(8),     // RATIONAL, SRATIONAL, DOUBLE
        _ => None,
    }
}

impl Entry {
    /// Absolute position and byte length of the entry's value.
    pub fn location(&self, tiff: &Tiff<'_>) -> Result<(u64, u64), ParseError> {
        let size =
            type_size(self.field_type).ok_or(ParseError::Malformed { what: "field type" })?;
        let len = size.checked_mul(u64::from(self.count)).ok_or(OVERFLOW)?;
        if len <= 4 {
            Ok((self.value_field, len))
        } else {
            Ok((tiff.resolve(tiff.u32_at(self.value_field)?)?, len))
        }
    }

    pub fn raw<'a>(&self, tiff: &Tiff<'a>) -> Result<&'a [u8], ParseError> {
        let (at, len) = self.location(tiff)?;
        tiff.bytes(at, len)
    }

    /// ASCII/UNDEFINED text up to the first NUL, trailing whitespace trimmed.
    pub fn ascii(&self, tiff: &Tiff<'_>) -> Result<String, ParseError> {
        if self.field_type != 2 && self.field_type != 7 {
            return Err(ParseError::Malformed {
                what: "text field type",
            });
        }
        let raw = self.raw(tiff)?;
        let text = raw.split(|b| *b == 0).next().unwrap_or_default();
        Ok(String::from_utf8_lossy(text).trim_end().to_owned())
    }

    /// SHORT, LONG or IFD values widened to u32.
    pub fn u32_values(&self, tiff: &Tiff<'_>) -> Result<Vec<u32>, ParseError> {
        if self.count > MAX_VALUES {
            return Err(ParseError::Malformed {
                what: "value count",
            });
        }
        let step = match self.field_type {
            3 => 2,
            4 | 13 => 4,
            _ => {
                return Err(ParseError::Malformed {
                    what: "integer field type",
                });
            }
        };
        let (at, _) = self.location(tiff)?;
        (0..u64::from(self.count))
            .map(|i| {
                let p = at.checked_add(i * step).ok_or(OVERFLOW)?;
                if step == 2 {
                    tiff.u16_at(p).map(u32::from)
                } else {
                    tiff.u32_at(p)
                }
            })
            .collect()
    }

    pub fn first_u32(&self, tiff: &Tiff<'_>) -> Result<u32, ParseError> {
        self.u32_values(tiff)?
            .first()
            .copied()
            .ok_or(ParseError::Malformed {
                what: "empty value",
            })
    }

    pub fn rationals(&self, tiff: &Tiff<'_>) -> Result<Vec<Rational>, ParseError> {
        if self.field_type != 5 {
            return Err(ParseError::Malformed {
                what: "rational field type",
            });
        }
        if self.count > MAX_VALUES {
            return Err(ParseError::Malformed {
                what: "value count",
            });
        }
        let (at, _) = self.location(tiff)?;
        (0..u64::from(self.count))
            .map(|i| {
                let p = at.checked_add(i * 8).ok_or(OVERFLOW)?;
                Ok(Rational {
                    num: tiff.u32_at(p)?,
                    den: tiff.u32_at(p.checked_add(4).ok_or(OVERFLOW)?)?,
                })
            })
            .collect()
    }

    pub fn rational(&self, tiff: &Tiff<'_>) -> Result<Rational, ParseError> {
        self.rationals(tiff)?
            .first()
            .copied()
            .ok_or(ParseError::Malformed {
                what: "empty value",
            })
    }
}

/// Maps a field-level failure to "field absent", but keeps `NeedMore` so
/// the caller can re-read a longer head. Used for optional tags.
pub fn optional<T>(result: Result<T, ParseError>) -> Result<Option<T>, ParseError> {
    match result {
        Ok(v) => Ok(Some(v)),
        Err(e @ ParseError::NeedMore { .. }) => Err(e),
        Err(_) => Ok(None),
    }
}

/// `len` bytes at absolute file offset `abs`, for structures that are not
/// TIFFs (a maker-note header, a JPEG marker). Same semantics as `Tiff::bytes`.
pub fn bytes_within(head: &[u8], file_len: u64, abs: u64, len: u64) -> Result<&[u8], ParseError> {
    let end = abs.checked_add(len).ok_or(OVERFLOW)?;
    if end > file_len {
        return Err(ParseError::Malformed {
            what: "offset past end of file",
        });
    }
    let have = u64::try_from(head.len()).map_err(|_| OVERFLOW)?;
    if end > have {
        return Err(ParseError::NeedMore { needed: end, have });
    }
    // Both values fit in usize because end <= head.len().
    head.get(abs as usize..end as usize).ok_or(OVERFLOW)
}

/// Reads the value of `tag` in `ifd`. `None` when the tag is absent or its
/// value is malformed; `NeedMore` still propagates.
pub fn optional_tag<T>(
    ifd: &Ifd,
    tag: u16,
    read: impl FnOnce(&Entry) -> Result<T, ParseError>,
) -> Result<Option<T>, ParseError> {
    match ifd.get(tag) {
        Some(entry) => optional(read(entry)),
        None => Ok(None),
    }
}

/// A byte range described by an offset tag and a length tag in the same
/// IFD (Nikon PreviewIFD, NEF SubIFD JPEG). The offset is relative to the
/// TIFF header. `None` if either tag is absent or the range is not inside
/// the file.
pub fn byte_range(
    tiff: &Tiff<'_>,
    ifd: &Ifd,
    offset_tag: u16,
    len_tag: u16,
) -> Result<Option<ByteRange>, ParseError> {
    let start = optional_tag(ifd, offset_tag, |e| {
        e.first_u32(tiff).and_then(|rel| tiff.resolve(rel))
    })?;
    let len = optional_tag(ifd, len_tag, |e| e.first_u32(tiff))?;
    Ok(match (start, len) {
        (Some(start), Some(len)) => ByteRange::within(start, u64::from(len), tiff.file_len()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16e(big: bool, v: u16) -> [u8; 2] {
        if big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    }

    fn u32e(big: bool, v: u32) -> [u8; 4] {
        if big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    }

    /// A SHORT value stored inline in the 4-byte value field.
    fn short4(big: bool, v: u16) -> [u8; 4] {
        let [a, b] = u16e(big, v);
        [a, b, 0, 0]
    }

    fn header(big: bool) -> Vec<u8> {
        let mut out = if big {
            b"MM\0*".to_vec()
        } else {
            b"II*\0".to_vec()
        };
        out.extend_from_slice(&u32e(big, 8));
        out
    }

    fn entry(big: bool, tag: u16, typ: u16, count: u32, value: [u8; 4]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&u16e(big, tag));
        out.extend_from_slice(&u16e(big, typ));
        out.extend_from_slice(&u32e(big, count));
        out.extend_from_slice(&value);
        out
    }

    /// Header, then IFD0 at offset 8 with three entries (an inline SHORT,
    /// an out-of-line ASCII at 50 and a RATIONAL array at 56) and a next
    /// pointer to IFD1 at offset 72. IFD1 holds one inline SHORT.
    fn sample(big: bool) -> Vec<u8> {
        let mut out = header(big);
        out.extend_from_slice(&u16e(big, 3));
        out.extend(entry(big, 0x0100, 3, 1, short4(big, 640)));
        out.extend(entry(big, 0x010F, 2, 6, u32e(big, 50)));
        out.extend(entry(big, 0x011A, 5, 2, u32e(big, 56)));
        out.extend_from_slice(&u32e(big, 72));
        out.extend_from_slice(b"NIKON\0");
        out.extend_from_slice(&u32e(big, 1));
        out.extend_from_slice(&u32e(big, 250));
        out.extend_from_slice(&u32e(big, 28));
        out.extend_from_slice(&u32e(big, 10));
        out.extend_from_slice(&u16e(big, 1));
        out.extend(entry(big, 0x0112, 3, 1, short4(big, 1)));
        out.extend_from_slice(&u32e(big, 0));
        out
    }

    /// A TIFF whose IFD0 holds one entry tagged 0x0100.
    fn single_entry(big: bool, typ: u16, count: u32, value: [u8; 4]) -> Vec<u8> {
        let mut out = header(big);
        out.extend_from_slice(&u16e(big, 1));
        out.extend(entry(big, 0x0100, typ, count, value));
        out.extend_from_slice(&u32e(big, 0));
        out
    }

    fn open(bytes: &[u8]) -> (Tiff<'_>, Ifd) {
        let (tiff, ifd0) = Tiff::parse(bytes, bytes.len() as u64, 0, &[TIFF_MAGIC]).unwrap();
        let ifd = tiff.read_ifd(ifd0).unwrap();
        (tiff, ifd)
    }

    #[test]
    fn sample_fixture_has_expected_length() {
        assert_eq!(sample(false).len(), 90);
        assert_eq!(sample(true).len(), 90);
    }

    #[test]
    fn parses_header_and_returns_ifd0_offset() {
        for big in [false, true] {
            let bytes = sample(big);
            let (tiff, ifd0) = Tiff::parse(&bytes, bytes.len() as u64, 0, &[TIFF_MAGIC]).unwrap();
            assert_eq!(ifd0, 8);
            let expected = if big {
                ByteOrder::Big
            } else {
                ByteOrder::Little
            };
            assert_eq!(tiff.order(), expected);
        }
    }

    #[test]
    fn rejects_unknown_byte_order_and_magic() {
        let bad_order = b"XX*\0\x08\0\0\0";
        assert_eq!(
            Tiff::parse(bad_order, 8, 0, &[TIFF_MAGIC]).unwrap_err(),
            ParseError::WrongFormat { expected: "tiff" }
        );
        let wrong_le_magic = [b'I', b'I', 43, 0, 8, 0, 0, 0];
        assert_eq!(
            Tiff::parse(&wrong_le_magic, 8, 0, &[TIFF_MAGIC]).unwrap_err(),
            ParseError::WrongFormat { expected: "tiff" }
        );
        let wrong_be_magic = [b'M', b'M', 0, 43, 0, 0, 0, 8];
        assert_eq!(
            Tiff::parse(&wrong_be_magic, 8, 0, &[TIFF_MAGIC]).unwrap_err(),
            ParseError::WrongFormat { expected: "tiff" }
        );
    }

    #[test]
    fn header_past_head_is_need_more() {
        let bytes = sample(false);
        let err = Tiff::parse(&bytes[..4], bytes.len() as u64, 0, &[TIFF_MAGIC]).unwrap_err();
        assert_eq!(err, ParseError::NeedMore { needed: 8, have: 4 });
    }

    #[test]
    fn reads_inline_short() {
        for big in [false, true] {
            let bytes = sample(big);
            let (tiff, ifd) = open(&bytes);
            let value = ifd.get(0x0100).unwrap().first_u32(&tiff).unwrap();
            assert_eq!(value, 640);
        }
    }

    #[test]
    fn reads_out_of_line_ascii() {
        for big in [false, true] {
            let bytes = sample(big);
            let (tiff, ifd) = open(&bytes);
            let text = ifd.get(0x010F).unwrap().ascii(&tiff).unwrap();
            assert_eq!(text, "NIKON");
        }
    }

    #[test]
    fn reads_rational_array() {
        for big in [false, true] {
            let bytes = sample(big);
            let (tiff, ifd) = open(&bytes);
            let values = ifd.get(0x011A).unwrap().rationals(&tiff).unwrap();
            assert_eq!(
                values,
                vec![Rational { num: 1, den: 250 }, Rational { num: 28, den: 10 }]
            );
        }
    }

    #[test]
    fn reads_ifd_chain() {
        for big in [false, true] {
            let bytes = sample(big);
            let (tiff, ifd) = open(&bytes);
            assert_eq!(ifd.entries.len(), 3);
            assert_eq!(ifd.next, Some(72));
            let ifd1 = tiff.read_ifd(72).unwrap();
            assert_eq!(ifd1.get(0x0112).unwrap().first_u32(&tiff).unwrap(), 1);
            assert_eq!(ifd1.next, None);
        }
    }

    #[test]
    fn ifd_offset_past_file_len_is_malformed() {
        let mut bytes = header(false);
        bytes[4..8].copy_from_slice(&u32e(false, 4096));
        let (tiff, ifd0) = Tiff::parse(&bytes, 8, 0, &[TIFF_MAGIC]).unwrap();
        assert_eq!(ifd0, 4096);
        assert_eq!(
            tiff.read_ifd(ifd0).unwrap_err(),
            ParseError::Malformed {
                what: "offset past end of file"
            }
        );
    }

    #[test]
    fn ifd_past_head_but_inside_file_is_need_more() {
        let bytes = sample(false);
        let file_len = bytes.len() as u64;
        let (tiff, ifd0) = Tiff::parse(&bytes[..40], file_len, 0, &[TIFF_MAGIC]).unwrap();
        match tiff.read_ifd(ifd0).unwrap_err() {
            ParseError::NeedMore { needed, have } => {
                assert_eq!(have, 40);
                assert!(
                    have < needed && needed <= file_len,
                    "needed {needed}, have {have}"
                );
            }
            other => panic!("expected NeedMore, got {other:?}"),
        }
    }

    #[test]
    fn out_of_line_value_past_head_is_need_more() {
        let bytes = sample(false);
        let (tiff, ifd0) = Tiff::parse(&bytes[..52], bytes.len() as u64, 0, &[TIFF_MAGIC]).unwrap();
        let ifd = tiff.read_ifd(ifd0).unwrap();
        let err = ifd.get(0x010F).unwrap().ascii(&tiff).unwrap_err();
        assert_eq!(
            err,
            ParseError::NeedMore {
                needed: 56,
                have: 52
            }
        );
    }

    #[test]
    fn bytes_at_u64_max_is_malformed_not_overflow_panic() {
        let bytes = sample(false);
        let (tiff, _) = open(&bytes);
        assert_eq!(
            tiff.bytes(u64::MAX, 2).unwrap_err(),
            ParseError::Malformed {
                what: "offset overflow"
            }
        );
    }

    #[test]
    fn entry_count_above_limit_is_malformed() {
        let mut bytes = header(false);
        bytes.extend_from_slice(&1025u16.to_le_bytes());
        let (tiff, ifd0) = Tiff::parse(&bytes, bytes.len() as u64, 0, &[TIFF_MAGIC]).unwrap();
        assert_eq!(
            tiff.read_ifd(ifd0).unwrap_err(),
            ParseError::Malformed {
                what: "ifd entry count"
            }
        );
    }

    #[test]
    fn unknown_field_type_is_malformed() {
        for big in [false, true] {
            let bytes = single_entry(big, 99, 1, [0; 4]);
            let (tiff, ifd) = open(&bytes);
            let err = ifd.get(0x0100).unwrap().location(&tiff).unwrap_err();
            assert_eq!(err, ParseError::Malformed { what: "field type" });
        }
    }

    #[test]
    fn count_u32_max_is_malformed_without_overflow_or_allocation() {
        for big in [false, true] {
            let long = single_entry(big, 4, u32::MAX, [0; 4]);
            let (tiff, ifd) = open(&long);
            let value = ifd.get(0x0100).unwrap();
            assert_eq!(
                value.u32_values(&tiff).unwrap_err(),
                ParseError::Malformed {
                    what: "value count"
                }
            );

            let rational = single_entry(big, 5, u32::MAX, [0; 4]);
            let (tiff, ifd) = open(&rational);
            let value = ifd.get(0x0100).unwrap();
            assert_eq!(
                value.rationals(&tiff).unwrap_err(),
                ParseError::Malformed {
                    what: "value count"
                }
            );

            // Under the value cap for text, so the length runs past the file.
            let ascii = single_entry(big, 2, u32::MAX, [0; 4]);
            let (tiff, ifd) = open(&ascii);
            let value = ifd.get(0x0100).unwrap();
            assert_eq!(
                value.ascii(&tiff).unwrap_err(),
                ParseError::Malformed {
                    what: "offset past end of file"
                }
            );
        }
    }

    #[test]
    fn optional_keeps_need_more_and_maps_other_errors_to_none() {
        assert_eq!(optional::<u32>(Ok(7)), Ok(Some(7)));
        assert_eq!(
            optional::<u32>(Err(ParseError::Malformed { what: "x" })),
            Ok(None)
        );
        assert_eq!(
            optional::<u32>(Err(ParseError::WrongFormat { expected: "tiff" })),
            Ok(None)
        );
        let need = ParseError::NeedMore {
            needed: 10,
            have: 4,
        };
        assert_eq!(optional::<u32>(Err(need.clone())), Err(need));
    }
}
