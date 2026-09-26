use std::fmt;

use anyhow::{Context, Result, bail};
use ratatui_image::picker::cap_parser::QueryStdioOptions;
use ratatui_image::picker::{Picker, ProtocolType};

pub const NEEDS_KITTY: &str = "termleaf needs a terminal that supports the Kitty graphics protocol \
     (for example Kitty or Ghostty, optionally inside herdr)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Kitty,
    Raster(Raster),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Raster {
    Sixel,
    Iterm2,
}

impl fmt::Display for Raster {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Sixel => "Sixel",
            Self::Iterm2 => "iTerm2",
        })
    }
}

pub fn detect() -> Result<(Protocol, Picker)> {
    let options = QueryStdioOptions {
        kitty_compression: true,
        ..QueryStdioOptions::default()
    };
    let picker = Picker::from_query_stdio_with_options(options).context("querying the terminal")?;
    let protocol = decide(picker.protocol_type())?;
    Ok((protocol, picker))
}

fn decide(found: ProtocolType) -> Result<Protocol> {
    match found {
        ProtocolType::Kitty => Ok(Protocol::Kitty),
        ProtocolType::Sixel => Ok(Protocol::Raster(Raster::Sixel)),
        ProtocolType::Iterm2 => Ok(Protocol::Raster(Raster::Iterm2)),
        ProtocolType::Halfblocks => bail!(NEEDS_KITTY),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIXEL: Protocol = Protocol::Raster(Raster::Sixel);
    const ITERM2: Protocol = Protocol::Raster(Raster::Iterm2);

    #[test]
    fn a_kitty_terminal_keeps_the_kitty_path() {
        assert_eq!(decide(ProtocolType::Kitty).unwrap(), Protocol::Kitty);
    }

    #[test]
    fn sixel_and_iterm2_terminals_take_the_raster_path() {
        assert_eq!(decide(ProtocolType::Sixel).unwrap(), SIXEL);
        assert_eq!(decide(ProtocolType::Iterm2).unwrap(), ITERM2);
    }

    #[test]
    fn a_terminal_without_image_support_is_told_it_needs_kitty() {
        assert_eq!(
            decide(ProtocolType::Halfblocks).unwrap_err().to_string(),
            "termleaf needs a terminal that supports the Kitty graphics protocol \
             (for example Kitty or Ghostty, optionally inside herdr)"
        );
    }
}
