# photosort MVP — Test requirements

Maps each acceptance criterion in `docs/design-plans/2026-11-20-photosort-mvp.md` (photosort-mvp.AC1.1 to AC12.5, 72 in all) to the automated tests and manual checks in the implementation phases `docs/implementation-plans/2026-11-20-photosort-mvp/phase_01.md` to `phase_08.md`. Phase 1 is infrastructure and verifies no AC.

## How to run

**Normal suite** (CI on `windows-latest`, and every phase gate):

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Oracle and ExifTool tests show as `ignored` here. Everything else must pass.

**Opt-in suite** (ExifTool oracle and lens-write integration tests). `-- --ignored` runs only the `#[ignore]`d tests:

```powershell
$env:PHOTOSORT_EXIFTOOL = 'C:\path\to\exiftool.exe'   # ExifTool 13.64
$env:PHOTOSORT_SAMPLES  = 'C:\path\to\zf-samples'     # real Z f JPEG/NEF, incl. silent-mode bursts
cargo test --workspace -- --ignored
```

Narrower runs: `cargo test -p photosort-core --test oracle -- --ignored --nocapture` (P2 T12) and `cargo test -p photosort lens_integration -- --ignored` (P8 T5).

**What fails loudly when the variables are unset.** Each test panics with a message naming the missing variable, or fails if the path doesn't exist. None of them returns early as a pass.

| Test | Location | Needs |
|---|---|---|
| `oracle_synthetic_matches_exiftool` | `crates/photosort-core/tests/oracle.rs` (P2 T12) | `PHOTOSORT_EXIFTOOL` |
| `oracle_samples_match_exiftool` | same file (P2 T12) | `PHOTOSORT_EXIFTOOL` + `PHOTOSORT_SAMPLES`; also fails if it finds zero files |
| ExifTool process tests (`-ver` = 13.64, sequential replies, restart after kill) | `crates/photosort/src/exiftool.rs` (P8 T3) | `PHOTOSORT_EXIFTOOL` |
| `lens_integration` module | `crates/photosort/src/copy.rs` (P8 T5) | `PHOTOSORT_EXIFTOOL` |
| `lens_samples_preserve_makernotes` | `crates/photosort/src/copy.rs` (P8 T5) | `PHOTOSORT_EXIFTOOL` + `PHOTOSORT_SAMPLES` |

Until the user designates a samples folder, the two `PHOTOSORT_SAMPLES` tests fail by design. P8 T7 says to report them as **pending, not passing**. Both must pass before MVP sign-off.

**Other runs:**
- P2 T11: run `crates/photosort-core/tests/robustness.rs` once locally with `PROPTEST_CASES=20000`. CI uses the default case counts.
- AC9.2 manual check: `PHOTOSORT_FRAME_LOG=<file>` enables the frame-time log (P3 T8).

**Conventions:**
- Shell tests live in `#[cfg(test)] mod tests` inside the module they test, because `photosort` is a binary crate (P3 conventions). They use real `tempfile::tempdir()` directories and testkit files, and never mock the filesystem.
- The only seams are `CopyEnv::free_space` and `CopyEnv::verify` (P5 conventions).
- Core cross-module tests live in `crates/photosort-core/tests/`.
- Synthetic fixtures come from `crates/photosort-testkit`, which deliberately doesn't depend on `photosort-core` (P2 T2).

**Manual results.** Every manual step goes under `## Phase N` in `docs/implementation-plans/2026-11-20-photosort-mvp/manual-test-log.md`. P2 T12 step 5 creates that file with sections Phase 2 to Phase 8.

**Legend:**
- `P3 T6` means Phase 3, Task 6, and `s3` means step 3 of a manual task.
- Test types:
  - **unit**: pure, in-module;
  - **integration**: real temp dirs, or a `tests/` crate;
  - **property**: proptest;
  - **opt-in ExifTool**: `#[ignore]`d, needs the variables above.

## AC1: Source selection and scan

| AC | Automated test(s) | Human verification |
|---|---|---|
| AC1.1 | **integration**, `crates/photosort/src/scan.rs` tests (P3 T6): `A.JPG`, `b.jpg`, `C.jpeg` and `D.NeF` in nested folders → four shots.<br>**unit**, `crates/photosort-core/src/metadata/registry.rs` tests (P2 T4): extension lookup ignores case and a leading `.`. | **`.orf` part: no test.** ORF is deferred (P2 decision): `.orf` isn't registered and no `orf.rs` exists. It's tracked by the ORF issue (P2 T13), whose URL is logged under `## Phase 2`.<br>P3 T10 s3: every generated shot appears. |
| AC1.2 | **unit**, `crates/photosort-core/src/grouping.rs` tests (P3 T1):<br>• `100NZ_F` JPG+NEF → one shot of 2 files;<br>• `101NZ_F/DSC_0001.JPG` → a separate shot;<br>• mixed-case stems still pair. | — (fully automated) |
| AC1.3 | **unit**, `grouping.rs` (P3 T1): `.MOV`, `.xmp` and `.dop` produce no shot.<br>**integration**, `scan.rs` (P3 T6): unregistered files are ignored. A truncated JPEG and a zero-byte NEF are listed with `meta: Err`, the other files parse, and `ParsePassDone` arrives. | Error-tile rendering: P3 T10 s3, using gen_card's edge files. `.MOV`/`.xmp` are absent, and the truncated JPEG and zero-byte NEF show "⚠ unreadable". |

