#[cfg(any(target_os = "linux", test))]
mod evdev;
#[cfg(any(target_os = "macos", test))]
mod gesture;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::keys::{Command, ScreenCell};
use crate::pdf::nearest_whole;

#[cfg(target_os = "linux")]
pub use linux::listen;
#[cfg(target_os = "macos")]
pub use macos::listen;

const PER_MILLE: f64 = 1000.0;
const CHECK_TIME: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    #[cfg_attr(
        not(any(target_os = "macos", test)),
        expect(
            dead_code,
            reason = "only the macOS tap can keep pinches from the terminal"
        )
    )]
    KeptFromTerminal,
    SharedWithTerminal,
}

pub fn report(outcome: &Result<Reach>) -> String {
    match outcome {
        Ok(Reach::KeptFromTerminal) => {
            "pinch is on and kept from the terminal, so only termleaf zooms".to_owned()
        }
        Ok(Reach::SharedWithTerminal) => "pinch is on, but the terminal gets every pinch too: \
             on macOS, allow your terminal app in System Settings > Privacy & Security > \
             Accessibility, then restart the terminal"
            .to_owned(),
        Err(error) => format!("pinch is off: {error:#}"),
    }
}

pub fn seen(input: PinchInput, reach: Reach) -> String {
    match (input, reach) {
        (PinchInput::Ended, _) => "pinch ended".to_owned(),
        (PinchInput::Scale(scale), Reach::KeptFromTerminal) => {
            format!("pinch ×{scale:.3} · kept from the terminal")
        }
        (PinchInput::Scale(scale), Reach::SharedWithTerminal) => {
            format!("pinch ×{scale:.3} · also reached the terminal")
        }
    }
}

pub fn check() {
    let (pinches, inbox) = mpsc::channel();
    let outcome = listen(
        move |input| {
            let _ = pinches.send(input);
        },
        PaneFocus::default(),
    );
    println!("{}", report(&outcome));
    let Ok(reach) = outcome else {
        return;
    };
    println!(
        "pinch over this window for the next {} seconds (Ctrl-C stops)",
        CHECK_TIME.as_secs()
    );
    let deadline = Instant::now() + CHECK_TIME;
    while let Ok(input) = inbox.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        println!("{}", seen(input, reach));
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PinchInput {
    Scale(f64),
    Ended,
}

#[derive(Debug, Clone)]
pub struct PaneFocus(Arc<AtomicBool>);

impl Default for PaneFocus {
    fn default() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }
}

impl PaneFocus {
    pub fn has(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    fn set(&self, focused: bool) {
        self.0.store(focused, Ordering::Relaxed);
    }
}

#[derive(Debug, Default)]
pub struct PinchGate {
    focus: PaneFocus,
    pointer: Option<ScreenCell>,
}

impl PinchGate {
    pub fn new(focus: PaneFocus) -> Self {
        Self {
            focus,
            pointer: None,
        }
    }

    pub fn focus(&mut self, focused: bool) {
        self.focus.set(focused);
    }

    pub fn pointer(&mut self, at: ScreenCell) {
        self.pointer = Some(at);
    }

    pub fn feed(&mut self, input: PinchInput) -> Option<Command> {
        let PinchInput::Scale(scale) = input else {
            return None;
        };
        if !self.focus.has() || !scale.is_finite() || scale <= 0.0 {
            return None;
        }
        let per_mille = nearest_whole(scale * PER_MILLE);
        (per_mille != 1000).then_some(Command::Magnify {
            per_mille,
            anchor: self.pointer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_check_says_when_pinches_are_kept_from_the_terminal() {
        assert_eq!(
            report(&Ok(Reach::KeptFromTerminal)),
            "pinch is on and kept from the terminal, so only termleaf zooms"
        );
    }

    #[test]
    fn the_check_says_how_to_keep_pinches_from_the_terminal() {
        let line = report(&Ok(Reach::SharedWithTerminal));
        assert!(line.starts_with("pinch is on, but the terminal gets every pinch too"));
        assert!(line.contains("Accessibility"), "{line}");
    }

    #[test]
    fn the_check_says_why_pinch_is_off() {
        assert_eq!(
            report(&Err(anyhow::anyhow!("pinch found no touchpad"))),
            "pinch is off: pinch found no touchpad"
        );
    }

    #[test]
    fn the_check_names_each_pinch_and_where_it_went() {
        assert_eq!(
            seen(PinchInput::Scale(1.05), Reach::KeptFromTerminal),
            "pinch ×1.050 · kept from the terminal"
        );
        assert_eq!(
            seen(PinchInput::Scale(0.9), Reach::SharedWithTerminal),
            "pinch ×0.900 · also reached the terminal"
        );
        assert_eq!(
            seen(PinchInput::Ended, Reach::KeptFromTerminal),
            "pinch ended"
        );
    }

    #[test]
    fn a_pinch_magnifies_by_exactly_its_own_scale() {
        let mut gate = PinchGate::default();
        assert_eq!(
            gate.feed(PinchInput::Scale(1.03)),
            Some(Command::Magnify {
                per_mille: 1030,
                anchor: None
            })
        );
        assert_eq!(
            gate.feed(PinchInput::Scale(0.98)),
            Some(Command::Magnify {
                per_mille: 980,
                anchor: None
            })
        );
    }

    #[test]
    fn a_pinch_too_small_to_matter_changes_nothing() {
        let mut gate = PinchGate::default();
        assert_eq!(gate.feed(PinchInput::Scale(1.0001)), None);
        assert_eq!(gate.feed(PinchInput::Ended), None);
    }

    #[test]
    fn nonsense_scales_are_ignored() {
        let mut gate = PinchGate::default();
        assert_eq!(gate.feed(PinchInput::Scale(0.0)), None);
        assert_eq!(gate.feed(PinchInput::Scale(f64::NAN)), None);
        assert_eq!(gate.feed(PinchInput::Scale(-2.0)), None);
    }

    #[test]
    fn the_gate_zooms_at_the_last_pointer_position() {
        let mut gate = PinchGate::default();
        let at = ScreenCell { column: 7, row: 3 };
        gate.pointer(at);
        assert_eq!(
            gate.feed(PinchInput::Scale(1.1)),
            Some(Command::Magnify {
                per_mille: 1100,
                anchor: Some(at)
            })
        );
    }

    #[test]
    fn the_os_listener_sees_the_focus_the_gate_was_told() {
        let focus = PaneFocus::default();
        let mut gate = PinchGate::new(focus.clone());
        assert!(focus.has());
        gate.focus(false);
        assert!(!focus.has());
        gate.focus(true);
        assert!(focus.has());
    }

    #[test]
    fn an_unfocused_pane_ignores_pinches() {
        let mut gate = PinchGate::default();
        gate.focus(false);
        assert_eq!(gate.feed(PinchInput::Scale(2.0)), None);
        gate.focus(true);
        assert!(gate.feed(PinchInput::Scale(1.1)).is_some());
    }
}
