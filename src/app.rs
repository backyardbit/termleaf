use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use crossterm::event::{
    self, DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture,
    Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use crossterm::execute;
use ratatui::DefaultTerminal;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui_image::FontSize;
use ratatui_image::picker::Capability;

use crate::editor::{self, Jumper};
use crate::encoder::{self, Encoded, Encoder, Job, Wanted};
use crate::follow::{self, Follow, Listener, Request};
use crate::graphics::{self, CellWatch, Choice, Protocol, TerminalInfo};
use crate::inverse::Inverse;
use crate::keys::{Command, Key, KeyParser, ScreenCell};
use crate::kitty::{self, CellGrid, ImageId, Payload, Placeholders, Placement};
use crate::layout::{CellSize, Pane, RenderedTile, Stretched, StretchedGrid, View};
use crate::mouse::{Gestures, MouseInput, Wheel};
use crate::pinch::{self, PinchGate, PinchInput};
use crate::raster;
use crate::renderer::{Generation, RenderKey, Renderer, Response};
use crate::search::Search;
use crate::shelf::Shelf;
use crate::viewer::Viewer;
use crate::watch::watch;

const RELOAD_SETTLE: Duration = Duration::from_millis(100);
const RELOAD_RETRY: Duration = Duration::from_millis(250);
const MAX_RELOAD_RETRIES: u32 = 3;
const TILE_BYTE_BUDGET: usize = 64 * 1024 * 1024;
const SHELF_BYTES: usize = 48 * 1024 * 1024;
const IDLE_WAIT: Duration = Duration::from_secs(3600);
const RESIZE_SETTLE: Duration = Duration::from_millis(300);
const ZOOM_SETTLE: Duration = Duration::from_millis(150);

pub struct Options {
    pub pinch: bool,
    pub follow: bool,
    pub graphics: Choice,
}

enum Event {
    Key(Key),
    Mouse(MouseInput),
    Pinch(PinchInput),
    Focus(bool),
    Resized,
    FileChanged,
    TerminalClosed,
    Renderer(Response),
    Encoded(u64, Encoded),
    Follow(Request),
}

pub fn run(path: PathBuf, options: Options) -> Result<()> {
    if !path.is_file() {
        bail!("no such file: {}", path.display());
    }
    let (events, inbox) = mpsc::channel();
    let (jobs, queued) = encoder::queue();
    let jobs = Arc::new(Mutex::new(jobs));
    let wanted = Wanted::default();
    let renderer = {
        let events = events.clone();
        Renderer::spawn(
            path.clone(),
            move |response| {
                let _ = events.send(Event::Renderer(response));
            },
            send_to(Arc::clone(&jobs), wanted.clone()),
            {
                let wanted = wanted.clone();
                move |key: &RenderKey| wanted.contains(key)
            },
        )
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
    let picker = match graphics::detect(options.graphics) {
        Ok((Protocol::Kitty, picker)) => picker,
        Ok((Protocol::Raster(protocol), picker)) => {
            drop((renderer, _watcher));
            return raster::run(&path, options, terminal, &picker, protocol);
        }
        Err(error) => {
            crate::terminal::restore(terminal);
            return Err(error);
        }
    };
    let _ = execute!(std::io::stdout(), EnableMouseCapture, EnableFocusChange);
    let listener = {
        let events = events.clone();
        Listener::spawn(
            &follow::directory(|name| std::env::var(name).ok()),
            &std::process::id().to_string(),
            move |request| {
                let _ = events.send(Event::Follow(request));
            },
        )
        .ok()
    };
    if options.pinch {
        let pinches = events.clone();
        let _ = pinch::listen(move |input| {
            let _ = pinches.send(Event::Pinch(input));
        });
    }
    let cell = cell_size(picker.font_size());
    let encoders = Encoders {
        jobs,
        wanted: wanted.clone(),
        payload: payload_for(&picker),
        events: events.clone(),
        epoch: 0,
    };
    let encoder = encoders.start(queued, cell);
    let neovims = events.clone();
    spawn_input(events);

    let pane = pane_of(page_area(terminal.get_frame().area()));
    let mut app = App {
        file_name: display_name(&path),
        inverse: Inverse::new(&path, Box::new(Jumper::default())),
        follow: Follow::new(options.follow).starting({
            let path = path.clone();
            move |gate| {
                editor::follow_editors(path, gate, move |request| {
                    let _ = neovims.send(Event::Follow(request));
                });
            }
        }),
        viewer: Viewer::new(pages, cell, pane),
        cells: CellWatch::new(crossterm::terminal::window_size().ok()),
        search: Search::default(),
        document_generation: 0,
        serial: 0,
        keys: KeyParser::default(),
        gestures: Gestures::default(),
        pinch: PinchGate::default(),
        renderer,
        encoder,
        encoders,
        wanted,
        shelf: Shelf::new(SHELF_BYTES),
        shelf_waits: false,
        requested_view: None,
        generation: 0,
        requested_generation: 0,
        reload_at: None,
        resize_settles_at: Some(Instant::now() + RESIZE_SETTLE),
        reload_retries: 0,
        tiles: Vec::new(),
        in_flight: Vec::new(),
        shown: None,
        zooming_until: None,
        stretched: Vec::new(),
        stretch_grids: std::collections::HashMap::new(),
        next_id: ImageId::first(),
        outgoing: String::new(),
    };
    let result = app.event_loop(&mut terminal, &inbox);
    drop(listener);
    app.forget_all_tiles();
    let _ = app.flush(&mut terminal);
    let _ = execute!(std::io::stdout(), DisableFocusChange, DisableMouseCapture);
    crate::terminal::restore(terminal);
    result
}

fn send_to(
    jobs: Arc<Mutex<SyncSender<Job>>>,
    wanted: Wanted,
) -> impl Fn(RenderKey, image::RgbImage) + Send + 'static {
    move |key, image| {
        if !wanted.contains(&key) {
            return;
        }
        let queue = jobs.lock().map(|queue| queue.clone());
        if let Ok(queue) = queue {
            let _ = queue.send(Job { key, image });
        }
    }
}

struct Encoders {
    jobs: Arc<Mutex<SyncSender<Job>>>,
    wanted: Wanted,
    payload: Payload,
    events: Sender<Event>,
    epoch: u64,
}

impl Encoders {
    fn start(&self, queued: Receiver<Job>, cell: CellSize) -> Encoder {
        let epoch = self.epoch;
        let events = self.events.clone();
        Encoder::spawn(
            queued,
            self.wanted.clone(),
            cell,
            self.payload,
            move |encoded| {
                let _ = events.send(Event::Encoded(epoch, encoded));
            },
        )
    }

    fn restart(&mut self, cell: CellSize) -> Encoder {
        let (jobs, queued) = encoder::queue();
        if let Ok(mut current) = self.jobs.lock() {
            *current = jobs;
        }
        self.epoch += 1;
        self.start(queued, cell)
    }
}

fn payload_for(picker: &TerminalInfo) -> Payload {
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
    let closed = events.clone();
    crate::terminal::on_hangup(move || {
        let _ = closed.send(Event::TerminalClosed);
    });
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
        MouseEventKind::Down(MouseButton::Left)
            if mouse
                .modifiers
                .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL) =>
        {
            Some(MouseInput::ModifierPress(at))
        }
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
    cells: CellWatch,
    search: Search,
    document_generation: Generation,
    serial: Generation,
    keys: KeyParser,
    gestures: Gestures,
    pinch: PinchGate,
    inverse: Inverse,
    follow: Follow,
    renderer: Renderer,
    encoder: Encoder,
    encoders: Encoders,
    wanted: Wanted,
    shelf: Shelf,
    shelf_waits: bool,
    requested_view: Option<View>,
    generation: Generation,
    requested_generation: Generation,
    reload_at: Option<Instant>,
    resize_settles_at: Option<Instant>,
    reload_retries: u32,
    tiles: Vec<CachedTile>,
    in_flight: Vec<RenderKey>,
    shown: Option<Shown>,
    zooming_until: Option<Instant>,
    stretched: Vec<(ImageId, Stretched)>,
    stretch_grids: std::collections::HashMap<ImageId, StretchedGrid>,
    next_id: ImageId,
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
            let cell = self.cells.cell(
                crossterm::terminal::window_size().ok(),
                self.viewer.view().layout.cell(),
            );
            self.fit_to_at(
                cell,
                pane_of(page_area(Rect::new(0, 0, size.width, size.height))),
                now,
            );
            self.prepare_frame(now);
            self.inverse.expire(now);
            if self.may_transmit(now) {
                self.resize_settles_at = None;
                self.flush(terminal)?;
            }
            terminal.draw(|frame| self.draw(frame))?;
            let first = match inbox.recv_timeout(self.idle_wait(Instant::now())) {
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

    fn idle_wait(&self, now: Instant) -> Duration {
        if self.shelf_waits {
            return Duration::ZERO;
        }
        [
            self.reload_at,
            self.resize_settles_at,
            self.zooming_until,
            self.inverse.notice_ends(),
        ]
        .into_iter()
        .flatten()
        .min()
        .map_or(IDLE_WAIT, |at| at.saturating_duration_since(now))
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
            Event::Focus(focused) => {
                self.pinch.focus(focused);
                self.follow.focus(focused);
            }
            Event::TerminalClosed => return Flow::Quit,
            Event::Resized => {}
            Event::FileChanged => {
                self.follow.reloading();
                self.reload_retries = 0;
                self.reload_at = Some(Instant::now() + RELOAD_SETTLE);
            }
            Event::Renderer(response) => self.receive(response),
            Event::Encoded(epoch, encoded) => self.receive_encoded(epoch, encoded),
            Event::Follow(request) => {
                self.follow
                    .request(request, &mut self.viewer, &mut self.inverse);
            }
        }
        Flow::Continue
    }

    fn fit_to_at(&mut self, cell: CellSize, pane: Pane, now: Instant) {
        let view = self.viewer.view();
        if view.pane == pane && view.layout.cell() == cell {
            return;
        }
        if view.layout.cell() != cell {
            self.encoder = self.encoders.restart(cell);
            self.refresh_highlights();
        }
        self.viewer.resized(cell, pane);
        self.resize_settles_at = Some(now + RESIZE_SETTLE);
    }

    fn may_transmit(&self, now: Instant) -> bool {
        self.resize_settles_at
            .is_none_or(|settles_at| now >= settles_at)
    }

    fn apply(&mut self, command: Command) -> Flow {
        self.apply_at(command, Instant::now())
    }

    fn apply_at(&mut self, command: Command, now: Instant) -> Flow {
        let revision = self.search.revision();
        if self.search.command(
            command,
            &mut self.keys,
            &mut self.viewer,
            &self.renderer,
            self.document_generation,
        ) {
            if self.search.revision() != revision {
                self.refresh_highlights();
            }
            return Flow::Continue;
        }
        if command == Command::Quit {
            return Flow::Quit;
        }
        if let Command::Inverse(at) = command {
            let position = self
                .viewer
                .position_under(at.or_else(|| self.gestures.pointer()));
            self.inverse.search(position, now);
            return Flow::Continue;
        }
        match command {
            Command::ToggleFollow => {
                self.follow.toggle(&mut self.viewer, &mut self.inverse);
                return Flow::Continue;
            }
            Command::SetFollow(on) => {
                self.follow.set(on, &mut self.viewer, &mut self.inverse);
                return Flow::Continue;
            }
            _ => {}
        }
        let scale = self.viewer.view().layout.scale();
        self.viewer.apply(command);
        if self.viewer.view().layout.scale() != scale {
            self.zooming_until = Some(now + ZOOM_SETTLE);
        }
        Flow::Continue
    }

    fn prepare_frame(&mut self, now: Instant) {
        self.request_tiles_at(now);
        self.stretch_shown_tiles();
    }

    fn zooming(&mut self, now: Instant) -> bool {
        if self.zooming_until.is_some_and(|until| now >= until) {
            self.zooming_until = None;
        }
        self.zooming_until.is_some()
    }

    fn stretch_shown_tiles(&mut self) {
        self.stretched.clear();
        let Some(shown) = &self.shown else {
            return;
        };
        let target = self.viewer.view();
        let from = shown.view.layout.scale();
        if from == target.layout.scale() {
            return;
        }
        let stretched: Vec<(ImageId, Stretched)> = self
            .tiles
            .iter()
            .filter(|tile| tile.key.scale == from && tile.key.generation == shown.generation)
            .filter_map(|tile| {
                let rendered = RenderedTile {
                    page: tile.key.page,
                    scale: from,
                    region: tile.key.region,
                };
                target.stretch(rendered).map(|placed| (tile.id, placed))
            })
            .collect();
        for (id, placed) in &stretched {
            if self.stretch_grids.get(id) != Some(&placed.grid) {
                self.stretch_grids.insert(*id, placed.grid);
                let grid = CellGrid {
                    columns: placed.grid.columns,
                    rows: placed.grid.rows,
                };
                self.outgoing
                    .push_str(&kitty::place(*id, Placement::Stretched, grid));
            }
        }
        self.stretched = stretched;
    }

    fn next_generation(&mut self) -> Generation {
        self.serial = self
            .serial
            .max(self.generation)
            .max(self.requested_generation)
            + 1;
        self.serial
    }

    fn refresh_highlights(&mut self) {
        self.generation = self.next_generation();
        self.renderer.highlight(
            self.document_generation,
            self.generation,
            self.search.highlights.clone(),
        );
        let shown = self.shown.as_ref().map(|shown| shown.generation);
        self.forget_tiles(|key| Some(key.generation) != shown);
        self.shelf.forget(|_| true);
        self.in_flight.clear();
        self.requested_view = None;
    }

    fn start_reload(&mut self) {
        self.reload_at = None;
        self.requested_generation = self.next_generation();
        self.renderer.load(self.requested_generation);
    }

    fn receive(&mut self, response: Response) {
        match response {
            Response::Loaded { generation, pages } if generation == self.requested_generation => {
                self.generation = generation;
                self.document_generation = generation;
                self.reload_retries = 0;
                self.viewer.reloaded(pages);
                self.search
                    .restart(&self.renderer, self.document_generation);
                self.inverse.reloaded();
                let shown = self.shown.as_ref().map(|shown| shown.generation);
                self.forget_tiles(|key| {
                    key.generation != generation && Some(key.generation) != shown
                });
                self.shelf.forget(|key| key.generation != generation);
                self.in_flight.clear();
                self.follow.reloaded(&mut self.viewer, &mut self.inverse);
            }
            Response::Unchanged { generation } if generation == self.requested_generation => {
                self.reload_retries = 0;
                self.viewer.unchanged();

                self.inverse.reloaded();
                self.follow.reloaded(&mut self.viewer, &mut self.inverse);
            }
            Response::Unreadable { generation } if generation == self.requested_generation => {
                self.viewer.unreadable();
                if self.reload_at.is_none() && self.reload_retries < MAX_RELOAD_RETRIES {
                    self.reload_retries += 1;
                    self.reload_at = Some(Instant::now() + RELOAD_RETRY);
                }
                if self.reload_at.is_none() {
                    self.follow.reloaded(&mut self.viewer, &mut self.inverse);
                }
            }
            Response::Searched { ticket, result } => {
                if self
                    .search
                    .receive(&ticket, self.document_generation, result, &mut self.viewer)
                {
                    self.refresh_highlights();
                }
            }
            Response::Loaded { .. } | Response::Unreadable { .. } | Response::Unchanged { .. } => {}
        }
    }

    fn receive_encoded(&mut self, epoch: u64, tile: Encoded) {
        if epoch == self.encoders.epoch {
            self.encoder.claimed();
        }
        self.in_flight.retain(|pending| *pending != tile.key);
        if tile.key.generation != self.generation || self.cached(tile.key).is_some() {
            return;
        }
        if self.wanted.contains(&tile.key) {
            self.store(&tile);
        } else {
            self.shelf.park(tile);
        }
    }

    fn store(&mut self, tile: &Encoded) {
        self.evict(tile.bytes);
        let id = self.next_id;
        self.next_id = id.next();
        kitty::transmit(id, &tile.image, &mut self.outgoing);
        self.tiles.push(CachedTile {
            key: tile.key,
            id,
            bytes: tile.bytes,
        });
    }

    fn evict(&mut self, incoming: usize) {
        let protected = self.visible_keys();
        let mut total = incoming + self.tiles.iter().map(|tile| tile.bytes).sum::<usize>();
        let mut index = 0;
        while total > TILE_BYTE_BUDGET && index < self.tiles.len() {
            let key = self.tiles[index].key;
            if protected.contains(&key) || self.wanted.contains(&key) {
                index += 1;
                continue;
            }
            let tile = self.tiles.remove(index);
            self.stretch_grids.remove(&tile.id);
            total -= tile.bytes;
            self.outgoing.push_str(&kitty::delete(tile.id));
        }
    }

    fn forget_tiles(&mut self, forget: impl Fn(&RenderKey) -> bool) {
        let mut kept = Vec::with_capacity(self.tiles.len());
        for tile in self.tiles.drain(..) {
            if forget(&tile.key) {
                self.stretch_grids.remove(&tile.id);
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
        self.shelf_waits = false;
        let view = self.viewer.view().clone();
        if view.pane.columns == 0 || view.pane.rows == 0 {
            return;
        }
        let stretching = self
            .shown
            .as_ref()
            .is_some_and(|shown| shown.view.layout.scale() != view.layout.scale());
        if self.zooming(now) && stretching {
            return;
        }
        let prefetch = if self.may_transmit(now) {
            self.neighbour_keys_that_fit(&view)
        } else {
            Vec::new()
        };
        let wanted = tile_keys(self.generation, &view);
        self.wanted
            .set(prefetch.iter().chain(&wanted).copied().collect());
        self.in_flight
            .retain(|key| wanted.contains(key) || prefetch.contains(key));
        let drawn = self.requested_view.as_ref() == Some(&view);
        self.requested_view = Some(view.clone());
        let view_ready = drawn && wanted.iter().all(|key| self.cached(*key).is_some());
        let mut deferred = false;
        for key in prefetch.iter().chain(&wanted) {
            let ready = if wanted.contains(key) {
                drawn
            } else {
                view_ready
            };
            if !ready && self.shelf.holds(*key) {
                deferred = true;
            } else {
                self.request(*key);
            }
        }
        self.shelf_waits =
            deferred && (!drawn || wanted.iter().all(|key| self.cached(*key).is_some()));
        if self.only_scrolled_from_shown(&view) {
            self.shown = Some(Shown {
                generation: self.generation,
                view: view.clone(),
            });
        }
        if !wanted.iter().all(|key| self.cached(*key).is_some()) || !self.may_transmit(now) {
            return;
        }
        let scale = view.layout.scale();
        let generation = self.generation;
        self.shown = Some(Shown { generation, view });
        self.forget_tiles(|key| key.scale != scale || key.generation != generation);
    }

    fn neighbour_keys(&self, view: &View) -> Vec<RenderKey> {
        let mut keys = Vec::new();
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
            keys.extend(tile_keys(self.generation, &neighbour));
        }
        keys
    }

    fn neighbour_keys_that_fit(&self, view: &View) -> Vec<RenderKey> {
        let cell = view.layout.cell();
        let mut window: Vec<RenderKey> = Vec::new();
        for key in self.visible_keys() {
            if !window.contains(&key) {
                window.push(key);
            }
        }
        let visible = window.len();
        let mut total = window.iter().fold(0, |total: usize, key| {
            total.saturating_add(tile_bytes(*key, cell))
        });
        for key in self.neighbour_keys(view) {
            if window.contains(&key) {
                continue;
            }
            total = total.saturating_add(tile_bytes(key, cell));
            if total > TILE_BYTE_BUDGET {
                break;
            }
            window.push(key);
        }
        window.split_off(visible)
    }

    fn only_scrolled_from_shown(&self, view: &View) -> bool {
        self.shown.as_ref().is_some_and(|shown| {
            shown.generation == self.generation
                && shown.view.pane == view.pane
                && shown.view.layout == view.layout
        })
    }

    fn request(&mut self, key: RenderKey) {
        if self.cached(key).is_some() || self.in_flight.contains(&key) {
            return;
        }
        if let Some(tile) = self.shelf.take(key) {
            self.store(&tile);
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

        for (id, placed) in &self.stretched {
            let area = Rect {
                x: pages.x + placed.area.x,
                y: pages.y + placed.area.y,
                ..placed.area
            }
            .intersection(pages);
            frame.render_widget(
                Placeholders {
                    id: *id,
                    placement: Placement::Stretched,
                    first_column: placed.first_column,
                    first_row: placed.first_row,
                },
                area,
            );
        }
        if let Some(shown) = self.shown.as_ref().filter(|_| self.stretched.is_empty()) {
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
                        placement: Placement::Tile,
                        first_column: placement.first_column,
                        first_row: placement.first_row,
                    },
                    area,
                );
            }
        }

        frame.render_widget(
            Line::styled(
                self.status(),
                Style::default().add_modifier(Modifier::REVERSED),
            ),
            status_area,
        );
    }

    fn status(&self) -> String {
        if let Some(query) = self.keys.search_line() {
            return format!("/{query}");
        }
        let search = self.search.status();
        let notice = self.follow.beside(self.inverse.notice());
        let notice = [notice, search]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        self.viewer.status_line(
            &self.file_name,
            self.keys.command_line(),
            (!notice.is_empty()).then_some(notice.as_str()),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Quit,
}

fn tile_bytes(key: RenderKey, cell: CellSize) -> usize {
    let width = u64::from(key.region.width.div_ceil(cell.width)) * u64::from(cell.width);
    let height = u64::from(key.region.height.div_ceil(cell.height)) * u64::from(cell.height);
    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .and_then(|bytes| usize::try_from(bytes).ok())
        .unwrap_or(usize::MAX)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverse::StatusOnly;
    use crate::layout::{TILE_COLUMNS, TILE_ROWS, Tile};

    const CELL: CellSize = CellSize {
        width: 10,
        height: 20,
    };

    fn headless_app(pane: Pane) -> (App, Receiver<Event>) {
        headless_app_for(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/three-pages.pdf"),
            pane,
        )
    }

    fn headless_app_for(path: &Path, pane: Pane) -> (App, Receiver<Event>) {
        let (events, inbox) = mpsc::channel();
        let (jobs, queued) = encoder::queue();
        let jobs = Arc::new(Mutex::new(jobs));
        let wanted = Wanted::default();
        let renderer = {
            let events = events.clone();
            Renderer::spawn(
                path.to_path_buf(),
                move |response| {
                    let _ = events.send(Event::Renderer(response));
                },
                send_to(Arc::clone(&jobs), wanted.clone()),
                {
                    let wanted = wanted.clone();
                    move |key: &RenderKey| wanted.contains(key)
                },
            )
        };
        let encoders = Encoders {
            jobs,
            wanted: wanted.clone(),
            payload: Payload::Raw,
            events,
            epoch: 0,
        };
        let encoder = encoders.start(queued, CELL);
        renderer.load(0);
        let Ok(Event::Renderer(Response::Loaded { pages, .. })) = inbox.recv() else {
            panic!("the fixture did not load");
        };
        let app = App {
            file_name: "doc.pdf".to_owned(),
            viewer: Viewer::new(pages, CELL, pane),
            cells: CellWatch::new(None),
            search: Search::default(),
            document_generation: 0,
            serial: 0,
            keys: KeyParser::default(),
            gestures: Gestures::default(),
            pinch: PinchGate::default(),
            inverse: Inverse::new(path, Box::new(StatusOnly)),
            follow: Follow::default(),
            renderer,
            encoder,
            encoders,
            wanted,
            shelf: Shelf::new(SHELF_BYTES),
            shelf_waits: false,
            requested_view: None,
            generation: 0,
            requested_generation: 0,
            reload_at: None,
            resize_settles_at: None,
            reload_retries: 0,
            tiles: Vec::new(),
            in_flight: Vec::new(),
            shown: None,
            zooming_until: None,
            stretched: Vec::new(),
            stretch_grids: std::collections::HashMap::new(),
            next_id: ImageId::first(),
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
                Ok(event) => {
                    app.handle(event);
                }
                Err(_) => panic!(
                    "the shown view never caught up; in flight: {:?}",
                    app.in_flight
                ),
            }
        }
    }

    fn type_search(app: &mut App, query: &str) {
        app.handle(Event::Key(Key::Char('/')));
        for character in query.chars() {
            app.handle(Event::Key(Key::Char(character)));
        }
        app.handle(Event::Key(Key::Enter));
    }

    fn finish_search(app: &mut App, inbox: &Receiver<Event>) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while app
            .search
            .status()
            .is_some_and(|status| status.contains("searching…"))
        {
            let event = inbox
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("search did not finish");
            app.handle(event);
        }
    }

    #[test]
    fn a_closed_terminal_exits_even_when_other_event_sources_remain_alive() {
        let (mut app, _inbox) = headless_app(Pane {
            columns: 80,
            rows: 24,
        });
        assert_eq!(app.handle(Event::TerminalClosed), Flow::Quit);
    }

    #[test]
    fn search_prompt_navigation_no_matches_and_dismissal_work_together() {
        let (mut app, inbox) = headless_app(Pane {
            columns: 80,
            rows: 24,
        });
        type_search(&mut app, "page");
        finish_search(&mut app, &inbox);
        assert!(app.status().ends_with("/page · 1/3"), "{}", app.status());
        app.handle(Event::Key(Key::Char('N')));
        assert_eq!(app.viewer.page(), 2);
        assert!(app.status().ends_with("/page · 3/3"));
        app.handle(Event::Key(Key::Char('n')));
        assert_eq!(app.viewer.page(), 0);
        type_search(&mut app, "not in this PDF café");
        finish_search(&mut app, &inbox);
        assert!(app.status().ends_with("no matches"));
        app.handle(Event::Key(Key::Escape));
        assert!(!app.status().contains("matches"));
        assert!(app.search.highlights.hits.is_empty());
    }

    #[test]
    fn cancelling_a_pending_search_rejects_its_late_result() {
        let (mut app, inbox) = headless_app(Pane {
            columns: 80,
            rows: 24,
        });
        type_search(&mut app, "three");
        let result = inbox.recv_timeout(Duration::from_secs(10)).unwrap();
        app.handle(Event::Key(Key::Escape));
        app.handle(result);
        assert_eq!(app.viewer.page(), 0);
        assert!(app.search.highlights.hits.is_empty());
        assert!(app.search.status().is_none());
    }

    #[test]
    fn a_new_search_rejects_the_previous_query_and_empty_enter_repeats() {
        let (mut app, inbox) = headless_app(Pane {
            columns: 80,
            rows: 24,
        });
        type_search(&mut app, "three");
        let previous = inbox.recv_timeout(Duration::from_secs(10)).unwrap();
        type_search(&mut app, "two");
        app.handle(previous);
        assert_eq!(app.viewer.page(), 0);
        finish_search(&mut app, &inbox);
        assert_eq!(app.viewer.page(), 1);
        assert!(app.status().ends_with("/two · 1/1"));
        app.handle(Event::Key(Key::Char('g')));
        app.handle(Event::Key(Key::Char('g')));
        type_search(&mut app, "");
        finish_search(&mut app, &inbox);
        assert_eq!(app.viewer.page(), 1);
    }

    #[test]
    fn reload_rejects_old_results_and_restarts_without_moving_the_view() {
        let (mut app, inbox) = headless_app(Pane {
            columns: 80,
            rows: 24,
        });
        type_search(&mut app, "page");
        let previous = inbox.recv_timeout(Duration::from_secs(10)).unwrap();
        let bytes = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/three-pages.pdf"),
        )
        .unwrap();
        app.viewer.apply(Command::Scroll {
            columns: 0,
            rows: 10,
        });
        let before = app.viewer.view().top;
        app.requested_generation = 7;
        app.handle(Event::Renderer(Response::Loaded {
            generation: 7,
            pages: crate::pdf::Pdf::from_bytes(&bytes).unwrap().pages(),
        }));
        app.handle(previous);
        assert!(app.search.highlights.hits.is_empty());
        let ticket = crate::search::Ticket {
            document: 7,
            query_id: 2,
            query: "page".to_owned(),
        };
        let hits = crate::pdf::Pdf::from_bytes(&bytes)
            .unwrap()
            .search(0, "page")
            .unwrap();
        app.handle(Event::Renderer(Response::Searched {
            ticket,
            result: Ok(hits),
        }));
        assert_eq!(app.viewer.view().top, before);
        assert_eq!(app.search.highlights.hits.len(), 1);
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
    fn a_scroll_is_shown_before_the_tiles_it_uncovers_have_rendered() {
        let (mut app, inbox) = headless_app(Pane {
            columns: 80,
            rows: 24,
        });
        settle(&mut app, &inbox);
        app.viewer.apply(Command::Last);
        app.request_tiles_at(Instant::now());
        let shown = app.shown.as_ref().map(|shown| &shown.view);
        assert_eq!(shown, Some(app.viewer.view()));
    }

    #[test]
    fn the_pane_above_is_prefetched_before_the_view_has_rendered() {
        let (mut app, inbox) = headless_app(Pane {
            columns: 80,
            rows: 24,
        });
        settle(&mut app, &inbox);
        app.viewer.apply(Command::Last);
        app.request_tiles_at(Instant::now() + RESIZE_SETTLE);
        let view = app.viewer.view();
        let above = View {
            top: view.top - view.pane.rows,
            ..view.clone()
        };
        for key in tile_keys(app.generation, &above) {
            assert!(
                app.in_flight.contains(&key) || app.cached(key).is_some(),
                "{key:?} was not prefetched"
            );
        }
    }

    #[test]
    fn renders_for_a_view_left_behind_are_no_longer_in_flight() {
        let (mut app, _inbox) = headless_app(Pane {
            columns: 80,
            rows: 24,
        });
        let now = Instant::now() + RESIZE_SETTLE;
        app.request_tiles_at(now);
        let first = tile_keys(app.generation, app.viewer.view());
        app.viewer.apply(Command::Last);
        app.request_tiles_at(now);
        let left_behind: Vec<RenderKey> = first
            .into_iter()
            .filter(|key| !app.wanted.contains(key))
            .collect();
        assert!(!left_behind.is_empty());
        for key in left_behind {
            assert!(!app.in_flight.contains(&key), "{key:?} is still in flight");
        }
    }

    #[test]
    fn a_render_that_is_no_longer_wanted_is_never_encoded() {
        let (app, inbox) = headless_app(Pane {
            columns: 80,
            rows: 24,
        });
        let keys = tile_keys(app.generation, app.viewer.view());
        let (stale, fresh) = (keys[0], keys[1]);
        app.wanted.set(vec![fresh]);
        app.renderer.render(stale);
        app.renderer.render(fresh);
        let mut encoded = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !encoded.contains(&fresh) && Instant::now() < deadline {
            if let Ok(Event::Encoded(_, tile)) = inbox.recv_timeout(Duration::from_millis(100)) {
                encoded.push(tile.key);
            }
        }
        while let Ok(event) = inbox.recv_timeout(Duration::from_millis(300)) {
            if let Event::Encoded(_, tile) = event {
                encoded.push(tile.key);
            }
        }
        assert!(encoded.contains(&fresh));
        assert!(!encoded.contains(&stale));
    }

    #[test]
    fn a_zoom_still_waits_for_its_tiles_before_it_is_shown() {
        let (mut app, inbox) = headless_app(Pane {
            columns: 80,
            rows: 24,
        });
        settle(&mut app, &inbox);
        app.viewer.apply(Command::Zoom {
            steps: 1,
            anchor: None,
        });
        app.request_tiles_at(Instant::now());
        let shown = app.shown.as_ref().map(|shown| &shown.view);
        assert_ne!(shown, Some(app.viewer.view()));
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
        app.fit_to_at(CELL, pane, Instant::now());
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
            CELL,
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
        let Ok(event) = inbox.recv_timeout(Duration::from_secs(10)) else {
            panic!("no render arrived");
        };
        app.handle(event);
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
            CELL,
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
            if let Ok(event) = inbox.recv_timeout(Duration::from_millis(200)) {
                app.handle(event);
            }
        }
        assert!(app.shown.is_some());
    }

    #[test]
    fn while_zooming_the_shown_tiles_are_stretched_instead_of_rendered() {
        let (mut app, inbox) = headless_app(Pane {
            columns: 80,
            rows: 30,
        });
        settle(&mut app, &inbox);
        app.outgoing.clear();
        app.in_flight.clear();
        let now = Instant::now();
        app.apply_at(
            Command::Magnify {
                per_mille: 1200,
                anchor: None,
            },
            now,
        );
        app.prepare_frame(now);
        assert!(!app.stretched.is_empty());
        assert!(app.outgoing.contains("a=p,U=1"));
        assert!(app.in_flight.is_empty());
    }

    #[test]
    fn sharp_tiles_replace_the_stretch_once_zooming_settles() {
        let (mut app, inbox) = headless_app(Pane {
            columns: 80,
            rows: 30,
        });
        settle(&mut app, &inbox);
        let now = Instant::now();
        app.apply_at(
            Command::Magnify {
                per_mille: 1200,
                anchor: None,
            },
            now,
        );
        app.prepare_frame(now);
        let later = now + ZOOM_SETTLE;
        let deadline = Instant::now() + Duration::from_secs(20);
        while !app.stretched.is_empty() && Instant::now() < deadline {
            app.prepare_frame(later);
            if let Ok(event) = inbox.recv_timeout(Duration::from_millis(200)) {
                app.handle(event);
            }
        }
        assert!(app.stretched.is_empty());
        let shown = app.shown.as_ref().unwrap().view.layout.scale();
        assert_eq!(shown, app.viewer.view().layout.scale());
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
        app.fit_to_at(CELL, pane, Instant::now());
        assert_eq!(app.viewer.view().top, 7);
    }

    #[test]
    fn after_a_new_cell_size_every_tile_in_view_is_sent_again_at_that_cell_size() {
        let pane = Pane {
            columns: 80,
            rows: 30,
        };
        let (mut app, inbox) = headless_app(pane);
        let first_tile = Tile {
            page: 0,
            column: 0,
            row: 0,
        };
        while app.viewer.view().layout.tile_region(first_tile).width >= TILE_COLUMNS * CELL.width
            || app.viewer.view().layout.tile_region(first_tile).height >= TILE_ROWS * CELL.height
        {
            app.viewer.apply(Command::Zoom {
                steps: -1,
                anchor: None,
            });
        }
        settle(&mut app, &inbox);
        app.viewer.apply(Command::Scroll {
            columns: 0,
            rows: 30,
        });
        app.request_tiles_at(Instant::now() + RESIZE_SETTLE);
        assert!(!app.in_flight.is_empty());
        app.outgoing.clear();
        let first_sent = app.next_id;
        app.fit_to_at(
            CellSize {
                width: 20,
                height: 40,
            },
            pane,
            Instant::now(),
        );
        settle(&mut app, &inbox);
        let sent: Vec<[u32; 4]> = app
            .outgoing
            .split("a=T,")
            .skip(1)
            .map(|sequence| {
                [",s=", ",v=", ",c=", ",r="].map(|field| {
                    let value = &sequence[sequence.find(field).unwrap() + field.len()..];
                    value.split(',').next().unwrap().parse().unwrap()
                })
            })
            .collect();
        assert!(!sent.is_empty());
        for [width, height, columns, rows] in sent {
            assert_eq!((width, height), (columns * 20, rows * 40));
        }
        let sent_ids: Vec<ImageId> = std::iter::successors(Some(first_sent), |id| Some(id.next()))
            .take_while(|id| *id != app.next_id)
            .collect();
        for key in tile_keys(app.generation, app.viewer.view()) {
            let id = app.cached(key).expect("every tile in view is cached");
            assert!(sent_ids.contains(&id));
        }
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

    fn encoded_tile(app: &App, key: RenderKey) -> Encoded {
        let cell = app.viewer.view().layout.cell();
        let image = image::RgbImage::new(key.region.width, key.region.height);
        encoder::encode(Job { key, image }, cell, Payload::Raw)
    }

    fn transmission(id: ImageId, tile: &Encoded) -> String {
        let mut expected = String::new();
        kitty::transmit(id, &tile.image, &mut expected);
        expected
    }

    #[test]
    fn an_encoded_tile_is_queued_for_the_terminal_before_it_can_be_placed() {
        let (mut app, _inbox) = headless_app(Pane {
            columns: 80,
            rows: 30,
        });
        let key = tile_keys(app.generation, app.viewer.view())[0];
        app.wanted.set(vec![key]);
        assert_eq!(app.cached(key), None);
        app.handle(Event::Encoded(0, encoded_tile(&app, key)));
        let id = app.cached(key).expect("the tile is cached");
        assert_eq!(app.outgoing, transmission(id, &encoded_tile(&app, key)));
    }

    #[test]
    fn a_tile_that_arrives_after_scrolling_away_is_sent_on_return_without_rendering_again() {
        let (mut app, _inbox) = headless_app(Pane {
            columns: 80,
            rows: 30,
        });
        let key = tile_keys(app.generation, app.viewer.view())[0];
        app.handle(Event::Encoded(0, encoded_tile(&app, key)));
        assert_eq!(app.cached(key), None);
        assert!(app.outgoing.is_empty());
        app.request_tiles_at(Instant::now());
        app.request_tiles_at(Instant::now());
        assert!(!app.in_flight.contains(&key));
        let id = app.cached(key).expect("the waiting tile is cached");
        assert!(
            app.outgoing
                .starts_with(&transmission(id, &encoded_tile(&app, key)))
        );
    }

    #[test]
    fn shelved_tiles_wait_for_the_view_to_be_drawn_and_prefetched_ones_for_it_to_complete() {
        let (mut app, _inbox) = headless_app(Pane {
            columns: 80,
            rows: 30,
        });
        let view = app.viewer.view().clone();
        let shown = tile_keys(app.generation, &view);
        let prefetched = app
            .neighbour_keys(&view)
            .into_iter()
            .find(|key| !shown.contains(key))
            .expect("a tile below the view");
        app.handle(Event::Encoded(0, encoded_tile(&app, shown[0])));
        app.handle(Event::Encoded(0, encoded_tile(&app, prefetched)));
        let now = Instant::now() + RESIZE_SETTLE;
        app.request_tiles_at(now);
        assert!(app.outgoing.is_empty());
        assert_eq!(app.idle_wait(now), Duration::ZERO);
        app.request_tiles_at(now);
        assert!(app.cached(shown[0]).is_some());
        assert_eq!(app.cached(prefetched), None);
        assert_ne!(app.idle_wait(now), Duration::ZERO);
        for key in &shown[1..] {
            app.handle(Event::Encoded(0, encoded_tile(&app, *key)));
        }
        app.request_tiles_at(now);
        assert!(app.cached(prefetched).is_some());
        assert!(!app.in_flight.contains(&prefetched));
    }

    #[test]
    fn a_tile_the_terminal_already_holds_is_not_transmitted_twice() {
        let (mut app, _inbox) = headless_app(Pane {
            columns: 80,
            rows: 30,
        });
        let key = tile_keys(app.generation, app.viewer.view())[0];
        app.wanted.set(vec![key]);
        app.handle(Event::Encoded(0, encoded_tile(&app, key)));
        app.outgoing.clear();
        app.handle(Event::Encoded(0, encoded_tile(&app, key)));
        app.wanted.set(Vec::new());
        app.handle(Event::Encoded(0, encoded_tile(&app, key)));
        assert!(app.outgoing.is_empty());
        assert!(!app.shelf.holds(key));
        assert_eq!(app.tiles.iter().filter(|tile| tile.key == key).count(), 1);
    }

    #[test]
    fn a_reload_clears_shelved_tiles_of_the_old_file_and_drops_its_late_ones() {
        let (mut app, _inbox) = headless_app(Pane {
            columns: 80,
            rows: 30,
        });
        let old = tile_keys(app.generation, app.viewer.view());
        app.handle(Event::Encoded(0, encoded_tile(&app, old[0])));
        let bytes = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/three-pages.pdf"),
        )
        .unwrap();
        let pages = crate::pdf::Pdf::from_bytes(&bytes).unwrap().pages();
        app.requested_generation = 1;
        app.handle(Event::Renderer(Response::Loaded {
            generation: 1,
            pages,
        }));
        assert!(!app.shelf.holds(old[0]));
        app.wanted.set(old.clone());
        app.handle(Event::Encoded(0, encoded_tile(&app, old[1])));
        assert_eq!(app.cached(old[1]), None);
        assert!(!app.shelf.holds(old[1]));
        assert!(!app.outgoing.contains("a=T"));
    }

    const HERDR_PANE_IMAGE_BYTES: usize = 64 * 1024 * 1024;

    #[derive(Default)]
    struct TerminalImages {
        held: Vec<(String, usize)>,
        most: usize,
    }

    impl TerminalImages {
        fn read(&mut self, app: &mut App) {
            for command in app.outgoing.split("\x1b_Gq=2,a=").skip(1) {
                let control = command.split([';', '\x1b']).next().unwrap_or_default();
                let field = |name: &str| {
                    control
                        .split(',')
                        .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
                        .unwrap_or_default()
                        .to_owned()
                };
                let id = field("i");
                self.held.retain(|(held, _)| *held != id);
                if control.starts_with('T') {
                    let pixels: usize =
                        field("s").parse::<usize>().unwrap() * field("v").parse::<usize>().unwrap();
                    self.held.push((id, pixels * 4));
                }
                self.most = self
                    .most
                    .max(self.held.iter().map(|(_, bytes)| bytes).sum());
            }
            app.outgoing.clear();
        }
    }

    fn settle_tiles(app: &mut App, inbox: &Receiver<Event>, terminal: &mut TerminalImages) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            app.request_tiles_at(Instant::now() + RESIZE_SETTLE);
            terminal.read(app);
            let view = app.viewer.view().clone();
            if app.in_flight.is_empty()
                && tile_keys(app.generation, &view)
                    .iter()
                    .all(|key| app.cached(*key).is_some())
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the view never settled; in flight: {:?}",
                app.in_flight
            );
            if let Ok(event) = inbox.recv_timeout(Duration::from_millis(200)) {
                app.handle(event);
            }
        }
    }

    #[test]
    fn scrolling_away_and_back_keeps_the_images_within_what_a_herdr_pane_holds() {
        let (mut app, inbox) = headless_app(Pane {
            columns: 300,
            rows: 100,
        });
        let mut terminal = TerminalImages::default();
        settle_tiles(&mut app, &inbox, &mut terminal);
        for key in ['j', 'j', 'g', 'g'] {
            app.handle(Event::Key(Key::Char(key)));
            settle_tiles(&mut app, &inbox, &mut terminal);
        }
        assert_eq!(app.viewer.page(), 0);
        assert!(
            terminal.most <= HERDR_PANE_IMAGE_BYTES,
            "the terminal was asked to hold {} MiB of images",
            terminal.most / 1024 / 1024
        );
    }

    fn thesis() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/synctex/thesis.pdf")
    }

    const THESIS_PANE: Pane = Pane {
        columns: 80,
        rows: 30,
    };

    const ON_INTRO: ScreenCell = ScreenCell {
        column: 30,
        row: 20,
    };

    #[test]
    fn capital_f_and_the_follow_commands_switch_follow_off_and_on() {
        let (mut app, _inbox) = headless_app_for(&thesis(), THESIS_PANE);
        app.handle(Event::Key(Key::Char('F')));
        assert_eq!(app.status(), "page 1/5 · doc.pdf · follow off");
        for key in ":follow".chars() {
            app.handle(Event::Key(Key::Char(key)));
        }
        app.handle(Event::Key(Key::Enter));
        assert_eq!(app.status(), "page 1/5 · doc.pdf");
    }

    #[test]
    fn a_follow_request_waits_for_a_reload_and_is_dropped_while_focused() {
        let (mut app, _inbox) = headless_app_for(&thesis(), THESIS_PANE);
        let intro = |line| {
            Event::Follow(Request {
                file: PathBuf::from("/tmp/thesis/chapters/intro.tex"),
                line,
                editor: Some("nvim"),
            })
        };
        app.handle(Event::Focus(false));
        app.handle(Event::Focus(true));
        app.handle(intro(35));
        assert_eq!(app.viewer.page(), 0);
        app.handle(Event::Focus(false));
        app.handle(Event::FileChanged);
        app.handle(intro(35));
        assert_eq!(app.viewer.page(), 0);
        app.reload_at = None;
        app.reload_retries = MAX_RELOAD_RETRIES;
        app.handle(Event::Renderer(Response::Unreadable { generation: 0 }));
        assert_eq!(app.viewer.page(), 2);
        app.handle(Event::FileChanged);
        app.handle(intro(5));
        app.handle(Event::Renderer(Response::Unchanged { generation: 0 }));
        assert_eq!(app.viewer.page(), 1);
        assert!(app.status().contains(" · follow: nvim at intro.tex:5"));
    }

    #[test]
    fn alt_click_names_the_source_under_the_pointer() {
        let (mut app, _inbox) = headless_app_for(&thesis(), THESIS_PANE);
        app.handle(Event::Key(Key::Char('j')));
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: ON_INTRO.column,
            row: ON_INTRO.row,
            modifiers: KeyModifiers::ALT,
        };
        app.handle(Event::Mouse(translate_mouse(mouse).unwrap()));
        app.handle(Event::Mouse(MouseInput::Release(ON_INTRO)));
        assert_eq!(app.status(), "page 2/5 · doc.pdf · intro.tex:4");
    }

    #[test]
    fn a_pdf_without_synctex_data_says_how_to_build_it() {
        let (mut app, _inbox) = headless_app(THESIS_PANE);
        app.handle(Event::Mouse(MouseInput::Hover(ON_INTRO)));
        app.handle(Event::Key(Key::Char('e')));
        assert_eq!(
            app.status(),
            "page 1/3 · doc.pdf · no SyncTeX data: build with -synctex=1"
        );
    }

    #[test]
    fn e_beside_the_page_says_there_is_no_source() {
        let (mut app, _inbox) = headless_app_for(&thesis(), THESIS_PANE);
        app.handle(Event::Key(Key::Char('a')));
        app.handle(Event::Mouse(MouseInput::Hover(ScreenCell {
            column: 0,
            row: 5,
        })));
        app.handle(Event::Key(Key::Char('e')));
        assert!(
            app.status().ends_with(" · no source here"),
            "{}",
            app.status()
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
    fn alt_and_control_presses_become_inverse_gestures() {
        let press = |modifiers| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 7,
            row: 9,
            modifiers,
        };
        assert_eq!(
            translate_mouse(press(KeyModifiers::ALT)),
            Some(MouseInput::ModifierPress(ScreenCell { column: 7, row: 9 }))
        );
        assert_eq!(
            translate_mouse(press(KeyModifiers::CONTROL)),
            Some(MouseInput::ModifierPress(ScreenCell { column: 7, row: 9 }))
        );
        assert_eq!(
            translate_mouse(press(KeyModifiers::NONE)),
            Some(MouseInput::Press(ScreenCell { column: 7, row: 9 }))
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
