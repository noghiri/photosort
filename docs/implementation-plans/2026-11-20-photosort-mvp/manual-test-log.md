# Manual test log

Results of manual and opt-in checks, one section per phase. Later phases append their own results to their section.

## Phase 2

- **Synthetic ExifTool oracle** (`oracle_synthetic_matches_exiftool`): 2026-11-21, ExifTool 13.64. 8 synthetic files compared (JPEG and NEF, both byte orders, with and without the Nikon maker note). **Pass.** 0 mismatches.
- **Synthetic ExifTool oracle, re-run after review fixes**: 2026-11-21, ExifTool 13.64. 9 synthetic files compared, now including a mixed-order JPEG (big-endian Exif, little-endian MPF) and located preview and full-JPEG offsets compared with ExifTool's. **Pass.** 0 mismatches.
- **Synthetic ExifTool oracle, re-run after second review cycle**: 2026-11-21, ExifTool 13.64. 13 synthetic files compared, adding a full-HD-class MPF preview (type 0x010002), a NEF whose only JPEG is in IFD0 (ExifTool `OtherImage`), and JPEGs whose only preview is a maker-note PreviewIFD (both byte orders). **Pass.** 0 mismatches.
- **Samples ExifTool oracle** (`oracle_samples_match_exiftool`): pending (awaiting user-designated Z f samples folder).
- **ORF tracking issue** (Task 13): https://github.com/noghiri/photosort/issues/1

## Phase 3

- Pending: no manual checks recorded yet.

## Phase 4

- Pending: no manual checks recorded yet.

## Phase 5

- Pending: no manual checks recorded yet.

## Phase 6

- Pending: no manual checks recorded yet.

## Phase 7

- Pending: no manual checks recorded yet.

## Phase 8

- Pending: no manual checks recorded yet.
