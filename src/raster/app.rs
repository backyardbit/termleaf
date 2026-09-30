use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use image::RgbImage;
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Widget;
use ratatui::{DefaultTerminal, Frame};

use crate::follow::{Follow, Request};
use crate::inverse::{Editors, Inverse};
use crate::keys::{Command, Key, KeyParser};
use crate::layout::{CellSize, Pane, View};
use crate::mouse::{Gestures, MouseInput};
use crate::pdf::PageInfo;
use crate::pinch::{PinchGate, PinchInput};
use crate::raster::frame::tile_keys;
use crate::raster::painter::{Job, Painter, Painting};
use crate::raster::tiles::Tiles;
use crate::renderer::{Generation, RenderKey, Renderer, Response};
use crate::viewer::Viewer;

const RELOAD_SETTLE: Duration = Duration::from_millis(100);
const RELOAD_RETRY: Duration = Duration::from_millis(250);
const MAX_RELOAD_RETRIES: u32 = 3;
const TILE_BYTE_BUDGET: usize = 48 * 1024 * 1024;
const IDLE_WAIT: Duration = Duration::from_secs(3600);
pub const RESIZE_SETTLE: Duration = Duration::from_millis(300);
const ZOOM_SETTLE: Duration = Duration::from_millis(150);
pub const PARTIAL_REPAINT_INTERVAL: Duration = Duration::from_millis(100);
pub const FRAME_INTERVAL: Duration = Duration::from_millis(33);
const UNRENDERED_VIEW_WAIT: Duration = Duration::from_millis(100);
const PAGE_ORIGIN: &str = "\x1b[1;1H";

pub enum Event {
    Key(Key),
    Mouse(MouseInput),
    Pinch(PinchInput),
    Focus(bool),
    Resized,
    FileChanged,
    Renderer(Response),
    Tile(RenderKey, RgbImage),
    Painted(Painting),
    Follow(Request),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
}

#[derive(Debug, Clone, PartialEq)]
struct Submitted {
    view: View,
    generation: Generation,
    present: Vec<RenderKey>,
}

struct OnScreen {
    view: View,
    generation: Generation,
}

pub struct App {
    file_name: String,
    inverse: Inverse,
    follow: Follow,
    viewer: Viewer,
    keys: KeyParser,
    gestures: Gestures,
    pinch: PinchGate,
    renderer: Renderer,
    painter: Painter,
    tiles: Tiles,
    in_flight: Vec<RenderKey>,
    generation: Generation,
    requested_generation: Generation,
    reload_at: Option<Instant>,
    reload_retries: u32,
    resize_settles_at: Option<Instant>,
    zooming_until: Option<Instant>,
    submitted: Option<Submitted>,
    partial_repaint_interval: Duration,
    partial_painted_at: Option<Instant>,
    frame_interval: Duration,
    painted_at: Option<Instant>,
    unrendered_view: Option<(View, Instant)>,
    paint_due: Option<Instant>,
    next_job: u64,
    first_valid_job: u64,
    written_job: Option<u64>,
    on_screen: Option<OnScreen>,
    outgoing: String,
}

pub struct Parts {
    pub follow: bool,
    pub file_name: String,
    pub path: PathBuf,
    pub editors: Box<dyn Editors>,
    pub pages: Vec<PageInfo>,
    pub cell: CellSize,
    pub pane: Pane,
    pub renderer: Renderer,
    pub painter: Painter,
    pub partial_repaint_interval: Duration,
    pub frame_interval: Duration,
}

impl App {
    pub fn new(parts: Parts) -> Self {
        Self {
            file_name: parts.file_name,
            inverse: Inverse::new(&parts.path, parts.editors),
            follow: Follow::new(parts.follow),
            viewer: Viewer::new(parts.pages, parts.cell, parts.pane),
            keys: KeyParser::default(),
            gestures: Gestures::default(),
            pinch: PinchGate::default(),
            renderer: parts.renderer,
            painter: parts.painter,
            tiles: Tiles::new(TILE_BYTE_BUDGET),
            in_flight: Vec::new(),
            generation: 0,
            requested_generation: 0,
            reload_at: None,
            reload_retries: 0,
            resize_settles_at: None,
            zooming_until: None,
            submitted: None,
            partial_repaint_interval: parts.partial_repaint_interval,
            partial_painted_at: None,
            frame_interval: parts.frame_interval,
            painted_at: None,
            unrendered_view: None,
            paint_due: None,
            next_job: 0,
            first_valid_job: 0,
            written_job: None,
            on_screen: None,
            outgoing: String::new(),
        }
    }

