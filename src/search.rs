use image::RgbImage;

use crate::keys::{Command, KeyParser};
use crate::layout::Position;
use crate::pdf::{PixelRegion, PointRect, Scale, nearest_whole};
use crate::renderer::{Generation, Renderer};
use crate::viewer::Viewer;

pub const MAX_HITS: usize = 10_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub page: usize,
    pub areas: Vec<PointRect>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ticket {
    pub document: Generation,
    pub query_id: u64,
    pub query: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Highlights {
    pub hits: Vec<Hit>,
    pub current: Option<usize>,
}

#[derive(Default)]
pub struct Search {
    query: String,
    query_id: u64,
    revision: u64,
    pending: bool,
    error: bool,
    limited: bool,
    jump_on_result: bool,
    pub highlights: Highlights,
}

impl Search {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn command(
        &mut self,
        command: Command,
        keys: &mut KeyParser,
        viewer: &mut Viewer,
        renderer: &Renderer,
        document: Generation,
    ) -> bool {
        match command {
            Command::SearchSubmit => {
                let query = keys.take_search();
                if !query.trim().is_empty() {
                    self.query = query;
                }
                self.restart(renderer, document);
                self.jump_on_result = true;
            }
            Command::SearchDismiss => {
                self.query.clear();
                self.restart(renderer, document);
            }
            Command::SearchNext(count) => self.advance(count, false, viewer),
            Command::SearchPrevious(count) => self.advance(count, true, viewer),
            _ => return false,
        }
        true
    }

    pub fn restart(&mut self, renderer: &Renderer, document: Generation) {
        self.query_id += 1;
        self.jump_on_result = false;
        if self.highlights != Highlights::default() {
            self.revision += 1;
        }
        self.highlights = Highlights::default();
        self.pending = !self.query.is_empty();
        self.error = false;
        self.limited = false;
        renderer.search(Ticket {
            document,
            query_id: self.query_id,
            query: self.query.clone(),
        });
    }

    pub fn receive(
        &mut self,
        ticket: &Ticket,
        document: Generation,
        result: Result<Vec<Hit>, String>,
        viewer: &mut Viewer,
    ) -> bool {
        if ticket.document != document || ticket.query_id != self.query_id || !self.pending {
            return false;
        }
        self.pending = false;
        match result {
            Ok(hits) => {
                self.limited = hits.len() >= MAX_HITS;
                let view = viewer.view();
                let position = view.layout.position_at(view.point_at(0.0, 0.0));
                self.highlights.current = (!hits.is_empty()).then(|| {
                    hits.iter()
                        .position(|hit| {
                            hit.page > position.page
                                || (hit.page == position.page
                                    && hit
                                        .areas
                                        .first()
                                        .is_some_and(|area| f64::from(area.y0) >= position.y))
                        })
                        .unwrap_or(0)
                });
                self.highlights.hits = hits;
                self.revision += 1;
                if self.jump_on_result {
                    self.reveal(viewer);
                }
            }
            Err(_) => self.error = true,
        }
        true
    }

    fn advance(&mut self, count: usize, backwards: bool, viewer: &mut Viewer) {
        let Some(current) = self.highlights.current else {
            return;
        };
        let total = self.highlights.hits.len();
        let distance = count % total;
        self.highlights.current = Some(if backwards {
            (current + total - distance) % total
        } else {
            (current + distance) % total
        });
        if self.highlights.current != Some(current) {
            self.revision += 1;
        }
        self.reveal(viewer);
    }

    fn reveal(&self, viewer: &mut Viewer) {
        let Some(hit) = self
            .highlights
            .current
            .and_then(|index| self.highlights.hits.get(index))
        else {
            return;
        };
        let Some(area) = hit.areas.first() else {
            return;
        };
        viewer.show(
            Position {
                page: hit.page,
                x: f64::from(area.x0),
                y: f64::from(area.y0),
            },
            true,
        );
    }

    pub fn status(&self) -> Option<String> {
        if self.query.is_empty() {
            return None;
        }
        let state = if self.pending {
            "searching…".to_owned()
        } else if self.error {
            "search failed".to_owned()
        } else if let Some(current) = self.highlights.current {
            let suffix = if self.limited {
                "+ (limit reached)"
            } else {
                ""
            };
            format!("{}/{}{suffix}", current + 1, self.highlights.hits.len())
        } else {
            "no matches".to_owned()
        };
        Some(format!("/{} · {state}", self.query))
    }
}

impl Highlights {
    pub fn tint(&self, page: usize, scale: Scale, region: PixelRegion, image: &mut RgbImage) {
        for (index, hit) in self
            .hits
            .iter()
            .enumerate()
            .filter(|(_, hit)| hit.page == page)
        {
            for area in &hit.areas {
                let scale = scale.pixels_per_point();
                let x0 = nearest_whole((f64::from(area.x0) * scale).floor())
                    .saturating_sub(region.x)
                    .min(image.width());
                let y0 = nearest_whole((f64::from(area.y0) * scale).floor())
                    .saturating_sub(region.y)
                    .min(image.height());
                let x1 = nearest_whole((f64::from(area.x1) * scale).ceil())
                    .saturating_sub(region.x)
                    .min(image.width());
                let y1 = nearest_whole((f64::from(area.y1) * scale).ceil())
                    .saturating_sub(region.y)
                    .min(image.height());
                for y in y0..y1 {
                    for x in x0..x1 {
                        let pixel = image.get_pixel_mut(x, y);
                        pixel[2] /= 3;
                        if self.current == Some(index) {
                            pixel[1] = pixel[1].saturating_sub(pixel[1] / 3);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{CellSize, Pane};
    use crate::pdf::{PageInfo, PageSize};

    fn viewer() -> Viewer {
        Viewer::new(
            vec![
                PageInfo {
                    size: PageSize {
                        width: 600.0,
                        height: 800.0
                    },
                    links: Vec::new()
                };
                3
            ],
            CellSize {
                width: 10,
                height: 20,
            },
            Pane {
                columns: 80,
                rows: 24,
            },
        )
    }

    fn hit(page: usize) -> Hit {
        Hit {
            page,
            areas: vec![PointRect {
                x0: 100.0,
                y0: 100.0,
                x1: 180.0,
                y1: 120.0,
            }],
        }
    }

    fn waiting() -> (Search, Ticket) {
        (
            Search {
                query: "page".to_owned(),
                query_id: 2,
                pending: true,
                ..Search::default()
            },
            Ticket {
                document: 7,
                query_id: 2,
                query: "page".to_owned(),
            },
        )
    }

    #[test]
    fn navigation_wraps_both_ways_and_large_counts_stay_in_bounds() {
        let (mut search, ticket) = waiting();
        let mut viewer = viewer();
        assert!(search.receive(&ticket, 7, Ok(vec![hit(0), hit(1), hit(2)]), &mut viewer));
        assert_eq!(search.status().as_deref(), Some("/page · 1/3"));
        search.advance(1, true, &mut viewer);
        assert_eq!(viewer.page(), 2);
        assert_eq!(search.status().as_deref(), Some("/page · 3/3"));
        search.advance(1, false, &mut viewer);
        assert_eq!(viewer.page(), 0);
        search.advance(usize::MAX, false, &mut viewer);
        assert!(search.highlights.current.unwrap() < 3);
    }

    #[test]
    fn older_queries_and_documents_cannot_move_the_view_or_replace_results() {
        let (mut search, mut ticket) = waiting();
        let mut viewer = viewer();
        ticket.query_id = 1;
        assert!(!search.receive(&ticket, 7, Ok(vec![hit(2)]), &mut viewer));
        ticket.query_id = 2;
        assert!(!search.receive(&ticket, 8, Ok(vec![hit(2)]), &mut viewer));
        assert_eq!(viewer.page(), 0);
        assert!(search.highlights.hits.is_empty());
        assert!(search.pending);
    }

    #[test]
    fn no_matches_and_extraction_failure_have_distinct_statuses() {
        let (mut search, ticket) = waiting();
        let mut viewer = viewer();
        search.receive(&ticket, 7, Ok(Vec::new()), &mut viewer);
        assert_eq!(search.status().as_deref(), Some("/page · no matches"));
        search.advance(1, false, &mut viewer);
        assert_eq!(viewer.page(), 0);
        search.pending = true;
        search.receive(&ticket, 7, Err("broken text".to_owned()), &mut viewer);
        assert_eq!(search.status().as_deref(), Some("/page · search failed"));
    }

    #[test]
    fn tint_clips_at_tile_edges_and_preserves_black_text() {
        let highlights = Highlights {
            hits: vec![hit(0)],
            current: Some(0),
        };
        let mut image = RgbImage::from_pixel(100, 40, image::Rgb([255, 255, 255]));
        image.put_pixel(1, 1, image::Rgb([0, 0, 0]));
        highlights.tint(
            0,
            Scale::from_pixels_per_point(2.0),
            PixelRegion {
                x: 250,
                y: 210,
                width: 100,
                height: 40,
            },
            &mut image,
        );
        assert_eq!(image.get_pixel(0, 0).0, [255, 170, 85]);
        assert_eq!(image.get_pixel(1, 1).0, [0, 0, 0]);
        assert_eq!(image.get_pixel(99, 39).0, [255, 255, 255]);
    }

    #[test]
    fn highlights_on_other_pages_do_not_change_pixels() {
        let highlights = Highlights {
            hits: vec![hit(1)],
            current: Some(0),
        };
        let mut image = RgbImage::from_pixel(20, 20, image::Rgb([255, 255, 255]));
        let before = image.clone();
        highlights.tint(
            0,
            Scale::from_pixels_per_point(1.0),
            PixelRegion {
                x: 100,
                y: 100,
                width: 20,
                height: 20,
            },
            &mut image,
        );
        assert_eq!(image, before);
    }
}
