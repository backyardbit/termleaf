mod query;

use std::fmt;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use crossterm::terminal::WindowSize;
use ratatui_image::FontSize;
use ratatui_image::picker::cap_parser::{QueryStdioOptions, Response};
use ratatui_image::picker::{Capability, ProtocolType};

use crate::layout::CellSize;

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

pub struct TerminalInfo {
    font_size: FontSize,
    capabilities: Vec<Capability>,
    tmux: bool,
}

impl TerminalInfo {
    pub fn font_size(&self) -> FontSize {
        self.font_size
    }

    pub fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    pub fn tmux_detected(&self) -> bool {
        self.tmux
    }
}

pub fn detect(choice: Choice) -> Result<(Protocol, TerminalInfo)> {
    let hints = QueryHints::from_env(|name| std::env::var(name).ok());
    if hints.tmux {
        let _ = tmux_server()
            .args(["set", "-p", "allow-passthrough", "on"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let options = QueryStdioOptions {
        kitty_compression: true,
        blacklist_protocols: if hints.blacklist {
            vec![ProtocolType::Kitty, ProtocolType::Sixel]
        } else {
            Vec::new()
        },
        ..QueryStdioOptions::default()
    };
    let replies = query::read(
        std::io::stdin().as_fd(),
        &mut std::io::stdout(),
        hints.tmux,
        options,
    )
    .context("querying the terminal")?;
    let fallback = crossterm::terminal::window_size()
        .ok()
        .and_then(|size| cell_of(&size))
        .map(|(width, height)| FontSize::new(width, height));
    let (found, terminal) = interpret(replies, hints, fallback);
    let protocol = decide(found, choice, Hints::from_env())?;
    Ok((protocol, terminal))
}

pub struct CellWatch {
    seen: Option<(u16, u16)>,
}

impl CellWatch {
    pub fn new(size: Option<WindowSize>) -> Self {
        Self {
            seen: size.as_ref().and_then(cell_of),
        }
    }

    pub fn cell(&mut self, size: Option<WindowSize>, current: CellSize) -> CellSize {
        match size.as_ref().and_then(cell_of) {
            Some(cell) if self.seen.replace(cell) != Some(cell) => CellSize {
                width: u32::from(cell.0),
                height: u32::from(cell.1),
            },
            _ => current,
        }
    }
}

fn cell_of(size: &WindowSize) -> Option<(u16, u16)> {
    let width = size.width.checked_div(size.columns)?;
    let height = size.height.checked_div(size.rows)?;
    (width > 0 && height > 0).then_some((width, height))
}

struct QueryHints {
    tmux: bool,
    iterm2: bool,
    blacklist: bool,
}

impl QueryHints {
    fn from_env(variable: impl Fn(&str) -> Option<String>) -> Self {
        let set = |name| variable(name).is_some_and(|value| !value.is_empty());
        let program = variable("TERM_PROGRAM").unwrap_or_default();
        let tmux = set("TMUX")
            || program == "tmux"
            || variable("TERM").is_some_and(|term| term.starts_with("tmux"));
        let iterm2 = [
            "iTerm",
            "WezTerm",
            "mintty",
            "vscode",
            "Tabby",
            "Hyper",
            "rio",
            "Bobcat",
            "WarpTerminal",
        ]
        .iter()
        .any(|name| program.contains(name))
            || variable("LC_TERMINAL").is_some_and(|name| name.contains("iTerm"))
            || (tmux && (set("ITERM_SESSION_ID") || set("WEZTERM_EXECUTABLE")));
        Self {
            tmux,
            iterm2,
            blacklist: set("WEZTERM_EXECUTABLE") || set("KONSOLE_VERSION"),
        }
    }
}

fn interpret(
    replies: Vec<Response>,
    hints: QueryHints,
    fallback: Option<FontSize>,
) -> (ProtocolType, TerminalInfo) {
    let mut found = if hints.iterm2 {
        ProtocolType::Iterm2
    } else {
        ProtocolType::Halfblocks
    };
    let mut font_size = None;
    let mut capabilities = Vec::new();
    for reply in replies {
        let capability = match reply {
            Response::Kitty => {
                found = ProtocolType::Kitty;
                Some(Capability::Kitty)
            }
            Response::Sixel => {
                if found != ProtocolType::Kitty {
                    found = ProtocolType::Sixel;
                }
                Some(Capability::Sixel)
            }
            Response::KittyCompression => Some(Capability::KittyCompression),
            Response::CellSize(cell) => {
                if let Some((width, height)) =
                    cell.filter(|(width, height)| *width > 0 && *height > 0)
                {
                    font_size = Some(FontSize::new(width, height));
                }
                Some(Capability::CellSize(cell))
            }
            Response::RectangularOps => Some(Capability::RectangularOps),
            Response::Background(red, green, blue) => {
                Some(Capability::Background(red, green, blue))
            }
            Response::Status | Response::CursorPositionReport(..) => None,
        };
        capabilities.extend(capability);
    }
    let font_size = font_size.or(fallback);
    if font_size.is_none() {
        found = ProtocolType::Halfblocks;
    }
    (
        found,
        TerminalInfo {
            font_size: font_size.unwrap_or(FontSize::new(10, 20)),
            capabilities,
            tmux: hints.tmux,
        },
    )
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

    fn no_hints() -> QueryHints {
        QueryHints::from_env(|_| None)
    }

    #[test]
    fn queried_kitty_and_compression_win_over_sixel_and_environment_hints() {
        let hints =
            QueryHints::from_env(|name| (name == "TERM_PROGRAM").then(|| "iTerm.app".to_owned()));
        let (protocol, terminal) = interpret(
            vec![
                Response::Kitty,
                Response::Sixel,
                Response::KittyCompression,
                Response::CellSize(Some((8, 16))),
            ],
            hints,
            Some(FontSize::new(10, 20)),
        );
        assert_eq!(protocol, ProtocolType::Kitty);
        assert_eq!(
            (terminal.font_size().width, terminal.font_size().height),
            (8, 16)
        );
        assert!(
            terminal
                .capabilities()
                .contains(&Capability::KittyCompression)
        );
    }

    #[test]
    fn sixel_and_iterm2_keep_ioctl_cell_size_when_the_query_has_no_cell_reply() {
        let cell = FontSize::new(8, 16);
        let (protocol, terminal) = interpret(vec![Response::Sixel], no_hints(), Some(cell));
        assert_eq!(protocol, ProtocolType::Sixel);
        assert_eq!(
            (terminal.font_size().width, terminal.font_size().height),
            (cell.width, cell.height)
        );
        let hints =
            QueryHints::from_env(|name| (name == "TERM_PROGRAM").then(|| "iTerm.app".to_owned()));
        let (protocol, terminal) = interpret(Vec::new(), hints, Some(cell));
        assert_eq!(protocol, ProtocolType::Iterm2);
        assert_eq!(
            (terminal.font_size().width, terminal.font_size().height),
            (cell.width, cell.height)
        );
    }

    #[test]
    fn the_cell_size_changes_only_when_the_reported_pixels_per_cell_change() {
        let size = |width, height| {
            Some(WindowSize {
                rows: 40,
                columns: 100,
                width,
                height,
            })
        };
        let queried = CellSize {
            width: 9,
            height: 18,
        };
        let retina = CellSize {
            width: 20,
            height: 40,
        };
        let mut cells = CellWatch::new(size(1000, 800));
        assert_eq!(cells.cell(size(1000, 800), queried), queried);
        assert_eq!(cells.cell(size(0, 0), queried), queried);
        assert_eq!(cells.cell(None, queried), queried);
        assert_eq!(cells.cell(size(2000, 1600), queried), retina);
        assert_eq!(cells.cell(size(2000, 1600), retina), retina);
    }

    #[test]
    fn missing_or_zero_cell_sizes_use_the_existing_default() {
        let (protocol, terminal) = interpret(
            vec![Response::Kitty, Response::CellSize(Some((0, 0)))],
            no_hints(),
            None,
        );
        assert_eq!(protocol, ProtocolType::Halfblocks);
        assert_eq!(
            (terminal.font_size().width, terminal.font_size().height),
            (10, 20)
        );
    }

    #[test]
    fn tmux_wrapping_and_terminal_blacklists_keep_their_environment_hints() {
        let hints = QueryHints::from_env(|name| match name {
            "TMUX" => Some("/tmp/tmux,123,0".to_owned()),
            "WEZTERM_EXECUTABLE" => Some("wezterm".to_owned()),
            _ => None,
        });
        assert!(hints.tmux);
        assert!(hints.iterm2);
        assert!(hints.blacklist);
        let (_, terminal) = interpret(Vec::new(), hints, Some(FontSize::new(10, 20)));
        assert!(terminal.tmux_detected());
        let hints =
            QueryHints::from_env(|name| (name == "KITTY_WINDOW_ID").then(|| "1".to_owned()));
        assert!(!hints.iterm2);
        let (protocol, _) = interpret(Vec::new(), hints, Some(FontSize::new(10, 20)));
        assert_eq!(protocol, ProtocolType::Halfblocks);
    }

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
