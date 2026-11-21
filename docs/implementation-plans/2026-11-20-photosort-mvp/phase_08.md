# photosort MVP Implementation Plan — Phase 8: Lens EXIF writing via ExifTool (stretch)

**Goal:** Apply resolved lens edits to copies during the copy pipeline.

**Architecture:**
- `photosort-core/src/lens_write.rs` (pure) turns a `LensEdit` + `LensProfile` into ExifTool arguments and parses ExifTool's replies.
- `crates/photosort/src/exiftool.rs` wraps one long-lived `-stay_open` ExifTool process, with numbered commands, stderr terminators, a timeout and restart.
- `copy.rs` adds a lens-edit step between verify/mtime and the no-replace rename. The edit runs only on temp copies, only for formats whose reader allows it, and records original values for the batch log.
- `scripts/package.ps1` builds a release folder with a checksum-verified ExifTool.

**Tech Stack:** ExifTool 13.64 (`exiftool-13.64_64.zip`, SHA-256 `72c29c44d1e860eebcaefa71692d979af9858fe9a6d7d791a5e7f1dac77804af`, confirmed against https://exiftool.org/checksums.txt), `serde_json` 1.0.154, PowerShell 7.

**Scope:** 8 phases from original design (this is phase 8 of 8)

**Codebase verified:** 2026-11-21

---

## Acceptance Criteria Coverage

This phase implements and tests:

### photosort-mvp.AC11: Lens EXIF writing (stretch)
- **photosort-mvp.AC11.1 Success:** A Chip edit sets the copy's ExifIFD `LensMake`, `LensModel` and `LensInfo` to the profile's values. `FocalLength`, `FNumber` and focus distance stay unchanged.
- **photosort-mvp.AC11.2 Success:** A Manual edit also writes `FocalLength`, `FocalLengthIn35mmFormat`, the shot's `FNumber`, `MaxApertureValue`, and `SubjectDistance` = infinity.
- **photosort-mvp.AC11.3 Success:** Maker-note data is byte-identical before and after an edit, as verified with ExifTool.
- **photosort-mvp.AC11.4 Success:** Edits apply to both the JPEG and the raw copy of a shot. Source files are never edited.
- **photosort-mvp.AC11.5 Failure:** If ExifTool is missing or returns an error, the copy is kept unedited and the summary flags the shot "lens not applied".
- **photosort-mvp.AC11.6 Success:** The batch log records the original lens field values of every edited file.

### photosort-mvp.AC12: Body-agnostic extensibility
- **photosort-mvp.AC12.5 Edge:** For a format whose reader reports `lens_write_supported = false`, the copy is kept unedited and the summary notes "lens edit unsupported for format".

**Decisions approved during planning:**
- Original values are read by ExifTool (`-json -n -G1` of the fields about to be written), in the same session, immediately before each write.
- The edit runs on the verified temp copy with `-overwrite_original -P`, before the no-replace rename. ExifTool replaces the file only on success, so a failed write leaves the unedited copy intact.
- An edit failure keeps the unedited copy and flags the file; the copy itself still succeeds.
- ExifTool is located via `Config.exiftool_path`, else `exiftool.exe` beside `photosort.exe`. It's spawned with `CREATE_NO_WINDOW`, and restarted once on a crash or a 60 s timeout.
- ExifTool-dependent tests are `#[ignore]` and fail loudly without `PHOTOSORT_EXIFTOOL`.
- `package.ps1` supports `-DryRun` and `-ZipPath`, and copies ExifTool's licence text into the release.

## Notes for the implementor (sources: ExifTool source @3d7a053 and `exiftool_pod`)

- **Stay-open protocol:**
  - start with `-stay_open True -@ -` and send one argument per line;
  - `-execute<N>` ends a command, and ExifTool prints `{ready<N>}` on stdout. `-q` suppresses `{ready}` unless numbered; do not use `-q`;
  - `-echo4 <text>` prints to stderr *after* processing, which terminates each command's stderr;
  - `-stay_open` / `False` quits.

  (exiftool:402-441, 1017-1027, 4923-4985)
- **Non-ASCII paths:** arguments in an argfile are not recoded on Windows, so `-charset filename=utf8` must come before `-@` (exiftool:7490-7523).
- **Write semantics:**
  - `LensInfo#=` takes four space-separated numbers (`#` skips print conversion);
  - `MaxApertureValue` takes an f-number, which ExifTool converts to APEX (`2·log2 f`);
  - `SubjectDistance=inf` is stored as the rational 1/0;
  - `FocalLengthIn35mmFormat` is `int16u`.

  (Exif.pm:2375-2391, 2892-2898, 5826-5843; Writer.pl:5227-5294)
- **Maker notes:** Nikon's maker note declares its own `Base`, so ExifTool does not base-fix it on write (MakerNotes.pm:56; WriteExif.pl:695-697, 1494). Byte identity is inferred, not documented, so Task 5 tests it.
- **Download:** the URL `https://exiftool.org/exiftool-13.64_64.zip` is inferred and may move to SourceForge's archive later. The SHA-256 is confirmed.
- **Windows zip contents:** `exiftool(-k).exe` plus `exiftool_files\`. Rename to `exiftool.exe` and keep both together.
- **`.gitignore`:** do not add `exiftool*`, because it would also ignore `crates/photosort/src/exiftool.rs`.

<!-- START_TASK_1 -->
### Task 1: DxO pre-check (human gate)

**Verifies:** None (design prerequisite: "Before starting Phase 8, hand-edit `LensModel`/`LensMake` on a scratch copy of a Fringer shot with ExifTool and open it in PhotoLab.")

Ask the user to do the following, and record the result under `## Phase 8` in `docs/implementation-plans/2026-11-20-photosort-mvp/manual-test-log.md`:
1. Copy one Fringer-adapter shot (JPEG + NEF) to a scratch folder.
2. Run `exiftool -overwrite_original -ExifIFD:LensMake=Canon "-ExifIFD:LensModel=Canon EF 80-200mm f/2.8L" "-ExifIFD:LensInfo#=80 200 2.8 2.8" <files>`.
3. Open it in DxO PhotoLab and note which lens module it proposes. Repeat on an untouched copy for comparison.

**If PhotoLab still proposes the wrong module after the edit, stop and escalate to the user before continuing.** The design notes that DxO may key on Nikon maker-note fields (`LensMountType`, `LensType`), which this design deliberately never writes.
<!-- END_TASK_1 -->

<!-- START_SUBCOMPONENT_A (tasks 2-3) -->
<!-- START_TASK_2 -->
### Task 2: ExifTool argument building and reply parsing (`photosort-core/src/lens_write.rs`)

**Verifies:** photosort-mvp.AC11.1, photosort-mvp.AC11.2 (which tags are written), photosort-mvp.AC11.6 (originals parsing)

**Files:**
- Create: `crates/photosort-core/src/lens_write.rs` (Functional Core)
- Modify: `crates/photosort-core/src/lib.rs`: `pub mod lens_write;`
- Modify: `crates/photosort-core/Cargo.toml`: move `serde_json = { workspace = true }` into `[dependencies]` (parsing ExifTool JSON is pure)

**Implementation:**

```rust
/// Tags this module may write; also the tags read back as "originals".
pub const LENS_TAGS: &[&str] = &["ExifIFD:LensMake", "ExifIFD:LensModel", "ExifIFD:LensInfo",
    "ExifIFD:FocalLength", "ExifIFD:FocalLengthIn35mmFormat", "ExifIFD:FNumber",
    "ExifIFD:MaxApertureValue", "ExifIFD:SubjectDistance"];

/// Arguments (one per line in the -@ stream) to apply `edit` to `file`.
/// Never includes a MakerNotes tag.
pub fn write_args(edit: &LensEdit, profile: &LensProfile, file: &str) -> Result<Vec<String>, LensWriteParseError>;

/// Arguments to read the current values of LENS_TAGS as JSON.
pub fn read_args(file: &str) -> Result<Vec<String>, LensWriteParseError>; // ["-json", "-n", "-G1", "-ExifIFD:LensMake", …, file]

/// Original values keyed by tag name, as ExifTool printed them (strings).
pub fn parse_originals(json_stdout: &str) -> Result<BTreeMap<String, String>, LensWriteParseError>;

/// "1 image files updated" → Ok; otherwise Err with the stderr/stdout text.
pub fn write_succeeded(stdout: &str, stderr: &str) -> Result<(), String>;
```

`write_args`:
- **Common:** `-overwrite_original`, `-P`, `-ExifIFD:LensMake=<make>`, `-ExifIFD:LensModel=<model>`, and `-ExifIFD:LensInfo#=<min_focal> <max_focal> <max_f_at_min> <max_f_at_max>`. Numbers are formatted with the shortest decimal (`28`, `2.8`).
- **Manual only:**
  - `-ExifIFD:FocalLength=<focal_mm>`;
  - `-ExifIFD:FocalLengthIn35mmFormat=<round(focal_mm)>` (full-frame assumption, accepted for MVP; DX crop shots will be wrong);
  - `-ExifIFD:FNumber=<edit.f_number>`;
  - `-ExifIFD:MaxApertureValue=<max_f>`;
  - `-ExifIFD:SubjectDistance=inf`.
- Then the file path.
- Any value or path containing `\r` or `\n` → `LensWriteParseError::InvalidValue`. The `-@` protocol is line-based, so a newline would inject an extra argument.

**Testing** (unit tests):
- AC11.1: a Chip edit → exactly the common tags plus the file. No `FocalLength`, `FNumber`, `SubjectDistance` or `MakerNotes`.
- AC11.2: the FDn 28mm Manual edit at f/8 → also `FocalLength=28`, `FocalLengthIn35mmFormat=28`, `FNumber=8`, `MaxApertureValue=2.8`, `SubjectDistance=inf`; `LensInfo#=28 28 2.8 2.8`.
- A lens model containing `\n` → `Err`.
- AC11.6: `parse_originals` on `[{"SourceFile":"x","ExifIFD:LensModel":"VR 80-200mm f/2.8G","ExifIFD:FNumber":2.8}]` → both values, as strings.
- `write_succeeded` accepts `"    1 image files updated"` and rejects `"    0 image files updated"` or a non-empty `Error:` on stderr.

**Verification:** `cargo test -p photosort-core lens_write`

**Commit:** `feat(core): build ExifTool lens-edit arguments and parse replies`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: Stay-open ExifTool process (`exiftool.rs`)

**Verifies:** photosort-mvp.AC11.5 (missing ExifTool is an error value); ExifTool-backed behaviour is verified in Task 5

**Files:**
- Create: `crates/photosort/src/exiftool.rs` (Imperative Shell)
- Modify: `crates/photosort/src/main.rs`: `mod exiftool;`

**Implementation:**

```rust
pub(crate) struct ExifTool { child: Child, stdin: ChildStdin, out: Receiver<String>, err: Receiver<String>, seq: u32, path: PathBuf }
pub(crate) struct Reply { pub stdout: String, pub stderr: String }

#[derive(Debug, thiserror::Error)]
pub(crate) enum ExifToolError {
    #[error("ExifTool not found at {0}")] NotFound(PathBuf),
    #[error("ExifTool could not start: {0}")] Spawn(io::Error),
    #[error("ExifTool did not answer within {0:?}")] Timeout(Duration),
    #[error("ExifTool exited unexpectedly")] Exited,
}

impl ExifTool {
    /// `<path> -charset filename=utf8 -stay_open True -@ -`, with piped stdio and
    /// `std::os::windows::process::CommandExt::creation_flags(0x0800_0000)` (CREATE_NO_WINDOW).
    pub fn start(path: &Path) -> Result<Self, ExifToolError>;
    /// Writes `args` one per line, then `-echo4`, `{done<N>}`, `-execute<N>`;
    /// collects stdout until `{ready<N>}` and stderr until `{done<N>}`.
    pub fn run(&mut self, args: &[String], timeout: Duration) -> Result<Reply, ExifToolError>;
    /// One automatic restart and retry on `Exited` or `Timeout`.
    pub fn run_with_restart(&mut self, args: &[String], timeout: Duration) -> Result<Reply, ExifToolError>;
}
impl Drop for ExifTool { /* send "-stay_open\nFalse\n"; wait up to 2 s; then kill */ }

/// `Config.exiftool_path` if set, else `<dir of current_exe>/exiftool.exe`.
pub(crate) fn locate(config_path: Option<&Path>) -> PathBuf;
```

- Two reader threads forward stdout and stderr lines into `out` and `err`.
- The `{ready<N>}` and `{done<N>}` matchers are small pure helpers in this file, with unit tests.
- `start` checks `path.is_file()` first → `NotFound`.

**Testing:**
- Unit (normal CI):
  - `start(Path::new("C:\\nope\\exiftool.exe"))` → `NotFound` (AC11.5);
  - the marker matcher recognises `{ready12}` only for seq 12;
  - `locate(None)` ends with `exiftool.exe`.
- Ignored (`#[ignore = "needs ExifTool: set PHOTOSORT_EXIFTOOL"]`, panicking with that message when the variable is unset or the path doesn't exist):
  - `run(["-ver"])` returns `13.64`;
  - two sequential commands each get their own reply;
  - killing the child, then `run_with_restart`, succeeds.

**Verification:** `cargo test -p photosort exiftool` (unit) and `cargo test -p photosort exiftool -- --ignored` with `PHOTOSORT_EXIFTOOL` set

**Commit:** `feat(app): add stay-open ExifTool process wrapper`
<!-- END_TASK_3 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 4-5) -->
<!-- START_TASK_4 -->
### Task 4: Lens-edit step in the copy pipeline

**Verifies:** photosort-mvp.AC11.4 (both files edited; sources untouched), photosort-mvp.AC11.5, photosort-mvp.AC11.6 (logging), photosort-mvp.AC12.5

**Files:**
- Modify: `crates/photosort/src/copy.rs`, `crates/photosort/src/copy_summary.rs`, `crates/photosort/src/logfmt.rs`, `crates/photosort/src/ui/dialogs.rs`, `crates/photosort/src/app.rs`
- Modify: the Phase 5 `copy.rs` and `copy_summary.rs` tests: construct `CopyContext { lens: None, .. }` and expect `ShotResult::Copied { lens: vec![], .. }`

**Implementation:**
- `CopyContext` gains `lens: Option<LensStep<'a>>`, with `pub(crate) struct LensStep<'a> { pub profiles: &'a [LensProfile], pub registry: &'a Registry, pub exiftool_path: PathBuf }`. `execute` starts the ExifTool process lazily, on the first file that needs it.
- `#[derive(Debug, Clone, PartialEq)] pub(crate) enum LensNote { Applied { originals: BTreeMap<String, String> }, NotApplied(String), Unsupported }`.
- `ShotResult::Copied` gains `lens: Vec<(String /*dest_name*/, LensNote)>`.
- The step runs after verify and `set_modified` (staging), and before commit, for each staged file with `lens_edit: Some(edit)`:
  1. `registry.format_for_kind(kind)` reports `lens_write_supported() == false` → `Unsupported` (AC12.5).
  2. The profile is missing → `NotApplied("profile deleted")`.
  3. ExifTool isn't available (`NotFound`/`Spawn`) → `NotApplied(err.to_string())` (AC11.5). Remember this for the rest of the batch, so later files skip the spawn.
  4. `read_args(tmp)` → `run_with_restart` → `parse_originals`.
  5. `write_args(edit, profile, tmp)` → `run` (not `run_with_restart`) → `write_succeeded`. On `Timeout`/`Exited`: restart the process, delete `<tmp>_exiftool_tmp` if it exists, then retry the write once with `run`. A leftover ExifTool temp file would make the retry fail with "Temporary file already exists" (inferred from Writer.pl, unverified). Success → `Applied { originals }`; an error → `NotApplied(msg)`.
  6. Re-apply `set_modified(source_mtime)` on the tmp as a belt-and-braces step (`-P` should already preserve it). A failure here never fails the shot (no `?`): it is appended to the file's note text and logged.
  7. After a `Timeout` or `Exited` error on the step 4 read, or on the step 5 retry, delete `<tmp>_exiftool_tmp` if it exists before recording `NotApplied`. This is ExifTool's own temp file from `-overwrite_original`; the name is inferred from Writer.pl and unverified. The Phase 5 end-of-run sweep also removes it.
- A lens failure **never** fails the shot. It is recorded, and the copy proceeds to commit.
- ExifTool only ever receives tmp paths inside the destination. Assert in code that the path starts with `ctx.dest` (and return `NotApplied` otherwise); never pass a source path (AC11.4: sources are never edited).
- `logfmt::render_batch` adds:
  - `LENS   DSC_0001.NEF: LensModel "VR 80-200mm f/2.8G"→"Canon EF 80-200mm f/2.8L"; LensMake ""→"Canon"; …` for `Applied`, listing every original from `LENS_TAGS` (AC11.6);
  - `NOLENS DSC_0002.JPG: <reason>`;
  - `UNSUPP DSC_0003.FKR: lens edit unsupported for format`.
- The summary dialog lists files flagged "lens not applied" and "lens edit unsupported for format".
- The app passes `lens: Some(LensStep { .., exiftool_path: exiftool::locate(config.exiftool_path.as_deref()) })` whenever any planned file has a `lens_edit`.

**Testing** (normal CI, no ExifTool needed):
- AC11.5: a plan where a J+N shot has a lens edit, with `exiftool_path` pointing to a nonexistent file → the shot is `Copied`, both files are byte-identical to the source, and the lens notes are `NotApplied("ExifTool not found …")`.
- AC12.5: a `.fkr` file (`test_support::FakeTiffReader`, `lens_write_supported = false`) with an edit → `Unsupported`, the copy is byte-identical, and the JPEG of the same shot still attempts its edit (here `NotApplied`, because ExifTool is absent).
- `render_batch` renders `LENS`, `NOLENS` and `UNSUPP` lines from crafted summaries.

**Verification:** `cargo test -p photosort copy` and `cargo test -p photosort logfmt`

**Commit:** `feat(app): apply lens edits to staged copies via ExifTool`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: ExifTool integration tests (opt-in)

**Verifies:** photosort-mvp.AC11.1, AC11.2, AC11.3, AC11.4, AC11.6

**Files:**
- Modify: `crates/photosort/src/copy.rs` (an ignored test module `lens_integration`)

**Implementation:** tests marked `#[ignore = "needs ExifTool: set PHOTOSORT_EXIFTOOL"]`, each panicking with that message if the variable is unset or the path doesn't exist. Each one:
- builds a temp source card with a testkit J+N shot (`ExifSpec::zf()` with `lens_model: "VR 80-200mm f/2.8G"`, and a Nikon maker note via `NikonSpec::zf`);
- plans with a lens edit and runs `execute` with `LensStep` pointing at `PHOTOSORT_EXIFTOOL`;
- reads results back with `<exiftool> -json -n -G1 <files>` via `std::process::Command`.

Cases:
- **AC11.1 (Chip "Canon EF 80-200mm f/2.8L", LensInfo 80/200/2.8/2.8):**
  - `ExifIFD:LensMake == "Canon"`, `LensModel` as in the profile, and `LensInfo == "80 200 2.8 2.8"`;
  - `FocalLength`, `FNumber` and `SubjectDistance` equal the source's values (read from the source with the same command).
- **AC11.2 (Manual FDn 28mm f/2.8 at f/8):**
  - `FocalLength 28`, `FocalLengthIn35mmFormat 28`, `FNumber 8`;
  - `MaxApertureValue` within 0.05 of 2.8 (ExifTool's `-n` converts APEX back to an f-number);
  - `SubjectDistance == "inf"`.
- **AC11.3:** `<exiftool> -b -MakerNotes <file>` stdout bytes are equal (BLAKE3) for the source and the edited copy. This holds for both the JPEG and the NEF, and for both testkit layouts (`NikonSpec.preview_inside` true and false).
- **AC11.3 on real files:** the synthetic layout may not match a real Z f, so also add `lens_samples_preserve_makernotes` (`#[ignore]`; needs `PHOTOSORT_EXIFTOOL` and `PHOTOSORT_SAMPLES`, and fails loudly without them). It copies up to 4 sample J+N pairs through `execute` with the FDn 28mm edit, and compares the source and copy `-b -MakerNotes` hashes.
- **AC11.4:** both the JPEG and the NEF copies carry the new `LensModel`, and the source snapshot (path, size, mtime, BLAKE3) is unchanged.
- **AC11.5 (ExifTool returns an error):** a shot whose JPEG source is a valid testkit JPEG truncated right after its APP1 segment (so ExifTool rejects the write), planned with a lens edit → the shot is `Copied`, the copy is byte-identical to the source, and the note is `NotApplied` carrying ExifTool's error text.
- **File type:** `File:FileType` is `NEF` for each edited NEF copy and `JPEG` for each JPEG. The staged name `.X.photosort-tmp.NEF` keeps the real extension, so ExifTool treats the file as NEF rather than generic TIFF.
- **AC5.9 with edits:** after each test, no file whose name contains `.photosort-tmp` remains in the destination.
- **AC11.6:** the summary's `Applied { originals }` has `ExifIFD:LensModel == "VR 80-200mm f/2.8G"`, and the rendered batch log contains it.

**Verification:** `$env:PHOTOSORT_EXIFTOOL='<path>'; cargo test -p photosort lens_integration -- --ignored`. Expected: all pass. Without the variable, `-- --ignored` fails with the message naming `PHOTOSORT_EXIFTOOL`.

**Commit:** `test(app): verify lens edits and maker-note preservation with ExifTool`
<!-- END_TASK_5 -->
<!-- END_SUBCOMPONENT_B -->

<!-- START_TASK_6 -->
### Task 6: Release packaging script (`scripts/package.ps1`)

**Verifies:** None (infrastructure).

**Files:**
- Create: `scripts/package.ps1`
- Modify: `.gitignore`: add `/dist/` and `/tools/`. **Do not** add `exiftool*`.
- Modify: `README.md`: a "Packaging" section showing `pwsh scripts/package.ps1 [-ZipPath <zip>] [-DryRun]`

**Implementation** (PowerShell 7; `Set-StrictMode -Version Latest`; `$ErrorActionPreference = 'Stop'`):

```powershell
param(
    [string]$ZipPath,   # use a pre-downloaded exiftool-13.64_64.zip instead of downloading
    [switch]$DryRun     # print every action without performing it
)
$ExifToolVersion = '13.64'
$ExifToolSha256  = '72c29c44d1e860eebcaefa71692d979af9858fe9a6d7d791a5e7f1dac77804af'
$ExifToolUrl     = "https://exiftool.org/exiftool-$($ExifToolVersion)_64.zip"
```

Steps (each announced with `Write-Host`; under `-DryRun`, actions are printed and skipped):
1. `cargo build -p photosort --release --locked`.
2. `$dist = dist\photosort-<version from Cargo.toml>`. If it exists, delete it, but only after checking that its resolved path is under the repo's `dist\`.
3. Get the zip:
   - `-ZipPath`, or download with `Invoke-WebRequest` into `tools\`;
   - compute `(Get-FileHash -Algorithm SHA256).Hash`;
   - compare it case-insensitively to `$ExifToolSha256`, and **throw** on a mismatch (the zip is deleted if this run downloaded it).
4. `Expand-Archive` into a fresh temp folder. Copy `exiftool(-k).exe` → `$dist\exiftool.exe` and `exiftool_files\` → `$dist\exiftool_files\`. Copy the zip's README/licence text to `$dist\EXIFTOOL-LICENSE.txt`.
5. Copy `target\release\photosort.exe` and `README.md` into `$dist`.
6. Print the folder path and its file list.

**Verification:**
- `pwsh scripts/package.ps1 -DryRun` → prints the steps, and creates nothing in `dist\` or `tools\`.
- `pwsh scripts/package.ps1 -ZipPath <the user's verified zip>` → `dist\photosort-0.1.0\` contains `photosort.exe`, `exiftool.exe`, `exiftool_files\` and the licence file. `dist\photosort-0.1.0\exiftool.exe -ver` prints `13.64`.
- A tampered zip (a copy with one byte appended) → the script throws a checksum error.
- `git status` shows no `dist\` or `tools\` entries.

**Commit:** `build: add release packaging with checksum-verified ExifTool`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: Phase verification and manual DxO check

**Verifies:** photosort-mvp.AC11.1–AC11.6, AC12.5 (end-to-end; append results to `docs/implementation-plans/2026-11-20-photosort-mvp/manual-test-log.md`)

Automated gate:
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`;
- plus `cargo test --workspace -- --ignored` with `PHOTOSORT_EXIFTOOL` set. Two tests also need `PHOTOSORT_SAMPLES`: the samples oracle (Phase 2) and `lens_samples_preserve_makernotes` (Task 5). If the user hasn't designated samples yet, those two fail by design; report them as pending, not passing. Both must pass before MVP sign-off.

Manual (needs the user):
1. Run the packaged app from `dist\`. In Settings → General the ExifTool path is blank, and the bundled `exiftool.exe` is used.
2. Copy real shots:
   - a Fringer (chipped) shot with a Chip rule;
   - a dumb-adapter shot with the FDn 28mm as the session lens at a confirmed f/5.6.

   The summary shows no "lens not applied" flags, and the batch log has `LENS` lines with originals.
3. Open the copies in DxO PhotoLab and record which lens module it proposes for each, compared with the untouched source. The design's success criterion is that DxO no longer proposes the wrong module.
4. Temporarily set the ExifTool path to a bogus file → copy → the summary flags "lens not applied", and the copies are unedited.
<!-- END_TASK_7 -->
