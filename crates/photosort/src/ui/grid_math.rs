//! Pure layout, focus and selection logic for the thumbnail grid. Positions
//! are indices into the display order.
// pattern: Functional Core

use std::collections::HashSet;
use std::ops::RangeInclusive;

use photosort_core::model::ShotId;

pub(crate) const TILE_MIN: f32 = 96.0;
pub(crate) const TILE_MAX: f32 = 640.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GridLayout {
    pub columns: usize,
    pub tile: f32,
    pub row_height: f32,
}

/// Columns that fit `available_width` at `tile` size, with `spacing` between
/// tiles. Always at least one column.
pub(crate) fn layout(available_width: f32, tile: f32, spacing: f32, caption: f32) -> GridLayout {
    let stride = tile + spacing;
    // A non-positive stride has no meaningful column count, so it gets one column.
    let columns = if stride > 0.0 {
        ((available_width + spacing) / stride).floor().max(1.0) as usize
    } else {
        1
    };
    GridLayout {
        columns,
        tile,
        row_height: tile + caption + spacing,
    }
}

pub(crate) fn clamp_tile(size: f32) -> f32 {
    size.clamp(TILE_MIN, TILE_MAX)
}

/// Scales a tile by a zoom step, staying within `TILE_MIN..=TILE_MAX`.
pub(crate) fn zoom_tile(size: f32, zoom_delta: f32) -> f32 {
    clamp_tile(size * zoom_delta)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Move {
    Left,
    Right,
    Up,
    Down,
}

/// Moves focus one step, clamped to the grid. Returns 0 when the grid is empty.
pub(crate) fn move_focus(focus: usize, mv: Move, columns: usize, len: usize) -> usize {
    // `len - 1` is only formed after the empty case has returned.
    let Some(last) = len.checked_sub(1) else {
        return 0;
    };
    let focus = focus.min(last);
    let columns = columns.max(1);
    match mv {
        Move::Left => focus.saturating_sub(1),
        Move::Right => focus.saturating_add(1).min(last),
        // Up from the first row stays put, rather than clamping to index 0.
        Move::Up if focus >= columns => focus - columns,
        Move::Up => focus,
        Move::Down => focus.saturating_add(columns).min(last),
    }
}

/// Focus plus an optional range anchor (Shift+arrows).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Selection {
    pub focus: usize,
    pub anchor: Option<usize>,
}

/// Moves the focus. With `extend`, the anchor stays where the range started.
/// Without it, the anchor is cleared. An empty grid gives the default selection.
pub(crate) fn apply_move(
    sel: Selection,
    mv: Move,
    extend: bool,
    columns: usize,
    len: usize,
) -> Selection {
    if len == 0 {
        return Selection::default();
    }
    let focus = move_focus(sel.focus, mv, columns, len);
    let anchor = extend.then(|| sel.anchor.unwrap_or(sel.focus));
    Selection { focus, anchor }
}

/// The positions from the anchor to the focus, inclusive, in either order.
pub(crate) fn selected_range(sel: Selection) -> RangeInclusive<usize> {
    let anchor = sel.anchor.unwrap_or(sel.focus);
    anchor.min(sel.focus)..=anchor.max(sel.focus)
}

/// If every target is ticked, untick them all. Otherwise tick them all.
pub(crate) fn toggle_ticks(ticks: &mut HashSet<ShotId>, targets: &[ShotId]) {
    if targets.is_empty() {
        return;
    }
    if targets.iter().all(|id| ticks.contains(id)) {
        for id in targets {
            ticks.remove(id);
        }
    } else {
        ticks.extend(targets.iter().copied());
    }
}

/// New scroll offset that reveals `row`, or `None` if it is already fully visible.
pub(crate) fn reveal_row(row: usize, row_height: f32, offset: f32, viewport: f32) -> Option<f32> {
    let top = row as f32 * row_height;
    let bottom = top + row_height;
    if top < offset {
        Some(top)
    } else if bottom > offset + viewport {
        // A row taller than the viewport scrolls to its top, so its start stays visible.
        Some((bottom - viewport).min(top))
    } else {
        None
    }
}

