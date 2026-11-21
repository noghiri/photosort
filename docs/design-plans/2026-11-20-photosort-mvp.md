# photosort MVP Design

## Summary

photosort is a Windows desktop app, written in Rust with egui, for culling photos off a camera's SD card and copying the keepers somewhere else. It's split into two crates. `photosort-core` is pure logic: it parses file headers from byte slices, groups files into shots, derives each shot's identity, plans copies and resolves lens rules, with no filesystem, threads or UI. `photosort` is the shell around it: it scans the card, decodes previews, runs the copy, saves state and draws the UI. All vendor-specific knowledge (Nikon, OM System, Sony) lives in two registries, format readers and maker-note readers, so supporting a new camera body means adding a reader rather than changing the core.

Everything the app reads, it reads in-process. The scan runs in two passes. The first lists the tree and shows placeholder tiles right away. The second runs on a worker pool, reads only each file's header (plus the tail range for JPEG previews) and pulls out metadata and the small embedded preview. The grid never decodes a full image, and caches are capped by a RAM budget. Already-copied detection and resume both depend on a three-rung identity key built from EXIF fields that lens edits never change. That's why a renamed or lens-edited copy still matches its source, and why saved ticks survive a rescan. Copies go to a temp file, get hash-verified, then take a no-overwrite rename, so the card is never touched and nothing in the output folder gets replaced. The stretch goal, rewriting lens EXIF on the copies, goes through a bundled ExifTool, because no pure-Rust crate is trusted to write NEF safely. The work is split into eight phases, with the last two covering the stretch goal.

## Definition of Done

A Windows-only Rust/egui desktop app (repo: noghiri/photosort) that lets the user:

1. Pick a source folder (the SD card); subfolders are scanned recursively.
2. Pick an output folder: a remembered base folder plus a chosen subfolder, or any arbitrary folder.
3. Scroll a thumbnail grid of every shot on the card. A RAW+JPEG pair is one tile; RAW-only (NEF/ORF) and JPEG-only shots are also supported.
4. Tick keepers with a checkbox, or click a tile for a larger preview.
5. Press one button to copy every file of each checked shot (JPEG + NEF/ORF) to the output folder.

The source card is never modified. The UI stays responsive with roughly 7,000 shots (a 64 GB card of burst JPEGs).

**Resume**
- Checkbox selections auto-save per card and are restored when the same card is reopened.
- Shots already present in the current output folder are badged "copied" and never copied twice, whether photosort or the user copied them, and even if they were renamed or lens-edited. A toggle hides copied shots.
- Shot identity key, built only from fields lens edits never touch, in order of preference:
  - (a) camera serial + Nikon ShutterCount, where present;
  - (b) otherwise serial + DateTimeOriginal + SubSecTimeOriginal + ISO + ExposureTime;
  - (c) otherwise file size + hash of the first 64 KB.

**Collisions**
- Existing files are never overwritten.
- If a different photo already has the same name, both files of the pair are copied with a matching suffix (`DSC_0001_2.NEF` + `DSC_0001_2.JPG`), and the renames are listed in the copy summary.
- Duplicate filenames across DCIM subfolders on one card are handled.

**Stretch goal: lens EXIF profiles**
- Saved lens profiles:
  - Chip (electronic/chipped adapter, e.g. Fringer) profiles fix lens identity only.
  - Manual (dumb adapter) profiles also write focal length and max aperture, plus a default shooting aperture and focus distance (e.g. Canon FD 28mm f/1.2 at f/8, focus at infinity).
- Rules map what the camera reported to a profile (e.g. `VR 80-200mm f/2.8G` → Canon EF 80-200mm f/2.8L).
- Because several manual lenses all report "no lens / 0 mm", the user selects the active manual profile for the session, and it applies to all no-lens shots.
- Per-shot override in the grid.
- Profiles are applied during copy, to the copies only.
- Success means DxO PhotoLab/FilmPack no longer proposes the wrong lens module.
- Which EXIF/maker-note fields DxO reads, and ExifTool vs pure Rust for writing, are decided by research.

**Out of scope:** moving or deleting files on the card, editing/ratings, video files, macOS/Linux.

**Long-term direction (shapes this design, not MVP scope):** photosort becomes a body-agnostic tool. Bodies beyond the Nikon Z f will be added over time, for example the OM System OM-1 Mark II and possibly Sony. Profiles will be applied smartly in bulk. The MVP builds the extension points, a format registry, per-vendor maker-note readers, rules that can be scoped by body, and per-body session profiles, so adding a body means adding a reader rather than changing the core.

## Acceptance Criteria

### photosort-mvp.AC1: Source selection and scan
- **photosort-mvp.AC1.1 Success:** Picking a source folder finds every file with a registered image extension (MVP: `.jpg`, `.jpeg`, `.nef`, `.orf`; any letter case) in that folder and all its subfolders.
- **photosort-mvp.AC1.2 Success:** `DSC_0001.JPG` and `DSC_0001.NEF` in the same directory form one shot. The same stem in `100NZ_F` and in `101NZ_F` forms two separate shots.
- **photosort-mvp.AC1.3 Edge:** Files with unregistered extensions (MOV, XMP, `.dop`) are ignored. An unreadable or corrupt image file gets an error tile, and the scan continues.

### photosort-mvp.AC2: Thumbnail grid and keyboard culling
- **photosort-mvp.AC2.1 Success:** Each shot appears as exactly one tile. Tiles are in capture order (`DateTimeOriginal` + `SubSecTimeOriginal`), with filename as the tie-break.
- **photosort-mvp.AC2.2 Success:** Each tile shows the embedded preview, a format badge (`J`, `N`, `J+N`, `O`), and a checkbox.
- **photosort-mvp.AC2.3 Success:** RAW-only and JPEG-only shots both display previews.
- **photosort-mvp.AC2.4 Success:** Clicking the checkbox toggles the tick. Clicking the image opens the overlay.
- **photosort-mvp.AC2.5 Success:** Arrow keys move the focus ring, and the grid scrolls to keep the focused tile visible.
- **photosort-mvp.AC2.6 Success:** Space toggles the focused shot's tick. Shift+arrows extend a range, and Space then toggles every shot in it.
- **photosort-mvp.AC2.7 Success:** The tile-size slider and Ctrl+scroll resize tiles between 96 and 640 px. Columns reflow to the window width.
- **photosort-mvp.AC2.8 Edge:** A shot with no embedded preview shows a placeholder with its filename. The grid never decodes a full-resolution image.

