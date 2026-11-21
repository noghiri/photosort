//! Groups listed files into shots, picks each shot's tile preview and orders
//! shots by capture time.
// pattern: Functional Core

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::metadata::{FormatReader, Registry};
use crate::model::{ByteRange, FileKind, ListedFile, Shot, ShotFile, ShotId};

/// Drops files with an unregistered extension, then groups the rest by
/// directory and lowercase stem. Shots come out in path order.
///
/// A second file of a kind already in a group starts a new shot, so no
/// registered file is lost.
pub fn group_files(mut files: Vec<ListedFile>, registry: &Registry) -> Vec<Shot> {
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let mut shots: Vec<Shot> = Vec::new();
    let mut by_group: HashMap<(PathBuf, String), Vec<usize>> = HashMap::new();
    for file in files {
        let Some(reader) = file
            .path
            .extension()
            .and_then(|ext| ext.to_str())
            .and_then(|ext| registry.format_for_extension(ext))
        else {
            continue;
        };
        let kind = reader.kind();
        let Some(stem) = file.path.file_stem() else {
            continue;
        };
        let dir = file
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let key = (dir.clone(), stem.to_string_lossy().to_lowercase());
        let candidates = by_group.entry(key).or_default();
        let existing = candidates
            .iter()
            .copied()
            .find(|&i| shots.get(i).is_some_and(|shot| !has_kind(shot, kind)));
        let index = match existing {
            Some(index) => index,
            None => {
                let index = shots.len();
                shots.push(Shot {
                    id: ShotId(0),
                    dir,
                    stem: stem.to_string_lossy().into_owned(),
                    files: Vec::new(),
                });
                candidates.push(index);
                index
            }
        };
        if let Some(shot) = shots.get_mut(index) {
            shot.files.push(ShotFile {
                path: file.path,
                kind,
                size: file.size,
                modified: file.modified,
                meta: None,
                identity: None,
                error: None,
            });
        }
    }
    // A card cannot hold 2^32 image files, so the conversion never fails in
    // practice. Failing it drops the shot rather than reusing an id.
    shots
        .into_iter()
        .enumerate()
        .filter_map(|(i, shot)| {
            let id = ShotId(u32::try_from(i).ok()?);
            Some(Shot { id, ..shot })
        })
        .collect()
}

fn has_kind(shot: &Shot, kind: FileKind) -> bool {
    shot.files.iter().any(|f| f.kind == kind)
}

impl Shot {
    /// Format badges joined with `+`, non-raw first: `"J"`, `"N"`, `"J+N"`.
    pub fn badge(&self, registry: &Registry) -> String {
        let mut readers: Vec<&dyn FormatReader> = self
            .files
            .iter()
            .filter_map(|f| registry.format_for_kind(f.kind))
            .collect();
        // `false` sorts before `true`, so non-raw readers come first.
        readers.sort_by_key(|r| r.is_raw());
        readers
            .iter()
            .map(|r| r.badge())
            .collect::<Vec<_>>()
            .join("+")
    }

    /// `DateTimeOriginal` and its subsec string from the first file that has
    /// a `DateTimeOriginal`. The subsec is `""` when absent.
    pub fn capture_time(&self) -> Option<(&str, &str)> {
        self.files.iter().find_map(|file| {
            let meta = file.meta.as_ref()?;
            let dto = meta
                .date_time_original
                .as_deref()
                .filter(|s| !s.is_empty())?;
            Some((dto, meta.subsec_time_original.as_deref().unwrap_or("")))
        })
    }

    /// The embedded preview of the first non-raw file that has one, else of
    /// a raw file. Never the full-size JPEG. `usize` indexes `self.files`.
    pub fn tile_preview(&self, registry: &Registry) -> Option<(usize, ByteRange)> {
        let find = |want_raw: bool| {
            self.files.iter().enumerate().find_map(|(i, file)| {
                let raw = registry.format_for_kind(file.kind)?.is_raw();
                if raw != want_raw {
                    return None;
                }
                Some((i, file.meta.as_ref()?.preview?))
            })
        };
        find(false).or_else(|| find(true))
    }

