#[cfg(target_os = "macos")]
mod macos;

use ratatui_image::FontSize;

const REPORTS_POINTS: [&str; 1] = ["tern"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelsPerPoint(u16);

impl PixelsPerPoint {
    pub const ONE: Self = Self(1);

    pub fn detect() -> Self {
        Self::for_terminal(std::env::var("TERM_PROGRAM").ok().as_deref(), display_scale)
    }

    fn for_terminal(program: Option<&str>, display: impl FnOnce() -> Option<f64>) -> Self {
        let reports_points = program.is_some_and(|program| {
            REPORTS_POINTS
                .iter()
                .any(|name| program.eq_ignore_ascii_case(name))
        });
        if !reports_points {
            return Self::ONE;
        }
        display()
            .map(f64::round)
            .and_then(|scale| {
                [2, 3, 4]
                    .into_iter()
                    .find(|whole| f64::from(*whole) == scale)
            })
            .map_or(Self::ONE, Self)
    }

    pub fn cell(self, (width, height): (u16, u16)) -> (u16, u16) {
        (width.saturating_mul(self.0), height.saturating_mul(self.0))
    }

    pub fn font(self, font: FontSize) -> FontSize {
        let (width, height) = self.cell((font.width, font.height));
        FontSize::new(width, height)
    }
}

#[cfg(target_os = "macos")]
fn display_scale() -> Option<f64> {
    macos::main_display_scale()
}

#[cfg(not(target_os = "macos"))]
fn display_scale() -> Option<f64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_that_reports_points_has_its_cells_scaled_to_the_display() {
        let scale = PixelsPerPoint::for_terminal(Some("tern"), || Some(2.0));
        assert_eq!(scale.cell((12, 24)), (24, 48));
        let font = scale.font(FontSize::new(12, 24));
        assert_eq!((font.width, font.height), (24, 48));
    }

    #[test]
    fn terminals_that_report_pixels_keep_their_cells_on_a_retina_display() {
        for program in [Some("ghostty"), Some("iTerm.app"), Some("WezTerm"), None] {
            let scale = PixelsPerPoint::for_terminal(program, || Some(2.0));
            assert_eq!(scale, PixelsPerPoint::ONE, "{program:?}");
        }
    }

    #[test]
    fn an_unknown_or_odd_display_scale_keeps_the_reported_cells() {
        for display in [None, Some(0.0), Some(f64::NAN), Some(9.0)] {
            let scale = PixelsPerPoint::for_terminal(Some("tern"), || display);
            assert_eq!(scale, PixelsPerPoint::ONE, "{display:?}");
        }
        assert_eq!(
            PixelsPerPoint::for_terminal(Some("Tern"), || Some(1.0)),
            PixelsPerPoint::ONE
        );
    }
}
