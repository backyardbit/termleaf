use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use crossterm::event::{
    self, Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use image::{DynamicImage, RgbImage, RgbaImage};
use ratatui::DefaultTerminal;
use ratatui::Frame;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::{FontSize, Image, Resize};

use crate::keys::{Command, Key, KeyParser};
use crate::pdf::PixelSize;
use crate::renderer::{Generation, RenderKey, Renderer, Response};
use crate::viewer::Viewer;
use crate::watch::watch;

const RELOAD_SETTLE: Duration = Duration::from_millis(100);
const RELOAD_RETRY: Duration = Duration::from_millis(250);
const MAX_RELOAD_RETRIES: u32 = 3;
const CACHED_PAGES: usize = 8;
const IDLE_WAIT: Duration = Duration::from_secs(3600);

enum Event {
    Key(Key),
    Resized,
    FileChanged,
    Renderer(Response),
}

pub fn run(path: PathBuf) -> Result<()> {
    if !path.is_file() {
        bail!("no such file: {}", path.display());
    }
    let (events, inbox) = mpsc::channel();
    let renderer = {
        let events = events.clone();
        Renderer::spawn(path.clone(), move |response| {
            let _ = events.send(Event::Renderer(response));
        })
    };
    renderer.load(0);
    let page_count = match inbox.recv().context("the render thread stopped")? {
        Event::Renderer(Response::Loaded { page_count, .. }) => page_count,
        _ => bail!("{} is not a readable PDF", path.display()),
    };

    let _watcher = {
        let events = events.clone();
        watch(&path, move || {
            let _ = events.send(Event::FileChanged);
        })?
    };

    let mut terminal = ratatui::init();
    let picker = match kitty_picker() {
        Ok(picker) => picker,
        Err(error) => {
            ratatui::restore();
            return Err(error);
        }
    };
    spawn_input(events);

    let mut app = App {
        file_name: display_name(&path),
        viewer: Viewer::new(page_count),
        keys: KeyParser::default(),
        renderer,
        font_size: picker.font_size(),
        picker,
        generation: 0,
        requested_generation: 0,
        reload_at: None,
        reload_retries: 0,
        cache: Vec::new(),
        in_flight: Vec::new(),
        shown: None,
    };
    let result = app.event_loop(&mut terminal, &inbox);
    ratatui::restore();
    result
}

fn kitty_picker() -> Result<Picker> {
    let picker = Picker::from_query_stdio().context("querying the terminal")?;
    if picker.protocol_type() != ProtocolType::Kitty {
        bail!(
            "termleaf needs a terminal that supports the Kitty graphics protocol \
             (for example Kitty or Ghostty, optionally inside herdr)"
        );
    }
    Ok(picker)
}

fn spawn_input(events: Sender<Event>) {
    thread::spawn(move || {
        while let Ok(terminal_event) = event::read() {
            let translated = match terminal_event {
                TerminalEvent::Key(key) => translate_key(key).map(Event::Key),
                TerminalEvent::Resize(..) => Some(Event::Resized),
                _ => None,
            };
            if let Some(event) = translated
                && events.send(event).is_err()
            {
                return;
            }
        }
    });
}

fn translate_key(key: KeyEvent) -> Option<Key> {
    if key.kind != KeyEventKind::Press {
        return None;
    }
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(Key::Interrupt),
        KeyCode::Char(character) => Some(Key::Char(character)),
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Esc => Some(Key::Escape),
        KeyCode::Backspace => Some(Key::Backspace),
        _ => None,
    }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

struct App {
    file_name: String,
    viewer: Viewer,
    keys: KeyParser,
    renderer: Renderer,
    picker: Picker,
    font_size: FontSize,
    generation: Generation,
    requested_generation: Generation,
    reload_at: Option<Instant>,
    reload_retries: u32,
    cache: Vec<(RenderKey, Protocol)>,
    in_flight: Vec<RenderKey>,
    shown: Option<RenderKey>,
}

impl App {
    fn event_loop(
        &mut self,
        terminal: &mut DefaultTerminal,
        inbox: &Receiver<Event>,
    ) -> Result<()> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;
            let wait = self
                .reload_at
                .map_or(IDLE_WAIT, |at| at.saturating_duration_since(Instant::now()));
            match inbox.recv_timeout(wait) {
                Ok(Event::Key(key)) => {
                    if let Some(command) = self.keys.feed(key) {
                        if command == Command::Quit {
                            return Ok(());
                        }
                        self.viewer.apply(command);
                    }
                }
                Ok(Event::Resized) => {}
                Ok(Event::FileChanged) => {
                    self.reload_retries = 0;
                    self.reload_at = Some(Instant::now() + RELOAD_SETTLE);
                }
                Ok(Event::Renderer(response)) => self.receive(response),
                Err(RecvTimeoutError::Timeout) => self.start_reload(),
                Err(RecvTimeoutError::Disconnected) => bail!("all event sources stopped"),
            }
        }
    }

    fn start_reload(&mut self) {
        self.reload_at = None;
        self.requested_generation += 1;
        self.renderer.load(self.requested_generation);
    }

    fn receive(&mut self, response: Response) {
        match response {
            Response::Loaded {
                generation,
                page_count,
            } if generation == self.requested_generation => {
                self.generation = generation;
                self.reload_retries = 0;
                self.viewer.reloaded(page_count);
                let shown = self.shown;
                self.cache
                    .retain(|(key, _)| key.generation == generation || Some(*key) == shown);
                self.in_flight.clear();
            }
            Response::Unchanged { generation } if generation == self.requested_generation => {
                self.reload_retries = 0;
                self.viewer.reloaded(self.viewer.page_count());
            }
            Response::Unreadable { generation } if generation == self.requested_generation => {
                self.viewer.unreadable();
                if self.reload_at.is_none() && self.reload_retries < MAX_RELOAD_RETRIES {
                    self.reload_retries += 1;
                    self.reload_at = Some(Instant::now() + RELOAD_RETRY);
                }
            }
            Response::Rendered { key, image } => {
                self.in_flight.retain(|pending| *pending != key);
                if key.generation != self.generation {
                    return;
                }
                let cells = cells_for(key.bounds, self.font_size);
                if let Ok(protocol) = self.picker.new_protocol(
                    DynamicImage::ImageRgba8(pad_to_cells(image, self.font_size)),
                    cells,
                    Resize::Fit(None),
                ) {
                    self.cache.push((key, protocol));
                    self.evict();
                }
            }
            Response::Loaded { .. } | Response::Unreadable { .. } | Response::Unchanged { .. } => {}
        }
    }

    fn evict(&mut self) {
        while self.cache.len() > CACHED_PAGES {
            let shown = self.shown;
            let Some(oldest) = self.cache.iter().position(|(key, _)| Some(*key) != shown) else {
                return;
            };
            self.cache.remove(oldest);
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let page_area = Rect {
            height: area.height.saturating_sub(1),
            ..area
        };
        let status_area = Rect {
            y: area.y + page_area.height,
            height: area.height.min(1),
            ..area
        };

        let bounds = PixelSize {
            width: u32::from(page_area.width) * u32::from(self.font_size.width),
            height: u32::from(page_area.height) * u32::from(self.font_size.height),
        };
        let wanted = RenderKey {
            generation: self.generation,
            page: self.viewer.page(),
            bounds,
        };
        if bounds.width > 0 && bounds.height > 0 {
            self.request(wanted);
            if self.cached(wanted).is_some() {
                self.shown = Some(wanted);
                self.prefetch_neighbours(wanted);
            }
        }
        if let Some(protocol) = self.shown.and_then(|key| self.cached(key)) {
            let size = protocol.size();
            frame.render_widget(Image::new(protocol), centered(page_area, size));
        }

        let status = self
            .viewer
            .status_line(&self.file_name, self.keys.command_line());
        frame.render_widget(
            Line::styled(status, Style::default().add_modifier(Modifier::REVERSED)),
            status_area,
        );
    }

    fn prefetch_neighbours(&mut self, around: RenderKey) {
        let neighbours = [around.page.checked_sub(1), around.page.checked_add(1)];
        for page in neighbours.into_iter().flatten() {
            if page < self.viewer.page_count() {
                self.request(RenderKey { page, ..around });
            }
        }
    }

    fn request(&mut self, key: RenderKey) {
        if self.cached(key).is_some() || self.in_flight.contains(&key) {
            return;
        }
        self.in_flight.push(key);
        self.renderer.render(key);
    }

    fn cached(&self, key: RenderKey) -> Option<&Protocol> {
        self.cache
            .iter()
            .find(|(cached, _)| *cached == key)
            .map(|(_, protocol)| protocol)
    }
}

