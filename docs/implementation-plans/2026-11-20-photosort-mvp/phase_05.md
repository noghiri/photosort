# photosort MVP Implementation Plan — Phase 5: Output folder, already-copied detection, and copy

**Goal:** Copy checked shots safely into the chosen output folder, never twice and never overwriting.

**Architecture:**
- **Pure core:**
  - `photosort-core/src/output.rs`: `OutputIndex`, `CopyStatus` and partner lookup;
  - `photosort-core/src/plan.rs`: skips, naming, suffixes, partner-stem reuse and in-batch collisions.
- **Shell:**
  - `output_index.rs` builds the index on a worker thread from the output folder's file headers (top level only);
  - `fsops.rs` wraps the Win32 no-replace rename and the free-space query;
  - `copy.rs` executes a plan: pre-flight, then temp copy with BLAKE3, verify, mtime and no-replace rename, with late-collision re-planning and failure scoping;
  - `batch_log.rs` appends to `%APPDATA%\photosort\logs\`.
- **UI:**
  - the top bar gains output selection, Hide copied and Copy;
  - tiles gain copied/partial badges;
  - `ui/dialogs.rs` adds the confirmation, progress, pre-flight error and summary dialogs.

**Tech Stack:** `windows-sys` 0.61.2 (`Win32_Storage_FileSystem`), `blake3` 1.8.7, `dirs` 7.0.0, `proptest` 1.12.0, `tempfile` 3.27.0.

**Scope:** 8 phases from original design (this is phase 5 of 8)

**Codebase verified:** 2026-11-21

---

## Acceptance Criteria Coverage

This phase implements and tests:

### photosort-mvp.AC5: Output selection and copy
- **photosort-mvp.AC5.1 Success:** The output is either a remembered base folder plus a subfolder (existing, or new and created at copy time), or an arbitrary folder chosen with Browse.
  - *"Remembered" across restarts is Phase 6; this phase keeps base folders in memory.*
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

### photosort-mvp.AC3 (completion)
- **photosort-mvp.AC3.3 Success:** ←/→ move to the previous or next shot, skipping shots the hide-copied filter is hiding. Space toggles the tick. Esc returns to the grid with focus on the shot that was showing.
  - *This phase supplies the real `is_hidden`.*

### photosort-mvp.AC12.1 (completion)
- **photosort-mvp.AC12.1 Success:** A test-only `FormatReader` for a fake TIFF-based extension can be registered. Its files are then scanned, grouped, identified, planned and copied, with no change to grouping, identity, planning or copy code.

**Decisions approved during planning:**
- Copy is enabled only after pass 2 finishes and the output index is current.
- After a batch, ticks are cleared for shots that copied fully or were skipped as already copied. Failed and not-attempted shots keep their tick.
- The batch log uses UTC.
- `PlannedFile` has no `lens_edit` field yet; Phase 7 adds it when `LensEdit` exists.

---

## Conventions for this phase

- `output.rs`, `plan.rs`, `copy_summary.rs`, `logfmt.rs` and `ui/output_select.rs` are Functional Core. `output_index.rs`, `fsops.rs`, `copy.rs`, `batch_log.rs` and `paths.rs` are Imperative Shell.
- Every `unsafe` block in `fsops.rs` has a `// SAFETY:` comment.
- Never use `std::fs::rename` for placing copies: on Windows it replaces existing files (std calls `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`).
- Shell tests use real temp directories. The only seams are `CopyEnv::free_space` and `CopyEnv::verify`, for conditions a test cannot create on a real disk (a full volume, a bad read-back).
- Empirically verified during planning on this machine: `MoveFileExW(from, to, MOVEFILE_WRITE_THROUGH)` without `MOVEFILE_REPLACE_EXISTING` fails with OS error 183 (`ErrorKind::AlreadyExists`) when `to` exists, including a case-only clash, and leaves the existing file intact. Microsoft Learn does not document the code or claim atomicity; the guarantee relied on is no-replace.

<!-- START_SUBCOMPONENT_A (tasks 1-3) -->
<!-- START_TASK_1 -->
### Task 1: Output index and copy status (`photosort-core/src/output.rs`)

**Verifies:** photosort-mvp.AC7.1, photosort-mvp.AC7.2 (status logic)

**Files:**
- Create: `crates/photosort-core/src/output.rs`
- Modify: `crates/photosort-core/src/lib.rs`: `pub mod output;`

**Implementation:**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyStatus { NotCopied, Partial, Copied }

