//! photosort desktop app entry point.
// pattern: Imperative Shell
// Release builds are GUI-only; debug builds keep the console for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
#[cfg_attr(not(test), expect(dead_code, reason = "wired into the app in Task 8"))]
mod budget;
#[cfg_attr(not(test), expect(dead_code, reason = "wired into the app in Task 8"))]
mod cache;
#[cfg_attr(not(test), expect(dead_code, reason = "wired into the app in Task 8"))]
mod preview;
#[cfg_attr(not(test), expect(dead_code, reason = "wired into the app in Task 8"))]
mod scan;
#[cfg_attr(not(test), expect(dead_code, reason = "wired into the app in Task 8"))]
mod scan_queue;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("photosort")
            .with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "photosort",
        options,
        Box::new(|_cc| Ok(Box::new(app::PhotosortApp))),
    )
}
