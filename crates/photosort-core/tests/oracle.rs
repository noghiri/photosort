//! Opt-in comparison of photosort's metadata parsers against ExifTool.
//!
//! Both tests are `#[ignore]`d, so a plain `cargo test` never runs them.
//!
//! - `oracle_synthetic_matches_exiftool` writes testkit JPEG and NEF files
//!   (both byte orders, with and without the Nikon maker note) into a temp
//!   directory. It needs `PHOTOSORT_EXIFTOOL`.
//! - `oracle_samples_match_exiftool` checks a folder of real files, recursively.
//!   It needs `PHOTOSORT_EXIFTOOL` and `PHOTOSORT_SAMPLES`. It fails if the
//!   folder holds no `.jpg`, `.jpeg` or `.nef` files, and also if it holds no
//!   same-stem JPEG+NEF pair: the AC4.5 identity check would then verify nothing.
//!
//! Both tests panic when a variable is unset or names a missing path. Neither
//! passes early. Run them with:
//!
//! ```powershell
//! $env:PHOTOSORT_EXIFTOOL = 'C:\path\to\exiftool.exe'
//! $env:PHOTOSORT_SAMPLES  = 'C:\path\to\zf-samples'
//! cargo test -p photosort-core --test oracle -- --ignored --nocapture
//! ```
//!
//! ExifTool 13.64 is the reference. The comparison runs
//! `exiftool -json -G1 -a -n`, so keys are `Group:Tag` and values are numeric
//! where ExifTool can print them as numbers. Every mismatch is collected and
//! reported in one table: `file | field | ours | exiftool/expected`.
//!
//! Located previews and full-size JPEGs are also compared by offset and
//! length with ExifTool's ranges: `PreviewIFD:PreviewImageStart` and the MPF
//! large thumbnail (`MPImageN:MPImageStart`, type 0x010001) for the preview,
//! and `*:JpgFromRawStart` for the full JPEG. A located range must equal one
//! of ExifTool's, and ExifTool must report one. A shared wrong offset base in
//! the testkit and the parser therefore cannot pass.
//!
//! The samples folder should include a few silent-mode (electronic shutter)
//! burst frames, so the ShutterCount rung is exercised on frames whose
//! mechanical count does not change.

use std::collections::HashMap;
use std::env;
use std::ffi::OsStr;
use std::fmt::{Debug, Write as _};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

use photosort_core::identity::identity_for;
use photosort_core::metadata::Registry;
use photosort_core::model::{ByteRange, IdentityKey, Metadata, ParseError, Rational};
use photosort_testkit::{
    ExifSpec, NikonSpec, Order, jpeg_file, jpeg_file_with_mpf_order, nef_file, tiny_jpeg,
};
use serde_json::{Map, Value};

/// ExifTool is called with at most this many files per process.
const EXIFTOOL_CHUNK: usize = 100;

/// Relative tolerance for ExifTool's `%.7g` float output.
const FLOAT_RELATIVE_TOLERANCE: f64 = 1e-6;

/// MPImageType as ExifTool prints it with `-n`: 0x010001, Large Thumbnail (VGA equivalent).
const MPF_LARGE_THUMBNAIL: u64 = 0x01_0001;
const Z_F_MODEL: &str = "NIKON Z f";
const Z_F_PREVIEW: (u32, u32) = (640, 424);
const Z_F_FULL: (u32, u32) = (6048, 4032);

/// Sizes of the synthetic previews, which the testkit writes.
const SYNTH_PREVIEW: (u32, u32) = (640, 424);
const SYNTH_FULL: (u32, u32) = (1200, 800);

/// What a located range must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The parser must not locate it.
    Absent,
    /// Located, decodes as a JPEG of exactly this size.
    Size(u32, u32),
    /// If located, it must decode as a JPEG. Its size is not asserted.
    IfPresent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Expectations {
    preview: Expect,
    full_jpeg: Expect,
}

