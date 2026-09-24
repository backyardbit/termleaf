mod app;
mod keys;
mod kitty;
mod layout;
mod mouse;
mod pdf;
mod pinch;
mod renderer;
mod viewer;
mod watch;

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: termleaf [--pinch] <file.pdf>

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

options:
  --pinch      zoom with a trackpad pinch. termleaf reads the trackpad from
               the OS, which needs Input Monitoring for your terminal on
               macOS or membership of the `input` group on Linux";

#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    Help,
    Version,
    View { path: PathBuf, pinch: bool },
    Misused,
}

fn parse(arguments: &[String]) -> Invocation {
    let pinch = arguments.iter().any(|argument| argument == "--pinch");
    let rest: Vec<&str> = arguments
        .iter()
        .map(String::as_str)
        .filter(|argument| *argument != "--pinch")
        .collect();
    match rest.as_slice() {
        ["-h" | "--help"] => Invocation::Help,
        ["-V" | "--version"] => Invocation::Version,
        [path] if !path.starts_with('-') => Invocation::View {
            path: PathBuf::from(path),
            pinch,
        },
        _ => Invocation::Misused,
    }
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let (path, pinch) = match parse(&arguments) {
        Invocation::Help => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Invocation::Version => {
            println!("termleaf {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Invocation::View { path, pinch } => (path, pinch),
        Invocation::Misused => {
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    match app::run(path, app::Options { pinch }) {
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

    fn run(arguments: &[&str]) -> Invocation {
        let owned: Vec<String> = arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect();
        parse(&owned)
    }

    #[test]
    fn a_file_is_viewed_without_pinch_by_default() {
        assert_eq!(
            run(&["thesis.pdf"]),
            Invocation::View {
                path: PathBuf::from("thesis.pdf"),
                pinch: false
            }
        );
    }

    #[test]
    fn pinch_is_opt_in_on_either_side_of_the_file() {
        let expected = Invocation::View {
            path: PathBuf::from("thesis.pdf"),
            pinch: true,
        };
        assert_eq!(run(&["--pinch", "thesis.pdf"]), expected);
        assert_eq!(run(&["thesis.pdf", "--pinch"]), expected);
    }

    #[test]
    fn help_and_version_still_work() {
        assert_eq!(run(&["--help"]), Invocation::Help);
        assert_eq!(run(&["-V"]), Invocation::Version);
    }

    #[test]
    fn an_unknown_flag_or_missing_file_is_a_misuse() {
        assert_eq!(run(&["--zoom", "thesis.pdf"]), Invocation::Misused);
        assert_eq!(run(&["--pinch"]), Invocation::Misused);
        assert_eq!(run(&[]), Invocation::Misused);
    }
}