### photosort-mvp.AC3: Preview overlay
- **photosort-mvp.AC3.1 Success:** Opening the overlay (Enter or a click) shows the embedded preview immediately. It's then replaced by a screen-resolution decode of the full file.
- **photosort-mvp.AC3.2 Success:** `Z` or a click toggles 100% zoom centred on the cursor, and dragging pans.
- **photosort-mvp.AC3.3 Success:** ←/→ move to the previous or next shot, skipping shots the hide-copied filter is hiding. Space toggles the tick. Esc returns to the grid with focus on the shot that was showing.
- **photosort-mvp.AC3.4 Success:** On a RAW-only NEF shot, 100% zoom shows the embedded full-size `JpgFromRaw` (6048×4032 on the Z f).
- **photosort-mvp.AC3.5 Success:** The info strip shows the filename(s), shutter speed, aperture, ISO and lens.
- **photosort-mvp.AC3.6 Failure:** If the full-file decode fails, the embedded preview stays visible with an error note, and the app doesn't crash.

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

### photosort-mvp.AC5: Output selection and copy
- **photosort-mvp.AC5.1 Success:** The output is either a remembered base folder plus a subfolder (existing, or new and created at copy time), or an arbitrary folder chosen with Browse.
- **photosort-mvp.AC5.2 Success:** Copy copies every file of each ticked shot. Before any lens edit, each copy is byte-identical to its source (BLAKE3) and has the source's modification time.
- **photosort-mvp.AC5.3 Success:** The source is never modified. A snapshot of the source tree (names, sizes, mtimes) is identical before and after a scan plus copy session.
- **photosort-mvp.AC5.4 Success:** Before copying, the confirmation shows the shot count, file count, total bytes, destination and rename count. Cancel copies nothing.
- **photosort-mvp.AC5.5 Failure:** Insufficient free space at the destination blocks the copy before any file is written.
- **photosort-mvp.AC5.6 Failure:** An unwritable destination blocks the copy with an error message.
- **photosort-mvp.AC5.7 Failure:** A verify hash mismatch fails that shot and removes its temp file. The batch continues, and the summary reports the failure.
- **photosort-mvp.AC5.8 Failure:** If the source disappears mid-batch, the remaining shots are aborted. The summary lists completed and failed shots, and ticks stay on every shot that didn't fully copy.
- **photosort-mvp.AC5.9 Success:** No `.photosort-tmp` files remain after any batch. The batch log records copies, skips, renames and errors.

### photosort-mvp.AC6: Name collisions
- **photosort-mvp.AC6.1 Success:** An existing destination file is never overwritten. That includes a file that appears between planning and the final rename.
- **photosort-mvp.AC6.2 Success:** When a different photo already has the name, both files of the pair are copied with the same next free suffix (`DSC_0001_2.NEF` + `DSC_0001_2.JPG`).
- **photosort-mvp.AC6.3 Success:** Two shots in one batch with the same name, from different DCIM folders, get distinct destination names.
- **photosort-mvp.AC6.4 Edge:** Name comparisons are case-insensitive, so `dsc_0001.jpg` clashes with `DSC_0001.JPG`.
- **photosort-mvp.AC6.5 Success:** A partial shot's missing file is copied using the stem of the partner already in the output (including a suffixed stem). If that name is taken, the file gets a fresh suffix and the shot is reported `unpaired`.
- **photosort-mvp.AC6.6 Success:** The copy summary lists every rename as source → destination.

### photosort-mvp.AC7: Already-copied detection
- **photosort-mvp.AC7.1 Success:** A shot whose files are in the current output folder is badged "copied". The match is by identity key regardless of filename, and also catches files copied by hand. If such a shot is ticked, the copy skips it and reports the skip.
- **photosort-mvp.AC7.2 Success:** A shot with only some file kinds in the output is badged "partial", and the copy brings over only the missing kinds.
- **photosort-mvp.AC7.3 Success:** Hide-copied hides copied shots and keeps partial ones visible. The output index refreshes when the output folder changes, after every copy, and on Rescan.

### photosort-mvp.AC8: Resume
- **photosort-mvp.AC8.1 Success:** Tick changes are saved within about 1 s. Closing and reopening the app on the same card restores them.
- **photosort-mvp.AC8.2 Success:** Ticks are keyed by identity, so they survive a rescan and the card mounting under a different drive letter.
- **photosort-mvp.AC8.3 Edge:** A reformatted card (new volume serial) starts with no ticks.
- **photosort-mvp.AC8.4 Edge:** Ticks for shots no longer on the card are pruned.
- **photosort-mvp.AC8.5 Success:** Base folders, last source, last output and tile size persist across restarts.
- **photosort-mvp.AC8.6 Failure:** A corrupt config or state file is renamed aside, and the app starts with defaults and tells the user. State writes are atomic, so an interrupted write leaves the previous valid file in place.

### photosort-mvp.AC9: Responsiveness at 7,000 shots
- **photosort-mvp.AC9.1 Success:** On a generated 7,000-shot source, placeholders for every shot appear within 3 s of selecting it.
- **photosort-mvp.AC9.2 Success:** While the metadata and preview pass runs, scrolling and ticking stay responsive, with no UI frame over 100 ms in the frame-time log.
- **photosort-mvp.AC9.3 Success:** Cache memory stays within the RAM budget: 25% of physical RAM by default, or the configured override. Evicted tiles reload when scrolled back into view.

### photosort-mvp.AC10: Lens profiles and per-shot aperture (stretch)
- **photosort-mvp.AC10.1 Success:** The user can create, edit and delete Chip and Manual profiles. Profiles live only in `%APPDATA%\photosort\config.json`, and a fresh install has none.
- **photosort-mvp.AC10.2 Success:** A Manual profile stores a focal length, an aperture range (max and min f-number), a stop step (full, half or third) and a default f-number.
- **photosort-mvp.AC10.3 Success:** A rule matching a reported `LensModel` exactly (e.g. `VR 80-200mm f/2.8G`), optionally scoped to a camera make/model, assigns its profile.
- **photosort-mvp.AC10.4 Success:** No-lens shots (empty `LensModel` and 0 mm or absent focal length) get the session manual profile set for their body. With no profile set for that body, they get no edit.
- **photosort-mvp.AC10.5 Edge:** A shot whose reported lens matches no rule gets no edit.
- **photosort-mvp.AC10.6 Success:** A per-shot override set with `L` (a profile, or "none") wins over rules and session profiles. It's persisted in the card state.
- **photosort-mvp.AC10.7 Success:** `[` / `]` and the overlay slider move a manual shot's f-number by the profile's stop step, within its range. The default is f/8, clamped to the nearest range end when f/8 falls outside the range.
- **photosort-mvp.AC10.8 Success:** Tiles show the profile and aperture. A default (unconfirmed) aperture is drawn visually distinct from a confirmed one, and setting the aperture marks it confirmed.
- **photosort-mvp.AC10.9 Success:** If any ticked manual-lens shots have unconfirmed apertures, the copy confirmation warns and offers Copy anyway, Cancel and Review apertures.
- **photosort-mvp.AC10.10 Success:** Review apertures steps only through those shots. Enter confirms the aperture and advances. After the last shot, it returns to the copy confirmation.
- **photosort-mvp.AC10.11 Success:** "Apply to all similar shots" assigns the chosen profile to every shot with the same body (make and serial) and the same reported lens as the focused shot.