/// Visible shots that need a preview request: no texture, not in flight, and a
/// preview exists.
pub(crate) fn previews_to_request(
    visible: &[ShotId],
    has_texture: impl Fn(ShotId) -> bool,
    in_flight: &HashSet<ShotId>,
    has_preview: impl Fn(ShotId) -> bool,
) -> Vec<ShotId> {
    visible
        .iter()
        .copied()
        .filter(|&id| !has_texture(id) && !in_flight.contains(&id) && has_preview(id))
        .collect()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const SPACING: f32 = 8.0;
    const CAPTION: f32 = 30.0;

    fn ids(values: &[u32]) -> Vec<ShotId> {
        values.iter().map(|&v| ShotId(v)).collect()
    }

    // AC2.7: layout and zoom

    #[test]
    fn columns_are_the_floor_of_width_plus_spacing_over_tile_plus_spacing() {
        assert_eq!(layout(1000.0, 200.0, SPACING, CAPTION).columns, 4);
        assert_eq!(layout(420.0, 200.0, SPACING, CAPTION).columns, 2);
        assert_eq!(layout(400.0, 200.0, SPACING, CAPTION).columns, 1);
    }

    #[test]
    fn the_column_count_reflows_as_the_width_changes() {
        let counts: Vec<usize> = [400.0, 420.0, 1000.0]
            .iter()
            .map(|&w| layout(w, 200.0, SPACING, CAPTION).columns)
            .collect();
        assert_eq!(counts, [1, 2, 4]);
    }

    #[test]
    fn a_grid_always_has_at_least_one_column() {
        assert_eq!(layout(0.0, 200.0, SPACING, CAPTION).columns, 1);
        assert_eq!(layout(-50.0, 200.0, SPACING, CAPTION).columns, 1);
    }

    #[test]
    fn row_height_is_tile_plus_caption_plus_spacing() {
        let grid = layout(1000.0, 200.0, SPACING, CAPTION);
        assert_eq!(grid.tile, 200.0);
        assert_eq!(grid.row_height, 200.0 + CAPTION + SPACING);
    }

    proptest! {
        #[test]
        fn more_width_never_means_fewer_columns(a in 0.0f32..4000.0, b in 0.0f32..4000.0) {
            let (narrow, wide) = if a <= b { (a, b) } else { (b, a) };
            let narrow_cols = layout(narrow, 200.0, SPACING, CAPTION).columns;
            let wide_cols = layout(wide, 200.0, SPACING, CAPTION).columns;
            prop_assert!(narrow_cols >= 1);
            prop_assert!(narrow_cols <= wide_cols);
        }
    }

    #[test]
    fn clamp_tile_pins_sizes_to_the_tile_range() {
        assert_eq!(clamp_tile(10.0), TILE_MIN);
        assert_eq!(clamp_tile(1000.0), TILE_MAX);
        assert_eq!(clamp_tile(200.0), 200.0);
    }

    proptest! {
        #[test]
        fn clamp_tile_stays_within_96_and_640(size in -1.0e9f32..1.0e9f32) {
            let clamped = clamp_tile(size);
            prop_assert!((TILE_MIN..=TILE_MAX).contains(&clamped));
        }

        #[test]
        fn zoom_tile_stays_within_96_and_640(size in -1.0e6f32..1.0e6f32, delta in 0.001f32..1000.0) {
            let zoomed = zoom_tile(size, delta);
            prop_assert!((TILE_MIN..=TILE_MAX).contains(&zoomed));
        }
    }

    #[test]
    fn zooming_in_grows_the_tile() {
        assert!(zoom_tile(200.0, 1.1) > 200.0);
    }

    // AC2.5: focus movement

    #[test]
    fn right_from_the_last_index_stays_put() {
        assert_eq!(move_focus(4, Move::Right, 3, 5), 4);
    }

    #[test]
    fn down_from_the_last_row_stays_in_range() {
        assert_eq!(move_focus(4, Move::Down, 3, 5), 4);
        assert_eq!(move_focus(2, Move::Down, 3, 5), 4);
    }

    #[test]
    fn up_from_the_first_row_stays_put() {
        assert_eq!(move_focus(1, Move::Up, 3, 5), 1);
        assert_eq!(move_focus(0, Move::Up, 3, 5), 0);
    }

    #[test]
    fn left_from_the_first_index_stays_put() {
        assert_eq!(move_focus(0, Move::Left, 3, 5), 0);
    }

    #[test]
    fn moves_step_by_one_or_one_row() {
        assert_eq!(move_focus(2, Move::Right, 3, 9), 3);
        assert_eq!(move_focus(2, Move::Left, 3, 9), 1);
        assert_eq!(move_focus(1, Move::Down, 3, 9), 4);
        assert_eq!(move_focus(4, Move::Up, 3, 9), 1);
    }

    #[test]
    fn every_move_on_an_empty_grid_returns_zero() {
        for mv in [Move::Left, Move::Right, Move::Up, Move::Down] {
            assert_eq!(move_focus(0, mv, 3, 0), 0, "{mv:?}");
            assert_eq!(move_focus(7, mv, 3, 0), 0, "{mv:?} from a stale focus");
        }
    }

    proptest! {
        #[test]
        fn focus_stays_inside_a_non_empty_grid(
            len in 1usize..100,
            columns in 1usize..10,
            focus_seed in any::<usize>(),
            mv_index in 0usize..4,
        ) {
            let focus = focus_seed % len;
            let mv = [Move::Left, Move::Right, Move::Up, Move::Down][mv_index];
            prop_assert!(move_focus(focus, mv, columns, len) < len);
        }
    }

    // AC2.6: range selection

    #[test]
    fn shift_right_three_times_from_five_selects_five_to_eight() {
        let mut sel = Selection {
            focus: 5,
            anchor: None,
        };
        for _ in 0..3 {
            sel = apply_move(sel, Move::Right, true, 3, 10);
        }
        assert_eq!(sel.focus, 8);
        assert_eq!(selected_range(sel), 5..=8);
    }

    #[test]
    fn a_plain_move_clears_the_anchor() {
        let extended = apply_move(
            Selection {
                focus: 5,
                anchor: None,
            },
            Move::Right,
            true,
            3,
            10,
        );
        let plain = apply_move(extended, Move::Left, false, 3, 10);
        assert_eq!(plain.anchor, None);
        assert_eq!(plain.focus, 5);
    }

    #[test]
    fn apply_move_on_an_empty_grid_gives_the_default_selection() {
        let stale = Selection {
            focus: 3,
            anchor: Some(1),
        };
        for mv in [Move::Left, Move::Right, Move::Up, Move::Down] {
            assert_eq!(
                apply_move(stale, mv, true, 3, 0),
                Selection::default(),
                "{mv:?}"
            );
            assert_eq!(
                apply_move(stale, mv, false, 3, 0),
                Selection::default(),
                "{mv:?}"
            );
        }
    }

    #[test]
    fn the_range_runs_either_way_round() {
        let forward = Selection {
            focus: 6,
            anchor: Some(2),
        };
        let backward = Selection {
            focus: 2,
            anchor: Some(6),
        };
        assert_eq!(selected_range(forward), 2..=6);
        assert_eq!(selected_range(backward), 2..=6);
    }

    #[test]
    fn a_selection_without_an_anchor_covers_only_its_focus() {
        let sel = Selection {
            focus: 4,
            anchor: None,
        };
        assert_eq!(selected_range(sel), 4..=4);
    }

    #[test]
    fn toggling_a_mixed_range_ticks_all_of_it() {
        let mut ticks = HashSet::from([ShotId(0)]);
        toggle_ticks(&mut ticks, &ids(&[0, 1, 2]));
        assert_eq!(ticks, HashSet::from([ShotId(0), ShotId(1), ShotId(2)]));
    }

    #[test]
    fn toggling_an_all_ticked_range_unticks_all_of_it() {
        let mut ticks = HashSet::from([ShotId(0), ShotId(1), ShotId(2)]);
        toggle_ticks(&mut ticks, &ids(&[0, 1, 2]));
        assert!(ticks.is_empty());
    }

    #[test]
    fn space_on_a_single_focus_toggles_that_shot() {
        let mut ticks = HashSet::new();
        toggle_ticks(&mut ticks, &ids(&[7]));
        assert_eq!(ticks, HashSet::from([ShotId(7)]));
        toggle_ticks(&mut ticks, &ids(&[7]));
        assert!(ticks.is_empty());
    }

    #[test]
    fn toggling_no_targets_changes_nothing() {
        let mut ticks = HashSet::from([ShotId(1)]);
        toggle_ticks(&mut ticks, &[]);
        assert_eq!(ticks, HashSet::from([ShotId(1)]));
    }

    // AC2.5: scrolling

    #[test]
    fn a_visible_row_needs_no_scroll() {
        // Row 2 spans 200..300, inside the viewport 150..450.
        assert_eq!(reveal_row(2, 100.0, 150.0, 300.0), None);
    }

    #[test]
    fn a_row_above_the_viewport_scrolls_up_to_it() {
        let offset = reveal_row(0, 100.0, 150.0, 300.0).expect("row 0 is above");
        assert!(offset < 150.0);
        assert_eq!(offset, 0.0);
    }

    #[test]
    fn a_row_below_the_viewport_scrolls_down_to_its_bottom() {
        // Row 5 ends at 600, so the viewport must end there: 600 - 300.
        assert_eq!(reveal_row(5, 100.0, 150.0, 300.0), Some(300.0));
    }

    #[test]
    fn a_row_taller_than_the_viewport_scrolls_to_its_top() {
        // Row 3 spans 1500..2000. Its bottom would put 1700 at the top, which
        // would hide its start, so the top wins.
        assert_eq!(reveal_row(3, 500.0, 0.0, 300.0), Some(1500.0));
    }

    // AC9.3: evicted tiles are requested again

    #[test]
    fn a_visible_shot_with_an_evicted_texture_is_requested_again() {
        let visible = ids(&[0, 1, 2, 3]);
        let textured = HashSet::from([ShotId(0)]);
        let in_flight = HashSet::from([ShotId(2)]);
        let with_preview = HashSet::from([ShotId(0), ShotId(1), ShotId(2)]);

        let requests = previews_to_request(
            &visible,
            |id| textured.contains(&id),
            &in_flight,
            |id| with_preview.contains(&id),
        );

        // 0 has a texture, 1 was evicted, 2 is in flight, 3 has no preview.
        assert_eq!(requests, ids(&[1]));
    }
}
