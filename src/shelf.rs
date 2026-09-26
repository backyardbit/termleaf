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

    #[cfg(test)]
    pub const fn bytes(&self) -> usize {
        self.bytes
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

    fn weight() -> usize {
        tile(0).image.encoded_bytes()
    }

    #[test]
    fn a_parked_tile_can_be_taken_back_once() {
        let mut shelf = Shelf::new(weight() * 4);
        shelf.park(tile(3));
        assert_eq!(shelf.take(key(3)).map(|tile| tile.key), Some(key(3)));
        assert!(shelf.take(key(3)).is_none());
        assert_eq!(shelf.bytes(), 0);
    }

    #[test]
    fn the_shelf_knows_which_tiles_it_holds() {
        let mut shelf = Shelf::new(weight() * 4);
        shelf.park(tile(3));
        assert!(shelf.holds(key(3)));
        assert!(!shelf.holds(key(4)));
    }

    #[test]
    fn the_least_recently_parked_tile_makes_room_first() {
        let mut shelf = Shelf::new(weight() * 2);
        shelf.park(tile(0));
        shelf.park(tile(1));
        shelf.park(tile(2));
        assert!(shelf.take(key(0)).is_none());
        assert!(shelf.take(key(1)).is_some());
        assert!(shelf.take(key(2)).is_some());
    }

    #[test]
    fn parked_bytes_never_exceed_the_capacity() {
        let capacity = weight() * 3 - 1;
        let mut shelf = Shelf::new(capacity);
        for page in 0..10 {
            shelf.park(tile(page));
            assert!(shelf.bytes() <= capacity);
        }
        assert_eq!(shelf.bytes(), weight() * 2);
    }

    #[test]
    fn a_tile_larger_than_the_shelf_is_not_kept() {
        let mut shelf = Shelf::new(weight() - 1);
        shelf.park(tile(0));
        assert!(shelf.take(key(0)).is_none());
        assert_eq!(shelf.bytes(), 0);
    }

    #[test]
    fn parking_the_same_tile_again_keeps_one_copy() {
        let mut shelf = Shelf::new(weight() * 4);
        shelf.park(tile(0));
        shelf.park(tile(0));
        assert_eq!(shelf.bytes(), weight());
    }

    #[test]
    fn forgotten_tiles_free_their_bytes() {
        let mut shelf = Shelf::new(weight() * 4);
        shelf.park(tile(0));
        shelf.park(tile(1));
        shelf.forget(|key| key.page == 0);
        assert!(shelf.take(key(0)).is_none());
        assert_eq!(shelf.bytes(), weight());
    }
}
