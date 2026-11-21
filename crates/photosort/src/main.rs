//! photosort desktop app entry point.
// pattern: Imperative Shell
// Release builds are GUI-only; debug builds keep the console for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod budget;
mod cache;
mod frame_log;
mod preview;
mod scan;
mod scan_model;
mod scan_queue;
mod ui;

use std::path::Path;
use std::thread::JoinHandle;

use eframe::egui;

fn main() -> eframe::Result {
    // Only the app holds the log's sender. `run_native` drops the app before it
    // returns, which ends the writer, so joining the writer afterwards cannot wait
    // on a sender that is still alive.
    let (frame_log, writer) = match frame_log_from_env() {
        Some((log, writer)) => (Some(log), Some(writer)),
        None => (None, None),
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("photosort")
            .with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        "photosort",
        options,
        Box::new(move |cc| Ok(Box::new(app::PhotosortApp::new(cc, frame_log)))),
    );
    // Without the join, the process could exit before the writer flushes its tail.
    if let Some(writer) = writer
        && writer.join().is_err()
    {
        eprintln!("photosort: the frame log writer panicked");
    }
    result
}

/// The frame log is opt-in: `PHOTOSORT_FRAME_LOG` names the file to write.
/// If the file cannot be created, the app runs without the log.
fn frame_log_from_env() -> Option<(frame_log::FrameLog, JoinHandle<()>)> {
    let path = std::env::var_os("PHOTOSORT_FRAME_LOG")?;
    match frame_log::spawn(Path::new(&path)) {
        Ok(opened) => Some(opened),
        Err(err) => {
            eprintln!("photosort: frame log disabled, failed to create the file: {err}");
            None
        }
    }
}
