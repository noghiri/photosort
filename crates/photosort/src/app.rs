//! Top-level egui application: owns the scan, the caches and the UI state,
//! and applies scan events and key presses once per frame.
// pattern: Imperative Shell

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use eframe::egui;
use photosort_core::grouping::capture_order;
use photosort_core::metadata::Registry;
use photosort_core::model::{Shot, ShotId};

use crate::budget::{CacheSplit, cache_budget_bytes, split};
use crate::cache::{PreviewBytes, Textures, physical_ram, preview_bytes, textures};
use crate::frame_log::{self, FrameLog};
use crate::preview::{DecodedImage, to_color_image};
use crate::scan::{ScanEvent, Scanner, Waker};
use crate::scan_model::{
    Phase, StatusCounts, apply_file_outcome, position_after_reorder, shot_at_mut,
};
use crate::ui::grid::{self, CAPTION, SPACING};
use crate::ui::grid_math::{
    GridLayout, Move, Selection, apply_move, layout, reveal_row, selected_range, toggle_ticks,
    zoom_tile,
};
use crate::ui::top_bar::{self, TopBarAction};

/// Events applied per frame. Past this, the rest wait for the next frame.
const EVENTS_PER_FRAME: usize = 5000;
/// Textures uploaded per frame, which bounds the per-frame upload cost.
const UPLOADS_PER_FRAME: usize = 8;
const DEFAULT_TILE: f32 = 200.0;
/// Used when the OS does not report physical RAM.
const FALLBACK_RAM: u64 = 8 * 1024 * 1024 * 1024;
/// Width assumed for the first frame, before the grid reports its own.
const FIRST_FRAME_WIDTH: f32 = 1000.0;

/// What a grid interaction asks for. Phase 4 opens the overlay for `Open`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GridAction {
    Open(ShotId),
}

pub(crate) struct PhotosortApp {
    pub(crate) registry: Arc<Registry>,
    pub(crate) generation: u64,
    pub(crate) source: Option<PathBuf>,
    pub(crate) scanner: Option<Scanner>,
    pub(crate) events: Option<Receiver<ScanEvent>>,
    pub(crate) shots: Vec<Shot>,
    pub(crate) order: Vec<ShotId>,
    pub(crate) ticks: HashSet<ShotId>,
    pub(crate) selection: Selection,
    pub(crate) tile_size: f32,
    pub(crate) cache_split: CacheSplit,
    pub(crate) bytes: Arc<PreviewBytes>,
    pub(crate) textures: Textures,
    pub(crate) in_flight: HashSet<ShotId>,
    pub(crate) pending_images: VecDeque<(ShotId, DecodedImage)>,
    /// Preview failures by shot, with the decoder's message. Such a shot is
    /// not requested again, which would otherwise retry every frame.
    pub(crate) failed_previews: HashMap<ShotId, String>,
    pub(crate) folder_pick: Option<Receiver<Option<PathBuf>>>,
    pub(crate) phase: Phase,
    pub(crate) status: StatusCounts,
    pub(crate) scroll_to: Option<f32>,
    /// Scroll offset and viewport height from the last frame.
    pub(crate) last_viewport: (f32, f32),
    /// The grid's layout from the last frame, used to keep focus in view.
    pub(crate) grid_layout: GridLayout,
    pub(crate) waker: Waker,
    pub(crate) frame_log: Option<FrameLog>,
    /// Set by each drawn `ui` pass and read by the next `logic` call. `logic`
    /// also runs while the window is hidden, and those frames are not logged.
    pub(crate) painted: bool,
}

impl PhotosortApp {
    pub(crate) fn new(cc: &eframe::CreationContext<'_>, frame_log: Option<FrameLog>) -> Self {
        // Ctrl+scroll is ours, and egui's own Ctrl+/- zoom would fight it.
        cc.egui_ctx
            .options_mut(|options| options.zoom_with_keyboard = false);
        let ctx = cc.egui_ctx.clone();
        let waker: Waker = Arc::new(move || ctx.request_repaint());
        // The override arrives with the config in Phase 6.
        let ram = physical_ram().unwrap_or(FALLBACK_RAM);
        Self::build(waker, frame_log, ram)
    }

