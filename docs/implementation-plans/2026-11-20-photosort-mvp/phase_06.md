# photosort MVP Implementation Plan — Phase 6: Persistence and resume

**Goal:** Close the app mid-sort and pick up exactly where you left off.

**Architecture:**
- Serde state types live in `photosort-core/src/state.rs`. So does the pure resume logic (`resume.rs`): debounce timing, tick ↔ identity mapping and pruning.
- The shell adds:
  - `persist.rs`: atomic load and save, rename-aside on corruption, and a persist worker thread so the UI thread does no file I/O;
  - `volume.rs`: the volume serial and card id, via Win32.
- The app restores ticks by identity as pass-2 metadata arrives, remembers settings in `config.json`, and saves per-card state under `cards/`.

**Tech Stack:** `serde` 1.0.229 (`derive`), `serde_json` 1.0.154, `windows-sys` 0.61.2 (`Win32_Storage_FileSystem`), `blake3` 1.8.7, `dirs` 7.0.0.

**Scope:** 8 phases from original design (this is phase 6 of 8)

**Codebase verified:** 2026-11-21

---

## Acceptance Criteria Coverage

This phase implements and tests:

### photosort-mvp.AC8: Resume
- **photosort-mvp.AC8.1 Success:** Tick changes are saved within about 1 s. Closing and reopening the app on the same card restores them.
- **photosort-mvp.AC8.2 Success:** Ticks are keyed by identity, so they survive a rescan and the card mounting under a different drive letter.
- **photosort-mvp.AC8.3 Edge:** A reformatted card (new volume serial) starts with no ticks.
- **photosort-mvp.AC8.4 Edge:** Ticks for shots no longer on the card are pruned.
- **photosort-mvp.AC8.5 Success:** Base folders, last source, last output and tile size persist across restarts.
- **photosort-mvp.AC8.6 Failure:** A corrupt config or state file is renamed aside, and the app starts with defaults and tells the user. State writes are atomic, so an interrupted write leaves the previous valid file in place.

### photosort-mvp.AC9 (completion)
- **photosort-mvp.AC9.3 Success:** Cache memory stays within the RAM budget: 25% of physical RAM by default, or the configured override. Evicted tiles reload when scrolled back into view.
  - *This phase wires `config.json` `ram_budget_mb` into the Phase 3 budget.*

### photosort-mvp.AC5 (completion)
- **photosort-mvp.AC5.1 Success:** The output is either a remembered base folder plus a subfolder (existing, or new and created at copy time), or an arbitrary folder chosen with Browse.
  - *"Remembered" across restarts.*

**Decisions approved during planning:**
- The debounce fires 1 s after the last change, capped at 2 s after the first unsaved change, so continuous ticking still saves within about 1-2 s.
- Unmatched saved ticks are pruned only after `ParsePassDone`, so ticks for not-yet-parsed shots survive an early save.
- The card id is the volume serial plus a hash of the lowercased volume-relative path, with a full-path hash fallback when the serial cannot be read.
- `last_source` is reopened automatically on startup if it still exists.
- A state file with `version > 1` is treated like a corrupt one (renamed aside, defaults used, user told).
- No `serde_ignored`: these files are written by the app.
- Lens fields (`profiles`, `rules`, `overrides`, `session_manual_profiles`) are added in Phase 7 with `#[serde(default)]`, still at `version: 1`, because nothing ships between phases.

---

## Notes for the implementor

- A shot can carry two identities: its JPEG and NEF share one on the counter and capture rungs, but differ on the content rung. A ticked shot saves every file's identity and is restored if any of them matches.
- `std::fs::rename` replaces an existing file on Windows. That is the wanted behaviour for atomic state writes (temp file in the same directory, then rename over), unlike copies, which must never replace.
- Verified during planning: `GetVolumePathNameW` → `"C:\"`, then `GetVolumeInformationW(root, …, &mut serial, …)` returns the serial (windows-sys 0.61.2, `Win32_Storage_FileSystem`). Microsoft Learn: the root path needs a trailing backslash, and a relative path resolves to the boot volume, so always pass an absolute path.

<!-- START_SUBCOMPONENT_A (tasks 1-2) -->
<!-- START_TASK_1 -->
### Task 1: State types and identity serde (`photosort-core/src/state.rs`)

**Verifies:** photosort-mvp.AC8.5 (fields exist and round-trip), photosort-mvp.AC8.2 (ticks stored as identity strings)

