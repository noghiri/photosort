# photosort MVP Implementation Plan — Phase 7: Lens profiles, rules, and per-shot aperture (stretch)

**Goal:** Decide which lens edit each copied shot will get, and let the user manage lenses and review and adjust edits. Nothing is written to files yet.

**Architecture:**
- Pure lens logic lives in `photosort-core/src/lens.rs`:
  - profile, rule, override and edit types;
  - resolution: override → first matching camera-scoped rule → per-body session manual profile;
  - f-stop scales and stepping;
  - the similar-shots query;
  - usage counts;
  - "draft from focused shot";
  - unconfirmed-aperture detection;
  - the review-queue state machine.
- Config and card state gain lens fields. `plan_copy` attaches a resolved `LensEdit` to every planned file.
- The UI adds:
  - a tabbed Settings window whose first-class **Lenses** tab manages profiles and rules, opened directly from a top-bar **Lenses…** button;
  - quick-add from the focused shot;
  - a per-body session profile selector;
  - the `L` picker;
  - `[`/`]` stepping and an overlay slider;
  - lens badges;
  - the confirmation warning and the review flow.

**Tech Stack:** `uuid` 1.29.0 (`serde` in core; `v4` + `serde` in the app), `serde` 1.0.229, egui/eframe 0.37.0.

**Scope:** 8 phases from original design (this is phase 7 of 8)

**Codebase verified:** 2026-11-21

---

## Acceptance Criteria Coverage

This phase implements and tests:

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

### photosort-mvp.AC12: Body-agnostic extensibility
- **photosort-mvp.AC12.3 Success:** A rule scoped to `NIKON CORPORATION` / `NIKON Z f` doesn't apply to shots from another body with the same reported `LensModel`.
- **photosort-mvp.AC12.4 Success:** In a source containing shots from two bodies, each body's no-lens shots get that body's session manual profile.

**Decisions approved during planning:**
- The override profile is serialised as the string `"none"` or a uuid (hand-written `Deserialize`; no `untagged`).
- Overrides are stored under every file identity of a shot.
- Stepping the aperture on a rule- or session-assigned shot creates an override with that profile and `aperture_confirmed = true`.
- Aperture ranges include the profile's own endpoints.
- `LensProfile.badge` (optional, at most 8 chars) labels tiles (`FD28 f/8`), falling back to the profile name truncated to 12 chars. An unconfirmed aperture is amber with a trailing `?`.
- Lens management is a first-class **Lenses** tab in a tabbed Settings window, with a top-bar **Lenses…** button and **Add lens from focused shot** (in the tab and the `L` picker).
- The reference manual lens for tests and manual checks is the user's **Canon FDn 28mm f/2.8**: 28 mm, f/2.8–22, half stops, default f/8, badge `FD28`.

---

<!-- START_SUBCOMPONENT_A (tasks 1-3) -->
<!-- START_TASK_1 -->
### Task 1: Lens contracts and persisted fields

**Verifies:** photosort-mvp.AC10.1 (fresh config has no profiles; profiles round-trip), photosort-mvp.AC10.2, photosort-mvp.AC10.6 (override persisted in card state)

**Files:**
- Modify: root `Cargo.toml` `[workspace.dependencies]`: `uuid = { version = "=1.29.0", features = ["serde"] }`
- Modify: `crates/photosort-core/Cargo.toml`: add `uuid = { workspace = true }`
- Modify: `crates/photosort/Cargo.toml`: add `uuid = { workspace = true, features = ["v4"] }` (v4 generation is random, so it stays in the shell)
- Create: `crates/photosort-core/src/lens.rs` (`// pattern: Functional Core`)
- Modify: `crates/photosort-core/src/lib.rs`: `pub mod lens;`
- Modify: `crates/photosort-core/src/state.rs`: add fields

**Implementation:**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopStep { Full, Half, Third }

