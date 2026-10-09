use anyhow::{Context, Result, bail, ensure};
use image::RgbImage;
use mupdf::{
    Colorspace, DestinationKind, Device, DisplayList, Document, Matrix, Page, Pixmap, Rect,
};

const END_OF_FILE_MARKER: &[u8] = b"%%EOF";
const TRAILER_WINDOW: usize = 1024;
const WHITE: i32 = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PixelSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PixelRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Scale(u32);

impl Scale {
    const UNITS_PER_PIXEL: f64 = 1000.0;

    pub fn from_pixels_per_point(pixels_per_point: f64) -> Self {
        Self(nearest_whole(pixels_per_point * Self::UNITS_PER_PIXEL).max(1))
    }

    pub fn at_most(pixels_per_point: f64) -> Self {
        Self(nearest_whole((pixels_per_point * Self::UNITS_PER_PIXEL).floor()).max(1))
    }

    pub fn pixels_per_point(self) -> f64 {
        f64::from(self.0) / Self::UNITS_PER_PIXEL
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is rounded and clamped into u32's range first"
)]
pub fn nearest_whole(value: f64) -> u32 {
    if value.is_nan() {
        return 0;
    }
    value.round().clamp(0.0, f64::from(u32::MAX)) as u32
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSize {
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointRect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl PointRect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        (self.x0..=self.x1).contains(&x) && (self.y0..=self.y1).contains(&y)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinkTarget {
    pub page: usize,
    pub top: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Link {
    pub area: PointRect,
    pub target: LinkTarget,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PageInfo {
    pub size: PageSize,
    pub links: Vec<Link>,
}

pub struct Pdf {
    recorded: Option<RecordedPage>,
    document: Document,
    page_count: usize,
}

struct RecordedPage {
    index: usize,
    list: DisplayList,
}

impl Pdf {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        ensure!(is_complete(bytes), "the PDF is incomplete");
        let document = Document::from_bytes(bytes, "application/pdf")?;
        let page_count = usize::try_from(document.page_count()?)?;
        ensure!(page_count > 0, "the PDF has no pages");
        Ok(Self {
            recorded: None,
            document,
            page_count,
        })
    }

    pub fn pages(&self) -> Vec<PageInfo> {
        (0..self.page_count)
            .map(|index| {
                self.load(index)
                    .and_then(|page| page_info(&page))
                    .unwrap_or(PageInfo {
                        size: PageSize {
                            width: 612.0,
                            height: 792.0,
                        },
                        links: Vec::new(),
                    })
            })
            .collect()
    }

    pub fn page_count(&self) -> usize {
        self.page_count
    }

    pub fn search(&self, page_index: usize, query: &str) -> Result<Vec<crate::search::Hit>> {
        use mupdf::text_page::SearchHitResponse;
        let mut hits = Vec::new();
        if query.trim().is_empty() {
            return Ok(hits);
        }
        let page = self.load(page_index)?;
        let bounds = page.bounds()?;
        let text = page.to_text_page(mupdf::TextPageFlags::empty())?;
        text.search_cb(query, &mut hits, |hits, quads| {
            let areas = quads
                .iter()
                .map(|quad| {
                    let points = [&quad.ul, &quad.ur, &quad.ll, &quad.lr];
                    PointRect {
                        x0: points.iter().map(|p| p.x).fold(f32::INFINITY, f32::min) - bounds.x0,
                        y0: points.iter().map(|p| p.y).fold(f32::INFINITY, f32::min) - bounds.y0,
                        x1: points.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max)
                            - bounds.x0,
                        y1: points.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max)
                            - bounds.y0,
                    }
                })
                .collect();
            hits.push(crate::search::Hit {
                page: page_index,
                areas,
            });
            if hits.len() >= crate::search::MAX_HITS {
                SearchHitResponse::AbortSearch
            } else {
                SearchHitResponse::ContinueSearch
            }
        })?;
        Ok(hits)
    }

    pub fn render(
        &mut self,
        page_index: usize,
        scale: Scale,
        region: PixelRegion,
    ) -> Result<RgbImage> {
        let list = self.recording(page_index)?;
        let bounds = list.bounds();
        let mut transform = Matrix::new_translate(-bounds.x0, -bounds.y0);
        let pixels_per_point = scale_factor(scale);
        transform.concat(Matrix::new_scale(pixels_per_point, pixels_per_point));
        let mut pixmap = Pixmap::new(
            &Colorspace::device_rgb(),
            i32::try_from(region.x)?,
            i32::try_from(region.y)?,
            i32::try_from(region.width)?,
            i32::try_from(region.height)?,
            false,
        )?;
        pixmap.clear_with(WHITE)?;
        {
            let device = Device::from_pixmap(&pixmap)?;
            list.run(&device, &transform, Rect::from(pixmap.rect()))?;
        }
        rgb_image(&pixmap)
    }

    fn recording(&mut self, page_index: usize) -> Result<&DisplayList> {
        let stale = self
            .recorded
            .as_ref()
            .is_none_or(|recorded| recorded.index != page_index);
        if stale {
            let list = self.load(page_index)?.to_display_list(true)?;
            self.recorded = Some(RecordedPage {
                index: page_index,
                list,
            });
        }
        self.recorded
            .as_ref()
            .map(|recorded| &recorded.list)
            .context("no page is recorded")
    }

    fn load(&self, page_index: usize) -> Result<Page> {
        Ok(self.document.load_page(i32::try_from(page_index)?)?)
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "a scale is a few thousand units at most, well inside f32's precision"
)]
fn scale_factor(scale: Scale) -> f32 {
    scale.pixels_per_point() as f32
}

fn page_info(page: &Page) -> Result<PageInfo> {
    let bounds = page.bounds()?;
    let links = page
        .links()?
        .filter_map(|link| {
            let destination = link.dest?;
            let page = usize::try_from(destination.loc.page_number).ok()?;
            Some(Link {
                area: PointRect {
                    x0: link.bounds.x0 - bounds.x0,
                    y0: link.bounds.y0 - bounds.y0,
                    x1: link.bounds.x1 - bounds.x0,
                    y1: link.bounds.y1 - bounds.y0,
                },
                target: LinkTarget {
                    page,
                    top: destination_top(destination.kind),
                },
            })
        })
        .collect();
    Ok(PageInfo {
        size: PageSize {
            width: bounds.x1 - bounds.x0,
            height: bounds.y1 - bounds.y0,
        },
        links,
    })
}

fn destination_top(kind: DestinationKind) -> Option<f32> {
    match kind {
        DestinationKind::XYZ { top, .. }
        | DestinationKind::FitH { top }
        | DestinationKind::FitBH { top } => top,
        DestinationKind::FitR { top, .. } => Some(top),
        _ => None,
    }
}

fn rgb_image(pixmap: &Pixmap) -> Result<RgbImage> {
    let width = pixmap.width();
    let height = pixmap.height();
    let stride = usize::try_from(pixmap.stride())?;
    let row_bytes = usize::try_from(width)? * 3;
    if pixmap.n() != 3 {
        bail!("unexpected pixmap with {} components", pixmap.n());
    }
    let samples = pixmap.samples();
    let mut pixels = Vec::with_capacity(row_bytes * usize::try_from(height)?);
    for row in samples.chunks(stride) {
        pixels.extend_from_slice(row.get(..row_bytes).unwrap_or(row));
    }
    RgbImage::from_raw(width, height, pixels).context("pixmap size mismatch")
}

fn is_complete(bytes: &[u8]) -> bool {
    let tail = &bytes[bytes.len().saturating_sub(TRAILER_WINDOW)..];
    tail.windows(END_OF_FILE_MARKER.len())
        .any(|window| window == END_OF_FILE_MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn unicode_and_multiline_matches_keep_one_navigation_stop_per_occurrence() {
        let pdf = open("search.pdf");
        assert_eq!(pdf.search(0, "CAFÉ").unwrap().len(), 1);
        assert_eq!(pdf.search(0, "naïve résumé").unwrap().len(), 1);
        assert_eq!(pdf.search(0, "needle").unwrap().len(), 2);
        let multiline = pdf.search(0, "across several").unwrap();
        assert_eq!(multiline.len(), 1);
        assert_eq!(multiline[0].areas.len(), 2);
    }

    #[test]
    fn native_text_search_finds_each_occurrence_and_ignores_case() {
        let pdf = open("three-pages.pdf");
        let hits = pdf.search(0, "THIS IS PAGE").unwrap();
        assert_eq!(hits.len(), 1);
        assert!(!hits[0].areas.is_empty());
        assert!(hits[0].areas[0].x1 > hits[0].areas[0].x0);
        assert!(pdf.search(0, "").unwrap().is_empty());
        assert!(pdf.search(0, "not present café").unwrap().is_empty());
    }

    #[test]
    fn counts_pages() {
        assert_eq!(open("three-pages.pdf").pages().len(), 3);
    }

    fn open(name: &str) -> Pdf {
        Pdf::from_bytes(&fs::read(fixture(name)).unwrap()).unwrap()
    }

    #[test]
    fn reports_every_page_size_in_points() {
        let pages = open("three-pages.pdf").pages();
        assert_eq!(pages.len(), 3);
        let letter = pages[0].size;
        assert!((letter.width - 612.0).abs() < 1.0 && (letter.height - 792.0).abs() < 1.0);
    }

    #[test]
    fn renders_just_the_requested_region() {
        let region = PixelRegion {
            x: 100,
            y: 200,
            width: 120,
            height: 80,
        };
        let image = open("three-pages.pdf")
            .render(0, Scale::from_pixels_per_point(1.5), region)
            .unwrap();
        assert_eq!(image.dimensions(), (120, 80));
    }

    #[test]
    fn a_region_is_cut_from_the_same_render_as_the_whole_page() {
        let mut pdf = open("three-pages.pdf");
        let scale = Scale::from_pixels_per_point(1.0);
        let whole = pdf
            .render(
                0,
                scale,
                PixelRegion {
                    x: 0,
                    y: 0,
                    width: 612,
                    height: 792,
                },
            )
            .unwrap();
        let region = PixelRegion {
            x: 90,
            y: 60,
            width: 200,
            height: 100,
        };
        let part = pdf.render(0, scale, region).unwrap();
        let expected = image::imageops::crop_imm(&whole, 90, 60, 200, 100).to_image();
        assert_eq!(part, expected);
    }

    #[test]
    fn a_page_renders_the_same_after_another_page_was_rendered() {
        let scale = Scale::from_pixels_per_point(1.0);
        let region = PixelRegion {
            x: 50,
            y: 40,
            width: 300,
            height: 200,
        };
        let first = open("three-pages.pdf").render(0, scale, region).unwrap();
        let mut pdf = open("three-pages.pdf");
        pdf.render(1, scale, region).unwrap();
        let again = pdf.render(0, scale, region).unwrap();
        assert_eq!(again, first);
        assert_ne!(pdf.render(1, scale, region).unwrap(), first);
    }

    #[test]
    fn the_region_outside_the_page_is_white() {
        let region = PixelRegion {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        };
        let image = open("three-pages.pdf")
            .render(0, Scale::from_pixels_per_point(1.0), region)
            .unwrap();
        assert_eq!(image.get_pixel(0, 0).0, [255, 255, 255]);
    }

    #[test]
    fn finds_an_internal_link_and_its_target_page() {
        let pages = open("linked.pdf").pages();
        let link = pages[0]
            .links
            .iter()
            .find(|link| link.target.page == 2)
            .expect("a link to page three");
        assert!(link.area.x1 > link.area.x0 && link.area.y1 > link.area.y0);
        assert!(link.area.y1 < pages[0].size.height);
    }

    #[test]
    fn a_link_lands_where_its_section_starts_measured_from_the_top() {
        let pages = open("linked.pdf").pages();
        let link = pages[0]
            .links
            .iter()
            .find(|link| link.target.page == 2)
            .unwrap();
        let top = link.target.top.expect("the section's position");
        assert!(top > 0.0 && top < pages[2].size.height / 2.0);
    }

    #[test]
    fn a_link_contains_the_points_inside_its_area() {
        let area = PointRect {
            x0: 10.0,
            y0: 20.0,
            x1: 50.0,
            y1: 30.0,
        };
        assert!(area.contains(12.0, 25.0));
        assert!(!area.contains(60.0, 25.0));
    }

    #[test]
    fn scale_round_trips_through_its_stored_form() {
        let scale = Scale::from_pixels_per_point(1.25);
        assert!((scale.pixels_per_point() - 1.25).abs() < 1e-9);
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
    fn an_image_split_across_tiles_lands_where_the_whole_page_puts_it() {
        let scale = Scale::from_pixels_per_point(1.0);
        let whole = open("image.pdf")
            .render(
                0,
                scale,
                PixelRegion {
                    x: 0,
                    y: 0,
                    width: 200,
                    height: 160,
                },
            )
            .unwrap();
        let mut tiled = open("image.pdf");
        for (x, width) in [(0, 80), (80, 120)] {
            let tile = tiled
                .render(
                    0,
                    scale,
                    PixelRegion {
                        x,
                        y: 0,
                        width,
                        height: 160,
                    },
                )
                .unwrap();
            let expected = image::imageops::crop_imm(&whole, x, 0, width, 160).to_image();
            let worst = tile
                .as_raw()
                .iter()
                .zip(expected.as_raw())
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap();
            assert!(worst <= 2, "tile at x {x} is off by {worst} levels");
        }
    }
}