fn pad_to_cells(image: RgbImage, font_size: FontSize) -> RgbaImage {
    let cell_width = u32::from(font_size.width.max(1));
    let cell_height = u32::from(font_size.height.max(1));
    let width = image.width().div_ceil(cell_width) * cell_width;
    let height = image.height().div_ceil(cell_height) * cell_height;
    let mut padded = RgbaImage::new(width, height);
    image::imageops::overlay(
        &mut padded,
        &DynamicImage::ImageRgb8(image).to_rgba8(),
        0,
        0,
    );
    padded
}

fn cells_for(bounds: PixelSize, font_size: FontSize) -> Size {
    let columns = bounds.width / u32::from(font_size.width.max(1));
    let rows = bounds.height / u32::from(font_size.height.max(1));
    Size {
        width: u16::try_from(columns).unwrap_or(u16::MAX),
        height: u16::try_from(rows).unwrap_or(u16::MAX),
    }
}

fn centered(area: Rect, size: Size) -> Rect {
    let width = size.width.min(area.width);
    let height = size.height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centers_a_smaller_image() {
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(centered(area, Size::new(40, 24)), Rect::new(20, 0, 40, 24));
    }

    #[test]
    fn clamps_an_oversized_image_to_the_area() {
        let area = Rect::new(2, 1, 10, 5);
        assert_eq!(centered(area, Size::new(30, 30)), Rect::new(2, 1, 10, 5));
    }

    #[test]
    fn pads_a_render_up_to_whole_cells() {
        let padded = pad_to_cells(RgbImage::new(95, 41), FontSize::new(10, 20));
        assert_eq!(padded.dimensions(), (100, 60));
    }

    #[test]
    fn padding_keeps_the_rendered_pixels_opaque() {
        let render = RgbImage::from_pixel(95, 41, image::Rgb([200, 100, 50]));
        let padded = pad_to_cells(render, FontSize::new(10, 20));
        assert_eq!(padded.get_pixel(94, 40).0, [200, 100, 50, 255]);
    }

    #[test]
    fn padding_is_transparent() {
        let padded = pad_to_cells(RgbImage::new(95, 41), FontSize::new(10, 20));
        assert_eq!(padded.get_pixel(99, 59).0[3], 0);
        assert_eq!(padded.get_pixel(95, 0).0[3], 0);
    }

    #[test]
    fn a_render_aligned_to_cells_keeps_its_size() {
        let padded = pad_to_cells(RgbImage::new(100, 60), FontSize::new(10, 20));
        assert_eq!(padded.dimensions(), (100, 60));
    }

    #[test]
    fn converts_pixel_bounds_to_cells() {
        let bounds = PixelSize {
            width: 800,
            height: 480,
        };
        assert_eq!(cells_for(bounds, FontSize::new(10, 20)), Size::new(80, 24));
    }

    #[test]
    fn control_c_interrupts() {
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(translate_key(key), Some(Key::Interrupt));
    }
}