/// EXIF LensInfo: min/max focal length (mm) and max aperture at each.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LensInfo { pub min_focal: f32, pub max_focal: f32, pub max_f_at_min: f32, pub max_f_at_max: f32 }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LensProfileKind {
    Chip,
    Manual { focal_mm: f32, max_f: f32, min_f: f32, step: StopStep, default_f: f32 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LensProfile {
    pub id: Uuid,
    pub name: String,
    /// Short tile label, e.g. "FD28"; at most 8 chars.
    #[serde(default)]
    pub badge: Option<String>,
    pub lens_make: String,
    pub lens_model: String,
    pub lens_info: LensInfo,
    pub kind: LensProfileKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "model", rename_all = "snake_case")]
pub enum LensMatch { ReportedModel(String), NoLens }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraScope { pub make: String, pub model: Option<String> }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LensRule { pub when: LensMatch, pub camera: Option<CameraScope>, pub profile: Uuid }

/// Serialised as the string "none" or a uuid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverrideProfile { None, Profile(Uuid) }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Override { pub profile: OverrideProfile, pub f_number: Option<f32>, pub aperture_confirmed: bool }

#[derive(Debug, Clone, PartialEq)]
pub struct LensEdit { pub profile: Uuid, pub f_number: Option<f32>, pub aperture_confirmed: bool }

/// `"<make>:<serial>"`, with empty parts when absent.
pub fn body_key(meta: &Metadata) -> String;
```

- `OverrideProfile` gets a hand-written `Serialize` (a str) and `Deserialize` (a visitor with `expecting` "\"none\" or a profile uuid").
- Internally tagged enums (`tag = "type"`) are fine; house style forbids only `untagged` and `flatten`.
- `state.rs` additions, all `#[serde(default)]`:
  - `Config.profiles: Vec<LensProfile>`, `Config.rules: Vec<LensRule>`;
  - `CardState.overrides: BTreeMap<IdentityKey, Override>` (serde_json map keys: `IdentityKey` serializes via `serialize_str`, so it is a valid JSON object key);
  - `CardState.session_manual_profiles: BTreeMap<String, Uuid>`, keyed by `body_key`.

  `version` stays 1: nothing has shipped.

**Testing** (unit tests):
- AC10.1: `Config::default().profiles.is_empty()`. A config with a Chip profile and the FDn 28mm Manual profile (badge `FD28`), plus two rules, round-trips through JSON.
- AC10.2: the Manual profile's `focal_mm` 28, `max_f` 2.8, `min_f` 22, `step` Half and `default_f` 8 survive the round-trip.
- AC10.6: a `CardState` with overrides (`None` and `Profile(uuid)`) round-trips. The JSON keys are identity strings, and `profile` is `"none"` or a uuid string. An invalid profile string → a deserialize error.
- JSON without the new fields (including `badge`) loads with defaults.

**Verification:** `cargo test -p photosort-core lens` and `cargo test -p photosort-core state`

**Commit:** `feat(core): add lens profile, rule and override types`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Lens resolution, f-stop logic and usage counts

**Verifies:** photosort-mvp.AC10.3, AC10.4, AC10.5, AC10.6, AC10.7, AC12.3, AC12.4

**Files:**
- Modify: `crates/photosort-core/src/lens.rs`

**Implementation:**

```rust
/// What the camera reported for a shot (from its first file with metadata).
pub struct ReportedLens<'a> { pub make: &'a str, pub model: &'a str, pub body: String,
    pub lens_model: Option<&'a str>, pub focal_length: Option<Rational> }
pub fn reported_lens(shot: &Shot) -> Option<ReportedLens<'_>>;

/// Empty or absent LensModel and FocalLength 0 or absent.
pub fn is_no_lens(r: &ReportedLens) -> bool;
/// Exact string compare; scope checks make, and model when `Some`.
pub fn rule_matches(rule: &LensRule, r: &ReportedLens) -> bool;

pub fn shot_override<'a>(shot: &Shot, overrides: &'a BTreeMap<IdentityKey, Override>) -> Option<&'a Override>;

pub struct LensSettings<'a> { pub profiles: &'a [LensProfile], pub rules: &'a [LensRule],
    pub session: &'a BTreeMap<String, Uuid>, pub overrides: &'a BTreeMap<IdentityKey, Override> }

/// Override → first matching rule (list order) → session manual profile for
/// the body (no-lens shots only). Unknown profile ids resolve to no edit.
pub fn resolve(shot: &Shot, s: &LensSettings) -> Option<LensEdit>;

/// Shots in this scan resolving to each profile (for the Lenses tab).
pub fn usage_counts(shots: &[Shot], s: &LensSettings) -> HashMap<Uuid, usize>;

pub fn scale(step: StopStep) -> &'static [f32];
/// The scale restricted to [max_f, min_f], plus both endpoints.
pub fn stops_for(profile: &LensProfile) -> Vec<f32>;  // empty for Chip
/// `default_f` clamped to [max_f, min_f].
pub fn default_aperture(profile: &LensProfile) -> Option<f32>;
pub enum Dir { Wider, Narrower }
/// Snap `current` to the nearest stop, then move one stop; clamped at the ends.
pub fn step_aperture(profile: &LensProfile, current: f32, dir: Dir) -> Option<f32>;
```