**Files:**
- Modify: root `Cargo.toml` `[workspace.dependencies]`: `serde = { version = "=1.0.229", features = ["derive"] }`
- Modify: `crates/photosort-core/Cargo.toml`: add `serde = { workspace = true }`; `serde_json` stays a dev-dependency
- Modify: `crates/photosort-core/src/model.rs`: add `PartialOrd, Ord` derives to `Rational` and `IdentityKey` (needed for `BTreeSet`; derives only, no behaviour change)
- Modify: `crates/photosort-core/src/identity.rs`: `impl Serialize for IdentityKey` (`serializer.serialize_str(&self.to_string())`) and `impl<'de> Deserialize<'de> for IdentityKey` (deserialize a `String`, then `parse().map_err(serde::de::Error::custom)`)
- Create: `crates/photosort-core/src/state.rs` (`// pattern: Functional Core`)
- Modify: `crates/photosort-core/src/lib.rs`: `pub mod state;`

**Implementation:**

```rust
pub const STATE_VERSION: u32 = 1;

/// Implemented by every persisted top-level type.
pub trait Versioned { fn version(&self) -> u32; }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedOutput {
    Base { base: PathBuf, subfolder: String },
    Arbitrary { path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub base_folders: Vec<PathBuf>,
    pub last_source: Option<PathBuf>,
    pub last_output: Option<SavedOutput>,
    pub ram_budget_mb: Option<u64>,
    pub tile_size: f32,
    pub exiftool_path: Option<PathBuf>,
}
impl Default for Config { /* version: STATE_VERSION, empty lists, None, tile_size: 200.0 */ }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CardState {
    pub version: u32,
    pub volume_serial: String,
    pub rel_path: String,
    /// Identities of every file of every ticked shot (sorted for stable diffs).
    pub ticked: BTreeSet<IdentityKey>,
    pub last_output: Option<SavedOutput>,
}
impl Default for CardState { /* version: STATE_VERSION, empty */ }
```

`SavedOutput` is externally tagged (serde's default) rather than `untagged`, because house style forbids `untagged` deserializers.

**Testing** (unit tests with `serde_json`):
- `Config` and `CardState` with every field set round-trip through JSON, and so do the defaults.
- An `IdentityKey` serialises as the Phase 2 string form (`"ctr|NIKON CORPORATION|…"`), and a bad string fails to deserialize with a message.
- JSON missing optional fields deserializes with the defaults (`#[serde(default)]`).
- A proptest round-trip over `CardState` with arbitrary identity sets.

**Verification:** `cargo test -p photosort-core state` and `cargo test -p photosort-core identity`

**Commit:** `feat(core): add persisted config and card state types`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Pure resume logic (`photosort-core/src/resume.rs`)

**Verifies:** photosort-mvp.AC8.1 (debounce timing), photosort-mvp.AC8.2, photosort-mvp.AC8.4

**Files:**
- Create: `crates/photosort-core/src/resume.rs` (Functional Core)
- Modify: `crates/photosort-core/src/lib.rs`: `pub mod resume;`

**Implementation:**

```rust
/// Trailing debounce with a maximum wait. Times are caller-supplied, so this stays pure.
#[derive(Debug, Clone, Copy, Default)]
pub struct Debounce { first_dirty: Option<Duration>, last_change: Option<Duration> }
pub const QUIET: Duration = Duration::from_millis(1000);
pub const MAX_WAIT: Duration = Duration::from_millis(2000);
impl Debounce {
    pub fn mark(&mut self, now: Duration);
    /// True when a save should happen now; the caller then calls `saved()`.
    pub fn due(&self, now: Duration) -> bool; // dirty && (now-last >= QUIET || now-first >= MAX_WAIT)
    pub fn saved(&mut self);
    pub fn is_dirty(&self) -> bool;
}

/// Identities of all files of the ticked shots.
pub fn ticked_identities(shots: &[Shot], ticks: &HashSet<ShotId>) -> BTreeSet<IdentityKey>;

/// Whether a just-parsed shot should be ticked from saved state.
pub fn restore_tick(shot: &Shot, saved: &BTreeSet<IdentityKey>) -> bool; // any file identity ∈ saved

/// Identities to write. Before the parse pass finishes, keep saved
/// identities that no shot has matched yet; afterwards, prune them.
pub fn identities_to_save(ticked: BTreeSet<IdentityKey>, unmatched_saved: &BTreeSet<IdentityKey>,
    parse_pass_done: bool) -> BTreeSet<IdentityKey>;
```

**Testing** (unit tests):
- AC8.1:
  - `mark(0)`, `due(500ms)` → false, `due(1000ms)` → true;
  - marks at 0, 600, 1200 and 1800 ms → `due(2000ms)` is true because of `MAX_WAIT`, although the quiet time has not elapsed;
  - after `saved()`, `due` is false.
- AC8.2: shots from one scan (ShotIds 0..3) are ticked, and their identities saved. A "rescan" builds the same files in a different order with new ShotIds, and `restore_tick` re-ticks exactly the same files.
- AC8.2 (pair): a J+N shot ticked with content-rung identities (two different keys) → restored when either file's identity is present.
- AC8.4:
  - `identities_to_save(ticked, unmatched = {X}, false)` keeps X;
  - with `true`, X is dropped.

**Verification:** `cargo test -p photosort-core resume`

**Commit:** `feat(core): add debounce, tick restore and prune logic`
<!-- END_TASK_2 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 3-4) -->
<!-- START_TASK_3 -->
### Task 3: Atomic persistence with rename-aside (`persist.rs`)

