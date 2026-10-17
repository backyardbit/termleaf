mod commits;
mod dpi;
mod e2e;
mod follow;
mod herdr;
mod jumps;
mod kitty;
mod pane_shell;
mod screen;
mod tmux;
mod wezterm;
mod xterm;
mod zellij;

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use ra_ap_rustc_lexer::{FrontmatterAllowed, TokenKind, tokenize};

const SAFETY_PREFIX: &str = "// SAFETY:";
const FORBIDDEN_NAME_FRAGMENT: &str = "shape";
const MOCKING_CRATES: &[&str] = &["mockall", "mockers", "mocktopus", "faux", "mry", "double"];
const FIXTURES_DIR: &str = "tests/fixtures/";
const SKIPPED_DIRS: &[&str] = &["target", ".git"];

#[derive(Debug, PartialEq, Eq)]
enum Rule {
    Comment,
    ForbiddenName,
    MockingCrate,
    UntrackedFixture,
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Rule::Comment => "no-comments",
            Rule::ForbiddenName => "no-shape-in-names",
            Rule::MockingCrate => "no-mocking-crates",
            Rule::UntrackedFixture => "fixtures-tracked",
        };
        f.write_str(name)
    }
}

#[derive(Debug)]
struct Violation {
    rule: Rule,
    line: usize,
    message: String,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let task = args.get(1).cloned();
    match task.as_deref() {
        Some("lint") => lint(),
        Some("check") => check(),
        Some("commits") => commits::run(
            &workspace_root(),
            args.get(2).map(String::as_str),
            args.get(3).map(String::as_str),
        ),
        Some("e2e") => e2e::run(&workspace_root()),
        Some("xterm") => xterm::run(&workspace_root()),
        Some("tmux") => jumps::run(&workspace_root(), jumps::Backend::Tmux),
        Some("herdr") => jumps::run(&workspace_root(), jumps::Backend::Herdr),
        Some("plain") => jumps::run(&workspace_root(), jumps::Backend::Plain),
        Some("zellij") => jumps::run(&workspace_root(), jumps::Backend::Zellij),
        Some("screen") => jumps::run(&workspace_root(), jumps::Backend::Screen),
        Some("kitty") => jumps::run(&workspace_root(), jumps::Backend::Kitty),
        Some("wezterm") => jumps::run(&workspace_root(), jumps::Backend::Wezterm),
        Some("follow") => follow::run(&workspace_root()),
        Some("dpi") => dpi::run(&workspace_root()),
        _ => {
            eprintln!(
                "usage: cargo xtask <lint|check|commits|e2e|xterm|tmux|herdr|plain|zellij|screen|kitty|wezterm|follow|dpi>"
            );
            eprintln!("  lint   run the anti-slop source checks");
            eprintln!("  check  run fmt, clippy, tests and lint");
            eprintln!(
                "  commits <base>..<head> [title]  check commit messages for attribution, (type): titles and a test before each fix"
            );
            eprintln!(
                "  e2e    drive termleaf in Ghostty and herdr on a virtual display (CI only)"
            );
            eprintln!(
                "  xterm  check termleaf's Sixel frames in xterm on a virtual display (CI only)"
            );
            eprintln!(
                "  tmux   jump nvim, vim and hx from termleaf in a headless tmux (needs tmux and the editors)"
            );
            eprintln!(
                "  herdr  jump nvim, vim and hx from termleaf in a headless herdr (needs herdr, tmux and the editors)"
            );
            eprintln!(
                "  plain  jump nvim over its socket with no multiplexer around termleaf (needs tmux as a pty host and nvim)"
            );
            eprintln!(
                "  zellij jump nvim, vim and hx from termleaf in a headless zellij 0.44+ (needs zellij, tmux and the editors)"
            );
            eprintln!(
                "  screen jump nvim, vim and hx from termleaf in a headless GNU screen (needs screen, tmux and the editors)"
            );
            eprintln!(
                "  kitty  jump nvim, vim and hx from termleaf between kitty windows over remote control (needs kitty, a display and the editors)"
            );
            eprintln!(
                "  wezterm jump nvim, vim and hx from termleaf between panes of a headless WezTerm mux server (needs wezterm, wezterm-mux-server and the editors)"
            );
            eprintln!(
                "  follow move termleaf with vim and the follow snippet, nvim and hx with no config, termleaf --follow, F, :follow, :nofollow and --no-follow in a headless tmux (needs tmux, vim, nvim and hx)"
            );
            eprintln!(
                "  dpi    change only the pixel size of termleaf's pty, alone and in zellij, and check the Sixel frame follows (needs zellij)"
            );
            ExitCode::FAILURE
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

fn check() -> ExitCode {
    let steps: [&[&str]; 3] = [
        &["fmt", "--all", "--check"],
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        &["test", "--workspace", "--quiet"],
    ];
    for args in steps {
        let passed = Command::new(env!("CARGO"))
            .args(args)
            .current_dir(workspace_root())
            .status()
            .is_ok_and(|status| status.success());
        if !passed {
            eprintln!("cargo {} failed", args.join(" "));
            return ExitCode::FAILURE;
        }
    }
    lint()
}

fn lint() -> ExitCode {
    let root = workspace_root();
    let mut reports = Vec::new();

    let Some(fixtures) = Fixtures::scan(&root) else {
        eprintln!("could not list tracked fixtures with git");
        return ExitCode::FAILURE;
    };
    let mut sources = Vec::new();
    collect_rust_files(&root, &mut sources);
    for path in sources {
        let Ok(source) = fs::read_to_string(&path) else {
            eprintln!("could not read {}", path.display());
            return ExitCode::FAILURE;
        };
        let found = check_source(&source)
            .into_iter()
            .chain(check_fixture_references(&source, &fixtures));
        for violation in found {
            reports.push((path.clone(), violation));
        }
    }

    let lockfile = root.join("Cargo.lock");
    if let Ok(lock) = fs::read_to_string(&lockfile) {
        for violation in check_lockfile(&lock) {
            reports.push((lockfile.clone(), violation));
        }
    }

    for (path, violation) in &reports {
        let relative = path.strip_prefix(&root).unwrap_or(path);
        eprintln!(
            "{}:{}: [{}] {}",
            relative.display(),
            violation.line,
            violation.rule,
            violation.message
        );
    }
    if reports.is_empty() {
        ExitCode::SUCCESS
    } else {
        eprintln!("{} anti-slop violation(s)", reports.len());
        ExitCode::FAILURE
    }
}

fn collect_rust_files(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if !SKIPPED_DIRS.iter().any(|skipped| name == *skipped) {
                collect_rust_files(&path, found);
            }
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

fn check_source(source: &str) -> Vec<Violation> {
    let mut violations = Vec::new();
    let mut offset = 0;
    let mut line = 1;
    for token in tokenize(source, FrontmatterAllowed::No) {
        let end = offset + usize::try_from(token.len).unwrap_or(usize::MAX);
        let text = source.get(offset..end).unwrap_or_default();
        match token.kind {
            TokenKind::LineComment { .. } if !text.starts_with(SAFETY_PREFIX) => {
                violations.push(comment_violation(line));
            }
            TokenKind::BlockComment { .. } => violations.push(comment_violation(line)),
            TokenKind::Ident | TokenKind::RawIdent
                if text.to_lowercase().contains(FORBIDDEN_NAME_FRAGMENT) =>
            {
                violations.push(Violation {
                    rule: Rule::ForbiddenName,
                    line,
                    message: format!(
                        "rename `{text}` for its domain role; \"shape\" describes structure, not ownership"
                    ),
                });
            }
            _ => {}
        }
        line += text.matches('\n').count();
        offset = end;
    }
    violations
}

struct Fixtures {
    tracked: BTreeSet<String>,
    untracked: BTreeSet<String>,
}

impl Fixtures {
    fn scan(root: &Path) -> Option<Self> {
        let output = Command::new("git")
            .args(["ls-files", "-z", "--", FIXTURES_DIR])
            .current_dir(root)
            .output()
            .ok()
            .filter(|output| output.status.success())?;
        let tracked: BTreeSet<String> = String::from_utf8(output.stdout)
            .ok()?
            .split('\0')
            .filter_map(|path| path.strip_prefix(FIXTURES_DIR))
            .map(str::to_owned)
            .collect();
        let mut on_disk = Vec::new();
        collect_files(&root.join(FIXTURES_DIR), &mut on_disk);
        let untracked = on_disk
            .iter()
            .filter_map(|path| path.strip_prefix(root.join(FIXTURES_DIR)).ok())
            .filter_map(|path| path.to_str())
            .filter(|path| !tracked.contains(*path))
            .map(str::to_owned)
            .collect();
        Some(Self { tracked, untracked })
    }

    fn untracked_reference(&self, literal: &str) -> Option<String> {
        if let Some((_, path)) = literal.split_once(FIXTURES_DIR) {
            let path = path
                .split(|c: char| !(c.is_alphanumeric() || "_.-/".contains(c)))
                .next()
                .unwrap_or_default();
            let directory = format!("{}/", path.trim_end_matches('/'));
            let tracked = self.tracked.contains(path)
                || self.tracked.iter().any(|file| file.starts_with(&directory));
            return (!path.is_empty() && !tracked).then(|| path.to_owned());
        }
        self.untracked
            .iter()
            .find(|path| *path == literal || path.ends_with(&format!("/{literal}")))
            .cloned()
    }
}

fn collect_files(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, found);
        } else {
            found.push(path);
        }
    }
}

fn check_fixture_references(source: &str, fixtures: &Fixtures) -> Vec<Violation> {
    let mut violations = Vec::new();
    let mut offset = 0;
    let mut line = 1;
    for token in tokenize(source, FrontmatterAllowed::No) {
        let end = offset + usize::try_from(token.len).unwrap_or(usize::MAX);
        let text = source.get(offset..end).unwrap_or_default();
        let literal = matches!(token.kind, TokenKind::Literal { .. })
            .then(|| text.strip_prefix('"')?.strip_suffix('"'))
            .flatten()
            .filter(|literal| !literal.is_empty());
        if let Some(path) = literal.and_then(|literal| fixtures.untracked_reference(literal)) {
            violations.push(Violation {
                rule: Rule::UntrackedFixture,
                line,
                message: format!(
                    "commit `{FIXTURES_DIR}{path}`; it is referenced here but not tracked by git, so CI will not have it (check .gitignore)"
                ),
            });
        }
        line += text.matches('\n').count();
        offset = end;
    }
    violations
}

fn comment_violation(line: usize) -> Violation {
    Violation {
        rule: Rule::Comment,
        line,
        message: "remove this comment; let names, types and tests carry the explanation (only `// SAFETY:` is allowed)".to_owned(),
    }
}

fn check_lockfile(lock: &str) -> Vec<Violation> {
    lock.lines()
        .enumerate()
        .filter_map(|(index, text)| {
            let name = text.strip_prefix("name = \"")?.strip_suffix('"')?;
            MOCKING_CRATES.contains(&name).then(|| Violation {
                rule: Rule::MockingCrate,
                line: index + 1,
                message: format!(
                    "drop `{name}`; replace dependencies in tests through a real trait and a faithful test implementation"
                ),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(source: &str) -> Vec<Rule> {
        check_source(source).into_iter().map(|v| v.rule).collect()
    }

    #[test]
    fn flags_every_comment_style() {
        let source = "// line\n/* block */\n/// doc\n//! inner doc\nfn main() {}\n";
        assert_eq!(
            rules(source),
            [Rule::Comment, Rule::Comment, Rule::Comment, Rule::Comment]
        );
    }

    #[test]
    fn allows_safety_comments() {
        let source = "fn f() {\n    // SAFETY: pointer is valid\n    unsafe {}\n}\n";
        assert!(rules(source).is_empty());
    }

    #[test]
    fn ignores_comment_markers_inside_strings() {
        let source = "fn f() { let url = \"https://example.com\"; let raw = r#\"/* x */\"#; }\n";
        assert!(rules(source).is_empty());
    }

    #[test]
    fn reports_the_line_of_the_violation() {
        let source = "fn f() {}\n\nfn g() {} // trailing\n";
        let lines: Vec<usize> = check_source(source).iter().map(|v| v.line).collect();
        assert_eq!(lines, [3]);
    }

    #[test]
    fn flags_forbidden_fragment_in_identifiers_case_insensitively() {
        let source = "struct PageShape;\nfn f() { let text_shape = 1; }\n";
        assert_eq!(rules(source), [Rule::ForbiddenName, Rule::ForbiddenName]);
    }

    #[test]
    fn allows_forbidden_fragment_inside_string_literals() {
        assert!(rules("fn f() { let s = \"shape\"; }\n").is_empty());
    }

    #[test]
    fn flags_fixture_references_git_does_not_track() {
        let fixtures = Fixtures {
            tracked: BTreeSet::from([
                "three-pages.pdf".to_owned(),
                "synctex/thesis.tex".to_owned(),
            ]),
            untracked: BTreeSet::from(["synctex/draft.synctex.gz".to_owned()]),
        };
        let source = "fn f() {\n    open(\"tests/fixtures/three-pages.pdf\");\n    open(\"tests/fixtures/gone.pdf\");\n    dir.join(\"draft.synctex.gz\");\n    dir.join(\"missing.pdf\");\n    root.join(\"tests/fixtures/synctex\");\n}\n";
        let lines: Vec<usize> = check_fixture_references(source, &fixtures)
            .iter()
            .map(|v| v.line)
            .collect();
        assert_eq!(lines, [3, 4]);
    }

    #[test]
    fn flags_mocking_crates_in_the_lockfile() {
        let lock = "[[package]]\nname = \"mockall\"\nversion = \"0.13.0\"\n\n[[package]]\nname = \"termleaf\"\n";
        let found: Vec<usize> = check_lockfile(lock).iter().map(|v| v.line).collect();
        assert_eq!(found, [2]);
    }
}