`resolve`, in detail:
- **Content-rung shots get no edit (decided during planning, for AC4.4):** if any file of the shot has `identity: None`, or an `IdentityKey::Content` identity, return `None` before checking anything else. Editing such a copy would change its content key (size + hash of the first 64 KB), so the edited copy would stop matching its source and could be copied again. Files with capture metadata (counter or capture rung) are unaffected; real Z f files use the counter rung.
- **Override:**
  - `None` → no edit;
  - `Profile(id)` → `LensEdit { profile: id, f_number: override.f_number.or(default_aperture(p)), aperture_confirmed: override.aperture_confirmed }`.
- **Rule or session match:** `LensEdit { profile, f_number: default_aperture(p), aperture_confirmed: false }`. The session applies only if `is_no_lens` and the session profile exists and is Manual.
- **Chip profiles** always give `f_number: None` and `aperture_confirmed: true` (nothing to confirm).

Scales (nominal marked values):
- **Full:** 1, 1.4, 2, 2.8, 4, 5.6, 8, 11, 16, 22, 32, 45, 64
- **Half:** 1, 1.2, 1.4, 1.7, 2, 2.4, 2.8, 3.3, 4, 4.8, 5.6, 6.7, 8, 9.5, 11, 13, 16, 19, 22, 27, 32, 38, 45, 54, 64
- **Third:** 1, 1.1, 1.2, 1.4, 1.6, 1.8, 2, 2.2, 2.5, 2.8, 3.2, 3.5, 4, 4.5, 5, 5.6, 6.3, 7.1, 8, 9, 10, 11, 13, 14, 16, 18, 20, 22, 25, 29, 32, 36, 40, 45, 51, 57, 64

Comparisons use an epsilon of 0.01.

Test fixture `fdn28()`: Manual, `focal_mm` 28, `max_f` 2.8, `min_f` 22, Half, `default_f` 8, badge `FD28`, name `Canon FDn 28mm f/2.8`, make `Canon`, model `Canon FDn 28mm f/2.8`, LensInfo 28/28/2.8/2.8.

**Testing** (unit tests, with `Shot`s built from synthetic `Metadata`):
- AC10.3: rule `ReportedModel("VR 80-200mm f/2.8G")` → the profile; `"VR 80-200mm f/2.8G "` (trailing space) doesn't match; scoped `NIKON CORPORATION`/`NIKON Z f` matches a Z f shot.
- AC12.3: the same scoped rule does not match a `NIKON Z 6_2` shot with the same `LensModel`; a scope with `model: None` matches both.
- AC10.4: no-lens shots (`lens_model` empty with focal 0, or both absent) get the body's session profile (`fdn28`); with no session entry → `None`; a lens-reporting shot is unaffected.
- AC12.4: two bodies (different serials), each with its own session profile → each body's no-lens shots get their own.
- AC10.5: an unmatched reported lens → `None`.
- AC4.4 (content rung): a shot whose file identity is `IdentityKey::Content` resolves to `None`, even with an override `Profile(p)`, a matching rule or a session profile. `usage_counts` does not count it.
- AC10.6: `OverrideProfile::None` beats a matching rule; `Profile(p2)` beats rule `p1`; an override stored under only the NEF's identity applies to the J+N shot.
- AC10.7 with `fdn28()`:
  - `stops_for` → `[2.8, 3.3, 4, 4.8, 5.6, 6.7, 8, 9.5, 11, 13, 16, 19, 22]`;
  - `step_aperture(8, Narrower)` → 9.5, `(8, Wider)` → 6.7, `(22, Narrower)` → 22, `(2.8, Wider)` → 2.8;
  - snapping: `step_aperture(7.9, Narrower)` → 9.5;
  - `default_aperture` → 8.
