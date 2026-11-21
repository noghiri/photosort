//! Two-pass card scan: lists and groups shots at once, then parses headers and
//! loads previews on a worker pool that serves visible shots first.
// pattern: Imperative Shell

use std::fs::{self, File};
use std::io::{self, Read};
use std::num::NonZero;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;

use photosort_core::grouping;
use photosort_core::identity::{CONTENT_HEAD_LEN, identity_for};
use photosort_core::metadata::{MAX_HEAD_LEN, Registry};
use photosort_core::model::{
    ByteRange, FileKind, IdentityKey, ListedFile, Metadata, ParseError, Shot, ShotId,
};
use walkdir::WalkDir;

use crate::cache::PreviewBytes;
use crate::preview::{DecodedImage, TILE_TEXTURE_MAX, decode_preview, read_range};
use crate::scan_queue::{Job, JobQueue};

/// Calls the UI's repaint hook. Every event send is followed by one call.
pub(crate) type Waker = Arc<dyn Fn() + Send + Sync>;

/// Decodes one preview job. `start` passes `load_preview`; a test passes a
/// loader that panics, to cover the panic guard around preview jobs.
type PreviewLoader =
    fn(&PreviewBytes, ShotId, &Path, ByteRange, Option<u16>) -> Result<DecodedImage, String>;

/// Bounded, so a slow UI applies backpressure to the workers.
const EVENT_CAPACITY: usize = 8192;
/// SD cards gain little from more parallel readers.
const MAX_WORKERS: usize = 4;
/// Head-read and parse attempts before a header that keeps asking for more
/// bytes is reported as an error.
const MAX_PARSE_ATTEMPTS: usize = 6;
/// `NeedMore` heads are read in whole chunks of this size.
const HEAD_CHUNK: u64 = 64 * 1024;

#[derive(Debug)]
pub(crate) enum ScanEvent {
    Listed {
        generation: u64,
        shots: Vec<Shot>,
        skipped: Vec<String>,
    },
    ListFailed {
        generation: u64,
        error: String,
    },
    FileParsed {
        generation: u64,
        shot: ShotId,
        file: usize,
        /// Boxed so the header metadata does not inflate every event.
        outcome: Box<FileOutcome>,
    },
    Preview {
        generation: u64,
        shot: ShotId,
        result: Result<DecodedImage, String>,
    },
    ParsePassDone {
        generation: u64,
    },
}

#[derive(Debug)]
pub(crate) struct FileOutcome {
    pub meta: Result<Metadata, String>,
    pub identity: Option<IdentityKey>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ScanError {
    #[error("failed to read source folder {}: {source}", path.display())]
    Root {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("source is not a folder: {}", path.display())]
    NotADirectory { path: PathBuf },
}

/// Walks `root` and groups the registered image files into shots. Entries
/// that cannot be read are returned as `skipped` messages, and the walk goes on.
/// It stops as soon as `stopped` returns true and returns whatever was listed
/// so far.
fn list_source_until(
    root: &Path,
    registry: &Registry,
    stopped: impl Fn() -> bool,
) -> Result<(Vec<Shot>, Vec<String>), ScanError> {
    let metadata = fs::metadata(root).map_err(|source| ScanError::Root {
        path: root.to_path_buf(),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(ScanError::NotADirectory {
            path: root.to_path_buf(),
        });
    }
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    for entry in WalkDir::new(root).follow_links(false) {
        if stopped() {
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) if err.depth() == 0 => {
                let source = err
                    .into_io_error()
                    .unwrap_or_else(|| io::Error::other("directory walk failed"));
                return Err(ScanError::Root {
                    path: root.to_path_buf(),
                    source,
                });
            }
            Err(err) => {
                skipped.push(err.to_string());
                continue;
            }
        };
        if !entry.file_type().is_file() || !is_registered(entry.path(), registry) {
            continue;
        }
        match listed_file(&entry) {
            Ok(file) => files.push(file),
            Err(message) => skipped.push(message),
        }
    }
    Ok((grouping::group_files(files, registry), skipped))
}

fn is_registered(path: &Path, registry: &Registry) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| registry.format_for_extension(ext).is_some())
}

fn listed_file(entry: &walkdir::DirEntry) -> Result<ListedFile, String> {
    let path = entry.path();
    let metadata = entry
        .metadata()
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let modified = metadata
        .modified()
        .map_err(|e| format!("failed to read modified time of {}: {e}", path.display()))?;
    Ok(ListedFile {
        path: path.to_path_buf(),
        size: metadata.len(),
        modified,
    })
}

/// The bytes read so far from the start of a file, and the file's length.
#[derive(Default)]
struct Head {
    bytes: Vec<u8>,
    file_len: u64,
}

/// Reads and parses the header of one file. Returns the metadata or the error
/// text, and the identity when any bytes were read.
pub(crate) fn parse_file(path: &Path, kind: FileKind, registry: &Registry) -> FileOutcome {
    let mut head = Head::default();
    let meta = parse_head(path, kind, registry, &mut head);
    let identity = (!head.bytes.is_empty())
        .then(|| identity_for(meta.as_ref().ok(), head.file_len, &head.bytes));
    FileOutcome { meta, identity }
}

/// Reads the head the reader asks for, then parses it. On `NeedMore`, reads
/// further, up to `MAX_PARSE_ATTEMPTS` parses in all.
fn parse_head(
    path: &Path,
    kind: FileKind,
    registry: &Registry,
    head: &mut Head,
) -> Result<Metadata, String> {
    let read_error = |e: io::Error| format!("failed to read {}: {e}", path.display());
    let mut file = File::open(path).map_err(read_error)?;
    head.file_len = file.metadata().map_err(read_error)?.len();

    let hint = registry
        .format_for_kind(kind)
        .map_or(0, |reader| reader.header_len_hint());
    let hint = u64::try_from(hint).unwrap_or(u64::MAX);
    let mut want = hint.max(CONTENT_HEAD_LEN as u64).min(head.file_len);
    for _ in 0..MAX_PARSE_ATTEMPTS {
        extend_head(&mut file, &mut head.bytes, want).map_err(read_error)?;
        match registry.parse(kind, &head.bytes, head.file_len) {
            Ok(meta) => return Ok(meta),
            Err(ParseError::NeedMore { needed, .. }) => {
                let have = u64::try_from(head.bytes.len()).unwrap_or(u64::MAX);
                if let Some(message) = unsatisfiable_need(needed, have, head.file_len) {
                    return Err(message);
                }
                want = next_head_len(needed, head.file_len);
            }
            Err(err) => return Err(err.to_string()),
        }
    }
    Err(format!("header needs more than {MAX_PARSE_ATTEMPTS} reads"))
}

