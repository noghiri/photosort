# photosort MVP Implementation Plan — Phase 3: Card scan and thumbnail grid

**Goal:** Pick a source folder and cull it from a responsive, keyboard-driven thumbnail grid.

**Architecture:**
- `photosort-core` gains shot grouping, capture ordering and tile-preview selection (pure).
- The `photosort` shell gains:
  - a two-pass scanner (`scan.rs`): pass 1 lists and groups immediately; pass 2 parses heads and loads previews on a worker pool whose queue (`scan_queue.rs`, pure) serves visible shots first;
  - a preview decoder (`preview.rs`);
  - byte-budgeted caches (`budget.rs`, `cache.rs`);
  - the egui UI (`ui/top_bar.rs`, `ui/grid.rs`), backed by pure grid logic (`ui/grid_math.rs`).
- Workers send typed events over a bounded channel and wake the UI through an injected waker closure (`ctx.request_repaint()` in the app). The UI thread never touches the filesystem.

**Tech Stack:** egui/eframe 0.37.0, `walkdir` 2.5.0, `quick_cache` 0.7.0, `rfd` 0.17.2, `windows-sys` 0.61.2 (`Win32_System_SystemInformation`), `image` 0.25.10; dev: `tempfile` 3.27.0, `photosort-testkit`.

**Scope:** 8 phases from original design (this is phase 3 of 8)

**Codebase verified:** 2026-11-20

---

## Acceptance Criteria Coverage

This phase implements and tests:

### photosort-mvp.AC1: Source selection and scan
- **photosort-mvp.AC1.1 Success:** Picking a source folder finds every file with a registered image extension (MVP: `.jpg`, `.jpeg`, `.nef`, `.orf`; any letter case) in that folder and all its subfolders.
  - *`.orf` is deferred by user decision (Phase 2, tracking issue). Registered MVP extensions are `.jpg`, `.jpeg` and `.nef`.*
- **photosort-mvp.AC1.2 Success:** `DSC_0001.JPG` and `DSC_0001.NEF` in the same directory form one shot. The same stem in `100NZ_F` and in `101NZ_F` forms two separate shots.
- **photosort-mvp.AC1.3 Edge:** Files with unregistered extensions (MOV, XMP, `.dop`) are ignored. An unreadable or corrupt image file gets an error tile, and the scan continues.

### photosort-mvp.AC2: Thumbnail grid and keyboard culling
- **photosort-mvp.AC2.1 Success:** Each shot appears as exactly one tile. Tiles are in capture order (`DateTimeOriginal` + `SubSecTimeOriginal`), with filename as the tie-break.
- **photosort-mvp.AC2.2 Success:** Each tile shows the embedded preview, a format badge (`J`, `N`, `J+N`, `O`), and a checkbox.
- **photosort-mvp.AC2.3 Success:** RAW-only and JPEG-only shots both display previews.
- **photosort-mvp.AC2.4 Success:** Clicking the checkbox toggles the tick. Clicking the image opens the overlay.
  - *This phase: the checkbox toggle; an image click emits `GridAction::Open`. Phase 4 opens the overlay.*
- **photosort-mvp.AC2.5 Success:** Arrow keys move the focus ring, and the grid scrolls to keep the focused tile visible.
- **photosort-mvp.AC2.6 Success:** Space toggles the focused shot's tick. Shift+arrows extend a range, and Space then toggles every shot in it.
- **photosort-mvp.AC2.7 Success:** The tile-size slider and Ctrl+scroll resize tiles between 96 and 640 px. Columns reflow to the window width.
- **photosort-mvp.AC2.8 Edge:** A shot with no embedded preview shows a placeholder with its filename. The grid never decodes a full-resolution image.

### photosort-mvp.AC9: Responsiveness at 7,000 shots
- **photosort-mvp.AC9.1 Success:** On a generated 7,000-shot source, placeholders for every shot appear within 3 s of selecting it.
- **photosort-mvp.AC9.2 Success:** While the metadata and preview pass runs, scrolling and ticking stay responsive, with no UI frame over 100 ms in the frame-time log.
- **photosort-mvp.AC9.3 Success:** Cache memory stays within the RAM budget: 25% of physical RAM by default, or the configured override. Evicted tiles reload when scrolled back into view.
  - *The configured override is persisted in Phase 6 (`config.json` `ram_budget_mb`). This phase implements and tests the budget computation with an override parameter.*

### photosort-mvp.AC12: Body-agnostic extensibility
- **photosort-mvp.AC12.1 Success:** A test-only `FormatReader` for a fake TIFF-based extension can be registered. Its files are then scanned, grouped, identified, planned and copied, with no change to grouping, identity, planning or copy code.
  - *This phase: scanned and grouped. Planning and copy follow in Phase 5.*

**Design interpretations approved during planning:**
- Space on a range: if every shot in the range is ticked, untick them all; otherwise tick them all.
- Placeholders appear in path order. The grid re-sorts to capture order once pass 2 (`ParsePassDone`) finishes, keeping the focused tile in view. Shots without a date sort after dated ones.