    fn build(waker: Waker, frame_log: Option<FrameLog>, ram: u64) -> Self {
        let cache_split = split(cache_budget_bytes(ram, None));
        Self {
            registry: Arc::new(Registry::with_builtin()),
            generation: 0,
            source: None,
            scanner: None,
            events: None,
            shots: Vec::new(),
            order: Vec::new(),
            ticks: HashSet::new(),
            selection: Selection::default(),
            tile_size: DEFAULT_TILE,
            cache_split,
            bytes: Arc::new(preview_bytes(&cache_split)),
            textures: textures(&cache_split),
            in_flight: HashSet::new(),
            pending_images: VecDeque::new(),
            failed_previews: HashMap::new(),
            folder_pick: None,
            phase: Phase::Idle,
            status: StatusCounts::default(),
            scroll_to: None,
            last_viewport: (0.0, 0.0),
            grid_layout: layout(FIRST_FRAME_WIDTH, DEFAULT_TILE, SPACING, CAPTION),
            waker,
            frame_log,
            painted: false,
        }
    }

    /// Starts a scan of `root`, dropping the previous one. Nothing from the
    /// old scan survives, so no old `ShotId` can refer to the new shots.
    pub(crate) fn start_scan(&mut self, root: PathBuf) {
        self.generation += 1;
        // Drop the receiver before the scanner, so the old threads find their
        // sends failing and exit.
        self.events = None;
        self.scanner = None;
        self.bytes = Arc::new(preview_bytes(&self.cache_split));
        self.textures.clear();
        self.shots.clear();
        self.order.clear();
        self.ticks.clear();
        self.in_flight.clear();
        self.pending_images.clear();
        self.failed_previews.clear();
        self.selection = Selection::default();
        self.scroll_to = Some(0.0);
        self.status = StatusCounts::default();
        self.phase = Phase::Listing;
        let (scanner, events) = Scanner::start(
            root.clone(),
            Arc::clone(&self.registry),
            Arc::clone(&self.bytes),
            Arc::clone(&self.waker),
            self.generation,
        );
        self.scanner = Some(scanner);
        self.events = Some(events);
        self.source = Some(root);
    }

