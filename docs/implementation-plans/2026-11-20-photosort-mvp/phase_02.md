# photosort MVP Implementation Plan — Phase 2: Metadata parsing and identity

**Goal:** Pure, panic-free parsers that extract everything photosort needs from JPEG and NEF headers, and the identity ladder built on them.

**Architecture:** All code lives in `photosort-core` (Functional Core). A shared bounds-checked TIFF walker (`metadata/tiff.rs`) underpins EXIF-in-JPEG, NEF, the MPF index and the Nikon maker note. Vendor knowledge lives only in format readers (`metadata/formats/`) and maker-note readers (`metadata/makernotes/`), both looked up through `metadata::Registry`. Parsers see only the first N bytes of a file (the "head") plus the file length, and report `ParseError::NeedMore` when a structure lies past the head so the shell can re-read. `identity.rs` turns `Metadata` into an `IdentityKey` using the counter → capture → content ladder. A new dev-only crate, `photosort-testkit`, builds synthetic JPEG/NEF/TIFF bytes independently of the parser.

**Tech Stack:** Rust 1.99.1; `blake3` 1.8.7; `thiserror` 2.0.21; dev: `proptest` 1.12.0, `image` 0.25.10 (`jpeg` feature), `serde_json` 1.0.154, `tempfile` 3.27.0; ExifTool 13.64 (oracle tests only, opt-in).

**Scope:** 8 phases from original design (this is phase 2 of 8)

**Codebase verified:** 2026-11-20

---

## Acceptance Criteria Coverage

This phase implements and tests:

### photosort-mvp.AC4: Metadata parsing and identity
- **photosort-mvp.AC4.1 Success:** For Z f JPEG and NEF files, the parser extracts these fields, and each value equals ExifTool's output in the oracle test:
  - `Make`, `Model`, serial;
  - `DateTimeOriginal`, `SubSecTimeOriginal`;
  - `ISO`, `ExposureTime`, `FNumber`, `FocalLength`;
  - `LensMake`, `LensModel`;
  - the shot counter (`ShutterCount`).
- **photosort-mvp.AC4.2 Success:** The parser locates the following, and each extracted range decodes at the expected size:
  - the JPEG MPF preview and the NEF PreviewIFD preview (640×424 on the Z f);
  - the NEF `JpgFromRaw` (6048×4032).
- **photosort-mvp.AC4.3 Success:** The identity key uses the counter key when a maker-note reader supplies a shot counter, the capture key when EXIF exists without a counter, and the content key when there's no usable EXIF.
- **photosort-mvp.AC4.4 Success:** A file's identity key is unchanged after renaming the file, or after changing any of `LensMake`, `LensModel`, `LensInfo`, `FNumber`, `FocalLength`, `MaxApertureValue` or `SubjectDistance`.
- **photosort-mvp.AC4.5 Success:** The JPEG and NEF of the same capture produce equal identity keys (counter and capture rungs).
- **photosort-mvp.AC4.6 Failure:** Truncated or malformed input, or out-of-bounds offsets, produce an error or `None` and never panic. Property and fuzz tests verify this.

### photosort-mvp.AC12: Body-agnostic extensibility
- **photosort-mvp.AC12.1 Success:** A test-only `FormatReader` for a fake TIFF-based extension can be registered. Its files are then scanned, grouped, identified, planned and copied, with no change to grouping, identity, planning or copy code.
  - *This phase covers registration, parsing and identity. Phase 3 extends the same test to scanning and grouping; Phase 5 to planning and copying.*
- **photosort-mvp.AC12.2 Edge:** A file whose `Make` has no `MakerNoteReader` falls to the capture or content identity key without error.

**Notes on scope:**
- AC4.1 and AC4.2 on *real* Z f files are verified by `oracle_samples_match_exiftool`, which needs a samples folder the user designates before MVP sign-off. In this phase they are verified on synthetic files (unit tests, plus `oracle_synthetic_matches_exiftool` against real ExifTool).
- AC4.6 "fuzz" is proptest-based (random bytes and mutated valid fixtures), per the user's decision. cargo-fuzz is not used.
- ORF is deferred entirely (user decision): `.orf` is not registered and no `orf.rs` is created. Task 13 opens the tracking issue.

---

## Conventions for this phase

- Every Rust file with runtime behaviour starts with `// pattern: Functional Core` after its `//!` doc. `mod.rs` files contain only `pub mod` / `pub use` lines and are exempt.
- `photosort-core` code never indexes slices with `[]` (enforced by `#![deny(clippy::indexing_slicing)]`); use `.get()` and the `Tiff` helpers. Unit tests may index.
- Absolute file offsets are `u64`. Offset arithmetic on untrusted values uses `checked_add`/`checked_mul`, or is done only after a bounds check that proves it cannot overflow (with a comment saying so).
- Tests for a module live in that file's `#[cfg(test)] mod tests`. Cross-module and oracle tests live in `crates/photosort-core/tests/`.
- Test command for the whole phase: `cargo test -p photosort-core`.

<!-- START_SUBCOMPONENT_A (tasks 1-2) -->
<!-- START_TASK_1 -->
### Task 1: Core data contracts (`model.rs`) and crate setup

**Verifies:** None (types; behaviour is tested via later tasks).

**Files:**
- Modify: `Cargo.toml` (root): add workspace dependencies and the testkit member
- Modify: `crates/photosort-core/Cargo.toml`: add dependencies
- Modify: `crates/photosort-core/src/lib.rs`
- Create: `crates/photosort-core/src/model.rs`

**Step 1: Root `Cargo.toml`**

Extend `[workspace.dependencies]`. Leave `members` unchanged; Task 2 adds the testkit member and its workspace dependency.

```toml
blake3 = "=1.8.7"
thiserror = "=2.0.21"
proptest = "=1.12.0"
image = { version = "=0.25.10", default-features = false, features = ["jpeg"] }
serde_json = "=1.0.154"
tempfile = "=3.27.0"
```

**Step 2: `crates/photosort-core/Cargo.toml`**

```toml
[dependencies]
blake3 = { workspace = true }
thiserror = { workspace = true }

[dev-dependencies]
proptest = { workspace = true }
image = { workspace = true }
serde_json = { workspace = true }
tempfile = { workspace = true }
```

(`photosort-testkit = { workspace = true }` is added to `[dev-dependencies]` in Task 2.)

**Step 3: `crates/photosort-core/src/lib.rs`**

