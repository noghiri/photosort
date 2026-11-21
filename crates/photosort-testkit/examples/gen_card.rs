//! Writes a synthetic SD card (`DCIM/100NZ_F`, `101NZ_F`, ...) for scan tests
//! and manual checks.
//!
//! Usage: `gen_card <out_dir> <shots> [--raw-every N]`
//!
//! `out_dir` must be missing or empty. The run refuses to write into a
//! non-empty directory, so it never mixes its files with an earlier card.
// pattern: Imperative Shell

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use photosort_testkit::{ExifSpec, NikonSpec, Order, jpeg_file, nef_file, tiny_jpeg};

const USAGE: &str = "usage: gen_card <out_dir> <shots> [--raw-every N]";
/// Folders hold up to 999 shots, as on a real camera card.
const SHOTS_PER_FOLDER: u32 = 999;
/// One shot per second, so a day is the largest run with distinct capture times.
const MAX_SHOTS: u32 = 86_400;
const DEFAULT_RAW_EVERY: u32 = 10;
const FIRST_FOLDER: u32 = 100;
const SHUTTER_BASE: u32 = 75_036;
const PREVIEW_WIDTH: u32 = 640;
const PREVIEW_HEIGHT: u32 = 424;
/// Bytes kept from a valid JPEG to make a truncated one.
const TRUNCATED_LEN: usize = 100;
/// Edge-case files, all in the first folder.
const EDGE_NAMES: [&str; 5] = [
    "CLIP_0001.MOV",
    "DSC_0001.xmp",
    "TRUNC_0001.JPG",
    "EMPTY_0001.NEF",
    "NOPV_0001.JPG",
];

/// Folder for the shot at `index` (0-based), e.g. `100NZ_F`.
fn folder_name(index: u32) -> String {
    format!("{}NZ_F", FIRST_FOLDER + index / SHOTS_PER_FOLDER)
}

/// Stem shared by a shot's JPEG and NEF. The counter restarts in each folder.
fn stem(index: u32) -> String {
    format!("DSC_{:04}", index % SHOTS_PER_FOLDER + 1)
}

/// `(DateTimeOriginal, SubSecTimeOriginal)` for the shot at `index`.
fn capture_time(index: u32) -> (String, String) {
    let (h, m, s) = (index / 3600, index / 60 % 60, index % 60);
    (
        format!("2026:11:20 {h:02}:{m:02}:{s:02}"),
        format!("{:02}", index % 100),
    )
}

/// Whether the shot at `index` also gets a NEF. `raw_every == 0` means never.
fn has_raw(index: u32, raw_every: u32) -> bool {
    raw_every > 0 && (index + 1).is_multiple_of(raw_every)
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    out_dir: PathBuf,
    shots: u32,
    raw_every: u32,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut positional = Vec::new();
    let mut raw_every = DEFAULT_RAW_EVERY;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--raw-every" {
            let value = iter
                .next()
                .ok_or_else(|| format!("--raw-every needs a value\n{USAGE}"))?;
            raw_every = value
                .parse()
                .map_err(|_| format!("invalid --raw-every value {value:?}"))?;
        } else {
            positional.push(arg);
        }
    }
    let [out_dir, shots] = positional.as_slice() else {
        return Err(USAGE.to_owned());
    };
    let shots: u32 = shots
        .parse()
        .map_err(|_| format!("invalid shot count {shots:?}"))?;
    if shots == 0 || shots > MAX_SHOTS {
        return Err(format!("shot count must be between 1 and {MAX_SHOTS}"));
    }
    Ok(Args {
        out_dir: PathBuf::from(out_dir),
        shots,
        raw_every,
    })
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    match parse_args(&raw).and_then(run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("gen_card: {message}");
            ExitCode::from(2)
        }
    }
}

fn run(args: Args) -> Result<(), String> {
    let started = Instant::now();
    refuse_non_empty(&args.out_dir)?;
    // One preview serves every file, so generating 7,000 shots stays fast.
    let preview = tiny_jpeg(PREVIEW_WIDTH, PREVIEW_HEIGHT);
    for index in 0..args.shots {
        write_shot(&args.out_dir, index, args.raw_every, &preview)?;
    }
    write_edge_cases(&args.out_dir, &preview)?;
    print_summary(&args.out_dir, args.shots, started.elapsed().as_secs_f64())
}

fn refuse_non_empty(out_dir: &Path) -> Result<(), String> {
    match fs::read_dir(out_dir) {
        Ok(mut entries) => match entries.next() {
            None => Ok(()),
            Some(_) => Err(format!(
                "{} is not empty; choose a missing or empty directory",
                out_dir.display()
            )),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("failed to read {}: {e}", out_dir.display())),
    }
}

/// The Exif block for the shot at `index`, with its capture time.
fn exif_for(index: u32) -> ExifSpec {
    let (dto, subsec) = capture_time(index);
    ExifSpec {
        date_time_original: Some(dto),
        subsec_time_original: Some(subsec),
        ..ExifSpec::zf()
    }
}

/// A JPEG with Exif, a Nikon counter and an MPF preview.
fn jpeg_for(index: u32, preview: &[u8]) -> Vec<u8> {
    let nikon = NikonSpec {
        shutter_count: Some(SHUTTER_BASE + index),
        ..NikonSpec::zf(None)
    };
    jpeg_file(
        Order::Big,
        Some(&exif_for(index)),
        Some(&nikon),
        Some(preview),
    )
}