### photosort-mvp.AC11: Lens EXIF writing (stretch)
- **photosort-mvp.AC11.1 Success:** A Chip edit sets the copy's ExifIFD `LensMake`, `LensModel` and `LensInfo` to the profile's values. `FocalLength`, `FNumber` and focus distance stay unchanged.
- **photosort-mvp.AC11.2 Success:** A Manual edit also writes `FocalLength`, `FocalLengthIn35mmFormat`, the shot's `FNumber`, `MaxApertureValue`, and `SubjectDistance` = infinity.
- **photosort-mvp.AC11.3 Success:** Maker-note data is byte-identical before and after an edit, as verified with ExifTool.
- **photosort-mvp.AC11.4 Success:** Edits apply to both the JPEG and the raw copy of a shot. Source files are never edited.
- **photosort-mvp.AC11.5 Failure:** If ExifTool is missing or returns an error, the copy is kept unedited and the summary flags the shot "lens not applied".
- **photosort-mvp.AC11.6 Success:** The batch log records the original lens field values of every edited file.

### photosort-mvp.AC12: Body-agnostic extensibility
- **photosort-mvp.AC12.1 Success:** A test-only `FormatReader` for a fake TIFF-based extension can be registered. Its files are then scanned, grouped, identified, planned and copied, with no change to grouping, identity, planning or copy code.
- **photosort-mvp.AC12.2 Edge:** A file whose `Make` has no `MakerNoteReader` falls to the capture or content identity key without error.
- **photosort-mvp.AC12.3 Success:** A rule scoped to `NIKON CORPORATION` / `NIKON Z f` doesn't apply to shots from another body with the same reported `LensModel`.
- **photosort-mvp.AC12.4 Success:** In a source containing shots from two bodies, each body's no-lens shots get that body's session manual profile.
- **photosort-mvp.AC12.5 Edge:** For a format whose reader reports `lens_write_supported = false`, the copy is kept unedited and the summary notes "lens edit unsupported for format".

## Glossary

