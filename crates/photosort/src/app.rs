//! Top-level egui application: owns the scan, the caches and the UI state,
//! and applies scan events and key presses once per frame.
// pattern: Imperative Shell

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread;

use eframe::egui;
use photosort_core::grouping::capture_order;
use photosort_core::metadata::Registry;
use photosort_core::model::{Shot, ShotId};

use crate::budget::{CacheSplit, cache_budget_bytes, split};
use crate::cache::{PreviewBytes, Textures, physical_ram, preview_bytes, textures};
use crate::frame_log;
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
    pub(crate) frame_log: Option<SyncSender<String>>,
}

impl PhotosortApp {
    pub(crate) fn new(
        cc: &eframe::CreationContext<'_>,
        frame_log: Option<SyncSender<String>>,
    ) -> Self {
        // Ctrl+scroll is ours, and egui's own Ctrl+/- zoom would fight it.
        cc.egui_ctx
            .options_mut(|options| options.zoom_with_keyboard = false);
        let ctx = cc.egui_ctx.clone();
        let waker: Waker = Arc::new(move || ctx.request_repaint());
        // The override arrives with the config in Phase 6.
        let ram = physical_ram().unwrap_or(FALLBACK_RAM);
        Self::build(waker, frame_log, ram)
    }

    fn build(waker: Waker, frame_log: Option<SyncSender<String>>, ram: u64) -> Self {
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
    fn handle_keys(&mut self, ctx: &egui::Context) -> Option<GridAction> {
        if ctx.egui_wants_keyboard_input() {
            return None;
        }
        let (moves, toggle, open) = ctx.input_mut(|input| {
            let mut moves = Vec::new();
            let arrows = [
                (egui::Key::ArrowLeft, Move::Left),
                (egui::Key::ArrowRight, Move::Right),
                (egui::Key::ArrowUp, Move::Up),
                (egui::Key::ArrowDown, Move::Down),
            ];
            for (key, mv) in arrows {
                // Shift first: a NONE match ignores an extra Shift, so checking
                // NONE first would swallow Shift+arrow.
                if input.consume_key(egui::Modifiers::SHIFT, key) {
                    moves.push((mv, true));
                } else if input.consume_key(egui::Modifiers::NONE, key) {
                    moves.push((mv, false));
                }
            }
            let toggle = input.consume_key(egui::Modifiers::NONE, egui::Key::Space);
            let open = input.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
            (moves, toggle, open)
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
        if toggle {
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

    /// Applies folder picks, scan events and uploads, then keys. Runs before `ui`.
    fn frame_logic(&mut self, ctx: &egui::Context, cpu_seconds: Option<f32>) {
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
        if let Some(log) = &self.frame_log {
            frame_log::send(log, frame_log::frame_line(cpu_seconds, events, uploads));
        }
    }
}

impl eframe::App for PhotosortApp {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.frame_logic(ctx, frame.info().cpu_usage);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
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
}