```rust
//! Pure logic for photosort: metadata parsing, shot grouping, identity,
//! copy planning, lens rules and persisted state types.
//!
//! This crate performs no filesystem, thread, environment or UI work.
//! Callers pass bytes and data in and get data out, which keeps every
//! decision testable without a card, a disk or a GPU.
#![forbid(unsafe_code)]
// Parsers handle untrusted bytes; every access must be bounds-checked.
#![deny(clippy::indexing_slicing)]
#![cfg_attr(test, allow(clippy::indexing_slicing))]

pub mod model;
```

Later tasks add `pub mod metadata;` (Task 3) and `pub mod identity;` (Task 9).

**Step 4: Create `crates/photosort-core/src/model.rs`**

```rust
//! Data contracts that cross phase boundaries: file kinds, parsed
//! metadata, byte ranges and identity keys.
// pattern: Functional Core

/// A file format, identified by a registered reader. Data, not a closed
/// vendor enum: new raw formats add a reader, not a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileKind(pub &'static str);

/// An unsigned EXIF RATIONAL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rational {
    pub num: u32,
    pub den: u32,
}

impl Rational {
    /// Lowest terms, so that `10/2500` and `1/250` compare equal.
    /// A zero denominator is returned unchanged.
    #[must_use]
    pub fn reduced(self) -> Rational {
        let g = gcd(self.num, self.den);
        if g <= 1 {
            return self;
        }
        Rational { num: self.num / g, den: self.den / g }
    }

    /// `None` when the denominator is zero.
    #[must_use]
    pub fn to_f64(self) -> Option<f64> {
        (self.den != 0).then(|| f64::from(self.num) / f64::from(self.den))
    }
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// A byte range in a file, guaranteed non-empty and inside the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ByteRange {
    pub offset: u64,
    pub len: u64,
}

impl ByteRange {
    /// `None` if the range is empty or extends past `file_len`.
    #[must_use]
    pub fn within(offset: u64, len: u64, file_len: u64) -> Option<ByteRange> {
        let end = offset.checked_add(len)?;
        (len > 0 && end <= file_len).then_some(ByteRange { offset, len })
    }

    #[must_use]
    pub fn end(&self) -> u64 {
        // Cannot overflow: `within` checked it.
        self.offset + self.len
    }
}

/// Everything photosort reads from a file header.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metadata {
    pub make: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
    /// EXIF "YYYY:MM:DD HH:MM:SS".
    pub date_time_original: Option<String>,
    pub subsec_time_original: Option<String>,
    pub iso: Option<u32>,
    pub exposure_time: Option<Rational>,
    pub f_number: Option<Rational>,
    pub focal_length: Option<Rational>,
    pub lens_make: Option<String>,
    pub lens_model: Option<String>,
    /// EXIF Orientation (1-8); previews are rotated by it in Phase 3.
    pub orientation: Option<u16>,
    /// From the vendor `MakerNoteReader` (Nikon: ShutterCount 0x00A7).
    pub shot_counter: Option<u64>,
    /// Embedded ~640 px JPEG.
    pub preview: Option<ByteRange>,
    /// Full-size embedded JPEG in a raw file (NEF JpgFromRaw).
    pub full_jpeg: Option<ByteRange>,
}

/// Identity of a photo, independent of file name and lens edits.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IdentityKey {
    Counter { make: String, serial: String, counter: u64, dto: String, subsec: String },
    Capture { make: String, serial: String, dto: String, subsec: String, iso: u32, exposure: Rational },
    Content { size: u64, head_blake3: [u8; 32] },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// The structure continues past the bytes supplied. `needed` is the
    /// smallest head length (from file start) that would let parsing
    /// progress; it never exceeds the file length.
    #[error("need the first {needed} bytes of the file, have {have}")]
    NeedMore { needed: u64, have: u64 },
    #[error("not a {expected} file")]
    WrongFormat { expected: &'static str },
    #[error("malformed {what}")]
    Malformed { what: &'static str },
}
```

**Step 5: Verify**

Run: `cargo build -p photosort-core`
Expected: builds with no warnings.

**Step 6: Commit:** `feat(core): add metadata, byte range and identity contracts`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: `photosort-testkit` synthetic file builders

**Verifies:** None (test support; exercised by every later test in this phase).

**Files:**
- Modify: root `Cargo.toml`: add `"crates/photosort-testkit"` to `members`, and `photosort-testkit = { path = "crates/photosort-testkit" }` to `[workspace.dependencies]`
- Modify: `crates/photosort-core/Cargo.toml`: add `photosort-testkit = { workspace = true }` to `[dev-dependencies]`
- Create: `crates/photosort-testkit/Cargo.toml`
- Create: `crates/photosort-testkit/src/lib.rs` (re-exports only)
- Create: `crates/photosort-testkit/src/spec.rs`
- Create: `crates/photosort-testkit/src/tiff_writer.rs`
- Create: `crates/photosort-testkit/src/files.rs`

`Cargo.toml`: workspace package fields, `publish = false`, `[lints] workspace = true`, `[dependencies] image = { workspace = true }`. The testkit deliberately does **not** depend on `photosort-core`: it writes bytes from the format specs, so a wrong assumption in the parser cannot be mirrored into the fixtures.

**`spec.rs`** (plain data, exempt from the pattern comment):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order { Little, Big }

/// Standard EXIF fields to write. `None` omits the tag.
#[derive(Debug, Clone, PartialEq)]
pub struct ExifSpec {
    pub make: Option<String>,
    pub model: Option<String>,
    pub orientation: Option<u16>,
    pub body_serial: Option<String>,          // ExifIFD 0xA431
    pub date_time_original: Option<String>,   // 0x9003
    pub subsec_time_original: Option<String>, // 0x9291
    pub iso: Option<u16>,                     // 0x8827 SHORT
    pub exposure_time: Option<(u32, u32)>,    // 0x829A
    pub f_number: Option<(u32, u32)>,         // 0x829D
    pub focal_length: Option<(u32, u32)>,     // 0x920A
    pub max_aperture: Option<(u32, u32)>,     // 0x9205
    pub subject_distance: Option<(u32, u32)>, // 0x9206
    pub lens_info: Option<[(u32, u32); 4]>,   // 0xA432
    pub lens_make: Option<String>,            // 0xA433
    pub lens_model: Option<String>,           // 0xA434
}