    pub fn event_loop(
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
            self.prepare_frame(now);
            self.inverse.expire(now);
            self.flush(terminal)?;
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
        [
            self.reload_at,
            self.resize_settles_at,
            self.zooming_until,
            self.paint_due,
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
                    return self.apply_at(command, Instant::now());
                }
            }
            Event::Mouse(input) => {
                self.pinch.pointer(input.at());
                if let Some(command) = self.gestures.feed(input, Instant::now()) {
                    return self.apply_at(command, Instant::now());
                }
            }
            Event::Pinch(input) => {
                if let Some(command) = self.pinch.feed(input) {
                    return self.apply_at(command, Instant::now());
                }
            }
            Event::Focus(focused) => {
                self.pinch.focus(focused);
                self.follow.focus(focused);
            }
            Event::Resized => {}
            Event::FileChanged => {
                self.follow.reloading();
                self.reload_retries = 0;
                self.reload_at = Some(Instant::now() + RELOAD_SETTLE);
            }
            Event::Renderer(response) => self.receive(response),
            Event::Tile(key, image) => self.receive_tile(key, image),
            Event::Painted(painting) => self.receive_painting(painting),
            Event::Follow(request) => {
                self.follow
                    .request(request, &mut self.viewer, &mut self.inverse);
            }
        }
        Flow::Continue
    }

    fn fit_to_at(&mut self, pane: Pane, now: Instant) {
        if self.viewer.view().pane != pane {
            let cell = self.viewer.view().layout.cell();
            self.viewer.resized(cell, pane);
            self.resize_settles_at = Some(now + RESIZE_SETTLE);
            self.on_screen = None;
            self.submitted = None;
            self.first_valid_job = self.next_job;
        }
    }

    fn may_paint(&mut self, now: Instant) -> bool {
        if self.resize_settles_at.is_some_and(|at| now >= at) {
            self.resize_settles_at = None;
        }
        self.resize_settles_at.is_none()
    }

    fn apply_at(&mut self, command: Command, now: Instant) -> Flow {
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

    fn zooming(&mut self, now: Instant) -> bool {
        if self.zooming_until.is_some_and(|until| now >= until) {
            self.zooming_until = None;
        }
        self.zooming_until.is_some()
    }

    fn prepare_frame(&mut self, now: Instant) {
        self.request_tiles_at(now);
        self.paint_at(now);
    }

    fn request_tiles_at(&mut self, now: Instant) {
        let view = self.viewer.view().clone();
        if view.pane.columns == 0 || view.pane.rows == 0 {
            return;
        }
        let rescaling = self
            .on_screen
            .as_ref()
            .is_some_and(|shown| shown.view.layout.scale() != view.layout.scale());
        if self.zooming(now) && rescaling {
            return;
        }
        let visible = tile_keys(self.generation, &view);
        let prefetch = if self.may_paint(now) {
            self.window(&view).split_off(visible.len())
        } else {
            Vec::new()
        };
        let window: Vec<RenderKey> = prefetch.into_iter().chain(visible).collect();
        self.request(&window);
    }

    fn paint_at(&mut self, now: Instant) {
        self.paint_due = None;
        let view = self.viewer.view().clone();
        if view.pane.columns == 0 || view.pane.rows == 0 || !self.may_paint(now) {
            return;
        }
        let wanted = tile_keys(self.generation, &view);
        let present: Vec<RenderKey> = wanted
            .iter()
            .copied()
            .filter(|key| self.tiles.contains(*key))
            .collect();
        let complete = present.len() == wanted.len();
        if !complete
            && present.is_empty()
            && !self
                .unrendered_view
                .as_ref()
                .is_some_and(|(seen, _)| *seen == view)
        {
            self.unrendered_view = Some((view.clone(), now));
        }
        if !complete && (present.is_empty() || !self.only_scrolled_from_screen(&view)) {
            return;
        }
        let submission = Submitted {
            view,
            generation: self.generation,
            present,
        };
        if self.submitted.as_ref() == Some(&submission) {
            return;
        }
        let partial_due = (!complete && self.filling_in(&submission))
            .then(|| {
                self.partial_painted_at
                    .map(|at| at + self.partial_repaint_interval)
            })
            .flatten();
        let frame_due = self.painted_at.map(|at| at + self.frame_interval);
        let unrendered_due = self
            .unrendered_view
            .as_ref()
            .filter(|(seen, _)| !complete && *seen == submission.view)
            .map(|(_, at)| *at + UNRENDERED_VIEW_WAIT);
        let held_back = partial_due
            .into_iter()
            .chain(frame_due)
            .chain(unrendered_due)
            .max()
            .filter(|due| now < *due);
        if let Some(due) = held_back {
            self.paint_due = Some(due);
            return;
        }
        self.partial_painted_at = (!complete).then_some(now);
        self.painted_at = Some(now);
        let id = self.next_job;
        self.next_job += 1;
        self.painter.submit(Job {
            id,
            view: submission.view.clone(),
            generation: submission.generation,
            tiles: self.tiles.visible(&submission.present),
        });
        self.submitted = Some(submission);
    }

    fn filling_in(&self, submission: &Submitted) -> bool {
        self.submitted.as_ref().is_some_and(|submitted| {
            submitted.view == submission.view && submitted.generation == submission.generation
        })
    }

    fn only_scrolled_from_screen(&self, view: &View) -> bool {
        self.on_screen.as_ref().is_some_and(|shown| {
            shown.generation == self.generation
                && shown.view.pane == view.pane
                && shown.view.layout == view.layout
        })
    }

    fn window(&self, view: &View) -> Vec<RenderKey> {
        self.tiles.window(
            &tile_keys(self.generation, view),
            &self.neighbour_keys(view),
        )
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

    fn request(&mut self, keys: &[RenderKey]) {
        let mut batch = Vec::new();
        for key in keys {
            if self.tiles.contains(*key) || self.in_flight.contains(key) {
                continue;
            }
            self.in_flight.retain(|pending| pending.scale == key.scale);
            self.in_flight.push(*key);
            batch.push(*key);
        }
        self.renderer.render_all(&batch);
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
                self.inverse.reloaded();
                self.tiles.forget(|key| key.generation != generation);
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
            Response::Loaded { .. } | Response::Unreadable { .. } | Response::Unchanged { .. } => {}
        }
    }

    fn receive_tile(&mut self, key: RenderKey, image: RgbImage) {
        self.in_flight.retain(|pending| *pending != key);
        if key.generation != self.generation {
            return;
        }
        let window = self.window(self.viewer.view());
        self.tiles.insert(key, image, &window);
    }

    fn receive_painting(&mut self, painting: Painting) {
        let stale = painting.id < self.first_valid_job
            || self
                .written_job
                .is_some_and(|written| painting.id <= written)
            || painting.view.pane != self.viewer.view().pane;
        if stale {
            return;
        }
        self.outgoing.push_str(PAGE_ORIGIN);
        self.outgoing.push_str(&painting.bytes);
        self.written_job = Some(painting.id);
        self.on_screen = Some(OnScreen {
            view: painting.view,
            generation: painting.generation,
        });
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

    fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        let pages = page_area(area);
        frame.render_widget(PaintedElsewhere, pages);
        frame.render_widget(
            Line::styled(
                self.status(),
                Style::default().add_modifier(Modifier::REVERSED),
            ),
            Rect {
                y: area.y + pages.height,
                height: area.height.min(1),
                ..area
            },
        );
    }

    fn status(&self) -> String {
        let notice = self.follow.beside(self.inverse.notice());
        self.viewer
            .status_line(&self.file_name, self.keys.command_line(), notice.as_deref())
    }
}

