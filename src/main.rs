mod app;
mod keys;
mod kitty;
mod layout;
mod mouse;
mod pdf;
mod renderer;
mod viewer;
mod watch;

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: termleaf <file.pdf>

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
  double-click        switch between fit width and fit page";

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let path = match arguments.as_slice() {
        [flag] if flag == "-h" || flag == "--help" => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        [flag] if flag == "-V" || flag == "--version" => {
            println!("termleaf {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        [path] => PathBuf::from(path),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    match app::run(path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("termleaf: {error:#}");
            ExitCode::FAILURE
        }
    }
}
