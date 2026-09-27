use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use anyhow::Result;
use image::RgbImage;

use crate::layout::{Pane, View};
use crate::raster::frame::{Rendered, compose};
use crate::renderer::Generation;

pub type Encode = Box<dyn Fn(&RgbImage, Pane) -> Result<String> + Send>;

pub struct Job {
    pub id: u64,
    pub view: View,
    pub generation: Generation,
    pub tiles: Vec<Rendered>,
}

pub struct Painting {
    pub id: u64,
    pub view: View,
    pub generation: Generation,
    pub bytes: String,
}

pub struct Painter {
    mailbox: Arc<Mailbox>,
}

#[derive(Default)]
struct Mailbox {
    slot: Mutex<Slot>,
    filled: Condvar,
}

#[derive(Default)]
struct Slot {
    job: Option<Job>,
    closed: bool,
}

impl Painter {
    pub fn spawn(encode: Encode, deliver: impl Fn(Painting) + Send + 'static) -> Self {
        let mailbox = Arc::new(Mailbox::default());
        let inbox = Arc::clone(&mailbox);
        thread::spawn(move || {
            while let Some(job) = inbox.take() {
                let frame = compose(&job.view, job.generation, &job.tiles);
                if let Ok(bytes) = encode(&frame, job.view.pane) {
                    deliver(Painting {
                        id: job.id,
                        view: job.view,
                        generation: job.generation,
                        bytes,
                    });
                }
            }
        });
        Self { mailbox }
    }

    pub fn submit(&self, job: Job) {
        if let Ok(mut slot) = self.mailbox.slot.lock() {
            slot.job = Some(job);
            self.mailbox.filled.notify_one();
        }
    }
}

impl Drop for Painter {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.mailbox.slot.lock() {
            slot.closed = true;
            self.mailbox.filled.notify_one();
        }
    }
}

impl Mailbox {
    fn take(&self) -> Option<Job> {
        let mut slot = self.slot.lock().ok()?;
        loop {
            if slot.closed {
                return None;
            }
            if let Some(job) = slot.job.take() {
                return Some(job);
            }
            slot = self.filled.wait(slot).ok()?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{self, Receiver};
    use std::time::Duration;

    use crate::layout::{CellSize, Layout};
    use crate::pdf::{PageSize, Scale};

    const PANE: Pane = Pane {
        columns: 4,
        rows: 2,
    };

    fn job(id: u64) -> Job {
        Job {
            id,
            view: View {
                layout: Layout::new(
                    vec![PageSize {
                        width: 40.0,
                        height: 40.0,
                    }],
                    Scale::from_pixels_per_point(1.0),
                    CellSize {
                        width: 10,
                        height: 20,
                    },
                ),
                pane: PANE,
                top: 0,
                left: 0,
            },
            generation: 0,
            tiles: Vec::new(),
        }
    }

    fn size_of_frame() -> Encode {
        Box::new(|frame, pane| {
            Ok(format!(
                "{}x{} for {}x{}",
                frame.width(),
                frame.height(),
                pane.columns,
                pane.rows
            ))
        })
    }

    fn next(paintings: &Receiver<Painting>) -> Painting {
        paintings.recv_timeout(Duration::from_secs(10)).unwrap()
    }

    #[test]
    fn a_submitted_view_comes_back_as_an_encoded_frame() {
        let (sender, paintings) = mpsc::channel();
        let painter = Painter::spawn(size_of_frame(), move |painting| {
            let _ = sender.send(painting);
        });
        painter.submit(job(7));
        let painting = next(&paintings);
        assert_eq!(painting.id, 7);
        assert_eq!(painting.view.pane, PANE);
        assert_eq!(painting.generation, 0);
        assert_eq!(painting.bytes, "40x40 for 4x2");
    }

    #[test]
    fn a_view_replaced_while_it_waits_is_never_painted() {
        let (started, encoding) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let released = Mutex::new(released);
        let (sender, paintings) = mpsc::channel();
        let painter = Painter::spawn(
            Box::new(move |_, _| {
                let _ = started.send(());
                let _ = released.lock().unwrap().recv();
                Ok(String::new())
            }),
            move |painting| {
                let _ = sender.send(painting);
            },
        );
        painter.submit(job(1));
        encoding.recv_timeout(Duration::from_secs(10)).unwrap();
        painter.submit(job(2));
        painter.submit(job(3));
        release.send(()).unwrap();
        release.send(()).unwrap();
        assert_eq!(next(&paintings).id, 1);
        assert_eq!(next(&paintings).id, 3);
        drop(release);
        assert!(paintings.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn the_painter_stops_when_it_is_dropped() {
        let (sender, paintings) = mpsc::channel();
        let painter = Painter::spawn(size_of_frame(), move |painting| {
            let _ = sender.send(painting);
        });
        drop(painter);
        assert!(matches!(
            paintings.recv_timeout(Duration::from_secs(10)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
    }
}