**Verifies:** photosort-mvp.AC8.6, photosort-mvp.AC8.5 (load/save round-trip)

**Files:**
- Create: `crates/photosort/src/persist.rs` (Imperative Shell)
- Modify: `crates/photosort/Cargo.toml`: add `serde` and `serde_json` (workspace)
- Modify: `crates/photosort/src/paths.rs`: add `config_path(app_dir: &Path) -> PathBuf` (`app_dir/config.json`) and `cards_dir(app_dir: &Path) -> PathBuf` (`app_dir/cards`)
- Modify: `crates/photosort/src/main.rs`: `mod persist;`

**Implementation:**

```rust
pub(crate) struct Loaded<T> { pub value: T, pub warning: Option<String> }

/// Missing → default, no warning. Unparseable → rename to `<name>.corrupt-<unix secs>`;
/// version > STATE_VERSION → rename to `<name>.newer-<unix secs>`; both then default + warning.
pub(crate) fn load_json<T: DeserializeOwned + Default + Versioned>(path: &Path) -> Loaded<T>;

/// Writes `<dir>/.<name>.tmp-<pid>`, flushes and `sync_all`s it, then
/// `std::fs::rename` over `path` (replacing on Windows). An interrupted
/// write leaves the previous file untouched.
pub(crate) fn save_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()>;

/// Background writer so the UI thread never does file I/O.
pub(crate) enum PersistCmd { Config(PathBuf, Config), Card(PathBuf, CardState), Flush(SyncSender<()>) }
pub(crate) fn spawn_persist_worker(status: Sender<String>) -> (Sender<PersistCmd>, JoinHandle<()>);
```

`save_json_atomic` calls `create_dir_all` on the parent first. The worker coalesces: it drains everything pending and keeps only the latest `Config`/`Card` value per path. It performs all of those writes **before** handling any `Flush` found in the same drain, in queue order, so a flush always sees every write queued before it. Task 5 adds `LoadCard`, which the drain loop treats as the same kind of barrier. A write error is sent on `status`, and the app shows it in the status line.

**Testing** (temp dirs):
- AC8.5: `save_json_atomic` then `load_json` returns an equal `Config`, with no warning.
- AC8.6 corrupt:
  - write `{not json` to `config.json`;
  - `load_json` → the default value, a `Some(warning)` naming the renamed file, `config.json` gone, and a `config.json.corrupt-*` file with the original bytes.
- AC8.6 newer: `{"version": 99}` → renamed aside as `.newer-*`, plus a warning.
- AC8.6 atomic:
  - save a valid file;
  - then simulate an interrupted write by creating a leftover `.config.json.tmp-123` with garbage (a crash after writing the temp file but before the rename);
  - `load_json` still returns the previous valid value, and the next save succeeds and leaves no new temp files.
- The worker: send 50 `Card` commands for one path, then `Flush` → the file holds the last value.

**Verification:** `cargo test -p photosort persist`

**Commit:** `feat(app): add atomic JSON persistence with corrupt-file rename`
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: Volume serial and card id (`volume.rs`)

**Verifies:** photosort-mvp.AC8.2 (drive-letter independence), photosort-mvp.AC8.3 (new serial → new card)