/// Nikon "type 3" maker note content. `None` omits the tag.
#[derive(Debug, Clone, PartialEq)]
pub struct NikonSpec {
    pub serial: Option<String>,        // 0x001D ASCII
    pub mechanical_count: Option<u32>, // 0x0037 LONG
    pub shutter_count: Option<u32>,    // 0x00A7 LONG
    pub preview_jpeg: Option<Vec<u8>>, // via PreviewIFD 0x0011
    /// true: preview bytes stored inside the MakerNote value (its count covers them);
    /// false: preview stored after the TIFF content. Real Z f layout is unverified,
    /// so tests exercise both.
    pub preview_inside: bool,
}
```

Plus constructors with realistic Z f-like values and a **fake** serial:
- `ExifSpec::zf()`:
  - Make `NIKON CORPORATION`, Model `NIKON Z f`, orientation 1;
  - body_serial `1234567`, DTO `2026:11:20 14:59:32`, subsec `92`;
  - ISO 800, exposure 1/250, f/2.8 as 28/10, focal 50/1, max aperture 297/100;
  - lens model `NIKKOR Z 50mm f/1.8 S`, lens make `NIKON`, lens_info 50/1 50/1 18/10 18/10, subject distance `None`.
- `NikonSpec::zf(preview: Option<Vec<u8>>)`: serial `1234567`, mechanical 10438, shutter 75036, `preview_inside: false`.

**`tiff_writer.rs`** (`// pattern: Functional Core`): a small TIFF serializer.
- `IfdBuilder` collects entries: `ascii(tag, &str)` (NUL-terminated), `short(tag, &[u16])`, `long(tag, &[u32])`, `rational(tag, &[(u32,u32)])`, `undefined(tag, &[u8])`, plus `child(tag, IfdBuilder)` (a LONG entry whose value is the child IFD's offset) and `children(tag, Vec<IfdBuilder>)` (for SubIFDs `0x014A`).
- It also supports `blob_offset(tag, Blob)` and `blob_len(tag, Blob)`. These are LONG entries whose value is the offset (relative to the TIFF base) or the length of a byte blob placed after all IFDs. They are used for `0x0201`/`0x0202` and `StripOffsets`.
- `fn write_tiff(order, magic: u16, ifd0: IfdBuilder, tail_padding: usize) -> TiffBytes` lays out the following:
  - The header.
  - IFD0 at offset 8.
  - Each IFD's out-of-line values right after its entry table.
  - Child IFDs depth-first.
  - `tail_padding` zero bytes.
  - All blobs.

  Entries are sorted by tag, and all offsets are relative to the start of the returned bytes. Two passes: compute every block's size and offset first, then serialize.
- `TiffBytes { bytes: Vec<u8>, blob_offsets: Vec<usize> }` reports where each blob landed (relative), for assertions.
- The maker-note case needs an **embedded** TIFF whose offsets are relative to its own header but whose blob (the preview JPEG) lives outside it. Support that with `write_embedded(order, ifd0, blob_base_delta)`: offsets written for blobs are `absolute_blob_position - embedded_header_position`. The caller passes the final absolute positions once the outer layout is known.

**`files.rs`** (`// pattern: Functional Core`): complete file builders returning byte vectors.

- `pub fn tiny_jpeg(width: u32, height: u32) -> Vec<u8>`: encodes a gradient with `image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80)` and `encode(&pixels, w, h, image::ExtendedColorType::L8)`.
- `pub fn nikon_maker_note(order, spec: &NikonSpec, preview_abs: Option<(u64, u64)>, mn_abs: u64) -> Vec<u8>`:
  - Writes `b"Nikon\0"`, then `[0x02, 0x10, 0x00, 0x00]`, then an embedded TIFF (magic 42) whose IFD0 holds `0x001D`, `0x0037`, `0x00A7` and, if there is a preview, `0x0011` (LONG, relative offset of a PreviewIFD).
  - The PreviewIFD holds `0x0201` = `preview_abs.0 - (mn_abs + 10)` and `0x0202` = `preview_abs.1`.
  - When `spec.preview_inside` is true, the preview bytes are appended inside the returned maker-note bytes (so the MakerNote entry's count covers them), `preview_abs` is ignored, and the builder computes the relative offset itself.
- `pub fn jpeg_file(order, exif: Option<&ExifSpec>, nikon: Option<&NikonSpec>, mpf_preview: Option<&[u8]>) -> Vec<u8>`. Layout:
  - SOI.
  - APP1 `"Exif\0\0"` + TIFF: IFD0 with Make/Model/Orientation and the ExifIFD pointer `0x8769`. The ExifIFD holds the ExifSpec fields, plus MakerNote `0x927C` (UNDEFINED) when `nikon` is `Some`.
  - APP2 `"MPF\0"` + TIFF (big-endian `MM`, magic 42): IFD0 with `0xB000` UNDEFINED `"0100"`, `0xB001` LONG 2, and `0xB002` UNDEFINED 32 bytes.
    - Entry 1 is the primary: attr `0x030000`, size = primary length, offset 0.
    - Entry 2 is the preview: attr `0x010001`, size = preview length, offset = preview absolute position minus the position of the `MM` byte.
    - The segment is omitted when `mpf_preview` is `None`.
  - The primary image body: `tiny_jpeg(16, 16)` without its leading SOI (`FF D8`).
  - The preview JPEG bytes appended at the end (the real MPF layout).

  Build in two passes: lay out with placeholder offsets, then patch the MPF entry and maker-note offsets. Nikon's NEF-style PreviewIFD is not used in JPEGs; pass `preview_jpeg: None` for JPEGs.
- `pub fn nef_file(order, exif: &ExifSpec, nikon: Option<&NikonSpec>, jpg_from_raw: Option<&[u8]>, padding: usize) -> Vec<u8>`. A TIFF at offset 0 (magic 42):
  - IFD0: `0x00FE`=1, Make, Model, Orientation, SubIFDs `0x014A` → [SubIFD0, SubIFD1], ExifIFD `0x8769`.
  - SubIFD0: `0x00FE`=0, Compression `0x0103`=6, `0x0201`/`0x0202` → the jpg_from_raw blob. Omitted if `None`.
  - SubIFD1: `0x00FE`=0, Compression=1, StripOffsets/StripByteCounts → a 64-byte zero blob (fake raw).
  - The ExifIFD as in JPEG, with MakerNote from `nikon_maker_note`. When `preview_inside` is false, its preview blob is placed after `padding` bytes; when it is true, the preview is inside the maker-note value.
  - `padding` zero bytes are inserted before the blobs. This lets tests push the preview and JpgFromRaw past a 128 KiB head while the IFDs and maker note stay inside it.
- `pub fn tiff_file(order, magic: u16, exif: &ExifSpec, nikon: Option<&NikonSpec>) -> Vec<u8>`: minimal TIFF with IFD0 + ExifIFD, for the AC12.1 fake format.

`lib.rs`: `pub mod files; pub mod spec; mod tiff_writer; pub use files::*; pub use spec::*;`

**Verification:**
Run: `cargo build -p photosort-testkit` and `cargo clippy -p photosort-testkit -- -D warnings`
Expected: no errors or warnings. (Builders are validated by the parser tests in Tasks 3-11 and against ExifTool in Task 12.)

**Commit:** `test(testkit): add synthetic JPEG, NEF and TIFF builders`
<!-- END_TASK_2 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 3-4) -->
<!-- START_TASK_3 -->
### Task 3: Bounds-checked TIFF walker (`metadata/tiff.rs`)

**Verifies:** photosort-mvp.AC4.6 (unit level: out-of-bounds and truncated structures return errors)

**Files:**
- Create: `crates/photosort-core/src/metadata/mod.rs` (start with `pub mod tiff;` only)
- Create: `crates/photosort-core/src/metadata/tiff.rs`
- Modify: `crates/photosort-core/src/lib.rs`: add `pub mod metadata;`

**Implementation** (the safety-critical core; implement as written, adjusting only for compiler feedback):

```rust
//! Bounds-checked TIFF/IFD walker shared by every TIFF-based structure
//! (EXIF in JPEG APP1, NEF, the MPF index and vendor maker notes).
//!
//! Offsets stored inside a TIFF are relative to its header (`base`).
//! Every position this module returns is an absolute file offset. Only
//! the first `head.len()` bytes of the file are available; a read past
//! that but inside the file returns `ParseError::NeedMore`.
// pattern: Functional Core

use crate::model::{ParseError, Rational};

/// TIFF/EXIF/NEF magic number.
pub const TIFF_MAGIC: u16 = 42;
/// Real IFDs have well under 200 entries.
const MAX_IFD_ENTRIES: u16 = 1024;
/// Upper bound on values decoded from one entry.
const MAX_VALUES: u32 = 4096;

const OVERFLOW: ParseError = ParseError::Malformed { what: "offset overflow" };

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
    pub fn parse(head: &'a [u8], file_len: u64, base: u64, magic: &[u16]) -> Result<(Self, u64), ParseError> {
        let probe = Tiff { head, file_len, base, order: ByteOrder::Little };
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
        let end = abs.checked_add(len).ok_or(OVERFLOW)?;
        if end > self.file_len {
            return Err(ParseError::Malformed { what: "offset past end of file" });
        }
        let have = self.head.len() as u64;
        if end > have {
            return Err(ParseError::NeedMore { needed: end, have });
        }
        // Both values fit in usize because end <= head.len().
        self.head.get(abs as usize..end as usize).ok_or(OVERFLOW)
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
            return Err(ParseError::Malformed { what: "ifd entry count" });
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
        let size = type_size(self.field_type).ok_or(ParseError::Malformed { what: "field type" })?;
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
            return Err(ParseError::Malformed { what: "text field type" });
        }
        let raw = self.raw(tiff)?;
        let text = raw.split(|b| *b == 0).next().unwrap_or_default();
        Ok(String::from_utf8_lossy(text).trim_end().to_owned())
    }

    /// SHORT, LONG or IFD values widened to u32.
    pub fn u32_values(&self, tiff: &Tiff<'_>) -> Result<Vec<u32>, ParseError> {
        if self.count > MAX_VALUES {
            return Err(ParseError::Malformed { what: "value count" });
        }
        let step = match self.field_type {
            3 => 2,
            4 | 13 => 4,
            _ => return Err(ParseError::Malformed { what: "integer field type" }),
        };
        let (at, _) = self.location(tiff)?;
        (0..u64::from(self.count))
            .map(|i| {
                let p = at.checked_add(i * step).ok_or(OVERFLOW)?;
                if step == 2 { tiff.u16_at(p).map(u32::from) } else { tiff.u32_at(p) }
            })
            .collect()
    }

    pub fn first_u32(&self, tiff: &Tiff<'_>) -> Result<u32, ParseError> {
        self.u32_values(tiff)?
            .first()
            .copied()
            .ok_or(ParseError::Malformed { what: "empty value" })
    }

    pub fn rationals(&self, tiff: &Tiff<'_>) -> Result<Vec<Rational>, ParseError> {
        if self.field_type != 5 {
            return Err(ParseError::Malformed { what: "rational field type" });
        }
        if self.count > MAX_VALUES {
            return Err(ParseError::Malformed { what: "value count" });
        }
        let (at, _) = self.location(tiff)?;
        (0..u64::from(self.count))
            .map(|i| {
                let p = at.checked_add(i * 8).ok_or(OVERFLOW)?;
                Ok(Rational { num: tiff.u32_at(p)?, den: tiff.u32_at(p.checked_add(4).ok_or(OVERFLOW)?)? })
            })
            .collect()
    }

    pub fn rational(&self, tiff: &Tiff<'_>) -> Result<Rational, ParseError> {
        self.rationals(tiff)?
            .first()
            .copied()
            .ok_or(ParseError::Malformed { what: "empty value" })
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
```

**Testing** (unit tests in `tiff.rs`; build inputs with small hand-written byte arrays so this module is tested independently of the testkit):
- Parses `II*\0` and `MM\0*` headers and returns the right IFD0 offset; rejects other magic with `WrongFormat`.
- Reads an inline SHORT, an out-of-line ASCII, a RATIONAL array and an IFD chain (`next`), in both byte orders.
- AC4.6:
  - an IFD offset past `file_len` → `Malformed`;
  - an offset inside the file but past the head → `NeedMore { needed, have }` with `have < needed <= file_len`;
  - an entry count above 1024 → `Malformed`;
  - an unknown field type → `Malformed`;
  - `count = u32::MAX` does not overflow or allocate (→ `Malformed`).
- `optional` keeps `NeedMore` and maps other errors to `Ok(None)`.

**Verification:** `cargo test -p photosort-core metadata::tiff`: all pass.

**Commit:** `feat(core): add bounds-checked TIFF/IFD walker`
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: Registry traits and lookup (`metadata/registry.rs`)

**Verifies:** photosort-mvp.AC12.1 (registration and lookup portion)

**Files:**
- Create: `crates/photosort-core/src/metadata/registry.rs`
- Modify: `crates/photosort-core/src/metadata/mod.rs`: `pub mod registry; pub use registry::*;`

**Implementation:**

```rust
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
        self.makernotes.iter().find(|r| r.matches_make(make)).map(AsRef::as_ref)
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
```

Methods:
- `Registry::empty()`.
- `Registry::with_builtin()`: registers `JpegReader`, `NefReader` and `NikonMakerNotes`. Tasks 6-8 add those lines as each reader lands. Until then `with_builtin` registers nothing, and its doc comment lists the readers it holds.
- `register_format(&mut self, Box<dyn FormatReader>) -> Result<(), RegistryError>`: rejects a duplicate kind or extension.
- `register_makernote(&mut self, Box<dyn MakerNoteReader>)`.
- `format_for_extension(&self, ext: &str) -> Option<&dyn FormatReader>`: case-insensitive, accepts a leading `.`.
- `format_for_kind(&self, FileKind) -> Option<&dyn FormatReader>`.
- `parse(&self, kind: FileKind, head: &[u8], file_len: u64) -> Result<Metadata, ParseError>`: builds `ParseInput` with `&self.makernotes`, and returns `Malformed { what: "unregistered file kind" }` for an unknown kind.

**Testing** (unit tests in `registry.rs` with a small in-test `StubReader`):
- AC12.1 (registration): a stub reader for extension `fkr` registers; `format_for_extension("FKR")`, `(".fkr")` and `("fkr")` all find it; `parse` dispatches to it.
- A duplicate extension or kind returns the matching `RegistryError`.
- An unknown extension returns `None`.

**Verification:** `cargo test -p photosort-core metadata::registry`

**Commit:** `feat(core): add format and maker-note reader registry`
<!-- END_TASK_4 -->
<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 5-8) -->
<!-- START_TASK_5 -->
### Task 5: Standard EXIF extraction (`metadata/exif.rs`)

