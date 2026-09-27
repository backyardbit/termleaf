#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the raster loop that draws frames is not built yet"
    )
)]
mod frame;

use anyhow::{Result, bail};

use crate::graphics::{NEEDS_KITTY, Raster};

pub fn run(raster: Raster) -> Result<()> {
    bail!("{raster} images are not supported yet; {NEEDS_KITTY}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixel_and_iterm2_say_they_are_not_supported_yet() {
        assert_eq!(
            run(Raster::Sixel).unwrap_err().to_string(),
            "Sixel images are not supported yet; termleaf needs a terminal that supports \
             the Kitty graphics protocol (for example Kitty or Ghostty, optionally inside herdr)"
        );
        assert_eq!(
            run(Raster::Iterm2).unwrap_err().to_string(),
            "iTerm2 images are not supported yet; termleaf needs a terminal that supports \
             the Kitty graphics protocol (for example Kitty or Ghostty, optionally inside herdr)"
        );
    }
}