---

## Conventions for this phase

- Shell files start with `// pattern: Imperative Shell`; `grid_math.rs`, `scan_queue.rs`, `budget.rs` and `grouping.rs` are `// pattern: Functional Core`.
- Shell modules are tested with `#[cfg(test)] mod tests` in the same file, because `photosort` is a binary crate. Use real temp directories (`tempfile::tempdir()`) and testkit-generated files. Do not mock the filesystem.
- Waiting on workers in tests is condition-based: receive events with `recv_timeout` in a loop until the expected condition holds, failing after 10 s. Never `sleep` for a fixed time.
- Test command: `cargo test --workspace`.

<!-- START_SUBCOMPONENT_A (tasks 1-2) -->
<!-- START_TASK_1 -->
### Task 1: Shot contracts, grouping and capture ordering (`photosort-core`)

**Verifies:** photosort-mvp.AC1.2, photosort-mvp.AC1.3 (unregistered extensions ignored), photosort-mvp.AC2.1, photosort-mvp.AC2.2 (badge text), photosort-mvp.AC2.8 (preview source never the full JPEG)

**Files:**
- Modify: `crates/photosort-core/src/model.rs`: add shot types
- Create: `crates/photosort-core/src/grouping.rs`
- Modify: `crates/photosort-core/src/lib.rs`: `pub mod grouping;`

**Implementation:**

Add to `model.rs`:

```rust
use std::path::PathBuf;
use std::time::SystemTime;

/// Stable within one scan; index into the scan's shot list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ShotId(pub u32);

/// A file found by the directory listing, before grouping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedFile {
    pub path: PathBuf,
    pub size: u64,
    pub modified: SystemTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShotFile {
    /// Absolute path on the source volume.
    pub path: PathBuf,
    pub kind: FileKind,
    pub size: u64,
    pub modified: SystemTime,
    /// `None` until pass 2 parses it.
    pub meta: Option<Metadata>,
    pub identity: Option<IdentityKey>,
    /// Set when pass 2 could not read or parse the file.
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Shot {
    pub id: ShotId,
    /// Source directory.
    pub dir: PathBuf,
    /// Filename stem, original case.
    pub stem: String,
    /// 1..=2 files, at most one per `FileKind`.
    pub files: Vec<ShotFile>,
}
```

`grouping.rs` (`// pattern: Functional Core`; `PathBuf` manipulation is pure data, with no filesystem access):

- `pub fn group_files(files: Vec<ListedFile>, registry: &Registry) -> Vec<Shot>`:
  1. Drop files whose extension `registry.format_for_extension` does not know (AC1.3). Sort the rest by path.
  2. Group by `(parent dir, stem.to_lowercase())` (AC1.2: the directory is part of the key).
  3. Within a group, the first file of each kind joins the shot. A second file of the **same** kind (e.g. `a.jpg` and `a.JPEG`) starts its own shot, so no file is lost.
  4. Assign `ShotId(0..)` in that sorted order.
- `impl Shot`:
  - `pub fn badge(&self, registry: &Registry) -> String`: each file's `reader.badge()`, non-raw first, then raw, joined with `+` (`"J"`, `"N"`, `"J+N"`).
  - `pub fn capture_time(&self) -> Option<(&str, &str)>`: DTO and subsec from the first file whose metadata has a DTO.
  - `pub fn tile_preview(&self, registry: &Registry) -> Option<(usize, ByteRange)>`: the `preview` range of the first non-raw file that has one, else of a raw file. **Never `full_jpeg`** (AC2.8).
  - `pub fn display_name(&self) -> String`: the first file's file name.
- `pub fn capture_order(shots: &[Shot]) -> Vec<ShotId>`: sorts by the key below.
  - Shots with a DTO come first, ordered by DTO string (EXIF format sorts lexically). Then subsec as a decimal fraction: right-pad digits to 9 so `"9"` > `"10"`. Then the lowercase display name.
  - Shots without a DTO follow, ordered by lowercase display name.

**Testing** (unit tests in `grouping.rs`, using `Registry::with_builtin()` and synthetic `ListedFile`s; no disk needed):
- AC1.2: `100NZ_F/DSC_0001.JPG` + `100NZ_F/DSC_0001.NEF` → one shot with 2 files. `101NZ_F/DSC_0001.JPG` → a separate shot. Mixed case `dsc_0001.jpg` + `DSC_0001.NEF` → one shot.
- AC1.3: `.MOV`, `.xmp` and `.dop` files produce no shot.
- AC2.1: shots with DTO/subsec `…:32`/`"92"`, `…:32`/`"9"` and `…:31`/`"99"` sort as 31.99, 32.9, 32.92. Equal times fall back to the name. Shots without metadata come last, in name order. Each shot appears exactly once.
- AC2.2: badges `"J"`, `"N"`, `"J+N"`.
- AC2.8: a NEF-only shot whose metadata has both `preview` and `full_jpeg` → `tile_preview` returns the preview range. A shot with neither → `None`.
- A same-kind duplicate (`a.jpg` and `a.JPEG`) → two shots.

