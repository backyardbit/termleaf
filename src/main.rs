mod app;
mod browser;
mod clipboard;
mod editor;
mod encoder;
mod follow;
mod graphics;
mod inverse;
mod keys;
mod kitty;
mod layout;
mod mouse;
mod outline;
mod pdf;
mod pinch;
mod raster;
mod renderer;
mod resume;
mod search;
mod selection;
mod shelf;
mod synctex;
mod terminal;
mod viewer;
mod visual;
mod watch;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::graphics::Choice;
use crate::resume::{Reporter, Resume, Token};

const USAGE: &str = "usage: termleaf [--no-pinch] [--no-follow] [--graphics <protocol>] <file.pdf>
       termleaf --follow <file.tex>:<line>[:<column>]

Shows a PDF in the terminal and reloads it whenever the file changes.

keys:
  j / k        next / previous page (takes a count, e.g. 5j)
  gg / G       first / last page (with a count: go to that page)
  :<n>         go to page n
  + / -        zoom in / out (= also zooms in)
  s / a        fit width / fit page
  /            search PDF text (Enter submits, Escape cancels/dismisses)
  n / N        next / previous match, wrapping at the ends
  t            table of contents (j/k move, Enter jumps, t/q/Escape close)
  F            follow the editor on / off (also :follow and :nofollow)
  q, Ctrl-C    quit

mouse:
  wheel               scroll (shift or a sideways swipe pans across)
  Ctrl + wheel        zoom at the pointer
  drag                pan
  click               follow a link, or open a web link in the browser
  double-click        switch between fit width and fit page
  pinch               zoom at the pointer (see below)

Pinch to zoom reads the trackpad from the OS, because terminals never pass
pinches on. It needs Input Monitoring for your terminal app on macOS, or
membership of the `input` group on Linux; without it, pinch stays off.

options:
  --no-pinch              never read the trackpad from the OS
  --no-follow             start with follow off (F turns it on)
  --graphics <protocol>   auto (the default), or kitty, sixel or iterm2 to use
                          that protocol whatever the terminal reports
  --follow <place>        show that source line in every termleaf whose PDF
                          was built from it, then exit";

#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    Help,
    Version,
    View {
        path: PathBuf,
        pinch: bool,
        follow: bool,
        graphics: Choice,
    },
    Follow(follow::Place),
    Misused,
}

fn parse(arguments: &[String]) -> Invocation {
    let mut pinch = true;
    let mut follow = true;
    let mut graphics = Choice::Auto;
    let mut rest = Vec::new();
    let mut arguments = arguments.iter().map(String::as_str);
    while let Some(argument) = arguments.next() {
        match argument {
            "--no-pinch" => pinch = false,
            "--no-follow" => follow = false,
            "--graphics" => match arguments.next().and_then(Choice::parse) {
                Some(choice) => graphics = choice,
                None => return Invocation::Misused,
            },
            _ => rest.push(argument),
        }
    }
    match rest.as_slice() {
        ["--follow", place] => follow::place(place).map_or(Invocation::Misused, Invocation::Follow),
        ["-h" | "--help"] => Invocation::Help,
        ["-V" | "--version"] => Invocation::Version,
        [path] if !path.starts_with('-') => Invocation::View {
            path: PathBuf::from(path),
            pinch,
            follow,
            graphics,
        },
        _ => Invocation::Misused,
    }
}

fn resume_argv(
    program: &Path,
    directory: &Path,
    path: &Path,
    options: &app::Options,
) -> Option<Vec<String>> {
    let mut argv = vec![program.to_str()?.to_owned()];
    if !options.pinch {
        argv.push("--no-pinch".to_owned());
    }
    if !options.follow {
        argv.push("--no-follow".to_owned());
    }
    if let Some(protocol) = options.graphics.flag() {
        argv.extend(["--graphics".to_owned(), protocol.to_owned()]);
    }
    argv.push(directory.join(path).to_str()?.to_owned());
    Some(argv)
}

fn resume_after(ended: &anyhow::Result<app::Ending>) -> Option<Resume> {
    match ended {
        Ok(app::Ending::TerminalClosed) => None,
        Ok(app::Ending::Quit) | Err(_) => Some(Resume::Clear),
    }
}

fn resume_command(path: &Path, options: &app::Options) -> Option<Resume> {
    let program = std::env::current_exe().ok()?;
    let directory = std::env::current_dir().ok()?;
    let argv = resume_argv(&program, &directory, path, options)?;
    Some(Resume::Run {
        argv,
        cwd: Some(directory),
    })
}

fn take_resume_token() -> Option<Token> {
    let token = std::env::var(resume::TOKEN_VARIABLE).ok()?;
    // SAFETY: this runs first in main, before termleaf starts any thread, so nothing else reads or writes the environment meanwhile.
    unsafe { std::env::remove_var(resume::TOKEN_VARIABLE) };
    Token::try_from(token.as_str()).ok()
}