    /// Opens the folder dialog on a thread, since the dialog blocks.
    fn spawn_folder_pick(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let picked = rfd::FileDialog::new().pick_folder();
            // The receiver is gone only when the app has closed, so the result
            // can be dropped then.
            let _ = tx.send(picked);
            ctx.request_repaint();
        });
        self.folder_pick = Some(rx);
    }

    fn drain_folder_pick(&mut self) {
        let Some(rx) = &self.folder_pick else {
            return;
        };
        match rx.try_recv() {
            Ok(Some(root)) => {
                self.folder_pick = None;
                self.start_scan(root);
            }
            Ok(None) | Err(TryRecvError::Disconnected) => self.folder_pick = None,
            Err(TryRecvError::Empty) => {}
        }
    }

    /// Applies up to `EVENTS_PER_FRAME` events. Returns how many it applied
    /// and whether the limit was reached, so more may be waiting.
    fn drain_events(&mut self) -> (usize, bool) {
        let mut applied = 0;
        while applied < EVENTS_PER_FRAME {
            let Some(rx) = &self.events else {
                break;
            };
            // Empty and disconnected both mean nothing more for this frame.
            let Ok(event) = rx.try_recv() else {
                break;
            };
            applied += 1;
            self.apply_event(event);
        }
        (applied, applied == EVENTS_PER_FRAME)
    }

    /// Applies one scan event. Events from an earlier scan are ignored.
    pub(crate) fn apply_event(&mut self, event: ScanEvent) {
        if event_generation(&event) != self.generation {
            return;
        }
        match event {
            ScanEvent::Listed { shots, skipped, .. } => {
                self.status.shots = shots.len();
                self.status.skipped = skipped.len();
                self.status.files_total = shots.iter().map(|shot| shot.files.len()).sum();
                // Listing gives path order, and ids are indices into `shots`.
                self.order = shots.iter().map(|shot| shot.id).collect();
                self.shots = shots;
                self.phase = Phase::Parsing;
            }
            // Terminal: the scanner sends nothing after this.
            ScanEvent::ListFailed { error, .. } => self.phase = Phase::Failed(error),
            ScanEvent::FileParsed {
                shot,
                file,
                outcome,
                ..
            } => {
                let target =
                    shot_at_mut(&mut self.shots, shot).and_then(|shot| shot.files.get_mut(file));
                if let Some(target) = target {
                    let failed = apply_file_outcome(target, *outcome);
                    self.status.files_parsed += 1;
                    if failed {
                        self.status.file_errors += 1;
                    }
                }
            }
            ScanEvent::Preview { shot, result, .. } => match result {
                Ok(image) => self.pending_images.push_back((shot, image)),
                Err(error) => {
                    self.in_flight.remove(&shot);
                    self.failed_previews.insert(shot, error);
                }
            },
            ScanEvent::ParsePassDone { .. } => {
                let new_order = capture_order(&self.shots);
                let old = &self.order;
                let focus = position_after_reorder(old, &new_order, self.selection.focus);
                let anchor = self
                    .selection
                    .anchor
                    .map(|anchor| position_after_reorder(old, &new_order, anchor));
                self.selection = Selection { focus, anchor };
                self.order = new_order;
                self.phase = Phase::Done;
                self.reveal_focus();
            }
        }
    }

    /// Scrolls so that the focused tile is visible, using last frame's viewport.
    fn reveal_focus(&mut self) {
        let row = self.selection.focus / self.grid_layout.columns.max(1);
        let (offset, viewport) = self.last_viewport;
        if let Some(scroll) = reveal_row(row, self.grid_layout.row_height, offset, viewport) {
            self.scroll_to = Some(scroll);
        }
    }

    /// Uploads at most `UPLOADS_PER_FRAME` decoded previews as textures.
    fn upload_textures(&mut self, ctx: &egui::Context) -> usize {
        let mut uploaded = 0;
        while uploaded < UPLOADS_PER_FRAME {
            let Some((id, image)) = self.pending_images.pop_front() else {
                break;
            };
            let handle = ctx.load_texture(
                format!("shot-{}", id.0),
                to_color_image(&image),
                egui::TextureOptions::LINEAR,
            );
            self.textures.insert(id, handle);
            self.in_flight.remove(&id);
            uploaded += 1;
        }
        uploaded
    }

    /// Arrow keys, Space and Enter. Nothing happens while a text field has
    /// the keyboard, so typing never triggers a grid action.
    ///
    /// Every key press in the frame counts, in the order it arrived. Arrows
    /// are applied once per press. Space toggles the same range on each press,
    /// so only its parity matters: two presses cancel, as they would over two
    /// frames. Enter opens once, since a second open of the same shot in the
    /// same frame changes nothing.
    fn handle_keys(&mut self, ctx: &egui::Context) -> Option<GridAction> {
        if ctx.egui_wants_keyboard_input() {
            return None;
        }
        let (moves, toggles, open) = ctx.input_mut(|input| {
            let moves = consume_arrow_presses(&mut input.events);
            let toggles = input.count_and_consume_key(egui::Modifiers::NONE, egui::Key::Space);
            let open = input.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
            (moves, toggles, open)
        });
        for (mv, extend) in moves {
            self.selection = apply_move(
                self.selection,
                mv,
                extend,
                self.grid_layout.columns,
                self.order.len(),
            );
            self.reveal_focus();
        }
        if toggles % 2 == 1 {
            let targets: Vec<ShotId> = selected_range(self.selection)
                .filter_map(|position| self.order.get(position).copied())
                .collect();
            toggle_ticks(&mut self.ticks, &targets);
        }
        if open {
            return self
                .order
                .get(self.selection.focus)
                .copied()
                .map(GridAction::Open);
        }
        None
    }

    /// Applies a grid action. Phase 4 opens the overlay here.
    fn handle_action(&mut self, action: GridAction) {
        match action {
            GridAction::Open(_) => {}
        }
    }

    /// Ctrl+scroll over the grid resizes the tiles.
    pub(crate) fn zoom_from_wheel(&mut self, zoom_delta: f32) {
        if zoom_delta != 1.0 {
            self.tile_size = zoom_tile(self.tile_size, zoom_delta);
        }
    }

    /// Applies folder picks, scan events and uploads, then keys. Runs before
    /// `ui`. Returns the events applied and the textures uploaded.
    fn frame_logic(&mut self, ctx: &egui::Context) -> (usize, usize) {
        self.drain_folder_pick();
        let (events, more_events) = self.drain_events();
        let uploads = self.upload_textures(ctx);
        // The key result is ignored until Phase 4 gives Open a meaning.
        if let Some(action) = self.handle_keys(ctx) {
            self.handle_action(action);
        }
        if more_events || !self.pending_images.is_empty() {
            ctx.request_repaint();
        }
        (events, uploads)
    }

    /// Writes a frame line for a drawn frame. A frame that `ui` did not draw
    /// has no CPU time of its own, so it writes nothing.
    fn log_frame(&self, painted: bool, cpu_seconds: Option<f32>, events: usize, uploads: usize) {
        if !painted {
            return;
        }
        if let Some(log) = &self.frame_log {
            log.send(frame_log::frame_line(cpu_seconds, events, uploads));
        }
    }
}