/// Why a `NeedMore` for `needed` bytes cannot be met by reading more, or
/// `None` when a longer read can. The built-in readers keep `needed` within
/// the file and `MAX_HEAD_LEN` (`bytes_within` reports anything past either as
/// `Malformed`), so the first two messages come only from a reader that does
/// not use that helper.
fn unsatisfiable_need(needed: u64, have: u64, file_len: u64) -> Option<String> {
    if needed > MAX_HEAD_LEN {
        Some(format!(
            "header is larger than the {MAX_HEAD_LEN}-byte head limit"
        ))
    } else if needed > file_len {
        Some(format!(
            "header is cut off: the file is {file_len} bytes but the header needs {needed}"
        ))
    } else if needed <= have {
        // The reader asked for bytes it already has, so another read cannot help.
        Some(format!(
            "reader asked for {needed} bytes after {have} were read"
        ))
    } else {
        None
    }
}

/// Grows `head` to `want` bytes from the start of the file. Bytes already read
/// are kept, and a file shorter than `want` leaves `head` short.
fn extend_head(file: &mut File, head: &mut Vec<u8>, want: u64) -> io::Result<()> {
    let have = u64::try_from(head.len()).unwrap_or(u64::MAX);
    if want > have {
        file.by_ref().take(want - have).read_to_end(head)?;
    }
    Ok(())
}

/// The next head length to read after `NeedMore { needed }`: `needed` rounded
/// up to a whole chunk, capped at the largest head and at the file length.
fn next_head_len(needed: u64, file_len: u64) -> u64 {
    needed
        .div_ceil(HEAD_CHUNK)
        .saturating_mul(HEAD_CHUNK)
        .min(MAX_HEAD_LEN)
        .min(file_len)
}

/// One pool thread per core, up to `MAX_WORKERS`.
fn worker_count(available: usize) -> usize {
    available.min(MAX_WORKERS)
}

/// Shared between the UI thread and the scan threads.
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    ready: Condvar,
    stop: AtomicBool,
}

