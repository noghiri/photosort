# photosort MVP Implementation Plan — Phase 4: Preview overlay

**Goal:** Inspect a shot at screen size and at 100% without leaving the keyboard.

**Architecture:**
- Pure, tested logic:
  - `photosort-core` decides *what* to decode (`Shot::full_source`: the JPEG file, a raw file's embedded `JpgFromRaw`, or the embedded preview as a fallback);
  - the shell's `ui/overlay_math.rs` handles navigation, zoom anchoring and pan clamping;
  - `ui/format.rs` handles EXIF value formatting.
- I/O and decoding: `preview.rs` gains a DCT-scaled full decode (`jpeg-decoder`), and `overlay_loader.rs` runs decodes on two worker threads with current-first priority.
- `ui/overlay.rs` draws the overlay, and `PhotosortApp` switches between `Mode::Grid` and `Mode::Overlay`.

**Tech Stack:** `jpeg-decoder` 0.3.2, `image` 0.25.10 (orientation), egui/eframe 0.37.0.

**Scope:** 8 phases from original design (this is phase 4 of 8)

**Codebase verified:** 2026-11-21

---

## Acceptance Criteria Coverage

This phase implements and tests:

### photosort-mvp.AC3: Preview overlay
- **photosort-mvp.AC3.1 Success:** Opening the overlay (Enter or a click) shows the embedded preview immediately. It's then replaced by a screen-resolution decode of the full file.
- **photosort-mvp.AC3.2 Success:** `Z` or a click toggles 100% zoom centred on the cursor, and dragging pans.
- **photosort-mvp.AC3.3 Success:** ←/→ move to the previous or next shot, skipping shots the hide-copied filter is hiding. Space toggles the tick. Esc returns to the grid with focus on the shot that was showing.
  - *The hide-copied filter itself arrives in Phase 5; this phase navigates through an `is_hidden` predicate that Phase 5 supplies.*
- **photosort-mvp.AC3.4 Success:** On a RAW-only NEF shot, 100% zoom shows the embedded full-size `JpgFromRaw` (6048×4032 on the Z f).
- **photosort-mvp.AC3.5 Success:** The info strip shows the filename(s), shutter speed, aperture, ISO and lens.
- **photosort-mvp.AC3.6 Failure:** If the full-file decode fails, the embedded preview stays visible with an error note, and the app doesn't crash.

### photosort-mvp.AC2: Thumbnail grid and keyboard culling (completion)
- **photosort-mvp.AC2.4 Success:** Clicking the checkbox toggles the tick. Clicking the image opens the overlay.
  - *Completes the image-click half deferred from Phase 3.*

---

## Notes for the implementor

- `jpeg-decoder::Decoder::scale(w, h)` picks the *smallest* factor (1/8, 1/4, 1/2, 1) whose output is ≥ the requested size in at least one axis (📖 docs.rs jpeg-decoder 0.3.2). A 6048×4032 file therefore decodes to 3024×2016 for a 1080p/1440p window, and the GPU downsamples the rest.
- A full 6048×4032 RGBA texture is about 97 MB. Only the current shot is ever held at full resolution; overlay textures live in their own small map, separate from the Phase 3 tile cache.
- Full decodes run on a dedicated two-thread `OverlayLoader`, not the scan pool, so they never queue behind thousands of parse jobs.

<!-- START_SUBCOMPONENT_A (tasks 1-2) -->
<!-- START_TASK_1 -->
### Task 1: Decode-source selection (`photosort-core`)

**Verifies:** photosort-mvp.AC3.4 (selection logic), photosort-mvp.AC3.1 (fallback to the embedded preview)

**Files:**
- Modify: `crates/photosort-core/src/grouping.rs`: add `FullSource` and `Shot::full_source`

**Implementation** (`// pattern: Functional Core` already on the file):

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FullSource {
    /// Decode the whole non-raw file (a JPEG).
    WholeFile { file: usize },
    /// Decode an embedded full-size JPEG inside a raw file (NEF JpgFromRaw).
    Embedded { file: usize, range: ByteRange },
    /// Nothing larger exists; the embedded preview is the best image.
    PreviewOnly { file: usize, range: ByteRange },
}

impl Shot {
    /// Best source for the overlay's full-resolution image, in order:
    /// the first non-raw file, then a raw file's `full_jpeg`, then
    /// `tile_preview`. `None` when the shot has none of these.
    pub fn full_source(&self, registry: &Registry) -> Option<FullSource>;
}
```

A non-raw file qualifies only if it parsed without error (`error.is_none()` and `meta.is_some()`). In the MVP the only non-raw format is JPEG.

**Testing** (unit tests in `grouping.rs`):
- A JPEG-only shot → `WholeFile`.
- J+N → `WholeFile` pointing at the JPEG.
- NEF-only with `full_jpeg` → `Embedded` with that range (AC3.4).
- NEF-only without `full_jpeg` but with a preview → `PreviewOnly`.
- A JPEG whose parse failed, paired with a NEF that has `full_jpeg` → `Embedded`.
- No previews and no full JPEG → `None`.

**Verification:** `cargo test -p photosort-core grouping`

**Commit:** `feat(core): choose overlay decode source per shot`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: DCT-scaled full decode (`preview.rs`)

**Verifies:** photosort-mvp.AC3.1 (screen-resolution decode), photosort-mvp.AC3.4 (full-size decode), photosort-mvp.AC3.6 (decode errors are values)

**Files:**
- Modify: root `Cargo.toml` `[workspace.dependencies]`: `jpeg-decoder = "=0.3.2"`
- Modify: `crates/photosort/Cargo.toml`: add `jpeg-decoder = { workspace = true }`
- Modify: `crates/photosort/src/preview.rs`

**Implementation:**

```rust
/// Decodes a JPEG, DCT-downscaled so the result is at least `target`
/// (after orientation) in one axis, or at full size when `target` is
/// `None`. Applies EXIF orientation.
pub(crate) fn decode_full(bytes: &[u8], target: Option<[u32; 2]>, orientation: Option<u16>)
    -> Result<DecodedImage, PreviewError>;

pub(crate) fn load_full(path: &Path, source: &FullSource, target: Option<[u32; 2]>,
    orientation: Option<u16>) -> Result<DecodedImage, PreviewError>;
```

`decode_full`:
1. `jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes))`, then `read_info()`. Then read `info()` and return `Err(PreviewError::Decode(image::ImageError::Limits(image::error::LimitErrorKind::DimensionError.into())))` if its width or height is greater than `DECODE_MAX_SIDE` (16,384). That constant is private to `preview.rs`, and `decode_full` lives in the same file, so it can use it directly. This check is the only dimension limit on this path. Do not rely on `jpeg-decoder` to enforce one, and do not rely on `decode_limits()`, which `decode_full` does not use.
2. If `target` is set, swap its axes for orientations 5-8 (they rotate 90°), then call `decoder.scale(w, h)` with values clamped to `u16`.
3. `decode()`, then convert by `info().pixel_format`:
   - `RGB24` → RGB;
   - `L8` → grey;
   - `L16` → take the high bytes;
   - `CMYK32` → `R = 255 - min(255, C + K)` and so on (inverted Adobe CMYK, as `jpeg-decoder` emits it).
4. Build an `image::RgbImage` (or `GrayImage`), wrap it in `DynamicImage`, and apply orientation as `decode_preview` does. Then `to_rgba8()`.

`load_full` handles each `FullSource`:
- `WholeFile` → `std::fs::read`;
- `Embedded` and `PreviewOnly` → `read_range`.

It then calls `decode_full`. A `PreviewOnly` source ignores `target` (it is already small).

**Testing** (unit tests, with `testkit::tiny_jpeg(1200, 800)` and a testkit NEF):
- `target = Some([300, 200])` → 300×200 (1/4 scale).
- `Some([301, 201])` → 600×400 (1/4 would give 300×200, which meets neither axis, so 1/2 is chosen).
- `None` → 1200×800.
- Orientation 6 with target `[200, 300]` → 200×300.
- AC3.4: `load_full` on a NEF-only temp file with `Embedded` → the JpgFromRaw size (1200×800 in the fixture).
- AC3.6: corrupt bytes, or a range past EOF → `Err`, no panic.
- `tiny_jpeg(DECODE_MAX_SIDE + 1, 8)` → `Err` from `decode_full` before any pixel decoding. The image is 8 px high (about 16k × 8 pixels), so the test stays cheap.
- AC3.6: a `tiny_jpeg` whose SOS marker (`FF DA`) is overwritten with `FF D9` → `decode_full` returns `Err` (the same corruption `gen_card` uses for `BRKN_0001.JPG` in Task 6).

**Verification:** `cargo test -p photosort preview`

**Commit:** `feat(app): add DCT-scaled full-image decode`
<!-- END_TASK_2 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 3-4) -->
<!-- START_TASK_3 -->
### Task 3: Overlay logic and EXIF formatting (`ui/overlay_math.rs`, `ui/format.rs`)

**Verifies:** photosort-mvp.AC3.2 (zoom anchoring and pan), photosort-mvp.AC3.3 (navigation skipping hidden shots), photosort-mvp.AC3.5 (formatting)

**Files:**
- Create: `crates/photosort/src/ui/overlay_math.rs` (Functional Core)
- Create: `crates/photosort/src/ui/format.rs` (Functional Core)
- Modify: `crates/photosort/src/ui/mod.rs`

**Implementation:**

`overlay_math.rs` (sizes in egui points; image sizes in pixels; `ppp` = pixels per point):

```rust
pub(crate) enum Step { Prev, Next }
/// Next/previous shot in `order` from `current`, skipping `is_hidden`; `None` at the ends.
pub(crate) fn neighbor(order: &[ShotId], current: ShotId, step: Step, is_hidden: impl Fn(ShotId) -> bool) -> Option<ShotId>;
/// Up to `n` shots on each side, nearest first, skipping hidden ones (prefetch order).
pub(crate) fn prefetch_set(order: &[ShotId], current: ShotId, n: usize, is_hidden: impl Fn(ShotId) -> bool) -> Vec<ShotId>;

