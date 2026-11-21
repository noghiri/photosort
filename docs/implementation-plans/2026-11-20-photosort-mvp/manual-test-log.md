# Manual test log

Results of manual and opt-in checks, one section per phase. Later phases append their own results to their section.

## Phase 2

- **Synthetic ExifTool oracle** (`oracle_synthetic_matches_exiftool`): 2026-11-21, ExifTool 13.64. 8 synthetic files compared (JPEG and NEF, both byte orders, with and without the Nikon maker note). **Pass.** 0 mismatches.
- **Synthetic ExifTool oracle, re-run after review fixes**: 2026-11-21, ExifTool 13.64. 9 synthetic files compared, now including a mixed-order JPEG (big-endian Exif, little-endian MPF) and located preview and full-JPEG offsets compared with ExifTool's. **Pass.** 0 mismatches.
- **Synthetic ExifTool oracle, re-run after second review cycle**: 2026-11-21, ExifTool 13.64. 13 synthetic files compared, adding a full-HD-class MPF preview (type 0x010002), a NEF whose only JPEG is in IFD0 (ExifTool `OtherImage`), and JPEGs whose only preview is a maker-note PreviewIFD (both byte orders). **Pass.** 0 mismatches.
- **Samples ExifTool oracle** (`oracle_samples_match_exiftool`): pending (awaiting user-designated Z f samples folder).
- **ORF tracking issue** (Task 13): https://github.com/noghiri/photosort/issues/1

## Phase 3

Task 10, 2026-11-21, by the user. Synthetic card from `gen_card` (7,000 shots, 7,705 files including the 5 edge-case files), release build, Windows 11.

- **AC9.1** (placeholders within 3 s): **Pass** by eye (not stopwatch-timed). `list_source` over 7,000 files measured about 40 ms in the automated test.
- **AC1.3** (edge files): **Pass.** `.MOV` and `.xmp` absent; `TRUNC_0001.JPG` and `EMPTY_0001.NEF` show "⚠ unreadable".
- **AC9.2** (frame times while the parse pass runs, scrolling and ticking): **Pass.** 5,180 frame lines, maximum 30.4 ms (limit 100 ms), trailer `# dropped 0`.
- **AC2.5 / AC2.6** (keyboard): **Pass.** Arrows move the focus ring and the grid follows; Shift+arrows extend the range; Space ticks or unticks the whole range.
  - Observed: pressing Space while holding an arrow key stops the repeat-scroll. Accepted by the user.
  - Observed: Shift+click does not extend the selection. Not a requirement (AC2.6 specifies Shift+arrows); noted for possible later work.
- **AC2.7** (tile slider and Ctrl+scroll resize, column reflow, Ctrl+scroll does not zoom the UI) and the checkbox-then-Space double-toggle check: not reported in this run.
- **AC2.2 / AC2.3 / AC2.8** (badges and previews): **Pass.** JPEG-only tiles show `J` with a preview; `DSC_0010` shows `J+N`; `NOPV_0001.JPG` shows a name-only placeholder; after deleting `DSC_0010.JPG` and rescanning, the `N` tile shows its preview.
- **AC2.1** (capture order after the pass): **Pass.**

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