#[derive(Default)]
struct State {
    queue: JobQueue,
    /// Parse jobs not yet finished in this pass.
    outstanding_parses: usize,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Blocks until a job is queued or the scan stops. A queued job always
    /// comes before the wait, so a notification sent before the wait is not lost.
    fn next_job(&self) -> Option<Job> {
        let mut state = self.lock();
        loop {
            if self.stopped() {
                return None;
            }
            if let Some(job) = state.queue.pop() {
                return Some(job);
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Queues the whole parse pass and sets its outstanding count under one
    /// lock, so no worker can see a partly queued pass.
    fn enqueue_parse_pass(&self, jobs: Vec<Job>) {
        let mut state = self.lock();
        if self.stopped() {
            return;
        }
        state.outstanding_parses = jobs.len();
        for job in jobs {
            state.queue.push(job);
        }
        self.ready.notify_all();
    }

    /// Records one finished parse. True for exactly one call per pass: the one
    /// that finishes the last parse.
    fn finish_parse(&self) -> bool {
        let mut state = self.lock();
        let Some(left) = state.outstanding_parses.checked_sub(1) else {
            return false;
        };
        state.outstanding_parses = left;
        left == 0
    }
}

impl Ctx {
    /// Sends one event, then wakes the UI. False when the scan should stop:
    /// the scanner was dropped, or the receiver is gone.
    fn emit(&self, event: ScanEvent) -> bool {
        if self.shared.stopped() || self.tx.send(event).is_err() {
            return false;
        }
        (self.waker)();
        true
    }
}

/// Handle to a running scan. Dropping it stops the threads but does not join
/// them: joining on the UI thread could wait on a slow card read.
pub(crate) struct Scanner {
    shared: Arc<Shared>,
}

/// Everything a scan thread needs. Each thread owns a clone.
#[derive(Clone)]
struct Ctx {
    shared: Arc<Shared>,
    tx: SyncSender<ScanEvent>,
    waker: Waker,
    registry: Arc<Registry>,
    bytes: Arc<PreviewBytes>,
    preview_loader: PreviewLoader,
    generation: u64,
}

impl Scanner {
    /// Starts listing `root` on a lister thread. Parse jobs follow the listing
    /// on the worker pool.
    pub(crate) fn start(
        root: PathBuf,
        registry: Arc<Registry>,
        bytes: Arc<PreviewBytes>,
        waker: Waker,
        generation: u64,
    ) -> (Scanner, Receiver<ScanEvent>) {
        Self::start_with_capacity(
            root,
            registry,
            bytes,
            waker,
            generation,
            EVENT_CAPACITY,
            load_preview,
        )
    }

    /// As `start`, with the event queue's capacity and the preview loader
    /// given. Only `start` passes `EVENT_CAPACITY` and `load_preview`; tests
    /// pass a small queue to fill it, or a loader that panics.
    fn start_with_capacity(
        root: PathBuf,
        registry: Arc<Registry>,
        bytes: Arc<PreviewBytes>,
        waker: Waker,
        generation: u64,
        capacity: usize,
        preview_loader: PreviewLoader,
    ) -> (Scanner, Receiver<ScanEvent>) {
        let (tx, rx) = mpsc::sync_channel(capacity);
        let ctx = Ctx {
            shared: Arc::new(Shared::default()),
            tx,
            waker,
            registry,
            bytes,
            preview_loader,
            generation,
        };
        let scanner = Scanner {
            shared: Arc::clone(&ctx.shared),
        };
        let lister = ctx.clone();
        spawn_thread("photosort-lister", move || run_lister(&root, &lister));
        let available = thread::available_parallelism().map_or(1, NonZero::get);
        for index in 0..worker_count(available) {
            let worker = ctx.clone();
            spawn_thread(&format!("photosort-scan-{index}"), move || {
                run_worker(&worker)
            });
        }
        (scanner, rx)
    }

    pub(crate) fn set_visible(&self, visible: Vec<ShotId>) {
        self.shared.lock().queue.set_visible(visible);
    }

    /// Queues a preview load unless one is already queued for `shot`.
    pub(crate) fn request_preview(
        &self,
        shot: ShotId,
        path: PathBuf,
        range: ByteRange,
        orientation: Option<u16>,
    ) {
        let mut state = self.shared.lock();
        if state.queue.contains_preview(shot) {
            return;
        }
        state.queue.push(Job::Preview {
            shot,
            path,
            range,
            orientation,
        });
        self.shared.ready.notify_one();
    }
}

/// Detaches a scan thread. The thread ends when its work is done or the scan
/// stops, so its handle is not kept.
fn spawn_thread(name: &str, body: impl FnOnce() + Send + 'static) {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(body)
        .expect("failed to start a scan thread");
}

/// Lists the source, sends `Listed`, then queues one parse per file in shot
/// order. An empty source sends `ParsePassDone` at once, because no worker
/// will finish a parse to send it.
fn run_lister(root: &Path, ctx: &Ctx) {
    let listing = list_source_until(root, &ctx.registry, || ctx.shared.stopped());
    if ctx.shared.stopped() {
        return;
    }
    let (shots, skipped) = match listing {
        Ok(listing) => listing,
        Err(err) => {
            ctx.emit(ScanEvent::ListFailed {
                generation: ctx.generation,
                error: err.to_string(),
            });
            return;
        }
    };
    let jobs = parse_jobs(&shots);
    let parses = jobs.len();
    if !ctx.emit(ScanEvent::Listed {
        generation: ctx.generation,
        shots,
        skipped,
    }) {
        return;
    }
    ctx.shared.enqueue_parse_pass(jobs);
    if parses == 0 {
        ctx.emit(ScanEvent::ParsePassDone {
            generation: ctx.generation,
        });
    }
}

/// One parse job per file, in shot order.
fn parse_jobs(shots: &[Shot]) -> Vec<Job> {
    shots
        .iter()
        .flat_map(|shot| {
            shot.files
                .iter()
                .enumerate()
                .map(move |(file, listed)| Job::Parse {
                    shot: shot.id,
                    file,
                    path: listed.path.clone(),
                    kind: listed.kind,
                })
        })
        .collect()
}

/// Runs jobs until the scan stops. The worker that finishes the last parse of
/// the pass sends `ParsePassDone`. A panicking job reports an error, so the
/// pass still finishes and the tile does not wait forever.
fn run_worker(ctx: &Ctx) {
    while let Some(job) = ctx.shared.next_job() {
        let sent = match job {
            Job::Parse {
                shot,
                file,
                path,
                kind,
            } => {
                let outcome = catch_panic(|| parse_file(&path, kind, &ctx.registry))
                    .unwrap_or_else(|message| FileOutcome {
                        meta: Err(message),
                        identity: None,
                    });
                let parsed = ctx.emit(ScanEvent::FileParsed {
                    generation: ctx.generation,
                    shot,
                    file,
                    outcome: Box::new(outcome),
                });
                parsed
                    && (!ctx.shared.finish_parse()
                        || ctx.emit(ScanEvent::ParsePassDone {
                            generation: ctx.generation,
                        }))
            }
            Job::Preview {
                shot,
                path,
                range,
                orientation,
            } => {
                let result = catch_panic(|| {
                    (ctx.preview_loader)(&ctx.bytes, shot, &path, range, orientation)
                })
                .and_then(std::convert::identity);
                ctx.emit(ScanEvent::Preview {
                    generation: ctx.generation,
                    shot,
                    result,
                })
            }
        };
        if !sent {
            return;
        }
    }
}

/// Runs one job body. A panic in it becomes an error message, so the job
/// still reports its outcome.
fn catch_panic<T>(body: impl FnOnce() -> T) -> Result<T, String> {
    panic::catch_unwind(AssertUnwindSafe(body)).map_err(|payload| {
        let detail = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("no message");
        format!("worker panicked: {detail}")
    })
}

/// Decodes a shot's preview, reading its bytes from disk on a cache miss and
/// keeping them in the cache.
fn load_preview(
    bytes: &PreviewBytes,
    shot: ShotId,
    path: &Path,
    range: ByteRange,
    orientation: Option<u16>,
) -> Result<DecodedImage, String> {
    let encoded = match bytes.get(&shot) {
        Some(cached) => cached,
        None => {
            let read = read_range(path, range)
                .map_err(|e| format!("failed to read preview from {}: {e}", path.display()))?;
            let encoded: Arc<[u8]> = Arc::from(read);
            bytes.insert(shot, Arc::clone(&encoded));
            encoded
        }
    };
    decode_preview(&encoded, orientation, TILE_TEXTURE_MAX).map_err(|e| e.to_string())
}

impl Drop for Scanner {
    fn drop(&mut self) {
        // The flag is set under the lock, so a worker that has just checked it
        // cannot miss the wake-up and wait forever.
        let _state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.ready.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    use photosort_core::metadata::exif::read_tiff_exif;
    use photosort_core::metadata::formats::{jpeg, nef};
    use photosort_core::metadata::tiff::TIFF_MAGIC;
    use photosort_core::metadata::{FormatReader, ParseInput};
    use photosort_testkit::{
        ExifSpec, NikonSpec, Order, jpeg_file, nef_file, tiff_file, tiny_jpeg,
    };
    use tempfile::{TempDir, tempdir};

    use super::*;
    use crate::budget::CacheSplit;
    use crate::cache::preview_bytes;

    /// The longest any test waits for a condition.
    const WAIT: Duration = Duration::from_secs(10);
    const MIB: u64 = 1024 * 1024;
    const ZF_DTO: &str = "2026:11:20 14:59:32";

    fn write_file(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().expect("file has a parent")).expect("create folder");
        fs::write(path, bytes).expect("write file");
    }

    fn jpeg_with_exif() -> Vec<u8> {
        jpeg_file(Order::Big, Some(&ExifSpec::zf()), None, None)
    }

    /// A waker that counts its calls. The test keeps the count and the scan
    /// keeps the waker.
    fn counting_waker() -> (Waker, Arc<AtomicUsize>) {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let waker: Waker = Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        (waker, wakes)
    }

    fn start_scan(
        root: &Path,
        registry: Arc<Registry>,
    ) -> (Scanner, Receiver<ScanEvent>, Arc<AtomicUsize>) {
        let (waker, wakes) = counting_waker();
        let bytes = Arc::new(preview_bytes(&CacheSplit {
            bytes: 64 * MIB,
            textures: 0,
        }));
        let (scanner, rx) = Scanner::start(root.to_path_buf(), registry, bytes, waker, 7);
        (scanner, rx, wakes)
    }

    /// Receives events until one satisfies `done`, failing after `WAIT`.
    fn receive_until(
        rx: &Receiver<ScanEvent>,
        done: impl Fn(&ScanEvent) -> bool,
    ) -> Vec<ScanEvent> {
        let deadline = Instant::now() + WAIT;
        let mut events = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = rx.recv_timeout(remaining).unwrap_or_else(|err| {
                panic!(
                    "no matching event within {WAIT:?} ({err}); received {} events",
                    events.len()
                )
            });
            let finished = done(&event);
            events.push(event);
            if finished {
                return events;
            }
        }
    }

    /// Polls `condition` until it holds, failing after `WAIT`.
    fn wait_until(description: &str, condition: impl Fn() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !condition() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {description}"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn is_parse_pass_done(event: &ScanEvent) -> bool {
        matches!(event, ScanEvent::ParsePassDone { .. })
    }

    /// The generation every event of a scan carries.
    fn generation(event: &ScanEvent) -> u64 {
        match event {
            ScanEvent::Listed { generation, .. }
            | ScanEvent::ListFailed { generation, .. }
            | ScanEvent::FileParsed { generation, .. }
            | ScanEvent::Preview { generation, .. }
            | ScanEvent::ParsePassDone { generation } => *generation,
        }
    }

    fn listed_shots(events: &[ScanEvent]) -> &[Shot] {
        events
            .iter()
            .find_map(|e| match e {
                ScanEvent::Listed { shots, .. } => Some(shots.as_slice()),
                _ => None,
            })
            .expect("a Listed event")
    }

    fn outcome_for(events: &[ScanEvent], shot: ShotId, file: usize) -> &FileOutcome {
        events
            .iter()
            .find_map(|e| match e {
                ScanEvent::FileParsed {
                    shot: s,
                    file: f,
                    outcome,
                    ..
                } if *s == shot && *f == file => Some(outcome.as_ref()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no FileParsed for {shot:?} file {file}"))
    }

    /// The outcome of the only file of the shot whose first file is `name`.
    fn outcome_by_name<'a>(events: &'a [ScanEvent], name: &str) -> &'a FileOutcome {
        let shot = listed_shots(events)
            .iter()
            .find(|s| s.display_name() == name)
            .unwrap_or_else(|| panic!("no shot named {name}"));
        outcome_for(events, shot.id, 0)
    }

    fn parse_pass_done_count(events: &[ScanEvent]) -> usize {
        events.iter().filter(|e| is_parse_pass_done(e)).count()
    }

    /// A format that stores its metadata in a TIFF, registered only in tests.
    /// It reuses the shared EXIF reader, so the scan code needs no change.
    struct FakeTiffReader;

    impl FormatReader for FakeTiffReader {
        fn kind(&self) -> FileKind {
            FileKind("fkr")
        }

        fn extensions(&self) -> &'static [&'static str] {
            &["fkr"]
        }

        fn badge(&self) -> &'static str {
            "F"
        }

        fn is_raw(&self) -> bool {
            true
        }

        fn header_len_hint(&self) -> usize {
            16 * 1024
        }

        fn parse(&self, input: &ParseInput<'_>) -> Result<Metadata, ParseError> {
            read_tiff_exif(input, 0, &[TIFF_MAGIC]).map(|file| file.meta)
        }

        fn lens_write_supported(&self) -> bool {
            false
        }
    }

    /// `jpeg` with `count` COM segments of `payload` bytes after the SOI.
    fn with_com_padding(jpeg: &[u8], count: usize, payload: usize) -> Vec<u8> {
        let len = u16::try_from(payload + 2).expect("segment length fits in u16");
        let mut out = jpeg[..2].to_vec();
        for _ in 0..count {
            out.extend([0xFF, 0xFE]);
            out.extend(len.to_be_bytes());
            out.extend(std::iter::repeat_n(b'x', payload));
        }
        out.extend(&jpeg[2..]);
        out
    }

    // AC1.1

    #[test]
    fn extensions_match_in_any_case_and_recursively() {
        let dir = tempdir().expect("temp dir");
        let bytes = jpeg_with_exif();
        write_file(&dir.path().join("A.JPG"), &bytes);
        write_file(&dir.path().join("nested/b.jpg"), &bytes);
        write_file(&dir.path().join("nested/deeper/C.jpeg"), &bytes);
        write_file(&dir.path().join("nested/deeper/D.NeF"), &bytes);

        let (shots, skipped) =
            list_source_until(dir.path(), &Registry::with_builtin(), || false).expect("lists");

        assert!(skipped.is_empty());
        let mut names: Vec<String> = shots.iter().map(Shot::display_name).collect();
        names.sort();
        assert_eq!(names, ["A.JPG", "C.jpeg", "D.NeF", "b.jpg"]);
    }

    #[test]
    fn a_missing_source_is_a_scan_error() {
        let dir = tempdir().expect("temp dir");
        let err = list_source_until(&dir.path().join("gone"), &Registry::with_builtin(), || {
            false
        })
        .expect_err("no such folder");
        assert!(matches!(err, ScanError::Root { .. }));
    }

    #[test]
    fn a_file_given_as_the_source_is_not_a_folder() {
        let dir = tempdir().expect("temp dir");
        let file = dir.path().join("DSC_0001.JPG");
        write_file(&file, &jpeg_with_exif());
        let err =
            list_source_until(&file, &Registry::with_builtin(), || false).expect_err("a file");
        assert!(matches!(err, ScanError::NotADirectory { .. }));
    }

    // AC1.3

    #[test]
    fn unregistered_extensions_are_not_listed() {
        let dir = tempdir().expect("temp dir");
        write_file(&dir.path().join("DSC_0001.MOV"), b"movie");
        write_file(&dir.path().join("DSC_0001.xmp"), b"<x:xmpmeta/>");
        write_file(&dir.path().join("DSC_0001.dop"), b"dop");
        write_file(&dir.path().join("DSC_0002.JPG"), &jpeg_with_exif());

        let (shots, _) =
            list_source_until(dir.path(), &Registry::with_builtin(), || false).expect("lists");

        assert_eq!(shots.len(), 1);
        assert_eq!(shots[0].display_name(), "DSC_0002.JPG");
    }

    // AC9.1

    #[test]
    fn listing_seven_thousand_shots_takes_under_three_seconds() {
        let dir = tempdir().expect("temp dir");
        // Every file shares one EXIF-only JPEG, which keeps generation fast.
        let bytes = jpeg_with_exif();
        for folder in 100..107 {
            for number in 1..=1000 {
                let path = dir
                    .path()
                    .join(format!("DCIM/{folder}NZ_F/DSC_{number:04}.JPG"));
                write_file(&path, &bytes);
            }
        }
        let registry = Registry::with_builtin();

        let started = Instant::now();
        let (shots, skipped) = list_source_until(dir.path(), &registry, || false).expect("lists");
        let elapsed = started.elapsed();

        eprintln!("list_source over 7,000 shots took {elapsed:?}");
        assert!(skipped.is_empty());
        assert_eq!(shots.len(), 7000);
        assert!(elapsed < Duration::from_secs(3), "listing took {elapsed:?}");
    }

    // parse_file

    #[test]
    fn a_header_that_needs_a_second_read_still_parses() {
        let dir = tempdir().expect("temp dir");
        let plain = jpeg_file(
            Order::Big,
            Some(&ExifSpec::zf()),
            Some(&NikonSpec::zf(None)),
            None,
        );
        // Three 65 KB COM segments push the EXIF past the first 128 KB read.
        let padded = with_com_padding(&plain, 3, 65_000);
        let path = dir.path().join("DSC_0001.JPG");
        write_file(&path, &padded);

        let first_read = &padded[..128 * 1024];
        let needs_more = Registry::with_builtin().parse(
            jpeg::KIND,
            first_read,
            u64::try_from(padded.len()).expect("fits"),
        );
        assert!(
            matches!(needs_more, Err(ParseError::NeedMore { .. })),
            "the fixture must need a second read, got {needs_more:?}"
        );

        let outcome = parse_file(&path, jpeg::KIND, &Registry::with_builtin());

        let meta = outcome.meta.expect("parses after the second read");
        assert_eq!(meta.make.as_deref(), Some("NIKON CORPORATION"));
        assert_eq!(meta.date_time_original.as_deref(), Some(ZF_DTO));
        assert_eq!(meta.shot_counter, Some(75_036));
        assert!(matches!(
            outcome.identity,
            Some(IdentityKey::Counter {
                counter: 75_036,
                ..
            })
        ));
    }

    /// A JPEG cut inside its APP1 TIFF header cannot be read at all.
    #[test]
    fn a_jpeg_cut_inside_its_tiff_header_is_an_error_with_a_content_identity() {
        let dir = tempdir().expect("temp dir");
        let full = jpeg_with_exif();
        let path = dir.path().join("TRUNC_0001.JPG");
        write_file(&path, &full[..16]);

        let outcome = parse_file(&path, jpeg::KIND, &Registry::with_builtin());

        assert!(outcome.meta.is_err());
        assert!(matches!(
            outcome.identity,
            Some(IdentityKey::Content { .. })
        ));
    }

    /// A JPEG cut inside its APP1 segment is an error, not partial metadata.
    /// The segment's declared end is past end-of-file, so it can never parse.
    #[test]
    fn a_jpeg_cut_inside_its_app1_segment_is_an_error_with_a_content_identity() {
        let dir = tempdir().expect("temp dir");
        let full = jpeg_with_exif();
        let path = dir.path().join("TRUNC_0001.JPG");
        write_file(&path, &full[..100]);

        let outcome = parse_file(&path, jpeg::KIND, &Registry::with_builtin());

        assert!(outcome.meta.is_err(), "{:?}", outcome.meta);
        assert!(matches!(
            outcome.identity,
            Some(IdentityKey::Content { .. })
        ));
    }

    #[test]
    fn a_zero_byte_nef_is_an_error_without_an_identity() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("EMPTY_0001.NEF");
        write_file(&path, &[]);

        let outcome = parse_file(&path, nef::KIND, &Registry::with_builtin());

        assert!(outcome.meta.is_err());
        assert_eq!(outcome.identity, None);
    }