**Verification:** `cargo test -p photosort-core grouping`

**Commit:** `feat(core): group files into shots and order by capture time`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Synthetic card generator (`photosort-testkit` example)

**Verifies:** None (test tooling for AC9 checks and manual testing).

**Files:**
- Create: `crates/photosort-testkit/examples/gen_card.rs`

**Implementation:** `cargo run -p photosort-testkit --example gen_card --release -- <out_dir> <shots> [--raw-every N]`
- Writes `DCIM/100NZ_F`, `101NZ_F`, … with 999 shots per folder, named `DSC_0001.JPG`… The counter restarts per folder, so stems repeat across folders.
- Each JPEG comes from `jpeg_file` with:
  - `ExifSpec::zf()` and an incrementing DTO (one shot per second, subsec cycling `00`-`99`);
  - `NikonSpec::zf(None)` with an incrementing `shutter_count`;
  - an MPF preview.
- Encode **one** 640×424 preview with `tiny_jpeg` and reuse its bytes, so 7,000 shots generate in seconds.
- Every Nth shot (default 10) also gets a NEF from `nef_file` with a PreviewIFD preview.
- It also writes one `.MOV` (random bytes), one `.xmp`, one truncated JPEG (the first 100 bytes of a valid file) and one zero-byte `.NEF`, to exercise AC1.3. It also writes `NOPV_0001.JPG`, a readable JPEG with valid EXIF but no MPF preview, for the AC2.8 placeholder.

**Verification:**
Run: `cargo run -p photosort-testkit --example gen_card --release -- %TEMP%\ps-card 7000`
Expected: about 7,700 image files under `DCIM\100NZ_F` … `DCIM\107NZ_F`, plus the 5 edge-case files, in well under a minute.

**Commit:** `test(testkit): add synthetic SD card generator`
<!-- END_TASK_2 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 3-5) -->
<!-- START_TASK_3 -->
### Task 3: Shell dependencies and RAM budget (`budget.rs`, `cache.rs`)

**Verifies:** photosort-mvp.AC9.3

**Files:**
- Modify: root `Cargo.toml` `[workspace.dependencies]`:

  ```toml
  walkdir = "=2.5.0"
  quick_cache = "=0.7.0"
  rfd = "=0.17.2"
  windows-sys = { version = "=0.61.2", features = ["Win32_Foundation", "Win32_System_SystemInformation", "Win32_Storage_FileSystem"] }
  ```

  (`rfd` 0.18.0 exists but was published under two weeks before planning; stay on 0.17.2.)
- Modify: `crates/photosort/Cargo.toml`: add `walkdir`, `quick_cache`, `rfd`, `windows-sys`, `image` and `thiserror` (workspace); `[dev-dependencies]`: `tempfile`, `proptest` and `photosort-testkit`.
- Create: `crates/photosort/src/budget.rs` (Functional Core)
- Create: `crates/photosort/src/cache.rs` (Imperative Shell)
- Modify: `crates/photosort/src/main.rs`: `mod budget; mod cache;`

**Implementation:**
- `budget.rs`:
  - `pub(crate) fn cache_budget_bytes(physical_ram: u64, override_mb: Option<u64>) -> u64`: the override in bytes if set, else `physical_ram / 4`.
  - `pub(crate) struct CacheSplit { pub bytes: u64, pub textures: u64 }` from `split(budget)`: 20% preview bytes; textures get the remaining 80%, capped at `TEXTURE_CAP = 2 GiB` because textures live in GPU memory, which is far smaller than system RAM on integrated GPUs. The split never exceeds the budget.
  - Estimated item counts for quick_cache's `estimated_items_capacity`: 64 KiB per preview, 640×424×4 per texture.
- `cache.rs`:
  - `#[cfg(windows)] pub(crate) fn physical_ram() -> Option<u64>`, using `windows_sys::Win32::System::SystemInformation::GlobalMemoryStatusEx` with `dwLength = size_of::<MEMORYSTATUSEX>() as u32` set before the call (📖 Microsoft Learn: "You must set this member before calling"). Return `ullTotalPhys`. The `unsafe` block gets a `// SAFETY:` comment: the struct is zero-initialised, `dwLength` is set, and the pointer is valid for the call.
  - `pub(crate) type PreviewBytes = quick_cache::sync::Cache<ShotId, Arc<[u8]>, ByteWeighter>;`
  - `pub(crate) type Textures = quick_cache::unsync::Cache<ShotId, egui::TextureHandle, TextureWeighter>;`
  - Weighters are `#[derive(Clone)]` (quick_cache requires `Weighter + Clone`) and implement `fn weight(&self, _: &K, v: &V) -> u64`. Bytes weigh `len`; textures weigh `w * h * 4` from `TextureHandle::size()`.
  - Constructors use `Cache::with_weighter(estimated_items, weight_capacity, weighter)`, taking the `CacheSplit`.

