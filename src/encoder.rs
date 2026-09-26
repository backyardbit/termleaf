use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;

use image::{RgbImage, Rgba, RgbaImage};

use crate::kitty::{self, CellGrid, EncodedImage, Payload};
use crate::layout::CellSize;
use crate::renderer::RenderKey;

pub const QUEUED_TILES: usize = 2;
pub const UNCLAIMED_TILES: usize = 2;

pub struct Job {
    pub key: RenderKey,
    pub image: RgbImage,
}

pub enum Encoded {
    Tile {
        key: RenderKey,
        image: EncodedImage,
        bytes: usize,
    },
    Dropped {
        key: RenderKey,
    },
}

pub fn queue() -> (SyncSender<Job>, Receiver<Job>) {
    mpsc::sync_channel(QUEUED_TILES)
}

#[derive(Clone, Default)]
struct Window(Arc<Mutex<Vec<RenderKey>>>);

impl Window {
    fn set(&self, keys: Vec<RenderKey>) {
        if let Ok(mut wanted) = self.0.lock() {
            *wanted = keys;
        }
    }

    fn contains(&self, key: &RenderKey) -> bool {
        self.0.lock().map_or(true, |wanted| wanted.contains(key))
    }
}

pub struct Encoder {
    window: Window,
    claims: Sender<()>,
}

impl Encoder {
    pub fn spawn(
        jobs: Receiver<Job>,
        cell: CellSize,
        payload: Payload,
        deliver: impl Fn(Encoded) + Send + 'static,
    ) -> Self {
        let window = Window::default();
        let (claims, claimed) = mpsc::channel();
        for _ in 0..UNCLAIMED_TILES {
            let _ = claims.send(());
        }
        let wanted = window.clone();
        thread::spawn(move || serve(&jobs, &claimed, &wanted, cell, payload, &deliver));
        Self { window, claims }
    }

    pub fn want(&self, keys: Vec<RenderKey>) {
        self.window.set(keys);
    }

    pub fn claimed(&self) {
        let _ = self.claims.send(());
    }
}

fn serve(
    jobs: &Receiver<Job>,
    claimed: &Receiver<()>,
    window: &Window,
    cell: CellSize,
    payload: Payload,
    deliver: &impl Fn(Encoded),
) {
    let mut free_slots = 0;
    for job in jobs {
        if !window.contains(&job.key) {
            deliver(Encoded::Dropped { key: job.key });
            continue;
        }
        if free_slots == 0 {
            if claimed.recv().is_err() {
                return;
            }
            free_slots += 1;
        }
        if !window.contains(&job.key) {
            deliver(Encoded::Dropped { key: job.key });
            continue;
        }
        free_slots -= 1;
        deliver(encode(job, cell, payload));
    }
}

pub fn encode(job: Job, cell: CellSize, payload: Payload) -> Encoded {
    let padded = pad_to_cells(job.image, cell);
    let grid = grid_of(&padded, cell);
    Encoded::Tile {
        key: job.key,
        bytes: padded.as_raw().len(),
        image: kitty::encode(&padded, grid, payload),
    }
}

fn pad_to_cells(image: RgbImage, cell: CellSize) -> RgbaImage {
    let width = image.width().div_ceil(cell.width) * cell.width;
    let height = image.height().div_ceil(cell.height) * cell.height;
    let mut padded = RgbaImage::new(width, height);
    for (padded_row, row) in padded.rows_mut().zip(image.rows()) {
        for (padded_pixel, pixel) in padded_row.zip(row) {
            let [red, green, blue] = pixel.0;
            *padded_pixel = Rgba([red, green, blue, u8::MAX]);
        }
    }
    padded
}