    #[test]
    fn worker_count_is_the_core_count_capped_at_four() {
        assert_eq!(worker_count(1), 1);
        assert_eq!(worker_count(4), 4);
        assert_eq!(worker_count(16), 4);
    }

    // AC12.1

    #[test]
    fn a_registered_test_format_is_scanned_and_grouped_with_its_jpeg() {
        let mut registry = Registry::with_builtin();
        registry
            .register_format(Box::new(FakeTiffReader))
            .expect("fkr is free");
        let dir = tempdir().expect("temp dir");
        let exif = ExifSpec::zf();
        let nikon = NikonSpec::zf(None);
        write_file(
            &dir.path().join("IMG_0001.fkr"),
            &tiff_file(Order::Little, TIFF_MAGIC, &exif, Some(&nikon)),
        );
        write_file(
            &dir.path().join("IMG_0001.JPG"),
            &jpeg_file(Order::Little, Some(&exif), Some(&nikon), None),
        );

        let (_scanner, rx, _) = start_scan(dir.path(), Arc::new(registry));
        let events = receive_until(&rx, is_parse_pass_done);

        let shots = listed_shots(&events);
        assert_eq!(shots.len(), 1, "one shot for the two files");
        assert_eq!(shots[0].files.len(), 2);
        let fake = shots[0]
            .files
            .iter()
            .position(|f| f.kind == FileKind("fkr"))
            .expect("the fkr file is in the shot");
        let outcome = outcome_for(&events, shots[0].id, fake);
        assert!(outcome.meta.is_ok(), "{:?}", outcome.meta);
        assert!(matches!(
            outcome.identity,
            Some(IdentityKey::Counter {
                counter: 75_036,
                ..
            })
        ));
    }

