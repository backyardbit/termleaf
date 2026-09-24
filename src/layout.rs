use ratatui::layout::Rect;

use crate::pdf::{PageSize, PixelRegion, PixelSize, Scale, nearest_whole};

pub const PAGE_GAP_ROWS: u32 = 1;
pub const TILE_COLUMNS: u32 = 64;
pub const TILE_ROWS: u32 = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pane {
    pub columns: u32,
    pub rows: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub page: usize,
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DocumentPoint {
    pub column: f64,
    pub row: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Tile {
    pub page: usize,
    pub column: u32,
    pub row: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TilePlacement {
    pub tile: Tile,
    pub area: Rect,
    pub first_column: u16,
    pub first_row: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderedTile {
    pub page: usize,
    pub scale: Scale,
    pub region: PixelRegion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StretchedGrid {
    pub columns: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stretched {
    pub grid: StretchedGrid,
    pub area: Rect,
    pub first_column: u16,
    pub first_row: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    sizes: Vec<PageSize>,
    scale: Scale,
    cell: CellSize,
    tops: Vec<u32>,
    width: u32,
}

impl Layout {
    pub fn new(sizes: Vec<PageSize>, scale: Scale, cell: CellSize) -> Self {
        let cell = CellSize {
            width: cell.width.max(1),
            height: cell.height.max(1),
        };
        let mut layout = Self {
            sizes,
            scale,
            cell,
            tops: Vec::new(),
            width: 0,
        };
        let mut top = 0;
        for page in 0..layout.sizes.len() {
            layout.tops.push(top);
            let (columns, rows) = layout.page_cells(page);
            top += rows + PAGE_GAP_ROWS;
            layout.width = layout.width.max(columns);
        }
        layout
    }

    pub fn scale(&self) -> Scale {
        self.scale
    }

    pub fn cell(&self) -> CellSize {
        self.cell
    }

    pub fn page_count(&self) -> usize {
        self.sizes.len()
    }

    pub fn page_pixels(&self, page: usize) -> PixelSize {
        let size = self.size(page);
        let pixels_per_point = self.scale.pixels_per_point();
        PixelSize {
            width: nearest_whole((f64::from(size.width) * pixels_per_point).ceil()).max(1),
            height: nearest_whole((f64::from(size.height) * pixels_per_point).ceil()).max(1),
        }
    }

    pub fn page_cells(&self, page: usize) -> (u32, u32) {
        let pixels = self.page_pixels(page);
        (
            pixels.width.div_ceil(self.cell.width),
            pixels.height.div_ceil(self.cell.height),
        )
    }

    pub fn page_top(&self, page: usize) -> u32 {
        self.tops.get(page).copied().unwrap_or_default()
    }

    pub fn page_left(&self, page: usize) -> u32 {
        (self.width - self.page_cells(page).0) / 2
    }

    pub fn height(&self) -> u32 {
        let Some(last) = self.sizes.len().checked_sub(1) else {
            return 0;
        };
        self.page_top(last) + self.page_cells(last).1
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn page_at_row(&self, row: u32) -> usize {
        self.tops
            .partition_point(|top| *top <= row)
            .saturating_sub(1)
    }

    pub fn position_at(&self, point: DocumentPoint) -> Position {
        let row = point.row.max(0.0);
        let page = self.page_at_row(nearest_whole(row.floor()));
        let points_per_row = f64::from(self.cell.height) / self.scale.pixels_per_point();
        let points_per_column = f64::from(self.cell.width) / self.scale.pixels_per_point();
        let height = f64::from(self.size(page).height);
        Position {
            page,
            x: (point.column - f64::from(self.page_left(page))) * points_per_column,
            y: ((row - f64::from(self.page_top(page))) * points_per_row).clamp(0.0, height),
        }
    }

    pub fn point_of(&self, position: Position) -> DocumentPoint {
        let page = position.page.min(self.sizes.len().saturating_sub(1));
        let pixels_per_point = self.scale.pixels_per_point();
        DocumentPoint {
            column: f64::from(self.page_left(page))
                + position.x * pixels_per_point / f64::from(self.cell.width),
            row: f64::from(self.page_top(page))
                + position.y * pixels_per_point / f64::from(self.cell.height),
        }
    }

    pub fn tile_region(&self, tile: Tile) -> PixelRegion {
        let page = self.page_pixels(tile.page);
        let tile_width = TILE_COLUMNS * self.cell.width;
        let tile_height = TILE_ROWS * self.cell.height;
        let x = tile.column * tile_width;
        let y = tile.row * tile_height;
        PixelRegion {
            x,
            y,
            width: page.width.saturating_sub(x).min(tile_width),
            height: page.height.saturating_sub(y).min(tile_height),
        }
    }

    fn size(&self, page: usize) -> PageSize {
        self.sizes.get(page).copied().unwrap_or(PageSize {
            width: 1.0,
            height: 1.0,
        })
    }
}

pub fn fit_width(sizes: &[PageSize], cell: CellSize, pane: Pane) -> Scale {
    let widest = sizes.iter().map(|size| size.width).fold(1.0, f32::max);
    Scale::at_most(pane_pixels(pane.columns, cell.width) / f64::from(widest))
}

pub fn fit_page(sizes: &[PageSize], cell: CellSize, pane: Pane) -> Scale {
    let tallest = sizes.iter().map(|size| size.height).fold(1.0, f32::max);
    let by_height = Scale::at_most(pane_pixels(pane.rows, cell.height) / f64::from(tallest));
    fit_width(sizes, cell, pane).min(by_height)
}

fn pane_pixels(cells: u32, cell_pixels: u32) -> f64 {
    f64::from(cells.max(1)) * f64::from(cell_pixels.max(1))
}

#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub layout: Layout,
    pub pane: Pane,
    pub top: u32,
    pub left: u32,
}

impl View {
    pub fn clamped(mut self) -> Self {
        self.top = self.top.min(self.max_top());
        self.left = self.left.min(self.max_left());
        self
    }

    pub fn max_top(&self) -> u32 {
        self.layout.height().saturating_sub(self.pane.rows)
    }

    pub fn max_left(&self) -> u32 {
        self.layout.width().saturating_sub(self.pane.columns)
    }

    fn margins(&self) -> (u32, u32) {
        (
            self.pane.columns.saturating_sub(self.layout.width()) / 2,
            self.pane.rows.saturating_sub(self.layout.height()) / 2,
        )
    }

    pub fn point_at(&self, screen_column: f64, screen_row: f64) -> DocumentPoint {
        let (margin_x, margin_y) = self.margins();
        DocumentPoint {
            column: f64::from(self.left) + screen_column - f64::from(margin_x),
            row: f64::from(self.top) + screen_row - f64::from(margin_y),
        }
    }

    pub fn scrolled_to(self, point: DocumentPoint, screen_column: f64, screen_row: f64) -> Self {
        let (margin_x, margin_y) = self.margins();
        let top = nearest_whole(point.row - screen_row + f64::from(margin_y));
        let left = nearest_whole(point.column - screen_column + f64::from(margin_x));
        Self { top, left, ..self }.clamped()
    }

    pub fn screen_of(&self, point: DocumentPoint) -> (f64, f64) {
        let (margin_x, margin_y) = self.margins();
        (
            point.column - f64::from(self.left) + f64::from(margin_x),
            point.row - f64::from(self.top) + f64::from(margin_y),
        )
    }

    pub fn stretch(&self, tile: RenderedTile) -> Option<Stretched> {
        let cell = self.layout.cell();
        let from = tile.scale.pixels_per_point();
        let factor = self.layout.scale().pixels_per_point() / from;
        let columns =
            cells(nearest_whole(f64::from(tile.region.width.div_ceil(cell.width)) * factor).max(1));
        let rows = cells(
            nearest_whole(f64::from(tile.region.height.div_ceil(cell.height)) * factor).max(1),
        );
        let position = Position {
            page: tile.page,
            x: f64::from(tile.region.x) / from,
            y: f64::from(tile.region.y) / from,
        };
        let (x, y) = self.screen_of(self.layout.point_of(position));
        let (left, top) = (nearest_signed(x), nearest_signed(y));
        let columns_shown = clip(left, columns, self.pane.columns)?;
        let rows_shown = clip(top, rows, self.pane.rows)?;
        Some(Stretched {
            grid: StretchedGrid { columns, rows },
            area: Rect {
                x: columns_shown.start,
                y: rows_shown.start,
                width: columns_shown.end - columns_shown.start,
                height: rows_shown.end - rows_shown.start,
            },
            first_column: skipped(left, columns_shown.start),
            first_row: skipped(top, rows_shown.start),
        })
    }

    pub fn page_under(&self, screen_column: u16, screen_row: u16) -> Option<Position> {
        let point = self.point_at(f64::from(screen_column) + 0.5, f64::from(screen_row) + 0.5);
        if point.row < 0.0 || point.column < 0.0 {
            return None;
        }
        let row = nearest_whole(point.row.floor());
        let column = nearest_whole(point.column.floor());
        let page = self.layout.page_at_row(row);
        let (columns, rows) = self.layout.page_cells(page);
        let top = self.layout.page_top(page);
        let left = self.layout.page_left(page);
        let inside = row < top + rows && (left..left + columns).contains(&column);
        inside.then(|| self.layout.position_at(point))
    }

    pub fn current_page(&self) -> usize {
        let bottom = self.top + self.pane.rows;
        let first = self.layout.page_at_row(self.top);
        let last = self.layout.page_at_row(bottom.saturating_sub(1));
        (first..=last)
            .max_by_key(|page| {
                let start = self.layout.page_top(*page).max(self.top);
                let end =
                    (self.layout.page_top(*page) + self.layout.page_cells(*page).1).min(bottom);
                (end.saturating_sub(start), std::cmp::Reverse(*page))
            })
            .unwrap_or(first)
    }

    pub fn placements(&self) -> Vec<TilePlacement> {
        let (margin_x, margin_y) = self.margins();
        let view_rows = self.top..self.top + self.pane.rows;
        let view_columns = self.left..self.left + self.pane.columns;
        let mut placements = Vec::new();
        let first = self.layout.page_at_row(self.top);
        for page in first..self.layout.page_count() {
            let page_top = self.layout.page_top(page);
            if page_top >= view_rows.end {
                break;
            }
            let page_left = self.layout.page_left(page);
            let (columns, rows) = self.layout.page_cells(page);
            for tile_row in 0..rows.div_ceil(TILE_ROWS) {
                let tile_top = page_top + tile_row * TILE_ROWS;
                let tile_rows = tile_top..(tile_top + TILE_ROWS).min(page_top + rows);
                let Some(visible_rows) = overlap(&tile_rows, &view_rows) else {
                    continue;
                };
                for tile_column in 0..columns.div_ceil(TILE_COLUMNS) {
                    let tile_left = page_left + tile_column * TILE_COLUMNS;
                    let tile_columns =
                        tile_left..(tile_left + TILE_COLUMNS).min(page_left + columns);
                    let Some(visible_columns) = overlap(&tile_columns, &view_columns) else {
                        continue;
                    };
                    placements.push(TilePlacement {
                        tile: Tile {
                            page,
                            column: tile_column,
                            row: tile_row,
                        },
                        area: Rect {
                            x: cells(visible_columns.start - self.left + margin_x),
                            y: cells(visible_rows.start - self.top + margin_y),
                            width: cells(visible_columns.len_u32()),
                            height: cells(visible_rows.len_u32()),
                        },
                        first_column: cells(visible_columns.start - tile_left),
                        first_row: cells(visible_rows.start - tile_top),
                    });
                }
            }
        }
        placements
    }
}

trait CellRange {
    fn len_u32(&self) -> u32;
}

impl CellRange for std::ops::Range<u32> {
    fn len_u32(&self) -> u32 {
        self.end.saturating_sub(self.start)
    }
}

fn overlap(a: &std::ops::Range<u32>, b: &std::ops::Range<u32>) -> Option<std::ops::Range<u32>> {
    let range = a.start.max(b.start)..a.end.min(b.end);
    (range.start < range.end).then_some(range)
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the value is rounded and clamped into i64's range first"
)]
fn nearest_signed(value: f64) -> i64 {
    if value.is_nan() {
        return 0;
    }
    value.round().clamp(-1e15, 1e15) as i64
}

fn clip(start: i64, length: u16, limit: u32) -> Option<std::ops::Range<u16>> {
    let end = start + i64::from(length);
    let first = start.clamp(0, i64::from(limit));
    let last = end.clamp(0, i64::from(limit));
    let first = u16::try_from(first).ok()?;
    let last = u16::try_from(last).ok()?;
    (first < last).then_some(first..last)
}

fn skipped(start: i64, shown_from: u16) -> u16 {
    u16::try_from(i64::from(shown_from) - start).unwrap_or(0)
}

fn cells(value: u32) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: CellSize = CellSize {
        width: 10,
        height: 20,
    };

    fn page(width: f32, height: f32) -> PageSize {
        PageSize { width, height }
    }

    fn at_one_to_one(sizes: Vec<PageSize>) -> Layout {
        Layout::new(sizes, Scale::from_pixels_per_point(1.0), CELL)
    }

    fn view(layout: Layout, columns: u32, rows: u32, top: u32, left: u32) -> View {
        View {
            layout,
            pane: Pane { columns, rows },
            top,
            left,
        }
    }

    #[test]
    fn pages_stack_with_a_gap_row_between_them() {
        let layout = at_one_to_one(vec![page(100.0, 200.0), page(100.0, 200.0)]);
        assert_eq!(layout.page_top(0), 0);
        assert_eq!(layout.page_top(1), 11);
        assert_eq!(layout.height(), 21);
    }

    #[test]
    fn a_partial_cell_rounds_the_page_up() {
        let layout = at_one_to_one(vec![page(105.0, 201.0)]);
        assert_eq!(layout.page_cells(0), (11, 11));
    }

    #[test]
    fn narrower_pages_are_centred_in_the_document() {
        let layout = at_one_to_one(vec![page(200.0, 100.0), page(100.0, 100.0)]);
        assert_eq!(layout.width(), 20);
        assert_eq!(layout.page_left(0), 0);
        assert_eq!(layout.page_left(1), 5);
    }

    #[test]
    fn scale_grows_the_page() {
        let layout = Layout::new(
            vec![page(100.0, 200.0)],
            Scale::from_pixels_per_point(2.0),
            CELL,
        );
        assert_eq!(layout.page_cells(0), (20, 20));
    }

    #[test]
    fn fit_width_makes_the_widest_page_fill_the_pane() {
        let sizes = [page(612.0, 792.0), page(300.0, 792.0)];
        let pane = Pane {
            columns: 80,
            rows: 24,
        };
        let layout = Layout::new(sizes.to_vec(), fit_width(&sizes, CELL, pane), CELL);
        assert_eq!(layout.page_cells(0).0, 80);
    }

    #[test]
    fn fit_page_makes_the_tallest_page_fit_the_pane() {
        let sizes = [page(612.0, 792.0)];
        let pane = Pane {
            columns: 80,
            rows: 24,
        };
        let layout = Layout::new(sizes.to_vec(), fit_page(&sizes, CELL, pane), CELL);
        assert_eq!(layout.page_cells(0).1, 24);
        assert!(layout.page_cells(0).0 <= 80);
    }

    #[test]
    fn gap_rows_belong_to_the_page_above() {
        let layout = at_one_to_one(vec![page(100.0, 200.0), page(100.0, 200.0)]);
        assert_eq!(layout.page_at_row(9), 0);
        assert_eq!(layout.page_at_row(10), 0);
        assert_eq!(layout.page_at_row(11), 1);
        assert_eq!(layout.page_at_row(500), 1);
    }

    #[test]
    fn a_position_survives_a_change_of_scale() {
        let sizes = vec![page(100.0, 200.0), page(100.0, 200.0)];
        let small = at_one_to_one(sizes.clone());
        let large = Layout::new(sizes, Scale::from_pixels_per_point(3.0), CELL);
        let position = small.position_at(DocumentPoint {
            column: 4.0,
            row: 15.0,
        });
        assert_eq!(position.page, 1);
        let moved = large.point_of(position);
        assert!((moved.row - (f64::from(large.page_top(1)) + 12.0)).abs() < 1e-9);
        assert!((moved.column - 12.0).abs() < 1e-9);
    }

    #[test]
    fn a_tile_covers_its_part_of_the_page_in_pixels() {
        let layout = at_one_to_one(vec![page(1500.0, 200.0)]);
        let region = layout.tile_region(Tile {
            page: 0,
            column: 2,
            row: 0,
        });
        assert_eq!(
            region,
            PixelRegion {
                x: 1280,
                y: 0,
                width: 220,
                height: 200,
            }
        );
    }

    #[test]
    fn the_view_stops_at_the_end_of_the_document() {
        let layout = at_one_to_one(vec![page(100.0, 2000.0)]);
        let view = view(layout, 10, 30, 500, 0).clamped();
        assert_eq!(view.top, 70);
    }

    #[test]
    fn a_document_smaller_than_the_pane_is_centred() {
        let layout = at_one_to_one(vec![page(200.0, 200.0)]);
        let placements = view(layout, 40, 30, 0, 0).placements();
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].area, Rect::new(10, 10, 20, 10));
    }

    #[test]
    fn scrolling_shows_a_later_slice_of_the_tile() {
        let layout = at_one_to_one(vec![page(100.0, 400.0)]);
        let placements = view(layout, 10, 10, 5, 0).placements();
        assert_eq!(placements[0].area, Rect::new(0, 0, 10, 10));
        assert_eq!(placements[0].first_row, 5);
    }

    #[test]
    fn the_gap_between_pages_is_left_empty() {
        let layout = at_one_to_one(vec![page(100.0, 200.0), page(100.0, 200.0)]);
        let placements = view(layout, 10, 6, 8, 0).placements();
        let areas: Vec<Rect> = placements.iter().map(|placed| placed.area).collect();
        assert_eq!(areas, [Rect::new(0, 0, 10, 2), Rect::new(0, 3, 10, 3)]);
        assert_eq!(placements[1].tile.page, 1);
        assert_eq!(placements[1].first_row, 0);
    }

    #[test]
    fn a_tall_page_is_split_into_tiles() {
        let layout = at_one_to_one(vec![page(100.0, 2000.0)]);
        let placements = view(layout, 10, 20, 40, 0).placements();
        assert_eq!(placements.len(), 2);
        assert_eq!(placements[0].area, Rect::new(0, 0, 10, 8));
        assert_eq!(placements[0].first_row, 40);
        assert_eq!(placements[1].tile.row, 1);
        assert_eq!(placements[1].area, Rect::new(0, 8, 10, 12));
        assert_eq!(placements[1].first_row, 0);
    }

    #[test]
    fn panning_sideways_shows_a_later_column_of_the_tile() {
        let layout = at_one_to_one(vec![page(300.0, 200.0)]);
        let placements = view(layout, 10, 10, 0, 3).placements();
        assert_eq!(placements[0].first_column, 3);
        assert_eq!(placements[0].area, Rect::new(0, 0, 10, 10));
    }

    #[test]
    fn the_current_page_is_the_one_filling_most_of_the_view() {
        let layout = at_one_to_one(vec![page(100.0, 200.0), page(100.0, 200.0)]);
        assert_eq!(view(layout.clone(), 10, 10, 4, 0).current_page(), 0);
        assert_eq!(view(layout, 10, 10, 7, 0).current_page(), 1);
    }

    #[test]
    fn finds_the_page_position_under_a_screen_cell() {
        let layout = at_one_to_one(vec![page(100.0, 200.0), page(100.0, 200.0)]);
        let view = view(layout, 10, 10, 8, 0);
        let position = view.page_under(2, 5).unwrap();
        assert_eq!(position.page, 1);
        assert!((position.y - 50.0).abs() < 1e-9);
        assert!((position.x - 25.0).abs() < 1e-9);
        assert_eq!(view.page_under(2, 2), None);
    }

    #[test]
    fn a_screen_point_round_trips_through_the_document() {
        let layout = at_one_to_one(vec![page(300.0, 2000.0)]);
        let view = view(layout, 10, 10, 7, 3);
        let point = view.point_at(2.5, 4.0);
        let (column, row) = view.screen_of(point);
        assert!((column - 2.5).abs() < 1e-9 && (row - 4.0).abs() < 1e-9);
    }

    fn rendered_at(view: &View, tile: Tile) -> RenderedTile {
        RenderedTile {
            page: tile.page,
            scale: view.layout.scale(),
            region: view.layout.tile_region(tile),
        }
    }

    #[test]
    fn stretching_to_the_same_view_matches_the_normal_placement() {
        let layout = at_one_to_one(vec![page(100.0, 2000.0), page(100.0, 200.0)]);
        let view = view(layout, 10, 20, 40, 0);
        for placed in view.placements() {
            let stretched = view.stretch(rendered_at(&view, placed.tile)).unwrap();
            assert_eq!(stretched.area, placed.area);
            assert_eq!(stretched.first_row, placed.first_row);
            assert_eq!(stretched.first_column, placed.first_column);
        }
    }

    #[test]
    fn stretching_to_twice_the_scale_doubles_the_tile() {
        let sizes = vec![page(100.0, 200.0)];
        let small = view(at_one_to_one(sizes.clone()), 40, 40, 0, 0);
        let large = view(
            Layout::new(sizes, Scale::from_pixels_per_point(2.0), CELL),
            40,
            40,
            0,
            0,
        );
        let tile = Tile {
            page: 0,
            column: 0,
            row: 0,
        };
        let stretched = large.stretch(rendered_at(&small, tile)).unwrap();
        assert_eq!(
            stretched.grid,
            StretchedGrid {
                columns: 20,
                rows: 20
            }
        );
        assert_eq!(stretched.area, Rect::new(10, 10, 20, 20));
    }

    #[test]
    fn a_stretched_tile_above_the_view_is_clipped_from_the_top() {
        let sizes = vec![page(100.0, 400.0)];
        let small = view(at_one_to_one(sizes.clone()), 10, 10, 0, 0);
        let large = view(
            Layout::new(sizes, Scale::from_pixels_per_point(2.0), CELL),
            10,
            10,
            6,
            0,
        );
        let tile = Tile {
            page: 0,
            column: 0,
            row: 0,
        };
        let stretched = large.stretch(rendered_at(&small, tile)).unwrap();
        assert_eq!(stretched.first_row, 6);
        assert_eq!(stretched.area.y, 0);
        assert_eq!(stretched.area.height, 10);
    }

    #[test]
    fn a_tile_stretched_out_of_the_view_is_not_drawn() {
        let sizes = vec![page(100.0, 4000.0)];
        let whole = view(at_one_to_one(sizes.clone()), 10, 10, 0, 0);
        let far = view(at_one_to_one(sizes), 10, 10, 150, 0);
        let tile = Tile {
            page: 0,
            column: 0,
            row: 0,
        };
        assert_eq!(far.stretch(rendered_at(&whole, tile)), None);
    }

    #[test]
    fn scrolling_to_a_point_puts_it_under_the_given_screen_cell() {
        let layout = at_one_to_one(vec![page(100.0, 2000.0)]);
        let view = view(layout, 10, 10, 0, 0).scrolled_to(
            DocumentPoint {
                column: 0.0,
                row: 50.0,
            },
            0.0,
            5.0,
        );
        assert_eq!(view.top, 45);
    }
}
