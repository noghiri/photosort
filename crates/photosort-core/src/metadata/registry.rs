//! Extension points for vendor knowledge: format readers (by file
//! extension) and maker-note readers (by EXIF Make).
// pattern: Functional Core

use crate::metadata::tiff::ByteOrder;
use crate::model::{ByteRange, FileKind, Metadata, ParseError};

/// Largest head the shell will ever read for metadata.
pub const MAX_HEAD_LEN: u64 = 16 * 1024 * 1024;

pub struct ParseInput<'a> {
    /// The first bytes of the file (always starts at file offset 0).
    pub head: &'a [u8],
    pub file_len: u64,
    pub makernotes: &'a [Box<dyn MakerNoteReader>],
}

impl ParseInput<'_> {
    #[must_use]
    pub fn makernote_for(&self, make: &str) -> Option<&dyn MakerNoteReader> {
        self.makernotes
            .iter()
            .find(|r| r.matches_make(make))
            .map(AsRef::as_ref)
    }
}

pub trait FormatReader: Send + Sync {
    fn kind(&self) -> FileKind;
    /// Lowercase, without the dot, e.g. `["nef"]`.
    fn extensions(&self) -> &'static [&'static str];
    /// Short tile badge, e.g. "J" or "N" (the grid joins them: "J+N").
    fn badge(&self) -> &'static str;
    fn is_raw(&self) -> bool;
    /// Bytes to read first; `ParseError::NeedMore` asks for more.
    fn header_len_hint(&self) -> usize;
    fn parse(&self, input: &ParseInput<'_>) -> Result<Metadata, ParseError>;
    /// Allowlist for lens EXIF writes (Phase 8).
    fn lens_write_supported(&self) -> bool;
}

/// A maker-note block located inside a parsed TIFF.
pub struct MakerNote<'a> {
    pub head: &'a [u8],
    pub file_len: u64,
    /// Absolute offset and length of the MakerNote (0x927C) value.
    pub offset: u64,
    pub len: u64,
    pub parent_order: ByteOrder,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VendorFields {
    pub shot_counter: Option<u64>,
    pub serial: Option<String>,
    /// Absolute range of a vendor-embedded preview JPEG.
    pub preview: Option<ByteRange>,
}

