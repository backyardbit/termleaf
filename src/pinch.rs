#[cfg(any(target_os = "linux", test))]
mod evdev;
#[cfg(any(target_os = "macos", test))]
mod gesture;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

use crate::keys::{Command, ScreenCell};

#[cfg(target_os = "linux")]
pub use linux::listen;
#[cfg(target_os = "macos")]
pub use macos::listen;

const STEP_FACTOR: f64 = 1.1;
const STEP_TOLERANCE: f64 = 1e-9;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PinchInput {
    Scale(f64),
    Ended,
}

#[derive(Debug, Default)]
pub struct PinchSteps {
    pending: f64,
}

impl PinchSteps {
    pub fn feed(&mut self, input: PinchInput) -> i32 {
        match input {
            PinchInput::Ended => {
                self.pending = 0.0;
                0
            }
            PinchInput::Scale(scale) if scale > 0.0 && scale.is_finite() => {
                self.pending += scale.ln() / STEP_FACTOR.ln();
                let steps = (self.pending + self.pending.signum() * STEP_TOLERANCE).trunc();
                self.pending -= steps;
                whole_steps(steps)
            }
            PinchInput::Scale(_) => 0,
        }
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the value is truncated and clamped into i32's range first"
)]
fn whole_steps(steps: f64) -> i32 {
    steps
        .trunc()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

#[derive(Debug)]
pub struct PinchGate {
    focused: bool,
    pointer: Option<ScreenCell>,
    steps: PinchSteps,
}

impl Default for PinchGate {
    fn default() -> Self {
        Self {
            focused: true,
            pointer: None,
            steps: PinchSteps::default(),
        }
    }
}

impl PinchGate {
    pub fn focus(&mut self, focused: bool) {
        self.focused = focused;
        self.steps.feed(PinchInput::Ended);
    }

    pub fn pointer(&mut self, at: ScreenCell) {
        self.pointer = Some(at);
    }

    pub fn feed(&mut self, input: PinchInput) -> Option<Command> {
        if !self.focused {
            return None;
        }
        let steps = self.steps.feed(input);
        (steps != 0).then_some(Command::Zoom {
            steps,
            anchor: self.pointer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pinch_out_by_one_step_zooms_in_one_step() {
        let mut steps = PinchSteps::default();
        assert_eq!(steps.feed(PinchInput::Scale(1.1)), 1);
    }

    #[test]
    fn small_pinches_add_up_to_a_step() {
        let mut steps = PinchSteps::default();
        let small = 1.1_f64.sqrt();
        assert_eq!(steps.feed(PinchInput::Scale(small)), 0);
        assert_eq!(steps.feed(PinchInput::Scale(small * 1.001)), 1);
    }

    #[test]
    fn a_pinch_in_zooms_out() {
        let mut steps = PinchSteps::default();
        assert_eq!(steps.feed(PinchInput::Scale(1.0 / 1.21)), -2);
    }

    #[test]
    fn the_end_of_a_pinch_drops_the_leftover() {
        let mut steps = PinchSteps::default();
        steps.feed(PinchInput::Scale(1.08));
        steps.feed(PinchInput::Ended);
        assert_eq!(steps.feed(PinchInput::Scale(1.05)), 0);
    }

    #[test]
    fn nonsense_scales_are_ignored() {
        let mut steps = PinchSteps::default();
        assert_eq!(steps.feed(PinchInput::Scale(0.0)), 0);
        assert_eq!(steps.feed(PinchInput::Scale(f64::NAN)), 0);
        assert_eq!(steps.feed(PinchInput::Scale(-2.0)), 0);
    }

    #[test]
    fn the_gate_zooms_at_the_last_pointer_position() {
        let mut gate = PinchGate::default();
        let at = ScreenCell { column: 7, row: 3 };
        gate.pointer(at);
        assert_eq!(
            gate.feed(PinchInput::Scale(1.1)),
            Some(Command::Zoom {
                steps: 1,
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
