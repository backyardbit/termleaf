use std::fmt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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
    if std::env::var_os("TMUX").is_some() {
        let _ = tmux_server()
            .args(["set", "-p", "allow-passthrough", "on"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let options = QueryStdioOptions {
        kitty_compression: true,
        ..QueryStdioOptions::default()
    };
    let picker = Picker::from_query_stdio_with_options(options).context("querying the terminal")?;
    let protocol = decide(picker.protocol_type(), choice, Hints::from_env())?;
    Ok((protocol, picker))
}

pub fn tmux_server() -> Command {
    Command::new(tmux_program(std::env::var("TMUX").ok().as_deref()))
}

fn tmux_program(tmux: Option<&str>) -> PathBuf {
    tmux.and_then(server_pid)
        .map(|pid| PathBuf::from(format!("/proc/{pid}/exe")))
        .filter(|program| program.exists())
        .unwrap_or_else(|| PathBuf::from("tmux"))
}

fn server_pid(tmux: &str) -> Option<u32> {
    tmux.rsplit(',').nth(1)?.parse().ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Hints {
    konsole: bool,
}

impl Hints {
    fn from_env() -> Self {
        Self {
            konsole: std::env::var_os("KONSOLE_VERSION").is_some_and(|version| !version.is_empty()),
        }
    }
}

fn decide(found: ProtocolType, choice: Choice, hints: Hints) -> Result<Protocol> {
    match (choice, found) {
        (Choice::Force(protocol), _) => Ok(protocol),
        (Choice::Auto, ProtocolType::Kitty) => Ok(Protocol::Kitty),
        (Choice::Auto, ProtocolType::Sixel) => Ok(Protocol::Raster(Raster::Sixel)),
        (Choice::Auto, ProtocolType::Iterm2) => Ok(Protocol::Raster(Raster::Iterm2)),
        (Choice::Auto, ProtocolType::Halfblocks) if hints.konsole => {
            Ok(Protocol::Raster(Raster::Iterm2))
        }
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
    const ELSEWHERE: Hints = Hints { konsole: false };
    const KONSOLE: Hints = Hints { konsole: true };

    #[test]
    fn a_kitty_terminal_keeps_the_kitty_path() {
        assert_eq!(
            decide(ProtocolType::Kitty, Choice::Auto, ELSEWHERE).unwrap(),
            Protocol::Kitty
        );
    }

    #[test]
    fn sixel_and_iterm2_terminals_take_the_raster_path() {
        assert_eq!(
            decide(ProtocolType::Sixel, Choice::Auto, ELSEWHERE).unwrap(),
            SIXEL
        );
        assert_eq!(
            decide(ProtocolType::Iterm2, Choice::Auto, ELSEWHERE).unwrap(),
            ITERM2
        );
    }

    #[test]
    fn a_forced_protocol_wins_over_what_the_terminal_reports() {
        assert_eq!(
            decide(
                ProtocolType::Halfblocks,
                Choice::Force(Protocol::Kitty),
                KONSOLE
            )
            .unwrap(),
            Protocol::Kitty
        );
        assert_eq!(
            decide(ProtocolType::Kitty, Choice::Force(SIXEL), ELSEWHERE).unwrap(),
            SIXEL
        );
        assert_eq!(
            decide(ProtocolType::Sixel, Choice::Force(ITERM2), ELSEWHERE).unwrap(),
            ITERM2
        );
    }

    #[test]
    fn a_terminal_without_image_support_is_pointed_at_the_flag() {
        assert_eq!(
            decide(ProtocolType::Halfblocks, Choice::Auto, ELSEWHERE)
                .unwrap_err()
                .to_string(),
            "termleaf needs a terminal that supports Kitty graphics, Sixel or iTerm2 images \
             (for example Kitty, Ghostty, herdr, WezTerm, iTerm2, Konsole, foot, or xterm -ti vt340); \
             if this terminal does support one of them, \
             run termleaf with --graphics kitty, sixel or iterm2"
        );
    }

    #[test]
    fn konsole_gets_iterm2_images_when_nothing_else_was_found() {
        assert_eq!(
            decide(ProtocolType::Halfblocks, Choice::Auto, KONSOLE).unwrap(),
            ITERM2
        );
    }

    #[test]
    fn the_konsole_hint_never_overrides_what_the_terminal_reports() {
        assert_eq!(
            decide(ProtocolType::Kitty, Choice::Auto, KONSOLE).unwrap(),
            Protocol::Kitty
        );
        assert_eq!(
            decide(ProtocolType::Sixel, Choice::Auto, KONSOLE).unwrap(),
            SIXEL
        );
    }

    #[test]
    fn the_tmux_that_runs_the_server_in_tmux_env_is_the_one_asked() {
        let pid = std::process::id();
        let running = if cfg!(target_os = "linux") {
            PathBuf::from(format!("/proc/{pid}/exe"))
        } else {
            PathBuf::from("tmux")
        };
        assert_eq!(
            tmux_program(Some(&format!("/tmp/tmux-1000/a,b,{pid},0"))),
            running
        );
        assert_eq!(
            tmux_program(Some("/tmp/tmux-1000/default")),
            PathBuf::from("tmux")
        );
        assert_eq!(tmux_program(None), PathBuf::from("tmux"));
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