- AC10.7 clamping (synthetic profiles): range 1.2–5.6 → 5.6; range 11–22 → 11.
- A proptest: any `step_aperture` result lies within `[max_f, min_f]`.
- A Chip profile → `f_number: None`. A rule pointing at a deleted profile → `None`.
- `usage_counts`: 3 no-lens shots with session `fdn28` and 2 matching a Chip rule → `{fdn28: 3, chip: 2}`; an override `None` removes a shot from the counts.

**Verification:** `cargo test -p photosort-core lens`

**Commit:** `feat(core): resolve lens edits, step apertures and count profile usage`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: Similar shots, quick-add drafts, unconfirmed detection, review queue, validation

**Verifies:** photosort-mvp.AC10.1 (create/edit validation), photosort-mvp.AC10.9 (detection), photosort-mvp.AC10.10 (review state machine), photosort-mvp.AC10.11

**Files:**
- Modify: `crates/photosort-core/src/lens.rs`

**Implementation:**

```rust
/// Same body (make + serial) and same reported lens (equal LensModel, or both no-lens) as `focused`, in `order`.
pub fn similar_shots(shots: &[Shot], order: &[ShotId], focused: ShotId) -> Vec<ShotId>;

/// Pre-filled profile + rule for "Add lens from focused shot". A shot
/// reporting a lens (chipped adapter) → Chip draft with a ReportedModel rule;
/// a no-lens shot → Manual draft (default_f 8, Half) with a NoLens rule.
/// The rule is scoped to the shot's make and model. The profile's own lens
/// make/model are left empty: what the camera reported is the wrong lens.
pub struct LensDraft { pub kind: LensProfileKind, pub rule: LensMatch, pub camera: Option<CameraScope> }
pub fn draft_from_shot(shot: &Shot) -> Option<LensDraft>;

/// Ticked shots whose resolved edit is Manual with `aperture_confirmed == false`, in order.
pub fn unconfirmed_manual(shots: &[Shot], order: &[ShotId], ticked: &HashSet<ShotId>, s: &LensSettings) -> Vec<ShotId>;

pub struct ReviewQueue { shots: Vec<ShotId>, pos: usize }
pub enum ReviewStep { Next(ShotId), Finished }
impl ReviewQueue {
    pub fn new(shots: Vec<ShotId>) -> Option<Self>; // None if empty
    pub fn current(&self) -> ShotId;
    pub fn confirm_and_advance(&mut self) -> ReviewStep;
    pub fn prev(&mut self) -> ShotId;
    pub fn next(&mut self) -> ShotId;
}

/// name and lens_model non-empty; badge at most 8 chars when set;
/// Manual: focal_mm > 0, 0 < max_f <= min_f, default_f > 0.
pub fn validate_profile(p: &LensProfile) -> Result<(), Vec<&'static str>>;
```

**Testing:**
- AC10.11: among shots from 2 bodies × 2 lenses, `similar_shots` returns exactly the focused group, in order. No-lens shots group only with no-lens shots of the same body.
- `draft_from_shot`:
  - a Z f shot reporting `VR 80-200mm f/2.8G` → Chip, `ReportedModel("VR 80-200mm f/2.8G")`, scope `NIKON CORPORATION`/`NIKON Z f`;
  - a no-lens shot → Manual, `NoLens`, same scope;
  - a shot with no metadata → `None`.