**Verifies:** photosort-mvp.AC4.1 (synthetic), photosort-mvp.AC12.2 (no maker-note reader is not an error)

**Files:**
- Create: `crates/photosort-core/src/metadata/exif.rs`
- Modify: `metadata/mod.rs`: `pub mod exif;`

**Implementation:**

```rust
pub struct TiffFile<'a> {
    pub tiff: Tiff<'a>,
    pub ifd0: Ifd,
    pub meta: Metadata,
}

/// Parses a TIFF header at `base`, then the EXIF fields reachable from IFD0.
pub fn read_tiff_exif<'a>(input: &ParseInput<'a>, base: u64, magic: &[u16]) -> Result<TiffFile<'a>, ParseError>;
```

Steps:
1. `Tiff::parse`, then `read_ifd(ifd0)`. Errors here propagate (no usable TIFF).
2. IFD0: Make `0x010F` and Model `0x0110` (`ascii`; an empty string becomes `None`), Orientation `0x0112` (`first_u32` → u16).
3. ExifIFD pointer `0x8769` (`first_u32` → `resolve` → `read_ifd`):
   - DTO `0x9003`, SubSec `0x9291`, BodySerial `0xA431`, LensMake `0xA433` and LensModel `0xA434`, read with `ascii`;
   - ISO `0x8827` (`first_u32`);
   - ExposureTime `0x829A`, FNumber `0x829D` and FocalLength `0x920A` (`rational`).
