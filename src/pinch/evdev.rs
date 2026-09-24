use super::PinchInput;

pub const RECORD_BYTES: usize = if cfg!(target_pointer_width = "64") {
    24
} else {
    16
};
const MAX_SLOTS: usize = 16;
const EV_SYN: u16 = 0x00;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0x00;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const INPUT_PROP_POINTER: u32 = 0x00;
const INPUT_PROP_DIRECT: u32 = 0x01;
const SCROLL_DOMINANCE: f64 = 1.5;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Point {
    x: f64,
    y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Fingers {
    first: Point,
    second: Point,
}

impl Fingers {
    fn spread(self) -> f64 {
        (self.first.x - self.second.x).hypot(self.first.y - self.second.y)
    }

    fn centre(self) -> Point {
        Point {
            x: (self.first.x + self.second.x) / 2.0,
            y: (self.first.y + self.second.y) / 2.0,
        }
    }
}

fn pinch_between(before: Fingers, after: Fingers) -> Option<f64> {
    let (spread_before, spread_after) = (before.spread(), after.spread());
    if spread_before <= 0.0 || spread_after <= 0.0 {
        return None;
    }
    let (centre_before, centre_after) = (before.centre(), after.centre());
    let glide = (centre_after.x - centre_before.x).hypot(centre_after.y - centre_before.y);
    let stretch = (spread_after - spread_before).abs();
    (stretch > 0.0 && stretch * SCROLL_DOMINANCE >= glide).then(|| spread_after / spread_before)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputEvent {
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

pub fn parse_record(record: &[u8]) -> Option<InputEvent> {
    let header = RECORD_BYTES - 8;
    let field = |at: usize, width: usize| record.get(header + at..header + at + width);
    Some(InputEvent {
        kind: u16::from_ne_bytes(field(0, 2)?.try_into().ok()?),
        code: u16::from_ne_bytes(field(2, 2)?.try_into().ok()?),
        value: i32::from_ne_bytes(field(4, 4)?.try_into().ok()?),
    })
}

pub fn is_touchpad(abs_capabilities: &str, properties: &str) -> bool {
    has_bit(abs_capabilities, u32::from(ABS_MT_POSITION_X))
        && has_bit(properties, INPUT_PROP_POINTER)
        && !has_bit(properties, INPUT_PROP_DIRECT)
}

fn has_bit(bitmask: &str, bit: u32) -> bool {
    let word = usize::try_from(bit / 64).unwrap_or(usize::MAX);
    bitmask
        .split_whitespace()
        .rev()
        .nth(word)
        .and_then(|text| u64::from_str_radix(text, 16).ok())
        .is_some_and(|value| value & (1 << (bit % 64)) != 0)
}

#[derive(Debug, Clone, Copy, Default)]
struct Contact {
    x: Option<f64>,
    y: Option<f64>,
}

impl Contact {
    fn point(self) -> Option<Point> {
        Some(Point {
            x: self.x?,
            y: self.y?,
        })
    }
}

#[derive(Debug, Default)]
pub struct Touches {
    slot: usize,
    contacts: [Option<Contact>; MAX_SLOTS],
    previous: Option<Fingers>,
}

impl Touches {
    pub fn feed(&mut self, event: InputEvent) -> Option<PinchInput> {
        match (event.kind, event.code) {
            (EV_ABS, ABS_MT_SLOT) => {
                self.slot = usize::try_from(event.value).unwrap_or(usize::MAX);
                None
            }
            (EV_ABS, ABS_MT_TRACKING_ID) => {
                let contact = (event.value >= 0).then(Contact::default);
                if let Some(slot) = self.contacts.get_mut(self.slot) {
                    *slot = contact;
                }
                None
            }
            (EV_ABS, ABS_MT_POSITION_X | ABS_MT_POSITION_Y) => {
                if let Some(Some(contact)) = self.contacts.get_mut(self.slot) {
                    let value = Some(f64::from(event.value));
                    if event.code == ABS_MT_POSITION_X {
                        contact.x = value;
                    } else {
                        contact.y = value;
                    }
                }
                None
            }
            (EV_SYN, SYN_REPORT) => self.frame(),
            _ => None,
        }
    }

    fn frame(&mut self) -> Option<PinchInput> {
        let points: Vec<Point> = self
            .contacts
            .iter()
            .flatten()
            .filter_map(|contact| contact.point())
            .collect();
        let current = match points.as_slice() {
            [first, second] => Some(Fingers {
                first: *first,
                second: *second,
            }),
            _ => None,
        };
        let previous = std::mem::replace(&mut self.previous, current);
        match (previous, current) {
            (Some(before), Some(after)) => pinch_between(before, after).map(PinchInput::Scale),
            (Some(_), None) => Some(PinchInput::Ended),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fingers(first: (f64, f64), second: (f64, f64)) -> Fingers {
        Fingers {
            first: Point {
                x: first.0,
                y: first.1,
            },
            second: Point {
                x: second.0,
                y: second.1,
            },
        }
    }

    fn record(kind: u16, code: u16, value: i32) -> Vec<u8> {
        let mut bytes = vec![0; RECORD_BYTES - 8];
        bytes.extend(kind.to_ne_bytes());
        bytes.extend(code.to_ne_bytes());
        bytes.extend(value.to_ne_bytes());
        bytes
    }

    fn abs(code: u16, value: i32) -> InputEvent {
        InputEvent {
            kind: EV_ABS,
            code,
            value,
        }
    }

    fn report() -> InputEvent {
        InputEvent {
            kind: EV_SYN,
            code: SYN_REPORT,
            value: 0,
        }
    }

    fn touch(touches: &mut Touches, slot: i32, x: i32, y: i32) {
        touches.feed(abs(ABS_MT_SLOT, slot));
        touches.feed(abs(ABS_MT_POSITION_X, x));
        touches.feed(abs(ABS_MT_POSITION_Y, y));
    }

    fn press(touches: &mut Touches, slot: i32, x: i32, y: i32) {
        touches.feed(abs(ABS_MT_SLOT, slot));
        touches.feed(abs(ABS_MT_TRACKING_ID, slot + 10));
        touch(touches, slot, x, y);
    }

    #[test]
    fn parses_a_kernel_input_record() {
        assert_eq!(
            parse_record(&record(EV_ABS, ABS_MT_POSITION_X, -7)),
            Some(abs(ABS_MT_POSITION_X, -7))
        );
    }

    #[test]
    fn a_short_record_is_not_an_event() {
        assert_eq!(parse_record(&[0; 10]), None);
    }

    #[test]
    fn a_pointer_device_with_multitouch_positions_is_a_touchpad() {
        let abs_capabilities = "660800011000003";
        assert!(has_bit(abs_capabilities, 0x35));
        assert!(is_touchpad(abs_capabilities, "5"));
    }

    #[test]
    fn a_touchscreen_is_not_a_touchpad() {
        assert!(!is_touchpad("660800011000003", "2"));
    }

    #[test]
    fn a_keyboard_is_not_a_touchpad() {
        assert!(!is_touchpad("0", "0"));
    }

    #[test]
    fn capability_words_are_read_from_the_right() {
        assert!(has_bit("1 0", 64));
        assert!(!has_bit("1 0", 0));
    }

    #[test]
    fn two_fingers_spreading_apart_report_a_pinch_out() {
        let mut touches = Touches::default();
        press(&mut touches, 0, 100, 100);
        press(&mut touches, 1, 200, 100);
        assert_eq!(touches.feed(report()), None);
        touch(&mut touches, 0, 90, 100);
        touch(&mut touches, 1, 210, 100);
        let Some(PinchInput::Scale(scale)) = touches.feed(report()) else {
            panic!("expected a pinch");
        };
        assert!((scale - 1.2).abs() < 1e-9);
    }

    #[test]
    fn lifting_a_finger_ends_the_pinch() {
        let mut touches = Touches::default();
        press(&mut touches, 0, 100, 100);
        press(&mut touches, 1, 200, 100);
        touches.feed(report());
        touches.feed(abs(ABS_MT_SLOT, 1));
        touches.feed(abs(ABS_MT_TRACKING_ID, -1));
        assert_eq!(touches.feed(report()), Some(PinchInput::Ended));
    }

    #[test]
    fn one_finger_never_pinches() {
        let mut touches = Touches::default();
        press(&mut touches, 0, 100, 100);
        touches.feed(report());
        touch(&mut touches, 0, 150, 100);
        assert_eq!(touches.feed(report()), None);
    }

    #[test]
    fn a_slot_beyond_the_table_is_ignored() {
        let mut touches = Touches::default();
        press(&mut touches, 40, 100, 100);
        assert_eq!(touches.feed(report()), None);
    }

    #[test]
    fn spreading_two_fingers_is_a_pinch_out() {
        let scale = pinch_between(
            fingers((100.0, 100.0), (200.0, 100.0)),
            fingers((90.0, 100.0), (210.0, 100.0)),
        );
        assert!((scale.unwrap() - 1.2).abs() < 1e-9);
    }

    #[test]
    fn two_fingers_moving_together_are_a_scroll_not_a_pinch() {
        let scale = pinch_between(
            fingers((100.0, 100.0), (200.0, 100.0)),
            fingers((100.0, 130.0), (201.0, 130.0)),
        );
        assert_eq!(scale, None);
    }

    #[test]
    fn fingers_on_the_same_spot_are_not_a_pinch() {
        let scale = pinch_between(
            fingers((100.0, 100.0), (100.0, 100.0)),
            fingers((100.0, 100.0), (120.0, 100.0)),
        );
        assert_eq!(scale, None);
    }
}
