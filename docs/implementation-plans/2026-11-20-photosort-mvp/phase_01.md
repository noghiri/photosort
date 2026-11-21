# photosort MVP Implementation Plan — Phase 1: Workspace scaffold and CI

**Goal:** A buildable Cargo workspace that opens an empty photosort window, checked by CI on `windows-latest`.

**Architecture:** Two-crate workspace. `crates/photosort-core` is the pure library (empty in this phase, `#![forbid(unsafe_code)]`). `crates/photosort` is the eframe binary. Shared dependency versions and lints live in the root `[workspace.dependencies]` and `[workspace.lints]` tables, and every crate opts in with `workspace = true`.

**Tech Stack:** Rust 1.99.1 (pinned by `rust-toolchain.toml`), eframe/egui 0.37.0 (default features, wgpu renderer), GitHub Actions.

**Scope:** 8 phases from original design (this is phase 1 of 8)

**Codebase verified:** 2026-11-20

---

## Acceptance Criteria Coverage

This phase is infrastructure. It implements and tests no acceptance criteria.

**Verifies: None.** Done when `cargo build` and `cargo test` succeed locally, the window opens, and CI passes on the first push.

---

## Conventions established in this phase (apply to all later phases)

- Dependency versions are pinned exactly (`"=x.y.z"`) in root `[workspace.dependencies]`; member crates use `{ workspace = true }`.
- Every member crate has `[lints] workspace = true`.
- Every source file with runtime behaviour starts with a `// pattern: Functional Core` or `// pattern: Imperative Shell` comment, after the `//!` module doc. Type-only files, re-export-only `mod.rs` files and tests are exempt.
- `photosort-core` never depends on filesystem, thread, environment or UI APIs.
- `pub(crate)` by default in the `photosort` binary crate.

<!-- START_SUBCOMPONENT_A (tasks 1-4) -->
<!-- START_TASK_1 -->
### Task 1: Workspace manifest and toolchain pin

**Verifies:** None (infrastructure).

**Files:**
- Create: `Cargo.toml`
- Create: `rust-toolchain.toml`

**Step 1: Create `Cargo.toml`**

```toml
[workspace]
resolver = "3"
members = ["crates/photosort-core", "crates/photosort"]

[workspace.package]
version = "0.1.0"
edition = "2024"
rust-version = "1.99"
repository = "https://github.com/noghiri/photosort"
publish = false

[workspace.dependencies]
photosort-core = { path = "crates/photosort-core" }
eframe = "=0.37.0"

[workspace.lints.clippy]
# House style: `push_str(&format!(..))` allocates needlessly; use `write!`.
format_push_string = "warn"
```

**Step 2: Create `rust-toolchain.toml`**

```toml
[toolchain]
channel = "1.99.1"
components = ["clippy", "rustfmt"]
profile = "minimal"
```

No commit yet; the workspace does not build until Tasks 2-3 add the members.
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: `photosort-core` library skeleton

**Verifies:** None (infrastructure).

**Files:**
- Create: `crates/photosort-core/Cargo.toml`
- Create: `crates/photosort-core/src/lib.rs`

**Step 1: Create `crates/photosort-core/Cargo.toml`**

```toml
[package]
name = "photosort-core"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
repository.workspace = true
publish.workspace = true

[lints]
workspace = true

[dependencies]
```

**Step 2: Create `crates/photosort-core/src/lib.rs`**

```rust
//! Pure logic for photosort: metadata parsing, shot grouping, identity,
//! copy planning, lens rules and persisted state types.
//!
//! This crate performs no filesystem, thread, environment or UI work.
//! Callers pass bytes and data in and get data out, which keeps every
//! decision testable without a card, a disk or a GPU.
#![forbid(unsafe_code)]
```

(No pattern comment: the file has no runtime behaviour yet. Phase 2 adds modules.)
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: `photosort` app crate that opens an empty window

**Verifies:** None (infrastructure).

**Files:**
- Create: `crates/photosort/Cargo.toml`
- Create: `crates/photosort/src/main.rs`
- Create: `crates/photosort/src/app.rs`

**Step 1: Create `crates/photosort/Cargo.toml`**

```toml
[package]
name = "photosort"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
repository.workspace = true
publish.workspace = true

[lints]
workspace = true

[dependencies]
photosort-core = { workspace = true }
eframe = { workspace = true }
```

**Step 2: Create `crates/photosort/src/main.rs`**

```rust
//! photosort desktop app entry point.
// pattern: Imperative Shell
// Release builds are GUI-only; debug builds keep the console for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("photosort")
            .with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "photosort",
        options,
        Box::new(|_cc| Ok(Box::new(app::PhotosortApp))),
    )
}
```

**Step 3: Create `crates/photosort/src/app.rs`**