**Testing:**
- `budget.rs` unit tests: 24 GiB → 6 GiB. An override of 512 → 512 MiB, regardless of RAM. `split` never exceeds the budget; for 64 GiB RAM (16 GiB budget) → bytes 3.2 GiB and textures capped at 2 GiB; for a 1 GiB budget → 0.2 GiB and 0.8 GiB.
- `cache.rs` unit test (AC9.3): a `PreviewBytes` cache with a 1 MiB weight capacity; inserting 100 × 64 KiB entries leaves `weight() <= 1 MiB`, and early keys are evicted (`get` → `None`).
- `physical_ram()` returns `Some(n)` with `n > 0` on the test machine.

**Verification:** `cargo test -p photosort budget` and `cargo test -p photosort cache`

**Commit:** `feat(app): add RAM-budgeted preview and texture caches`
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: Visible-first job queue (`scan_queue.rs`)

**Verifies:** photosort-mvp.AC9.2 (scheduling logic: on-screen work first)

**Files:**
- Create: `crates/photosort/src/scan_queue.rs` (Functional Core)
- Modify: `crates/photosort/src/main.rs`: `mod scan_queue;`

**Implementation:**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Job {
    Parse { shot: ShotId, file: usize, path: PathBuf, kind: FileKind },
    Preview { shot: ShotId, path: PathBuf, range: ByteRange, orientation: Option<u16> },
}
```

`JobQueue` holds pending jobs grouped per shot (`BTreeMap<ShotId, VecDeque<Job>>`), plus the current `visible: Vec<ShotId>` in on-screen order.
- `push(job)`.
- `set_visible(Vec<ShotId>)`.
- `pop() -> Option<Job>`: first, jobs of visible shots in visible order, with `Preview` before `Parse` for the same shot; otherwise the lowest `ShotId`'s next job.
- `clear()`.
- `len()`.
- `contains_preview(shot) -> bool`, to avoid duplicate preview requests.

`pop` is O(visible + log n), which is fine for under 200 visible tiles.

**Testing** (unit tests):
- With no visible set, pop order is by `ShotId`.
- After `set_visible([7, 3])`, shot 7's jobs come out first, then 3's, then the rest.
- A preview job for a visible shot comes out before its parse job.
- `clear()` empties the queue.
- A property test: every pushed job is popped exactly once.

**Verification:** `cargo test -p photosort scan_queue`

**Commit:** `feat(app): add visible-first scan job queue`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Preview decoding (`preview.rs`)

**Verifies:** photosort-mvp.AC2.3, photosort-mvp.AC2.8 (decodes only embedded previews)

**Files:**
- Create: `crates/photosort/src/preview.rs` (Imperative Shell: it owns range reads from disk)
- Modify: `crates/photosort/src/main.rs`: `mod preview;`

**Implementation:**
- `pub(crate) struct DecodedImage { pub size: [usize; 2], pub rgba: Vec<u8> }`. Our own type, so scan code and tests do not need egui.
- `pub(crate) fn read_range(path: &Path, range: ByteRange) -> io::Result<Vec<u8>>`: `File::open` (read-only), `seek`, `read_exact`.
- `pub(crate) fn decode_preview(bytes: &[u8], orientation: Option<u16>) -> Result<DecodedImage, PreviewError>`:
  - `image::load_from_memory_with_format(bytes, ImageFormat::Jpeg)`;
  - if `orientation` maps through `image::metadata::Orientation::from_exif(o as u8)`, call `apply_orientation` (decoding does not apply orientation automatically);
  - then `to_rgba8()`.
  - `PreviewError` is a thiserror enum.
- `pub(crate) fn to_color_image(img: &DecodedImage) -> egui::ColorImage`: `ColorImage::from_rgba_unmultiplied(img.size, &img.rgba)`.

**Testing** (unit tests, using `photosort_testkit::tiny_jpeg`):
- AC2.3: a 640×424 JPEG decodes to `[640, 424]`; with orientation 6 → `[424, 640]`.
- Corrupt bytes → `Err`, no panic.
- `read_range` on a temp file returns exactly the requested bytes; a range past the end → `Err`.

**Verification:** `cargo test -p photosort preview`

**Commit:** `feat(app): decode embedded previews with EXIF orientation`
<!-- END_TASK_5 -->
<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 6-7) -->
<!-- START_TASK_6 -->
### Task 6: Two-pass scanner (`scan.rs`)

**Verifies:** photosort-mvp.AC1.1, photosort-mvp.AC1.3, photosort-mvp.AC9.1, photosort-mvp.AC12.1 (scan + group portion)

**Files:**
- Create: `crates/photosort/src/scan.rs` (Imperative Shell)
- Modify: `crates/photosort/src/main.rs`: `mod scan;`

**Implementation:**

```rust
pub(crate) type Waker = Arc<dyn Fn() + Send + Sync>;

#[derive(Debug)]
pub(crate) enum ScanEvent {
    Listed { generation: u64, shots: Vec<Shot>, skipped: Vec<String> },
    ListFailed { generation: u64, error: String },
    FileParsed { generation: u64, shot: ShotId, file: usize, outcome: FileOutcome },
    Preview { generation: u64, shot: ShotId, result: Result<DecodedImage, String> },
    ParsePassDone { generation: u64 },
}

