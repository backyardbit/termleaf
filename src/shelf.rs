use crate::encoder::Encoded;
use crate::renderer::RenderKey;

pub struct Shelf {
    capacity: usize,
    bytes: usize,
    tiles: Vec<Encoded>,
}

impl Shelf {
    pub const fn new(capacity: usize) -> Self {
        Self {
            capacity,
            bytes: 0,
            tiles: Vec::new(),
        }
    }

    pub fn park(&mut self, tile: Encoded) {
        self.take(tile.key);
        let weight = tile.image.encoded_bytes();
        if weight > self.capacity {
            return;
        }
        while self.bytes + weight > self.capacity && !self.tiles.is_empty() {
            let oldest = self.tiles.remove(0);
            self.bytes -= oldest.image.encoded_bytes();
        }
        self.bytes += weight;
        self.tiles.push(tile);
    }

    pub fn take(&mut self, key: RenderKey) -> Option<Encoded> {
        let index = self.tiles.iter().position(|tile| tile.key == key)?;
        let tile = self.tiles.remove(index);
        self.bytes -= tile.image.encoded_bytes();
        Some(tile)
    }

    pub fn holds(&self, key: RenderKey) -> bool {
        self.tiles.iter().any(|tile| tile.key == key)
    }

    pub fn forget(&mut self, forget: impl Fn(&RenderKey) -> bool) {
        self.tiles.retain(|tile| !forget(&tile.key));
        self.bytes = self
            .tiles
            .iter()
            .map(|tile| tile.image.encoded_bytes())
            .sum();
    }
}

#[cfg(test)]
mod tests {
    use image::RgbImage;

    use super::*;
    use crate::encoder::{self, Job};
    use crate::kitty::Payload;
    use crate::layout::CellSize;
    use crate::pdf::{PixelRegion, Scale};

    const CELL: CellSize = CellSize {
        width: 10,
        height: 20,
    };

    fn key(page: usize) -> RenderKey {
        RenderKey {
            generation: 0,
            page,
            scale: Scale::from_pixels_per_point(1.0),
            region: PixelRegion {
                x: 0,
                y: 0,
                width: 40,
                height: 40,
            },
        }
    }

    fn tile(page: usize) -> Encoded {
        let image = RgbImage::new(40, 40);
        encoder::encode(
            Job {
                key: key(page),
                image,
            },
            CELL,
            Payload::Raw,
        )
    }

    #[test]
    fn parking_past_the_byte_cap_evicts_the_least_recently_parked_tiles() {
        let weight = tile(0).image.encoded_bytes();
        assert!(weight >= 40 * 40 * 4 * 4 / 3);
        let mut shelf = Shelf::new(weight * 3 - 1);
        let held = |shelf: &Shelf| -> Vec<usize> {
            (0..3).filter(|page| shelf.holds(key(*page))).collect()
        };
        shelf.park(tile(0));
        shelf.park(tile(1));
        shelf.park(tile(0));
        shelf.park(tile(0));
        assert_eq!(held(&shelf), [0, 1]);
        shelf.park(tile(2));
        assert_eq!(held(&shelf), [0, 2]);
        let mut small = Shelf::new(weight - 1);
        small.park(tile(0));
        assert!(held(&small).is_empty());
    }
}