- **Shot**: One photo as the user thinks of it. It's 1 or 2 files in the same directory with the same case-insensitive stem, such as a RAW+JPEG pair. A shot is the unit of the grid, ticking and copying.
- **RAW+JPEG pair**: A camera setting that saves each frame as both a raw file and a JPEG. photosort shows the pair as one tile.
- **NEF / ORF / ARW**: Raw file extensions for Nikon, OM System (Olympus) and Sony. NEF and ORF are in scope. ORF is blocked on sample files, and ARW is a later addition.
- **DCIM**: The top-level folder cameras write photos into. It has numbered subfolders (e.g. `100NZ_F`, `101NZ_F`) that can hold the same filename more than once.
- **EXIF**: Standard metadata embedded in image files: camera make and model, capture time, ISO, exposure, lens and so on.
- **Maker note**: A vendor-specific EXIF block holding fields the standard doesn't cover, such as Nikon's shutter count. Its layout is proprietary, and each vendor needs its own reader.
- **TIFF / IFD / ExifIFD**: TIFF is the container structure that EXIF and NEF files are built on. An IFD (image file directory) is one table of tagged entries in it. The ExifIFD is the one holding the lens and exposure fields photosort edits. PreviewIFD is the NEF directory that points at the preview image.
- **MPF (Multi-Picture Format)**: A JPEG extension that stores extra images, including the 640×424 preview, usually near the end of the file.
- **`JpgFromRaw`**: The full-size JPEG embedded inside a NEF (6048×4032 on the Z f). The overlay uses it for 100% zoom on RAW-only shots, so photosort never demosaics raw data.
- **Demosaicing**: Converting raw sensor data into a viewable image. It's out of scope.
- **Z f**: The Nikon Z f camera. It's the MVP's reference body, and the sample files and expected sizes come from it.
- **`ShutterCount`**: A Nikon maker-note field (tag `0x00A7`) counting shutter activations. It's the basis of the strongest identity rung. photosort only reads it. Per ExifTool's tag docs, writing it re-encrypts Nikon's other encrypted data.
- **`DateTimeOriginal` / `SubSecTimeOriginal`**: EXIF capture time to the second, and the fractional-second part. Together they order tiles and feed the identity key.
- **`FormatReader`**: A trait implemented once per file format, registered by extension. It parses the container and locates previews, and it reports whether lens writes are safe for that format.
- **`MakerNoteReader`**: A trait implemented once per camera vendor, chosen by EXIF `Make`. It supplies optional vendor fields, chiefly the shot counter. A make with no reader isn't an error.
- **Shot counter**: The vendor-neutral name for a camera's frame counter. Nikon's is `ShutterCount`.
- **Identity key / identity ladder**: A string key that identifies a photo independent of filename or lens edits. It's built from the best available rung: counter key, then capture key, then content key.
- **Output index (`OutputIndex`)**: A map of identity keys to the files already in the current output folder. It drives the "copied" and "partial" badges and the skip logic.
- **Copied / partial / not copied**: A shot's status against the output folder. Copied means every file kind is there, partial means some are, and not copied means none.
- **Unpaired**: A copy summary flag for a partial shot whose missing file couldn't reuse its partner's stem in the output, so it got a fresh suffix.
- **Copy plan (`CopyPlan`)**: The pure-logic result of planning. It says which files to skip, what to name each copy, how collisions resolve and which lens edit applies, all before any I/O.
- **Chip profile / Manual profile**: The two lens profile types. A Chip profile is for an electronic (chipped) adapter and only fixes lens identity fields. A Manual profile is for a dumb adapter and also writes focal length, aperture range and a per-shot f-number.
- **Fringer**: A brand of electronic lens adapter. The document uses it as the example of a chipped adapter.
- **Dumb adapter**: A purely mechanical lens adapter. The camera reports no lens and 0 mm, so photosort has to supply the lens data.
- **No-lens shot**: A shot with an empty `LensModel` and a focal length of 0 or absent, the signature of a dumb adapter.
- **Lens rule (`LensRule`)**: A mapping from what the camera reported (an exact `LensModel` string, or "no lens") to a profile. It can be scoped to a camera make or model, and the first match in list order wins.
- **Session manual profile**: The manual profile the user picks for a body, which applies to that body's no-lens shots. It exists because several manual lenses all report the same "no lens" data.
- **Per-shot override**: A profile (or "none") set on one shot, which beats rules and session profiles. It's saved in the card state file.
- **Confirmed / unconfirmed aperture**: A manual-lens shot starts with the profile's default f-number (unconfirmed). Setting it explicitly marks it confirmed. The UI and copy confirmation flag unconfirmed ones.
- **Stop step**: Whether aperture adjusts in full, half or third stops.
- **`LensMake` / `LensModel` / `LensInfo`**: EXIF fields identifying the lens. `LensInfo` is min/max focal length and max aperture at each.
- **`FocalLengthIn35mmFormat`, `MaxApertureValue`, `SubjectDistance`**: Other EXIF fields a Manual edit writes. `SubjectDistance` is set to infinity.
- **DX crop mode**: A Nikon mode that uses a smaller part of the sensor. `FocalLengthIn35mmFormat` will be wrong for those shots, which the document accepts for the MVP.
- **DxO PhotoLab / FilmPack**: Raw-processing software whose lens-module suggestion is the goal of the lens-editing stretch work. What it reads from the files is unverified.
- **ExifTool**: A command-line metadata tool. In photosort it's the writer for lens edits (bundled, not committed) and the reference oracle for parser tests.
- **`-stay_open` / `-execute` / `{ready}`**: ExifTool's persistent-process mode. photosort feeds it batches of arguments, each ended with `-execute`, and waits for `{ready}` before sending the next.
- **Exiv2**: An EXIF library. The document cites its history of corrupting Nikon maker notes on raw writes as a reason not to use it.
- **Oracle test**: A test that checks parser output against a trusted reference, here `exiftool -json` on real samples. It's `#[ignore]`d and opt-in via `PHOTOSORT_SAMPLES` and `PHOTOSORT_EXIFTOOL`.
- **Property and fuzz tests**: Tests that check invariants over generated or random inputs instead of fixed examples. Here they check that malformed input never panics and that copy plans never collide.
- **BLAKE3**: A fast cryptographic hash. It verifies copies byte-for-byte and forms the content identity key (first 64 KB plus file size).
- **mtime**: File modification time. Copies keep the source's mtime.
- **`MoveFileExW` / `MOVEFILE_REPLACE_EXISTING`**: The Win32 rename call. Omitting the flag makes it fail if the destination exists, which is how photosort guarantees no overwrites.
- **Atomic write**: Writing to a temp file in the same directory, then renaming it into place, so an interrupted write leaves the old file intact.
- **Debounce**: Delaying a save until changes have stopped for a short interval (about 1 s here), so rapid ticking doesn't cause a write per click.
- **Volume serial number**: A disk's serial number, read via `GetVolumeInformationW`. It identifies a card across drive-letter changes, and a reformat produces a new one.
- **egui / eframe**: A Rust immediate-mode GUI library and its app framework. The UI is redrawn from state each frame, and workers call `ctx.request_repaint()` to prompt a redraw.
- **Virtualized grid (`ScrollArea::show_rows`)**: A scroll area that only builds the rows currently visible, which keeps thousands of tiles cheap.
- **LRU cache**: A cache that evicts the least recently used entry when it hits its size cap. photosort keeps one for preview bytes and one for textures.
- **`GlobalMemoryStatusEx`**: A Windows API for physical RAM size, used to set the cache budget (25% by default).
- **Functional core, imperative shell**: A design pattern where pure data-in, data-out logic lives in one place and all I/O lives in a thin outer layer. Here that's the crate boundary.
- **`rfd`**: A Rust native file-dialog crate, used for the folder pickers.
- **Smart Sync**: A Dropbox feature that downloads online-only files on access. photosort indexes only the current output folder so it doesn't trigger downloads.

## Architecture

photosort is a single-window egui/eframe 0.37 desktop app for Windows. It's split into a pure library crate and an application crate, so all decision-making logic is testable without a card, a disk, or a GPU.

```
crates/
  photosort-core/   pure: parsing, identity, grouping, copy planning, lens rules, state types
  photosort/        shell: filesystem, worker threads, textures, ExifTool process, egui UI
```

### Approach

Reads happen in-process in Rust. Writes (lens EXIF, stretch goal only) go through a bundled ExifTool. Rust keeps the card scan fast, and it means the core app works without any external tool. ExifTool is used for writes because no pure-Rust crate writes NEF safely; Exiv2 has a history of corrupting Nikon maker notes on raw writes. A full-Rust writer is on the roadmap, not in the MVP.

### `photosort-core` (functional core)

- **Metadata parsers**: pure functions over byte slices, organised as two registries so that no other part of the system branches on camera vendor:
  - **Format readers** (`FormatReader`), registered by file extension. Each one parses its container and locates the previews:
    - JPEG: EXIF from APP1, and the 640×424 preview from MPF, which usually sits near the end of the file.
    - NEF: TIFF IFDs, the PreviewIFD preview about 100 KB into the file, and the full-size `JpgFromRaw`.
    - ORF: to be written once OM System samples exist.
    - ARW and others can be added later.
  - **Maker-note readers** (`MakerNoteReader`), chosen by EXIF `Make`. Each one supplies optional vendor-specific fields, chiefly a vendor-neutral **shot counter**.
    - The Nikon reader uses `ShutterCount` (tag `0x00A7`). It's read-only, and photosort never writes it, because ExifTool's tag docs say writing it re-encrypts Nikon's other encrypted data.
    - OM System and Sony readers come later.
    - A make with no reader is not an error. Identity just falls to a lower rung.
  - A shared TIFF/EXIF IFD walker underpins all TIFF-based formats.
- **Grouping**: turns a file listing into `Shot`s keyed by *source directory + case-insensitive stem*, so `100NZ_F\DSC_0001` and `101NZ_F\DSC_0001` are distinct shots.
- **Identity**: derives an `IdentityKey` per file from parsed metadata, using the ladder below.
- **Copy planning**: builds a `CopyPlan` from the checked shots, the `OutputIndex`, and the existing destination names. This step decides which files to skip, what to name each copy, how to resolve collisions, and which lens edit applies.
- **Lens resolution**: maps each shot to a `LensEdit` from, in priority order:
  1. a per-shot override;
  2. the first matching rule, where rules are predicates over reported lens fields, optionally scoped to a camera make/model;
  3. the session manual profile for that body's serial, for no-lens shots.