/// One parsed real file, as the same-stem pairing check sees it.
struct Member {
    is_jpeg: bool,
    path: PathBuf,
    identity: IdentityKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Mismatch {
    file: String,
    field: &'static str,
    ours: String,
    reference: String,
}

#[test]
#[ignore = "opt-in: needs ExifTool (and samples); see docs in oracle.rs"]
fn oracle_synthetic_matches_exiftool() {
    let [exiftool] = require_env(["PHOTOSORT_EXIFTOOL"]);

    let dir = tempfile::tempdir().expect("creates a temp directory");
    let cases = synthetic_cases();
    let mut files = Vec::with_capacity(cases.len());
    for (name, bytes, expect) in &cases {
        let path = dir.path().join(name);
        fs::write(&path, bytes).expect("writes a synthetic file");
        files.push((path, *expect));
    }

    let paths: Vec<PathBuf> = files.iter().map(|(path, _)| path.clone()).collect();
    let records = exiftool_records(&exiftool, &paths);
    let registry = Registry::with_builtin();
    let mut mismatches = Vec::new();
    for (path, expect) in &files {
        verify_file(
            &registry,
            path,
            records.get(&path_key(path)),
            *expect,
            &mut mismatches,
        );
    }

    assert_no_mismatches(files.len(), &mismatches);
}

#[test]
#[ignore = "opt-in: needs ExifTool (and samples); see docs in oracle.rs"]
fn oracle_samples_match_exiftool() {
    let [exiftool, samples] = require_env(["PHOTOSORT_EXIFTOOL", "PHOTOSORT_SAMPLES"]);
    assert!(
        samples.is_dir(),
        "PHOTOSORT_SAMPLES is not a folder: {}",
        samples.display()
    );

    let mut paths = Vec::new();
    collect_photos(&samples, &mut paths);
    paths.sort();
    assert!(
        !paths.is_empty(),
        "PHOTOSORT_SAMPLES holds no .jpg, .jpeg or .nef files: {}",
        samples.display()
    );

    let records = exiftool_records(&exiftool, &paths);
    let registry = Registry::with_builtin();
    let mut mismatches = Vec::new();
    // Grouped by (folder, stem): the same-stem JPEG and NEF of one capture.
    let mut groups: HashMap<(PathBuf, String), Vec<Member>> = HashMap::new();

    for path in &paths {
        let record = records.get(&path_key(path));
        let expect = real_expectations(path, record);
        if let Some(identity) = verify_file(&registry, path, record, expect, &mut mismatches) {
            let folder = path.parent().unwrap_or(Path::new("")).to_path_buf();
            let stem = path
                .file_stem()
                .and_then(OsStr::to_str)
                .unwrap_or_default()
                .to_owned();
            groups.entry((folder, stem)).or_default().push(Member {
                is_jpeg: is_jpeg(path),
                path: path.clone(),
                identity,
            });
        }
    }

    // AC4.5 on real data: the JPEG and NEF of one capture must share an identity key.
    let mut pairs = 0usize;
    for members in groups.values() {
        for jpeg in members.iter().filter(|m| m.is_jpeg) {
            for nef in members.iter().filter(|m| !m.is_jpeg) {
                pairs += 1;
                if jpeg.identity != nef.identity {
                    mismatches.push(Mismatch {
                        file: jpeg.path.display().to_string(),
                        field: "identity (vs same-stem NEF)",
                        ours: format!("{:?}", jpeg.identity),
                        reference: format!("{:?}", nef.identity),
                    });
                }
            }
        }
    }
    assert!(
        pairs > 0,
        "no same-stem JPEG+NEF pairs in {}, so AC4.5 is unverified",
        samples.display()
    );

    assert_no_mismatches(paths.len(), &mismatches);
}

/// Reads each named variable. Panics, naming every missing one, if any is
/// unset. Each returned path exists.
fn require_env<const N: usize>(names: [&str; N]) -> [PathBuf; N] {
    let missing: Vec<&str> = names
        .into_iter()
        .filter(|name| env::var_os(name).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "opt-in oracle test needs {} set (see the module docs in tests/oracle.rs)",
        missing.join(" and ")
    );
    names.map(|name| {
        let path = PathBuf::from(env::var_os(name).expect("checked above"));
        assert!(
            path.exists(),
            "{name} names a path that does not exist: {}",
            path.display()
        );
        path
    })
}

/// The synthetic files and what each must decode to.
fn synthetic_cases() -> Vec<(&'static str, Vec<u8>, Expectations)> {
    let exif = ExifSpec::zf();
    let preview = tiny_jpeg(SYNTH_PREVIEW.0, SYNTH_PREVIEW.1);
    let raw_jpeg = tiny_jpeg(SYNTH_FULL.0, SYNTH_FULL.1);
    let nikon = NikonSpec::zf(Some(preview.clone()));
    let nikon_inside = NikonSpec {
        preview_inside: true,
        ..nikon.clone()
    };

    let jpeg_mpf = Expectations {
        preview: Expect::Size(SYNTH_PREVIEW.0, SYNTH_PREVIEW.1),
        full_jpeg: Expect::Absent,
    };
    let jpeg_plain = Expectations {
        preview: Expect::Absent,
        full_jpeg: Expect::Absent,
    };
    let nef_preview = Expectations {
        preview: Expect::Size(SYNTH_PREVIEW.0, SYNTH_PREVIEW.1),
        full_jpeg: Expect::Size(SYNTH_FULL.0, SYNTH_FULL.1),
    };
    let nef_no_preview = Expectations {
        preview: Expect::Absent,
        full_jpeg: Expect::Size(SYNTH_FULL.0, SYNTH_FULL.1),
    };

    vec![
        (
            "jpeg-le-note-mpf.jpg",
            jpeg_file(
                Order::Little,
                Some(&exif),
                Some(&NikonSpec::zf(None)),
                Some(&preview),
            ),
            jpeg_mpf,
        ),
        (
            "jpeg-be-note-mpf.jpg",
            jpeg_file(
                Order::Big,
                Some(&exif),
                Some(&NikonSpec::zf(None)),
                Some(&preview),
            ),
            jpeg_mpf,
        ),
        (
            "jpeg-be-note-mpf-le.jpg",
            jpeg_file_with_mpf_order(
                Order::Big,
                Order::Little,
                Some(&exif),
                Some(&NikonSpec::zf(None)),
                Some(&preview),
            ),
            jpeg_mpf,
        ),
        (
            "jpeg-le-exif-only.jpg",
            jpeg_file(Order::Little, Some(&exif), None, None),
            jpeg_plain,
        ),
        (
            "jpeg-be-exif-only.jpg",
            jpeg_file(Order::Big, Some(&exif), None, None),
            jpeg_plain,
        ),
        (
            "nef-le-note-preview.nef",
            nef_file(Order::Little, &exif, Some(&nikon), Some(&raw_jpeg), 0),
            nef_preview,
        ),
        (
            "nef-be-note-inside.nef",
            nef_file(Order::Big, &exif, Some(&nikon_inside), Some(&raw_jpeg), 0),
            nef_preview,
        ),
        (
            "nef-be-note-padded.nef",
            nef_file(Order::Big, &exif, Some(&nikon), Some(&raw_jpeg), 200_000),
            nef_preview,
        ),
        (
            "nef-le-no-note.nef",
            nef_file(Order::Little, &exif, None, Some(&raw_jpeg), 0),
            nef_no_preview,
        ),
    ]
}

/// Expectations for a real file, taken from ExifTool's model. Z f files must
/// have the sizes from the plan. Other bodies only need previews that decode.
fn real_expectations(path: &Path, record: Option<&Map<String, Value>>) -> Expectations {
    let z_f = record
        .and_then(|r| reference_text(r, "IFD0:Model"))
        .as_deref()
        == Some(Z_F_MODEL);
    match (is_jpeg(path), z_f) {
        (true, true) => Expectations {
            preview: Expect::Size(Z_F_PREVIEW.0, Z_F_PREVIEW.1),
            full_jpeg: Expect::Absent,
        },
        (true, false) => Expectations {
            preview: Expect::IfPresent,
            full_jpeg: Expect::Absent,
        },
        (false, true) => Expectations {
            preview: Expect::Size(Z_F_PREVIEW.0, Z_F_PREVIEW.1),
            full_jpeg: Expect::Size(Z_F_FULL.0, Z_F_FULL.1),
        },
        (false, false) => Expectations {
            preview: Expect::IfPresent,
            full_jpeg: Expect::IfPresent,
        },
    }
}

/// Parses one file and records every disagreement with ExifTool and every
/// preview that fails its expectation. Returns the identity key when the
/// parse succeeded.
fn verify_file(
    registry: &Registry,
    path: &Path,
    record: Option<&Map<String, Value>>,
    expect: Expectations,
    mismatches: &mut Vec<Mismatch>,
) -> Option<IdentityKey> {
    let file = path.display().to_string();
    let Some(record) = record else {
        push(
            mismatches,
            &file,
            "exiftool record",
            "<parsed>".to_owned(),
            "<absent>".to_owned(),
        );
        return None;
    };

    let parsed = match parse_header(registry, path) {
        Ok(parsed) => parsed,
        Err(message) => {
            push(
                mismatches,
                &file,
                "parse",
                message,
                "parsed by ExifTool".to_owned(),
            );
            return None;
        }
    };

    compare_metadata(&file, &parsed.meta, record, mismatches);
    check_range(
        mismatches,
        &file,
        path,
        "preview",
        parsed.meta.preview,
        expect.preview,
    );
    check_range(
        mismatches,
        &file,
        path,
        "full_jpeg",
        parsed.meta.full_jpeg,
        expect.full_jpeg,
    );
    check_located(
        mismatches,
        &file,
        "preview offset (vs ExifTool)",
        parsed.meta.preview,
        &preview_references(record),
    );
    check_located(
        mismatches,
        &file,
        "full_jpeg offset (vs ExifTool)",
        parsed.meta.full_jpeg,
        &full_jpeg_references(record),
    );

    Some(identity_for(
        Some(&parsed.meta),
        parsed.file_len,
        &parsed.head,
    ))
}

struct Parsed {
    meta: Metadata,
    head: Vec<u8>,
    file_len: u64,
}

/// The shell's head loop. It starts with the reader's hint and, on `NeedMore`,
/// re-reads up to the length the parser asked for, capped at `MAX_HEAD_LEN`.
fn parse_header(registry: &Registry, path: &Path) -> Result<Parsed, String> {
    let ext = path
        .extension()
        .and_then(OsStr::to_str)
        .ok_or_else(|| "file has no extension".to_owned())?;
    let reader = registry
        .format_for_extension(ext)
        .ok_or_else(|| format!("no registered reader for .{ext}"))?;
    let kind = reader.kind();
    let file_len = fs::metadata(path).map_err(|e| e.to_string())?.len();

    let hint = u64::try_from(reader.header_len_hint()).unwrap_or(u64::MAX);
    let mut want = hint
        .min(file_len)
        .min(photosort_core::metadata::MAX_HEAD_LEN);
    loop {
        let head = read_head(path, want)?;
        match registry.parse(kind, &head, file_len) {
            Ok(meta) => {
                return Ok(Parsed {
                    meta,
                    head,
                    file_len,
                });
            }
            Err(ParseError::NeedMore { needed, have }) => {
                let next = needed
                    .min(file_len)
                    .min(photosort_core::metadata::MAX_HEAD_LEN);
                if next <= have {
                    return Err(format!(
                        "needs {needed} bytes, but {have} were read of a {file_len}-byte file"
                    ));
                }
                want = next;
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn read_head(path: &Path, len: u64) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    file.take(len)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    Ok(buf)
}

/// Field-by-field comparison. Absence on both sides is equal. Presence on one
/// side only is a mismatch.
fn compare_metadata(
    file: &str,
    ours: &Metadata,
    record: &Map<String, Value>,
    mismatches: &mut Vec<Mismatch>,
) {
    let text = |key: &str| reference_text(record, key);
    let number = |key: &str| reference_f64(record, key);

    check_eq(
        mismatches,
        file,
        "make",
        ours.make.clone(),
        text("IFD0:Make"),
    );
    check_eq(
        mismatches,
        file,
        "model",
        ours.model.clone(),
        text("IFD0:Model"),
    );
    check_eq(
        mismatches,
        file,
        "serial",
        ours.serial.clone(),
        text("ExifIFD:SerialNumber").or_else(|| text("Nikon:SerialNumber")),
    );
    check_eq(
        mismatches,
        file,
        "date_time_original",
        ours.date_time_original.clone(),
        text("ExifIFD:DateTimeOriginal"),
    );
    check_eq(
        mismatches,
        file,
        "subsec_time_original",
        ours.subsec_time_original.clone(),
        text("ExifIFD:SubSecTimeOriginal"),
    );
    check_eq(
        mismatches,
        file,
        "iso",
        ours.iso.map(|v| v.to_string()),
        text("ExifIFD:ISO"),
    );
    check_f64(
        mismatches,
        file,
        "exposure_time",
        ours.exposure_time.and_then(Rational::to_f64),
        number("ExifIFD:ExposureTime"),
    );
    check_f64(
        mismatches,
        file,
        "f_number",
        ours.f_number.and_then(Rational::to_f64),
        number("ExifIFD:FNumber"),
    );
    check_f64(
        mismatches,
        file,
        "focal_length",
        ours.focal_length.and_then(Rational::to_f64),
        number("ExifIFD:FocalLength"),
    );
    check_eq(
        mismatches,
        file,
        "lens_make",
        ours.lens_make.clone(),
        text("ExifIFD:LensMake"),
    );
    check_eq(
        mismatches,
        file,
        "lens_model",
        ours.lens_model.clone(),
        text("ExifIFD:LensModel"),
    );
    check_eq(
        mismatches,
        file,
        "shot_counter",
        ours.shot_counter.map(|v| v.to_string()),
        text("Nikon:ShutterCount"),
    );
}

/// Checks that a located range matches its expectation. A located range
/// must always decode as a JPEG starting with `FF D8`.
fn check_range(
    mismatches: &mut Vec<Mismatch>,
    file: &str,
    path: &Path,
    field: &'static str,
    range: Option<ByteRange>,
    expect: Expect,
) {
    let decoded = range.map(|r| decode_range(path, r));
    let reference = match expect {
        Expect::Absent => "<absent>".to_owned(),
        Expect::IfPresent => "decodes as a JPEG".to_owned(),
        Expect::Size(w, h) => format!("{w}x{h} JPEG"),
    };
    let ours = match (expect, &decoded) {
        (Expect::Absent, None) | (Expect::IfPresent, None) => return,
        (Expect::IfPresent, Some(Ok(_))) => return,
        (Expect::Size(w, h), Some(Ok((aw, ah)))) if (w, h) == (*aw, *ah) => return,
        (_, None) => "<absent>".to_owned(),
        (_, Some(Ok((aw, ah)))) => format!("{aw}x{ah} JPEG"),
        (_, Some(Err(message))) => format!("undecodable: {message}"),
    };
    push(mismatches, file, field, ours, reference);
}

/// Checks a located range against the offsets ExifTool reports for the same
/// structure. Absence on both sides is equal. A located range must equal one
/// of ExifTool's ranges, so an empty ExifTool list never matches a range.
fn check_located(
    mismatches: &mut Vec<Mismatch>,
    file: &str,
    field: &'static str,
    ours: Option<ByteRange>,
    reference: &[(u64, u64)],
) {
    let agrees = match ours {
        None => reference.is_empty(),
        Some(range) => reference.contains(&(range.offset, range.len)),
    };
    if agrees {
        return;
    }
    let ours = match ours {
        Some(range) => format!("offset {} len {}", range.offset, range.len),
        None => "<absent>".to_owned(),
    };
    let reference = if reference.is_empty() {
        "<absent>".to_owned()
    } else {
        reference
            .iter()
            .map(|(offset, len)| format!("offset {offset} len {len}"))
            .collect::<Vec<_>>()
            .join("; ")
    };
    push(mismatches, file, field, ours, reference);
}

/// ExifTool's embedded-JPEG preview ranges: the PreviewIFD preview (NEF and
/// maker note) and the MPF large thumbnail, whose `MPImageType` is 0x010001.
fn preview_references(record: &Map<String, Value>) -> Vec<(u64, u64)> {
    let mut ranges: Vec<(u64, u64)> =
        embedded_jpegs(record, "PreviewImageStart", "PreviewImageLength")
            .into_iter()
            .map(|(_, range)| range)
            .collect();
    ranges.extend(
        embedded_jpegs(record, "MPImageStart", "MPImageLength")
            .into_iter()
            .filter(|(group, _)| {
                reference_u64(record, &format!("{group}:MPImageType")) == Some(MPF_LARGE_THUMBNAIL)
            })
            .map(|(_, range)| range),
    );
    ranges
}

/// ExifTool's full-size JPEG ranges, from whichever group names JpgFromRaw.
fn full_jpeg_references(record: &Map<String, Value>) -> Vec<(u64, u64)> {
    embedded_jpegs(record, "JpgFromRawStart", "JpgFromRawLength")
        .into_iter()
        .map(|(_, range)| range)
        .collect()
}

/// Each `Group:start_tag` with its `Group:len_tag`, as (group, (offset, len)).
/// ExifTool's group names vary (`SubIFD`, `SubIFD1`, `MPImage2`), so every
/// group that has the start tag is taken.
fn embedded_jpegs(
    record: &Map<String, Value>,
    start_tag: &str,
    len_tag: &str,
) -> Vec<(String, (u64, u64))> {
    let suffix = format!(":{start_tag}");
    record
        .keys()
        .filter_map(|key| {
            let group = key.strip_suffix(&suffix)?;
            let start = reference_u64(record, key)?;
            let len = reference_u64(record, &format!("{group}:{len_tag}"))?;
            Some((group.to_owned(), (start, len)))
        })
        .collect()
}

/// Reads the located bytes from disk and decodes them. The bytes must start
/// with the JPEG SOI marker and must decode.
fn decode_range(path: &Path, range: ByteRange) -> Result<(u32, u32), String> {
    let bytes = read_range(path, range).map_err(|e| e.to_string())?;
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return Err(format!("{range:?} does not start with FF D8"));
    }
    let image = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    Ok((image.width(), image.height()))
}

fn read_range(path: &Path, range: ByteRange) -> io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(range.offset))?;
    let mut buf = Vec::new();
    file.take(range.len).read_to_end(&mut buf)?;
    if buf.len() as u64 != range.len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "located range extends past the end of the file",
        ));
    }
    Ok(buf)
}

