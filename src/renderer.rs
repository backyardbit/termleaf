use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use image::RgbImage;

use crate::pdf::{Pdf, PixelSize};

pub type Generation = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderKey {
    pub generation: Generation,
    pub page: usize,
    pub bounds: PixelSize,
}

#[derive(Debug, PartialEq, Eq)]
enum Request {
    Load(Generation),
    Render(RenderKey),
}

pub enum Response {
    Loaded {
        generation: Generation,
        page_count: usize,
    },
    Unreadable {
        generation: Generation,
    },
    Unchanged {
        generation: Generation,
    },
    Rendered {
        key: RenderKey,
        image: RgbImage,
    },
}

pub struct Renderer {
    requests: Sender<Request>,
}

impl Renderer {
    pub fn spawn(path: PathBuf, respond: impl Fn(Response) + Send + 'static) -> Self {
        let (requests, inbox) = mpsc::channel();
        thread::spawn(move || serve(&path, &inbox, &respond));
        Self { requests }
    }

    pub fn load(&self, generation: Generation) {
        self.send(Request::Load(generation));
    }

    pub fn render(&self, key: RenderKey) {
        self.send(Request::Render(key));
    }

    fn send(&self, request: Request) {
        let _ = self.requests.send(request);
    }
}

struct LoadedPdf {
    generation: Generation,
    pdf: Pdf,
    bytes: Vec<u8>,
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
            let page_count = pdf.page_count();
            *loaded = Some(LoadedPdf {
                generation,
                pdf,
                bytes,
            });
            Response::Loaded {
                generation,
                page_count,
            }
        }
        Err(_) => Response::Unreadable { generation },
    }
}

fn serve(path: &Path, inbox: &Receiver<Request>, respond: &impl Fn(Response)) {
    let mut loaded: Option<LoadedPdf> = None;
    let mut pending: Vec<Request> = Vec::new();
    loop {
        pending.extend(inbox.try_iter());
        if pending.is_empty() {
            match inbox.recv() {
                Ok(request) => pending.push(request),
                Err(_) => return,
            }
            continue;
        }
        match next_request(&mut pending) {
            Request::Load(generation) => respond(load(path, generation, &mut loaded)),
            Request::Render(key) => {
                let Some(current) = &loaded else {
                    continue;
                };
                if current.generation != key.generation {
                    continue;
                }
                if let Ok(image) = current.pdf.render(key.page, key.bounds) {
                    respond(Response::Rendered { key, image });
                }
            }
        }
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
    let newest = pending.pop().unwrap_or(Request::Load(0));
    pending.retain(|request| *request != newest);
    newest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(page: usize) -> Request {
        Request::Render(RenderKey {
            generation: 1,
            page,
            bounds: PixelSize {
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
    fn reloading_identical_bytes_reports_unchanged() {
        let path = scratch_copy_of_fixture();
        let (sender, responses) = mpsc::channel();
        let renderer = Renderer::spawn(path.clone(), move |response| {
            let _ = sender.send(response);
        });

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
        let renderer = Renderer::spawn(path.clone(), move |response| {
            let _ = sender.send(response);
        });

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
        let renderer = Renderer::spawn(path.clone(), move |response| {
            let _ = sender.send(response);
        });

        renderer.load(0);
        next_response(&responses);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(b"\n%%EOF\n");
        std::fs::write(&path, &bytes).unwrap();
        renderer.load(1);
        assert!(matches!(
            next_response(&responses),
            Response::Loaded {
                generation: 1,
                page_count: 3
            }
        ));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_deleted_file_is_unreadable() {
        let path = scratch_copy_of_fixture();
        let (sender, responses) = mpsc::channel();
        let renderer = Renderer::spawn(path.clone(), move |response| {
            let _ = sender.send(response);
        });

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
}
