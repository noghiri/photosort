//! photosort desktop app entry point.
// pattern: Imperative Shell
// Release builds are GUI-only; debug builds keep the console for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;

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