- **State types**: the serde types for `config.json` and the per-card state files.

### `photosort` (imperative shell)

- **Scanner** (`scan.rs`):
  - Pass 1 lists the source tree and emits shots right away as placeholders.
  - Pass 2 runs on a worker pool. It reads each file's header chunk, plus the MPF tail range for JPEGs, then emits metadata, identity, and preview bytes.
  - Work is prioritized by what's on screen.
- **Preview service** (`preview.rs`, `cache.rs`):
  - Decodes 640 px previews into tile textures.
  - Decodes full files for the overlay.
  - Keeps two LRU caches: preview bytes and textures. Together they're capped at a budget that defaults to 25% of physical RAM (via `GlobalMemoryStatusEx`), with an absolute override in settings.
- **Output indexer** (`output_index.rs`): reads image headers in the current output folder and builds the `OutputIndex`. It re-runs when the output folder changes, after each copy, and on Rescan.
- **Copy executor** (`copy.rs`): runs a `CopyPlan`, file by file, as temp copy → verify → set mtime → lens edit (stretch) → no-overwrite rename.
- **Persistence** (`persist.rs`, `volume.rs`):
  - Writes `config.json` and the per-card state files under `%APPDATA%\photosort\`.
  - Writes are debounced to about 1 s after the last change and are atomic (temp file, then rename).
  - The card id comes from the volume serial number.
- **ExifTool runner** (`exiftool.rs`, stretch): one `-stay_open True -@ -` process fed argument batches. Each batch is terminated by `-execute`, and the runner waits for the `{ready}` reply.
- **UI** (`ui/`):
  - The top bar: source, output base/subfolder/browse, session lens profile, tile-size slider, hide-copied toggle, status, and the Copy button.
  - The virtualized grid (`ScrollArea::show_rows`, one egui row per grid row).
  - The preview overlay, and dialogs for copy confirmation, aperture review, summary, and settings/profile editor.

Workers send results to the UI over channels and call `ctx.request_repaint()`. The UI thread never does file I/O.

### Shot identity ladder

A file's identity is built only from fields that lens edits never touch. That way a lens-edited or renamed copy still matches its source.

1. **Counter key**: `Make` + camera serial + vendor shot counter + `DateTimeOriginal` + `SubSecTimeOriginal`. Used when a maker-note reader supplies a counter, which today means Nikon `ShutterCount`.
2. **Capture key**: `Make` + serial + `DateTimeOriginal` + `SubSecTimeOriginal` + `ISO` + `ExposureTime`. Used when there's EXIF but no counter, for example a body without a maker-note reader yet.
3. **Content key**: file size + BLAKE3 hash of the first 64 KB. Used when there's no usable EXIF.

`FNumber` and `FocalLength` are deliberately excluded: manual profiles overwrite them on copies.

The DoD names only serial + `ShutterCount` for rung 1. The capture time was added to it because nobody has confirmed whether the Z f's ShutterCount advances on every frame of an electronic-shutter burst, and if it doesn't, burst frames would collide.

The output index stores `(IdentityKey, FileKind)` pairs. A shot is **copied** when every one of its file kinds is present, **partial** when some are present, and **not copied** otherwise.

### Preview model

| Use | Source | Notes |
|---|---|---|
| Grid tile (96–640 px) | Embedded 640×424 preview (JPEG: MPF; NEF: PreviewIFD) | Never decodes the full image. Tiles above 640 px aren't offered. |
| Overlay, fit-to-window | Embedded preview shown instantly, then a screen-resolution decode of the full file | The ±2 neighbouring shots are prefetched. |
| Overlay, 100% zoom | Full JPEG decoded from disk; RAW-only shots use NEF `JpgFromRaw` (full size) | Raw demosaicing is out of scope. |

There's no disk thumbnail cache. Reopening a card re-scans it, which is estimated at 10–20 s for a 7,000-shot card but hasn't been measured.

### Contracts

These types cross phase boundaries and get persisted, so their shape is fixed here. Field names are indicative; the implementation may add fields.

```rust
// Format identity is data, not a closed vendor enum: new raw formats register a reader.
pub struct FileKind(pub &'static str);   // "jpeg", "nef", "orf", later "arw", ...

pub trait FormatReader: Send + Sync {
    fn kind(&self) -> FileKind;
    fn extensions(&self) -> &'static [&'static str];        // Lowercase, e.g. ["nef"].
    fn is_raw(&self) -> bool;
    fn header_len_hint(&self) -> usize;                     // Bytes to read for metadata.
    fn parse(&self, head: &[u8], file_len: u64) -> Result<Metadata, ParseError>;
    fn lens_write_supported(&self) -> bool;                 // Allowlist: JPEG, NEF in MVP.
}

pub trait MakerNoteReader: Send + Sync {
    fn matches_make(&self, make: &str) -> bool;             // E.g. "NIKON CORPORATION".
    fn read(&self, maker_note: &[u8], ctx: &TiffContext) -> VendorFields;
}
pub struct VendorFields { pub shot_counter: Option<u64> /* plus vendor extras */ }

pub struct ShotFile {
    pub path: PathBuf,            // Absolute path on the source volume.
    pub kind: FileKind,
    pub size: u64,
    pub modified: SystemTime,
    pub meta: Option<Metadata>,   // None until pass 2 parses it, or if unparseable.
    pub identity: Option<IdentityKey>,
}

pub struct Shot {
    pub id: ShotId,               // Stable within one scan.
    pub dir: PathBuf,             // Source directory.
    pub stem: String,             // Original filename stem, original case.
    pub files: Vec<ShotFile>,     // 1..=2 files, at most one per FileKind.
}

pub struct Metadata {
    pub make: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub date_time_original: Option<String>,   // EXIF "YYYY:MM:DD HH:MM:SS".
    pub subsec_time_original: Option<String>,
    pub iso: Option<u32>,
    pub exposure_time: Option<Rational>,
    pub f_number: Option<Rational>,
    pub focal_length: Option<Rational>,
    pub lens_make: Option<String>,
    pub lens_model: Option<String>,
    pub shot_counter: Option<u64>,            // From the vendor MakerNoteReader (Nikon: ShutterCount 0x00A7).
    pub preview: Option<ByteRange>,           // Embedded ~640px JPEG.
    pub full_jpeg: Option<ByteRange>,         // NEF JpgFromRaw.
}