impl eframe::App for PhotosortApp {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // The flag is read here, so it describes the `ui` pass before this one.
        let painted = std::mem::take(&mut self.painted);
        let (events, uploads) = self.frame_logic(ctx);
        self.log_frame(painted, frame.info().cpu_usage, events, uploads);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.painted = true;
        let bar = egui::Panel::top("top_bar").show(ui, |ui| top_bar::show(ui, self));
        match bar.inner {
            TopBarAction::None => {}
            TopBarAction::PickFolder => self.spawn_folder_pick(ui.ctx()),
            TopBarAction::Rescan => {
                if let Some(root) = self.source.clone() {
                    self.start_scan(root);
                }
            }
        }
        let action = egui::CentralPanel::default()
            .show(ui, |ui| grid::show(ui, self))
            .inner;
        if let Some(action) = action {
            self.handle_action(action);
        }
    }
}

/// Removes every arrow press from `events` and returns them in order, each
/// with whether Shift extends the selection. Shift is checked first: a plain
/// arrow match ignores an extra Shift, so checking it first would swallow
/// Shift+arrow.
fn consume_arrow_presses(events: &mut Vec<egui::Event>) -> Vec<(Move, bool)> {
    let mut moves = Vec::new();
    events.retain(|event| {
        let egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } = event
        else {
            return true;
        };
        let Some(mv) = arrow_move(*key) else {
            return true;
        };
        if modifiers.matches_logically(egui::Modifiers::SHIFT) {
            moves.push((mv, true));
        } else if modifiers.matches_logically(egui::Modifiers::NONE) {
            moves.push((mv, false));
        } else {
            return true;
        }
        false
    });
    moves
}

fn arrow_move(key: egui::Key) -> Option<Move> {
    match key {
        egui::Key::ArrowLeft => Some(Move::Left),
        egui::Key::ArrowRight => Some(Move::Right),
        egui::Key::ArrowUp => Some(Move::Up),
        egui::Key::ArrowDown => Some(Move::Down),
        _ => None,
    }
}