    // Scanner

    #[test]
    fn every_file_gets_exactly_one_parse_event_and_one_parse_pass_done() {
        let dir = tempdir().expect("temp dir");
        let preview = tiny_jpeg(640, 424);
        let exif = ExifSpec::zf();
        let nikon = NikonSpec::zf(Some(preview.clone()));
        write_file(
            &dir.path().join("100NZ_F/DSC_0001.JPG"),
            &jpeg_file(Order::Big, Some(&exif), None, Some(&preview)),
        );
        write_file(
            &dir.path().join("100NZ_F/DSC_0001.NEF"),
            &nef_file(Order::Big, &exif, Some(&nikon), None, 0),
        );
        write_file(&dir.path().join("100NZ_F/DSC_0002.JPG"), &jpeg_with_exif());
        write_file(
            &dir.path().join("101NZ_F/DSC_0003.NEF"),
            &nef_file(Order::Big, &exif, Some(&nikon), None, 0),
        );

        let (_scanner, rx, _) = start_scan(dir.path(), Arc::new(Registry::with_builtin()));
        let events = receive_until(&rx, is_parse_pass_done);

        let expected: HashSet<(ShotId, usize)> = listed_shots(&events)
            .iter()
            .flat_map(|shot| (0..shot.files.len()).map(move |file| (shot.id, file)))
            .collect();
        let parsed: Vec<(ShotId, usize)> = events
            .iter()
            .filter_map(|e| match e {
                ScanEvent::FileParsed { shot, file, .. } => Some((*shot, *file)),
                _ => None,
            })
            .collect();
        assert_eq!(parsed.len(), expected.len(), "one FileParsed per file");
        assert_eq!(parsed.into_iter().collect::<HashSet<_>>(), expected);
        assert_eq!(parse_pass_done_count(&events), 1);
        assert!(events.iter().all(|e| generation(e) == 7));
    }