- AC10.9: ticked manual shots, some with confirmed overrides → only the unconfirmed ones, in order. Chip and no-edit shots are excluded.
- AC10.10: a queue of 3 → `Next(b)`, `Next(c)`, `Finished`; `new(vec![])` → `None`; `prev` at 0 stays.
- AC10.1: `validate_profile` accepts `fdn28()` and a Chip profile; it rejects an empty name, a badge `"TOOLONGXX"` (9 chars), `max_f > min_f`, and `focal_mm = 0`.

**Verification:** `cargo test -p photosort-core lens`

**Commit:** `feat(core): add similar shots, quick-add drafts, review queue and validation`
<!-- END_TASK_3 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_TASK_4 -->
### Task 4: Attach lens edits to copy plans

**Verifies:** photosort-mvp.AC10.3–AC10.6 (edits reach the plan)

**Files:**
- Modify: `crates/photosort-core/src/plan.rs`: `PlannedFile` gains `pub lens_edit: Option<LensEdit>`; `plan_copy(ticked, index, lens: &dyn Fn(&Shot) -> Option<LensEdit>)`
- Modify: every existing `plan_copy` caller: the Phase 5 `plan.rs` unit and property tests and the Phase 5 `copy.rs` tests (pass `&|_| None`), and `app.rs` (below)
- Modify: `crates/photosort/src/app.rs`: `plan_copy(.., &|shot| lens::resolve(shot, &settings))`

`PlannedFile`, `PlannedShot` and `CopyPlan` drop `Eq` (they keep `PartialEq`) because `LensEdit` holds an `f32`. `ShotResult` does not contain plan types, so its derives are unaffected; `assert_eq!` on plans keeps working with `PartialEq`.

**Testing:** a resolver returning an edit for one shot → every planned file of that shot carries it, and other shots carry `None`. The Phase 5 property tests still pass.

**Verification:** `cargo test -p photosort-core plan` and `cargo test --workspace`

**Commit:** `feat(core): carry resolved lens edits in copy plans`
<!-- END_TASK_4 -->

<!-- START_SUBCOMPONENT_B (tasks 5-6) -->
<!-- START_TASK_5 -->
### Task 5: Tabbed Settings with a Lenses tab, and quick-add

**Verifies:** photosort-mvp.AC10.1, photosort-mvp.AC10.2, photosort-mvp.AC10.3 (rule creation) (manual in Task 7; logic tested in Tasks 2-3)

**Files:**
- Create: `crates/photosort/src/ui/settings.rs` (Imperative Shell)
- Modify: `crates/photosort/src/ui/top_bar.rs` (**Lenses…** and **Settings…** buttons), `ui/mod.rs`, `app.rs`