#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutputIndex {
    /// Identity → (kind, existing file name) for every parsed image in the folder.
    pub by_identity: HashMap<IdentityKey, Vec<(FileKind, String)>>,
    /// Every name in the folder (any file type), lowercased.
    pub names_lower: HashSet<String>,
}

impl OutputIndex {
    pub fn insert_name(&mut self, name: &str);
    pub fn insert_image(&mut self, identity: IdentityKey, kind: FileKind, name: &str);
    /// The existing output file matching this shot file, if any.
    pub fn existing_for(&self, file: &ShotFile) -> Option<&str>;
}

/// Indices of the shot's files not present in the output.
/// A file without an identity counts as missing.
pub fn missing_files(shot: &Shot, index: &OutputIndex) -> Vec<usize>;
pub fn copy_status(shot: &Shot, index: &OutputIndex) -> CopyStatus;
/// Stem of an existing output file belonging to this shot (for partner reuse).
pub fn partner_stem(shot: &Shot, index: &OutputIndex) -> Option<String>;
```

A file is "present" when `by_identity[file.identity]` contains an entry with the same `kind`. The JPEG and NEF of one capture share an identity key, so the kind must match too.

**Testing** (unit tests):
- AC7.1: a J+N shot whose identities are both indexed, under unrelated names (`Holiday.JPG`, `Holiday.NEF`) → `Copied`.
- AC7.2: only the JPEG indexed → `Partial`, `missing_files` returns the NEF's index, and `partner_stem == Some("Holiday")`.
- Nothing indexed → `NotCopied`.
- A file with `identity: None` counts as missing.
- The same identity with a different kind is not a match.

**Verification:** `cargo test -p photosort-core output`

**Commit:** `feat(core): add output index and copied/partial status`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Copy planning (`photosort-core/src/plan.rs`)

**Verifies:** photosort-mvp.AC6.2, photosort-mvp.AC6.3, photosort-mvp.AC6.4, photosort-mvp.AC6.5, photosort-mvp.AC7.1 (skip), photosort-mvp.AC7.2 (only missing kinds), photosort-mvp.AC12.1 (plan portion)

**Files:**
- Create: `crates/photosort-core/src/plan.rs`
- Modify: `crates/photosort-core/src/lib.rs`: `pub mod plan;`

**Implementation:**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile { pub src: PathBuf, pub kind: FileKind, pub size: u64, pub dest_name: String }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedShot {
    pub shot: ShotId,
    /// Only kinds missing from the output.
    pub files: Vec<PlannedFile>,
    /// The destination stem differs from the source stem.
    pub renamed: bool,
    /// A partial shot whose partner stem was taken; it got a fresh suffix.
    pub unpaired: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CopyPlan {
    pub shots: Vec<PlannedShot>,
    /// Ticked shots already fully present in the output.
    pub skipped: Vec<ShotId>,
    pub total_bytes: u64,
}

impl CopyPlan {
    pub fn file_count(&self) -> usize;
    pub fn rename_count(&self) -> usize; // shots with renamed || unpaired
}

/// `ticked` in display order.
pub fn plan_copy(ticked: &[&Shot], index: &OutputIndex) -> CopyPlan;

/// First stem from `base`, `base_2`, `base_3`, … for which `{stem}.{ext}`
/// is free (case-insensitive) for every ext. Returns the stem and whether a suffix was applied.
pub fn next_free_stem(base: &str, exts: &[&str], taken_lower: &HashSet<String>) -> (String, bool);
```

`plan_copy`:
1. `taken = index.names_lower.clone()`.
2. For each ticked shot:
   - `missing = missing_files(shot, index)`. If it's empty → push to `skipped` (AC7.1).
   - Extensions are taken from each source file name with original case (`.JPG` stays `.JPG`).
   - **Partial** (some present):
     - if `partner_stem` gives `p`, and `{p}.{ext}` is free for every missing file → use `p`, with `renamed = p != shot.stem`;
     - otherwise → `next_free_stem(&shot.stem, ..)`, with `unpaired = true` (AC6.5).
   - **Not copied** → `next_free_stem(&shot.stem, all exts, &taken)`, with `renamed` = suffix applied (AC6.2, AC6.3).
   - Insert each chosen `dest_name.to_lowercase()` into `taken`, so later shots in the same batch see them (AC6.3).
   - Add the missing files' sizes to `total_bytes`.
3. All comparisons go through `to_lowercase()` (AC6.4).

