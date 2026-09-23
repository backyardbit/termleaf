use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use image::RgbImage;
use mupdf::{Colorspace, Document, Matrix};

const END_OF_FILE_MARKER: &[u8] = b"%%EOF";
const TRAILER_WINDOW: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PixelSize {
    pub width: u32,
    pub height: u32,
}

pub struct Pdf {
    document: Document,
    page_count: usize,
}

impl Pdf {
    pub fn open(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        ensure!(is_complete(bytes), "the PDF is incomplete");
        let document = Document::from_bytes(bytes, "application/pdf")?;
        let page_count = usize::try_from(document.page_count()?)?;
        ensure!(page_count > 0, "the PDF has no pages");
        Ok(Self {
            document,
            page_count,
        })
    }

    pub fn page_count(&self) -> usize {
        self.page_count
    }

    pub fn render(&self, page_index: usize, bounds: PixelSize) -> Result<RgbImage> {
        let page = self.document.load_page(i32::try_from(page_index)?)?;
        let rect = page.bounds()?;
        let scale = fit_scale(rect.x1 - rect.x0, rect.y1 - rect.y0, bounds);
        let pixmap = page.to_pixmap(
            &Matrix::new_scale(scale, scale),
            &Colorspace::device_rgb(),
            false,
            false,
        )?;
        let width = pixmap.width();
        let height = pixmap.height();
        let stride = usize::try_from(pixmap.stride())?;
        let row_bytes = usize::try_from(width)? * 3;
        if pixmap.n() != 3 {
            bail!("unexpected pixmap with {} components", pixmap.n());
        }
        let pixels = pixmap
            .samples()
            .chunks(stride)
            .flat_map(|row| row.get(..row_bytes).unwrap_or(row))
            .copied()
            .collect();
        RgbImage::from_raw(width, height, pixels).context("pixmap size mismatch")
    }
}

fn is_complete(bytes: &[u8]) -> bool {
    let tail = &bytes[bytes.len().saturating_sub(TRAILER_WINDOW)..];
    tail.windows(END_OF_FILE_MARKER.len())
        .any(|window| window == END_OF_FILE_MARKER)
}

fn fit_scale(page_width: f32, page_height: f32, bounds: PixelSize) -> f32 {
    if page_width <= 0.0 || page_height <= 0.0 {
        return 1.0;
    }
    let horizontal = bounds.width as f32 / page_width;
    let vertical = bounds.height as f32 / page_height;
    horizontal.min(vertical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn counts_pages() {
        let pdf = Pdf::open(&fixture("three-pages.pdf")).unwrap();
        assert_eq!(pdf.page_count(), 3);
    }

    #[test]
    fn renders_within_the_requested_bounds() {
        let pdf = Pdf::open(&fixture("three-pages.pdf")).unwrap();
        let bounds = PixelSize {
            width: 400,
            height: 300,
        };
        let image = pdf.render(1, bounds).unwrap();
        assert!(image.width() <= bounds.width && image.height() <= bounds.height);
        assert!(image.width() == bounds.width || image.height() == bounds.height);
    }

    #[test]
    fn rejects_a_half_written_file() {
        let bytes = fs::read(fixture("three-pages.pdf")).unwrap();
        let half = &bytes[..bytes.len() / 2];
        assert!(Pdf::from_bytes(half).is_err());
    }

    #[test]
    fn rejects_an_empty_file() {
        assert!(Pdf::from_bytes(&[]).is_err());
    }

    #[test]
    fn rejects_a_missing_file() {
        assert!(Pdf::open(&fixture("missing.pdf")).is_err());
    }

    #[test]
    fn fit_scale_is_limited_by_the_tighter_side() {
        let bounds = PixelSize {
            width: 200,
            height: 1000,
        };
        assert!((fit_scale(100.0, 100.0, bounds) - 2.0).abs() < f32::EPSILON);
    }
}