#[derive(Debug)]
pub(crate) struct FileOutcome {
    pub meta: Result<Metadata, String>,
    pub identity: Option<IdentityKey>,
}
```

Free functions (directly testable):
- `pub(crate) fn list_source(root: &Path, registry: &Registry) -> Result<(Vec<Shot>, Vec<String>), ScanError>`:
  - `WalkDir::new(root).follow_links(false)`;
  - for each file entry, keep it if `registry.format_for_extension(ext)` is `Some`, taking size and modified time from `entry.metadata()`;
  - collect per-entry errors as `skipped` messages and keep walking;
  - then `grouping::group_files`.
  - A missing or unreadable `root` → `ScanError` (thiserror).
- `pub(crate) fn parse_file(path: &Path, kind: FileKind, registry: &Registry) -> FileOutcome`:
  - `File::open` (read-only);
  - read `max(reader.header_len_hint(), CONTENT_HEAD_LEN)` bytes, or the whole file if shorter;
  - call `registry.parse`;
  - on `NeedMore { needed }`, read up to `min(round_up(needed, 64 KiB), MAX_HEAD_LEN, file_len)` and retry, at most 6 attempts;
  - then `identity_for(meta.as_ref().ok(), file_len, &head)`.
  - An I/O error → `meta: Err(msg)`, with identity computed from whatever head was read, or `None` if nothing was read.

`Scanner` (owns threads):
- `Scanner::start(root: PathBuf, registry: Arc<Registry>, bytes: Arc<PreviewBytes>, waker: Waker, generation: u64) -> (Scanner, Receiver<ScanEvent>)`.
- Uses `std::sync::mpsc::sync_channel(8192)`. It's bounded, so a slow UI applies backpressure.
- A lister thread runs `list_source`, sends `Listed`, then pushes one `Parse` job per file, ordered by shot.
- `workers = available_parallelism().min(MAX_WORKERS)` threads, with `const MAX_WORKERS: usize = 4`. SD cards gain little from more parallel readers. The threads share `Arc<(Mutex<JobQueue>, Condvar)>`.
- A `Parse` worker calls `parse_file` and sends `FileParsed`.
- A `Preview` worker uses cached bytes if present, else `read_range` and inserts them into `bytes`. It then calls `decode_preview` and sends `Preview`.
- After the last `Parse` job completes, `ParsePassDone` is sent. Track outstanding parse jobs with a counter.
- Every send is followed by `waker()`.
- `set_visible(&self, Vec<ShotId>)` and `request_preview(&self, shot, path, range, orientation)`, which skips the request if `contains_preview` is true.
- `Drop` sets a stop flag and calls `notify_all`, but does **not** join: joining on the UI thread could block on an in-flight SD-card read or a full channel. Workers check the flag before each job and before each send, and treat a send error (receiver dropped) as a stop. The lister checks it between directory entries and before pushing jobs. Threads exit on their own.
- The generation number lets the app ignore events from a previous scan after Rescan or a new source.

**Testing** (unit tests in `scan.rs`, writing a card into `tempfile::tempdir()` with testkit builders):
- AC1.1: `A.JPG`, `b.jpg`, `C.jpeg`, `D.NeF` across nested folders → four shots. Extensions in any case are found, recursively.
- AC1.3:
  - `.MOV`, `.XMP` and `.dop` files → ignored.
  - A truncated JPEG and a zero-byte NEF → `Listed` includes them. Their `FileParsed` outcomes have `meta: Err(..)`, every other file still parses, and `ParsePassDone` arrives.
- AC9.1: a temp card of 7,000 testkit JPEGs (EXIF only, no MPF preview, to keep generation fast). `list_source` completes in under 3 s. Measure only the `list_source` call with `Instant`.
- AC12.1 (scan + group): register an in-test `FakeTiffReader` (extension `fkr`, badge `F`, parse via `exif::read_tiff_exif`) into a registry. A temp `IMG_0001.fkr` (from `testkit::tiff_file`) next to `IMG_0001.JPG` → one shot with two files. Its `FileParsed` outcome has a `Counter` identity. No grouping or scan code changes.
- `Scanner` end-to-end: start on a small card and collect events with `recv_timeout` until `ParsePassDone`. Every file has exactly one `FileParsed`. After `request_preview` for one shot, a `Preview` event arrives with the expected size.

**Verification:** `cargo test -p photosort scan`

**Commit:** `feat(app): add two-pass scanner with visible-first worker pool`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: Pure grid logic (`ui/grid_math.rs`)

**Verifies:** photosort-mvp.AC2.5, photosort-mvp.AC2.6, photosort-mvp.AC2.7, photosort-mvp.AC9.3 (evicted tiles are re-requested)

**Files:**
- Create: `crates/photosort/src/ui/mod.rs` (`pub(crate) mod grid_math;`, more in Task 8)
- Create: `crates/photosort/src/ui/grid_math.rs` (Functional Core)
- Modify: `crates/photosort/src/main.rs`: `mod ui;`

**Implementation** (all functions pure; positions are indices into the display order `order: &[ShotId]`):

```rust
pub(crate) const TILE_MIN: f32 = 96.0;
pub(crate) const TILE_MAX: f32 = 640.0;