**Testing** (unit tests + proptest):
- AC6.2: an output with `DSC_0001.JPG` from a different identity; a ticked J+N `DSC_0001` → `DSC_0001_2.JPG` + `DSC_0001_2.NEF`, `renamed`.
- AC6.3: two ticked shots `100NZ_F/DSC_0001` and `101NZ_F/DSC_0001`, with an empty output → `DSC_0001.*` and `DSC_0001_2.*`.
- AC6.4: an existing `dsc_0001.jpg` clashes with a planned `DSC_0001.JPG`.
- AC6.5:
  - a partial shot with an existing `DSC_0001_3.JPG` (same identity) → the NEF is planned as `DSC_0001_3.NEF`, not unpaired;
  - with `DSC_0001_3.NEF` also taken by another photo → a fresh suffix and `unpaired = true`.
- AC7.1/AC7.2: a fully present shot → in `skipped` and absent from `shots`; a partial shot → only the missing kind planned.
- AC12.1 (plan): a shot with a `FileKind("fake")` file plans like any other.
- AC5.4 figures: a plan with 2 not-copied J+N shots (one renamed), 1 partial shot (one missing file) and 1 skipped shot → `file_count() == 5`, `rename_count() == 1`, and `total_bytes` equals the sum of those 5 files' sizes.
- Property tests (Phase 5 "Done when"). Generate:
  - random batches: 1-40 shots drawn from a small stem pool, so collisions are frequent;
  - random dirs and random kinds;
  - a random set of existing names, plus random index entries.

  Assert, for every plan:
  1. No destination name equals an existing name, case-insensitively.
  2. Within each `PlannedShot`, every `dest_name` has the same stem.
  3. All destination names in the plan are unique, case-insensitively.
  4. Every ticked shot appears exactly once, in `shots` or `skipped`.

**Verification:** `cargo test -p photosort-core plan`

**Commit:** `feat(core): plan copies with suffixes, partner reuse and skip logic`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: Output selection model (`ui/output_select.rs`)

**Verifies:** photosort-mvp.AC5.1 (path resolution)

**Files:**
- Create: `crates/photosort/src/ui/output_select.rs` (Functional Core)
- Modify: `crates/photosort/src/ui/mod.rs`

