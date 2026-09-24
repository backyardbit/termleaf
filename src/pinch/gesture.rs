use super::PinchInput;

const PINCH: i64 = 8;
const GESTURE_ENDED: i64 = 62;
const PHASE_ENDED: i64 = 4;
const PHASE_CANCELLED: i64 = 8;

pub fn decode(kind: i64, phase: i64, value: f64) -> Option<PinchInput> {
    if kind == GESTURE_ENDED {
        return Some(PinchInput::Ended);
    }
    if kind != PINCH {
        return None;
    }
    if phase == PHASE_ENDED || phase == PHASE_CANCELLED {
        return Some(PinchInput::Ended);
    }
    (value != 0.0).then_some(PinchInput::Scale(1.0 + value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pinch_gesture_carries_its_magnification() {
        assert_eq!(decode(PINCH, 2, 0.1), Some(PinchInput::Scale(1.1)));
    }

    #[test]
    fn a_pinch_in_shrinks() {
        assert_eq!(decode(PINCH, 2, -0.2), Some(PinchInput::Scale(0.8)));
    }

    #[test]
    fn other_gestures_are_ignored() {
        assert_eq!(decode(5, 2, 0.3), None);
        assert_eq!(decode(6, 2, 0.3), None);
    }

    #[test]
    fn the_end_of_a_gesture_ends_the_pinch() {
        assert_eq!(decode(GESTURE_ENDED, 0, 0.0), Some(PinchInput::Ended));
        assert_eq!(decode(PINCH, PHASE_ENDED, 0.0), Some(PinchInput::Ended));
        assert_eq!(decode(PINCH, PHASE_CANCELLED, 0.0), Some(PinchInput::Ended));
    }

    #[test]
    fn a_pinch_without_movement_changes_nothing() {
        assert_eq!(decode(PINCH, 2, 0.0), None);
    }
}