**Files:**
- Create: `crates/photosort/src/volume.rs` (Imperative Shell; the file-name derivation is a pure helper inside it, tested directly)
- Modify: `crates/photosort/src/main.rs`: `mod volume;`

**Implementation:**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CardId {
    /// `serial` as 8 uppercase hex digits; `rel_path` relative to the volume root.
    Volume { serial: String, rel_path: String },
    /// Volume serial unavailable (e.g. network path): keyed by the full path.
    Path { path: String },
}

/// Uses GetVolumePathNameW (absolute path in; 1024-char buffer) and
/// GetVolumeInformationW (root with trailing backslash) via windows-sys.
pub(crate) fn card_id(source: &Path) -> CardId;

/// `<serial>_<first 16 hex of blake3(rel_path.to_lowercase() with '\' separators)>.json`,
/// or `path_<16 hex of blake3(lowercased full path)>.json`.
pub(crate) fn card_file_name(id: &CardId) -> String;
```

`card_id` canonicalises nothing that could change the drive letter:
1. `std::path::absolute(source)`;
2. the volume root via `GetVolumePathNameW`;
3. strip that prefix case-insensitively to get `rel_path`, and trim trailing separators.

Each `unsafe` call gets a `// SAFETY:` comment (NUL-terminated buffers that outlive the call, valid out-pointers, correct buffer lengths).

**Testing:**
- `card_id(tempdir)` on the test machine → `Volume` with an 8-hex serial and a `rel_path` that does not start with a drive letter.
- AC8.2: `card_file_name(Volume { serial: "1A2B3C4D", rel_path: "DCIM" })` equals the name for `rel_path: "dcim"`, and does not depend on any drive letter (the id has none).
- AC8.3: the same `rel_path` with a different serial → a different file name.
- The `Path` fallback name is deterministic.

**Verification:** `cargo test -p photosort volume`

**Commit:** `feat(app): derive card id from volume serial and relative path`
<!-- END_TASK_4 -->
<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 5-6) -->
<!-- START_TASK_5 -->
### Task 5: Wire persistence into the app

**Verifies:** photosort-mvp.AC8.1, AC8.4, AC8.5, AC8.6 (user notification), AC9.3 (override), AC5.1 (remembered base folders) (manual in Task 6; logic covered by Tasks 1-4)

**Files:**
- Modify: `crates/photosort/src/app.rs`, `crates/photosort/src/ui/top_bar.rs`

**Implementation:**

Startup (`PhotosortApp::new`):
1. `app_dir = paths::app_dir()`. If that's `None`, run without persistence and show a status warning.
2. `load_json::<Config>(config_path)`. This is one small file read before the first frame, so it's acceptable at construction. Keep any warning to show.
3. Apply the config:
   - `tile_size`, `base_folders`, and `last_output` → `OutputSelection`;
   - the cache budget comes from `cache_budget_bytes(physical_ram, config.ram_budget_mb)` (AC9.3).
4. Spawn the persist worker.
5. If `last_source` exists on disk → start a scan of it (Phase 3 flow).

When a scan starts:
- **Before** the Phase 3 clear step (which drops shots and ticks): if `card_loaded` and the card debounce is dirty, build the previous card's `CardState` (as in "In `logic` each frame" below) and send its `PersistCmd::Card` immediately, so a rescan of the same card never reads stale state. Never save a card whose state has not loaded; that would overwrite its saved ticks.
- Then reset `card_loaded = false`, `saved_ticks`, `unmatched`, `touched`, `pass_done` and `card_debounce`.
- Send `PersistCmd::LoadCard { generation, source, cards_dir, reply }`. Add this variant to `persist.rs`; the drain loop treats it as a barrier exactly like `Flush` (Task 3): pending writes run first. Its `reply` is `Sender<CardLoaded>`, and `struct CardLoaded { generation: u64, card: CardId, path: PathBuf, loaded: Loaded<CardState> }`. The persist worker:
  - computes `volume::card_id(&source)` (Win32 calls that touch the card, so they stay off the UI thread);
  - derives `card_path = cards_dir/card_file_name(&card)`;
  - runs `load_json::<CardState>`, then replies with a `CardLoaded`.

  Because the worker handles commands in order, the load always sees every earlier write.
