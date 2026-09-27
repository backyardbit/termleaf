use std::sync::mpsc::Sender;
use std::thread;

use crossterm::event::{
    self, Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind,
};

use crate::keys::{Key, ScreenCell};
use crate::mouse::{MouseInput, Wheel};
use crate::raster::app::Event;

pub fn spawn_input(events: Sender<Event>) {
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
