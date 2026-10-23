use std::ffi::c_void;

type DisplayMode = *mut c_void;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGMainDisplayID() -> u32;
    fn CGDisplayCopyDisplayMode(display: u32) -> DisplayMode;
    fn CGDisplayModeGetWidth(mode: DisplayMode) -> usize;
    fn CGDisplayModeGetPixelWidth(mode: DisplayMode) -> usize;
    fn CGDisplayModeRelease(mode: DisplayMode);
}

pub fn main_display_scale() -> Option<f64> {
    // SAFETY: CGMainDisplayID takes no arguments and only reads which display is the main one.
    let display = unsafe { CGMainDisplayID() };
    // SAFETY: CGDisplayCopyDisplayMode accepts any display id and returns an owned mode or null.
    let mode = unsafe { CGDisplayCopyDisplayMode(display) };
    if mode.is_null() {
        return None;
    }
    // SAFETY: mode is a non-null display mode owned here and not yet released.
    let points = unsafe { CGDisplayModeGetWidth(mode) };
    // SAFETY: mode is a non-null display mode owned here and not yet released.
    let pixels = unsafe { CGDisplayModeGetPixelWidth(mode) };
    // SAFETY: mode was returned by a Copy function, so it is owned here and released exactly once.
    unsafe { CGDisplayModeRelease(mode) };
    let points = u32::try_from(points).ok().filter(|points| *points > 0)?;
    let pixels = u32::try_from(pixels).ok()?;
    Some(f64::from(pixels) / f64::from(points))
}