## AC2: Thumbnail grid and keyboard culling

| AC | Automated test(s) | Human verification |
|---|---|---|
| AC2.1 | **unit**, `crates/photosort-core/src/grouping.rs` `capture_order` (P3 T1):<br>• 31.99 < 32.9 < 32.92;<br>• equal times fall back to the name;<br>• undated shots go last, by name;<br>• each shot appears exactly once. | P3 T10 s7: capture order after pass 2. Approved decision: placeholders are in path order, and the grid re-sorts on `ParsePassDone`. |
| AC2.2 | **unit**, `grouping.rs` `badge` (P3 T1): `"J"`, `"N"`, `"J+N"`. | P3 T10 s6: tiles show preview, badge and checkbox. **The `O` badge is untestable while ORF is deferred** (P2 T13 issue). |
| AC2.3 | **unit**, `crates/photosort/src/preview.rs` (P3 T5): a 640×424 JPEG decodes to `[640, 424]`, and to `[424, 640]` with orientation 6.<br>**unit**, `nikon.rs` (P2 T6) and `jpeg.rs` (P2 T7): the NEF PreviewIFD and JPEG MPF ranges decode to 640×424. | P3 T10 s6: JPEG-only tiles show a preview. Delete a JPEG partner and Rescan: the NEF-only `N` tile still shows its preview. |
| AC2.4 | None for the click handling. P3 T8 states that UI drawing is verified manually. Tick toggling uses `toggle_ticks` (see AC2.6). | **Checkbox:** P3 T10 s5 (checkbox click, then Space, doesn't double-toggle).<br>**Image click opens the overlay:** P4 T7 s1 (Phase 4 completes this half). |
| AC2.5 | **unit**, `crates/photosort/src/ui/grid_math.rs` (P3 T7):<br>• `move_focus` clamps at the edges;<br>• on an empty grid, `move_focus` returns 0 and `apply_move` returns the default;<br>• `reveal_row` gives `None` for a visible row, a smaller offset for a row above, and `(row+1)*h - viewport` for a row below. | P3 T10 s5: arrows move the ring, and the grid scrolls to keep it visible. |
| AC2.6 | **unit**, `grid_math.rs` (P3 T7):<br>• Shift+Right×3 from 5 → `5..=8`;<br>• a plain move clears the anchor;<br>• `toggle_ticks` on a mixed range ticks all, and on an all-ticked range unticks all. | P3 T10 s5. Approved decision: when every shot in the range is ticked, Space unticks them all. |
| AC2.7 | **unit + property**, `grid_math.rs` (P3 T7):<br>• the `layout` column formula reflows with width;<br>• `clamp_tile` and `zoom_tile` stay within 96..=640 (proptest);<br>• `zoom_tile(200, 1.1) > 200`. | P3 T10 s5: the slider and Ctrl+scroll resize from 96 to 640, columns reflow, and Ctrl+scroll doesn't zoom the whole UI. |
| AC2.8 | **unit**, `grouping.rs` `tile_preview` (P3 T1): a NEF with both `preview` and `full_jpeg` → the preview range (never `full_jpeg`); a shot with neither → `None`. | P3 T10 s6: `NOPV_0001.JPG` (readable, with no embedded preview; written by gen_card, P3 T2) shows a placeholder with its filename. |

## AC3: Preview overlay

| AC | Automated test(s) | Human verification |
|---|---|---|
| AC3.1 | **unit**, `crates/photosort-core/src/grouping.rs` `full_source` (P4 T1): JPEG → `WholeFile`; NEF-only without `full_jpeg` → `PreviewOnly` fallback.<br>**unit**, `crates/photosort/src/preview.rs` `decode_full` (P4 T2): `[300,200]` → 300×200, `[301,201]` → 600×400, `None` → full size.<br>**integration**, `crates/photosort/src/overlay_loader.rs` (P4 T4): a Fit `[300,200]` request → one 300×200 result; `set_wanted` replaces the pending jobs. | P4 T7 s1: Enter or a click shows the tile preview at once, then it sharpens. |
| AC3.2 | **unit**, `crates/photosort/src/ui/overlay_math.rs` (P4 T3):<br>• `zoom_at_cursor` + `zoomed_paint` keep the cursor pixel in place (≤ 0.5 px);<br>• `pan` by (+10, 0) moves `center_px` by `10*ppp`;<br>• the result is clamped at the edges;<br>• UVs stay in 0..=1. | P4 T7 s2: `Z` keeps the detail under the pointer, dragging pans, and `Z` or a click returns to fit. |
| AC3.3 | **unit**, `overlay_math.rs` `neighbor`/`prefetch_set` (P4 T3): hidden shots are skipped in both directions, with `None` at the ends.<br>**unit**, `grid_math.rs` `refocus` (P5 T8): keeps the shot, else the nearest following position, else the last; 0 when empty. | P4 T7 s3: ←/→, Space, and Esc refocus with the shot scrolled into view.<br>P5 T9 s5: the real hide-copied `is_hidden` (Phase 4 used a stub returning `false`). |
| AC3.4 | **unit**, `grouping.rs` `full_source` (P4 T1): NEF-only with `full_jpeg` → `Embedded`.<br>**integration**, `preview.rs` `load_full` (P4 T2): a NEF temp file with `Embedded` → the JpgFromRaw size (1200×800 fixture).<br>**opt-in ExifTool**, `oracle_samples_match_exiftool` (P2 T12): the real Z f `full_jpeg` decodes at 6048×4032. | P4 T7 s4: `RAW_0001.NEF` at 100% → 3000×2000 (synthetic). **Pending:** 6048×4032 on a real Z f RAW-only NEF, once the user provides samples. |
| AC3.5 | **unit**, `crates/photosort/src/ui/format.rs` (P4 T3):<br>• `1/250` and `10/2500` → `1/250 s`; `2/1` → `2 s`; `13/10` → `1.3 s`;<br>• `f/2.8` and `f/8`;<br>• `info_line` holds both file names, `ISO 800` and the lens. | P4 T7 s5: the info strip shows names, shutter speed, aperture, ISO and lens. |
| AC3.6 | **unit**, `preview.rs` (P4 T2): corrupt bytes, a range past EOF, or SOS overwritten with EOI → `Err`, no panic.<br>**integration**, `overlay_loader.rs` (P4 T4): a corrupt file → `Err`, and the loader keeps serving later requests. | P4 T7 s6 on `BRKN_0001.JPG`: the preview stays, an error note shows, and the app keeps working. |

## AC4: Metadata parsing and identity

| AC | Automated test(s) | Human verification |
|---|---|---|
| AC4.1 | **unit:**<br>• `crates/photosort-core/src/metadata/exif.rs` (P2 T5): every field equals the spec, in both byte orders;<br>• `metadata/makernotes/nikon.rs` (P2 T6): `shot_counter == 75036`, not the mechanical 10438;<br>• `metadata/formats/jpeg.rs` (P2 T7) and `metadata/formats/nef.rs` (P2 T8): all fields plus the counter, both orders.<br>**opt-in ExifTool**, `crates/photosort-core/tests/oracle.rs` (P2 T12):<br>• `oracle_synthetic_matches_exiftool`: each field equals `exiftool -json -G1 -a -n`;<br>• `oracle_samples_match_exiftool`: the same on real Z f files. | **Pending (P2 T12 s4):** the samples oracle on a user-designated Z f folder, including silent-mode burst frames. Log it under `## Phase 2`. The synthetic oracle result is logged in P2 T12 s3/s5. |
| AC4.2 | **unit:**<br>• `nikon.rs` (P2 T6): the PreviewIFD range decodes 640×424;<br>• `jpeg.rs` (P2 T7): the MPF preview decodes 640×424 and its bytes match;<br>• `nef.rs` (P2 T8): preview 640×424; `full_jpeg` decodes to the fixture size; ranges past a 128 KiB head are located without `NeedMore`; the `preview_inside` layout also works.<br>**opt-in ExifTool**, oracle (P2 T12): synthetic sizes; on the Z f, preview 640×424 and `full_jpeg` 6048×4032. | **Pending:** the same samples-oracle run (P2 T12 s4). |
| AC4.3 | **unit**, `crates/photosort-core/src/identity.rs` (P2 T9):<br>• NEF with a Nikon note → `Counter`;<br>• `shutter_count: None` → `Capture`;<br>• JPEG without EXIF → `Content`;<br>• `identity_for(None, ..)` → `Content`. | — |
| AC4.4 | **unit**, `identity.rs` (P2 T9): for each of the 7 lens/aperture fields, a file pair differing only in that field gives equal keys on the counter and capture rungs. Renaming is covered by construction: there is no filename input, as the test doc comment states.<br>**unit**, `crates/photosort-core/src/lens.rs` (P7 T2): content-rung shots resolve to no lens edit. Their copies are never edited, so their content key can't change (planning decision). | — |
| AC4.5 | **unit**, `identity.rs` (P2 T9): `jpeg_file` and `nef_file` from the same specs give equal keys on the counter rung, and on the capture rung even with NEF exposure `10/2500`.<br>**opt-in ExifTool**, `oracle_samples_match_exiftool` (P2 T12, comparison step 5): same-stem real J+N pairs have equal keys. | **Pending:** the real-data part depends on the samples run (P2 T12 s4). |
| AC4.6 | **unit**, `crates/photosort-core/src/metadata/tiff.rs` `#[cfg(test)]` (P2 T3):<br>• an offset past `file_len` → `Malformed`;<br>• an offset past the head → `NeedMore` with `have < needed <= file_len`;<br>• more than 1024 entries, an unknown type, or `count = u32::MAX` → `Malformed`.<br>Also `jpeg.rs` (P2 T7): a 200-byte head → `NeedMore`; not a JPEG → `WrongFormat`.<br>**property**, `crates/photosort-core/tests/robustness.rs` (P2 T11):<br>• random bytes per reader (1024 cases) never panic;<br>• mutated or truncated valid files never panic, and every `ByteRange` ends ≤ `file_len`;<br>• `NeedMore` converges to the full parse within 64 steps;<br>• `identity_for` never panics. | Executor step (not logged): one local run with `PROPTEST_CASES=20000` (P2 T11). "Fuzz" means proptest only, by user decision; cargo-fuzz isn't used. |

## AC5: Output selection and copy

| AC | Automated test(s) | Human verification |
|---|---|---|
| AC5.1 | **unit**, `crates/photosort/src/ui/output_select.rs` (P5 T3):<br>• base + subfolder joins; empty → the base; `a/b` nests;<br>• `..\x`, `C:\x` and UNC paths → `EscapesBase`;<br>• `Arbitrary(p)` → `p`.<br>**unit**, `crates/photosort-core/src/state.rs` (P6 T1): `base_folders`/`last_output` round-trip.<br>**integration**, `crates/photosort/src/persist.rs` (P6 T3): save then load returns an equal value. | **In-memory part:** P5 T9 s1. A new subfolder is created at copy time, an existing one can be picked, Browse works, `..\x` disables Copy, and the keyboard gate holds.<br>**"Remembered" part:** P6 T6 s5, after a restart. |
| AC5.2 | **integration**, `crates/photosort/src/copy.rs` tests (P5 T6): two J+N shots; each destination's BLAKE3 equals its source's, and its mtime equals the source mtime. | — |
| AC5.3 | **integration**, `copy.rs` (P5 T6): a snapshot (relative path, size, mtime) taken before `list_source` + `parse_file` + `plan_copy` + `execute` equals the one taken after.<br>**opt-in ExifTool**, `lens_integration` (P8 T5): the source snapshot including BLAKE3 is unchanged with edits on. | — |
| AC5.4 | **unit**, `crates/photosort-core/src/plan.rs` (P5 T2): 2 not-copied J+N (one renamed), 1 partial and 1 skipped → `file_count() == 5`, `rename_count() == 1`, and `total_bytes` is the sum of those files. | P5 T9 s2: the dialog shows shots, files, bytes, destination and renames. Cancel leaves the output unchanged. |
| AC5.5 | **integration**, `copy.rs` (P5 T6): the `free_space` seam returns 10 → `preflight` gives `InsufficientSpace`, and `execute` writes no file. | — (needs the seam; a full volume can't be created in a test) |
| AC5.6 | **integration**, `copy.rs` (P5 T6): a destination whose parent is a regular file → `CannotCreate`; an existing folder with an `icacls` Everyone-write deny → `NotWritable`, and no file is created. | P5 T9 s8: Browse to `C:\Windows` → the pre-flight error dialog appears, and nothing is written. |
| AC5.7 | **integration**, `copy.rs` (P5 T6): a `verify` seam returning `false` for `0002` → that shot is `Failed`, the others are `Copied`, and no `.photosort-tmp` remains. | — |
| AC5.8 | **integration**, `copy.rs` (P5 T6): the observer deletes the source root at shot 2 → shot 1 `Copied`, shot 2 `Failed { "source disappeared" }`, the rest `NotAttempted`, and `aborted == SourceGone`.<br>**unit**, `crates/photosort/src/copy_summary.rs` `ticks_to_clear` (P5 T6): only `Copied` and `SkippedAlreadyCopied` are cleared. | — Approved decision: skipped-as-copied shots also lose their tick. |
| AC5.9 | **integration**, `copy.rs` (P5 T6): no `.photosort-tmp` file remains after every copy test. The opt-in `lens_integration` (P8 T5) repeats this with edits.<br>**unit**, `crates/photosort/src/logfmt.rs` (P5 T7): `render_batch` emits `COPY`, `RENAME`, `SKIP` and `ERROR`; `utc_parts` epoch, leap-day and 2025 cases.<br>**integration**, `crates/photosort/src/batch_log.rs` (P5 T7): `append` creates the folder and appends two blocks in order. | P5 T9 s7: `%APPDATA%\photosort\logs\<UTC date>.log` has COPY, SKIP and RENAME lines. Approved decision: UTC. |

## AC6: Name collisions

| AC | Automated test(s) | Human verification |
|---|---|---|
| AC6.1 | **integration**, `crates/photosort/src/fsops.rs` (P5 T5): `move_no_replace` onto an existing `b.jpg` (target `B.JPG`) → `is_exists_error`, and `b.jpg` keeps its content.<br>**integration**, `crates/photosort/src/copy.rs` (P5 T6): `DSC_0001.JPG` created after planning stays untouched, and the shot lands as `_2` with `renamed`.<br>**property**, `crates/photosort-core/src/plan.rs` (P5 T2): no destination equals an existing name, case-insensitively. | — |
| AC6.2 | **unit**, `plan.rs` (P5 T2): an existing `DSC_0001.JPG` with another identity → `DSC_0001_2.JPG` + `DSC_0001_2.NEF`, `renamed`.<br>**integration**, `copy.rs` (P5 T6): a pre-existing different `DSC_0001.NEF` → both files copied with `_2`.<br>**property**, `plan.rs`: every `dest_name` in a `PlannedShot` shares one stem. | — |
| AC6.3 | **unit**, `plan.rs` (P5 T2): `100NZ_F/DSC_0001` and `101NZ_F/DSC_0001` → `DSC_0001.*` and `DSC_0001_2.*`.<br>**property**, `plan.rs`: all destinations in a plan are unique, case-insensitively. | — |
| AC6.4 | **unit**, `plan.rs` (P5 T2): an existing `dsc_0001.jpg` clashes with `DSC_0001.JPG`.<br>**integration**, `fsops.rs` (P5 T5): a case-only clash is refused.<br>**property**, `plan.rs`: the invariants are checked case-insensitively. | — |
| AC6.5 | **unit**, `plan.rs` (P5 T2): with `DSC_0001_3.JPG` (same identity) in the output, the NEF → `DSC_0001_3.NEF`, not unpaired. With `DSC_0001_3.NEF` also taken → a fresh suffix and `unpaired = true`. | P5 T9 s4: delete one NEF from the output; Copy brings only the NEF, with the partner's stem. |
| AC6.6 | **integration**, `copy.rs` (P5 T6): `Copied` results carry every `(src, dest_name)` pair.<br>**unit**, `logfmt.rs` (P5 T7): a `RENAME` line. | P5 T9 s9: a collision with a different `DSC_0001.JPG` → the summary's **Renamed** list shows both `→ DSC_0001_2.*` lines. |

## AC7: Already-copied detection

| AC | Automated test(s) | Human verification |
|---|---|---|
| AC7.1 | **unit**, `crates/photosort-core/src/output.rs` (P5 T1): J+N indexed under unrelated names → `Copied`; the same identity with a different kind isn't a match.<br>**unit**, `crates/photosort-core/src/plan.rs` (P5 T2): a fully present shot → `skipped`, not in `shots`.<br>**integration**, `crates/photosort/src/output_index.rs` (P5 T4):<br>• copies named `renamed.jpg` and `x.NEF` → `Copied`;<br>• a hand-copied JPEG is detected;<br>• a leftover `.photosort-tmp` doesn't count. | P5 T9:<br>• s3: tiles show `copied`; re-tick and copy → the summary reports the skip;<br>• s6: hand-copied and renamed files in another folder → `copied`. |
| AC7.2 | **unit**, `output.rs` (P5 T1): only the JPEG indexed → `Partial`, `missing_files` = the NEF, and `partner_stem == "Holiday"`.<br>**unit**, `plan.rs` (P5 T2): a partial shot → only the missing kind is planned.<br>**integration**, `output_index.rs` (P5 T4): only the JPEG present → `Partial`. | P5 T9 s4: delete a NEF; the tile shows `partial` after Rescan; Copy brings only the NEF. |
| AC7.3 | Partial only:<br>• **unit**, `output.rs` `copy_status` (P5 T1): `Copied` and `Partial` are distinct, which `is_hidden` relies on;<br>• **unit**, `grid_math.rs` `refocus` (P5 T8).<br>The filter and the re-index triggers live in `app.rs` and have no automated test (P5 T8). | **Human for the filter and the triggers:**<br>• P5 T9 s5: Hide copied hides copied shots and keeps partial ones; Esc refocus works;<br>• re-index after a copy (s3), on Rescan (s4), and on an output-folder change (s6). |

## AC8: Resume

| AC | Automated test(s) | Human verification |
|---|---|---|
| AC8.1 | **unit**, `crates/photosort-core/src/resume.rs` (P6 T2):<br>• `due` is false at 500 ms and true at 1000 ms;<br>• continuous marks still save by `MAX_WAIT` at 2000 ms;<br>• `due` is false after `saved()`.<br>**integration**, `crates/photosort/src/persist.rs`:<br>• (P6 T3) 50 `Card` commands then `Flush` → the file holds the last value;<br>• (P6 T5) `Card` then `LoadCard` → the reply returns that value with the generation echoed. | P6 T6 s1: ticks are in the card JSON within about 1 s, and close/reopen restores them. Approved decision: 1 s quiet, capped at 2 s. |
| AC8.2 | **unit**, `crates/photosort-core/src/state.rs` (P6 T1): ticks serialise as identity strings; a bad string fails; proptest `CardState` round-trip.<br>**unit**, `resume.rs` (P6 T2): a rescan with new ShotIds re-ticks the same files; a J+N shot with content-rung keys is restored by either key.<br>**unit**, `crates/photosort/src/volume.rs` (P6 T4): the card file name has no drive letter and treats `rel_path` case-insensitively; `card_id(tempdir)` → `Volume`. | P6 T6 s2: Rescan keeps the ticks. **Real SD card or USB stick:** change the drive letter and the ticks are restored. |
| AC8.3 | **unit**, `volume.rs` (P6 T4): the same `rel_path` with a different serial → a different file name. | P6 T6 s3: the card folder copied to another volume starts with no ticks. |
| AC8.4 | **unit**, `resume.rs` (P6 T2): `identities_to_save` keeps unmatched identities before `ParsePassDone` and drops them after. | P6 T6 s4: delete 3 ticked files; after pass 2 the card JSON no longer has them. Approved decision: prune only after `ParsePassDone`. |
| AC8.5 | **unit**, `state.rs` (P6 T1): `Config` with every field set round-trips, and missing fields fall back to defaults.<br>**integration**, `persist.rs` (P6 T3): `save_json_atomic` then `load_json` returns an equal value with no warning. | P6 T6 s5: base folder, output, tile size 320 and last source are restored. |
| AC8.6 | **integration**, `persist.rs` (P6 T3):<br>• `{not json` → defaults, a warning naming the renamed file, and `.corrupt-*` holding the original bytes;<br>• `{"version": 99}` → `.newer-*` and a warning;<br>• a leftover `.config.json.tmp-123` → the previous value still loads, and the next save leaves no temp file. | **"Tells the user":** P6 T6 s6. A `{` config on start shows a banner naming `config.json.corrupt-…`. |

## AC9: Responsiveness at 7,000 shots

| AC | Automated test(s) | Human verification |
|---|---|---|
| AC9.1 | **integration**, `crates/photosort/src/scan.rs` (P3 T6): `list_source` on 7,000 testkit JPEGs takes under 3 s. This times the listing only. | **Placeholders on screen within 3 s:** P3 T10 s3, on a 7,000-shot gen_card source. |
| AC9.2 | Scheduling only: **unit + property**, `crates/photosort/src/scan_queue.rs` (P3 T4):<br>• visible shots first, and a preview before its parse;<br>• `clear()` empties the queue;<br>• every job pops exactly once (proptest). | **The 100 ms criterion is human only:** P3 T10 s4. Scroll and tick during pass 2 with `PHOTOSORT_FRAME_LOG` set; the logged maximum must be under 100 ms. |
| AC9.3 | **unit**, `crates/photosort/src/budget.rs` (P3 T3): 24 GiB → 6 GiB; an override of 512 → 512 MiB; the split stays within budget, and textures are capped at 2 GiB.<br>**unit**, `crates/photosort/src/cache.rs` (P3 T3): 100 × 64 KiB entries in a 1 MiB cache → weight ≤ 1 MiB and early keys evicted; `physical_ram() > 0`.<br>**unit**, `grid_math.rs` `previews_to_request` (P3 T7): an evicted visible shot is re-requested; in-flight and preview-less shots are not. | P6 T6 s7: with `ram_budget_mb: 256`, scroll the 7,000-shot card. The working set stays bounded and tiles reload.<br>Split: P3 computes the budget, P6 T5 wires the config override, and the P7 T5 General tab edits it. |

## AC10: Lens profiles and per-shot aperture (stretch)

All unit tests here are in `crates/photosort-core/src/lens.rs` unless stated otherwise.

| AC | Automated test(s) | Human verification |
|---|---|---|
| AC10.1 | **unit**, `lens.rs` + `crates/photosort-core/src/state.rs` (P7 T1): `Config::default().profiles` is empty; Chip + FDn 28 Manual + 2 rules round-trip; JSON missing the new fields loads with defaults.<br>**unit**, `validate_profile` (P7 T3): accepts `fdn28()` and a Chip profile; rejects an empty name, a 9-char badge, `max_f > min_f` and `focal_mm = 0`. | **Create, edit, delete, and "only in config.json":** P7 T7 s1. A fresh `%APPDATA%\photosort` shows an empty tab; changes persist across a restart and appear in `config.json`. |
| AC10.2 | **unit** (P7 T1): `focal_mm` 28, `max_f` 2.8, `min_f` 22, `Half` and `default_f` 8 survive the round-trip. | P7 T7 s1: the FDn 28 Manual profile is created through the editor. |
| AC10.3 | **unit** (P7 T2): an exact `ReportedModel` matches; a trailing space doesn't; a scoped Z f rule matches a Z f shot.<br>**unit**, `crates/photosort-core/src/plan.rs` (P7 T4): every planned file of the resolved shot carries the edit, and other shots carry `None`. | P7 T7 s2: quick-add from the focused shot → matching tiles show the badge, and the usage count is shown. |
| AC10.4 | **unit** (P7 T2): no-lens shots (empty + 0 mm, or both absent) get the body's session profile; with no session entry → `None`; a lens-reporting shot is unaffected. | P7 T7 s3: each body's no-lens tiles show their own badge at `f/8?`. |
| AC10.5 | **unit** (P7 T2): an unmatched reported lens → `None`; a rule pointing at a deleted profile → `None`. | — |
| AC10.6 | **unit** (P7 T2): an override `None` beats a matching rule; `Profile(p2)` beats rule `p1`; an override stored under only the NEF identity applies to the J+N shot.<br>**unit** (P7 T1): `CardState` overrides round-trip with identity-string keys and `"none"`/uuid values; an invalid value is an error. | P7 T7 s5: `L` → None removes the badge, survives a restart, and lowers the usage count. |
| AC10.7 | **unit + property** (P7 T2):<br>• `stops_for(fdn28)` lists 2.8 … 22;<br>• stepping 8 → 9.5 or 6.7; clamps at 22 and 2.8; 7.9 snaps → 9.5;<br>• the default is 8; it clamps to 5.6 for a 1.2–5.6 range and to 11 for 11–22;<br>• proptest: every result stays in range. | P7 T7 s4: `[`/`]` on a tile steps and clamps. The overlay slider moves through the half stops f/2.8 … f/22. |
| AC10.8 | **unit**, `crates/photosort/src/ui/format.rs` `lens_badge` (P7 T6): a badge → `FD28 f/8`; no badge → the truncated name; unconfirmed → `f/8?` + `true`; Chip → the badge only. | P7 T7: s3 shows amber `f/8?`; s4 shows `]` → white `FD28 f/9.5` (confirmed). Approved decision: stepping creates a confirmed override. |
| AC10.9 | **unit**, `unconfirmed_manual` (P7 T3): only the unconfirmed ticked Manual shots, in order; Chip and no-edit shots are excluded. | P7 T7 s7: Copy warns and offers Copy anyway, Cancel and Review apertures. |
| AC10.10 | **unit**, `ReviewQueue` (P7 T3): a queue of 3 → `Next(b)`, `Next(c)`, `Finished`; an empty queue → `None`; `prev` at 0 stays. | P7 T7 s7: Review steps through exactly 3 shots, Enter confirms each, and the flow returns to the confirmation without the warning. |
| AC10.11 | **unit**, `similar_shots` (P7 T3): with 2 bodies × 2 lenses, it returns exactly the focused group in order; no-lens shots group per body. | P7 T7 s6: `L` + Apply to all similar shots covers only that body's no-lens shots. |

## AC11: Lens EXIF writing (stretch)

| AC | Automated test(s) | Human verification |
|---|---|---|
| AC11.1 | **unit**, `crates/photosort-core/src/lens_write.rs` (P8 T2): a Chip edit → exactly the common tags plus the file, with no `FocalLength`, `FNumber`, `SubjectDistance` or `MakerNotes`.<br>**opt-in ExifTool**, `crates/photosort/src/copy.rs` `lens_integration` (P8 T5): `LensMake`, `LensModel` and `LensInfo` (`80 200 2.8 2.8`) are set; `FocalLength`, `FNumber` and `SubjectDistance` equal the source's. | P8 T7 s2: a real Fringer shot with a Chip rule gives no "lens not applied" flag. |
| AC11.2 | **unit**, `lens_write.rs` (P8 T2): FDn 28 at f/8 → `FocalLength=28`, `FocalLengthIn35mmFormat=28`, `FNumber=8`, `MaxApertureValue=2.8`, `SubjectDistance=inf`.<br>**opt-in ExifTool**, `lens_integration` (P8 T5): reads back 28 / 28 / 8, `MaxApertureValue` within 0.05 of 2.8, and `SubjectDistance == "inf"`. | P8 T7 s2: a real dumb-adapter shot at a confirmed f/5.6. DX-crop `FocalLengthIn35mmFormat` is accepted as wrong for the MVP. |
| AC11.3 | **opt-in ExifTool**, `lens_integration` (P8 T5): the BLAKE3 of `-b -MakerNotes` is equal for the source and the copy, for JPEG and NEF, in both `preview_inside` layouts.<br>**opt-in ExifTool + samples**, `lens_samples_preserve_makernotes` (P8 T5): up to 4 real J+N pairs.<br>**unit**, `lens_write.rs` (P8 T2): no `MakerNotes` tag in the arguments. | **Pending:** the real-file run needs `PHOTOSORT_SAMPLES` and fails by design until then (P8 T7). |
| AC11.4 | **opt-in ExifTool**, `lens_integration` (P8 T5): the JPEG and NEF copies both carry the new `LensModel`; the source snapshot (path, size, mtime, BLAKE3) is unchanged; `File:FileType` is `NEF`/`JPEG`.<br>**integration**, `copy.rs` (P8 T4, the AC12.5 case): the JPEG of the same shot still attempts its edit. | P8 T7 s2. The "only paths under `ctx.dest`" guard is a code assertion (P8 T4), not a test. |
| AC11.5 | **unit**, `crates/photosort/src/exiftool.rs` (P8 T3): `start` on a nonexistent path → `NotFound`.<br>**integration**, `copy.rs` (P8 T4): a bogus `exiftool_path` → the shot is `Copied`, both files are byte-identical to the source, and the notes are `NotApplied("ExifTool not found …")`.<br>**unit**, `lens_write.rs` `write_succeeded` (P8 T2): rejects `0 image files updated` and `Error:` on stderr.<br>**opt-in ExifTool**, `lens_integration` (P8 T5): a truncated JPEG planned with an edit → ExifTool errors, the copy is byte-identical, and the note is `NotApplied` with the error text. | P8 T7 s4: a bogus path → the summary flags "lens not applied" and the copies are unedited. |
| AC11.6 | **unit**, `lens_write.rs` `parse_originals` (P8 T2): returns both values as strings.<br>**unit**, `crates/photosort/src/logfmt.rs` (P8 T4): `LENS`, `NOLENS` and `UNSUPP` lines.<br>**opt-in ExifTool**, `lens_integration` (P8 T5): `Applied.originals` has `LensModel == "VR 80-200mm f/2.8G"`, and the rendered log contains it. | P8 T7 s2: the batch log has `LENS` lines with the originals. |

## AC12: Body-agnostic extensibility

| AC | Automated test(s) | Human verification |
|---|---|---|
| AC12.1 | Split across three phases. Each test registers a test-only reader without changing library code.<br>**Phase 2 (registration, parse, identity):**<br>• **unit**, `crates/photosort-core/src/metadata/registry.rs` (T4): a stub `fkr` reader registers, lookup works in any case, and `parse` dispatches to it;<br>• **integration**, `crates/photosort-core/tests/extensibility.rs` (T10): `FakeTiffReader` added to `with_builtin()` gives a `Counter` identity equal to a NEF built from the same specs.<br>**Phase 3 (scan, group):** **integration**, `crates/photosort/src/scan.rs` (T6): `IMG_0001.fkr` + `IMG_0001.JPG` → one shot of 2 files with a `Counter` identity.<br>**Phase 5 (plan, copy):**<br>• **unit**, `crates/photosort-core/src/plan.rs` (T2): a `FileKind("fake")` file plans like any other;<br>• **integration**, `crates/photosort/src/copy.rs` (T6): a `.fkr` + `.JPG` shot, using `test_support::FakeTiffReader`, is copied byte-identical. | — |
| AC12.2 | **unit**, `crates/photosort-core/src/metadata/exif.rs` (P2 T5): Make `ACME` with an unregistered maker note → `Ok`, `shot_counter == None`.<br>**unit**, `crates/photosort-core/src/identity.rs` (P2 T9): `ACME` → `Capture`; `ACME` without a DTO → `Content`; neither errors.<br>**integration**, `crates/photosort-core/tests/extensibility.rs` (P2 T10): the fake file with Make `ACME` → `Capture`. | — |
| AC12.3 | **unit**, `crates/photosort-core/src/lens.rs` (P7 T2): a rule scoped to `NIKON CORPORATION`/`NIKON Z f` doesn't match a `NIKON Z 6_2` shot with the same `LensModel`; a scope with `model: None` matches both. | P7 T7 s8, with gen_card `--two-bodies`: the `Z 6_2` shots show no badge. |
| AC12.4 | **unit**, `lens.rs` (P7 T2): two serials, each with its own session profile → each body's no-lens shots get their own. | P7 T7 s3. |
| AC12.5 | **integration**, `crates/photosort/src/copy.rs` (P8 T4): a `.fkr` file (`lens_write_supported = false`) with an edit → `Unsupported` and a byte-identical copy, while the JPEG of the same shot still attempts its edit.<br>**unit**, `logfmt.rs` (P8 T4): an `UNSUPP … lens edit unsupported for format` line. | None possible. No user-reachable MVP format reports `false` (JPEG and NEF are allowlisted, and ORF is unregistered), so only the test reader exercises it. P8 T7 lists AC12.5 but has no step for it. |

## Gaps

None remaining. Drafting found six places where the phases' coverage was weaker than the AC text. Each was resolved in the phase files before this document was written:

1. **AC4.4, content rung.** Decided during planning: `resolve` gives no lens edit to shots with a content-rung (or missing) identity, so their copies are never edited and their identity can't change. The `L` picker explains why (P7 T2, P7 T6).
2. **AC5.6.** P5 T6 adds an `icacls` write-deny test for `NotWritable`, and P5 T9 s8 adds a manual check of the pre-flight error dialog.
3. **AC6.6.** P5 T9 s9 checks the summary's **Renamed** list.
4. **AC10.7.** P7 T7 s4 exercises the overlay aperture slider.
5. **AC11.5.** P8 T5 adds an opt-in case where ExifTool rejects the write.
6. **AC2.8.** gen_card writes `NOPV_0001.JPG` (P3 T2), and P3 T10 s6 checks its placeholder.

## Pending before MVP sign-off

- **Samples oracle (P2 T12 s4):** `oracle_samples_match_exiftool` on a user-designated Z f folder that includes silent-mode burst frames. It covers AC4.1, AC4.2 (640×424 preview, 6048×4032 JpgFromRaw) and AC4.5 on real data. Log it under `## Phase 2`.
- **Real-file maker-note check (P8 T5):** `lens_samples_preserve_makernotes` (AC11.3). P8 T7 says to report it as pending, not passing, until samples exist.
- **AC3.4 at real size (P4 T7 s4):** 100% zoom on a real Z f RAW-only NEF shows 6048×4032.
- **AC8.2 drive-letter check (P6 T6 s2):** needs a real SD card or USB stick.
- **DxO pre-check (P8 T1, a human gate before Phase 8):** hand-edit a scratch Fringer shot and open it in PhotoLab. If PhotoLab still proposes the wrong module, stop and escalate.
- **DxO check on real copies (P8 T7 s1–s3):** run from the packaged `dist\` app and record which lens module PhotoLab proposes for each copy. This is the design's lens success criterion: DxO no longer proposes the wrong module.
- **ORF tracking issue (P2 T13):** create it after the user confirms (the repo is public), and log its URL under `## Phase 2`. The `.orf` part of AC1.1 and the `O` badge in AC2.2 stay open until ORF ships.
- **Manual-test log:** every manual step for Phases 3 to 8 is recorded in its section of `docs/implementation-plans/2026-11-20-photosort-mvp/manual-test-log.md`.

## Coverage check

72/72 acceptance criteria are mapped to an automated test, a documented human verification, or both. Per group:
- AC1: 3
- AC2: 8
- AC3: 6
- AC4: 6
- AC5: 9
- AC6: 6
- AC7: 3
- AC8: 6
- AC9: 3
- AC10: 11
- AC11: 6
- AC12: 5

The six coverage gaps found while drafting are resolved in the phase files (see Gaps).
