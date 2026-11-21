//! Pure decisions behind the app's view of a scan: progress text, the parse
//! result written into a shot, re-focusing after the capture-order sort, and
//! which previews to request.
// pattern: Functional Core

use std::fmt::Write;
use std::ops::Range;
use std::path::PathBuf;

use photosort_core::metadata::Registry;
use photosort_core::model::{ByteRange, Shot, ShotFile, ShotId};

use crate::scan::FileOutcome;

/// Where the scan is. `Failed` is terminal: the scanner sends nothing after
/// `ListFailed`, so nothing moves the phase out of it except a new scan.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum Phase {
    #[default]
    Idle,
    Listing,
    Parsing,
    Done,
    Failed(String),
}

/// Counts shown in the top bar.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct StatusCounts {
    pub shots: usize,
    pub skipped: usize,
    pub files_total: usize,
    pub files_parsed: usize,
    pub file_errors: usize,
}

/// The one-line status, such as `7,000 shots · parsed 3,210/7,700 · 2 errors`.
pub(crate) fn status_text(phase: &Phase, counts: &StatusCounts) -> String {
    match phase {
        Phase::Idle => "no source selected".to_owned(),
        Phase::Listing => "listing…".to_owned(),
        Phase::Failed(error) => format!("scan failed: {error}"),
        Phase::Parsing | Phase::Done => {
            let errors = if counts.file_errors == 1 {
                "error"
            } else {
                "errors"
            };
            let mut text = format!(
                "{} shots · parsed {}/{} · {} {errors}",
                group_thousands(counts.shots),
                group_thousands(counts.files_parsed),
                group_thousands(counts.files_total),
                group_thousands(counts.file_errors),
            );
            if counts.skipped > 0 {
                // Writing to a String cannot fail.
                let _ = write!(text, " · {} skipped", group_thousands(counts.skipped));
            }
            text
        }
    }
}

/// `1234567` becomes `"1,234,567"`.
pub(crate) fn group_thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Writes a parse result into its file. Returns true when the file failed.
pub(crate) fn apply_file_outcome(file: &mut ShotFile, outcome: FileOutcome) -> bool {
    file.identity = outcome.identity;
    match outcome.meta {
        Ok(meta) => {
            file.meta = Some(meta);
            file.error = None;
            false
        }
        Err(error) => {
            file.meta = None;
            file.error = Some(error);
            true
        }
    }
}

/// True when the shot has files and every one of them failed to parse.
pub(crate) fn is_unreadable(shot: &Shot) -> bool {
    !shot.files.is_empty() && shot.files.iter().all(|file| file.error.is_some())
}

/// The position that holds the same shot after the order changed. Falls back
/// to the nearest valid position if the shot is not in `new`.
pub(crate) fn position_after_reorder(old: &[ShotId], new: &[ShotId], position: usize) -> usize {
    let last = new.len().saturating_sub(1);
    let Some(&shot) = old.get(position) else {
        return position.min(last);
    };
    new.iter()
        .position(|&id| id == shot)
        .unwrap_or_else(|| position.min(last))
}

/// The shots in the rows `rows` of a grid with `columns` columns, in order.
pub(crate) fn visible_ids(order: &[ShotId], rows: Range<usize>, columns: usize) -> Vec<ShotId> {
    let start = rows.start.saturating_mul(columns).min(order.len());
    let end = rows.end.saturating_mul(columns).min(order.len());
    order.get(start..end).unwrap_or(&[]).to_vec()
}

/// The shot with this id. Ids are indices into the scan's shot list, but a
/// stale id from an earlier scan can be out of range, so this never indexes.
pub(crate) fn shot_at(shots: &[Shot], id: ShotId) -> Option<&Shot> {
    shots.get(id.0 as usize)
}

pub(crate) fn shot_at_mut(shots: &mut [Shot], id: ShotId) -> Option<&mut Shot> {
    shots.get_mut(id.0 as usize)
}

/// Rows needed to show `len` tiles in `columns` columns.
pub(crate) fn row_count(len: usize, columns: usize) -> usize {
    len.div_ceil(columns.max(1))
}