4. MakerNote `0x927C`: if `make` is set and `input.makernote_for(make)` finds a reader:
   - take `entry.location()` as `MakerNote { offset, len }` and call `read`;
   - `NeedMore` propagates; any other error → `VendorFields::default()`;
   - copy `shot_counter`, `preview`, and `serial` (only when `0xA431` was absent).
   - No reader for the make → nothing happens and there's no error (AC12.2).

Every optional tag goes through `tiff::optional(...)?`, so a malformed optional field becomes `None` while `NeedMore` still propagates.

**Testing** (unit tests in `exif.rs` using `photosort_testkit::tiff_file(Order::Little | Order::Big, 42, &ExifSpec::zf(), None)` with `ParseInput { makernotes: &[] }`):
- AC4.1 (synthetic): every listed field equals the spec value. Serial comes from BodySerial; exposure is `Rational { num: 1, den: 250 }`.
- AC12.2: Make `ACME` with an arbitrary maker-note blob and no registered reader → `Ok`, with `shot_counter == None`.
- Each tag removed in turn (spec field `None`) → that `Metadata` field is `None`, and the others are unchanged.

**Verification:** `cargo test -p photosort-core metadata::exif`

**Commit:** `feat(core): extract standard EXIF fields via shared TIFF walker`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Nikon maker-note reader (`metadata/makernotes/nikon.rs`)

**Verifies:** photosort-mvp.AC4.1 (shot counter, synthetic), photosort-mvp.AC4.2 (NEF PreviewIFD location, synthetic)

**Files:**
- Create: `crates/photosort-core/src/metadata/makernotes/mod.rs` (`pub mod nikon;`)
- Create: `crates/photosort-core/src/metadata/makernotes/nikon.rs`
- Modify: `metadata/mod.rs` (`pub mod makernotes;`), `registry.rs` (`with_builtin` registers `NikonMakerNotes`)

**Implementation** (`pub struct NikonMakerNotes;`):
- `matches_make`: `make.trim().to_ascii_uppercase().starts_with("NIKON")`.
- `read`:
  1. `bytes(offset, 7)` must equal `b"Nikon\0\x02"`. Otherwise → `WrongFormat { expected: "nikon type 3 maker note" }` (older Nikon layouts have no counter we support).
  2. `Tiff::parse(head, file_len, offset + 10, &[TIFF_MAGIC])` (checked add), then `read_ifd(ifd0)`. Source: ExifTool MakerNotes.pm:51-57, `Base = maker-note start + 10`.
  3. `0x00A7` ShutterCount: `first_u32` → `shot_counter`. Never read `0x0037` (MechanicalShutterCount): it does not advance in silent mode, so burst frames would collide.
  4. `0x001D` SerialNumber: `ascii` → `serial`.
  5. `0x0011` PreviewIFD: `first_u32` → `resolve` → `read_ifd`. Then `0x0201` (`first_u32` → `resolve`) and `0x0202` (`first_u32`) → `ByteRange::within(start, len, file_len)` → `preview`.
  6. Steps 3-5 are each wrapped in `tiff::optional`.

**Testing** (unit tests; a NEF from `photosort_testkit::nef_file` parsed through `exif::read_tiff_exif` with `makernotes: &[Box::new(NikonMakerNotes)]`):
- AC4.1: `shot_counter == Some(75036)`, not the mechanical 10438.
- AC4.2 (synthetic): the `preview` range sliced from the full file decodes with `image::load_from_memory` to the testkit preview size (use `tiny_jpeg(640, 424)`).
- A `"Nikon\0\x01"` header, or a non-Nikon make → `VendorFields` absent, and the parse still succeeds.
- Both byte orders, and both `preview_inside` values.

**Verification:** `cargo test -p photosort-core metadata::makernotes`

**Commit:** `feat(core): read Nikon ShutterCount, serial and PreviewIFD`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: JPEG reader with MPF preview (`metadata/formats/jpeg.rs`)

**Verifies:** photosort-mvp.AC4.1 (synthetic JPEG), photosort-mvp.AC4.2 (JPEG MPF preview, synthetic)