**Implementation:**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OutputSelection {
    Base { base: PathBuf, subfolder: String },
    Arbitrary(PathBuf),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum OutputPathError {
    #[error("subfolder must be a relative path inside the base folder")]
    EscapesBase,
}

/// Base + trimmed subfolder (empty → base itself), or the arbitrary folder.
/// Rejects absolute subfolders, drive prefixes and `..` components.
pub(crate) fn resolve_output(sel: &OutputSelection) -> Result<PathBuf, OutputPathError>;
```

**Testing:**
- `Base { C:\Photos, "2026-11 trip" }` → `C:\Photos\2026-11 trip`.
- An empty or whitespace subfolder → the base.
- `"a/b"` → nested.
- `"..\\x"`, `"C:\\x"` and `"\\\\server\\x"` → `EscapesBase`.
- `Arbitrary(p)` → `p`.

**Verification:** `cargo test -p photosort output_select`

**Commit:** `feat(app): resolve output folder from base + subfolder or browse`
<!-- END_TASK_3 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 4-7) -->
<!-- START_TASK_4 -->
### Task 4: Output indexer (`output_index.rs`)

**Verifies:** photosort-mvp.AC7.1 (detection by identity, including hand-copied and renamed files), photosort-mvp.AC7.2

**Files:**
- Create: `crates/photosort/src/output_index.rs` (Imperative Shell)
- Create: `crates/photosort/src/test_support.rs` (compiled only under `#[cfg(test)]`): move `FakeTiffReader` here from the `scan.rs` tests, plus helpers for writing testkit J/N pairs into a temp dir
- Modify: `crates/photosort/src/main.rs`: `mod output_index;` and `#[cfg(test)] mod test_support;`; update the `scan.rs` tests to use `test_support::FakeTiffReader`

**Implementation:**
- `pub(crate) fn build_index(dir: &Path, registry: &Registry) -> io::Result<OutputIndex>`:
  - a missing folder → an empty index (it will be created at copy time);
  - `read_dir` (top level only; the design indexes only the current output folder, which avoids Dropbox Smart Sync downloads);
  - every entry's name goes through `insert_name`;
  - skip image parsing for names containing `.photosort-tmp`: an interrupted copy's leftovers must never count as copied (they still go into `names_lower`);
  - for other files with a registered extension, call `scan::parse_file` and, if it gives an identity, `insert_image`.
- `pub(crate) fn spawn_index(dir, registry, waker, generation) -> Receiver<(u64, PathBuf, io::Result<OutputIndex>)>` runs `build_index` on a thread.

**Testing** (temp dirs + testkit):
- AC7.1:
  - copy a testkit J+N pair into the output under different names (`renamed.jpg`, `x.NEF`) → `copy_status` of the source shot (identities from `parse_file`) is `Copied`;
  - a hand-copied JPEG with its original name → detected.
- AC7.2: only the JPEG present → `Partial`.
- `names_lower` includes non-image files (`notes.txt`).
- A leftover `.DSC_0001.photosort-tmp.JPG` → its name is in `names_lower`, but it does not make the shot `Copied`.
- A missing dir → an empty index.

**Verification:** `cargo test -p photosort output_index`

**Commit:** `feat(app): index output folder by identity`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Win32 file operations (`fsops.rs`)

**Verifies:** photosort-mvp.AC6.1 (no-replace primitive)

**Files:**
- Modify: root `Cargo.toml` `[workspace.dependencies]`: `dirs = "=7.0.0"`
- Modify: `crates/photosort/Cargo.toml`: add `blake3` and `dirs` (workspace)
- Create: `crates/photosort/src/fsops.rs` (Imperative Shell)
- Modify: `crates/photosort/src/main.rs`: `mod fsops;`

**Implementation:**

```rust
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, MoveFileExW, MOVEFILE_WRITE_THROUGH};

fn wide(p: &Path) -> Vec<u16> { p.as_os_str().encode_wide().chain(std::iter::once(0)).collect() }

/// Renames `from` to `to`, failing with `ErrorKind::AlreadyExists` if `to`
/// exists (any case). Never replaces. `from` and `to` must be on the same volume.
pub(crate) fn move_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let (f, t) = (wide(from), wide(to));
    // SAFETY: both buffers are NUL-terminated UTF-16 that outlive the call.
    let ok = unsafe { MoveFileExW(f.as_ptr(), t.as_ptr(), MOVEFILE_WRITE_THROUGH) };
    if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

pub(crate) fn is_exists_error(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::AlreadyExists || matches!(e.raw_os_error(), Some(80 | 183))
}

/// Bytes available to the calling user on the volume holding `dir`
/// (`lpFreeBytesAvailableToCaller`; quotas can make it smaller than total free space).
pub(crate) fn free_space_for_caller(dir: &Path) -> io::Result<u64> {
    let w = wide(dir);
    let mut avail = 0u64;
    // SAFETY: `w` is NUL-terminated; `avail` is a valid out-pointer; the other outputs are optional and null.
    let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, std::ptr::null_mut(), std::ptr::null_mut()) };
    if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(avail) }
}

pub(crate) fn is_storage_full(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::StorageFull || matches!(e.raw_os_error(), Some(39 | 112))
}
```

**Testing** (temp dir):
- AC6.1: an existing `b.jpg` ("old") with `move_no_replace(a.tmp, B.JPG)` → `Err` with `is_exists_error`, and `b.jpg` still reads "old".
- A rename to a free name succeeds.
- `free_space_for_caller(temp)` > 0.

**Verification:** `cargo test -p photosort fsops`

**Commit:** `feat(app): add no-replace rename and free-space query`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Copy executor (`copy.rs`)

**Verifies:** photosort-mvp.AC5.2, AC5.3, AC5.5, AC5.6, AC5.7, AC5.8, AC5.9 (no temp files), AC6.1, AC6.2 (end-to-end), AC6.6 (renames recorded), AC12.1 (copy portion)

**Files:**
- Create: `crates/photosort/src/copy.rs` (Imperative Shell)
- Create: `crates/photosort/src/copy_summary.rs` (Functional Core): `ShotResult`, `AbortReason`, `CopySummary`, and `pub(crate) fn ticks_to_clear(summary: &CopySummary) -> Vec<ShotId>` (shots whose result is `Copied` or `SkippedAlreadyCopied`)
- Modify: `crates/photosort/src/main.rs`: `mod copy; mod copy_summary;`

**Implementation:**

```rust
pub(crate) struct CopyEnv {
    /// Seams for conditions a test cannot create on a real disk.
    pub free_space: fn(&Path) -> io::Result<u64>,
    pub verify: fn(&Path, &blake3::Hash) -> io::Result<bool>,
}
impl Default for CopyEnv { /* fsops::free_space_for_caller, verify_by_rehash */ }

pub(crate) struct CopyContext<'a> { pub source_root: &'a Path, pub dest: &'a Path, pub env: CopyEnv }

#[derive(Debug, thiserror::Error)]
pub(crate) enum PreflightError {
    #[error("the destination is inside the source folder")] DestInsideSource,
    #[error("cannot create the destination folder: {0}")] CannotCreate(io::Error),
    #[error("the destination folder is not writable: {0}")] NotWritable(io::Error),
    #[error("not enough free space: need {needed} bytes, {available} available")] InsufficientSpace { needed: u64, available: u64 },
    #[error("cannot determine free space: {0}")] FreeSpaceUnknown(io::Error),
}

pub(crate) enum CopyEvent { ShotStarted(ShotId), Bytes { done: u64, total: u64 }, ShotFinished(ShotId, ShotResult) }

// The next three types live in copy_summary.rs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ShotResult {
    Copied { files: Vec<(PathBuf, String)>, renamed: bool, unpaired: bool },
    SkippedAlreadyCopied,
    Failed { reason: String },
    NotAttempted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AbortReason { SourceGone, DestinationFull }

pub(crate) struct CopySummary { pub results: Vec<(ShotId, ShotResult)>, pub aborted: Option<AbortReason> }

pub(crate) fn preflight(plan: &CopyPlan, ctx: &CopyContext) -> Result<(), PreflightError>;
pub(crate) fn execute(plan: &CopyPlan, ctx: &CopyContext, observer: &mut dyn FnMut(CopyEvent)) -> CopySummary;
```

`preflight` runs in this order, and fails before any copy is written:
1. Canonicalise the source root, and the nearest existing ancestor of the destination, with `std::fs::canonicalize`. Compare them case-insensitively with `Path::starts_with` on lowercased strings. A destination inside the source → `DestInsideSource`.
2. `create_dir_all(dest)` → `CannotCreate`.
3. The write probe: `OpenOptions::new().write(true).create_new(true)` on `dest/.photosort-write-probe.photosort-tmp`, then delete it → `NotWritable`.
4. `free_space(dest) >= plan.total_bytes` → `InsufficientSpace`, or `FreeSpaceUnknown` if the query fails.

`execute`:
- Calls `preflight` and returns with every shot `NotAttempted` if it fails. The app shows pre-flight errors before calling this, but `execute` checks again because state may have changed.
- Each skipped shot → `SkippedAlreadyCopied`.
- Each planned shot, in order:
  1. **Stage.** For each file, `tmp = dest/.{dest_stem}.photosort-tmp.{ext}` (e.g. `.DSC_0001.photosort-tmp.NEF`). The real extension stays last so ExifTool (Phase 8) identifies the format by name, and the `.photosort-tmp` infix marks the file as photosort's. Delete a stale tmp of that exact name if present.
     - Open the source with `File::open` (read-only).
     - Open the tmp with `create_new`.
     - Copy through a 1 MiB buffer while feeding `blake3::Hasher`, emitting `Bytes` events every 4 MiB.
     - `sync_all`, then `env.verify(tmp, &hash)` (the default re-reads the tmp and compares hashes).
     - On mismatch → `Failed("verify mismatch")` (AC5.7).
     - `tmp_file.set_modified(source_mtime)` (AC5.2).
  2. **Classify any staging error:**
     - the source open or read fails *and* `!ctx.source_root.exists()` → this shot is `Failed { reason: "source disappeared" }`, and the batch aborts with `SourceGone`;
     - `fsops::is_storage_full` → this shot is `Failed { reason: "destination full" }`, and the batch aborts with `DestinationFull`;
     - otherwise → `Failed(err)`.

     On any failure or abort, delete this shot's temp files.
  3. **Commit.** For each file, `move_no_replace(tmp, dest/dest_name)`. On `is_exists_error` (a late collision, AC6.1):
     - re-list the destination names (`read_dir`, lowercased);
     - `(stem, _) = next_free_stem(original_source_stem, exts, &taken)`;
     - `move_no_replace` this shot's already-committed files to the new stem;
     - continue with the remaining tmp files under the new stem, and mark `renamed`;
     - give up after 10 attempts → `Failed`.
  4. Emit `ShotFinished`.
- After an abort, the remaining shots → `NotAttempted`.
- On return, remove every file in the destination whose name starts with one of this run's tmp names (AC5.9). This covers the tmp itself and any helper file derived from it, such as ExifTool's `_exiftool_tmp` in Phase 8.

**Testing** (temp source card and destination, built with testkit; sources written with known mtimes via `File::set_modified`):
- AC5.2: copy two J+N shots. Each destination file's BLAKE3 equals its source's, and `metadata().modified()` equals the source mtime.
- AC5.3: snapshot the source tree (relative path, size, mtime) before `list_source` + `parse_file` + `plan_copy` + `execute`, and again after. The snapshots are identical.
- AC5.5: `CopyEnv { free_space: |_| Ok(10), .. }` → `preflight` returns `InsufficientSpace`, and `execute` writes nothing (the destination contains no files).
- AC5.6: a destination whose parent is a regular file → `CannotCreate`.
- AC5.6 (existing but unwritable folder):
  - create a temp destination folder, then run `icacls <dir> /deny *S-1-1-0:(OI)(CI)(W)` via `std::process::Command`. `S-1-1-0` is Everyone, and a deny entry applies to administrators too, so this works on CI runners;
  - `preflight` → `NotWritable`, and no file is created;
  - cleanup runs `icacls <dir> /remove:d *S-1-1-0` before the temp dir drops;
  - assert that the `icacls` exit status is success, so a missing tool fails the test instead of silently passing.
- AC5.7: a `verify` that returns `false` for files whose name contains `0002` → that shot is `Failed`, the other shots are `Copied`, and no file whose name contains `.photosort-tmp` remains.
- AC5.8: the observer deletes the whole source root on `ShotStarted` of the 2nd shot → summary: shot 1 `Copied`, shot 2 `Failed { reason: "source disappeared" }`, the rest `NotAttempted`, `aborted == Some(SourceGone)`.
- AC5.9: after every test above, no file whose name contains `.photosort-tmp` exists in the destination.
- AC5.8 (ticks): `ticks_to_clear` on a summary with `Copied`, `SkippedAlreadyCopied`, `Failed` and `NotAttempted` shots returns only the first two (unit test in `copy_summary.rs`).
- AC6.1 (late collision): compute a plan, *then* create `DSC_0001.JPG` (different bytes) in the destination, then `execute` → the existing file is untouched, and the shot lands as `DSC_0001_2.JPG` + `DSC_0001_2.NEF` with `renamed: true`.
- AC6.2 end-to-end: a pre-existing different `DSC_0001.NEF` → both files copied with `_2`.
- AC6.6: the `Copied` results carry every `(src, dest_name)` pair, used for the summary's rename list.
- AC12.1 (copy): a `.fkr` + `.JPG` shot scanned with `test_support::FakeTiffReader` registered → planned and copied byte-identical, with no change to `plan.rs` or `copy.rs`.

**Verification:** `cargo test -p photosort copy`

**Commit:** `feat(app): execute copy plans with verify and no-overwrite placement`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: Batch log (`paths.rs`, `logfmt.rs`, `batch_log.rs`)

**Verifies:** photosort-mvp.AC5.9 (log content)

**Files:**
- Create: `crates/photosort/src/paths.rs` (Imperative Shell): `app_dir() -> Option<PathBuf>` = `dirs::config_dir()?.join("photosort")` (Roaming AppData on Windows; verified during planning), and `logs_dir(app_dir: &Path) -> PathBuf` = `app_dir.join("logs")`
- Create: `crates/photosort/src/logfmt.rs` (Functional Core):
  - `utc_parts(unix_secs: u64) -> (i32, u32, u32, u32, u32, u32)`, using the days-from-civil algorithm;
  - `log_file_name(unix_secs) -> String` (`"YYYY-MM-DD.log"`);
  - `render_batch(&BatchRecord) -> String`.
- Create: `crates/photosort/src/batch_log.rs` (Imperative Shell): `append(logs_dir: &Path, record: &BatchRecord) -> io::Result<()>`, which calls `create_dir_all` and opens with `OpenOptions::new().append(true).create(true)`.
- Modify: `crates/photosort/src/main.rs`: `mod paths; mod logfmt; mod batch_log;`

`BatchRecord<'a> { started_unix: u64, finished_unix: u64, source: &'a Path, dest: &'a Path, plan: &'a CopyPlan, summary: &'a CopySummary }`. `render_batch` produces one block:

```text
=== batch 2026-11-21T14:03:22Z → 2026-11-21T14:05:10Z
source: E:\DCIM
dest:   D:\Photos\2026-11 trip
COPY   E:\DCIM\100NZ_F\DSC_0001.JPG -> DSC_0001.JPG
RENAME E:\DCIM\101NZ_F\DSC_0001.NEF -> DSC_0001_2.NEF
SKIP   DSC_0007 (already copied)
ERROR  DSC_0009: verify mismatch
ABORT  source disappeared; 12 shots not attempted
```

**Testing:**
- `utc_parts(0)` = 1970-01-01 00:00:00.
- `utc_parts(951_782_400)` = 2000-02-29 00:00:00 (a leap day).
- `utc_parts(1_763_683_200)` = 2025-11-21 00:00:00.
- `render_batch` includes a `COPY`, `RENAME`, `SKIP` and `ERROR` line for a summary containing each.
- `append` creates the folder and appends two blocks in order.

**Verification:** `cargo test -p photosort logfmt` and `cargo test -p photosort batch_log`

**Commit:** `feat(app): write per-batch copy log`
<!-- END_TASK_7 -->
<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 8-9) -->
<!-- START_TASK_8 -->
### Task 8: UI: output selection, hide-copied, badges, Copy and dialogs

