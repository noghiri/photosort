//! Per-frame timing log for checking the responsiveness criteria. A writer
//! thread owns the file, so the UI thread never does file I/O.
// pattern: Imperative Shell

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;

/// Lines between flushes. A crash loses at most this many lines.
const FLUSH_EVERY: usize = 120;
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

/// Opens `path` and starts the writer thread. The returned sender is the only
/// way to write; dropping it ends the thread after it flushes.
pub(crate) fn spawn(path: &Path) -> io::Result<SyncSender<String>> {
    let file = File::create(path)?;
    let (tx, rx) = mpsc::sync_channel(QUEUE_CAPACITY);
    thread::Builder::new()
        .name("photosort-frame-log".to_owned())
        .spawn(move || write_lines(BufWriter::new(file), &rx))?;
    Ok(tx)
}

/// Sends a line without blocking. A full queue drops the line.
pub(crate) fn send(tx: &SyncSender<String>, line: String) {
    match tx.try_send(line) {
        Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
    }
}

fn write_lines(mut out: impl Write, rx: &Receiver<String>) {
    let mut unflushed = 0;
    for line in rx {
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
    // The channel closed, so this is the last chance to flush the tail.
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_line_has_cpu_milliseconds_then_the_counts() {
        assert_eq!(frame_line(Some(0.0123), 40, 3), "12.3 40 3");
    }

    #[test]
    fn the_first_frame_has_no_cpu_figure() {
        assert_eq!(frame_line(None, 0, 0), "- 0 0");
    }

    #[test]
    fn the_writer_writes_every_line_once_the_channel_closes() {
        let (tx, rx) = mpsc::sync_channel(QUEUE_CAPACITY);
        for i in 0..(FLUSH_EVERY + 5) {
            tx.send(format!("line {i}")).expect("queue has room");
        }
        drop(tx);
        let mut out = Vec::new();
        write_lines(&mut out, &rx);
        let text = String::from_utf8(out).expect("utf-8");
        assert_eq!(text.lines().count(), FLUSH_EVERY + 5);
        assert!(text.ends_with("line 124\n"));
    }

    #[test]
    fn a_full_queue_drops_the_line_instead_of_blocking() {
        let (tx, _rx) = mpsc::sync_channel(1);
        send(&tx, "kept".to_owned());
        send(&tx, "dropped".to_owned());
    }
}
