//! Top-level egui application.
// pattern: Imperative Shell

use eframe::egui;

pub(crate) struct PhotosortApp;

impl eframe::App for PhotosortApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("photosort");
        });
    }
}