**Verifies:** photosort-mvp.AC5.1, AC5.4, AC7.1 (badge), AC7.3, AC3.3 (real `is_hidden`) (manual in Task 9; logic covered above)

**Files:**
- Modify: `crates/photosort/src/ui/top_bar.rs`, `ui/grid.rs`, `ui/overlay.rs`, `app.rs`
- Create: `crates/photosort/src/ui/dialogs.rs` (Imperative Shell)
- Modify: `crates/photosort/src/ui/grid_math.rs`: add `refocus(order_before: &[ShotId], order_after: &[ShotId], focus_shot: ShotId) -> usize` (with tests: keeps the same shot if still visible, else the nearest following position, else the last; returns 0 when `order_after` is empty)

**Implementation:**

App state additions:
- `base_folders: Vec<PathBuf>`;
- `output: Option<OutputSelection>`, `output_dir: Option<PathBuf>` (resolved);
- `index: Option<OutputIndex>`, `index_generation: u64`, `index_rx`;
- `hide_copied: bool`;
- `copy_job: Option<CopyJob>` (thread handle + event receiver);
- `dialog: Option<Dialog>`.

Re-index triggers (AC7.3): call `spawn_index` when `output_dir` changes, after a copy finishes, and on Rescan. Ignore stale generations.

`is_hidden(shot)` = `hide_copied && index.map(|i| copy_status(shot, i)) == Some(CopyStatus::Copied)`. The grid draws `visible_order = order.filter(!is_hidden)`. When it changes, `selection = Selection { focus: old_visible.get(selection.focus).map_or(0, |&s| refocus(&order, &visible_order, s)), anchor: None }`, where `old_visible` is the previous visible order. With no previous focus shot (the previous visible order was empty), focus resets to 0.

