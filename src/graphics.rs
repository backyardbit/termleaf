use std::fmt;

use anyhow::{Context, Result, bail};
use ratatui_image::picker::cap_parser::QueryStdioOptions;
use ratatui_image::picker::{Picker, ProtocolType};

pub const NEEDS_IMAGES: &str = "termleaf needs a terminal that supports Kitty graphics, Sixel or iTerm2 images \
     (for example Kitty, Ghostty, herdr, WezTerm, iTerm2, Konsole, foot, or xterm -ti vt340)";

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Auto,
    Force(Protocol),
}

impl Choice {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "kitty" => Some(Self::Force(Protocol::Kitty)),
            "sixel" => Some(Self::Force(Protocol::Raster(Raster::Sixel))),
            "iterm2" => Some(Self::Force(Protocol::Raster(Raster::Iterm2))),
            _ => None,
        }
    }
}

impl fmt::Display for Raster {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Sixel => "Sixel",
            Self::Iterm2 => "iTerm2",
        })
    }
}

pub fn detect(choice: Choice) -> Result<(Protocol, Picker)> {
    let options = QueryStdioOptions {
        kitty_compression: true,
        ..QueryStdioOptions::default()
    };
    let picker = Picker::from_query_stdio_with_options(options).context("querying the terminal")?;
    let protocol = decide(picker.protocol_type(), choice)?;
    Ok((protocol, picker))
}

fn decide(found: ProtocolType, choice: Choice) -> Result<Protocol> {
    match (choice, found) {
        (Choice::Force(protocol), _) => Ok(protocol),
        (Choice::Auto, ProtocolType::Kitty) => Ok(Protocol::Kitty),
        (Choice::Auto, ProtocolType::Sixel) => Ok(Protocol::Raster(Raster::Sixel)),
        (Choice::Auto, ProtocolType::Iterm2) => Ok(Protocol::Raster(Raster::Iterm2)),
        (Choice::Auto, ProtocolType::Halfblocks) => {
            bail!(
                "{NEEDS_IMAGES}; if this terminal does support one of them, \
                 run termleaf with --graphics kitty, sixel or iterm2"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIXEL: Protocol = Protocol::Raster(Raster::Sixel);
    const ITERM2: Protocol = Protocol::Raster(Raster::Iterm2);

    #[test]
    fn a_kitty_terminal_keeps_the_kitty_path() {
        assert_eq!(
            decide(ProtocolType::Kitty, Choice::Auto).unwrap(),
            Protocol::Kitty
        );
    }

    #[test]
    fn sixel_and_iterm2_terminals_take_the_raster_path() {
        assert_eq!(decide(ProtocolType::Sixel, Choice::Auto).unwrap(), SIXEL);
        assert_eq!(decide(ProtocolType::Iterm2, Choice::Auto).unwrap(), ITERM2);
    }

    #[test]
    fn a_forced_protocol_wins_over_what_the_terminal_reports() {
        assert_eq!(
            decide(ProtocolType::Halfblocks, Choice::Force(Protocol::Kitty)).unwrap(),
            Protocol::Kitty
        );
        assert_eq!(
            decide(ProtocolType::Kitty, Choice::Force(SIXEL)).unwrap(),
            SIXEL
        );
        assert_eq!(
            decide(ProtocolType::Sixel, Choice::Force(ITERM2)).unwrap(),
            ITERM2
        );
    }

    #[test]
    fn a_terminal_without_image_support_is_pointed_at_the_flag() {
        assert_eq!(
            decide(ProtocolType::Halfblocks, Choice::Auto)
                .unwrap_err()
                .to_string(),
            "termleaf needs a terminal that supports Kitty graphics, Sixel or iTerm2 images \
             (for example Kitty, Ghostty, herdr, WezTerm, iTerm2, Konsole, foot, or xterm -ti vt340); \
             if this terminal does support one of them, \
             run termleaf with --graphics kitty, sixel or iterm2"
        );
    }

    #[test]
    fn each_flag_value_names_one_choice() {
        assert_eq!(Choice::parse("auto"), Some(Choice::Auto));
        assert_eq!(Choice::parse("kitty"), Some(Choice::Force(Protocol::Kitty)));
        assert_eq!(Choice::parse("sixel"), Some(Choice::Force(SIXEL)));
        assert_eq!(Choice::parse("iterm2"), Some(Choice::Force(ITERM2)));
        assert_eq!(Choice::parse("Kitty"), None);
        assert_eq!(Choice::parse("halfblocks"), None);
    }
}