/// Letterboxed rect for fitting `image_px` into `viewport`.
pub(crate) fn fit_rect(image_px: [f32; 2], viewport: Rect) -> Rect;

/// 100% view: `center_px` is the image pixel at the viewport centre.
#[derive(Clone, Copy, Debug, PartialEq)] pub(crate) struct Zoom { pub center_px: Vec2 }
/// Entering 100% from fit: the image pixel under `cursor` stays under `cursor`.
pub(crate) fn zoom_at_cursor(image_px: [f32; 2], viewport: Rect, cursor: Pos2, ppp: f32) -> Zoom;
/// Dragging by `delta` points moves the image with the pointer; clamped so the image keeps covering the viewport where possible.
pub(crate) fn pan(z: Zoom, delta: Vec2, image_px: [f32; 2], viewport: Rect, ppp: f32) -> Zoom;
/// On-screen rect and UV sub-rect to paint at 100%.
pub(crate) fn zoomed_paint(z: Zoom, image_px: [f32; 2], viewport: Rect, ppp: f32) -> (Rect, Rect);
```

`format.rs`:
- `format_exposure(Rational) -> String`: under 1 s as `1/N s` (e.g. `1/250 s`); otherwise decimal seconds with at most one decimal (`2 s`, `1.3 s`).
- `format_f_number(Rational) -> String`: `f/2.8`, `f/8`.
- `format_focal(Rational) -> String`: `50 mm`.
- `info_line(shot: &Shot) -> String`: the file names (`DSC_0001.JPG + DSC_0001.NEF`), exposure, aperture, `ISO 800` and the lens model, joined with ` · `, omitting missing parts.

**Testing** (unit tests):
- AC3.3:
  - `neighbor` skips hidden shots in both directions and returns `None` at the ends;
  - `prefetch_set` with n=2 returns the nearest unhidden neighbours, alternating sides.
- AC3.2:
  - `zoom_at_cursor` then `zoomed_paint` maps the image pixel that was under the cursor in fit view back to the cursor position (within 0.5 px);
  - `pan` by (+10, 0) moves `center_px` left by `10 * ppp` px;
  - the result is clamped at the image edges;
  - UVs always lie within 0..=1.
- AC3.5: `1/250` → `1/250 s`, `10/2500` → `1/250 s`, `2/1` → `2 s`, `13/10` → `1.3 s`, `28/10` → `f/2.8`, `8/1` → `f/8`. `info_line` contains both file names, `ISO 800` and the lens.

**Verification:** `cargo test -p photosort overlay_math` and `cargo test -p photosort format`

**Commit:** `feat(app): add overlay navigation, zoom math and EXIF formatting`
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: Overlay loader (`overlay_loader.rs`)

**Verifies:** photosort-mvp.AC3.1 (screen-res decode replaces the preview), photosort-mvp.AC3.6 (failures reported, not panicked)

**Files:**
- Create: `crates/photosort/src/overlay_loader.rs` (Imperative Shell)
- Modify: `crates/photosort/src/main.rs`: `mod overlay_loader;`

**Implementation:**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Detail { Fit([u32; 2]), Full }

pub(crate) struct LoadRequest { pub shot: ShotId, pub detail: Detail, pub path: PathBuf,
    pub source: FullSource, pub orientation: Option<u16> }

pub(crate) struct LoadResult { pub shot: ShotId, pub detail: Detail, pub image: Result<DecodedImage, String> }
```