From here on, **every grid position indexes `visible_order`, not `order`**. Update the Phase 3 and Phase 4 handlers to match:
- the focus and anchor, `selected_range`, the Space targets and the Enter target (Phase 3 Task 8);
- `reveal_row` and the row count;
- the Esc refocus from the overlay (Phase 4 Task 5): `pos = refocus(&order, &visible_order, shot)`. This keeps the shot if it is still visible. If it was hidden while the overlay was open (for example, **Hide copied** ticked in the top bar, or a re-index marked it copied), `pos` falls back to the nearest following visible position, else the last. Never `unwrap` a position lookup.

Overlay navigation (`neighbor`, `prefetch_set`) keeps using `order` with `is_hidden`, which gives the same sequence. Copy planning deliberately uses the full `order` (step 1 below).

Top bar:
- A base folder `egui::ComboBox` (in-memory list) with **Add base folder…**, which uses `rfd` on a thread.
- A subfolder `TextEdit`. Below it, existing subfolders of the base are listed as selectable labels, read on a thread when the base changes.
- **Browse…** → `OutputSelection::Arbitrary`.
- The resolved path label, in red with the `OutputPathError` text when invalid.
- A **Hide copied** checkbox.
- **Copy**, enabled only when all hold: `output_dir` is valid, at least one tick, `ParsePassDone` received, the index is current for `output_dir`, and no copy is running. A hover tooltip says which condition is missing.