eframe 0.37 replaced `App::update` with a required `ui` method (plus an optional `logic`). Panels take a `&mut Ui`, and `show` replaces the deprecated `show_inside`. Clippy rejects `PhotosortApp::default()` on a unit struct (`default_constructed_unit_structs`), which is why `main.rs` uses `Box::new(app::PhotosortApp)`.

```rust
//! Top-level egui application.
// pattern: Imperative Shell

use eframe::egui;

pub(crate) struct PhotosortApp;

impl eframe::App for PhotosortApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("photosort");
        });
    }
}
```
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: Verify the scaffold and commit

**Verifies:** None (infrastructure).

**Step 1: Build and check**

Run: `cargo build --workspace`
Expected: `Finished` with no warnings. The first build compiles eframe/wgpu and takes about a minute. If rustup reports that toolchain 1.99.1 is missing, it installs it automatically from `rust-toolchain.toml`.

Run: `cargo test --workspace`
Expected: succeeds, running 0 tests in each crate.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no errors.

Run: `cargo fmt --all --check`
Expected: no output, exit code 0. If it fails, run `cargo fmt --all` and re-check.

**Step 2: Open the window**

Run: `cargo run -p photosort`
Expected: a window titled "photosort", about 1280×800, showing the heading "photosort". Close it; the process exits with code 0.

**Step 3: Commit** (`Cargo.lock` is committed because `photosort` is a binary)

```bash
git add Cargo.toml Cargo.lock rust-toolchain.toml crates/
git commit -m "chore: scaffold photosort workspace with empty eframe window"
```
<!-- END_TASK_4 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_TASK_5 -->
### Task 5: CI workflow on windows-latest

**Verifies:** None (infrastructure).

**Files:**
- Create: `.github/workflows/ci.yml`

**Step 1: Create the workflow**

Third-party actions are pinned to commit SHAs (tag in the trailing comment) so that a moved tag cannot change what runs. `--locked` makes CI fail if `Cargo.lock` is stale.

```yaml
name: CI

on:
  push:
    branches: ["**"]
  pull_request:

permissions:
  contents: read

concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: true

jobs:
  check:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
      - uses: dtolnay/rust-toolchain@263313d2989f864a2b8613f97277288638e7d8f8 # 1.99.1
        with:
          components: clippy, rustfmt
      - uses: Swatinem/rust-cache@d1527c8f67ef9af357290475d4af4b0fab8f12e0 # v2.9.3
      - name: Format
        run: cargo fmt --all --check
      - name: Clippy
        run: cargo clippy --workspace --all-targets --locked -- -D warnings
      - name: Test
        run: cargo test --workspace --locked
```

The workflow syntax is validated by GitHub on the push in Task 7; a YAML error shows as a failed run with a parse message.

**Step 2: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add fmt, clippy and test workflow on windows-latest"
```
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: README with build instructions

**Verifies:** None (infrastructure).

**Files:**
- Create: `README.md`

**Step 1: Create `README.md`**

````markdown
# photosort

A Windows desktop app for culling photos on a camera's SD card and copying the keepers to another folder. The card is never modified.

Status: early development. See `docs/design-plans/2026-11-20-photosort-mvp.md`.

## Requirements

- Windows 10 or 11 (x64).
- [rustup](https://rustup.rs/). The pinned toolchain (`rust-toolchain.toml`, Rust 1.99.1 with clippy and rustfmt) installs automatically on the first `cargo` command.
- Visual Studio 2022 Build Tools with the "Desktop development with C++" workload (MSVC linker and Windows SDK).

## Build and run

```powershell
cargo run -p photosort            # debug build, opens the app
cargo build -p photosort --release
```

## Checks

These are the same checks CI runs:

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

## Layout

- `crates/photosort-core`: pure logic (parsing, identity, grouping, copy planning, lens rules, state types). No I/O.
- `crates/photosort`: the app (filesystem, worker threads, UI).

## Test data

Only synthetic fixtures are committed. Real camera files carry serial numbers and personal images, so keep them in the git-ignored `samples/` folder or outside the repo.
````

**Step 2: Commit**

```bash
git add README.md
git commit -m "docs: add README with build instructions"
```
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: First push and CI confirmation

**Verifies:** None (infrastructure).

The repository `noghiri/photosort` is **public**. Pushing is outward-facing, so get the user's confirmation before running Step 1.

**Step 1: Push the branch**

```bash
git push -u origin photosort-mvp
```

**Step 2: Watch CI**

Run: `gh run list --branch photosort-mvp --limit 1`, then `gh run watch <run-id> --exit-status`
Expected: the `CI / check` job succeeds (Format, Clippy and Test steps all green). If it fails, read the log with `gh run view <run-id> --log-failed`, fix the cause, commit and push again. Do not weaken the checks.
<!-- END_TASK_7 -->
