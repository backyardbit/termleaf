#[cfg(any(target_os = "linux", test))]
mod evdev;
#[cfg(any(target_os = "macos", test))]
mod gesture;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

use crate::keys::{Command, ScreenCell};
use crate::pdf::nearest_whole;

#[cfg(target_os = "linux")]
pub use linux::listen;
#[cfg(target_os = "macos")]
pub use macos::listen;

const PER_MILLE: f64 = 1000.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PinchInput {
    Scale(f64),
    Ended,
}

#[derive(Debug)]
pub struct PinchGate {
    focused: bool,
    pointer: Option<ScreenCell>,
}

impl Default for PinchGate {
    fn default() -> Self {
        Self {
            focused: true,
            pointer: None,
        }
    }
}

impl PinchGate {
    pub fn focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub fn pointer(&mut self, at: ScreenCell) {
        self.pointer = Some(at);
    }

    pub fn feed(&mut self, input: PinchInput) -> Option<Command> {
        let PinchInput::Scale(scale) = input else {
            return None;
        };
        if !self.focused || !scale.is_finite() || scale <= 0.0 {
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
    fn an_unfocused_pane_ignores_pinches() {
        let mut gate = PinchGate::default();
        gate.focus(false);
        assert_eq!(gate.feed(PinchInput::Scale(2.0)), None);
        gate.focus(true);
        assert!(gate.feed(PinchInput::Scale(1.1)).is_some());
    }
}