`OverlayLoader::start(waker) -> (OverlayLoader, Receiver<LoadResult>)`:
- Two threads.
- `set_wanted(Vec<LoadRequest>)` replaces the pending list. The order is the priority: current Full (if zoomed), current Fit, then the ±2 neighbours' Fit. Already-running jobs finish, and the app drops their results if they are no longer wanted.
- Each job calls `preview::load_full` and sends a `LoadResult`, then calls `waker()`.
- `std::sync::mpsc::sync_channel(16)`.
- `Drop` sets a stop flag and notifies the threads without joining (a decode in flight must not block the UI thread). A send error (receiver dropped) also stops them.

**Testing** (unit tests, with a temp dir):
- Requesting a JPEG at Fit `[300, 200]` → one `LoadResult` of 300×200.
- A request for a corrupt file → `image: Err(..)`, and the loader keeps serving later requests (AC3.6).
- `set_wanted` with a new list before the old jobs start → only the new shots are delivered. Collect with `recv_timeout` until the expected set arrives, and assert no unexpected shot ids.

**Verification:** `cargo test -p photosort overlay_loader`

**Commit:** `feat(app): add prioritized overlay decode loader`
<!-- END_TASK_4 -->
<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 5-6) -->
<!-- START_TASK_5 -->
### Task 5: Overlay UI and mode switching (`ui/overlay.rs`, `app.rs`)