    #[test]
    fn corrupt_files_get_error_outcomes_while_the_rest_parse() {
        let dir = tempdir().expect("temp dir");
        let valid = jpeg_with_exif();
        write_file(&dir.path().join("good.JPG"), &valid);
        write_file(&dir.path().join("trunc.JPG"), &valid[..100]);
        write_file(&dir.path().join("empty.NEF"), &[]);
        write_file(&dir.path().join("clip.MOV"), b"movie");
        write_file(&dir.path().join("sidecar.xmp"), b"<x:xmpmeta/>");
        write_file(&dir.path().join("notes.dop"), b"dop");

        let (_scanner, rx, _) = start_scan(dir.path(), Arc::new(Registry::with_builtin()));
        let events = receive_until(&rx, is_parse_pass_done);

        let names: Vec<String> = listed_shots(&events)
            .iter()
            .map(Shot::display_name)
            .collect();
        assert_eq!(names, ["empty.NEF", "good.JPG", "trunc.JPG"]);
        assert!(outcome_by_name(&events, "good.JPG").meta.is_ok());
        assert!(outcome_by_name(&events, "trunc.JPG").meta.is_err());
        assert!(outcome_by_name(&events, "empty.NEF").meta.is_err());
    }

    #[test]
    fn an_empty_source_lists_nothing_and_still_sends_parse_pass_done() {
        let dir = tempdir().expect("temp dir");
        let (_scanner, rx, _) = start_scan(dir.path(), Arc::new(Registry::with_builtin()));

        let events = receive_until(&rx, is_parse_pass_done);

        assert_eq!(events.len(), 2, "a Listed event, then ParsePassDone");
        assert!(matches!(
            &events[0],
            ScanEvent::Listed { shots, skipped, .. } if shots.is_empty() && skipped.is_empty()
        ));
        assert!(events.iter().all(|e| generation(e) == 7));
    }

    #[test]
    fn a_missing_source_sends_list_failed() {
        let dir = tempdir().expect("temp dir");
        let (_scanner, rx, _) =
            start_scan(&dir.path().join("gone"), Arc::new(Registry::with_builtin()));

        let events = receive_until(&rx, |e| matches!(e, ScanEvent::ListFailed { .. }));

        let Some(ScanEvent::ListFailed { generation, error }) = events.last() else {
            panic!("the last event is ListFailed");
        };
        assert_eq!(*generation, 7);
        assert!(error.contains("gone"), "{error}");
    }

    #[test]
    fn a_requested_preview_arrives_decoded_at_its_size() {
        let dir: TempDir = tempdir().expect("temp dir");
        let preview = tiny_jpeg(640, 424);
        write_file(
            &dir.path().join("DSC_0001.JPG"),
            &jpeg_file(Order::Big, Some(&ExifSpec::zf()), None, Some(&preview)),
        );
        let registry = Arc::new(Registry::with_builtin());
        let (scanner, rx, _) = start_scan(dir.path(), Arc::clone(&registry));
        let events = receive_until(&rx, is_parse_pass_done);

        let mut shot = listed_shots(&events)[0].clone();
        for (index, file) in shot.files.iter_mut().enumerate() {
            if let Ok(meta) = &outcome_for(&events, shot.id, index).meta {
                file.meta = Some(meta.clone());
            }
        }
        let (index, range) = shot
            .tile_preview(&registry)
            .expect("the JPEG has an embedded preview");
        let orientation = shot.files[index].meta.as_ref().and_then(|m| m.orientation);
        scanner.set_visible(vec![shot.id]);
        scanner.request_preview(shot.id, shot.files[index].path.clone(), range, orientation);

        let events = receive_until(&rx, |e| matches!(e, ScanEvent::Preview { .. }));
        let Some(ScanEvent::Preview {
            generation,
            shot: id,
            result,
        }) = events.last()
        else {
            panic!("the last event is a Preview");
        };
        assert_eq!(*generation, 7);
        assert_eq!(*id, shot.id);
        assert_eq!(
            result.as_ref().expect("the preview decodes").size,
            [640, 424]
        );
    }