fn main() -> ExitCode {
    let token = take_resume_token();
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
            follow,
            graphics,
        } => (
            path,
            app::Options {
                pinch,
                follow,
                graphics,
            },
        ),
        Invocation::Follow(place) => {
            let file = std::path::absolute(&place.file).unwrap_or(place.file);
            let directory = follow::directory(|name| std::env::var(name).ok());
            follow::broadcast(&directory, &follow::Place { file, ..place });
            return ExitCode::SUCCESS;
        }
        Invocation::Misused => {
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    let resume = token
        .map(Reporter::new)
        .zip(resume_command(&path, &options));
    let ended = app::run(path, options, || {
        if let Some((reporter, command)) = &resume {
            reporter.send(command, &mut std::io::stdout());
        }
    });
    if let Some((reporter, _)) = &resume
        && let Some(after) = resume_after(&ended)
    {
        reporter.send(&after, &mut std::io::stdout());
    }
    match ended {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            use std::io::Write;
            let _ = writeln!(std::io::stderr(), "termleaf: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::{Protocol, Raster};

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
                follow: true,
                graphics: Choice::Auto,
            }
        );
    }

    #[test]
    fn no_pinch_turns_it_off_on_either_side_of_the_file() {
        let expected = Invocation::View {
            path: PathBuf::from("thesis.pdf"),
            pinch: false,
            follow: true,
            graphics: Choice::Auto,
        };
        assert_eq!(run(&["--no-pinch", "thesis.pdf"]), expected);
        assert_eq!(run(&["thesis.pdf", "--no-pinch"]), expected);
    }

    #[test]
    fn no_follow_starts_with_follow_off_on_either_side_of_the_file() {
        let expected = Invocation::View {
            path: PathBuf::from("thesis.pdf"),
            pinch: true,
            follow: false,
            graphics: Choice::Auto,
        };
        assert_eq!(run(&["--no-follow", "thesis.pdf"]), expected);
        assert_eq!(run(&["thesis.pdf", "--no-follow"]), expected);
    }

    #[test]
    fn graphics_forces_a_protocol_on_either_side_of_the_file() {
        let expected = Invocation::View {
            path: PathBuf::from("thesis.pdf"),
            pinch: false,
            follow: true,
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

    fn options(pinch: bool, follow: bool, graphics: Choice) -> app::Options {
        app::Options {
            pinch,
            follow,
            graphics,
        }
    }

    fn resume_of(path: &str, options: &app::Options) -> Option<Vec<String>> {
        resume_argv(
            Path::new("/opt/bin/termleaf"),
            Path::new("/home/me/thesis"),
            Path::new(path),
            options,
        )
    }

    #[test]
    fn resuming_reopens_the_same_file_by_its_absolute_path() {
        assert_eq!(
            resume_of("out/thesis.pdf", &options(true, true, Choice::Auto)).unwrap(),
            ["/opt/bin/termleaf", "/home/me/thesis/out/thesis.pdf"]
        );
        assert_eq!(
            resume_of("/tmp/notes.pdf", &options(true, true, Choice::Auto)).unwrap(),
            ["/opt/bin/termleaf", "/tmp/notes.pdf"]
        );
    }

    #[test]
    fn resuming_keeps_the_options_termleaf_was_started_with() {
        let resumed = resume_of(
            "thesis.pdf",
            &options(false, false, Choice::Force(Protocol::Raster(Raster::Sixel))),
        )
        .unwrap();
        assert_eq!(
            resumed,
            [
                "/opt/bin/termleaf",
                "--no-pinch",
                "--no-follow",
                "--graphics",
                "sixel",
                "/home/me/thesis/thesis.pdf",
            ]
        );
        assert_eq!(
            parse(&resumed[1..]),
            Invocation::View {
                path: PathBuf::from("/home/me/thesis/thesis.pdf"),
                pinch: false,
                follow: false,
                graphics: Choice::Force(Protocol::Raster(Raster::Sixel)),
            }
        );
    }

    #[test]
    fn only_a_deliberate_quit_or_a_failure_withdraws_the_resume_command() {
        assert_eq!(resume_after(&Ok(app::Ending::Quit)), Some(Resume::Clear));
        assert_eq!(
            resume_after(&Err(anyhow::anyhow!("no graphics"))),
            Some(Resume::Clear)
        );
        assert_eq!(resume_after(&Ok(app::Ending::TerminalClosed)), None);
    }

    #[test]
    fn help_and_version_still_work() {
        assert_eq!(run(&["--help"]), Invocation::Help);
        assert_eq!(run(&["-V"]), Invocation::Version);
    }

    #[test]
    fn follow_takes_a_file_a_line_and_maybe_a_column() {
        assert_eq!(
            run(&["--follow", "chapters/intro.tex:12"]),
            Invocation::Follow(follow::Place {
                file: PathBuf::from("chapters/intro.tex"),
                line: 12,
                column: 1,
            })
        );
        assert_eq!(run(&["--follow", "intro.tex"]), Invocation::Misused);
        assert_eq!(run(&["--follow"]), Invocation::Misused);
    }

    #[test]
    fn an_unknown_flag_or_missing_file_is_a_misuse() {
        assert_eq!(run(&["--zoom", "thesis.pdf"]), Invocation::Misused);
        assert_eq!(run(&["--pinch", "thesis.pdf"]), Invocation::Misused);
        assert_eq!(run(&["--no-pinch"]), Invocation::Misused);
        assert_eq!(run(&[]), Invocation::Misused);
    }
}
