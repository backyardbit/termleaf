use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use image::RgbImage;

use crate::search::{Highlights, Hit, MAX_HITS, Ticket};

use crate::pdf::{PageInfo, Pdf, PixelRegion, Scale};

pub type Generation = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderKey {
    pub generation: Generation,
    pub page: usize,
    pub scale: Scale,
    pub region: PixelRegion,
}

#[derive(Debug, PartialEq)]
enum Request {
    Load(Generation),
    Render(RenderKey),
    Search(Ticket),
    Highlight {
        document: Generation,
        generation: Generation,
        highlights: Highlights,
    },
}

pub enum Response {
    Searched {
        ticket: Ticket,
        result: Result<Vec<Hit>, String>,
    },
    Loaded {
        generation: Generation,
        pages: Vec<PageInfo>,
    },
    Unreadable {
        generation: Generation,
    },
    Unchanged {
        generation: Generation,
    },
}

pub struct Renderer {
    requests: Sender<Vec<Request>>,
}

impl Renderer {
    pub fn spawn(
        path: PathBuf,
        respond: impl Fn(Response) + Send + 'static,
        deliver: impl Fn(RenderKey, RgbImage) + Send + 'static,
    ) -> Self {
        let (requests, inbox) = mpsc::channel();
        thread::spawn(move || serve(&path, &inbox, &respond, &deliver));
        Self { requests }
    }

    pub fn search(&self, ticket: Ticket) {
        self.send(Request::Search(ticket));
    }

    pub fn highlight(&self, document: Generation, generation: Generation, highlights: Highlights) {
        self.send(Request::Highlight {
            document,
            generation,
            highlights,
        });
    }

    pub fn load(&self, generation: Generation) {
        self.send(Request::Load(generation));
    }

    pub fn render(&self, key: RenderKey) {
        self.send(Request::Render(key));
    }

    pub fn render_all(&self, keys: &[RenderKey]) {
        if keys.is_empty() {
            return;
        }
        let _ = self
            .requests
            .send(keys.iter().copied().map(Request::Render).collect());
    }

    fn send(&self, request: Request) {
        let _ = self.requests.send(vec![request]);
    }
}

struct LoadedPdf {
    generation: Generation,
    pdf: Pdf,
    bytes: Vec<u8>,
    rendered_generation: Generation,
    highlights: Highlights,
}

fn load(path: &Path, generation: Generation, loaded: &mut Option<LoadedPdf>) -> Response {
    let Ok(bytes) = fs::read(path) else {
        return Response::Unreadable { generation };
    };
    if loaded
        .as_ref()
        .is_some_and(|current| current.bytes == bytes)
    {
        return Response::Unchanged { generation };
    }
    match Pdf::from_bytes(&bytes) {
        Ok(pdf) => {
            let pages = pdf.pages();
            *loaded = Some(LoadedPdf {
                generation,
                rendered_generation: generation,
                highlights: Highlights::default(),
                pdf,
                bytes,
            });
            Response::Loaded { generation, pages }
        }
        Err(_) => Response::Unreadable { generation },
    }
}

fn serve(
    path: &Path,
    inbox: &Receiver<Vec<Request>>,
    respond: &impl Fn(Response),
    deliver: &impl Fn(RenderKey, RgbImage),
) {
    let mut loaded: Option<LoadedPdf> = None;
    let mut pending: Vec<Request> = Vec::new();
    let mut searching: Option<Scan> = None;
    loop {
        pending.extend(inbox.try_iter().flatten());
        if pending.is_empty() && searching.is_some() {
            scan_page(&mut searching, loaded.as_ref(), respond);
            continue;
        }
        if pending.is_empty() {
            match inbox.recv() {
                Ok(batch) => pending.extend(batch),
                Err(_) => return,
            }
            continue;
        }
        match next_request(&mut pending) {
            Request::Load(generation) => {
                respond(load(path, generation, &mut loaded));
            }
            Request::Search(ticket) => {
                searching = (!ticket.query.is_empty()).then_some(Scan {
                    ticket,
                    page: 0,
                    hits: Vec::new(),
                });
            }
            Request::Highlight {
                document,
                generation,
                highlights,
            } => {
                if let Some(current) = &mut loaded
                    && current.generation == document
                {
                    current.rendered_generation = generation;
                    current.highlights = highlights;
                }
            }
            Request::Render(key) => {
                let Some(current) = &mut loaded else {
                    continue;
                };
                if current.rendered_generation != key.generation {
                    continue;
                }
                if let Ok(mut image) = current.pdf.render(key.page, key.scale, key.region) {
                    current
                        .highlights
                        .tint(key.page, key.scale, key.region, &mut image);
                    deliver(key, image);
                }
            }
        }
    }
}

