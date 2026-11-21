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
use std::sync::mpsc::SyncSender;

use eframe::egui;

fn main() -> eframe::Result {
    let frame_log = frame_log_from_env();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("photosort")
            .with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "photosort",
        options,
        Box::new(move |cc| Ok(Box::new(app::PhotosortApp::new(cc, frame_log)))),
    )
}

/// The frame log is opt-in: `PHOTOSORT_FRAME_LOG` names the file to write.
/// If the file cannot be created, the app runs without the log.
fn frame_log_from_env() -> Option<SyncSender<String>> {
    let path = std::env::var_os("PHOTOSORT_FRAME_LOG")?;
    match frame_log::spawn(Path::new(&path)) {
        Ok(sender) => Some(sender),
        Err(err) => {
            eprintln!("photosort: frame log disabled, failed to create the file: {err}");
            None
        }
    }
}
