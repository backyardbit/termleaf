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

fn serve(path: &Path, inbox: &Receiver<Request>, respond: &impl Fn(Response)) {
    let mut loaded: Option<(Generation, Pdf)> = None;
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
            Request::Load(generation) => match Pdf::open(path) {
                Ok(pdf) => {
                    respond(Response::Loaded {
                        generation,
                        page_count: pdf.page_count(),
                    });
                    loaded = Some((generation, pdf));
                }
                Err(_) => respond(Response::Unreadable { generation }),
            },
            Request::Render(key) => {
                let Some((generation, pdf)) = &loaded else {
                    continue;
                };
                if *generation != key.generation {
                    continue;
                }
                if let Ok(image) = pdf.render(key.page, key.bounds) {
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