    /// The first file's file name.
    pub fn display_name(&self) -> String {
        self.files
            .first()
            .and_then(|f| f.path.file_name())
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
    }
}

/// Shots with a capture time come first, ordered by `DateTimeOriginal`,
/// then subsec, then name. Shots without one follow, in name order.
pub fn capture_order(shots: &[Shot]) -> Vec<ShotId> {
    let mut keys: Vec<SortKey> = shots.iter().map(sort_key).collect();
    keys.sort();
    keys.into_iter().map(|(.., id)| id).collect()
}

/// `(undated, dto, subsec, lowercase name, id)`. `false` sorts before
/// `true`, so dated shots come first. Trailing zeros are dropped from the
/// subsec: for digit strings, a lexical comparison of the trimmed form is a
/// numeric comparison of the fraction (`"9"` is 0.9, which sorts after
/// `"10"` = 0.1). The id makes the order total.
type SortKey = (bool, String, String, String, ShotId);

fn sort_key(shot: &Shot) -> SortKey {
    let time = shot.capture_time();
    let (dto, subsec) = time.unwrap_or_default();
    (
        time.is_none(),
        dto.to_owned(),
        subsec.trim_end_matches('0').to_owned(),
        shot.display_name().to_lowercase(),
        shot.id,
    )
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use proptest::prelude::*;

    use super::*;
    use crate::metadata::formats::nef;
    use crate::model::Metadata;

    fn listed(path: &str) -> ListedFile {
        ListedFile {
            path: PathBuf::from(path),
            size: 1,
            modified: SystemTime::UNIX_EPOCH,
        }
    }

    fn group(paths: &[&str]) -> Vec<Shot> {
        let files = paths.iter().map(|p| listed(p)).collect();
        group_files(files, &Registry::with_builtin())
    }

    /// One undated shot for `path`, with the given id.
    fn shot_at(id: u32, path: &str) -> Shot {
        let mut shot = group(&[path]).remove(0);
        shot.id = ShotId(id);
        shot
    }

    /// A shot whose first file has the given capture time.
    fn dated_at(id: u32, path: &str, dto: &str, subsec: &str) -> Shot {
        let mut shot = shot_at(id, path);
        if let Some(file) = shot.files.first_mut() {
            file.meta = Some(Metadata {
                date_time_original: Some(dto.to_owned()),
                subsec_time_original: Some(subsec.to_owned()),
                ..Metadata::default()
            });
        }
        shot
    }

    fn names(shots: &[Shot], order: &[ShotId]) -> Vec<String> {
        order
            .iter()
            .filter_map(|id| shots.iter().find(|s| s.id == *id))
            .map(Shot::display_name)
            .collect()
    }

    // AC1.2

    #[test]
    fn jpeg_and_raw_with_the_same_stem_in_one_directory_form_one_shot() {
        let shots = group(&["100NZ_F/DSC_0001.JPG", "100NZ_F/DSC_0001.NEF"]);
        assert_eq!(shots.len(), 1);
        assert_eq!(shots[0].files.len(), 2);
    }

    #[test]
    fn same_stem_in_different_directories_forms_separate_shots() {
        let shots = group(&["100NZ_F/DSC_0001.JPG", "101NZ_F/DSC_0001.JPG"]);
        assert_eq!(shots.len(), 2);
        assert_eq!(shots[0].dir, PathBuf::from("100NZ_F"));
        assert_eq!(shots[1].dir, PathBuf::from("101NZ_F"));
    }

    #[test]
    fn stems_match_regardless_of_case() {
        let shots = group(&["dsc_0001.jpg", "DSC_0001.NEF"]);
        assert_eq!(shots.len(), 1);
        assert_eq!(shots[0].files.len(), 2);
    }

    // AC1.3

    #[test]
    fn unregistered_extensions_produce_no_shot() {
        let shots = group(&["DSC_0001.MOV", "DSC_0001.xmp", "DSC_0002.dop"]);
        assert!(shots.is_empty());
    }

    #[test]
    fn unregistered_file_does_not_join_a_registered_shot_with_the_same_stem() {
        let shots = group(&["DSC_0001.JPG", "DSC_0001.xmp"]);
        assert_eq!(shots.len(), 1);
        assert_eq!(shots[0].files.len(), 1);
    }

    // Same-kind duplicates keep every file.

    #[test]
    fn two_files_of_the_same_kind_form_two_shots() {
        let shots = group(&["a.jpg", "a.JPEG"]);
        assert_eq!(shots.len(), 2);
        assert!(shots.iter().all(|s| s.files.len() == 1));
    }

    #[test]
    fn shot_ids_follow_path_order_and_are_unique() {
        let shots = group(&["b/x.jpg", "a/y.nef", "a/x.jpg"]);
        let ids: Vec<ShotId> = shots.iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![ShotId(0), ShotId(1), ShotId(2)]);
        assert_eq!(shots[0].dir, PathBuf::from("a"));
    }

    // AC2.2

    #[test]
    fn badges_are_j_n_and_j_plus_n() {
        let reg = Registry::with_builtin();
        let jpeg = group(&["DSC_0001.JPG"]);
        let raw = group(&["DSC_0002.NEF"]);
        let both = group(&["DSC_0003.JPG", "DSC_0003.NEF"]);
        assert_eq!(jpeg[0].badge(&reg), "J");
        assert_eq!(raw[0].badge(&reg), "N");
        assert_eq!(both[0].badge(&reg), "J+N");
    }

    #[test]
    fn badge_puts_the_non_raw_file_first_whatever_the_path_order() {
        // "A.NEF" sorts before "a.jpg" by path, yet the badge still reads J+N.
        let reg = Registry::with_builtin();
        let shots = group(&["dir/A.NEF", "dir/a.jpg"]);
        assert_eq!(shots.len(), 1);
        assert_eq!(shots[0].badge(&reg), "J+N");
    }

    // AC2.8

    #[test]
    fn tile_preview_uses_the_raw_preview_and_never_the_full_jpeg() {
        let reg = Registry::with_builtin();
        let mut shots = group(&["DSC_0001.NEF"]);
        let preview = ByteRange {
            offset: 100,
            len: 50,
        };
        let full = ByteRange {
            offset: 200,
            len: 9000,
        };
        if let Some(file) = shots[0].files.first_mut() {
            file.meta = Some(Metadata {
                preview: Some(preview),
                full_jpeg: Some(full),
                ..Metadata::default()
            });
        }
        assert_eq!(shots[0].tile_preview(&reg), Some((0, preview)));
    }

    #[test]
    fn tile_preview_is_none_when_no_file_has_an_embedded_preview() {
        let reg = Registry::with_builtin();
        let mut shots = group(&["DSC_0001.NEF"]);
        if let Some(file) = shots[0].files.first_mut() {
            file.meta = Some(Metadata {
                full_jpeg: Some(ByteRange {
                    offset: 200,
                    len: 9000,
                }),
                ..Metadata::default()
            });
        }
        assert_eq!(shots[0].tile_preview(&reg), None);
    }

    #[test]
    fn tile_preview_is_none_for_a_shot_without_metadata() {
        let reg = Registry::with_builtin();
        let shots = group(&["DSC_0001.JPG"]);
        assert_eq!(shots[0].tile_preview(&reg), None);
    }

    #[test]
    fn tile_preview_prefers_the_non_raw_file_over_the_raw_one() {
        let reg = Registry::with_builtin();
        // "A.NEF" sorts before "a.jpg", so the raw file is at index 0 and only
        // the non-raw rule can pick the JPEG at index 1.
        let mut shots = group(&["dir/A.NEF", "dir/a.jpg"]);
        let jpeg_preview = ByteRange {
            offset: 10,
            len: 20,
        };
        let raw_preview = ByteRange {
            offset: 30,
            len: 40,
        };
        for file in &mut shots[0].files {
            let preview = if file.kind == nef::KIND {
                raw_preview
            } else {
                jpeg_preview
            };
            file.meta = Some(Metadata {
                preview: Some(preview),
                ..Metadata::default()
            });
        }
        assert_eq!(shots[0].tile_preview(&reg), Some((1, jpeg_preview)));
    }

    // AC2.1

    #[test]
    fn capture_order_compares_subseconds_as_fractions() {
        let shots = vec![
            dated_at(0, "c.jpg", "2026:11:20 10:00:32", "92"),
            dated_at(1, "b.jpg", "2026:11:20 10:00:32", "9"),
            dated_at(2, "a.jpg", "2026:11:20 10:00:31", "99"),
        ];
        let order = capture_order(&shots);
        assert_eq!(names(&shots, &order), vec!["a.jpg", "b.jpg", "c.jpg"]);
    }

    #[test]
    fn subsec_comparison_is_numeric_not_textual() {
        // "10" is 0.10 and "9" is 0.9, so "10" comes first.
        let shots = vec![
            dated_at(0, "y.jpg", "2026:11:20 10:00:32", "9"),
            dated_at(1, "x.jpg", "2026:11:20 10:00:32", "10"),
        ];
        let order = capture_order(&shots);
        assert_eq!(names(&shots, &order), vec!["x.jpg", "y.jpg"]);
    }

    #[test]
    fn equal_capture_times_fall_back_to_the_name_case_insensitively() {
        let shots = vec![
            dated_at(0, "b.jpg", "2026:11:20 10:00:00", "00"),
            dated_at(1, "A.jpg", "2026:11:20 10:00:00", "00"),
        ];
        let order = capture_order(&shots);
        assert_eq!(names(&shots, &order), vec!["A.jpg", "b.jpg"]);
    }

    #[test]
    fn undated_shots_come_last_in_name_order() {
        let shots = vec![
            shot_at(0, "zeta.jpg"),
            dated_at(1, "mid.jpg", "2026:11:20 10:00:00", "00"),
            shot_at(2, "Alpha.jpg"),
        ];
        let order = capture_order(&shots);
        assert_eq!(
            names(&shots, &order),
            vec!["mid.jpg", "Alpha.jpg", "zeta.jpg"]
        );
    }

    #[test]
    fn empty_dto_counts_as_undated() {
        let shots = vec![
            dated_at(0, "blank.jpg", "", "00"),
            dated_at(1, "dated.jpg", "2026:11:20 10:00:00", "00"),
        ];
        let order = capture_order(&shots);
        assert_eq!(names(&shots, &order), vec!["dated.jpg", "blank.jpg"]);
    }

    #[test]
    fn capture_time_takes_the_first_file_that_has_a_dto() {
        let mut shot = group(&["DSC_0001.JPG", "DSC_0001.NEF"]).remove(0);
        if let Some(file) = shot.files.get_mut(1) {
            file.meta = Some(Metadata {
                date_time_original: Some("2026:11:20 10:00:00".to_owned()),
                subsec_time_original: Some("5".to_owned()),
                ..Metadata::default()
            });
        }
        assert_eq!(shot.capture_time(), Some(("2026:11:20 10:00:00", "5")));
    }

    #[test]
    fn each_shot_appears_exactly_once_in_capture_order() {
        let shots = vec![
            dated_at(0, "a.jpg", "2026:11:20 10:00:02", "00"),
            shot_at(1, "b.jpg"),
            dated_at(2, "c.jpg", "2026:11:20 10:00:01", "00"),
            shot_at(3, "d.nef"),
        ];
        let mut order = capture_order(&shots);
        assert_eq!(order.len(), 4);
        order.sort();
        assert_eq!(order, vec![ShotId(0), ShotId(1), ShotId(2), ShotId(3)]);
    }

    proptest! {
        #[test]
        fn capture_order_is_a_permutation_of_the_shots(
            seeds in prop::collection::vec((0u8..3, "[0-9]{0,4}"), 0..40),
        ) {
            let shots: Vec<Shot> = seeds
                .iter()
                .enumerate()
                .map(|(i, (day, subsec))| {
                    let dto = format!("2026:11:{:02} 10:00:00", 20 + day);
                    let id = u32::try_from(i).unwrap_or(u32::MAX);
                    dated_at(id, &format!("d/f{i}.jpg"), &dto, subsec)
                })
                .collect();
            let mut order = capture_order(&shots);
            prop_assert_eq!(order.len(), shots.len());
            order.sort();
            order.dedup();
            prop_assert_eq!(order.len(), shots.len());
        }
    }
}
