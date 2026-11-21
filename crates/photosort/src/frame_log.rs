//! Per-frame timing log for checking the responsiveness criteria. A writer
//! thread owns the file, so the UI thread never does file I/O.
// pattern: Imperative Shell

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Lines between flushes while lines keep arriving. A crash loses at most this many lines.
const FLUSH_EVERY: usize = 120;
/// With no new line for this long, the writer flushes what it holds, so a short
/// run still reaches the file.
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);
/// Lines queued for the writer. A full queue drops lines rather than blocking
/// the UI thread.
const QUEUE_CAPACITY: usize = 4096;

/// One log line: CPU time of the previous frame in ms (`-` on the first
/// frame), then the scan events handled and the textures uploaded this frame.
pub(crate) fn frame_line(cpu_seconds: Option<f32>, events: usize, uploads: usize) -> String {
    let cpu = match cpu_seconds {
        Some(seconds) => format!("{:.1}", seconds * 1000.0),
        None => "-".to_owned(),
    };
    format!("{cpu} {events} {uploads}")
}

/// The UI thread's end of the log. Sending never blocks. A line that does not
/// fit in the queue is counted, and the writer reports the count when the log
/// closes, so a gap in the file is never silent.
pub(crate) struct FrameLog {
    tx: SyncSender<String>,
    dropped: Arc<AtomicU64>,
}

impl FrameLog {
    /// Queues a line without blocking. A line that is not queued is counted.
    pub(crate) fn send(&self, line: String) {
        if self.tx.try_send(line).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Opens `path` and starts the writer thread. Dropping the returned `FrameLog`
/// ends the writer once it has written and flushed the queue. Join the handle
/// to wait for that.
pub(crate) fn spawn(path: &Path) -> io::Result<(FrameLog, JoinHandle<()>)> {
    let file = File::create(path)?;
    let (log, rx) = channel(QUEUE_CAPACITY);
    let dropped = Arc::clone(&log.dropped);
    let writer = thread::Builder::new()
        .name("photosort-frame-log".to_owned())
        .spawn(move || write_lines(BufWriter::new(file), &rx, &dropped, FLUSH_INTERVAL))?;
    Ok((log, writer))
}

fn channel(capacity: usize) -> (FrameLog, Receiver<String>) {
    let (tx, rx) = mpsc::sync_channel(capacity);
    (
        FrameLog {
            tx,
            dropped: Arc::new(AtomicU64::new(0)),
        },
        rx,
    )
}

/// Writes lines until the sender is dropped. Flushes after `FLUSH_EVERY` lines,
/// and after `interval` with no new line. At the end it writes `# dropped N`,
/// so the count is in the file. The `#` keeps it apart from the numeric lines.
fn write_lines(
    mut out: impl Write,
    rx: &Receiver<String>,
    dropped: &AtomicU64,
    interval: Duration,
) {
    let mut unflushed = 0;
    loop {
        match rx.recv_timeout(interval) {
            Ok(line) => {
                if writeln!(out, "{line}").is_err() {
                    return;
                }
                unflushed += 1;
                if unflushed == FLUSH_EVERY {
                    if out.flush().is_err() {
                        return;
                    }
                    unflushed = 0;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if out.flush().is_err() {
                    return;
                }
                unflushed = 0;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = writeln!(out, "# dropped {}", dropped.load(Ordering::Relaxed));
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Instant;

    use super::*;

    /// What a `Sink` received, and how often it was flushed. The test keeps a
    /// handle, so it can look while the writer runs.
    #[derive(Default)]
    struct Recorded {
        bytes: Vec<u8>,
        flushes: usize,
    }

    impl Recorded {
        fn text(&self) -> String {
            String::from_utf8(self.bytes.clone()).expect("utf-8")
        }
    }

    /// A writer that records into shared state, so a missing flush is visible.
    struct Sink(Arc<Mutex<Recorded>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .expect("sink lock")
                .bytes
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.lock().expect("sink lock").flushes += 1;
            Ok(())
        }
    }

    fn sink() -> (Sink, Arc<Mutex<Recorded>>) {
        let recorded = Arc::new(Mutex::new(Recorded::default()));
        (Sink(Arc::clone(&recorded)), recorded)
    }

    #[test]
    fn a_frame_line_has_cpu_milliseconds_then_the_counts() {
        assert_eq!(frame_line(Some(0.0123), 40, 3), "12.3 40 3");
    }

    #[test]
    fn the_first_frame_has_no_cpu_figure() {
        assert_eq!(frame_line(None, 0, 0), "- 0 0");
    }

    #[test]
    fn the_writer_writes_every_line_then_the_drop_count_once_the_log_closes() {
        let (log, rx) = channel(QUEUE_CAPACITY);
        let dropped = Arc::clone(&log.dropped);
        for i in 0..(FLUSH_EVERY + 5) {
            log.send(format!("line {i}"));
        }
        drop(log);
        let (out, recorded) = sink();

        write_lines(out, &rx, &dropped, FLUSH_INTERVAL);

        let text = recorded.lock().expect("sink lock").text();
        assert_eq!(text.lines().count(), FLUSH_EVERY + 6);
        assert!(text.ends_with("line 124\n# dropped 0\n"), "{text}");
    }

    /// Two lines are far below `FLUSH_EVERY`, and the log stays open, so only
    /// the timer can make them reach the sink.
    #[test]
    fn a_quiet_writer_flushes_on_the_timer_before_the_log_closes() {
        let (log, rx) = channel(QUEUE_CAPACITY);
        let dropped = Arc::clone(&log.dropped);
        let (out, recorded) = sink();
        let writer =
            thread::spawn(move || write_lines(out, &rx, &dropped, Duration::from_millis(10)));
        log.send("first".to_owned());
        log.send("second".to_owned());

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            {
                let seen = recorded.lock().expect("sink lock");
                if seen.flushes > 0 {
                    assert_eq!(seen.text(), "first\nsecond\n");
                    break;
                }
            }
            assert!(Instant::now() < deadline, "the timer never flushed");
            thread::sleep(Duration::from_millis(5));
        }

        drop(log);
        writer.join().expect("writer ends");
        assert!(
            recorded
                .lock()
                .expect("sink lock")
                .text()
                .ends_with("# dropped 0\n")
        );
    }

    /// The queue holds one line, so the next two are not queued. The count
    /// reaches the file when the log closes.
    #[test]
    fn a_full_queue_drops_the_line_and_the_count_reaches_the_file() {
        let (log, rx) = channel(1);
        let dropped = Arc::clone(&log.dropped);
        log.send("kept".to_owned());
        log.send("dropped one".to_owned());
        log.send("dropped two".to_owned());
        assert_eq!(dropped.load(Ordering::Relaxed), 2);
        drop(log);
        let (out, recorded) = sink();

        write_lines(out, &rx, &dropped, FLUSH_INTERVAL);

        assert_eq!(
            recorded.lock().expect("sink lock").text(),
            "kept\n# dropped 2\n"
        );
    }

    #[test]
    fn spawn_writes_the_file_and_the_writer_ends_once_the_log_is_dropped() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("frames.log");
        let (log, writer) = spawn(&path).expect("the file opens");
        log.send("12.3 40 3".to_owned());
        drop(log);

        writer.join().expect("writer ends");

        assert_eq!(
            std::fs::read_to_string(&path).expect("log is readable"),
            "12.3 40 3\n# dropped 0\n"
        );
    }
}