    /// The tile bound applies on the scan path, not only inside `decode_preview`.
    /// A 2000 × 1500 embedded preview comes back fitted to 1280 × 960.
    #[test]
    fn a_requested_preview_larger_than_the_tile_bound_is_fitted() {
        let dir: TempDir = tempdir().expect("temp dir");
        write_file(
            &dir.path().join("DSC_0001.JPG"),
            &jpeg_file(
                Order::Big,
                Some(&ExifSpec::zf()),
                None,
                Some(&tiny_jpeg(2000, 1500)),
            ),
        );
        let registry = Arc::new(Registry::with_builtin());
        let (scanner, rx, _) = start_scan(dir.path(), Arc::clone(&registry));
        let events = receive_until(&rx, is_parse_pass_done);

        let mut shot = listed_shots(&events)[0].clone();
        for (index, file) in shot.files.iter_mut().enumerate() {
            if let Ok(meta) = &outcome_for(&events, shot.id, index).meta {
                file.meta = Some(meta.clone());
            }
        }
        let (index, range) = shot
            .tile_preview(&registry)
            .expect("the JPEG has an embedded preview");
        let orientation = shot.files[index].meta.as_ref().and_then(|m| m.orientation);
        scanner.set_visible(vec![shot.id]);
        scanner.request_preview(shot.id, shot.files[index].path.clone(), range, orientation);

        let mut events = receive_until(&rx, |e| matches!(e, ScanEvent::Preview { .. }));
        let Some(ScanEvent::Preview { result, .. }) = events.pop() else {
            panic!("the last event is a Preview");
        };
        assert_eq!(result.expect("the preview decodes").size, [1280, 960]);
    }

    #[test]
    fn every_event_is_followed_by_a_wake() {
        let dir = tempdir().expect("temp dir");
        write_file(&dir.path().join("DSC_0001.JPG"), &jpeg_with_exif());
        write_file(&dir.path().join("DSC_0002.JPG"), &jpeg_with_exif());
        let (_scanner, rx, wakes) = start_scan(dir.path(), Arc::new(Registry::with_builtin()));
        let events = receive_until(&rx, is_parse_pass_done);
        let expected = events.len();

        wait_until("one wake per event", || {
            wakes.load(Ordering::SeqCst) == expected
        });
    }

    #[test]
    fn the_last_parse_to_finish_reports_the_pass_done_exactly_once() {
        let shared = Shared::default();
        let jobs = (0..3)
            .map(|i| Job::Parse {
                shot: ShotId(i),
                file: 0,
                path: PathBuf::from(format!("{i}.jpg")),
                kind: FileKind("jpeg"),
            })
            .collect();
        shared.enqueue_parse_pass(jobs);

        let finished = [
            shared.finish_parse(),
            shared.finish_parse(),
            shared.finish_parse(),
            shared.finish_parse(),
        ];
        assert_eq!(finished, [false, false, true, false]);
    }

    #[test]
    fn dropping_the_receiver_and_the_scanner_mid_scan_stops_every_thread() {
        let dir = tempdir().expect("temp dir");
        for number in 1..=200 {
            write_file(
                &dir.path().join(format!("DSC_{number:04}.JPG")),
                &jpeg_with_exif(),
            );
        }
        let (scanner, rx, wakes) = start_scan(dir.path(), Arc::new(Registry::with_builtin()));
        let probe = Arc::downgrade(&wakes);
        drop(wakes);
        receive_until(&rx, |e| matches!(e, ScanEvent::Listed { .. }));

        drop(rx);
        drop(scanner);

        // Each thread holds the waker. Once every thread has exited, it is dropped.
        wait_until("every scan thread to exit", || probe.upgrade().is_none());
    }

    /// Once the pass is done the workers sit idle in `Condvar::wait`, and the
    /// receiver is still alive, so only the stop flag can wake them.
    #[test]
    fn dropping_only_the_scanner_stops_the_idle_workers() {
        let dir = tempdir().expect("temp dir");
        for number in 1..=3 {
            write_file(
                &dir.path().join(format!("DSC_{number:04}.JPG")),
                &jpeg_with_exif(),
            );
        }
        let (scanner, rx, wakes) = start_scan(dir.path(), Arc::new(Registry::with_builtin()));
        let probe = Arc::downgrade(&wakes);
        drop(wakes);
        receive_until(&rx, is_parse_pass_done);

        drop(scanner);

        wait_until("the idle workers to exit", || probe.upgrade().is_none());
        drop(rx);
    }

    /// With the queue full and nobody draining it, the workers block in `send`.
    /// Dropping the scanner must still return, and the threads exit once the
    /// receiver is gone.
    #[test]
    fn a_full_queue_does_not_block_dropping_the_scanner() {
        let dir = tempdir().expect("temp dir");
        for number in 1..=50 {
            write_file(
                &dir.path().join(format!("DSC_{number:04}.JPG")),
                &jpeg_with_exif(),
            );
        }
        let (waker, wakes) = counting_waker();
        let bytes = Arc::new(preview_bytes(&CacheSplit {
            bytes: 64 * MIB,
            textures: 0,
        }));
        let (scanner, rx) = Scanner::start_with_capacity(
            dir.path().to_path_buf(),
            Arc::new(Registry::with_builtin()),
            bytes,
            waker,
            7,
            2,
            load_preview,
        );
        let probe = Arc::downgrade(&wakes);
        receive_until(&rx, |e| matches!(e, ScanEvent::Listed { .. }));
        // One event is taken, and each send wakes once after it lands. Three
        // wakes therefore mean the two-slot queue holds two events.
        wait_until("the queue to fill", || wakes.load(Ordering::SeqCst) >= 3);

        drop(scanner);
        drop(wakes);
        drop(rx);

        wait_until("every scan thread to exit", || probe.upgrade().is_none());
    }