struct Scan {
    ticket: Ticket,
    page: usize,
    hits: Vec<Hit>,
}

fn scan_page(scan: &mut Option<Scan>, loaded: Option<&LoadedPdf>, respond: &impl Fn(Response)) {
    let Some(mut current) = scan.take() else {
        return;
    };
    let Some(loaded) = loaded.filter(|loaded| loaded.generation == current.ticket.document) else {
        return;
    };
    match loaded.pdf.search(current.page, &current.ticket.query) {
        Ok(hits) => current
            .hits
            .extend(hits.into_iter().take(MAX_HITS - current.hits.len())),
        Err(error) => {
            respond(Response::Searched {
                ticket: current.ticket,
                result: Err(error.to_string()),
            });
            return;
        }
    }
    current.page += 1;
    if current.page == loaded.pdf.page_count() || current.hits.len() >= MAX_HITS {
        respond(Response::Searched {
            ticket: current.ticket,
            result: Ok(current.hits),
        });
    } else {
        *scan = Some(current);
    }
}

fn next_request(pending: &mut Vec<Request>) -> Request {
    if let Some(index) = pending
        .iter()
        .rposition(|request| matches!(request, Request::Load(_)))
    {
        let load = pending.remove(index);
        pending.retain(|request| !matches!(request, Request::Load(_)));
        return load;
    }
    if let Some(index) = pending
        .iter()
        .position(|request| !matches!(request, Request::Render(_)))
    {
        return pending.remove(index);
    }
    let newest = pending.pop().unwrap_or(Request::Load(0));
    pending.retain(|request| *request != newest);
    if let Request::Render(wanted) = &newest {
        pending.retain(|request| match request {
            Request::Render(key) => key.scale == wanted.scale,
            _ => true,
        });
    }
    newest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(page: usize) -> Request {
        render_at(page, 1.0)
    }

    fn render_at(page: usize, pixels_per_point: f64) -> Request {
        Request::Render(RenderKey {
            generation: 1,
            page,
            scale: Scale::from_pixels_per_point(pixels_per_point),
            region: PixelRegion {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            },
        })
    }

    fn scratch_copy_of_fixture() -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let directory = std::env::temp_dir().join(format!("termleaf-renderer-{nanos}"));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("doc.pdf");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/three-pages.pdf"),
            &path,
        )
        .unwrap();
        path
    }

    fn next_response(responses: &Receiver<Response>) -> Response {
        responses
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap()
    }

    #[test]
    fn highlight_revisions_render_pixels_without_reloading_the_pdf() {
        let path = scratch_copy_of_fixture();
        let (send, responses) = mpsc::channel();
        let (tiles, rendered) = mpsc::channel();
        let renderer = Renderer::spawn(
            path.clone(),
            move |response| {
                send.send(response).unwrap();
            },
            move |key, image| {
                tiles.send((key, image)).unwrap();
            },
        );
        renderer.load(0);
        next_response(&responses);
        let pdf = Pdf::from_bytes(&std::fs::read(&path).unwrap()).unwrap();
        renderer.highlight(
            0,
            1,
            Highlights {
                hits: pdf.search(0, "page").unwrap(),
                current: Some(0),
            },
        );
        let key = RenderKey {
            generation: 1,
            page: 0,
            scale: Scale::from_pixels_per_point(1.0),
            region: PixelRegion {
                x: 0,
                y: 0,
                width: 600,
                height: 800,
            },
        };
        renderer.render(key);
        let (returned, image) = rendered
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("highlighted tile never rendered");
        assert_eq!(returned, key);
        assert!(
            image
                .pixels()
                .any(|pixel| pixel[0] > 200 && pixel[1] > 120 && pixel[2] < 100)
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_search_scans_one_page_per_turn_and_preserves_multiline_hits() {
        let path = scratch_copy_of_fixture();
        let mut loaded = None;
        load(&path, 7, &mut loaded);
        let ticket = Ticket {
            document: 7,
            query_id: 2,
            query: "page".to_owned(),
        };
        let mut scan = Some(Scan {
            ticket: ticket.clone(),
            page: 0,
            hits: Vec::new(),
        });
        let (send, responses) = mpsc::channel();
        let respond = |response| send.send(response).unwrap();
        scan_page(&mut scan, loaded.as_ref(), &respond);
        assert_eq!(scan.as_ref().unwrap().page, 1);
        assert_eq!(scan.as_ref().unwrap().hits.len(), 1);
        scan_page(&mut scan, loaded.as_ref(), &respond);
        scan_page(&mut scan, loaded.as_ref(), &respond);
        assert!(scan.is_none());
        assert!(
            matches!(next_response(&responses), Response::Searched { ticket: returned, result: Ok(hits) } if returned == ticket && hits.len() == 3)
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn queued_cancellation_precedes_render_work() {
        let ticket = Ticket {
            document: 1,
            query_id: 2,
            query: String::new(),
        };
        let mut pending = vec![render(0), Request::Search(ticket.clone()), render(1)];
        assert_eq!(next_request(&mut pending), Request::Search(ticket));
    }

    #[test]
    fn reloading_identical_bytes_reports_unchanged() {
        let path = scratch_copy_of_fixture();
        let (sender, responses) = mpsc::channel();
        let renderer = Renderer::spawn(
            path.clone(),
            move |response| {
                let _ = sender.send(response);
            },
            |_, _| {},
        );

        renderer.load(0);
        assert!(matches!(
            next_response(&responses),
            Response::Loaded { generation: 0, .. }
        ));
        renderer.load(1);
        assert!(matches!(
            next_response(&responses),
            Response::Unchanged { generation: 1 }
        ));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn identical_bytes_after_a_broken_write_still_count_as_unchanged() {
        let path = scratch_copy_of_fixture();
        let good = std::fs::read(&path).unwrap();
        let (sender, responses) = mpsc::channel();
        let renderer = Renderer::spawn(
            path.clone(),
            move |response| {
                let _ = sender.send(response);
            },
            |_, _| {},
        );

        renderer.load(0);
        next_response(&responses);
        std::fs::write(&path, &good[..good.len() / 2]).unwrap();
        renderer.load(1);
        assert!(matches!(
            next_response(&responses),
            Response::Unreadable { generation: 1 }
        ));
        std::fs::write(&path, &good).unwrap();
        renderer.load(2);
        assert!(matches!(
            next_response(&responses),
            Response::Unchanged { generation: 2 }
        ));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn changed_bytes_load_a_new_generation() {
        let path = scratch_copy_of_fixture();
        let (sender, responses) = mpsc::channel();
        let renderer = Renderer::spawn(
            path.clone(),
            move |response| {
                let _ = sender.send(response);
            },
            |_, _| {},
        );

        renderer.load(0);
        next_response(&responses);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(b"\n%%EOF\n");
        std::fs::write(&path, &bytes).unwrap();
        renderer.load(1);
        assert!(matches!(
            next_response(&responses),
            Response::Loaded { generation: 1, pages } if pages.len() == 3
        ));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_deleted_file_is_unreadable() {
        let path = scratch_copy_of_fixture();
        let (sender, responses) = mpsc::channel();
        let renderer = Renderer::spawn(
            path.clone(),
            move |response| {
                let _ = sender.send(response);
            },
            |_, _| {},
        );

        renderer.load(0);
        next_response(&responses);
        std::fs::remove_file(&path).unwrap();
        renderer.load(1);
        assert!(matches!(
            next_response(&responses),
            Response::Unreadable { generation: 1 }
        ));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn loads_jump_the_queue_and_collapse() {
        let mut pending = vec![render(1), Request::Load(2), render(2), Request::Load(3)];
        assert_eq!(next_request(&mut pending), Request::Load(3));
        assert_eq!(pending, [render(1), render(2)]);
    }

    #[test]
    fn the_newest_render_goes_first() {
        let mut pending = vec![render(1), render(2), render(3)];
        assert_eq!(next_request(&mut pending), render(3));
    }

    #[test]
    fn duplicate_renders_are_dropped() {
        let mut pending = vec![render(4), render(1), render(4)];
        assert_eq!(next_request(&mut pending), render(4));
        assert_eq!(pending, [render(1)]);
    }

    #[test]
    fn renders_at_an_outdated_scale_are_dropped() {
        let mut pending = vec![render_at(1, 1.0), render_at(2, 1.0), render_at(1, 1.5)];
        assert_eq!(next_request(&mut pending), render_at(1, 1.5));
        assert!(pending.is_empty());
    }

    #[test]
    fn a_load_keeps_the_renders_queued_behind_it() {
        let mut pending = vec![render_at(1, 1.0), Request::Load(2), render_at(2, 1.5)];
        assert_eq!(next_request(&mut pending), Request::Load(2));
        assert_eq!(pending.len(), 2);
    }
}
