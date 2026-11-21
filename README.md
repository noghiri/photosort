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