**Verifies:** photosort-mvp.AC2.4 (image click opens overlay), photosort-mvp.AC3.1–AC3.6 (manual verification in Task 7)

**Files:**
- Create: `crates/photosort/src/ui/overlay.rs` (Imperative Shell)
- Modify: `crates/photosort/src/app.rs`
- Modify: `crates/photosort/src/ui/mod.rs`

**Implementation:**

App state:
- `mode: Mode`, where `enum Mode { Grid, Overlay(OverlayState) }`;
- `struct OverlayState { shot: ShotId, zoom: Option<Zoom>, error: Option<String> }`;
- `loader: OverlayLoader`, `loads: Receiver<LoadResult>`;
- `overlay_textures: HashMap<(ShotId, Detail), egui::TextureHandle>`.
  - After each navigation, keep only keys in the current wanted set (the current shot's Fit and Full, and the neighbours' Fit), so at most 5 Fit textures and 1 Full are held.
- `fn is_hidden(&self, ShotId) -> bool { false }` (Phase 5 replaces its body).

Scan start: when a scan starts (Phase 3 Task 8 step 1), also call `loader.set_wanted(vec![])`, set `mode = Mode::Grid` and clear `overlay_textures`. The overlay must never hold a `ShotId` from a previous scan; the top bar's Rescan and Source… buttons stay usable while the overlay is open.

Opening:
- Enter, or `GridAction::Open(shot)` from a tile click → `Mode::Overlay(OverlayState { shot, zoom: None, error: None })`, then `request_loads()`.
- `request_loads()`:
  - computes `target = viewport_size_points * ctx.pixels_per_point()` (as u32), with `prefetch_set(order, shot, 2, is_hidden)`;
  - builds `LoadRequest`s from `shot.full_source(&registry)`. A shot with `None` gets no request, and the overlay shows the filename placeholder;
  - calls `loader.set_wanted`. When zoomed, `Detail::Full` for the current shot comes first.

`logic`:
- In every mode, drain `loads` so its bounded channel never fills, and drop results that are no longer wanted. In `Mode::Overlay`:
  - an `Ok` → upload a texture, at most 2 per frame (`ctx.load_texture`, `TextureOptions::LINEAR`);
  - an `Err` for the current shot → `state.error = Some(msg)`.
