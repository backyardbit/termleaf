//! Prototype (flag `TERMLEAF_RESIDENT=<density>`, e.g. 0.75): render tiles at a lower pixel density and let
//! the terminal stretch them onto the full-density cell grid, so more pages fit in the image budget and can
//! be warmed ahead of a held paging key. Off unless the variable is set.

use std::sync::OnceLock;

pub fn density() -> Option<f64> {
    static DENSITY: OnceLock<Option<f64>> = OnceLock::new();
    *DENSITY.get_or_init(|| {
        let value: f64 = std::env::var("TERMLEAF_RESIDENT").ok()?.parse().ok()?;
        (value > 0.1 && value < 1.0).then_some(value)
    })
}

#[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixel sizes are small and positive")]
pub fn shrink(value: u32, density: f64) -> u32 {
    (f64::from(value) * density).floor() as u32
}

#[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixel sizes are small and positive")]
pub fn shrink_up(value: u32, density: f64) -> u32 {
    (f64::from(value) * density).ceil().max(1.0) as u32
}