Tiles: a `copied` (green) or `partial` (amber) badge from `copy_status`.

Copy flow:
1. **Copy** → `plan = plan_copy(&ticked, index)`, where `ticked` lists the ticked shots in the full display `order`, including shots hidden by Hide copied. That way a hidden copied shot is skipped and its tick cleared, rather than staying ticked forever. Then run `preflight` on a thread (it touches the disk). An error → `Dialog::PreflightError(msg)` (AC5.5, AC5.6).
2. On success → `Dialog::Confirm`, showing:
   - shots `plan.shots.len()` (+ skipped count);
   - files `plan.file_count()`;
   - total bytes (human-readable);
   - destination;
   - renames `plan.rename_count()` (AC5.4).

   **Cancel** closes the dialog and nothing is written.
3. **Copy** in the dialog → spawn a thread running `execute`, with an observer that sends `CopyEvent`s over a channel and calls the waker. `Dialog::Progress` shows bytes done/total and the current shot.
4. On finish:
   - `batch_log::append(paths::logs_dir(app_dir), ..)` (on the copy thread; a failure to write the log is shown in the summary, not fatal);
   - clear the ticks returned by `copy_summary::ticks_to_clear(&summary)` (AC5.8: every other shot keeps its tick);
   - re-index;
   - `Dialog::Summary`: counts per result; "Renamed" lists every `src → dest` (AC6.6); unpaired flags; failed with reasons; the abort reason with the not-attempted count (AC5.8).

