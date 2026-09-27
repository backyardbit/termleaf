mod app;
mod frame;
mod input;
mod iterm2;
mod painter;
mod sixel;
mod tiles;

use std::path::Path;
use std::sync::mpsc;

use anyhow::{Context, Result, bail};
use crossterm::event::{
    DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture,
};
use crossterm::execute;
use ratatui::DefaultTerminal;
use ratatui_image::picker::Picker;

use crate::app::Options;
use crate::graphics::Raster;
use crate::layout::CellSize;
use crate::pinch;
use crate::raster::app::{App, Event, Parts, page_area, pane_of};
use crate::raster::painter::{Encode, Painter};
use crate::renderer::{Renderer, Response};
use crate::watch::watch;

pub fn run(
    path: &Path,
    options: Options,
    mut terminal: DefaultTerminal,
    picker: &Picker,
    raster: Raster,
) -> Result<()> {
    let result = show(path, &options, &mut terminal, picker, raster);
    let _ = execute!(std::io::stdout(), DisableFocusChange, DisableMouseCapture);
    ratatui::restore();
    result
}

fn encoder_for(raster: Raster) -> Encode {
    match raster {
        Raster::Sixel => Box::new(|frame, _| sixel::encode(frame)),
        Raster::Iterm2 => Box::new(iterm2::encode),
    }
}

fn outside_tmux(raster: Raster, tmux: bool) -> Result<()> {
    if tmux {
        bail!("{raster} images inside tmux are not supported yet; run termleaf outside tmux");
    }
    Ok(())
}

fn show(
    path: &Path,
    options: &Options,
    terminal: &mut DefaultTerminal,
    picker: &Picker,
    raster: Raster,
) -> Result<()> {
    let encode = encoder_for(raster);
    outside_tmux(raster, picker.tmux_detected())?;
    let (events, inbox) = mpsc::channel();
    let renderer = {
        let responses = events.clone();
        let tiles = events.clone();
        Renderer::spawn(
            path.to_path_buf(),
            move |response| {
                let _ = responses.send(Event::Renderer(response));
            },
            move |key, image| {
                let _ = tiles.send(Event::Tile(key, image));
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
        watch(path, move || {
            let _ = events.send(Event::FileChanged);
        })?
    };
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    if options.pinch {
        let _ = execute!(std::io::stdout(), EnableFocusChange);
        let pinches = events.clone();
        let _ = pinch::listen(move |input| {
            let _ = pinches.send(Event::Pinch(input));
        });
    }
    let painter = {
        let events = events.clone();
        Painter::spawn(encode, move |painting| {
            let _ = events.send(Event::Painted(painting));
        })
    };
    input::spawn_input(events);
    let font = picker.font_size();
    let mut app = App::new(Parts {
        file_name: path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        ),
        pages,
        cell: CellSize {
            width: u32::from(font.width.max(1)),
            height: u32::from(font.height.max(1)),
        },
        pane: pane_of(page_area(terminal.get_frame().area())),
        renderer,
        painter,
    });
    app.event_loop(terminal, &inbox)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_raster_protocol_writes_its_own_escape_sequence() {
        let frame = image::RgbImage::new(20, 20);
        let pane = crate::layout::Pane {
            columns: 2,
            rows: 1,
        };
        assert!(
            encoder_for(Raster::Sixel)(&frame, pane)
                .unwrap()
                .starts_with("\x1bP")
        );
        assert!(
            encoder_for(Raster::Iterm2)(&frame, pane)
                .unwrap()
                .starts_with("\x1b]1337;File=")
        );
    }

    #[test]
    fn raster_images_refuse_to_start_inside_tmux() {
        assert!(outside_tmux(Raster::Sixel, false).is_ok());
        assert_eq!(
            outside_tmux(Raster::Sixel, true).unwrap_err().to_string(),
            "Sixel images inside tmux are not supported yet; run termleaf outside tmux"
        );
    }
}
