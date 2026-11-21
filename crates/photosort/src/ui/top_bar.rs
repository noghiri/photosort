//! Top bar: source picker, rescan, tile-size slider and status line.
// pattern: Imperative Shell

use eframe::egui;

use crate::app::PhotosortApp;
use crate::scan_model::{Phase, status_text};
use crate::ui::grid_math::{TILE_MAX, TILE_MIN};

/// What the top bar asks the app to do. The app acts on it after drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TopBarAction {
    None,
    PickFolder,
    Rescan,
}

pub(crate) fn show(ui: &mut egui::Ui, app: &mut PhotosortApp) -> TopBarAction {
    let mut action = TopBarAction::None;
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(app.folder_pick.is_none(), egui::Button::new("Source…"))
            .clicked()
        {
            action = TopBarAction::PickFolder;
        }
        if ui
            .add_enabled(app.source.is_some(), egui::Button::new("Rescan"))
            .clicked()
        {
            action = TopBarAction::Rescan;
        }
        let source = app.source.as_ref().map_or_else(
            || "no source folder".to_owned(),
            |p| p.display().to_string(),
        );
        ui.label(source);
        ui.add(egui::Slider::new(&mut app.tile_size, TILE_MIN..=TILE_MAX).text("tile size"));
        let color = if matches!(app.phase, Phase::Failed(_)) {
            ui.visuals().error_fg_color
        } else {
            ui.visuals().text_color()
        };
        ui.colored_label(color, status_text(&app.phase, &app.status));
    });
    action
}
