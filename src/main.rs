mod app;
mod editor;
mod encoder;
mod follow;
mod graphics;
mod inverse;
mod keys;
mod kitty;
mod layout;
mod mouse;
mod pdf;
mod pinch;
mod raster;
mod renderer;
mod shelf;
mod synctex;
mod viewer;
mod watch;

use std::path::PathBuf;
use std::process::ExitCode;

use crate::graphics::Choice;

const USAGE: &str = "usage: termleaf [--no-pinch] [--graphics <protocol>] <file.pdf>

Shows a PDF in the terminal and reloads it whenever the file changes.

keys:
  j / k        next / previous page (takes a count, e.g. 5j)
  gg / G       first / last page (with a count: go to that page)
  :<n>         go to page n
  + / -        zoom in / out (= also zooms in)
  s / a        fit width / fit page
  q, Ctrl-C    quit

mouse:
  wheel               scroll (shift or a sideways swipe pans across)
  Ctrl + wheel        zoom at the pointer
  drag                pan
  click               follow a link
  double-click        switch between fit width and fit page
  pinch               zoom at the pointer (see below)

Pinch to zoom reads the trackpad from the OS, because terminals never pass
pinches on. It needs Input Monitoring for your terminal app on macOS, or
membership of the `input` group on Linux; without it, pinch stays off.

options:
  --no-pinch              never read the trackpad from the OS
  --graphics <protocol>   auto (the default), or kitty, sixel or iterm2 to use
                          that protocol whatever the terminal reports";

#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    Help,
    Version,
    View {
        path: PathBuf,
        pinch: bool,
        graphics: Choice,
    },
    Misused,
}

fn parse(arguments: &[String]) -> Invocation {
    let mut pinch = true;
    let mut graphics = Choice::Auto;
    let mut rest = Vec::new();
    let mut arguments = arguments.iter().map(String::as_str);
    while let Some(argument) = arguments.next() {
        match argument {
            "--no-pinch" => pinch = false,
            "--graphics" => match arguments.next().and_then(Choice::parse) {
                Some(choice) => graphics = choice,
                None => return Invocation::Misused,
            },
            _ => rest.push(argument),
        }
    }
    match rest.as_slice() {
        ["-h" | "--help"] => Invocation::Help,
        ["-V" | "--version"] => Invocation::Version,
        [path] if !path.starts_with('-') => Invocation::View {
            path: PathBuf::from(path),
            pinch,
            graphics,
        },
        _ => Invocation::Misused,
    }
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let (path, options) = match parse(&arguments) {
        Invocation::Help => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Invocation::Version => {
            println!("termleaf {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Invocation::View {
            path,
            pinch,
            graphics,
        } => (path, app::Options { pinch, graphics }),
        Invocation::Misused => {
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    match app::run(path, options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("termleaf: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::Protocol;

    fn run(arguments: &[&str]) -> Invocation {
        let owned: Vec<String> = arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect();
        parse(&owned)
    }

    #[test]
    fn pinch_is_on_and_graphics_are_detected_by_default() {
        assert_eq!(
            run(&["thesis.pdf"]),
            Invocation::View {
                path: PathBuf::from("thesis.pdf"),
                pinch: true,
                graphics: Choice::Auto,
            }
        );
    }

    #[test]
    fn no_pinch_turns_it_off_on_either_side_of_the_file() {
        let expected = Invocation::View {
            path: PathBuf::from("thesis.pdf"),
            pinch: false,
            graphics: Choice::Auto,
        };
        assert_eq!(run(&["--no-pinch", "thesis.pdf"]), expected);
        assert_eq!(run(&["thesis.pdf", "--no-pinch"]), expected);
    }

    #[test]
    fn graphics_forces_a_protocol_on_either_side_of_the_file() {
        let expected = Invocation::View {
            path: PathBuf::from("thesis.pdf"),
            pinch: false,
            graphics: Choice::Force(Protocol::Kitty),
        };
        assert_eq!(
            run(&["--graphics", "kitty", "--no-pinch", "thesis.pdf"]),
            expected
        );
        assert_eq!(
            run(&["--no-pinch", "thesis.pdf", "--graphics", "kitty"]),
            expected
        );
    }

    #[test]
    fn graphics_without_a_known_protocol_is_a_misuse() {
        assert_eq!(run(&["thesis.pdf", "--graphics"]), Invocation::Misused);
        assert_eq!(run(&["--graphics", "thesis.pdf"]), Invocation::Misused);
        assert_eq!(
            run(&["--graphics", "halfblocks", "thesis.pdf"]),
            Invocation::Misused
        );
    }

    #[test]
    fn help_and_version_still_work() {
        assert_eq!(run(&["--help"]), Invocation::Help);
        assert_eq!(run(&["-V"]), Invocation::Version);
    }

    #[test]
    fn an_unknown_flag_or_missing_file_is_a_misuse() {
        assert_eq!(run(&["--zoom", "thesis.pdf"]), Invocation::Misused);
        assert_eq!(run(&["--pinch", "thesis.pdf"]), Invocation::Misused);
        assert_eq!(run(&["--no-pinch"]), Invocation::Misused);
        assert_eq!(run(&[]), Invocation::Misused);
    }
}