    /// A format whose parse panics. Registered only in tests.
    struct PanickingReader;

    impl FormatReader for PanickingReader {
        fn kind(&self) -> FileKind {
            FileKind("pnc")
        }

        fn extensions(&self) -> &'static [&'static str] {
            &["pnc"]
        }

        fn badge(&self) -> &'static str {
            "P"
        }

        fn is_raw(&self) -> bool {
            false
        }

        fn header_len_hint(&self) -> usize {
            16 * 1024
        }

        fn parse(&self, _input: &ParseInput<'_>) -> Result<Metadata, ParseError> {
            panic!("reader exploded")
        }

        fn lens_write_supported(&self) -> bool {
            false
        }
    }

    /// A format that always asks for `needed` bytes. Registered only in tests.
    struct NeedsMoreReader {
        needed: u64,
    }

    impl FormatReader for NeedsMoreReader {
        fn kind(&self) -> FileKind {
            FileKind("nmr")
        }

        fn extensions(&self) -> &'static [&'static str] {
            &["nmr"]
        }

        fn badge(&self) -> &'static str {
            "N"
        }

        fn is_raw(&self) -> bool {
            false
        }

        fn header_len_hint(&self) -> usize {
            16 * 1024
        }

        fn parse(&self, input: &ParseInput<'_>) -> Result<Metadata, ParseError> {
            Err(ParseError::NeedMore {
                needed: self.needed,
                have: u64::try_from(input.head.len()).expect("fits"),
            })
        }

        fn lens_write_supported(&self) -> bool {
            false
        }
    }

    /// The error text for a file of `file_bytes` whose reader asks for `needed`.
    fn need_more_error(needed: u64, file_bytes: &[u8]) -> String {
        let mut registry = Registry::with_builtin();
        registry
            .register_format(Box::new(NeedsMoreReader { needed }))
            .expect("nmr is free");
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("DSC_0001.nmr");
        write_file(&path, file_bytes);
        parse_file(&path, FileKind("nmr"), &registry)
            .meta
            .expect_err("the header cannot be read")
    }

    #[test]
    fn a_header_larger_than_the_head_limit_says_so() {
        let message = need_more_error(MAX_HEAD_LEN + 1, &[0; 100]);
        assert!(message.contains("larger than the"), "{message}");
        assert!(message.contains("head limit"), "{message}");
        assert!(!message.contains("cut off"), "{message}");
    }

    #[test]
    fn a_header_that_runs_past_the_file_says_it_is_cut_off() {
        let message = need_more_error(200, &[0; 100]);
        assert!(message.contains("cut off"), "{message}");
        assert!(message.contains("100 bytes"), "{message}");
    }

    #[test]
    fn a_reader_asking_for_bytes_it_already_has_is_reported_as_such() {
        let message = need_more_error(50, &[0; 100]);
        assert!(message.contains("asked for 50 bytes"), "{message}");
        assert!(!message.contains("cut off"), "{message}");
    }

    #[test]
    fn catch_panic_turns_a_panic_into_its_message() {
        assert_eq!(catch_panic(|| 7), Ok(7));
        assert_eq!(
            catch_panic(|| -> u8 { panic!("boom") }),
            Err("worker panicked: boom".to_owned())
        );
        assert_eq!(
            catch_panic(|| -> u8 { panic!("{}", "formatted") }),
            Err("worker panicked: formatted".to_owned())
        );
    }

    /// A panicking parse still reports, so the pass ends and the other shots parse.
    #[test]
    fn a_panicking_parse_reports_an_error_and_the_pass_still_ends() {
        let mut registry = Registry::with_builtin();
        registry
            .register_format(Box::new(PanickingReader))
            .expect("pnc is free");
        let dir = tempdir().expect("temp dir");
        write_file(&dir.path().join("DSC_0001.pnc"), b"x");
        write_file(&dir.path().join("DSC_0002.JPG"), &jpeg_with_exif());

        let (_scanner, rx, _) = start_scan(dir.path(), Arc::new(registry));
        let events = receive_until(&rx, is_parse_pass_done);

        assert_eq!(parse_pass_done_count(&events), 1);
        let panicked = outcome_by_name(&events, "DSC_0001.pnc");
        assert!(
            matches!(&panicked.meta, Err(message) if message == "worker panicked: reader exploded"),
            "{:?}",
            panicked.meta
        );
        assert!(outcome_by_name(&events, "DSC_0002.JPG").meta.is_ok());
    }

    /// A preview loader that always panics. Only the panic-guard test uses it.
    fn panicking_preview_loader(
        _bytes: &PreviewBytes,
        _shot: ShotId,
        _path: &Path,
        _range: ByteRange,
        _orientation: Option<u16>,
    ) -> Result<DecodedImage, String> {
        panic!("preview exploded")
    }

    /// A panicking preview job still reports its outcome, so its tile does not
    /// wait forever and the worker keeps running.
    #[test]
    fn a_panicking_preview_job_reports_an_error() {
        let dir = tempdir().expect("temp dir");
        let (waker, _) = counting_waker();
        let bytes = Arc::new(preview_bytes(&CacheSplit {
            bytes: 64 * MIB,
            textures: 0,
        }));
        let (scanner, rx) = Scanner::start_with_capacity(
            dir.path().to_path_buf(),
            Arc::new(Registry::with_builtin()),
            bytes,
            waker,
            7,
            EVENT_CAPACITY,
            panicking_preview_loader,
        );
        let shot = ShotId(0);
        scanner.request_preview(
            shot,
            dir.path().join("DSC_0001.JPG"),
            ByteRange { offset: 0, len: 1 },
            None,
        );

        let mut events = receive_until(&rx, |e| matches!(e, ScanEvent::Preview { .. }));
        let Some(ScanEvent::Preview {
            shot: id, result, ..
        }) = events.pop()
        else {
            panic!("the last event is a Preview");
        };
        assert_eq!(id, shot);
        assert!(
            matches!(&result, Err(message) if message == "worker panicked: preview exploded"),
            "{result:?}"
        );
    }
}