pub enum IdentityKey {
    Counter { make: String, serial: String, counter: u64, dto: String, subsec: String },
    Capture { make: String, serial: String, dto: String, subsec: String, iso: u32, exposure: Rational },
    Content { size: u64, head_blake3: [u8; 32] },
}
// Serialized as a single string, e.g. "ctr:NIKON CORPORATION:1234567:10042:2026:11:20 14:59:32:92" (fake serial).

pub enum CopyStatus { NotCopied, Partial, Copied }

pub struct OutputIndex {
    pub by_identity: HashMap<IdentityKey, Vec<(FileKind, String)>>, // Kind + existing file name.
    pub names_lower: HashSet<String>,   // All names in the folder, lowercased; Windows is case-insensitive.
}

pub struct CopyPlan { pub shots: Vec<PlannedShot>, pub total_bytes: u64 }
pub struct PlannedShot {
    pub shot: ShotId,
    pub files: Vec<PlannedFile>,        // Only kinds missing from the output.
    pub renamed: bool,                  // A suffix was applied.
    pub unpaired: bool,                 // Couldn't match an existing partner's stem.
}
pub struct PlannedFile { pub src: PathBuf, pub dest_name: String, pub lens_edit: Option<LensEdit> }

pub enum StopStep { Full, Half, Third }
pub enum LensProfileKind {
    Chip,                               // Electronic/chipped adapter: identity only.
    Manual { focal_mm: f32, max_f: f32, min_f: f32, step: StopStep, default_f: f32 },
}
pub struct LensProfile {
    pub id: Uuid,
    pub name: String,                   // E.g. "Canon FD 28mm f/1.2".
    pub lens_make: String,
    pub lens_model: String,
    pub lens_info: LensInfo,            // Min/max focal length, max aperture at each.
    pub kind: LensProfileKind,
}
pub struct LensRule {
    pub when: LensMatch,
    pub camera: Option<CameraScope>,    // None = any body.
    pub profile: Uuid,
}                                       // Rules are evaluated in list order; first match wins.
pub enum LensMatch {
    ReportedModel(String),              // Exact LensModel string, e.g. "VR 80-200mm f/2.8G".
    NoLens,                             // Empty LensModel and FocalLength 0 (or absent).
}
pub struct CameraScope { pub make: String, pub model: Option<String> }  // E.g. NIKON CORPORATION / NIKON Z f.

pub struct LensEdit {
    pub profile: Uuid,
    pub f_number: Option<f32>,          // Manual profiles only.
    pub aperture_confirmed: bool,       // False = still the default; flagged in UI.
}
```

**`%APPDATA%\photosort\config.json`** (never shipped in the repo; the app starts with no profiles):
`{ "version": 1, "base_folders": [..], "last_source": .., "last_output": .., "ram_budget_mb": null | n, "tile_size": n, "exiftool_path": null | path, "profiles": [LensProfile], "rules": [LensRule] }`

**`%APPDATA%\photosort\cards\<volume-serial>_<rel-path-hash>.json`**:
`{ "version": 1, "volume_serial": "..", "rel_path": "DCIM", "ticked": [IdentityKey], "overrides": { IdentityKey: { "profile": Uuid | "none", "f_number": f, "aperture_confirmed": bool } }, "session_manual_profiles": { "<make>:<serial>": Uuid }, "last_output": path }`

The session manual profile is chosen per camera body. The top-bar dropdown sets it for the body of the focused shot, and shows one entry per body when the source holds shots from several.

**`%APPDATA%\photosort\logs\YYYY-MM-DD.log`**: one block per copy batch. It records source, destination, each copied file, skips, renames, lens edits with the **original** lens field values, and errors.

### Copy pipeline

1. **Pre-flight.** Destination exists or can be created, is writable, and has free space ≥ `CopyPlan.total_bytes`. Any failure blocks the copy before a single byte is written.
2. **Naming** (core).
   - Each shot keeps its original stem unless any of its destination names collides, case-insensitively, with an existing name or another shot in the batch. Then the whole shot takes the next free `_2`, `_3`, … suffix.
   - A partial shot reuses its partner's existing stem in the output. If that name is taken, it gets a fresh suffix and is marked `unpaired`.
3. **Per file.**
   1. Stream from the card into `.<dest_name>.photosort-tmp` in the destination, hashing with BLAKE3 on the way.
   2. Read the temp file back and compare hashes.
   3. Set the mtime to the source's.
   4. *(Stretch)* Apply the lens edit with ExifTool, but only if the file's `FormatReader` reports `lens_write_supported`. Otherwise the copy is kept unedited and noted as "lens edit unsupported for format". If ExifTool fails, keep the unedited copy and flag "lens not applied".
   5. `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING`. If the name has appeared since planning, re-plan that shot with the next suffix.
4. **Failure scope.** A file error fails its shot, and the batch continues. Destination-full or source-gone aborts the remaining shots. Temp files are always removed. Ticks are cleared only for shots that fully succeeded.

The source is opened read-only, and photosort never writes anything under the source root.

### UI summary

- **Grid keys:**
  - arrows move focus;
  - Shift+arrows extend a range;
  - Space ticks the focused shot or range;
  - Enter opens the overlay;
  - `[` / `]` step the manual aperture;
  - `L` picks the lens profile for the focused shot or range;
  - Ctrl+scroll resizes tiles.
- **Overlay keys:**
  - ←/→ go to the previous or next shot;
  - Space ticks;
  - `Z` or a click toggles 100% at the cursor, and you drag to pan;
  - `[` / `]` and `L` work as in the grid;
  - Esc returns to the grid with focus on the current shot.
- **Tile badges:** formats present (`J`, `N`, `J+N`, `O`), copied or partial, and the lens profile. Manual-profile tiles also show the aperture, e.g. `FD28 f/8`. An unconfirmed (default) aperture is drawn visually distinct from a confirmed one.
- **Copy confirmation:**
  - It shows N shots, M files, size, destination, and how many shots will be renamed.
  - If any ticked manual-lens shots have unconfirmed apertures, it adds a warning with three choices:
    - **Copy anyway**;
    - **Cancel**;
    - **Review apertures**: opens the overlay restricted to those shots. `[` / `]` adjust the aperture, and Enter confirms it and moves to the next shot. After the last one, you return to the confirmation.

## Existing patterns

There aren't any: the repository was empty when this design was written. It introduces these patterns:

- **Vendor knowledge lives only in registries.** Format readers and maker-note readers are the only code that knows about Nikon, OM System or Sony. Grouping, identity, planning, copy, persistence and UI see only `FileKind`, `Metadata` and `IdentityKey`. Adding a body means adding readers, plus a `lens_write_supported` flip once its writes have been verified.
- **Functional core, imperative shell**, split at the crate level. `photosort-core` has no filesystem, thread, or UI dependencies, and every decision that can be expressed as data in → data out lives there.
- **Message passing to the UI**: worker threads own I/O and send typed results over channels. The egui `App` owns all UI state.
- **Atomic writes** for every file photosort creates: temp file in the same directory, then rename. Copies use a no-overwrite rename.
- **ExifTool as test oracle**: parser correctness is checked against ExifTool output on real samples. The opt-in test requires the `PHOTOSORT_SAMPLES` and `PHOTOSORT_EXIFTOOL` environment variables.
- **Synthetic fixtures only** in the public repo (fake serials, generated image content). Real camera files stay in the git-ignored `samples/` directory or outside the repo.

## Implementation phases

<!-- START_PHASE_1 -->
### Phase 1: Workspace scaffold and CI
**Goal:** A buildable workspace that opens an empty photosort window.

**Components:**
- Workspace `Cargo.toml` with members `crates/photosort-core` and `crates/photosort`; `rust-toolchain.toml` pinned to the stable toolchain.
- `crates/photosort-core/src/lib.rs`, an empty library.
- `crates/photosort/src/main.rs`, which opens an eframe 0.37 window titled "photosort".
- `.github/workflows/ci.yml` running on `windows-latest`: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`.
- `README.md` with build instructions.

