//! Virtualized thumbnail grid. Draws only the visible rows, applies this
//! frame's clicks after drawing, and asks the scanner for the previews the
//! visible tiles need.
// pattern: Imperative Shell

use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;

use eframe::egui::{
    self, Align2, Color32, FontId, Id, Rect, Sense, Stroke, StrokeKind, pos2, vec2,
};
use photosort_core::metadata::Registry;
use photosort_core::model::{Shot, ShotId};

use crate::app::{GridAction, PhotosortApp};
use crate::cache::Textures;
use crate::scan_model::{Phase, is_unreadable, preview_source, row_count, shot_at, visible_ids};
use crate::ui::grid_math::{
    Selection, fit_within, layout, previews_to_request, selected_range, toggle_ticks, truncate_name,
};

/// Gap between tiles, both ways.
pub(crate) const SPACING: f32 = 8.0;
/// Height under each tile, for its file name.
pub(crate) const CAPTION: f32 = 18.0;
const CHECK_SIZE: f32 = 20.0;
const CHECK_INSET: f32 = 6.0;
/// Rough glyph width at the caption size, used to cut names to the tile.
const GLYPH_WIDTH: f32 = 7.0;

/// What one tile asks for this frame. Only one tile can be clicked per frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TileEvent {
    None,
    Toggle(ShotId),
    Open { position: usize, shot: ShotId },
}

/// What drawing reads from the app. Borrowed, so drawing cannot change state.
struct Tiles<'a> {
    registry: &'a Registry,
    textures: &'a Textures,
    ticks: &'a HashSet<ShotId>,
    in_flight: &'a HashSet<ShotId>,
    failed: &'a HashMap<ShotId, String>,
    selection: Selection,
    range: RangeInclusive<usize>,
    tile: f32,
}

pub(crate) fn show(ui: &mut egui::Ui, app: &mut PhotosortApp) -> Option<GridAction> {
    // Ctrl+scroll resizes the tiles while the pointer is over the grid.
    if ui.rect_contains_pointer(ui.max_rect()) {
        let zoom = ui.input(|input| input.zoom_delta());
        app.zoom_from_wheel(zoom);
    }
    if app.order.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label(empty_text(&app.phase));
        });
        return None;
    }

    // The scrollbar takes space from the row, so the columns are fitted to what is left.
    let width = ui.available_width() - ui.spacing().scroll.allocated_width();
    let grid = layout(width, app.tile_size, SPACING, CAPTION);
    app.grid_layout = grid;
    let rows = row_count(app.order.len(), grid.columns);
    let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
    if let Some(offset) = app.scroll_to.take() {
        scroll = scroll.vertical_scroll_offset(offset);
    }
    // show_rows reads this spacing for its row stride.
    ui.spacing_mut().item_spacing = vec2(SPACING, SPACING);

    let tiles = Tiles {
        registry: &app.registry,
        textures: &app.textures,
        ticks: &app.ticks,
        in_flight: &app.in_flight,
        failed: &app.failed_previews,
        selection: app.selection,
        range: selected_range(app.selection),
        tile: grid.tile,
    };
    let mut event = TileEvent::None;
    let mut visible_rows = 0..0;
    let output = scroll.show_rows(ui, grid.tile + CAPTION, rows, |ui, rows| {
        for row in rows.clone() {
            ui.horizontal(|ui| {
                for column in 0..grid.columns {
                    let position = row * grid.columns + column;
                    let Some(shot) = app
                        .order
                        .get(position)
                        .and_then(|&id| shot_at(&app.shots, id))
                    else {
                        break;
                    };
                    let tile_event = draw_tile(ui, &tiles, position, shot);
                    if tile_event != TileEvent::None {
                        event = tile_event;
                    }
                }
            });
        }
        visible_rows = rows;
    });
    app.last_viewport = (output.state.clamped_offset().y, output.inner_rect.height());

    let mut action = None;
    match event {
        TileEvent::None => {}
        TileEvent::Toggle(id) => toggle_ticks(&mut app.ticks, &[id]),
        TileEvent::Open { position, shot } => {
            app.selection = Selection {
                focus: position,
                anchor: None,
            };
            action = Some(GridAction::Open(shot));
        }
    }

    let visible = visible_ids(&app.order, visible_rows, grid.columns);
    if let Some(scanner) = &app.scanner {
        scanner.set_visible(visible.clone());
    }
    let requests = previews_to_request(
        &visible,
        |id| app.textures.contains_key(&id),
        &app.in_flight,
        |id| {
            !app.failed_previews.contains_key(&id)
                && shot_at(&app.shots, id)
                    .and_then(|shot| preview_source(shot, &app.registry))
                    .is_some()
        },
    );
    for id in requests {
        let Some(source) =
            shot_at(&app.shots, id).and_then(|shot| preview_source(shot, &app.registry))
        else {
            continue;
        };
        if let Some(scanner) = &app.scanner {
            scanner.request_preview(id, source.path, source.range, source.orientation);
            app.in_flight.insert(id);
        }
    }
    action
}