**Implementation:**
- `egui::Window` "Settings" with `SettingsTab { Lenses, General }`, selected by `ui.selectable_value` tabs at the top.
- **Lenses…** opens the window on `Lenses`, and **Settings…** opens it on the last-used tab.
- **Lenses tab:**
  - **Profile list.** One row per profile: badge (or `—`), name, kind (`Chip` / `Manual 28 mm f/2.8–22`), usage count in the current scan (`usage_counts`), and the rules targeting it, as indented sub-rows (`VR 80-200mm f/2.8G · NIKON Z f`, `No lens · any body`). The row actions are **Edit**, **Add rule** and **Delete**.
    - Delete confirms inline ("Delete 'X'? N rules and M overrides using it will stop applying." with **Delete**/**Cancel**). egui has no `confirm()`.
  - **Add Chip lens** / **Add Manual lens** open the editor with a draft and `Uuid::new_v4()`. Manual drafts default to `default_f` 8 and `step` Half.
  - **Add lens from focused shot** (disabled without a focused, parsed shot) opens the editor from `draft_from_shot`. It shows the reported lens as read-only context ("Camera reported: VR 80-200mm f/2.8G on NIKON Z f", or "Camera reported: no lens on NIKON Z f"). Saving creates the profile **and** its rule in one step.
  - **Editor form:**
    - name, badge (at most 8 chars), lens make, lens model;
    - LensInfo (four `DragValue`s);
    - for Manual: focal mm, max f, min f, a stop-step `ComboBox`, and default f.

    **Save** is enabled only when `validate_profile` passes, and lists the errors otherwise.
  - **Rule order** (a collapsible section): all rules in evaluation order with ↑/↓ (first match wins) and delete. Each row has a `LensMatch` selector (Reported model + text, or No lens), an optional camera scope (make, plus optional model), and a profile `ComboBox`.
- **General tab:** the ExifTool path (text plus **Browse…** via `rfd::FileDialog::new().add_filter("exiftool", &["exe"]).pick_file()` on a thread), stored in `Config.exiftool_path` for Phase 8. Also a **Cache budget** field: a `DragValue` in MB with a **Use default (25% of RAM)** checkbox, bound to `Config.ram_budget_mb` (AC9.3's configured override). It takes effect on the next start, and the label says so.
- Every change marks the config debounce (Phase 6), so profiles and rules persist only in `config.json` (AC10.1).

**Verification:** `cargo build -p photosort` and `cargo clippy --workspace --all-targets -- -D warnings`

**Commit:** `feat(app): add Lenses settings tab with quick-add from focused shot`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Session profiles, L picker, aperture stepping, badges, confirm warning and review

**Verifies:** photosort-mvp.AC10.4, AC10.6–AC10.11, AC12.4 (manual in Task 7; logic covered in Tasks 2-3)

**Files:**
- Modify: `crates/photosort/src/ui/top_bar.rs`, `ui/grid.rs`, `ui/overlay.rs`, `ui/dialogs.rs`, `app.rs`
- Modify: `crates/photosort/src/ui/format.rs`: add `lens_badge(edit: &LensEdit, profile: &LensProfile) -> (String, bool /*unconfirmed*/)`

**Implementation:**
- **Keyboard gate:** `L`, `[` and `]` use the Phase 3 keyboard gate (only when `!ctx.egui_wants_keyboard_input()` and no window or modal is open), so typing in the Lenses editor never triggers them.
- **Card state wiring (persists AC10.6 and the session lenses):** when the card state arrives (Phase 6 Task 5), merge `overrides` and `session_manual_profiles` into app state. Entries the user made before the load arrived win over loaded ones. Every card save builds `CardState` with the current `overrides` and `session_manual_profiles` as well as `ticked`, so a tick save never drops them. When a scan starts, clear `overrides` and `session_manual_profiles` **after** Phase 6's pre-clear save of the previous card: they belong to that card, and must not count as "entries made before the load arrived" for the next one.
- **Session profile selector (AC10.4, AC12.4):**
  - for each distinct `body_key` among the scanned shots, a top-bar `ComboBox` labelled `<model> …<last 4 of serial>`, with "No session lens" plus every Manual profile (shown as its badge or name);
  - with one body, a single combo;
  - the focused shot's body is highlighted;
  - changes write `CardState.session_manual_profiles` and mark the card debounce.
- **`L` picker (AC10.6, AC10.11):**
  - `L` in the grid (focused shot or range) or the overlay opens a modal: "Default (rules/session)" (removes the override), "None (no edit)", every profile, and **Add lens from focused shot…** (which opens the Lenses tab editor, Task 5);
  - an **Apply to all similar shots** checkbox widens the target to `similar_shots(focused)`;
  - when the focused shot is on the content rung, the picker shows "Lens edits need capture metadata (DateTimeOriginal); this shot can't be edited", and choosing a profile has no effect for such shots (`resolve` ignores them; Task 2);
  - choosing writes an `Override` under every file identity of each target (`f_number: None`, `aperture_confirmed: false`), or removes it for Default. Then mark the card debounce.
- **Aperture (AC10.7, AC10.8):**
  - `[` / `]` (`egui::Key::OpenBracket` / `CloseBracket`) → for each target whose resolved edit is Manual: `step_aperture(profile, current_or_default, Wider/Narrower)`, then write an override with that profile, the new f-number, and `aperture_confirmed: true`;
  - the overlay gets an aperture `Slider` over `stops_for(profile)` indices, which does the same.
- **Tile badge (AC10.8):** bottom-left `lens_badge`, either `FD28 f/8` or, without a badge, the name truncated to 12 chars plus `f/8`. Unconfirmed is amber with a trailing `?` (`FD28 f/8?`); confirmed is white. Chip profiles show only the badge or name.
- **Copy confirmation (AC10.9):** if `unconfirmed_manual(..)` is non-empty, the dialog shows "N manual-lens shots still have the default aperture" with **Copy anyway**, **Cancel** and **Review apertures**.
- **Review (AC10.10):**
  - `Mode::Review { queue: ReviewQueue, .. }` uses the overlay view restricted to the queue;
  - ←/→ move within the queue, and `[`/`]` and the slider adjust;
  - Enter → confirm (`aperture_confirmed = true`) and `confirm_and_advance`;
  - `Finished`, or Esc → re-run `plan_copy` and return to the confirmation dialog.

**Testing:** `lens_badge` unit tests (badge set → `FD28 f/8`; badge `None` → truncated name; unconfirmed → `f/8?` with `true`; Chip → badge only). Everything else is covered by Tasks 2-3 and Task 7.

**Verification:** `cargo build -p photosort`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`