**Files:**
- Create: `crates/photosort-core/src/metadata/formats/mod.rs` (`pub mod jpeg;`)
- Create: `crates/photosort-core/src/metadata/formats/jpeg.rs`
- Modify: `metadata/mod.rs` (`pub mod formats;`), `registry.rs` (`with_builtin` registers `JpegReader`)

**Implementation** (`pub struct JpegReader;`, `pub const KIND: FileKind = FileKind("jpeg");`):
- `extensions` `["jpg", "jpeg"]`, `badge` `"J"`, `is_raw` false, `header_len_hint` `128 * 1024`, `lens_write_supported` true.
- `parse`:
  1. Read bytes 0..2 through the same bounds helper used for markers, so a head shorter than 2 bytes returns `NeedMore` (or `Malformed` if the file itself is shorter). They must be `FF D8`; otherwise → `WrongFormat { expected: "jpeg" }`.
  2. Walk the markers from position 2, capped at 512 segments:
     - skip `0xFF` fill bytes;
     - `D0..=D7` and `01` have no length;
     - stop at `DA` (SOS) or `D9`;
     - otherwise read the BE u16 length (≥ 2), with payload = `pos+4 .. pos+2+len`.
     - Record `exif_base = payload + 6` for the first APP1 (`E1`) whose payload starts with `b"Exif\0\0"`.
     - Record `mpf_base = payload + 4` for the first APP2 (`E2`) whose payload starts with `b"MPF\0"`.
     - A marker header past the head → `NeedMore { needed: pos + 4 }` (bounded by `file_len`; `Malformed` if past it).
     - A byte other than `0xFF` where a marker is expected → `Malformed`.
     - Use a small local helper with the same `bytes`/`NeedMore` semantics as `Tiff::bytes`; never index directly.
  3. EXIF: if `exif_base` is set → `exif::read_tiff_exif(input, exif_base, &[TIFF_MAGIC])?.meta`, else `Metadata::default()`. A JPEG without EXIF is valid.
  4. MPF:
     - `Tiff::parse(head, file_len, mpf_base, &[TIFF_MAGIC])`, then `read_ifd(ifd0)`.
     - Read `0xB002` with `raw()`: N × 16-byte entries, N ≤ 64, read with the MPF TIFF's byte order. Each entry is attr u32 @0, size u32 @4, offset u32 @8.
     - The preview is the **first** entry with `offset != 0`, `size != 0` and `(attr & 0x00FF_FFFF) & 0x0F_0000 == 0x01_0000`.
     - Its absolute start is `mpf_base + offset`, giving `ByteRange::within(start, size, file_len)`.
     - Wrap the MPF block in `tiff::optional`; a broken MPF means `preview = None`.
     - Source: ExifTool MPF.pm:197-231.
  5. If MPF gave no preview, keep the maker-note `preview` already in `meta`.

**Testing** (unit tests via `photosort_testkit::jpeg_file`):
- AC4.1 (synthetic): all ExifSpec fields plus `shot_counter` (Nikon maker note in APP1), in both byte orders.
- AC4.2 (synthetic): the `preview` range from `jpeg_file(.., Some(&tiny_jpeg(640, 424)))` decodes to 640×424, and its bytes equal the preview passed in.
- A JPEG with no APP1/APP2 → `Ok(Metadata::default())`.
- `NeedMore`: parsing only the first 200 bytes of a file whose APP1 is longer returns `NeedMore` with `needed <= file_len`.
- Not a JPEG → `WrongFormat`.

**Verification:** `cargo test -p photosort-core metadata::formats::jpeg`

**Commit:** `feat(core): parse JPEG EXIF and MPF preview`
<!-- END_TASK_7 -->

<!-- START_TASK_8 -->
### Task 8: NEF reader with PreviewIFD and JpgFromRaw (`metadata/formats/nef.rs`)

**Verifies:** photosort-mvp.AC4.1 (synthetic NEF), photosort-mvp.AC4.2 (NEF preview and JpgFromRaw, synthetic)

**Files:**
- Create: `crates/photosort-core/src/metadata/formats/nef.rs`
- Modify: `formats/mod.rs` (`pub mod nef;`), `registry.rs` (`with_builtin` registers `NefReader`)

**Implementation** (`pub struct NefReader;`, `pub const KIND: FileKind = FileKind("nef");`):
- `extensions` `["nef"]`, `badge` `"N"`, `is_raw` true, `header_len_hint` `128 * 1024`, `lens_write_supported` true.
- `parse`:
  1. `exif::read_tiff_exif(input, 0, &[TIFF_MAGIC])`. Its `meta.preview` comes from Nikon's PreviewIFD.
  2. JpgFromRaw candidates are IFD0 itself plus each IFD listed in IFD0's SubIFDs `0x014A` (`u32_values`, cap 16; one level only, so no loops are possible).
     - A candidate has both `0x0201` (`first_u32` → `resolve`) and `0x0202`, giving `ByteRange::within`.
     - `full_jpeg` is the candidate with the largest `len`, excluding a range equal to `meta.preview`.
     - Wrap each candidate in `tiff::optional`.
     - Source: rawler nef.rs:390-406. ExifTool names only SubIFD index 0 "JpgFromRaw", so do not depend on the index.
- The module doc notes the inferred, unverified assumption that HE/HE\* compression does not change the container. The oracle on real Z f samples confirms it.

**Testing** (unit tests via `photosort_testkit::nef_file`):
- AC4.1 (synthetic): all fields plus `shot_counter`, in both byte orders.
- AC4.2 (synthetic): `preview` decodes to 640×424; `full_jpeg` decodes to the JpgFromRaw size passed in (use `tiny_jpeg(1200, 800)` to keep tests fast; the real 6048×4032 is checked by the oracle).
- With `preview_inside: false` and `padding = 200 * 1024`: the IFDs and maker note stay inside a 128 KiB head, the preview and JpgFromRaw ranges lie past it, and parsing succeeds without `NeedMore` (ranges are located, not read).
- With `preview_inside: true`: the preview range lies inside the maker-note value's range and still decodes to 640×424.
- A NEF with no SubIFD JPEG → `full_jpeg = None`; no maker note → `preview = None`, `shot_counter = None`.

**Verification:** `cargo test -p photosort-core metadata::formats::nef`

**Commit:** `feat(core): parse NEF EXIF, PreviewIFD preview and JpgFromRaw`
<!-- END_TASK_8 -->
<!-- END_SUBCOMPONENT_C -->

<!-- START_SUBCOMPONENT_D (tasks 9-10) -->
<!-- START_TASK_9 -->
### Task 9: Identity ladder and string form (`identity.rs`)

**Verifies:** photosort-mvp.AC4.3, photosort-mvp.AC4.4, photosort-mvp.AC4.5, photosort-mvp.AC12.2

