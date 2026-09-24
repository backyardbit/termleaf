use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use crossterm::event::{
    self, DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture,
    Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use crossterm::execute;
use image::{DynamicImage, RgbImage, RgbaImage};
use ratatui::DefaultTerminal;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui_image::FontSize;
use ratatui_image::picker::cap_parser::QueryStdioOptions;
use ratatui_image::picker::{Capability, Picker, ProtocolType};

use crate::keys::{Command, Key, KeyParser, ScreenCell};
use crate::kitty::{self, CellGrid, ImageId, Payload, Placeholders};
use crate::layout::{CellSize, Pane, View};
use crate::mouse::{Gestures, MouseInput, Wheel};
use crate::pinch::{self, PinchGate, PinchInput};
use crate::renderer::{Generation, RenderKey, Renderer, Response};
use crate::viewer::Viewer;
use crate::watch::watch;

const RELOAD_SETTLE: Duration = Duration::from_millis(100);
const RELOAD_RETRY: Duration = Duration::from_millis(250);
const MAX_RELOAD_RETRIES: u32 = 3;
const TILE_BYTE_BUDGET: usize = 192 * 1024 * 1024;
const IDLE_WAIT: Duration = Duration::from_secs(3600);
const RESIZE_SETTLE: Duration = Duration::from_millis(300);

pub struct Options {
    pub pinch: bool,
}

enum Event {
    Key(Key),
    Mouse(MouseInput),
    Pinch(PinchInput),
    Focus(bool),
    Resized,
    FileChanged,
    Renderer(Response),
}

pub fn run(path: PathBuf, options: Options) -> Result<()> {
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
    let pages = match inbox.recv().context("the render thread stopped")? {
        Event::Renderer(Response::Loaded { pages, .. }) => pages,
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
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    if options.pinch {
        let _ = execute!(std::io::stdout(), EnableFocusChange);
        let pinches = events.clone();
        let _ = pinch::listen(move |input| {
            let _ = pinches.send(Event::Pinch(input));
        });
    }
    spawn_input(events);

    let cell = cell_size(picker.font_size());
    let pane = pane_of(page_area(terminal.get_frame().area()));
    let mut app = App {
        file_name: display_name(&path),
        viewer: Viewer::new(pages, cell, pane),
        keys: KeyParser::default(),
        gestures: Gestures::default(),
        pinch: PinchGate::default(),
        renderer,
        generation: 0,
        requested_generation: 0,
        reload_at: None,
        resize_settles_at: Some(Instant::now() + RESIZE_SETTLE),
        reload_retries: 0,
        tiles: Vec::new(),
        in_flight: Vec::new(),
        shown: None,
        next_id: ImageId::first(),
        payload: payload_for(&picker),
        outgoing: String::new(),
    };
    let result = app.event_loop(&mut terminal, &inbox);
    app.forget_all_tiles();
    let _ = app.flush(&mut terminal);
    let _ = execute!(std::io::stdout(), DisableFocusChange, DisableMouseCapture);
    ratatui::restore();
    result
}

fn kitty_picker() -> Result<Picker> {
    let options = QueryStdioOptions {
        kitty_compression: true,
        ..QueryStdioOptions::default()
    };
    let picker = Picker::from_query_stdio_with_options(options).context("querying the terminal")?;
    if picker.protocol_type() != ProtocolType::Kitty {
        bail!(
            "termleaf needs a terminal that supports the Kitty graphics protocol \
             (for example Kitty or Ghostty, optionally inside herdr)"
        );
    }
    Ok(picker)
}

fn payload_for(picker: &Picker) -> Payload {
    compression_if(picker.capabilities())
}

fn compression_if(capabilities: &[Capability]) -> Payload {
    if capabilities.contains(&Capability::KittyCompression) {
        Payload::Zlib
    } else {
        Payload::Raw
    }
}

fn spawn_input(events: Sender<Event>) {
    thread::spawn(move || {
        while let Ok(terminal_event) = event::read() {
            if let Some(event) = translate_event(terminal_event)
                && events.send(event).is_err()
            {
                return;
            }
        }
    });
}

fn translate_event(terminal_event: TerminalEvent) -> Option<Event> {
    match terminal_event {
        TerminalEvent::Key(key) => translate_key(key).map(Event::Key),
        TerminalEvent::Mouse(mouse) => translate_mouse(mouse).map(Event::Mouse),
        TerminalEvent::Resize(..) => Some(Event::Resized),
        TerminalEvent::FocusGained => Some(Event::Focus(true)),
        TerminalEvent::FocusLost => Some(Event::Focus(false)),
        TerminalEvent::Paste(_) => None,
    }
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

fn translate_mouse(mouse: MouseEvent) -> Option<MouseInput> {
    let at = ScreenCell {
        column: mouse.column,
        row: mouse.row,
    };
    let wheel = |direction| {
        Some(MouseInput::Wheel {
            direction,
            zoom: mouse.modifiers.contains(KeyModifiers::CONTROL),
            sideways: mouse.modifiers.contains(KeyModifiers::SHIFT),
            at,
        })
    };
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => Some(MouseInput::Press(at)),
        MouseEventKind::Drag(MouseButton::Left) => Some(MouseInput::Drag(at)),
        MouseEventKind::Up(MouseButton::Left) => Some(MouseInput::Release(at)),
        MouseEventKind::ScrollUp => wheel(Wheel::Up),
        MouseEventKind::ScrollDown => wheel(Wheel::Down),
        MouseEventKind::ScrollLeft => wheel(Wheel::Left),
        MouseEventKind::ScrollRight => wheel(Wheel::Right),
        MouseEventKind::Moved => Some(MouseInput::Hover(at)),
        _ => None,
    }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn cell_size(font_size: FontSize) -> CellSize {
    CellSize {
        width: u32::from(font_size.width.max(1)),
        height: u32::from(font_size.height.max(1)),
    }
}

fn page_area(area: Rect) -> Rect {
    Rect {
        height: area.height.saturating_sub(1),
        ..area
    }
}

fn pane_of(area: Rect) -> Pane {
    Pane {
        columns: u32::from(area.width),
        rows: u32::from(area.height),
    }
}

struct CachedTile {
    key: RenderKey,
    id: ImageId,
    bytes: usize,
}

struct Shown {
    generation: Generation,
    view: View,
}

struct App {
    file_name: String,
    viewer: Viewer,
    keys: KeyParser,
    gestures: Gestures,
    pinch: PinchGate,
    renderer: Renderer,
    generation: Generation,
    requested_generation: Generation,
    reload_at: Option<Instant>,
    resize_settles_at: Option<Instant>,
    reload_retries: u32,
    tiles: Vec<CachedTile>,
    in_flight: Vec<RenderKey>,
    shown: Option<Shown>,
    next_id: ImageId,
    payload: Payload,
    outgoing: String,
}

impl App {
    fn event_loop(
        &mut self,
        terminal: &mut DefaultTerminal,
        inbox: &Receiver<Event>,
    ) -> Result<()> {
        loop {
            let size = terminal.size()?;
            let now = Instant::now();
            self.fit_to_at(
                pane_of(page_area(Rect::new(0, 0, size.width, size.height))),
                now,
            );
            self.request_tiles_at(now);
            if self.may_transmit(now) {
                self.resize_settles_at = None;
                self.flush(terminal)?;
            }
            terminal.draw(|frame| self.draw(frame))?;
            let wait = [self.reload_at, self.resize_settles_at]
                .into_iter()
                .flatten()
                .min()
                .map_or(IDLE_WAIT, |at| at.saturating_duration_since(Instant::now()));
            let first = match inbox.recv_timeout(wait) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => {
                    if self.reload_at.is_some_and(|at| at <= Instant::now()) {
                        self.start_reload();
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => bail!("all event sources stopped"),
            };
            for event in std::iter::once(first).chain(inbox.try_iter()) {
                if self.handle(event) == Flow::Quit {
                    return Ok(());
                }
            }
        }
    }

    fn handle(&mut self, event: Event) -> Flow {
        match event {
            Event::Key(key) => {
                if let Some(command) = self.keys.feed(key) {
                    return self.apply(command);
                }
            }
            Event::Mouse(input) => {
                self.pinch.pointer(input.at());
                if let Some(command) = self.gestures.feed(input, Instant::now()) {
                    return self.apply(command);
                }
            }
            Event::Pinch(input) => {
                if let Some(command) = self.pinch.feed(input) {
                    return self.apply(command);
                }
            }
            Event::Focus(focused) => self.pinch.focus(focused),
            Event::Resized => {}
            Event::FileChanged => {
                self.reload_retries = 0;
                self.reload_at = Some(Instant::now() + RELOAD_SETTLE);
            }
            Event::Renderer(response) => self.receive(response),
        }
        Flow::Continue
    }

    fn fit_to_at(&mut self, pane: Pane, now: Instant) {
        if self.viewer.view().pane != pane {
            let cell = self.viewer.view().layout.cell();
            self.viewer.resized(cell, pane);
            self.resize_settles_at = Some(now + RESIZE_SETTLE);
        }
    }

    fn may_transmit(&self, now: Instant) -> bool {
        self.resize_settles_at
            .is_none_or(|settles_at| now >= settles_at)
    }

    fn apply(&mut self, command: Command) -> Flow {
        if command == Command::Quit {
            return Flow::Quit;
        }
        self.viewer.apply(command);
        Flow::Continue
    }

    fn start_reload(&mut self) {
        self.reload_at = None;
        self.requested_generation += 1;
        self.renderer.load(self.requested_generation);
    }

    fn receive(&mut self, response: Response) {
        match response {
            Response::Loaded { generation, pages } if generation == self.requested_generation => {
                self.generation = generation;
                self.reload_retries = 0;
                self.viewer.reloaded(pages);
                let shown = self.shown.as_ref().map(|shown| shown.generation);
                self.forget_tiles(|key| {
                    key.generation != generation && Some(key.generation) != shown
                });
                self.in_flight.clear();
            }
            Response::Unchanged { generation } if generation == self.requested_generation => {
                self.reload_retries = 0;
                self.viewer.unchanged();
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
                if key.generation == self.generation {
                    self.store(key, image);
                }
            }
            Response::Loaded { .. } | Response::Unreadable { .. } | Response::Unchanged { .. } => {}
        }
    }

    fn store(&mut self, key: RenderKey, image: RgbImage) {
        let cell = self.viewer.view().layout.cell();
        let padded = pad_to_cells(image, cell);
        let grid = grid_of(&padded, cell);
        let id = self.next_id;
        self.next_id = id.next();
        self.outgoing
            .push_str(&kitty::transmit(id, &padded, grid, self.payload));
        self.tiles.push(CachedTile {
            key,
            id,
            bytes: padded.as_raw().len(),
        });
        self.evict();
    }

    fn evict(&mut self) {
        let protected = self.visible_keys();
        let mut total: usize = self.tiles.iter().map(|tile| tile.bytes).sum();
        let mut index = 0;
        while total > TILE_BYTE_BUDGET && index < self.tiles.len() {
            if protected.contains(&self.tiles[index].key) {
                index += 1;
                continue;
            }
            let tile = self.tiles.remove(index);
            total -= tile.bytes;
            self.outgoing.push_str(&kitty::delete(tile.id));
        }
    }

    fn forget_tiles(&mut self, forget: impl Fn(&RenderKey) -> bool) {
        let mut kept = Vec::with_capacity(self.tiles.len());
        for tile in self.tiles.drain(..) {
            if forget(&tile.key) {
                self.outgoing.push_str(&kitty::delete(tile.id));
            } else {
                kept.push(tile);
            }
        }
        self.tiles = kept;
    }

    fn forget_all_tiles(&mut self) {
        self.forget_tiles(|_| true);
    }

    fn flush(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        if self.outgoing.is_empty() {
            return Ok(());
        }
        let backend = terminal.backend_mut();
        backend.write_all(self.outgoing.as_bytes())?;
        backend.flush()?;
        self.outgoing.clear();
        Ok(())
    }

    fn visible_keys(&self) -> Vec<RenderKey> {
        let mut keys = tile_keys(self.generation, self.viewer.view());
        if let Some(shown) = &self.shown {
            keys.extend(tile_keys(shown.generation, &shown.view));
        }
        keys
    }

    fn request_tiles_at(&mut self, now: Instant) {
        let view = self.viewer.view().clone();
        if view.pane.columns == 0 || view.pane.rows == 0 {
            return;
        }
        let wanted = tile_keys(self.generation, &view);
        for key in &wanted {
            self.request(*key);
        }
        if !wanted.iter().all(|key| self.cached(*key).is_some()) || !self.may_transmit(now) {
            return;
        }
        for neighbour in [
            View {
                top: view.top.saturating_add(view.pane.rows),
                ..view.clone()
            }
            .clamped(),
            View {
                top: view.top.saturating_sub(view.pane.rows),
                ..view.clone()
            },
        ] {
            for key in tile_keys(self.generation, &neighbour) {
                self.request(key);
            }
        }
        let scale = view.layout.scale();
        let generation = self.generation;
        self.shown = Some(Shown { generation, view });
        self.forget_tiles(|key| key.scale != scale || key.generation != generation);
    }

    fn request(&mut self, key: RenderKey) {
        if self.cached(key).is_some() || self.in_flight.contains(&key) {
            return;
        }
        self.in_flight.retain(|pending| pending.scale == key.scale);
        self.in_flight.push(key);
        self.renderer.render(key);
    }

    fn cached(&self, key: RenderKey) -> Option<ImageId> {
        self.tiles
            .iter()
            .find(|tile| tile.key == key)
            .map(|tile| tile.id)
    }

    fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        let pages = page_area(area);
        let status_area = Rect {
            y: area.y + pages.height,
            height: area.height.min(1),
            ..area
        };

        if let Some(shown) = &self.shown {
            for placement in shown.view.placements() {
                let key = tile_key(shown.generation, &shown.view, placement.tile);
                let Some(id) = self.cached(key) else {
                    continue;
                };
                let area = Rect {
                    x: pages.x + placement.area.x,
                    y: pages.y + placement.area.y,
                    ..placement.area
                }
                .intersection(pages);
                frame.render_widget(
                    Placeholders {
                        id,
                        first_column: placement.first_column,
                        first_row: placement.first_row,
                    },
                    area,
                );
            }
        }

        let status = self
            .viewer
            .status_line(&self.file_name, self.keys.command_line());
        frame.render_widget(
            Line::styled(status, Style::default().add_modifier(Modifier::REVERSED)),
            status_area,
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Quit,
}

fn tile_keys(generation: Generation, view: &View) -> Vec<RenderKey> {
    view.placements()
        .into_iter()
        .map(|placement| tile_key(generation, view, placement.tile))
        .collect()
}

fn tile_key(generation: Generation, view: &View, tile: crate::layout::Tile) -> RenderKey {
    RenderKey {
        generation,
        page: tile.page,
        scale: view.layout.scale(),
        region: view.layout.tile_region(tile),
    }
}

fn pad_to_cells(image: RgbImage, cell: CellSize) -> RgbaImage {
    let width = image.width().div_ceil(cell.width) * cell.width;
    let height = image.height().div_ceil(cell.height) * cell.height;
    let mut padded = RgbaImage::new(width, height);
    image::imageops::overlay(
        &mut padded,
        &DynamicImage::ImageRgb8(image).to_rgba8(),
        0,
        0,
    );
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
    use super::*;

    const CELL: CellSize = CellSize {
        width: 10,
        height: 20,
    };

    fn headless_app(pane: Pane) -> (App, Receiver<Event>) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/three-pages.pdf");
        let (events, inbox) = mpsc::channel();
        let renderer = Renderer::spawn(path, move |response| {
            let _ = events.send(Event::Renderer(response));
        });
        renderer.load(0);
        let Ok(Event::Renderer(Response::Loaded { pages, .. })) = inbox.recv() else {
            panic!("the fixture did not load");
        };
        let app = App {
            file_name: "doc.pdf".to_owned(),
            viewer: Viewer::new(pages, CELL, pane),
            keys: KeyParser::default(),
            gestures: Gestures::default(),
            pinch: PinchGate::default(),
            renderer,
            generation: 0,
            requested_generation: 0,
            reload_at: None,
            resize_settles_at: None,
            reload_retries: 0,
            tiles: Vec::new(),
            in_flight: Vec::new(),
            shown: None,
            next_id: ImageId::first(),
            payload: Payload::Raw,
            outgoing: String::new(),
        };
        (app, inbox)
    }

    fn settle(app: &mut App, inbox: &Receiver<Event>) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            app.request_tiles_at(Instant::now() + RESIZE_SETTLE);
            if app.shown.as_ref().map(|shown| &shown.view) == Some(app.viewer.view()) {
                return;
            }
            let wait = deadline.saturating_duration_since(Instant::now());
            match inbox.recv_timeout(wait) {
                Ok(Event::Renderer(response)) => app.receive(response),
                Ok(_) => {}
                Err(_) => panic!(
                    "the shown view never caught up; in flight: {:?}",
                    app.in_flight
                ),
            }
        }
    }

    #[test]
    fn the_shown_view_follows_a_scroll_after_the_pane_is_resized() {
        let (mut app, inbox) = headless_app(Pane {
            columns: 80,
            rows: 30,
        });
        settle(&mut app, &inbox);
        app.viewer.resized(
            CELL,
            Pane {
                columns: 84,
                rows: 35,
            },
        );
        settle(&mut app, &inbox);
        app.viewer.apply(Command::Scroll {
            columns: 0,
            rows: 80,
        });
        settle(&mut app, &inbox);
    }

    #[test]
    fn a_pane_size_that_arrives_without_a_resize_event_is_picked_up() {
        let (mut app, _inbox) = headless_app(Pane {
            columns: 0,
            rows: 0,
        });
        let pane = Pane {
            columns: 84,
            rows: 34,
        };
        app.fit_to_at(pane, Instant::now());
        assert_eq!(app.viewer.view().pane, pane);
        app.request_tiles_at(Instant::now());
        assert!(!app.in_flight.is_empty());
    }

    #[test]
    fn images_wait_until_the_pane_size_has_settled() {
        let (mut app, inbox) = headless_app(Pane {
            columns: 80,
            rows: 30,
        });
        let resized_at = Instant::now();
        app.fit_to_at(
            Pane {
                columns: 84,
                rows: 34,
            },
            resized_at,
        );
        assert!(!app.may_transmit(resized_at));
        assert!(!app.may_transmit(resized_at + RESIZE_SETTLE / 2));
        assert!(app.may_transmit(resized_at + RESIZE_SETTLE));
        app.request_tiles_at(resized_at);
        let Ok(Event::Renderer(response)) = inbox.recv_timeout(Duration::from_secs(10)) else {
            panic!("no render arrived");
        };
        app.receive(response);
        app.request_tiles_at(resized_at);
        assert!(app.shown.is_none());
    }

    #[test]
    fn a_settled_pane_shows_its_tiles() {
        let (mut app, inbox) = headless_app(Pane {
            columns: 80,
            rows: 30,
        });
        let resized_at = Instant::now();
        app.fit_to_at(
            Pane {
                columns: 84,
                rows: 34,
            },
            resized_at,
        );
        let later = resized_at + RESIZE_SETTLE;
        let deadline = Instant::now() + Duration::from_secs(20);
        while app.shown.is_none() && Instant::now() < deadline {
            app.request_tiles_at(later);
            if let Ok(Event::Renderer(response)) = inbox.recv_timeout(Duration::from_millis(200)) {
                app.receive(response);
            }
        }
        assert!(app.shown.is_some());
    }

    #[test]
    fn a_pinch_zooms_the_page() {
        let (mut app, _inbox) = headless_app(Pane {
            columns: 80,
            rows: 30,
        });
        let before = app.viewer.view().layout.scale();
        app.handle(Event::Pinch(PinchInput::Scale(1.21)));
        assert!(app.viewer.view().layout.scale() > before);
    }

    #[test]
    fn a_pinch_is_ignored_while_another_pane_has_focus() {
        let (mut app, _inbox) = headless_app(Pane {
            columns: 80,
            rows: 30,
        });
        let before = app.viewer.view().layout.scale();
        app.handle(Event::Focus(false));
        app.handle(Event::Pinch(PinchInput::Scale(1.21)));
        assert_eq!(app.viewer.view().layout.scale(), before);
    }

    #[test]
    fn focus_events_are_forwarded() {
        assert!(matches!(
            translate_event(TerminalEvent::FocusLost),
            Some(Event::Focus(false))
        ));
        assert!(matches!(
            translate_event(TerminalEvent::FocusGained),
            Some(Event::Focus(true))
        ));
    }

    #[test]
    fn pointer_motion_is_forwarded_as_hover() {
        let moved = MouseEvent {
            kind: MouseEventKind::Moved,
            column: 5,
            row: 6,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            translate_mouse(moved),
            Some(MouseInput::Hover(ScreenCell { column: 5, row: 6 }))
        );
    }

    #[test]
    fn an_unchanged_pane_keeps_the_scroll_position() {
        let pane = Pane {
            columns: 80,
            rows: 30,
        };
        let (mut app, _inbox) = headless_app(pane);
        app.viewer.apply(Command::Scroll {
            columns: 0,
            rows: 7,
        });
        app.fit_to_at(pane, Instant::now());
        assert_eq!(app.viewer.view().top, 7);
    }

    #[test]
    fn tiles_at_an_old_scale_are_deleted_once_the_new_scale_is_shown() {
        let (mut app, inbox) = headless_app(Pane {
            columns: 80,
            rows: 30,
        });
        settle(&mut app, &inbox);
        let old_scale = app.viewer.view().layout.scale();
        app.outgoing.clear();
        app.viewer.apply(Command::Zoom {
            steps: 2,
            anchor: None,
        });
        settle(&mut app, &inbox);
        assert!(app.tiles.iter().all(|tile| tile.key.scale != old_scale));
        assert!(app.outgoing.contains("a=d"));
    }

    #[test]
    fn the_shown_view_follows_a_resize_that_lands_mid_render() {
        let (mut app, inbox) = headless_app(Pane {
            columns: 80,
            rows: 30,
        });
        app.request_tiles_at(Instant::now());
        app.viewer.resized(
            CELL,
            Pane {
                columns: 84,
                rows: 35,
            },
        );
        app.viewer.apply(Command::Scroll {
            columns: 0,
            rows: 80,
        });
        settle(&mut app, &inbox);
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

    #[test]
    fn the_status_row_is_kept_out_of_the_page_area() {
        assert_eq!(page_area(Rect::new(0, 0, 80, 24)), Rect::new(0, 0, 80, 23));
    }

    #[test]
    fn control_wheel_becomes_a_zoom_gesture() {
        let mouse = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 3,
            row: 4,
            modifiers: KeyModifiers::CONTROL,
        };
        assert_eq!(
            translate_mouse(mouse),
            Some(MouseInput::Wheel {
                direction: Wheel::Up,
                zoom: true,
                sideways: false,
                at: ScreenCell { column: 3, row: 4 },
            })
        );
    }

    #[test]
    fn a_left_drag_is_forwarded_and_other_buttons_are_not() {
        let drag = |button| MouseEvent {
            kind: MouseEventKind::Drag(button),
            column: 1,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            translate_mouse(drag(MouseButton::Left)),
            Some(MouseInput::Drag(ScreenCell { column: 1, row: 2 }))
        );
        assert_eq!(translate_mouse(drag(MouseButton::Right)), None);
    }

    #[test]
    fn tiles_are_compressed_when_the_terminal_can_inflate_them() {
        assert_eq!(
            compression_if(&[Capability::Kitty, Capability::KittyCompression]),
            Payload::Zlib
        );
    }

    #[test]
    fn tiles_stay_raw_when_the_terminal_did_not_confirm_compression() {
        assert_eq!(compression_if(&[Capability::Kitty]), Payload::Raw);
    }

    #[test]
    fn control_c_interrupts() {
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(translate_key(key), Some(Key::Interrupt));
    }
}