- Until the reply arrives, `card_loaded = false`: no card state is saved, and nothing is restored or pruned.
- When a reply arrives (ignore it unless its `generation` is the current scan's):
  - `saved_ticks = state.ticked` and `unmatched = saved_ticks.clone()`;
  - card `last_output` (if any) overrides the global one;
  - a warning is shown if the file was corrupt;
  - apply `restore_tick` to every shot already parsed **except shots in `touched`**, and remove every parsed shot's identities from `unmatched`;
  - set `card_loaded = true`.

On `FileParsed` (once `card_loaded`):
- if the shot is not in `touched` and `restore_tick(shot, &saved_ticks)`, tick it. `touched` holds every shot whose tick the user changed in this scan, so a J+N shot unticked between its two parse events is not re-ticked;
- remove the shot's identities from `unmatched`;
- if the shot is ticked, `card_debounce.mark(now)`, because its identities are now known.

On `ParsePassDone`: `card_debounce.mark(now)`, so the next save prunes (pruning also requires `card_loaded`).

Tick changes (Space, checkbox, copy clearing ticks) → insert the shot into `touched` and `card_debounce.mark(now)`. Config changes (tile size, base folders, output, source) → `config_debounce.mark(now)`.

In `logic` each frame:
- when `card_loaded` and `card_debounce.due(now)`, build a complete `CardState`:
  - `version: STATE_VERSION`;
  - `volume_serial` and `rel_path` from the `CardId` (empty strings for the `Path` fallback, whose file name already encodes the path);
  - `ticked: identities_to_save(ticked_identities(..), &unmatched, pass_done)`;
  - `last_output`: the current output as `SavedOutput`.

  Then send `PersistCmd::Card`, then `saved()`;
- likewise for the config.
- `now` is `Instant::now() - app_start` as a `Duration`.
- While either debounce is dirty, call `ctx.request_repaint_after(Duration::from_millis(250))` so the save fires even without input.

On exit (`fn on_exit(&mut self)`; in eframe 0.37 it takes no `gl` argument unless the `glow` feature is enabled, and the default features use wgpu): send the final `Card` (only if `card_loaded`), the `Config` and a `Flush`, then wait up to 2 s on its reply.

Warnings: the top bar shows a dismissable amber banner, e.g. "config.json was unreadable and was renamed to config.json.corrupt-…; started with defaults" (AC8.6).

**Testing:**
- A unit test in `persist.rs` (temp dirs): send `Card(p, v1)` immediately followed by `LoadCard` for a source whose card file is `p`. Compute `p` as `cards_dir/card_file_name(&card_id(source))`. The reply's `loaded.value` equals v1, and its `generation` echoes the request.
- Everything else is covered by Tasks 1-4 and checked manually in Task 6.

**Verification:** `cargo build -p photosort`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`

**Commit:** `feat(app): persist settings and per-card ticks with resume by identity`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Phase verification and manual resume checks

**Verifies:** photosort-mvp.AC8.1–AC8.6, photosort-mvp.AC5.1 (remembered), photosort-mvp.AC9.3 (override) (manual; append results to `docs/implementation-plans/2026-11-20-photosort-mvp/manual-test-log.md`)

Automated gate: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`.

Manual:
1. AC8.1:
   - open a generated card and tick 10 shots;
   - within about 1 s, `%APPDATA%\photosort\cards\*.json` contains them;
   - close and reopen the app → the same card reopens and the same 10 tiles are ticked once pass 2 reaches them.
2. AC8.2:
   - Rescan → the ticks stay.
   - With a real SD card or USB stick: note the ticks, eject it, change its drive letter in Disk Management (or use another reader), reopen → the ticks are restored.
3. AC8.3: copy the card's folder to a *different* volume (e.g. a USB stick) and open it there → it starts with no ticks (different volume serial).
4. AC8.4: delete 3 ticked files from the card copy, reopen, and wait for pass 2 to finish → the card JSON no longer contains their identities.
5. AC8.5 and AC5.1: set a base folder, an output, and tile size 320, then restart → all are restored, and the last source reopens.
6. AC8.6: with the app closed, replace `config.json` with `{` → on start, a banner names `config.json.corrupt-…`, the defaults are in use, and the original bytes are in the renamed file.
7. AC9.3: set `"ram_budget_mb": 256` in `config.json`, restart, and scroll the whole 7,000-shot card. Memory stays bounded (Task Manager shows the working set roughly stable rather than growing per shot), and tiles reload when scrolled back.
<!-- END_TASK_6 -->
<!-- END_SUBCOMPONENT_C -->