**Dependencies:** None.

**Done when:** `cargo build` and `cargo test` succeed locally, the window opens, and CI passes on the first push.
<!-- END_PHASE_1 -->

<!-- START_PHASE_2 -->
### Phase 2: Metadata parsing and identity
**Goal:** Pure parsers that extract everything the app needs from JPEG and NEF headers, and the identity ladder built on them.

**Components:**
- `crates/photosort-core/src/metadata/`:
  - `registry.rs`: `FormatReader` and `MakerNoteReader` traits, plus lookup by extension and by `Make`.
  - `tiff.rs`: shared IFD walker with byte-order handling and bounds-checked offsets.
  - `exif.rs`: standard EXIF fields.
  - `formats/jpeg.rs`: APP1 EXIF and MPF preview.
  - `formats/nef.rs`: PreviewIFD preview and `JpgFromRaw`.
  - `formats/orf.rs`: blocked on OM-1 Mark II samples.
  - `makernotes/nikon.rs`: header and `ShutterCount` → `shot_counter`.
- `crates/photosort-core/src/identity.rs`: the vendor-neutral identity ladder and its string serialization.
- `crates/photosort-core/src/model.rs`: the `FileKind`, `Metadata`, `IdentityKey` and `ByteRange` contracts.
- Synthetic fixture builder (test support in `photosort-core`) and `crates/photosort-core/tests/fixtures/`.
- `crates/photosort-core/tests/oracle.rs`: an `#[ignore]`d test comparing parser output against `exiftool -json` across `PHOTOSORT_SAMPLES`.

**Dependencies:** Phase 1.

**Covers:** photosort-mvp.AC4.1–AC4.6, AC12.1–AC12.2.

**Done when:**
- Unit tests pass on the synthetic fixtures, including truncated and malformed inputs.
- The oracle test passes on the local Z f samples.
- ORF support is either verified against OM-1 Mark II samples or explicitly deferred with a tracking issue.
<!-- END_PHASE_2 -->

<!-- START_PHASE_3 -->
### Phase 3: Card scan and thumbnail grid
**Goal:** Pick a source folder and cull it from a responsive, keyboard-driven thumbnail grid.

**Components:**
- `crates/photosort-core/src/grouping.rs`: files to `Shot`s by directory + case-insensitive stem.
- `crates/photosort/src/scan.rs`: the two-pass scanner on a worker pool, with on-screen-first priority.
- `crates/photosort/src/preview.rs` and `cache.rs`:
  - preview decode and texture upload;
  - preview-bytes and texture LRUs under a RAM budget (default 25% of physical RAM, override in settings).
- `crates/photosort/src/ui/top_bar.rs`: source picker via `rfd`, Rescan, tile-size slider, status line.
- `crates/photosort/src/ui/grid.rs`:
  - virtualized grid;
  - tiles with placeholder, checkbox and format badge;
  - focus ring, arrows, Shift-range, Space, Ctrl+scroll.
- In-memory tick state.

**Dependencies:** Phase 2.

**Covers:** photosort-mvp.AC1.1–AC1.3, AC2.1–AC2.8, AC9.1–AC9.3.

**Done when:**
- Grouping tests pass.
- Scanner tests pass against a generated temp-dir card.
- The responsiveness check on a generated 7,000-shot card meets AC9.
- The grid keyboard behaviour works per the manual test plan.
<!-- END_PHASE_3 -->

<!-- START_PHASE_4 -->
### Phase 4: Preview overlay
**Goal:** Inspect a shot at screen size and at 100% without leaving the keyboard.

**Components:**
- `crates/photosort/src/ui/overlay.rs`:
  - fit view, using the embedded preview first and then a screen-res decode;
  - Z or click toggles 100% at the cursor, with drag to pan;
  - ←/→, Space and Esc;
  - EXIF info strip.
- Full-file decode path in `preview.rs`: JPEG, or NEF `JpgFromRaw` for RAW-only shots.
- ±2 prefetch.

**Dependencies:** Phase 3.

**Covers:** photosort-mvp.AC3.1–AC3.6.

**Done when:** Decode-source selection tests pass (JPEG vs `JpgFromRaw` vs embedded fallback), and the overlay behaviour works per the manual test plan.
<!-- END_PHASE_4 -->

<!-- START_PHASE_5 -->
### Phase 5: Output folder, already-copied detection, and copy
**Goal:** Copy checked shots safely into the chosen output folder, never twice and never overwriting.

**Components:**
- `crates/photosort/src/ui/top_bar.rs` additions:
  - output base dropdown, subfolder field and Browse;
  - Hide-copied toggle;
  - Copy button.