fn grid_of(image: &RgbaImage, cell: CellSize) -> CellGrid {
    CellGrid {
        columns: u16::try_from(image.width() / cell.width).unwrap_or(u16::MAX),
        rows: u16::try_from(image.height() / cell.height).unwrap_or(u16::MAX),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::kitty::ImageId;
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
                width: 95,
                height: 41,
            },
        }
    }

    fn job(page: usize) -> Job {
        Job {
            key: key(page),
            image: RgbImage::from_pixel(95, 41, image::Rgb([200, 100, 50])),
        }
    }

    fn spawn() -> (Encoder, SyncSender<Job>, Receiver<Encoded>) {
        let (jobs, queued) = queue();
        let (sender, encoded) = mpsc::channel();
        let encoder = Encoder::spawn(queued, CELL, Payload::Zlib, move |tile| {
            let _ = sender.send(tile);
        });
        (encoder, jobs, encoded)
    }

    fn next(encoded: &Receiver<Encoded>) -> Encoded {
        encoded.recv_timeout(Duration::from_secs(10)).unwrap()
    }

    fn written(image: &EncodedImage) -> String {
        let mut sequence = String::new();
        kitty::transmit(ImageId::first(), image, &mut sequence);
        sequence
    }

    #[test]
    fn a_wanted_tile_arrives_padded_and_encoded() {
        let (encoder, jobs, encoded) = spawn();
        encoder.want(vec![key(0)]);
        jobs.send(job(0)).unwrap();
        let Encoded::Tile {
            key: got,
            image,
            bytes,
        } = next(&encoded)
        else {
            panic!("the wanted tile was dropped");
        };
        assert_eq!(got, key(0));
        assert_eq!(bytes, 100 * 60 * 4);
        let padded = pad_to_cells(job(0).image, CELL);
        let expected = kitty::encode(&padded, grid_of(&padded, CELL), Payload::Zlib);
        assert_eq!(written(&image), written(&expected));
    }

    #[test]
    fn a_tile_scrolled_out_of_the_window_is_dropped_without_encoding() {
        let (encoder, jobs, encoded) = spawn();
        encoder.want(vec![key(1)]);
        jobs.send(job(0)).unwrap();
        assert!(matches!(next(&encoded), Encoded::Dropped { key: got } if got == key(0)));
    }

    #[test]
    fn encoding_waits_once_the_ui_has_unclaimed_tiles_to_write() {
        let (encoder, jobs, encoded) = spawn();
        let pages: Vec<usize> = (0..=UNCLAIMED_TILES).collect();
        encoder.want(pages.iter().map(|page| key(*page)).collect());
        for page in &pages {
            jobs.send(job(*page)).unwrap();
        }
        for _ in 0..UNCLAIMED_TILES {
            assert!(matches!(next(&encoded), Encoded::Tile { .. }));
        }
        assert!(encoded.recv_timeout(Duration::from_millis(300)).is_err());
        encoder.claimed();
        assert!(matches!(next(&encoded), Encoded::Tile { .. }));
    }

    #[test]
    fn pads_a_render_up_to_whole_cells() {
        let padded = pad_to_cells(RgbImage::new(95, 41), CELL);
        assert_eq!(padded.dimensions(), (100, 60));
    }

    #[test]
    fn padding_keeps_the_rendered_pixels_opaque() {
        let render = RgbImage::from_pixel(95, 41, image::Rgb([200, 100, 50]));
        let padded = pad_to_cells(render, CELL);
        assert_eq!(padded.get_pixel(94, 40).0, [200, 100, 50, 255]);
    }

    #[test]
    fn padding_is_transparent() {
        let padded = pad_to_cells(RgbImage::new(95, 41), CELL);
        assert_eq!(padded.get_pixel(99, 59).0[3], 0);
        assert_eq!(padded.get_pixel(95, 0).0[3], 0);
    }

    #[test]
    fn a_render_aligned_to_cells_keeps_its_size() {
        let padded = pad_to_cells(RgbImage::new(100, 60), CELL);
        assert_eq!(padded.dimensions(), (100, 60));
    }

    #[test]
    fn a_padded_tile_covers_whole_cells() {
        let padded = pad_to_cells(RgbImage::new(795, 470), CELL);
        assert_eq!(
            grid_of(&padded, CELL),
            CellGrid {
                columns: 80,
                rows: 24
            }
        );
    }
}