**Files:**
- Create: `crates/photosort-core/src/identity.rs`
- Modify: `lib.rs` (`pub mod identity;`)

**Implementation:**

```rust
/// Bytes from file start hashed for the content rung.
pub const CONTENT_HEAD_LEN: usize = 64 * 1024;

/// `head` must be the first bytes of the file; at most
/// `CONTENT_HEAD_LEN` of them are hashed. The file name is deliberately
/// not an input, so renaming cannot change identity.
#[must_use]
pub fn identity_for(meta: Option<&Metadata>, file_len: u64, head: &[u8]) -> IdentityKey;
```

Rules (strings are already trimmed by the parser; treat an empty string as absent):
1. **Counter**: make, serial, `shot_counter` and DTO all present → `Counter { make, serial, counter, dto, subsec: subsec.unwrap_or_default() }`.
2. **Capture**: make, DTO, ISO, and `exposure_time` with `den != 0` all present → `Capture { make, serial: serial.unwrap_or_default(), dto, subsec, iso, exposure: exposure.reduced() }`.
3. **Content**: otherwise → `Content { size: file_len, head_blake3: *blake3::hash(head.get(..min(len, CONTENT_HEAD_LEN))).as_bytes() }`.

`FNumber`, `FocalLength` and every lens field are never inputs, because Manual profiles overwrite them on copies.

String form: implement `Display` and `FromStr` for `IdentityKey`, with an `IdentityParseError` (thiserror).
- Fields are joined by `|`.
- Inside string fields, `%` → `%25` and `|` → `%7C`.
- Prefixes:
  - `ctr|make|serial|counter|dto|subsec`
  - `cap|make|serial|dto|subsec|iso|num/den`
  - `cnt|size|<64 lowercase hex>`
- Example: `ctr|NIKON CORPORATION|1234567|75036|2026:11:20 14:59:32|92`.
- `FromStr` rejects a wrong field count, a bad number, bad hex or an unknown prefix.