pub(crate) struct GridLayout { pub columns: usize, pub tile: f32, pub row_height: f32 }
pub(crate) fn layout(available_width: f32, tile: f32, spacing: f32, caption: f32) -> GridLayout; // columns >= 1
pub(crate) fn clamp_tile(size: f32) -> f32;
pub(crate) fn zoom_tile(size: f32, zoom_delta: f32) -> f32; // clamp_tile(size * zoom_delta)

#[derive(Clone, Copy)] pub(crate) enum Move { Left, Right, Up, Down }
pub(crate) fn move_focus(focus: usize, mv: Move, columns: usize, len: usize) -> usize; // clamps at edges; returns 0 when len == 0

/// Focus plus an optional range anchor (Shift+arrows).
#[derive(Clone, Copy, Default)] pub(crate) struct Selection { pub focus: usize, pub anchor: Option<usize> }
pub(crate) fn apply_move(sel: Selection, mv: Move, extend: bool, columns: usize, len: usize) -> Selection; // Selection::default() when len == 0
// No helper here may compute `len - 1` without first handling len == 0: the grid is empty before a scan, on a source with no images, and when Hide copied hides every shot.
pub(crate) fn selected_range(sel: Selection) -> RangeInclusive<usize>;

/// If every target is ticked, untick them all; otherwise tick them all.
pub(crate) fn toggle_ticks(ticks: &mut HashSet<ShotId>, targets: &[ShotId]);

/// New scroll offset that reveals `row`, or `None` if it is already fully visible.
pub(crate) fn reveal_row(row: usize, row_height: f32, offset: f32, viewport: f32) -> Option<f32>;

/// Visible shots that need a preview request: no texture, not in flight, and a preview exists.
pub(crate) fn previews_to_request(visible: &[ShotId], has_texture: impl Fn(ShotId) -> bool,
    in_flight: &HashSet<ShotId>, has_preview: impl Fn(ShotId) -> bool) -> Vec<ShotId>;