/// Where a shot's tile preview lives on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreviewSource {
    pub path: PathBuf,
    pub range: ByteRange,
    pub orientation: Option<u16>,
}

/// The preview to load for a tile, or `None` when no file has one.
pub(crate) fn preview_source(shot: &Shot, registry: &Registry) -> Option<PreviewSource> {
    let (index, range) = shot.tile_preview(registry)?;
    let file = shot.files.get(index)?;
    Some(PreviewSource {
        path: file.path.clone(),
        range,
        orientation: file.meta.as_ref().and_then(|meta| meta.orientation),
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::SystemTime;

    use photosort_core::grouping::group_files;
    use photosort_core::metadata::Registry;
    use photosort_core::model::{ByteRange, IdentityKey, ListedFile, Metadata, Shot, ShotId};

    use super::*;

    fn shot(path: &str) -> Shot {
        let listed = ListedFile {
            path: PathBuf::from(path),
            size: 1,
            modified: SystemTime::UNIX_EPOCH,
        };
        group_files(vec![listed], &Registry::with_builtin()).remove(0)
    }

    /// One shot made of a JPEG and a NEF with the same stem.
    fn pair(jpeg: &str, nef: &str) -> Shot {
        let listed = [jpeg, nef]
            .iter()
            .map(|path| ListedFile {
                path: PathBuf::from(path),
                size: 1,
                modified: SystemTime::UNIX_EPOCH,
            })
            .collect();
        group_files(listed, &Registry::with_builtin()).remove(0)
    }

    fn ids(values: &[u32]) -> Vec<ShotId> {
        values.iter().map(|&v| ShotId(v)).collect()
    }

    fn counts() -> StatusCounts {
        StatusCounts {
            shots: 7000,
            skipped: 0,
            files_total: 7700,
            files_parsed: 3210,
            file_errors: 2,
        }
    }

    // Status text

    #[test]
    fn status_line_groups_thousands_and_pluralises_errors() {
        assert_eq!(
            status_text(&Phase::Parsing, &counts()),
            "7,000 shots · parsed 3,210/7,700 · 2 errors"
        );
    }

    #[test]
    fn a_single_error_is_singular() {
        let one = StatusCounts {
            file_errors: 1,
            ..counts()
        };
        assert!(status_text(&Phase::Done, &one).ends_with("· 1 error"));
    }

    #[test]
    fn skipped_entries_are_shown_only_when_there_are_some() {
        let skipped = StatusCounts {
            skipped: 3,
            ..counts()
        };
        assert!(status_text(&Phase::Done, &skipped).ends_with("· 2 errors · 3 skipped"));
        assert!(!status_text(&Phase::Done, &counts()).contains("skipped"));
    }

    #[test]
    fn a_failed_scan_shows_its_error_in_the_status() {
        let failed = Phase::Failed("source is not a folder: D:/x".to_owned());
        assert_eq!(
            status_text(&failed, &counts()),
            "scan failed: source is not a folder: D:/x"
        );
    }

    #[test]
    fn idle_and_listing_have_their_own_text() {
        assert_eq!(status_text(&Phase::Idle, &counts()), "no source selected");
        assert_eq!(status_text(&Phase::Listing, &counts()), "listing…");
    }

    #[test]
    fn group_thousands_inserts_a_comma_every_three_digits() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1000), "1,000");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
    }

    // Parse results

    #[test]
    fn a_successful_parse_sets_metadata_and_identity_and_clears_the_error() {
        let mut file = shot("/card/DSC_0001.JPG").files.remove(0);
        file.error = Some("stale".to_owned());
        let identity = IdentityKey::Content {
            size: 10,
            head_blake3: [1; 32],
        };
        let outcome = FileOutcome {
            meta: Ok(Metadata {
                iso: Some(100),
                ..Metadata::default()
            }),
            identity: Some(identity.clone()),
        };
        assert!(!apply_file_outcome(&mut file, outcome));
        assert_eq!(file.meta.and_then(|m| m.iso), Some(100));
        assert_eq!(file.identity, Some(identity));
        assert_eq!(file.error, None);
    }

    #[test]
    fn a_failed_parse_records_the_error_and_reports_it() {
        let mut file = shot("/card/DSC_0001.JPG").files.remove(0);
        let outcome = FileOutcome {
            meta: Err("malformed jpeg".to_owned()),
            identity: None,
        };
        assert!(apply_file_outcome(&mut file, outcome));
        assert_eq!(file.meta, None);
        assert_eq!(file.error.as_deref(), Some("malformed jpeg"));
    }

    #[test]
    fn a_shot_is_unreadable_only_when_every_file_failed() {
        let mut shot = shot("/card/DSC_0001.JPG");
        assert!(
            !is_unreadable(&shot),
            "a file with no result yet is not unreadable"
        );
        shot.files[0].error = Some("bad".to_owned());
        assert!(is_unreadable(&shot));
    }

    #[test]
    fn a_shot_with_one_readable_file_is_not_unreadable() {
        let mut shot = pair("/card/DSC_0001.JPG", "/card/DSC_0001.NEF");
        shot.files[0].error = Some("bad".to_owned());
        assert!(!is_unreadable(&shot));
    }

    // Re-focusing after the capture-order sort

    #[test]
    fn focus_follows_the_same_shot_across_a_reorder() {
        let old = ids(&[0, 1, 2, 3]);
        let new = ids(&[2, 0, 3, 1]);
        // Position 1 held ShotId 1, which is now at position 3.
        assert_eq!(position_after_reorder(&old, &new, 1), 3);
    }

    #[test]
    fn a_focus_past_the_end_is_clamped_to_the_last_shot() {
        assert_eq!(position_after_reorder(&ids(&[0, 1]), &ids(&[1, 0]), 9), 1);
    }

    #[test]
    fn re_focusing_an_empty_grid_gives_zero() {
        assert_eq!(position_after_reorder(&ids(&[0, 1]), &[], 1), 0);
        assert_eq!(position_after_reorder(&[], &[], 0), 0);
    }

    // Visible rows

    #[test]
    fn the_visible_ids_are_the_shots_in_the_rendered_rows() {
        let order = ids(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(visible_ids(&order, 1..3, 4), ids(&[4, 5, 6, 7, 8, 9]));
    }

    #[test]
    fn visible_ids_past_the_end_are_clipped_and_never_panic() {
        let order = ids(&[0, 1, 2]);
        assert_eq!(visible_ids(&order, 5..9, 4), Vec::<ShotId>::new());
        assert_eq!(visible_ids(&[], 0..4, 4), Vec::<ShotId>::new());
    }

    #[test]
    fn a_stale_id_past_the_end_resolves_to_nothing() {
        let mut shots = vec![shot("/card/DSC_0001.JPG")];
        assert!(shot_at(&shots, ShotId(0)).is_some());
        assert!(shot_at(&shots, ShotId(1)).is_none());
        assert!(shot_at_mut(&mut shots, ShotId(5)).is_none());
    }

    #[test]
    fn row_count_rounds_up_and_has_a_column_floor() {
        assert_eq!(row_count(0, 4), 0);
        assert_eq!(row_count(7, 3), 3);
        assert_eq!(row_count(6, 3), 2);
        assert_eq!(row_count(5, 0), 5);
    }

    // Preview requests

    #[test]
    fn a_shot_without_parsed_metadata_has_no_preview_source() {
        let shot = shot("/card/DSC_0001.JPG");
        assert_eq!(preview_source(&shot, &Registry::with_builtin()), None);
    }

    #[test]
    fn the_preview_source_carries_the_path_range_and_orientation() {
        let mut shot = shot("/card/DSC_0001.JPG");
        shot.files[0].meta = Some(Metadata {
            preview: Some(ByteRange {
                offset: 100,
                len: 2000,
            }),
            orientation: Some(6),
            ..Metadata::default()
        });
        assert_eq!(
            preview_source(&shot, &Registry::with_builtin()),
            Some(PreviewSource {
                path: PathBuf::from("/card/DSC_0001.JPG"),
                range: ByteRange {
                    offset: 100,
                    len: 2000,
                },
                orientation: Some(6),
            })
        );
    }
}