struct PaintedElsewhere;

impl Widget for PaintedElsewhere {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buffer[(x, y)].set_diff_option(CellDiffOption::Skip);
            }
        }
    }
}

pub fn page_area(area: Rect) -> Rect {
    Rect {
        height: area.height.saturating_sub(1),
        ..area
    }
}

pub fn pane_of(area: Rect) -> Pane {
    Pane {
        columns: u32::from(area.width),
        rows: u32::from(area.height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverse::StatusOnly;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::raster::sixel;

    const CELL: CellSize = CellSize {
        width: 10,
        height: 20,
    };
    const PANE: Pane = Pane {
        columns: 80,
        rows: 24,
    };

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn scratch_copy_of(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let directory = std::env::temp_dir().join(format!("termleaf-raster-{nanos}"));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("doc.pdf");
        std::fs::copy(fixture(name), &path).unwrap();
        path
    }

    fn headless_app(path: PathBuf, pane: Pane) -> (App, Receiver<Event>) {
        headless_app_with(path, pane, PARTIAL_REPAINT_INTERVAL, Duration::ZERO)
    }

    fn headless_app_with(
        path: PathBuf,
        pane: Pane,
        partial_repaint_interval: Duration,
        frame_interval: Duration,
    ) -> (App, Receiver<Event>) {
        let (events, inbox) = mpsc::channel();
        let renderer = {
            let responses = events.clone();
            let tiles = events.clone();
            Renderer::spawn(
                path.clone(),
                move |response| {
                    let _ = responses.send(Event::Renderer(response));
                },
                move |key, image| {
                    let _ = tiles.send(Event::Tile(key, image));
                },
            )
        };
        let painter = Painter::spawn(Box::new(|frame, _| sixel::encode(frame)), move |painting| {
            let _ = events.send(Event::Painted(painting));
        });
        renderer.load(0);
        let Ok(Event::Renderer(Response::Loaded { pages, .. })) = inbox.recv() else {
            panic!("the fixture did not load");
        };
        let app = App::new(Parts {
            follow: true,
            file_name: "doc.pdf".to_owned(),
            path,
            editors: Box::new(StatusOnly),
            pages,
            cell: CELL,
            pane,
            renderer,
            painter,
            partial_repaint_interval,
            frame_interval,
        });
        (app, inbox)
    }

    fn settled() -> Instant {
        Instant::now() + RESIZE_SETTLE
    }

    fn wanted(app: &App) -> Vec<RenderKey> {
        tile_keys(app.generation, app.viewer.view())
    }

    fn fully_painted(app: &App) -> bool {
        let on_screen = app
            .on_screen
            .as_ref()
            .is_some_and(|shown| &shown.view == app.viewer.view());
        let complete = app
            .submitted
            .as_ref()
            .is_some_and(|submitted| submitted.present == wanted(app));
        let written = app.next_job.checked_sub(1) == app.written_job;
        on_screen && complete && written
    }

    fn settle(app: &mut App, inbox: &Receiver<Event>) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            app.prepare_frame(settled());
            if fully_painted(app) {
                return;
            }
            let wait = deadline.saturating_duration_since(Instant::now());
            match inbox.recv_timeout(wait) {
                Ok(event) => {
                    app.handle(event);
                }
                Err(_) => panic!("the view was never painted; in flight: {:?}", app.in_flight),
            }
        }
    }

    fn frames_written(app: &App) -> usize {
        app.outgoing.matches(&format!("{PAGE_ORIGIN}\x1bP")).count()
    }

    fn next_painting(app: &mut App, inbox: &Receiver<Event>) -> Painting {
        loop {
            match inbox.recv_timeout(Duration::from_secs(20)) {
                Ok(Event::Painted(painting)) => return painting,
                Ok(event) => {
                    app.handle(event);
                }
                Err(_) => panic!("no frame was painted"),
            }
        }
    }

    #[test]
    fn the_first_frame_waits_until_every_visible_tile_has_rendered() {
        let (mut app, inbox) = headless_app(fixture("three-pages.pdf"), PANE);
        app.prepare_frame(settled());
        assert!(app.submitted.is_none());
        settle(&mut app, &inbox);
        assert_eq!(frames_written(&app), 1);
    }

    #[test]
    fn a_frame_is_written_at_the_page_origin_as_one_sixel_image() {
        let (mut app, inbox) = headless_app(fixture("three-pages.pdf"), PANE);
        settle(&mut app, &inbox);
        assert!(
            app.outgoing
                .starts_with("\x1b[1;1H\x1bP9;1;0q\"1;1;800;480")
        );
        assert!(app.outgoing.ends_with("\x1b\\"));
    }

    #[test]
    fn a_scroll_over_rendered_tiles_writes_one_frame() {
        let (mut app, inbox) = headless_app(fixture("three-pages.pdf"), PANE);
        settle(&mut app, &inbox);
        app.outgoing.clear();
        app.apply_at(
            Command::Scroll {
                columns: 0,
                rows: 1,
            },
            Instant::now(),
        );
        settle(&mut app, &inbox);
        assert_eq!(frames_written(&app), 1);
    }

    #[test]
    fn a_burst_of_scrolls_writes_one_frame() {
        let (mut app, inbox) = headless_app(fixture("three-pages.pdf"), PANE);
        settle(&mut app, &inbox);
        app.outgoing.clear();
        for _ in 0..10 {
            app.apply_at(
                Command::Scroll {
                    columns: 0,
                    rows: 1,
                },
                Instant::now(),
            );
        }
        settle(&mut app, &inbox);
        assert_eq!(frames_written(&app), 1);
    }

    fn scroll_past_the_prefetch(app: &mut App) {
        app.apply_at(
            Command::Scroll {
                columns: 0,
                rows: PANE.rows.cast_signed() * 3 / 2,
            },
            Instant::now(),
        );
    }

    #[test]
    fn a_scroll_is_painted_at_once_and_filled_in_when_its_tiles_arrive() {
        let (mut app, inbox) = headless_app(fixture("three-pages.pdf"), PANE);
        settle(&mut app, &inbox);
        app.outgoing.clear();
        scroll_past_the_prefetch(&mut app);
        app.prepare_frame(settled());
        let first = app.submitted.clone().unwrap();
        assert_eq!(&first.view, app.viewer.view());
        assert!(first.present.len() < wanted(&app).len());
        settle(&mut app, &inbox);
        assert!(frames_written(&app) >= 1);
    }

    #[test]
    fn a_zoom_keeps_the_old_frame_until_the_new_scale_has_rendered() {
        let (mut app, inbox) = headless_app(fixture("three-pages.pdf"), PANE);
        settle(&mut app, &inbox);
        let before = app.viewer.view().layout.scale();
        app.outgoing.clear();
        let now = Instant::now();
        app.apply_at(
            Command::Zoom {
                steps: 1,
                anchor: None,
            },
            now,
        );
        app.prepare_frame(now);
        assert!(app.in_flight.is_empty());
        assert_eq!(
            app.on_screen
                .as_ref()
                .map(|shown| shown.view.layout.scale()),
            Some(before)
        );
        settle(&mut app, &inbox);
        assert_eq!(frames_written(&app), 1);
        assert_ne!(
            app.on_screen
                .as_ref()
                .map(|shown| shown.view.layout.scale()),
            Some(before)
        );
    }

    #[test]
    fn a_reload_is_painted_once_the_new_document_has_rendered() {
        let path = scratch_copy_of("three-pages.pdf");
        let (mut app, inbox) = headless_app(path.clone(), PANE);
        settle(&mut app, &inbox);
        app.outgoing.clear();
        std::fs::copy(fixture("five-pages.pdf"), &path).unwrap();
        app.start_reload();
        while app.generation == 0 {
            let event = inbox.recv_timeout(Duration::from_secs(20)).unwrap();
            app.handle(event);
        }
        settle(&mut app, &inbox);
        assert_eq!(frames_written(&app), 1);
        assert_eq!(app.viewer.page_count(), 5);
        assert_eq!(
            app.on_screen.as_ref().map(|shown| shown.generation),
            Some(1)
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_resize_repaints_at_the_new_size_once_it_settles() {
        let (mut app, inbox) = headless_app(fixture("three-pages.pdf"), PANE);
        settle(&mut app, &inbox);
        app.outgoing.clear();
        let resized_at = Instant::now();
        app.fit_to_at(
            Pane {
                columns: 84,
                rows: 34,
            },
            resized_at,
        );
        app.prepare_frame(resized_at);
        assert!(app.submitted.is_none());
        settle(&mut app, &inbox);
        assert_eq!(frames_written(&app), 1);
        assert!(app.outgoing.contains("\"1;1;840;680"));
    }

    #[test]
    fn a_frame_painted_for_the_old_pane_size_is_never_written() {
        let (mut app, inbox) = headless_app(fixture("three-pages.pdf"), PANE);
        settle(&mut app, &inbox);
        app.outgoing.clear();
        app.apply_at(
            Command::Scroll {
                columns: 0,
                rows: 1,
            },
            Instant::now(),
        );
        app.prepare_frame(settled());
        let painting = next_painting(&mut app, &inbox);
        app.fit_to_at(
            Pane {
                columns: 84,
                rows: 34,
            },
            Instant::now(),
        );
        app.handle(Event::Painted(painting));
        assert!(app.outgoing.is_empty());
    }

    #[test]
    fn capital_f_and_the_follow_commands_switch_follow_off_and_on() {
        let (mut app, _inbox) = headless_app(fixture("synctex/thesis.pdf"), PANE);
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
        let (mut app, _inbox) = headless_app(fixture("synctex/thesis.pdf"), PANE);
        let intro = |line| {
            Event::Follow(Request {
                file: PathBuf::from("/tmp/thesis/chapters/intro.tex"),
                line,
            })
        };
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
        assert!(app.status().contains(" · follow: intro.tex:5"));
    }

    #[test]
    fn a_modifier_click_names_the_source_until_the_notice_expires() {
        let (mut app, _inbox) = headless_app(fixture("synctex/thesis.pdf"), PANE);
        app.handle(Event::Key(Key::Char('j')));
        let on_intro = crate::keys::ScreenCell {
            column: 30,
            row: 20,
        };
        app.handle(Event::Mouse(MouseInput::ModifierPress(on_intro)));
        app.handle(Event::Mouse(MouseInput::Release(on_intro)));
        assert_eq!(app.status(), "page 2/5 · doc.pdf · intro.tex:4");
        let now = Instant::now();
        assert!(app.idle_wait(now) <= Duration::from_secs(4));
        app.inverse.expire(now + Duration::from_secs(4));
        assert_eq!(app.status(), "page 2/5 · doc.pdf");
    }

    #[test]
    fn the_status_row_is_drawn_as_text_and_the_pages_are_left_to_the_frame() {
        let (app, _inbox) = headless_app(fixture("three-pages.pdf"), PANE);
        let mut terminal = Terminal::new(TestBackend::new(80, 25)).unwrap();
        let drawn = terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = drawn.buffer;
        assert_eq!(buffer[(0, 0)].diff_option, CellDiffOption::Skip);
        assert_eq!(buffer[(79, 23)].diff_option, CellDiffOption::Skip);
        assert_eq!(buffer[(0, 24)].diff_option, CellDiffOption::None);
        let status: String = (0..80).map(|x| buffer[(x, 24)].symbol()).collect();
        assert!(status.starts_with("page 1/3 · doc.pdf"), "{status}");
    }

    #[test]
    fn prefetched_tiles_are_not_rendered_again_when_the_cache_is_over_budget() {
        let (mut app, inbox) = headless_app(
            fixture("three-pages.pdf"),
            Pane {
                columns: 80,
                rows: 30,
            },
        );
        app.tiles = Tiles::new(1);
        settle(&mut app, &inbox);
        let deadline = Instant::now() + Duration::from_secs(20);
        while !app.in_flight.is_empty() {
            let wait = deadline.saturating_duration_since(Instant::now());
            let event = inbox.recv_timeout(wait).unwrap();
            app.handle(event);
            app.prepare_frame(settled());
        }
        app.prepare_frame(settled());
        assert!(app.in_flight.is_empty());
    }

    fn settle_counting_renders(app: &mut App, inbox: &Receiver<Event>, budget: usize) -> usize {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut renders = 0;
        loop {
            app.prepare_frame(settled());
            if fully_painted(app) && app.in_flight.is_empty() {
                return renders;
            }
            let wait = deadline.saturating_duration_since(Instant::now());
            match inbox.recv_timeout(wait) {
                Ok(event) => {
                    if matches!(event, Event::Tile(..)) {
                        renders += 1;
                    }
                    app.handle(event);
                    assert!(app.tiles.bytes() <= budget);
                }
                Err(_) => panic!("renders never stopped; in flight: {:?}", app.in_flight),
            }
        }
    }

    #[test]
    fn scrolls_zooms_and_jumps_render_each_window_at_most_once_within_the_budget() {
        let (mut app, inbox) = headless_app(fixture("five-pages.pdf"), PANE);
        let budget = 4 * 640 * 960 * 3;
        app.tiles = Tiles::new(budget);
        let scroll = Command::Scroll {
            columns: 0,
            rows: 10,
        };
        let zoom_in = Command::Zoom {
            steps: 1,
            anchor: None,
        };
        let zoom_out = Command::Zoom {
            steps: -1,
            anchor: None,
        };
        let steps = [
            None,
            Some(scroll),
            Some(scroll),
            Some(scroll),
            Some(scroll),
            Some(zoom_in),
            Some(zoom_in),
            Some(zoom_in),
            Some(scroll),
            Some(zoom_out),
            Some(Command::Last),
            Some(Command::First),
            Some(Command::GoTo(3)),
        ];
        for step in steps {
            if let Some(command) = step {
                app.apply_at(command, Instant::now());
            }
            let renders = settle_counting_renders(&mut app, &inbox, budget);
            let window = app.window(app.viewer.view()).len();
            assert!(
                renders <= window,
                "{step:?} rendered {renders} tiles for a window of {window}"
            );
            app.prepare_frame(settled());
            assert!(app.in_flight.is_empty(), "{step:?} kept rendering");
        }
    }

    fn all_visible_rendered(app: &App) -> bool {
        wanted(app).iter().all(|key| app.tiles.contains(*key))
    }

    fn next_tile(app: &mut App, inbox: &Receiver<Event>) {
        loop {
            let event = inbox.recv_timeout(Duration::from_secs(20)).unwrap();
            let tile = matches!(event, Event::Tile(..));
            app.handle(event);
            if tile {
                return;
            }
        }
    }

    #[test]
    fn a_jump_to_unrendered_pages_is_painted_once_when_its_tiles_arrive_quickly() {
        let (mut app, inbox) = headless_app(fixture("five-pages.pdf"), PANE);
        settle(&mut app, &inbox);
        let shown = app.next_job;
        app.apply_at(Command::Last, Instant::now());
        assert!(wanted(&app).iter().all(|key| !app.tiles.contains(*key)));
        let start = settled();
        app.prepare_frame(start);
        assert_eq!(app.next_job, shown);
        while !all_visible_rendered(&app) {
            next_tile(&mut app, &inbox);
            app.prepare_frame(start);
        }
        assert_eq!(app.next_job, shown + 1);
        assert!(
            app.submitted
                .as_ref()
                .is_some_and(|submitted| submitted.present == wanted(&app))
        );
    }

    #[test]
    fn a_jump_whose_tiles_are_slow_is_painted_partially_after_a_wait() {
        let (mut app, inbox) = headless_app(fixture("five-pages.pdf"), PANE);
        settle(&mut app, &inbox);
        let shown = app.next_job;
        app.apply_at(Command::Last, Instant::now());
        let start = settled();
        app.prepare_frame(start);
        next_tile(&mut app, &inbox);
        assert!(!all_visible_rendered(&app));
        app.prepare_frame(start);
        assert_eq!(app.next_job, shown);
        assert!(app.idle_wait(start) <= UNRENDERED_VIEW_WAIT);
        app.prepare_frame(start + UNRENDERED_VIEW_WAIT);
        assert_eq!(app.next_job, shown + 1);
        assert!(
            app.submitted
                .as_ref()
                .is_some_and(|submitted| !submitted.present.is_empty())
        );
    }

    #[test]
    fn tiles_arriving_soon_after_a_partial_frame_are_painted_together() {
        let (mut app, inbox) = headless_app(fixture("three-pages.pdf"), PANE);
        settle(&mut app, &inbox);
        scroll_past_the_prefetch(&mut app);
        let start = settled();
        app.prepare_frame(start);
        let missing = wanted(&app).len() - app.submitted.as_ref().unwrap().present.len();
        assert!(missing >= 2, "only {missing} tiles were missing");
        let partial = app.next_job;
        let soon = start + PARTIAL_REPAINT_INTERVAL / 2;
        while !all_visible_rendered(&app) {
            next_tile(&mut app, &inbox);
            app.prepare_frame(soon);
            if !all_visible_rendered(&app) {
                assert_eq!(app.next_job, partial);
                assert!(app.idle_wait(soon) <= PARTIAL_REPAINT_INTERVAL);
            }
        }
        assert_eq!(app.next_job, partial + 1);
    }

    #[test]
    fn without_an_interval_each_tile_repaints_the_partial_frame() {
        let (mut app, inbox) = headless_app_with(
            fixture("three-pages.pdf"),
            PANE,
            Duration::ZERO,
            Duration::ZERO,
        );
        settle(&mut app, &inbox);
        scroll_past_the_prefetch(&mut app);
        let start = settled();
        app.prepare_frame(start);
        let partial = app.next_job;
        next_tile(&mut app, &inbox);
        assert!(!all_visible_rendered(&app));
        app.prepare_frame(start);
        assert_eq!(app.next_job, partial + 1);
    }

    #[test]
    fn a_partial_frame_is_repainted_once_the_interval_has_passed() {
        let (mut app, inbox) = headless_app(fixture("three-pages.pdf"), PANE);
        settle(&mut app, &inbox);
        scroll_past_the_prefetch(&mut app);
        let start = settled();
        app.prepare_frame(start);
        let partial = app.next_job;
        next_tile(&mut app, &inbox);
        assert!(!all_visible_rendered(&app));
        app.prepare_frame(start + PARTIAL_REPAINT_INTERVAL);
        assert_eq!(app.next_job, partial + 1);
    }

    fn paced_app(frame_interval: Duration) -> (App, Receiver<Event>) {
        headless_app_with(
            fixture("three-pages.pdf"),
            PANE,
            PARTIAL_REPAINT_INTERVAL,
            frame_interval,
        )
    }

    fn scroll_one_row(app: &mut App) {
        app.apply_at(
            Command::Scroll {
                columns: 0,
                rows: 1,
            },
            Instant::now(),
        );
    }

    #[test]
    fn scrolls_within_the_frame_interval_are_painted_together() {
        let (mut app, inbox) = paced_app(FRAME_INTERVAL);
        settle(&mut app, &inbox);
        let start = settled() + FRAME_INTERVAL;
        scroll_one_row(&mut app);
        app.prepare_frame(start);
        let painted = app.next_job;
        scroll_one_row(&mut app);
        let soon = start + FRAME_INTERVAL / 2;
        app.prepare_frame(soon);
        assert_eq!(app.next_job, painted);
        assert!(app.idle_wait(soon) <= FRAME_INTERVAL / 2);
        app.prepare_frame(start + FRAME_INTERVAL);
        assert_eq!(app.next_job, painted + 1);
    }

    #[test]
    fn without_a_frame_interval_each_scroll_is_painted_at_once() {
        let (mut app, inbox) = paced_app(Duration::ZERO);
        settle(&mut app, &inbox);
        let start = settled();
        scroll_one_row(&mut app);
        app.prepare_frame(start);
        let painted = app.next_job;
        scroll_one_row(&mut app);
        app.prepare_frame(start);
        assert_eq!(app.next_job, painted + 1);
    }
}