```

**Testing** (unit tests; proptest where noted):
- AC2.7:
  - `layout` gives `floor((w + spacing) / (tile + spacing))` columns, at least 1, and the count reflows as the width changes;
  - `clamp_tile` and `zoom_tile` stay within 96..=640 (proptest over any size and delta);
  - `zoom_tile(200, 1.1) > 200`.
- AC2.5:
  - `move_focus` Right from the last index stays put, Down from the last row stays in range, Up from row 0 stays;
  - empty grid: `move_focus` with `len == 0` returns 0 for every `Move`, and `apply_move` with `len == 0` returns `Selection::default()` (no panic or overflow);
  - `reveal_row` returns `None` for a visible row, a smaller offset for a row above, and `(row+1)*h - viewport` for a row below.
- AC2.6:
  - Shift+Right×3 from 5 → range 5..=8, and a plain move clears the anchor;
  - `toggle_ticks` on a mixed range ticks all, and on an all-ticked range unticks all;
  - Space on a single focus toggles that shot.
- AC9.3: a shot whose texture was evicted (`has_texture` false), is visible, is not in flight and has a preview → included. In flight → excluded. No preview → excluded (placeholder).

**Verification:** `cargo test -p photosort grid_math`

**Commit:** `feat(app): add pure grid layout, focus and selection logic`
<!-- END_TASK_7 -->
<!-- END_SUBCOMPONENT_C -->

<!-- START_SUBCOMPONENT_D (tasks 8-9) -->
<!-- START_TASK_8 -->
### Task 8: App state, top bar and virtualized grid

**Verifies:** photosort-mvp.AC2.2, photosort-mvp.AC2.3, photosort-mvp.AC2.4 (checkbox part), photosort-mvp.AC2.8 (manually verified in Task 10)

**Files:**
- Modify: `crates/photosort/src/app.rs`: replace the Phase 1 unit struct with stateful `PhotosortApp`
- Modify: `crates/photosort/src/main.rs`: build the app with `PhotosortApp::new(cc, frame_log)`
- Create: `crates/photosort/src/ui/top_bar.rs`, `crates/photosort/src/ui/grid.rs`
- Modify: `crates/photosort/src/ui/mod.rs`

**Implementation:**

`PhotosortApp` fields:
- `registry: Arc<Registry>`;
- `generation: u64`;
- `source: Option<PathBuf>`;
- `scanner: Option<Scanner>`, `events: Option<Receiver<ScanEvent>>`;
- `shots: Vec<Shot>` (indexed by `ShotId.0`), `order: Vec<ShotId>`;
- `ticks: HashSet<ShotId>` (in-memory; Phase 6 persists by identity);
- `selection: Selection`;
- `tile_size: f32` (default 200);
- `bytes: Arc<PreviewBytes>`, `textures: Textures`, `in_flight: HashSet<ShotId>`;
- `pending_images: VecDeque<(ShotId, DecodedImage)>`;
- `folder_pick: Option<Receiver<Option<PathBuf>>>`;
- `status: StatusCounts`;
- `scroll_to: Option<f32>`, `last_viewport: (f32, f32)`;
- `frame_log: Option<Sender<String>>` (to the frame-log writer thread).

The waker passed to `Scanner::start` is `let ctx = cc.egui_ctx.clone(); Arc::new(move || ctx.request_repaint())`. The budget is `cache_budget_bytes(physical_ram().unwrap_or(8 GiB), None)` (the override arrives with config in Phase 6).

`eframe::App::logic(&mut self, ctx, frame)` (runs before `ui` each frame):
1. If the folder-pick receiver has a result, start a scan:
   - bump `generation`;
   - drop the old event receiver first, then the old scanner (non-blocking; its threads exit on their own);
   - replace `bytes` with a fresh `PreviewBytes` cache and pass it to the new scanner (the old cache is keyed by the previous scan's `ShotId`s, so reusing it would show wrong previews after Rescan);
   - clear the shots, `order`, ticks, textures, `pending_images` and in-flight set, and set `selection = Selection::default()` and `scroll_to = Some(0.0)`, so nothing refers to the old scan's `ShotId`s.

   Throughout the app, resolve a `ShotId` with `shots.get(id.0 as usize)` and skip `None`; never index `shots` directly.
2. Drain up to 5,000 events, ignoring those whose `generation` is not current.
   - `Listed` → `shots`, and `order = (0..n)` (path order).
   - `FileParsed` → fill `meta`, `identity` and `error`.
   - `Preview` → push to `pending_images`, or mark an error.
   - `ParsePassDone` → `order = capture_order(&shots)`, keep `selection.focus` on the same `ShotId`, and set `scroll_to` via `reveal_row`.
3. Upload at most 8 `pending_images` as textures (`ctx.load_texture(format!("shot-{}", id.0), to_color_image(..), egui::TextureOptions::LINEAR)`), insert them into `textures`, and remove them from `in_flight`. This bounds per-frame upload cost.
4. Keys, via `ctx.input_mut(|i| …)`, **only when `!ctx.egui_wants_keyboard_input()` and no window or modal of ours is open**. This keyboard gate applies to every app shortcut added in this and later phases, so typing a space, `L`, `[`, `]` or Enter into a text field never triggers a grid action.
   - For each arrow key, first `i.consume_key(Modifiers::SHIFT, key)` → `apply_move(.., extend: true, ..)`, then `i.consume_key(Modifiers::NONE, key)` → `apply_move(.., extend: false, ..)`. The order matters: egui matches modifiers with `matches_logically`, which ignores an extra Shift, so checking `NONE` first would swallow Shift+arrow (egui 0.37 `input_state/mod.rs`, `consume_shortcut` docs). After either, set `scroll_to` from `reveal_row`;
   - `consume_key(Modifiers::NONE, Key::Space)` → `toggle_ticks` on the `ShotId`s at the selected positions (`selected_range(selection).filter_map(|p| order.get(p).copied())`, so an empty grid is a no-op);
   - `consume_key(Modifiers::NONE, Key::Enter)` → `order.get(selection.focus).copied().map(GridAction::Open)`, where `enum GridAction { Open(ShotId) }`. No action when the grid is empty; never index `order` directly.

   Consuming the keys means no focused widget also acts on them.
5. In `new`, call `cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false)` once.

`ui(&mut self, ui, frame)`:
- `top_bar::show(ui, &mut state)` inside `egui::Panel::top("top_bar").show(ui, …)`:
  - a **Source…** button, which spawns a thread running `rfd::FileDialog::new().pick_folder()` and returns the result over a channel (the sync dialog would block the UI thread);
  - the source path label;
  - a **Rescan** button (restarts a scan on the same source; disabled without one);
  - a tile-size `egui::Slider` (96..=640);
  - status text such as `7,000 shots · parsed 3,210/7,700 · 2 errors`.
- `grid::show(ui, &mut state) -> Option<GridAction>` inside `egui::CentralPanel::default().show(ui, …)`:
  - `layout(...)`, then `rows = ceil(order.len() / columns)`;
  - `egui::ScrollArea::vertical().auto_shrink([false, false])`, with `.vertical_scroll_offset(y)` when `scroll_to.take()` is `Some`, then `.show_rows(ui, row_height, rows, |ui, row_range| …)`;
  - after `show_rows`, store `out.state.clamped_offset().y` and `out.inner_rect.height()` for `reveal_row` next frame.
- For each tile, `let (rect, _) = ui.allocate_exact_size(vec2(tile, tile + caption), Sense::hover())`, then `let tile_resp = ui.interact(rect, egui::Id::unique(("tile", shot.id.0)), Sense::click())`. Interact the checkbox **after** the tile, so it sits on top and receives its own clicks:
  - If there's a texture, `ui.painter().image(tex.id(), fit_rect, Rect::from_min_max(pos2(0,0), pos2(1,1)), Color32::WHITE)`, letterboxed to keep its aspect.
  - Otherwise a placeholder rect with the display name, plus "loading…" when a preview is in flight, or "⚠ unreadable" with the error when every file failed. A shot with no preview at all just shows its name (AC2.8).
  - The badge text (`shot.badge(&registry)`) top-left.
  - The checkbox: a 20×20 rect top-right with its own `ui.interact(rect, id, Sense::click())`; a click → toggle that shot. Draw ✓ when ticked.
  - The focus ring: `painter().rect_stroke(rect, 4.0, Stroke::new(2.0, color), StrokeKind::Inside)` when the position equals `selection.focus`. The range is drawn with a fainter fill.
  - `tile_resp.clicked()` (and not the checkbox) → focus that position and return `GridAction::Open(shot.id)`.
  - Ids are derived from the shot, so they are stable and distinct per tile: `egui::Id::unique(("tile", shot.id.0))` for the tile (above) and `egui::Id::unique(("tile_tick", shot.id.0))` for its checkbox. `Id::new` is deprecated in 0.37; `Id::unique` is a deterministic hash, so the same source gives the same id every frame.
- Ctrl+scroll: if the grid rect contains the pointer and `ui.input(|i| i.zoom_delta()) != 1.0`, set `tile_size = zoom_tile(tile_size, zoom_delta)`.
- After drawing, compute the visible `ShotId`s from the rendered row range. Then `scanner.set_visible(visible.clone())`, and for each id in `previews_to_request(...)` call `scanner.request_preview(...)` with the path and range from `shot.tile_preview(&registry)`, adding it to `in_flight`.
- `GridAction::Open` is ignored in this phase (Phase 4 opens the overlay).

Frame log:
- `main.rs` reads `PHOTOSORT_FRAME_LOG` (application entry point, so env access is allowed) and passes `Option<PathBuf>`.
- If set, a small writer thread owns the `BufWriter<File>`, so the UI thread does no file I/O. Each frame `logic` sends one line over the channel: `frame.info().cpu_usage` converted to ms with 1 decimal (first field), then the event and upload counts. The writer flushes every 120 lines and when the channel closes.

**Testing:** UI drawing is verified manually (Task 10); its decisions are covered by `grid_math` (Task 7), `grouping` (Task 1) and `scan` (Task 6).

**Verification:** `cargo build -p photosort` with no warnings; `cargo clippy --workspace --all-targets -- -D warnings`.

**Commit:** `feat(app): add source picker, status bar and virtualized thumbnail grid`
<!-- END_TASK_8 -->

<!-- START_TASK_9 -->
### Task 9: Phase-wide automated verification

**Verifies:** None (gate).

Run, and expect success for each: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked` (oracle tests stay `ignored`).