- `crates/photosort/src/output_index.rs`: builds `OutputIndex`, then derives copied, partial and not-copied status per shot.
- `crates/photosort-core/src/plan.rs`: `CopyPlan` construction covering skips, suffixes, partner-stem reuse, `unpaired`, and in-batch collisions.
- `crates/photosort/src/copy.rs`: pre-flight, temp copy with BLAKE3 verify, mtime, `MoveFileExW` no-replace, re-plan on late collision, failure scoping.
- `crates/photosort/src/ui/dialogs.rs`: confirmation, progress and summary.
- Batch log writer for `%APPDATA%\photosort\logs\`.

**Dependencies:** Phases 2–3.

**Covers:** photosort-mvp.AC5.1–AC5.9, AC6.1–AC6.6, AC7.1–AC7.3.

**Done when:**
- Property tests on `plan.rs` pass:
  - no destination ever names an existing file;
  - every shot's files share one stem;
  - all destinations in a plan are unique, case-insensitively.
- Copy-executor integration tests in temp dirs pass for each failure case.
<!-- END_PHASE_5 -->

<!-- START_PHASE_6 -->
### Phase 6: Persistence and resume
**Goal:** Close the app mid-sort and pick up exactly where you left off.

**Components:**
- `crates/photosort-core/src/state.rs`: serde types for `config.json` and the card state file, with a `version` field.
- `crates/photosort/src/persist.rs`: load and save with debounced atomic writes; a corrupt file is renamed aside, not crashed on.
- `crates/photosort/src/volume.rs`: volume serial number lookup (`GetVolumeInformationW`) and the card id.
- Restore of ticks by identity key as pass-2 metadata arrives; pruning of keys no longer on the card; remembered base folders and last source/output.

**Dependencies:** Phase 5.

**Covers:** photosort-mvp.AC8.1–AC8.6.

**Done when:** State round-trip and corrupt-file tests pass, and the resume scenarios in the manual test plan pass.
<!-- END_PHASE_6 -->

<!-- START_PHASE_7 -->
### Phase 7: Lens profiles, rules, and per-shot aperture (stretch)
**Goal:** Decide which lens edit each copied shot will get, and let the user review and adjust it. Nothing is written to files yet.

**Components:**
- `crates/photosort-core/src/lens.rs`:
  - `LensProfile`, `LensRule`, `LensMatch`, `CameraScope` and `LensEdit` contracts;
  - resolution in priority order: override, then first matching camera-scoped rule, then the per-body session manual profile;
  - an "apply to similar" query: all shots with the same body and reported lens as a given shot;
  - f-stop scales for full, half and third stops;
  - default-f clamping to the profile range.
- `crates/photosort/src/ui/settings.rs`: profile editor (add, edit, delete) and rule editor; ExifTool path setting.
- Per-body session manual profile dropdown in the top bar.
- `L` profile picker, with an "apply to all similar shots" action (same body + same reported lens).
- `[` / `]` aperture stepping and the overlay aperture slider.
- Tile lens and aperture badges, with the confirmed/unconfirmed distinction.
- Copy-confirmation warning with Copy anyway / Cancel / Review apertures, and the review flow.
- Overrides persisted in the card state file.

**Dependencies:** Phases 5–6.

**Covers:** photosort-mvp.AC10.1–AC10.11, AC12.3–AC12.4.

**Done when:** `lens.rs` resolution and f-stop tests pass, and the profile, override and review flows work per the manual test plan.
<!-- END_PHASE_7 -->

<!-- START_PHASE_8 -->
### Phase 8: Lens EXIF writing via ExifTool (stretch)
**Goal:** Apply resolved lens edits to copies during the copy pipeline.

**Components:**
- `crates/photosort/src/exiftool.rs`: a `-stay_open` process wrapper with argument batches, `{ready}` handling, error parsing, and restart on crash.
- Copy pipeline step 4 in `copy.rs`.
  - Every profile writes ExifIFD `LensMake`, `LensModel` and `LensInfo`.
  - Manual profiles also write `FocalLength`, `FocalLengthIn35mmFormat`, `FNumber`, `MaxApertureValue`, and `SubjectDistance` = infinity.
  - Maker notes are never written.
  - On failure, keep the unedited copy and report it.
- Original lens values recorded in the batch log.
- `scripts/package.ps1`: builds a release folder and downloads ExifTool 13.64 (`exiftool-13.64_64.zip`, SHA-256 `72c29c44d1e860eebcaefa71692d979af9858fe9a6d7d791a5e7f1dac77804af`), verifying the checksum. ExifTool is never committed.

**Dependencies:** Phase 7.

**Covers:** photosort-mvp.AC11.1–AC11.6, AC12.5.

**Done when:**
- Integration tests pass that write edits to synthetic JPEG and NEF copies and verify the result with ExifTool. These skip when ExifTool is absent.
- Maker-note bytes are shown to be unchanged.
- A manual DxO PhotoLab check on real copies is recorded in the test plan.
<!-- END_PHASE_8 -->

## Additional considerations

**DxO is unverified.**
- DxO's support article says PhotoLab assigns modules from lens and camera metadata, and may suggest modules for other lenses when no module exists for the pair. Its field list couldn't be fetched (HTTP 403).
- The realistic goal is that DxO stops proposing a wrong module, not that it picks a Canon module.
- Nikon maker-note fields (`LensMountType: F-mount`, `LensType: G`) stay as the camera wrote them. If DxO keys on those, rewriting ExifIFD fields won't be enough.
- Before starting Phase 8, hand-edit `LensModel`/`LensMake` on a scratch copy of a Fringer shot with ExifTool and open it in PhotoLab.

**Z f burst behaviour is unverified.** ExifTool's docs say `ShutterCount` "includes both mechanical and electronic shutter activations for models with this feature." Nobody has confirmed whether the Z f counts every C30/C120 frame. That's why rung 1 of the ladder includes capture time. Verify it in Phase 2 with a short burst sample.

**`SubSecTimeOriginal` has hundredth-of-a-second precision** on the Z f samples. Rung 2 could collide for frames under 10 ms apart that also share ISO and exposure. That doesn't affect Nikon files, which use rung 1.

**`FocalLengthIn35mmFormat` assumes full-frame capture.** It'll be wrong for shots taken in the Z f's DX crop mode. That's accepted for the MVP.

**Dropbox.** Only the current output folder is indexed, and the user keeps working folders fully local. So indexing doesn't trigger Smart Sync downloads.

**OM System / Olympus.** ORF parsing is blocked on OM-1 Mark II sample files from the user. Still unverified until those samples arrive:
- where the ORF embedded preview lives (reportedly inside the maker notes);
- whether OM System exposes a usable shot counter;
- whether ExifTool lens writes to ORF are safe.

**Adding a body later.** Adding Sony or another body takes:
- a `FormatReader` (e.g. ARW);
- optionally a `MakerNoteReader` for a shot counter;
- oracle-test samples;
- a decision on `lens_write_supported` after verifying ExifTool writes on that format.

No changes to grouping, identity, planning, copy, persistence or UI should be needed, and AC12 tests enforce that boundary.