fn empty_text(phase: &Phase) -> &'static str {
    match phase {
        Phase::Idle => "Choose a source folder with Source…",
        Phase::Listing => "Listing the folder…",
        Phase::Failed(_) => "The folder could not be scanned. See the status line.",
        Phase::Parsing | Phase::Done => "No photos found in this folder.",
    }
}

fn draw_tile(ui: &mut egui::Ui, tiles: &Tiles, position: usize, shot: &Shot) -> TileEvent {
    let tile = tiles.tile;
    let (rect, _) = ui.allocate_exact_size(vec2(tile, tile + CAPTION), Sense::hover());
    // Registered before the checkbox, so the checkbox sits on top and gets its own clicks.
    let tile_response = ui.interact(rect, Id::unique(("tile", shot.id.0)), Sense::click());
    let image_rect = Rect::from_min_size(rect.min, vec2(tile, tile));
    let painter = ui.painter();
    let accent = ui.visuals().selection.stroke.color;
    let name = shot.display_name();

    painter.rect_filled(image_rect, 4.0, Color32::from_gray(32));
    match tiles.textures.get(&shot.id) {
        Some(texture) => {
            let size = texture.size();
            let [w, h] = fit_within([size[0] as f32, size[1] as f32], tile);
            let fitted = Rect::from_center_size(image_rect.center(), vec2(w, h));
            painter.image(
                texture.id(),
                fitted,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        None => {
            let text = placeholder(tiles, shot, tile);
            painter.text(
                image_rect.center(),
                Align2::CENTER_CENTER,
                text,
                FontId::proportional(13.0),
                Color32::LIGHT_GRAY,
            );
        }
    }

    let badge = shot.badge(tiles.registry);
    if !badge.is_empty() {
        let galley = painter.layout_no_wrap(badge, FontId::proportional(12.0), Color32::WHITE);
        let badge_rect = Rect::from_min_size(
            image_rect.min + vec2(4.0, 4.0),
            galley.size() + vec2(8.0, 4.0),
        );
        painter.rect_filled(badge_rect, 3.0, Color32::from_black_alpha(160));
        painter.galley(badge_rect.min + vec2(4.0, 2.0), galley, Color32::WHITE);
    }

    if tiles.range.contains(&position) {
        painter.rect_filled(image_rect, 4.0, accent.gamma_multiply(0.35));
    }
    if position == tiles.selection.focus {
        painter.rect_stroke(rect, 4.0, Stroke::new(2.0, accent), StrokeKind::Inside);
    }

    painter.text(
        pos2(rect.min.x + 2.0, image_rect.max.y + 2.0),
        Align2::LEFT_TOP,
        truncate_name(&name, caption_chars(tile)),
        FontId::proportional(12.0),
        ui.visuals().text_color(),
    );

    let box_rect = Rect::from_min_size(
        pos2(
            image_rect.max.x - CHECK_INSET - CHECK_SIZE,
            image_rect.min.y + CHECK_INSET,
        ),
        vec2(CHECK_SIZE, CHECK_SIZE),
    );
    let checkbox = ui.interact(
        box_rect,
        Id::unique(("tile_tick", shot.id.0)),
        Sense::click(),
    );
    let ticked = tiles.ticks.contains(&shot.id);
    let painter = ui.painter();
    let fill = if ticked {
        accent
    } else {
        Color32::from_black_alpha(140)
    };
    painter.rect_filled(box_rect, 3.0, fill);
    painter.rect_stroke(
        box_rect,
        3.0,
        Stroke::new(1.5, Color32::WHITE),
        StrokeKind::Inside,
    );
    if ticked {
        let at = |x: f32, y: f32| box_rect.min + vec2(x, y);
        let check = Stroke::new(2.0, Color32::WHITE);
        painter.line_segment([at(4.0, 10.0), at(8.5, 14.5)], check);
        painter.line_segment([at(8.5, 14.5), at(16.0, 5.5)], check);
    }

    if checkbox.clicked() {
        return TileEvent::Toggle(shot.id);
    }
    if tile_response.clicked() {
        return TileEvent::Open {
            position,
            shot: shot.id,
        };
    }
    TileEvent::None
}

/// The caption and, when there is something to say, one more line: a parse
/// failure, a preview in flight, or a preview that did not decode.
fn placeholder(tiles: &Tiles, shot: &Shot, tile: f32) -> String {
    let name = truncate_name(&shot.display_name(), caption_chars(tile));
    let detail = if is_unreadable(shot) {
        let error = shot.files.iter().find_map(|file| file.error.as_deref());
        Some(format!(
            "⚠ unreadable: {}",
            truncate_name(error.unwrap_or_default(), caption_chars(tile))
        ))
    } else if tiles.in_flight.contains(&shot.id) {
        Some("loading…".to_owned())
    } else {
        tiles
            .failed
            .get(&shot.id)
            .map(|error| format!("no preview: {}", truncate_name(error, caption_chars(tile))))
    };
    match detail {
        Some(detail) => format!("{name}\n{detail}"),
        None => name,
    }
}

fn caption_chars(tile: f32) -> usize {
    // Nothing narrower than a few glyphs fits, however small the tile.
    ((tile / GLYPH_WIDTH) as usize).max(4)
}
