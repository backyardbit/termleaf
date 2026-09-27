use std::sync::Arc;

use image::RgbImage;

use crate::raster::frame::Rendered;
use crate::renderer::RenderKey;

pub struct Tiles {
    budget: usize,
    held: Vec<Rendered>,
}

impl Tiles {
    pub fn new(budget: usize) -> Self {
        Self {
            budget,
            held: Vec::new(),
        }
    }

    pub fn contains(&self, key: RenderKey) -> bool {
        self.held.iter().any(|tile| tile.key == key)
    }

    pub fn visible(&mut self, keys: &[RenderKey]) -> Vec<Rendered> {
        let mut visible = Vec::new();
        for key in keys {
            if let Some(index) = self.held.iter().position(|tile| tile.key == *key) {
                let tile = self.held.remove(index);
                visible.push(Rendered {
                    key: tile.key,
                    image: Arc::clone(&tile.image),
                });
                self.held.push(tile);
            }
        }
        visible
    }

    pub fn insert(&mut self, key: RenderKey, image: RgbImage, protected: &[RenderKey]) {
        self.held.retain(|tile| tile.key != key);
        self.held.push(Rendered {
            key,
            image: Arc::new(image),
        });
        let mut total: usize = self.held.iter().map(bytes_of).sum();
        let mut index = 0;
        while total > self.budget && index < self.held.len() {
            if protected.contains(&self.held[index].key) {
                index += 1;
                continue;
            }
            total -= bytes_of(&self.held.remove(index));
        }
    }

    pub fn forget(&mut self, forget: impl Fn(&RenderKey) -> bool) {
        self.held.retain(|tile| !forget(&tile.key));
    }
}

fn bytes_of(tile: &Rendered) -> usize {
    tile.image.as_raw().len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::{PixelRegion, Scale};

    const TILE_BYTES: usize = 10 * 10 * 3;

    fn key(page: usize) -> RenderKey {
        RenderKey {
            generation: 0,
            page,
            scale: Scale::from_pixels_per_point(1.0),
            region: PixelRegion {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            },
        }
    }

    fn tile() -> RgbImage {
        RgbImage::new(10, 10)
    }

    fn held_pages(tiles: &Tiles) -> Vec<usize> {
        (0..10).filter(|page| tiles.contains(key(*page))).collect()
    }

    #[test]
    fn an_inserted_tile_is_held_and_handed_out() {
        let mut tiles = Tiles::new(TILE_BYTES * 4);
        tiles.insert(key(1), tile(), &[]);
        assert!(tiles.contains(key(1)));
        let visible = tiles.visible(&[key(1), key(2)]);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].key, key(1));
    }

    #[test]
    fn over_budget_the_least_recently_used_tile_goes_first() {
        let mut tiles = Tiles::new(TILE_BYTES * 2);
        tiles.insert(key(1), tile(), &[]);
        tiles.insert(key(2), tile(), &[]);
        tiles.visible(&[key(1)]);
        tiles.insert(key(3), tile(), &[]);
        assert_eq!(held_pages(&tiles), [1, 3]);
    }

    #[test]
    fn a_visible_tile_is_never_evicted() {
        let mut tiles = Tiles::new(TILE_BYTES);
        tiles.insert(key(1), tile(), &[]);
        tiles.insert(key(2), tile(), &[key(1), key(2)]);
        assert_eq!(held_pages(&tiles), [1, 2]);
    }

    #[test]
    fn forgotten_tiles_are_dropped() {
        let mut tiles = Tiles::new(TILE_BYTES * 4);
        tiles.insert(key(1), tile(), &[]);
        tiles.insert(key(2), tile(), &[]);
        tiles.forget(|held| held.page == 1);
        assert_eq!(held_pages(&tiles), [2]);
    }

    #[test]
    fn a_tile_shares_its_pixels_instead_of_copying_them() {
        let mut tiles = Tiles::new(TILE_BYTES * 4);
        tiles.insert(key(1), tile(), &[]);
        let first = tiles.visible(&[key(1)]);
        let second = tiles.visible(&[key(1)]);
        assert!(Arc::ptr_eq(&first[0].image, &second[0].image));
    }
}