**Commit:** only if fixes were needed: `chore: fix lints`.
<!-- END_TASK_9 -->
<!-- END_SUBCOMPONENT_D -->

<!-- START_TASK_10 -->
### Task 10: Manual responsiveness and keyboard checks

**Verifies:** photosort-mvp.AC2.1–AC2.8, photosort-mvp.AC9.1, photosort-mvp.AC9.2 (manual; append results to `docs/implementation-plans/2026-11-20-photosort-mvp/manual-test-log.md`)

Steps (a human, or the executor when it can drive the app; record the results under `## Phase 3` in `docs/implementation-plans/2026-11-20-photosort-mvp/manual-test-log.md`):
1. `cargo run -p photosort-testkit --example gen_card --release -- $env:TEMP\ps-card 7000`
2. `$env:PHOTOSORT_FRAME_LOG="$env:TEMP\ps-frames.log"; cargo run -p photosort --release`
3. Pick `%TEMP%\ps-card`:
   - **AC9.1:** placeholder tiles for all shots appear within 3 s (status shows the shot count).
   - The four edge-case files: `.MOV` and `.xmp` are absent, while the truncated JPEG and the zero-byte NEF show "⚠ unreadable" tiles.
4. While the parse pass runs, scroll quickly and tick tiles with Space.
   - **AC9.2:** afterwards, `Get-Content $env:TEMP\ps-frames.log | ForEach-Object { [double]($_ -split ' ')[0] } | Measure-Object -Maximum` reports a maximum below 100 ms.
5. Keyboard (AC2.5-AC2.7):
   - arrows move the ring, and the grid scrolls to keep it visible;
   - Shift+arrows extend the range, and Space ticks or unticks it;
   - the slider and Ctrl+scroll resize tiles from 96 to 640, and columns reflow when the window is resized.
   - Confirm Ctrl+scroll does **not** also zoom the whole UI, and that clicking a checkbox and then pressing Space does not double-toggle.
6. AC2.2, AC2.3 and AC2.8: JPEG-only tiles show `J` with a preview; RAW+JPEG tiles show `J+N`. Delete one JPEG partner to make a NEF-only shot, Rescan, and confirm the `N` tile shows its preview. `NOPV_0001.JPG` (readable, with no embedded preview) shows a placeholder with its filename.
7. After the pass completes, tiles are in capture order (AC2.1).
<!-- END_TASK_10 -->
