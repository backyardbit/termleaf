use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::mpsc;
use std::thread;

use anyhow::{Result, bail};
use core_foundation::base::TCFType;
use core_foundation::mach_port::{CFMachPort, CFMachPortRef};
use core_foundation::runloop::{CFRunLoop, kCFRunLoopCommonModes};

use super::PinchInput;
use super::gesture::decode;

const GESTURE_EVENT: u32 = 29;
const GESTURE_KIND_FIELD: u32 = 110;
const GESTURE_VALUE_FIELD: u32 = 113;
const GESTURE_PHASE_FIELD: u32 = 132;
const SESSION_EVENT_TAP: u32 = 1;
const TAIL_APPEND_EVENT_TAP: u32 = 1;
const LISTEN_ONLY: u32 = 1;

type EventRef = *mut c_void;
type TapCallback = extern "C" fn(*mut c_void, u32, EventRef, *mut c_void) -> EventRef;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: TapCallback,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventGetIntegerValueField(event: EventRef, field: u32) -> i64;
    fn CGEventGetDoubleValueField(event: EventRef, field: u32) -> f64;
}

type Sink = Box<dyn Fn(PinchInput) + Send + Sync>;

static SINK: OnceLock<Sink> = OnceLock::new();

pub fn listen(send: impl Fn(PinchInput) + Clone + Send + Sync + 'static) -> Result<()> {
    // SAFETY: CGPreflightListenEventAccess takes no arguments and only reads the process's permission state.
    let allowed = unsafe { CGPreflightListenEventAccess() };
    if !allowed {
        // SAFETY: CGRequestListenEventAccess takes no arguments; it asks macOS to show the permission prompt.
        let _ = unsafe { CGRequestListenEventAccess() };
        bail!(
            "pinch needs Input Monitoring: allow your terminal in System Settings > \
             Privacy & Security > Input Monitoring, then restart the terminal"
        );
    }
    if SINK.set(Box::new(send)).is_err() {
        bail!("pinch is already listening");
    }
    let (started, outcome) = mpsc::channel();
    thread::spawn(move || run_tap(&started));
    match outcome.recv() {
        Ok(true) => Ok(()),
        _ => bail!("pinch could not watch trackpad gestures"),
    }
}

fn run_tap(started: &mpsc::Sender<bool>) {
    // SAFETY: the callback is a plain extern "C" function valid for the whole program, the mask selects gesture events only, and a listen-only tap never alters events.
    let port = unsafe {
        CGEventTapCreate(
            SESSION_EVENT_TAP,
            TAIL_APPEND_EVENT_TAP,
            LISTEN_ONLY,
            1 << GESTURE_EVENT,
            on_event,
            std::ptr::null_mut(),
        )
    };
    if port.is_null() {
        let _ = started.send(false);
        return;
    }
    // SAFETY: the port was just returned non-null by CGEventTapCreate, which hands over one reference that the wrapper now owns.
    let port = unsafe { CFMachPort::wrap_under_create_rule(port) };
    let Ok(source) = port.create_runloop_source(0) else {
        let _ = started.send(false);
        return;
    };
    // SAFETY: kCFRunLoopCommonModes is an immutable CoreFoundation constant.
    let mode = unsafe { kCFRunLoopCommonModes };
    CFRunLoop::get_current().add_source(&source, mode);
    let _ = started.send(true);
    CFRunLoop::run_current();
}

extern "C" fn on_event(
    _proxy: *mut c_void,
    event_type: u32,
    event: EventRef,
    _user_info: *mut c_void,
) -> EventRef {
    if event_type != GESTURE_EVENT || event.is_null() {
        return event;
    }
    // SAFETY: event is the non-null gesture event CoreGraphics passed to this callback and stays valid for its duration.
    let kind = unsafe { CGEventGetIntegerValueField(event, GESTURE_KIND_FIELD) };
    // SAFETY: as above; reading a field never mutates the event.
    let phase = unsafe { CGEventGetIntegerValueField(event, GESTURE_PHASE_FIELD) };
    // SAFETY: as above; reading a field never mutates the event.
    let value = unsafe { CGEventGetDoubleValueField(event, GESTURE_VALUE_FIELD) };
    if let (Some(input), Some(sink)) = (decode(kind, phase, value), SINK.get()) {
        sink(input);
    }
    event
}
