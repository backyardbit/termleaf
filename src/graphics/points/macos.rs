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
    // SAFETY: CGMainDisplayID takes no arguments, and CGDisplayCopyDisplayMode returns an owned mode or null for any display id.
    let mode = unsafe { CGDisplayCopyDisplayMode(CGMainDisplayID()) };
    if mode.is_null() {
        return None;
    }
    // SAFETY: mode is a non-null display mode owned here, read twice and released exactly once.
    let (points, pixels) = unsafe {
        let sizes = (
            CGDisplayModeGetWidth(mode),
            CGDisplayModeGetPixelWidth(mode),
        );
        CGDisplayModeRelease(mode);
        sizes
    };
    let points = u32::try_from(points).ok().filter(|points| *points > 0)?;
    let pixels = u32::try_from(pixels).ok()?;
    Some(f64::from(pixels) / f64::from(points))
}