fn write_shot(out_dir: &Path, index: u32, raw_every: u32, preview: &[u8]) -> Result<(), String> {
    let dir = out_dir.join("DCIM").join(folder_name(index));
    fs::create_dir_all(&dir).map_err(|e| io_error(&dir, &e))?;
    let stem = stem(index);
    write_file(&dir.join(format!("{stem}.JPG")), &jpeg_for(index, preview))?;
    if has_raw(index, raw_every) {
        let nikon = NikonSpec {
            shutter_count: Some(SHUTTER_BASE + index),
            ..NikonSpec::zf(Some(preview.to_vec()))
        };
        let nef = nef_file(Order::Big, &exif_for(index), Some(&nikon), None, 0);
        write_file(&dir.join(format!("{stem}.NEF")), &nef)?;
    }
    Ok(())
}

/// The five files that exercise AC1.3 and the AC2.8 placeholder path.
fn write_edge_cases(out_dir: &Path, preview: &[u8]) -> Result<(), String> {
    let dir = out_dir.join("DCIM").join(folder_name(0));
    fs::create_dir_all(&dir).map_err(|e| io_error(&dir, &e))?;
    let [mov, xmp, truncated, empty_nef, no_preview] = EDGE_NAMES;

    // Deterministic bytes, so the file is the same on every run.
    let junk: Vec<u8> = (0u32..4096)
        .map(|i| (i.wrapping_mul(31) ^ (i >> 3)) as u8)
        .collect();
    write_file(&dir.join(mov), &junk)?;
    write_file(
        &dir.join(xmp),
        b"<?xpacket begin=\"\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\
          <x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>",
    )?;

    let valid = jpeg_for(0, preview);
    let head: Vec<u8> = valid.iter().copied().take(TRUNCATED_LEN).collect();
    write_file(&dir.join(truncated), &head)?;
    write_file(&dir.join(empty_nef), &[])?;

    // Readable and with Exif, but no MPF preview, so the tile has no preview.
    let nikon = NikonSpec {
        shutter_count: Some(1),
        ..NikonSpec::zf(None)
    };
    let readable = jpeg_file(Order::Big, Some(&exif_for(0)), Some(&nikon), None);
    write_file(&dir.join(no_preview), &readable)
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    fs::write(path, bytes).map_err(|e| io_error(path, &e))
}

fn io_error(path: &Path, e: &io::Error) -> String {
    format!("failed to write {}: {e}", path.display())
}

fn print_summary(out_dir: &Path, shots: u32, seconds: f64) -> Result<(), String> {
    let dcim = out_dir.join("DCIM");
    let read =
        fs::read_dir(&dcim).map_err(|e| format!("failed to read {}: {e}", dcim.display()))?;
    let mut folders: Vec<PathBuf> = read
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .collect();
    folders.sort();
    println!(
        "wrote {shots} shots to {} in {seconds:.2} s",
        dcim.display()
    );
    for folder in &folders {
        let count = fs::read_dir(folder)
            .map_err(|e| format!("failed to read {}: {e}", folder.display()))?
            .count();
        let name = folder.file_name().unwrap_or_default().to_string_lossy();
        println!("  {name}: {count} files");
    }
    let first = dcim.join(folder_name(0));
    for name in EDGE_NAMES {
        let present = first.join(name).is_file();
        println!(
            "  edge {name}: {}",
            if present { "present" } else { "MISSING" }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn folders_hold_999_shots_then_move_on() {
        assert_eq!(folder_name(0), "100NZ_F");
        assert_eq!(folder_name(998), "100NZ_F");
        assert_eq!(folder_name(999), "101NZ_F");
        assert_eq!(folder_name(6999), "107NZ_F");
    }

    #[test]
    fn file_counter_restarts_in_each_folder() {
        assert_eq!(stem(0), "DSC_0001");
        assert_eq!(stem(998), "DSC_0999");
        assert_eq!(stem(999), "DSC_0001");
    }

    #[test]
    fn capture_time_advances_one_second_per_shot_with_cycling_subsec() {
        assert_eq!(
            capture_time(0),
            ("2026:11:20 00:00:00".to_owned(), "00".to_owned())
        );
        assert_eq!(
            capture_time(1),
            ("2026:11:20 00:00:01".to_owned(), "01".to_owned())
        );
        assert_eq!(
            capture_time(3661),
            ("2026:11:20 01:01:01".to_owned(), "61".to_owned())
        );
        assert_eq!(capture_time(100).1, "00");
    }

    #[test]
    fn every_nth_shot_gets_a_raw_counting_from_one() {
        assert!(!has_raw(0, 10));
        assert!(has_raw(9, 10));
        assert!(has_raw(19, 10));
        assert!(!has_raw(10, 10));
    }

    #[test]
    fn zero_raw_every_writes_no_raw() {
        assert!(!has_raw(0, 0));
        assert!(!has_raw(9, 0));
    }

    #[test]
    fn parses_positional_args_with_default_raw_cadence() {
        let parsed = parse_args(&args(&["out", "7000"])).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            parsed,
            Args {
                out_dir: "out".into(),
                shots: 7000,
                raw_every: 10,
            }
        );
    }

    #[test]
    fn parses_raw_every_before_or_after_positionals() {
        let before = parse_args(&args(&["--raw-every", "4", "out", "7"]));
        let after = parse_args(&args(&["out", "7", "--raw-every", "4"]));
        assert_eq!(before, after);
        assert_eq!(before.map(|a| a.raw_every), Ok(4));
    }

    #[test]
    fn rejects_missing_or_bad_arguments() {
        assert!(parse_args(&args(&[])).is_err());
        assert!(parse_args(&args(&["out"])).is_err());
        assert!(parse_args(&args(&["out", "x"])).is_err());
        assert!(parse_args(&args(&["out", "0"])).is_err());
        assert!(parse_args(&args(&["out", "86401"])).is_err());
        assert!(parse_args(&args(&["out", "7", "--raw-every"])).is_err());
        assert!(parse_args(&args(&["out", "7", "--raw-every", "many"])).is_err());
    }
}