**Commit:** `feat(app): add lens picker, aperture stepping, review flow and lens badges`
<!-- END_TASK_6 -->
<!-- END_SUBCOMPONENT_B -->

<!-- START_TASK_7 -->
### Task 7: Phase verification and manual lens-flow checks

**Verifies:** photosort-mvp.AC10.1–AC10.11, AC12.3, AC12.4 (manual parts; append results to `docs/implementation-plans/2026-11-20-photosort-mvp/manual-test-log.md`)

Automated gate: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`.

Extend `crates/photosort-testkit/examples/gen_card.rs` with two flags:
- `--lens-mix`: some shots have `lens_model: None` and focal 0/1 (no lens), and some report `VR 80-200mm f/2.8G`;
- `--two-bodies`: alternates serial `1234567`/`7654321`, with every 3rd shot of the second body set to model `NIKON Z 6_2`.

Manual:
1. AC10.1 and AC10.2:
   - a fresh `%APPDATA%\photosort` (rename the folder) → the Lenses tab is empty;
   - **Lenses…** opens Settings on the Lenses tab;
   - create Manual "Canon FDn 28mm f/2.8" (badge `FD28`, make `Canon`, model `Canon FDn 28mm f/2.8`, LensInfo 28/28/2.8/2.8, 28 mm, f/2.8–22, Half, default 8) and Chip "Canon EF 80-200mm f/2.8L";
   - edit one and delete a scratch one, then restart → the changes persist, and `config.json` contains them.
2. Quick-add and AC10.3: focus a `VR 80-200mm f/2.8G` shot and choose **Add lens from focused shot** → the editor shows "Camera reported: VR 80-200mm f/2.8G on NIKON Z f". Point the rule at the EF profile → matching tiles show its badge, and the Lenses tab shows its usage count.
3. AC10.4 and AC12.4: with `--two-bodies`, create a second Manual profile (any spec, badge `TEST`), set `FD28` as one body's session lens and `TEST` as the other's → each body's no-lens tiles show their own badge at `f/8?` (amber).
4. AC10.7 and AC10.8: `]` on an FD28 tile → `FD28 f/9.5` in white; `[` repeatedly → stops at `f/2.8`; `]` repeatedly → stops at `f/22`. In the overlay, the aperture slider moves through the half stops f/2.8 … f/22 and sets a confirmed (white) aperture.
5. AC10.6: `L` → None on one shot → the badge disappears, survives restart, and its usage count drops by one.
6. AC10.11: `L` → FD28 + **Apply to all similar shots** → every no-lens shot of that body gets it; the other body's don't.
7. AC10.9 and AC10.10: tick 3 unconfirmed FD28 shots → Copy warns. **Review apertures** steps through exactly those 3, Enter confirms each, and after the last you're back at the confirmation without the warning.
8. AC12.3: scope the VR rule to `NIKON CORPORATION`/`NIKON Z f` → the `NIKON Z 6_2` shots reporting the same lens show no badge.
9. Keyboard gate: in the Lenses editor, type `Canon EF 80-200mm f/2.8L [x]` into the model field → the text appears, no `L` picker opens, and no tile aperture changes.
<!-- END_TASK_7 -->
