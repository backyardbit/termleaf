use std::sync::Arc;

use image::{Rgb, RgbImage};

use crate::layout::{Tile, View};
use crate::renderer::{Generation, RenderKey};

pub const GAP: Rgb<u8> = Rgb([48, 48, 48]);

pub struct Rendered {
    pub key: RenderKey,
    pub image: Arc<RgbImage>,
}

pub fn tile_keys(generation: Generation, view: &View) -> Vec<RenderKey> {
    view.placements()
        .into_iter()
        .map(|placement| tile_key(generation, view, placement.tile))
        .collect()
}

fn tile_key(generation: Generation, view: &View, tile: Tile) -> RenderKey {
    RenderKey {
        generation,
        page: tile.page,
        scale: view.layout.scale(),
        region: view.layout.tile_region(tile),
    }
}

pub fn compose(view: &View, generation: Generation, tiles: &[Rendered]) -> RgbImage {
    let cell = view.layout.cell();
    let mut frame = RgbImage::from_pixel(
        view.pane.columns * cell.width,
        view.pane.rows * cell.height,
        GAP,
    );
    for placement in view.placements() {
        let key = tile_key(generation, view, placement.tile);
        let Some(tile) = tiles.iter().find(|tile| tile.key == key) else {
            continue;
        };
        let from = PixelOrigin {
            x: u32::from(placement.first_column) * cell.width,
            y: u32::from(placement.first_row) * cell.height,
        };
        let to = PixelOrigin {
            x: u32::from(placement.area.x) * cell.width,
            y: u32::from(placement.area.y) * cell.height,
        };
        let width = (u32::from(placement.area.width) * cell.width)
            .min(tile.image.width().saturating_sub(from.x))
            .min(frame.width().saturating_sub(to.x));
        let height = (u32::from(placement.area.height) * cell.height)
            .min(tile.image.height().saturating_sub(from.y))
            .min(frame.height().saturating_sub(to.y));
        copy_rows(&tile.image, from, &mut frame, to, width, height);
    }
    frame
}

#[derive(Debug, Clone, Copy)]
struct PixelOrigin {
    x: u32,
    y: u32,
}

fn copy_rows(
    source: &RgbImage,
    from: PixelOrigin,
    target: &mut RgbImage,
    to: PixelOrigin,
    width: u32,
    height: u32,
) {
    let row_bytes = byte_offset(width, 0, 1);
    let source_width = source.width();
    let target_width = target.width();
    let source_bytes = source.as_raw();
    let target_bytes: &mut [u8] = target;
    for row in 0..height {
        let start = byte_offset(from.x, from.y + row, source_width);
        let end = byte_offset(to.x, to.y + row, target_width);
        target_bytes[end..end + row_bytes].copy_from_slice(&source_bytes[start..start + row_bytes]);
    }
}

fn byte_offset(x: u32, y: u32, width: u32) -> usize {
    let pixel = u64::from(y) * u64::from(width) + u64::from(x);
    usize::try_from(pixel * 3).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{CellSize, Layout, Pane};
    use crate::pdf::{PageSize, Scale};

    const CELL: CellSize = CellSize {
        width: 10,
        height: 20,
    };
    const PAGE: PageSize = PageSize {
        width: 100.0,
        height: 50.0,
    };
    const RED: Rgb<u8> = Rgb([200, 30, 30]);
    const BLUE: Rgb<u8> = Rgb([30, 30, 200]);

    fn view_of(pages: usize, pane: Pane, top: u32) -> View {
        View {
            layout: Layout::new(vec![PAGE; pages], Scale::from_pixels_per_point(1.0), CELL),
            pane,
            top,
            left: 0,
        }
    }

    fn pane() -> Pane {
        Pane {
            columns: 12,
            rows: 4,
        }
    }

    fn rendered(key: RenderKey, image: RgbImage) -> Rendered {
        Rendered {
            key,
            image: Arc::new(image),
        }
    }

    fn key_of_page(view: &View, page: usize) -> RenderKey {
        tile_keys(0, view)
            .into_iter()
            .find(|key| key.page == page)
            .unwrap()
    }

    fn rows_numbered(width: u32, height: u32) -> RgbImage {
        RgbImage::from_fn(width, height, |_, y| {
            let level = u8::try_from(y).unwrap();
            Rgb([level, level, level])
        })
    }

    #[test]
    fn a_frame_covers_the_whole_pane_in_pixels() {
        let frame = compose(&view_of(3, pane(), 0), 0, &[]);
        assert_eq!(frame.dimensions(), (120, 80));
    }

    #[test]
    fn without_tiles_the_frame_is_all_gap() {
        let frame = compose(&view_of(3, pane(), 0), 0, &[]);
        assert!(frame.pixels().all(|pixel| *pixel == GAP));
    }

    #[test]
    fn a_tile_is_drawn_where_its_page_sits_in_the_pane() {
        let view = view_of(3, pane(), 0);
        let tile = rendered(key_of_page(&view, 0), RgbImage::from_pixel(100, 50, RED));
        let frame = compose(&view, 0, &[tile]);
        assert_eq!(*frame.get_pixel(10, 0), RED);
        assert_eq!(*frame.get_pixel(109, 49), RED);
        assert_eq!(*frame.get_pixel(9, 0), GAP);
        assert_eq!(*frame.get_pixel(110, 0), GAP);
        assert_eq!(*frame.get_pixel(10, 50), GAP);
        assert_eq!(*frame.get_pixel(10, 79), GAP);
    }

    #[test]
    fn a_scrolled_view_shows_the_matching_slice_of_each_tile() {
        let view = view_of(3, pane(), 1);
        let first = rendered(key_of_page(&view, 0), rows_numbered(100, 50));
        let second = rendered(key_of_page(&view, 1), RgbImage::from_pixel(100, 50, BLUE));
        let frame = compose(&view, 0, &[first, second]);
        assert_eq!(*frame.get_pixel(10, 0), Rgb([20, 20, 20]));
        assert_eq!(*frame.get_pixel(10, 29), Rgb([49, 49, 49]));
        assert_eq!(*frame.get_pixel(10, 30), GAP);
        assert_eq!(*frame.get_pixel(10, 60), BLUE);
        assert_eq!(*frame.get_pixel(109, 79), BLUE);
    }

    #[test]
    fn tiles_of_another_generation_are_not_drawn() {
        let view = view_of(3, pane(), 0);
        let stale = rendered(key_of_page(&view, 0), RgbImage::from_pixel(100, 50, RED));
        let frame = compose(&view, 1, &[stale]);
        assert!(frame.pixels().all(|pixel| *pixel == GAP));
    }
}