Dialogs use `egui::Modal::new(egui::Id::unique("copy_dialog")).show(ctx, …)`. While a dialog is open, grid and overlay keys are inactive.

**Testing:** `refocus` unit tests; everything else is covered by Tasks 1-7 and manual checks.

**Verification:** `cargo build -p photosort`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`

**Commit:** `feat(app): add output selection, hide-copied, copy dialogs and status badges`
<!-- END_TASK_8 -->

<!-- START_TASK_9 -->
### Task 9: Phase verification and manual copy checks

**Verifies:** photosort-mvp.AC5.1, AC5.4, AC7.1, AC7.2, AC7.3, AC3.3 (manual; append results to `docs/implementation-plans/2026-11-20-photosort-mvp/manual-test-log.md`)

Automated gate: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`.

Manual, on a generated card plus an empty output folder:
1. AC5.1:
   - add a base folder, type a new subfolder name, and copy → the folder is created;
   - choose an existing subfolder from the list;
   - **Browse…** to an arbitrary folder;
   - `..\x` shows an error and disables Copy;
   - keyboard gate: focus a tile, then type `2026 L trip` into the subfolder field → the text appears in the field, no tick changes, and no picker opens.
2. AC5.4: tick 5 shots → the confirmation shows 5 shots, the right file count, bytes, destination and renames. **Cancel** → the output folder is unchanged.
3. Copy them → the summary appears.
   - AC7.1: the tiles show `copied`.
   - Re-tick one and copy → the summary reports it as skipped.
4. AC7.2: delete one NEF from the output → the tile shows `partial` after re-index (Rescan). Copy → only the NEF is copied, with the partner's stem.
5. AC7.3: **Hide copied** hides the copied tiles and keeps partial ones; overlay ←/→ skips hidden shots (AC3.3). With Hide copied on, Enter opens the ringed shot, Space ticks the ringed shot or range, and Esc from the overlay returns the ring to the shot that was showing. Then open a copied shot with Hide copied off, tick **Hide copied** in the top bar, and press Esc → the ring lands on the next visible shot, with no crash.
6. Copy the same shots by hand into another folder, renaming one, and select that folder → those tiles are `copied`.
7. Check `%APPDATA%\photosort\logs\<UTC date>.log` contains COPY, SKIP and RENAME lines.
8. AC5.6 (dialog): choose **Browse…** → `C:\Windows` (or any folder whose Security tab denies you Write) and press Copy → the pre-flight error dialog names the problem, and nothing is written. AC5.5 uses the same dialog; its free-space condition is covered by the automated seam test.
9. AC6.6: put a *different* photo named `DSC_0001.JPG` in the output, then tick the source J+N shot `DSC_0001` and copy → the summary's **Renamed** list shows `…\DSC_0001.JPG → DSC_0001_2.JPG` and `…\DSC_0001.NEF → DSC_0001_2.NEF`.
<!-- END_TASK_9 -->
<!-- END_SUBCOMPONENT_C -->