- Keys (`consume_key`) (same keyboard gate as Phase 3 Task 8: only when `!ctx.egui_wants_keyboard_input()` and no window or modal is open), with grid keys inactive in this mode:
  - ←/→ → `neighbor(..)` → change shot, reset zoom and error, `request_loads()`;
  - Space → `toggle_ticks` on `[shot]`;
  - `Z` → toggle zoom: entering uses `zoom_at_cursor` with the current pointer position (`ctx.input(|i| i.pointer.hover_pos())`), or the viewport centre when there's no pointer, then `request_loads()`;
  - Esc → `loader.set_wanted(vec![])`, `Mode::Grid`, `selection = Selection { focus: pos, anchor: None }`, where `pos` is the shot's position in the grid's display order (`visible_order` once Phase 5 adds it), and `scroll_to = reveal_row(..)` (AC3.3).

`overlay::show(ui, state, …)` replaces the grid in `CentralPanel`:
- Black background.
- The image, best available first:
  - when zoomed and present, the `Full` texture, drawn with `zoomed_paint`;
  - else the `Fit` texture via `fit_rect`;
  - else the tile texture from `textures` (the embedded preview, AC3.1 "immediately");
  - else, request the tile preview through the scanner (as the grid does) and show the name.
- Response `Sense::click_and_drag()` on the image area:
  - a click (not a drag) toggles zoom at the click position;
  - a drag while zoomed → `pan(zoom, response.drag_delta(), ..)`.
- Bottom info strip in `egui::Panel::bottom("overlay_info").show(ui, …)`: `info_line(shot)`, a tick indicator, and position `123 / 7000`. When `state.error` is `Some`, a red note "full image failed to load: …" appears while the embedded preview stays (AC3.6).

**Testing:** decisions are covered by Tasks 1-4; drawing and interaction are verified manually in Task 7.

**Verification:** `cargo build -p photosort` and `cargo clippy --workspace --all-targets -- -D warnings`

**Commit:** `feat(app): add preview overlay with zoom, pan and keyboard navigation`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Generator fixtures for overlay checks and phase verification

**Verifies:** None (tooling + gate).

**Files:**
- Modify: `crates/photosort-testkit/examples/gen_card.rs`

**Implementation:** add three manual-test shots in `DCIM/199NZ_F`:
- `RAW_0001.NEF`: NEF-only, with a 640×424 preview and a 3000×2000 `tiny_jpeg` JpgFromRaw.
- `BRKN_0001.JPG`: a valid EXIF+MPF preview, but the primary image's SOS marker (`FF DA`, the first one after the APP segments) is overwritten with `FF D9` (EOI). The full decode fails (unit-tested in Task 2), while the header parse and the MPF preview still work.
- `PORT_0001.JPG`: orientation 6 (portrait).

**Verification:** run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings` and `cargo test --workspace --locked`; all succeed.

**Commit:** `test(testkit): add overlay edge cases to card generator`
<!-- END_TASK_6 -->
<!-- END_SUBCOMPONENT_C -->

<!-- START_TASK_7 -->
### Task 7: Manual overlay checks

**Verifies:** photosort-mvp.AC2.4, photosort-mvp.AC3.1–AC3.6 (manual; append results to `docs/implementation-plans/2026-11-20-photosort-mvp/manual-test-log.md`)

On a generated card (`cargo run -p photosort-testkit --example gen_card --release -- $env:TEMP\ps-card-small 200`):
1. AC2.4/AC3.1: click a tile image, or press Enter on a focused tile → the overlay shows the tile preview at once, then sharpens when the screen-res decode lands.
2. AC3.2:
   - `Z` with the pointer over a detail → that detail stays under the pointer at 100%;
   - dragging pans;
   - `Z` or a click returns to fit;
   - a click toggles zoom at the click point.
3. AC3.3:
   - ←/→ step through shots;
   - Space ticks, and the grid shows the tick after Esc;
   - Esc returns to the grid with the ring on the last shown shot, scrolled into view.
4. AC3.4: on `RAW_0001.NEF`, zoom to 100% → the image is 3000×2000 (synthetic). On a real Z f RAW-only NEF (when the user provides samples): 6048×4032.
5. AC3.5: the info strip shows the file names, shutter speed, aperture, ISO and lens.
6. AC3.6: on `BRKN_0001.JPG`, the preview stays and an error note appears. The app keeps working.
7. `PORT_0001.JPG` displays upright in both the tile and the overlay.
<!-- END_TASK_7 -->