fn event_generation(event: &ScanEvent) -> u64 {
    match event {
        ScanEvent::Listed { generation, .. }
        | ScanEvent::ListFailed { generation, .. }
        | ScanEvent::FileParsed { generation, .. }
        | ScanEvent::Preview { generation, .. }
        | ScanEvent::ParsePassDone { generation } => *generation,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::{Duration, Instant};

    use photosort_core::model::{FileKind, IdentityKey, Metadata, ShotFile};
    use photosort_testkit::{ExifSpec, Order, jpeg_file};

    use super::*;
    use crate::scan::FileOutcome;
    use crate::ui::grid_math::TILE_MAX;

    /// The longest any test waits for a scan to make progress.
    const WAIT: Duration = Duration::from_secs(10);
    const FALLBACK: u64 = 8 * 1024 * 1024 * 1024;

    fn app() -> PhotosortApp {
        let waker: Waker = Arc::new(|| {});
        PhotosortApp::build(waker, None, FALLBACK)
    }

    /// A shot with one JPEG file per path, all in one stem.
    fn shot(id: u32, paths: &[&str]) -> Shot {
        let files = paths
            .iter()
            .map(|path| ShotFile {
                path: PathBuf::from(path),
                kind: FileKind("jpeg"),
                size: 1,
                modified: std::time::SystemTime::UNIX_EPOCH,
                meta: None,
                identity: None,
                error: None,
            })
            .collect();
        Shot {
            id: ShotId(id),
            dir: PathBuf::from("/card"),
            stem: "DSC".to_owned(),
            files,
        }
    }

    fn with_capture_time(mut shot: Shot, dto: &str) -> Shot {
        for file in &mut shot.files {
            file.meta = Some(Metadata {
                date_time_original: Some(dto.to_owned()),
                ..Metadata::default()
            });
        }
        shot
    }

    fn listed(generation: u64, shots: Vec<Shot>) -> ScanEvent {
        ScanEvent::Listed {
            generation,
            shots,
            skipped: Vec::new(),
        }
    }

    /// Feeds events from the running scan until `done` holds, failing after `WAIT`.
    fn pump_until(app: &mut PhotosortApp, done: impl Fn(&PhotosortApp) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done(app) {
            assert!(
                Instant::now() < deadline,
                "scan made no progress in {WAIT:?}"
            );
            let Some(events) = app.events.as_ref() else {
                panic!("no scan is running");
            };
            match events.recv_timeout(Duration::from_millis(100)) {
                Ok(event) => app.apply_event(event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("scan ended before the condition held")
                }
            }
        }
    }

    #[test]
    fn a_listing_shows_the_shots_in_path_order_and_starts_parsing() {
        let mut app = app();
        app.generation = 1;
        app.apply_event(listed(
            1,
            vec![shot(0, &["/card/a.jpg"]), shot(1, &["/card/b.jpg"])],
        ));
        assert_eq!(app.order, vec![ShotId(0), ShotId(1)]);
        assert_eq!(app.phase, Phase::Parsing);
        assert_eq!(app.status.shots, 2);
        assert_eq!(app.status.files_total, 2);
    }

    #[test]
    fn events_from_an_earlier_scan_are_ignored() {
        let mut app = app();
        app.generation = 2;
        app.apply_event(listed(1, vec![shot(0, &["/card/a.jpg"])]));
        assert!(app.shots.is_empty());
        assert_eq!(app.phase, Phase::Idle);
    }

    #[test]
    fn a_failed_listing_is_terminal_and_keeps_its_error() {
        let mut app = app();
        app.generation = 1;
        app.apply_event(ScanEvent::ListFailed {
            generation: 1,
            error: "source is not a folder: /x".to_owned(),
        });
        assert_eq!(
            app.phase,
            Phase::Failed("source is not a folder: /x".to_owned())
        );
    }

    #[test]
    fn a_parsed_file_counts_towards_progress_and_its_errors() {
        let mut app = app();
        app.generation = 1;
        app.apply_event(listed(1, vec![shot(0, &["/card/a.jpg"])]));
        app.apply_event(ScanEvent::FileParsed {
            generation: 1,
            shot: ShotId(0),
            file: 0,
            outcome: Box::new(FileOutcome {
                meta: Err("malformed".to_owned()),
                identity: None,
            }),
        });
        assert_eq!(app.status.files_parsed, 1);
        assert_eq!(app.status.file_errors, 1);
        assert!(crate::scan_model::is_unreadable(&app.shots[0]));
    }

    #[test]
    fn a_parsed_file_for_a_shot_that_does_not_exist_is_dropped() {
        let mut app = app();
        app.generation = 1;
        app.apply_event(listed(1, vec![shot(0, &["/card/a.jpg"])]));
        app.apply_event(ScanEvent::FileParsed {
            generation: 1,
            shot: ShotId(9),
            file: 0,
            outcome: Box::new(FileOutcome {
                meta: Ok(Metadata::default()),
                identity: Some(IdentityKey::Content {
                    size: 1,
                    head_blake3: [0; 32],
                }),
            }),
        });
        assert_eq!(app.status.files_parsed, 0);
    }

    #[test]
    fn a_failed_preview_is_recorded_so_it_is_not_requested_again() {
        let mut app = app();
        app.generation = 1;
        app.in_flight.insert(ShotId(3));
        app.apply_event(ScanEvent::Preview {
            generation: 1,
            shot: ShotId(3),
            result: Err("preview does not decode".to_owned()),
        });
        assert!(!app.in_flight.contains(&ShotId(3)));
        assert_eq!(
            app.failed_previews.get(&ShotId(3)).map(String::as_str),
            Some("preview does not decode")
        );
    }

    #[test]
    fn a_decoded_preview_waits_for_upload_and_stays_in_flight() {
        let mut app = app();
        app.generation = 1;
        app.in_flight.insert(ShotId(0));
        app.apply_event(ScanEvent::Preview {
            generation: 1,
            shot: ShotId(0),
            result: Ok(DecodedImage {
                size: [1, 1],
                rgba: vec![0; 4],
            }),
        });
        assert_eq!(app.pending_images.len(), 1);
        assert!(app.in_flight.contains(&ShotId(0)));
    }

    #[test]
    fn the_parse_pass_orders_by_capture_time_and_keeps_focus_on_the_same_shot() {
        let mut app = app();
        app.generation = 1;
        app.apply_event(listed(
            1,
            vec![
                with_capture_time(shot(0, &["/card/a.jpg"]), "2026:11:20 10:00:00"),
                with_capture_time(shot(1, &["/card/b.jpg"]), "2026:11:20 09:00:00"),
                with_capture_time(shot(2, &["/card/c.jpg"]), "2026:11:20 11:00:00"),
            ],
        ));
        // Path order is 0, 1, 2. Focus on position 0, which is ShotId(0).
        app.selection = Selection {
            focus: 0,
            anchor: None,
        };
        app.apply_event(ScanEvent::ParsePassDone { generation: 1 });
        assert_eq!(app.order, vec![ShotId(1), ShotId(0), ShotId(2)]);
        assert_eq!(app.order.get(app.selection.focus), Some(&ShotId(0)));
        assert_eq!(app.phase, Phase::Done);
    }

    #[test]
    fn a_range_anchor_follows_its_shot_through_the_sort_too() {
        let mut app = app();
        app.generation = 1;
        app.apply_event(listed(
            1,
            vec![
                with_capture_time(shot(0, &["/card/a.jpg"]), "2026:11:20 10:00:00"),
                with_capture_time(shot(1, &["/card/b.jpg"]), "2026:11:20 09:00:00"),
            ],
        ));
        app.selection = Selection {
            focus: 1,
            anchor: Some(0),
        };
        app.apply_event(ScanEvent::ParsePassDone { generation: 1 });
        // New order is [1, 0]. Focus was ShotId(1) at 1, now at 0. Anchor was ShotId(0) at 0, now at 1.
        assert_eq!(app.selection.focus, 0);
        assert_eq!(app.selection.anchor, Some(1));
    }

    #[test]
    fn ctrl_scroll_zoom_resizes_tiles_and_stays_within_their_range() {
        let mut app = app();
        app.zoom_from_wheel(1.5);
        assert_eq!(app.tile_size, 300.0);
        app.zoom_from_wheel(1.0);
        assert_eq!(app.tile_size, 300.0);
        app.zoom_from_wheel(100.0);
        assert_eq!(app.tile_size, TILE_MAX);
    }

    /// Writes two JPEGs, starts a real scan and waits for it to finish.
    #[test]
    fn a_real_scan_lists_parses_and_orders_a_folder() {
        let dir = tempfile::tempdir().expect("temp dir");
        let bytes = jpeg_file(Order::Big, Some(&ExifSpec::zf()), None, None);
        write(&dir.path().join("DSC_0002.JPG"), &bytes);
        write(&dir.path().join("DSC_0001.JPG"), &bytes);

        let mut app = app();
        app.start_scan(dir.path().to_path_buf());
        pump_until(&mut app, |app| app.phase == Phase::Done);

        assert_eq!(app.shots.len(), 2);
        assert_eq!(app.status.files_parsed, 2);
        assert_eq!(app.status.file_errors, 0);
        // Same capture time, so the name breaks the tie.
        let names: Vec<String> = app
            .order
            .iter()
            .filter_map(|&id| crate::scan_model::shot_at(&app.shots, id))
            .map(Shot::display_name)
            .collect();
        assert_eq!(names, ["DSC_0001.JPG", "DSC_0002.JPG"]);
    }

    /// A new scan drops the old shots, so no old id survives into it.
    #[test]
    fn a_rescan_clears_the_previous_scan_before_it_starts() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(
            &dir.path().join("DSC_0001.JPG"),
            &jpeg_file(Order::Big, None, None, None),
        );
        let mut app = app();
        app.start_scan(dir.path().to_path_buf());
        pump_until(&mut app, |app| app.phase == Phase::Done);
        app.ticks.insert(ShotId(0));

        app.start_scan(dir.path().to_path_buf());
        assert!(app.ticks.is_empty());
        assert_eq!(app.selection, Selection::default());
        assert_eq!(app.phase, Phase::Listing);
        assert_eq!(app.generation, 2);
    }

    /// The source path is not a folder, so the scan fails with a message.
    #[test]
    fn a_source_that_is_not_a_folder_fails_the_scan() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("loose.jpg");
        write(&file, b"not a folder");
        let mut app = app();
        app.start_scan(file);
        pump_until(&mut app, |app| matches!(app.phase, Phase::Failed(_)));
        assert!(matches!(&app.phase, Phase::Failed(error) if error.contains("not a folder")));
    }

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).expect("write file");
    }

    #[test]
    fn the_waker_is_called_by_a_scan() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let waker: Waker = Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let dir = tempfile::tempdir().expect("temp dir");
        let mut app = PhotosortApp::build(waker, None, FALLBACK);
        app.start_scan(dir.path().to_path_buf());
        pump_until(&mut app, |app| app.phase == Phase::Done);
        assert!(calls.load(Ordering::SeqCst) > 0);
    }

    /// An app over `len` shots in one column, so Right and Down step by one.
    fn app_with_order(len: u32) -> PhotosortApp {
        let mut app = app();
        app.order = (0..len).map(ShotId).collect();
        app.grid_layout.columns = 1;
        app
    }

    fn key_press(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    /// Runs one egui pass whose input is `events`, and handles its keys.
    /// Returns the action the keys produced.
    fn press_keys(app: &mut PhotosortApp, events: Vec<egui::Event>) -> Option<GridAction> {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events,
            ..Default::default()
        };
        let mut action = None;
        let mut output = ctx.run_ui(input, |_ui| action = app.handle_keys(&ctx));
        // epaint asserts in debug builds that texture changes are handled. The
        // test makes none, so clearing is the handling.
        output.textures_delta.clear();
        action
    }

    #[test]
    fn shift_arrow_extends_the_selection_and_a_plain_arrow_clears_it() {
        let mut app = app_with_order(3);
        press_keys(
            &mut app,
            vec![key_press(egui::Key::ArrowRight, egui::Modifiers::SHIFT)],
        );
        assert_eq!(
            app.selection,
            Selection {
                focus: 1,
                anchor: Some(0)
            }
        );

        press_keys(
            &mut app,
            vec![key_press(egui::Key::ArrowRight, egui::Modifiers::NONE)],
        );
        assert_eq!(
            app.selection,
            Selection {
                focus: 2,
                anchor: None
            }
        );
    }

    #[test]
    fn every_arrow_press_in_one_frame_moves_the_focus() {
        let mut app = app_with_order(5);
        let right = key_press(egui::Key::ArrowRight, egui::Modifiers::NONE);
        press_keys(&mut app, vec![right; 3]);
        assert_eq!(app.selection.focus, 3);
    }

    #[test]
    fn a_mixed_run_of_arrows_keeps_its_order() {
        // Shift+Right then plain Right: the range is started, then cleared.
        let mut app = app_with_order(5);
        press_keys(
            &mut app,
            vec![
                key_press(egui::Key::ArrowRight, egui::Modifiers::SHIFT),
                key_press(egui::Key::ArrowRight, egui::Modifiers::NONE),
            ],
        );
        assert_eq!(
            app.selection,
            Selection {
                focus: 2,
                anchor: None
            }
        );
    }

    /// Two Space presses in one frame cancel, as they would over two frames.
    /// Three toggle once.
    #[test]
    fn space_presses_in_one_frame_toggle_by_parity() {
        let mut app = app_with_order(3);
        let space = || key_press(egui::Key::Space, egui::Modifiers::NONE);
        press_keys(&mut app, vec![space(), space()]);
        assert!(app.ticks.is_empty(), "two presses cancel");

        press_keys(&mut app, vec![space(), space(), space()]);
        assert_eq!(app.ticks, HashSet::from([ShotId(0)]));
    }

    #[test]
    fn enter_pressed_twice_in_one_frame_opens_the_focused_shot_once() {
        let mut app = app_with_order(3);
        let enter = key_press(egui::Key::Enter, egui::Modifiers::NONE);
        let action = press_keys(&mut app, vec![enter; 2]);
        assert_eq!(action, Some(GridAction::Open(ShotId(0))));
    }

    #[test]
    fn upload_textures_uploads_at_most_eight_per_frame() {
        let mut app = app();
        for i in 0..20 {
            app.pending_images.push_back((
                ShotId(i),
                DecodedImage {
                    size: [1, 1],
                    rgba: vec![0; 4],
                },
            ));
        }
        let ctx = egui::Context::default();

        let uploaded = app.upload_textures(&ctx);

        assert_eq!(uploaded, 8);
        assert_eq!(app.textures.len(), 8);
        assert_eq!(app.pending_images.len(), 12);
    }

    #[test]
    fn drain_events_stops_at_5000_and_leaves_the_rest_queued() {
        let mut app = app();
        app.generation = 1;
        let (tx, rx) = mpsc::channel();
        for _ in 0..6000 {
            // A stale generation, so each event is a no-op and only the count matters.
            tx.send(ScanEvent::ParsePassDone { generation: 0 })
                .expect("queue is open");
        }
        app.events = Some(rx);

        let (applied, more) = app.drain_events();

        assert_eq!(applied, 5000);
        assert!(more, "more events are waiting");
        let rest = app.events.as_ref().expect("the scan is still running");
        assert_eq!(rest.try_iter().count(), 1000);
    }

    /// Only a drawn frame writes a line. A hidden frame has no CPU time of its own.
    #[test]
    fn only_a_drawn_frame_writes_a_log_line() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("frames.log");
        let (log, writer) = frame_log::spawn(&path).expect("the file opens");
        let waker: Waker = Arc::new(|| {});
        let app = PhotosortApp::build(waker, Some(log), FALLBACK);

        app.log_frame(false, Some(0.5), 1, 1);
        app.log_frame(true, None, 2, 3);
        app.log_frame(true, Some(0.0123), 4, 5);
        drop(app);

        writer.join().expect("the writer ends");
        assert_eq!(
            std::fs::read_to_string(&path).expect("the log is readable"),
            "- 2 3\n12.3 4 5\n# dropped 0\n"
        );
    }

    /// Drives `logic` and `ui` through a real egui pass. `Frame::_new_kittest`
    /// is `#[doc(hidden)]` in eframe; the exact pin `=0.37.0` keeps it stable.
    /// A `logic` call logs only when the `ui` pass before it drew.
    #[test]
    fn logic_logs_a_line_only_after_a_drawn_pass() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("frames.log");
        let (log, writer) = frame_log::spawn(&path).expect("the file opens");
        let waker: Waker = Arc::new(|| {});
        let mut app = PhotosortApp::build(waker, Some(log), FALLBACK);
        let ctx = egui::Context::default();
        let mut frame = eframe::Frame::_new_kittest();

        // No `ui` pass has run, so this logic-only frame writes nothing.
        eframe::App::logic(&mut app, &ctx, &mut frame);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            eframe::App::ui(&mut app, ui, &mut frame);
        });
        // The texture delta is not uploaded here, and epaint panics on drop if
        // it is not cleared.
        output.textures_delta.clear();
        // The drawn pass is logged by the logic call that follows it.
        eframe::App::logic(&mut app, &ctx, &mut frame);
        // A logic-only frame after the drawn one writes nothing: the flag was
        // taken by the logic call above.
        eframe::App::logic(&mut app, &ctx, &mut frame);
        drop(app);

        writer.join().expect("the writer ends");
        assert_eq!(
            std::fs::read_to_string(&path).expect("the log is readable"),
            "- 0 0\n# dropped 0\n"
        );
    }
}