/// Runs ExifTool in chunks and returns each record keyed by `SourceFile`
/// (forward slashes).
fn exiftool_records(exiftool: &Path, paths: &[PathBuf]) -> HashMap<String, Map<String, Value>> {
    let mut records = HashMap::new();
    for chunk in paths.chunks(EXIFTOOL_CHUNK) {
        let output = Command::new(exiftool)
            .args(["-json", "-G1", "-a", "-n"])
            .args(chunk)
            .output()
            .unwrap_or_else(|e| panic!("failed to run ExifTool at {}: {e}", exiftool.display()));
        assert!(
            output.status.success(),
            "ExifTool exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        let rows: Vec<Map<String, Value>> = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|e| panic!("ExifTool output is not a JSON array of objects: {e}"));
        for row in rows {
            let source = row
                .get("SourceFile")
                .and_then(Value::as_str)
                .map(|s| s.replace('\\', "/"));
            if let Some(source) = source {
                records.insert(source, row);
            }
        }
    }
    records
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn collect_photos(folder: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(folder).unwrap_or_else(|e| panic!("cannot read {}: {e}", folder.display()));
    for entry in entries {
        let path = entry
            .unwrap_or_else(|e| panic!("cannot list {}: {e}", folder.display()))
            .path();
        if path.is_dir() {
            collect_photos(&path, out);
        } else if is_photo(&path) {
            out.push(path);
        }
    }
}

fn is_photo(path: &Path) -> bool {
    matches!(
        extension_lower(path).as_deref(),
        Some("jpg" | "jpeg" | "nef")
    )
}

fn is_jpeg(path: &Path) -> bool {
    matches!(extension_lower(path).as_deref(), Some("jpg" | "jpeg"))
}

fn extension_lower(path: &Path) -> Option<String> {
    path.extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase)
}

/// A reference value as an unsigned integer, or `None` if it is absent or not a number.
fn reference_u64(record: &Map<String, Value>, key: &str) -> Option<u64> {
    match record.get(key)? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// A reference value as a string. ExifTool prints number-like strings
/// (serial `1234567`, SubSec `92`) as JSON numbers, so numbers are converted
/// with `to_string()`.
fn reference_text(record: &Map<String, Value>, key: &str) -> Option<String> {
    match record.get(key)? {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        other => Some(other.to_string()),
    }
}

fn reference_f64(record: &Map<String, Value>, key: &str) -> Option<f64> {
    match record.get(key)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn check_eq<T: PartialEq + Debug>(
    mismatches: &mut Vec<Mismatch>,
    file: &str,
    field: &'static str,
    ours: Option<T>,
    reference: Option<T>,
) {
    if ours != reference {
        push(mismatches, file, field, show(&ours), show(&reference));
    }
}

fn check_f64(
    mismatches: &mut Vec<Mismatch>,
    file: &str,
    field: &'static str,
    ours: Option<f64>,
    reference: Option<f64>,
) {
    let equal = match (ours, reference) {
        (None, None) => true,
        (Some(a), Some(b)) => (a - b).abs() <= FLOAT_RELATIVE_TOLERANCE * a.abs().max(b.abs()),
        _ => false,
    };
    if !equal {
        push(mismatches, file, field, show(&ours), show(&reference));
    }
}

fn show<T: Debug>(value: &Option<T>) -> String {
    match value {
        Some(v) => format!("{v:?}"),
        None => "<absent>".to_owned(),
    }
}

fn push(
    mismatches: &mut Vec<Mismatch>,
    file: &str,
    field: &'static str,
    ours: String,
    reference: String,
) {
    mismatches.push(Mismatch {
        file: file.to_owned(),
        field,
        ours,
        reference,
    });
}

fn assert_no_mismatches(files: usize, mismatches: &[Mismatch]) {
    if mismatches.is_empty() {
        eprintln!("oracle: {files} file(s) match ExifTool");
        return;
    }
    let mut table = String::new();
    writeln!(
        table,
        "{:<48} {:<28} {:<40} exiftool/expected",
        "file", "field", "ours"
    )
    .expect("writing to a String does not fail");
    for m in mismatches {
        writeln!(
            table,
            "{:<48} {:<28} {:<40} {}",
            m.file, m.field, m.ours, m.reference
        )
        .expect("writing to a String does not fail");
    }
    panic!(
        "{} mismatch(es) across {files} file(s):\n{table}",
        mismatches.len()
    );
}