(The design's example `ctr:…:2026:11:20 14:59:32:92` used `:` as the separator, which is ambiguous because `DateTimeOriginal` contains `:`.)

**Testing** (unit tests in `identity.rs`, using testkit files parsed via `Registry::with_builtin()`):
- AC4.3: a NEF with a Nikon note → `Counter`; the same file with `shutter_count: None` → `Capture`; a JPEG with no EXIF → `Content`; `identity_for(None, ..)` → `Content`.
- AC4.4: pairs of files that differ only in `lens_make`, `lens_model`, `lens_info`, `f_number`, `focal_length`, `max_aperture` or `subject_distance`. One case per field, for both rungs (with and without shutter count). Keys are equal. Renaming is covered by construction (no name input); state that in the test doc comment.
- AC4.5: `jpeg_file` and `nef_file` from the same `ExifSpec::zf()`/`NikonSpec::zf()` give equal keys on the counter rung, and again with `shutter_count: None` on the capture rung, even when the NEF stores exposure as `10/2500`.
- AC12.2: Make `ACME`, no reader → `Capture`; Make `ACME` with no DTO → `Content`. Neither panics nor errors.
- Proptest round-trip: for arbitrary `IdentityKey` values (each variant with `".*"` strings, including `|` and `%`), `key.to_string().parse::<IdentityKey>() == Ok(key)`. Add explicit cases for empty strings and for strings containing `|`, `%` and `%7C`.

**Verification:** `cargo test -p photosort-core identity`

**Commit:** `feat(core): derive identity keys with counter, capture and content rungs`
<!-- END_TASK_9 -->

<!-- START_TASK_10 -->
### Task 10: Extensibility test with a fake TIFF-based format

**Verifies:** photosort-mvp.AC12.1 (registration, parsing and identity portion), photosort-mvp.AC12.2

**Files:**
- Create: `crates/photosort-core/tests/extensibility.rs`

**Implementation:** the test file defines `FakeTiffReader`:
- kind `FileKind("fake")`, extension `fkr`, badge `"F"`, `is_raw` true, `lens_write_supported` false;
- `parse` calls `photosort_core::metadata::exif::read_tiff_exif(input, 0, &[TIFF_MAGIC])` and returns `.meta`.

It registers the reader into `Registry::with_builtin()` without touching library code.

**Testing:**
- AC12.1: `format_for_extension("FKR")` finds it; `parse` of `photosort_testkit::tiff_file(.., Some(&NikonSpec::zf(None)))` yields a `Counter` identity equal to the one from a NEF built from the same specs.
- AC12.2: the same fake file with Make `ACME` → `Capture`.

**Verification:** `cargo test -p photosort-core --test extensibility`

**Commit:** `test(core): register a fake TIFF-based format without core changes`
<!-- END_TASK_10 -->
<!-- END_SUBCOMPONENT_D -->

<!-- START_TASK_11 -->
### Task 11: Robustness property tests (no panics, ranges stay in bounds)

**Verifies:** photosort-mvp.AC4.6

**Files:**
- Create: `crates/photosort-core/tests/robustness.rs`

**Testing** (proptest; commit any `proptest-regressions/` files proptest writes):
1. **Random bytes**: for each built-in reader, `any::<Vec<u8>>()` (size 0..4096), plus `file_len` in `head.len()..=head.len()+1_000_000`. Parsing returns `Ok` or `Err` and never panics. 1024 cases.
2. **Mutated valid files**: start from `jpeg_file`/`nef_file` (both byte orders), then apply 1-8 random byte overwrites and an optional truncation. Never panics. **Invariant:** every `ByteRange` in an `Ok` result has `end() <= file_len`. 1024 cases.
3. **NeedMore converges**: for a valid file and a random head length `k` in `0..=len`, repeatedly parse `&file[..k]` and, on `NeedMore { needed }`, set `k = needed`. The invariants: `needed > k` (progress) and `needed <= len` on every step, and the loop ends with a result equal to `parse(&file)`. Cap at 64 iterations; reaching the cap fails the test.
4. `identity_for` never panics on arbitrary metadata, `file_len` and head.

**Verification:** `cargo test -p photosort-core --test robustness`: passes. Run once locally with `PROPTEST_CASES=20000` and confirm it still passes. CI uses the default case counts.

**Commit:** `test(core): property-test parsers against malformed and truncated input`
<!-- END_TASK_11 -->

<!-- START_TASK_12 -->
### Task 12: ExifTool oracle tests (synthetic and real samples)

**Verifies:** photosort-mvp.AC4.1, photosort-mvp.AC4.2 (against ExifTool; the real-sample run is a pending human step)

**Files:**
- Create: `crates/photosort-core/tests/oracle.rs`

**Implementation:**
- Two tests, both `#[ignore = "opt-in: needs ExifTool (and samples); see docs in oracle.rs"]`. Each reads its env vars at the start and **panics with an explicit message naming the missing variable** if it's unset or the path doesn't exist. Neither ever returns early as a pass.
  - `oracle_synthetic_matches_exiftool` needs `PHOTOSORT_EXIFTOOL`. It writes testkit JPEG and NEF files (both byte orders, with and without the Nikon note) into a `tempfile::tempdir()` and runs the comparison.
  - `oracle_samples_match_exiftool` needs `PHOTOSORT_EXIFTOOL` and `PHOTOSORT_SAMPLES`. It collects `.jpg`/`.jpeg`/`.nef` files (any case) recursively and runs the comparison. It fails if it finds zero files.
- Comparison:
  1. Run `<exiftool> -json -G1 -a -n <files…>` via `std::process::Command`, chunking 100 files per call. Map results by `SourceFile`.
  2. For each file, parse the head with `Registry::with_builtin()` using the shell's loop: start with `header_len_hint`, and on `NeedMore` re-read up to `MAX_HEAD_LEN`.
  3. Compare:

     | Our field | ExifTool key | How |
     |---|---|---|
     | `make` | `IFD0:Make` | exact |
     | `model` | `IFD0:Model` | exact |
     | `serial` | `ExifIFD:SerialNumber`, else `Nikon:SerialNumber` | exact |
     | `date_time_original` | `ExifIFD:DateTimeOriginal` | exact |
     | `subsec_time_original` | `ExifIFD:SubSecTimeOriginal` | as strings: JSON numbers are converted with `to_string()`, because ExifTool emits `"92"` as `92` |
     | `iso` | `ExifIFD:ISO` | integer |
     | `exposure_time`, `f_number`, `focal_length` | `ExifIFD:ExposureTime`, `ExifIFD:FNumber`, `ExifIFD:FocalLength` | `to_f64()` within relative 1e-6, since ExifTool prints `%.7g` |
     | `lens_make`, `lens_model` | `ExifIFD:LensMake`, `ExifIFD:LensModel` | exact |
     | `shot_counter` | `Nikon:ShutterCount` | integer |

     Normalise every ExifTool value compared as a string: ExifTool emits number-like strings (serial `1234567`, SubSec `92`) as JSON numbers, so convert JSON numbers with `to_string()` before comparing. The SubSec row is one instance of this rule. Absence on both sides counts as equal. Presence on only one side is a mismatch.
  4. Previews (AC4.2):
     - read each located range from disk;
     - it must start with `FF D8` and decode with `image::load_from_memory`;
     - when `IFD0:Model == "NIKON Z f"`, assert `preview` is 640×424 for JPEG and NEF, and `full_jpeg` is 6048×4032 for NEF;
     - for synthetic files, assert the testkit sizes.
  5. Real samples only: same-stem JPEG+NEF pairs in the same folder must have equal `identity_for` keys (AC4.5 on real data).
  6. Collect every mismatch and fail once with a table of `file / field / ours / exiftool`.
- The module doc explains how to run it:

  ```powershell
  $env:PHOTOSORT_EXIFTOOL = 'C:\path\to\exiftool.exe'
  $env:PHOTOSORT_SAMPLES  = 'C:\path\to\zf-samples'
  cargo test -p photosort-core --test oracle -- --ignored --nocapture
  ```

  The samples folder should include a few silent-mode (electronic shutter) burst frames, so that the ShutterCount rung is exercised on frames whose mechanical count does not change.

**Verification:**
1. Run `cargo test -p photosort-core --test oracle` without env vars. Expected: 2 tests `ignored`, 0 failed.
2. Run `cargo test -p photosort-core --test oracle -- --ignored` without env vars. Expected: both tests **fail**, with messages naming `PHOTOSORT_EXIFTOOL` (and `PHOTOSORT_SAMPLES`).
3. Ask the user for the ExifTool 13.64 path. During planning a copy was found in an earlier session's scratch folder (its zip matched SHA-256 `72c29c44d1e860eebcaefa71692d979af9858fe9a6d7d791a5e7f1dac77804af`), but it may have been cleaned up. Never write a local path into committed files: this repository is public. Run the synthetic oracle with `PHOTOSORT_EXIFTOOL` set. Expected: pass. A mismatch means the testkit or the parser is wrong; fix the cause and re-run the Task 3-11 tests.
4. **Pending human step (before MVP sign-off):** the user designates a Z f samples folder and the samples oracle passes. Record the run (date, file count, result) under `## Phase 2` in `docs/implementation-plans/2026-11-20-photosort-mvp/manual-test-log.md`. Do not block the phase on this step; the user chose synthetic tests for now.
5. Create `docs/implementation-plans/2026-11-20-photosort-mvp/manual-test-log.md` with a `# Manual test log` heading and one section per phase (`## Phase 2` … `## Phase 8`). Record the synthetic oracle result (date, pass/fail) under `## Phase 2`, and leave the samples run as "pending". Later phases append their manual-check results to their own section. Commit it with this task.

**Commit:** `test(core): add opt-in ExifTool oracle tests`
<!-- END_TASK_12 -->

<!-- START_TASK_13 -->
### Task 13: ORF tracking issue

**Verifies:** None (process). Satisfies the Phase 2 "Done when": *ORF support is either verified against OM-1 Mark II samples or explicitly deferred with a tracking issue.*

The repository is public, so get the user's confirmation before creating the issue.

```bash
gh issue create --repo noghiri/photosort --title "ORF (OM System OM-1 Mark II) support" --body-file -
```

Body:

```text
ORF is deferred from the MVP: `.orf` is not a registered extension, so ORF files are ignored by the scan.

Blocked on OM-1 Mark II sample files. To do once samples exist:
- Confirm ORF's TIFF magic variants and IFD layout.
- Locate the embedded preview (reportedly inside the maker notes) and any full-size JPEG.
- Decide whether OM System exposes a usable shot counter; add an OM System MakerNoteReader if so.
- Verify ExifTool lens writes to ORF before setting lens_write_supported = true.
- Add ORF samples to the ExifTool oracle run.

Design: docs/design-plans/2026-11-20-photosort-mvp.md (Additional considerations: OM System / Olympus).
```

Record the issue URL under `## Phase 2` in `docs/implementation-plans/2026-11-20-photosort-mvp/manual-test-log.md`.
<!-- END_TASK_13 -->


<!-- START_TASK_14 -->
### Task 14: Phase-wide verification

**Verifies:** None (gate).

Run, and expect success for each:
- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`. The two oracle tests from Task 12 show as `ignored`; everything else passes.

**Commit:** only if formatting or clippy fixes were needed: `chore: fix lints`.
<!-- END_TASK_14 -->