pub trait MakerNoteReader: Send + Sync {
    fn matches_make(&self, make: &str) -> bool;
    /// May return `NeedMore`; other errors mean "no vendor fields".
    fn read(&self, note: &MakerNote<'_>) -> Result<VendorFields, ParseError>;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("extension {0:?} is already registered")]
    DuplicateExtension(&'static str),
    #[error("file kind {0:?} is already registered")]
    DuplicateKind(&'static str),
}

#[derive(Default)]
pub struct Registry {
    formats: Vec<Box<dyn FormatReader>>,
    makernotes: Vec<Box<dyn MakerNoteReader>>,
}

impl Registry {
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Holds the built-in readers. Empty until the JPEG, NEF and Nikon
    /// readers are added.
    #[must_use]
    pub fn with_builtin() -> Self {
        Self::empty()
    }

    /// Rejects a reader whose kind or any extension is already taken.
    /// The kind is checked first.
    pub fn register_format(&mut self, reader: Box<dyn FormatReader>) -> Result<(), RegistryError> {
        let kind = reader.kind().0;
        if self.formats.iter().any(|f| f.kind().0 == kind) {
            return Err(RegistryError::DuplicateKind(kind));
        }
        for ext in reader.extensions() {
            if self.formats.iter().any(|f| f.extensions().contains(ext)) {
                return Err(RegistryError::DuplicateExtension(ext));
            }
        }
        self.formats.push(reader);
        Ok(())
    }

    pub fn register_makernote(&mut self, reader: Box<dyn MakerNoteReader>) {
        self.makernotes.push(reader);
    }

    /// Case-insensitive; accepts a leading dot.
    #[must_use]
    pub fn format_for_extension(&self, ext: &str) -> Option<&dyn FormatReader> {
        let ext = ext.trim_start_matches('.').to_ascii_lowercase();
        self.formats
            .iter()
            .find(|f| f.extensions().contains(&ext.as_str()))
            .map(AsRef::as_ref)
    }

    #[must_use]
    pub fn format_for_kind(&self, kind: FileKind) -> Option<&dyn FormatReader> {
        self.formats
            .iter()
            .find(|f| f.kind() == kind)
            .map(AsRef::as_ref)
    }

    pub fn parse(
        &self,
        kind: FileKind,
        head: &[u8],
        file_len: u64,
    ) -> Result<Metadata, ParseError> {
        let reader = self.format_for_kind(kind).ok_or(ParseError::Malformed {
            what: "unregistered file kind",
        })?;
        reader.parse(&ParseInput {
            head,
            file_len,
            makernotes: &self.makernotes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubReader {
        kind: &'static str,
        exts: &'static [&'static str],
    }

    impl FormatReader for StubReader {
        fn kind(&self) -> FileKind {
            FileKind(self.kind)
        }

        fn extensions(&self) -> &'static [&'static str] {
            self.exts
        }

        fn badge(&self) -> &'static str {
            "S"
        }

        fn is_raw(&self) -> bool {
            false
        }

        fn header_len_hint(&self) -> usize {
            16
        }

        fn parse(&self, _input: &ParseInput<'_>) -> Result<Metadata, ParseError> {
            Ok(Metadata {
                make: Some(self.kind.to_owned()),
                ..Metadata::default()
            })
        }

        fn lens_write_supported(&self) -> bool {
            false
        }
    }

    struct StubMakerNote;

    impl MakerNoteReader for StubMakerNote {
        fn matches_make(&self, make: &str) -> bool {
            make.starts_with("STUB")
        }

        fn read(&self, _note: &MakerNote<'_>) -> Result<VendorFields, ParseError> {
            Ok(VendorFields::default())
        }
    }

    fn stub(kind: &'static str, exts: &'static [&'static str]) -> Box<dyn FormatReader> {
        Box::new(StubReader { kind, exts })
    }

    #[test]
    fn registers_fake_format_and_finds_it_by_any_extension_spelling() {
        let mut reg = Registry::empty();
        reg.register_format(stub("fkr", &["fkr"])).unwrap();
        assert!(reg.format_for_extension("FKR").is_some());
        assert!(reg.format_for_extension(".fkr").is_some());
        assert!(reg.format_for_extension("fkr").is_some());
        assert!(reg.format_for_kind(FileKind("fkr")).is_some());
    }

    #[test]
    fn unknown_extension_and_kind_are_none() {
        let mut reg = Registry::empty();
        reg.register_format(stub("fkr", &["fkr"])).unwrap();
        assert!(reg.format_for_extension("nef").is_none());
        assert!(reg.format_for_kind(FileKind("nef")).is_none());
        assert!(Registry::empty().format_for_extension("fkr").is_none());
    }

    #[test]
    fn duplicate_extension_is_rejected() {
        let mut reg = Registry::empty();
        reg.register_format(stub("first", &["fkr"])).unwrap();
        assert_eq!(
            reg.register_format(stub("second", &["fkr"])),
            Err(RegistryError::DuplicateExtension("fkr"))
        );
    }

    #[test]
    fn duplicate_kind_is_rejected() {
        let mut reg = Registry::empty();
        reg.register_format(stub("fkr", &["aaa"])).unwrap();
        assert_eq!(
            reg.register_format(stub("fkr", &["bbb"])),
            Err(RegistryError::DuplicateKind("fkr"))
        );
    }

    #[test]
    fn parse_dispatches_to_the_registered_reader() {
        let mut reg = Registry::empty();
        reg.register_format(stub("fkr", &["fkr"])).unwrap();
        let meta = reg.parse(FileKind("fkr"), &[0u8; 4], 4).unwrap();
        assert_eq!(meta.make.as_deref(), Some("fkr"));
    }

    #[test]
    fn parse_of_unregistered_kind_is_malformed() {
        let err = Registry::empty()
            .parse(FileKind("nope"), &[], 0)
            .unwrap_err();
        assert_eq!(
            err,
            ParseError::Malformed {
                what: "unregistered file kind"
            }
        );
    }

    #[test]
    fn makernote_reader_is_found_by_make() {
        let mut reg = Registry::empty();
        reg.register_makernote(Box::new(StubMakerNote));
        let input = ParseInput {
            head: &[],
            file_len: 0,
            makernotes: &reg.makernotes,
        };
        assert!(input.makernote_for("STUB CAMERA").is_some());
        assert!(input.makernote_for("ACME").is_none());
    }
}
